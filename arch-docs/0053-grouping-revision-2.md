# ADR-0053: Grouping revision 2 for exceptions without application frames and thread stacks

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

Some SDKs send the stack of a message in `threads` rather than in `stacktrace`: Go
`CaptureMessage` with `AttachStacktrace` puts it into the current thread and sends no
exception. Revision 1 does not read threads, so such messages group by their text, and
a text that carries a pod name or another per-instance value splits one failure into
many Issues.

ADR-0014 forbids changing revision 1 under existing projects.

## Decision

The Normalizer reads the stack of the crashed thread, else the current thread, else
the only thread, as Sentry does. `threads` itself stays among the unknown fields, so
the canonical body does not change.

Revision 2 differs from revision 1 in two cases. First, an Event with no exceptions
and no event stack but with a thread stack — a message: revision 2 groups by the
application frames of that stack, by the revision 1 rules, before it falls back
further. One rule differs: when the stack marks any frame as application code
(`in_app: true`), only the marked frames count, as in Sentry, while revision 1 also
counts frames without a mark. The Android SDK marks only the application's own frames
and leaves system and WebView frames unmarked. The file name of a WebView frame
carries the WebView build, and some vendors add frames of their own, so counting
unmarked frames would split one message by device. A thread without application
frames changes nothing, so a message still groups by its text. Neither does a native
thread stack, as Cocoa attaches to a message: its debug identifiers and
module-relative addresses change with every build of the application and every OS
version, while the text of the message stays the same.
An exception never takes the thread stack, as in Sentry, where the exception
strategies take precedence over the threads one: that stack shows where the error was
reported rather than where it happened, so the different errors of one handler would
share an Issue.

Second, an Event with at least one exception, no applicable exception or native stack
and no normalized message — the Events that revision 1 groups as
`<empty error>`. For them revision 2 selects, in order:

1. `exception_stack` over library frames: the exception types plus up to eight frames,
   newest first, chosen by the revision 1 identity rules except that library frames
   count. Lines still count only for frames without a function, so a library release
   that moves lines does not split Issues. An exception with native frames skips this
   step: `native_stack` could not place it, and its symbolicated names would make the
   Issue depend on which debug files were uploaded.
2. `message` over the exception: the exception types plus the newest exception value
   that is not empty after normalization. The value normalizer extends the revision 1
   message normalizer with one rule of Sentry's parameterization
   (`sentry/grouping/parameterization.py`): a token with a URL becomes `{url}`.
   Browsers put the failing URL into the value, so a dynamic import of a chunk named
   after its build hash would open a new Issue on every deploy, and one failure would
   split by host. Bare host names stay as they are: a token rule cannot tell them from
   file names such as `index.js`. Everything else keeps the message rules of
   ADR-0014, unlike in Sentry. E-mail and IP addresses stay, since a token rule
   misreads forms such as `pkg@1.2.3` and `[::1]:`. A long number becomes `{number}`,
   while a short one such as an HTTP status stays, because it tells failures apart. A
   ServiceWorker script missing on one host (`404`) and a gateway failing on another
   (`502`) are two failures. Messages keep the revision 1 normalizer, so their keys do
   not change.

An Event with no exception type, value or frame keeps the `<empty error>` component.

Everywhere else revision 2 selects what revision 1 selects and keeps the revision 1
key, revision prefix included. Moving a project to revision 2 therefore changes only
its `<empty error>` Issue and the Issues of messages that carry a thread stack; every
other Issue keeps receiving its Events. The Issue culprit also falls back to the
thread stack; it is not part of a key, so it does so under either revision. An SDK
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
- Messages with a thread stack, such as Go log hooks, group by where they were logged
  rather than by their text; their former Issues stop receiving them.
- Thread frames are not symbolicated, so a thread stack that needs source maps groups
  by its raw frames.
- Revision 1 keys and golden vectors are unchanged; revision 2 adds vectors for the
  new cases.
- An Issue may carry a revision 1 key while its project is pinned to revision 2.
- A dedicated operation for moving a project between revisions remains to be designed.
