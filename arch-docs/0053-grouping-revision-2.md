# ADR-0053: Grouping revision 2 for exceptions without application frames

- Status: Accepted
- Date: 2026-10-23

## Context

Revision 1 (ADR-0014) groups an exception by its type and application frames. An
exception whose frames are all library frames (`in_app: false`), or that has no frames
at all, falls through to the message strategy. SDKs send exceptions without an event
message, so all of them get the single `<empty error>` component: every such failure
of a project shares one Issue, under the title of whichever came first.

This is the common case for some platforms rather than an edge case. The Node SDK
marks every `node_modules` frame as library code, so a failure raised inside a
framework such as NestJS or an ORM has no application frames. Browsers report
unhandled rejections such as `Failed to fetch dynamically imported module` without a
stack. A new failure of either kind then alerts as a regression of an unrelated Issue,
or not at all.

ADR-0014 forbids changing revision 1 under existing projects.

## Decision

Revision 2 differs from revision 1 in one case only: an Event with at least one
exception, no applicable exception or native stack and no normalized message — the
Events that revision 1 groups as `<empty error>`. For them revision 2 selects, in
order:

1. `exception_stack` over library frames: the exception types plus up to eight frames,
   newest first, chosen by the revision 1 identity rules except that library frames
   count. Lines still count only for frames without a function, so a library release
   that moves lines does not split Issues. An exception with native frames skips this
   step: `native_stack` could not place it, and its symbolicated names would make the
   Issue depend on which debug files were uploaded.
2. `message` over the exception: the exception types plus the newest exception value
   that is not empty after the revision 1 message normalizer.

An Event with no exception type, value or frame keeps the `<empty error>` component.

Everywhere else revision 2 selects what revision 1 selects and keeps the revision 1
key, revision prefix included. Moving a project to revision 2 therefore splits only
its `<empty error>` Issue; every other Issue keeps receiving its Events. An SDK
fingerprint with `{{ default }}` embeds the revision 2 default and takes the revision
2 prefix only when that default is one of the cases above.

New projects are created with revision 2. An existing project keeps its pinned
revision and moves only through an explicit operation, as ADR-0014 requires. Until a
dedicated operation exists, an operator sets `grouping_revision` to 2 in the
`projects` collection (MongoDB) or the `metric.projects` table (PostgreSQL). The
Processor reads the revision for every Event, so the change applies to Events
processed after it.

A server that does not know revision 2 fails to group the Events of a project pinned
to it. Before downgrading below this change, move such projects back to revision 1.

## Consequences

- Library-only failures and frameless exceptions get Issues of their own and alert as
  new Issues; the former `<empty error>` Issue stops receiving them.
- Revision 1 keys and golden vectors are unchanged; revision 2 adds vectors for the
  new cases.
- An Issue may carry a revision 1 key while its project is pinned to revision 2.
- A dedicated operation for moving a project between revisions remains to be designed.
