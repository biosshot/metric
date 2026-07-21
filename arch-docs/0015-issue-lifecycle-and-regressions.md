# ADR-0015: Issue lifecycle and regression semantics

- Status: Accepted
- Date: 2026-07-21

## Context

An Issue is both a durable group of equivalent Events and the user's workflow state
for that group. Processing must define whether a new occurrence reopens a resolved
Issue, must not treat an old queued occurrence as a regression merely because it was
processed late, and must preserve ignored Issues without discarding their events.

Issue counters are approximate under the crash contract in ADR-0005, but user
workflow state and regression transitions must use the atomicity of a single MongoDB
Issue document.

## Decision

### States and transitions

The first version has exactly three Issue states:

```text
open
resolved
ignored
```

The allowed conceptual transitions are:

```text
open -> resolved                 manual or API resolve
resolved -> open                 new accepted event; regression
open|resolved -> ignored         manual or API ignore
ignored -> open                  manual or API unignore
```

There is no separate `archived` state initially. Unignore always returns an Issue to
`open`; it does not restore a hidden previous state.

### Regression clock

The server-assigned Event `received_at` determines whether an occurrence arrived
after resolution. Client `occurred_at` is not used because client clocks can be
incorrect and SDKs can deliver buffered events long after they occurred.

```text
is_regression = issue.status == resolved
    && event.received_at > issue.resolved_at
```

An Event accepted before the resolve command does not reopen the Issue even if a RAM
queue delay, retry, restart, or MongoDB backlog causes Processor to finalize it after
the command. An Event accepted after the resolve command reopens the Issue even if
its client occurrence timestamp is older.

For a FinalizeBatch containing several occurrences for one Issue, the equivalent
decision uses the batch's server receipt-time range. When reopening, the stored
regression event is a deterministic relevant Event selected from that batch.

### Atomic Issue update

The Finalizer's single-document MongoDB update always maintains first/last seen and
the approximate occurrence count. In the same atomic update it conditionally changes
`resolved` to `open` when a qualifying receipt time exists and sets:

```javascript
{
  status: "open",
  status_changed_at,
  last_regressed_at,
  last_regression_event_id,
  regression_count
}
```

Only the update that observes `resolved` increments `regression_count`. Concurrent
occurrences serialize on the Issue document: the first qualifying update reopens it,
and later updates observe `open`. Retrying an Issue update after a Finalizer crash can
still overcount occurrences as accepted by ADR-0005, but it cannot count the same
reopening twice because the Issue is already open.

The ordering between a user command and Processor is intentional:

- if event processing finishes and the user resolves afterward, the later user
  command leaves the Issue resolved;
- if the user resolves and the server subsequently accepts a new Event, the Event
  reopens the Issue;
- if the server accepted the Event before resolution but processes it afterward, its
  earlier `received_at` prevents reopening.

No multi-document transaction is required for this transition.

### Ignored behavior

An ignored Issue remains ignored regardless of new Events. Processor continues to
store Event documents and update `last_seen`, `occurrence_count`, and hourly buckets,
but it does not create a regression or automatically return the Issue to `open`.

Ignored Issues are omitted from the default open-Issue view and suppress future
notification actions. The initial ignore mode is indefinite. Time-based ignore,
event-count snooze, release-based ignore, and Scheduler-driven wakeup are deferred.

### Issue fields

ADR-0024 compacts the initial workflow fields to:

```javascript
{
  s, // absent=open, 1=resolved, 2=ignored
  w: { t, a }, // current resolved/ignored time and actor
  d: { t, e, c } // optional last regression; c absent means one
}
```

ActorRef identifies a user, API credential, or explicit system action. Returning to
open removes `s` and `w`; historical transitions remain in `issue_activities`.

Release-based resolution, resolve-in-next-release, and conditional auto-resolution
are not implemented initially.

### Activity history

The shared database includes a low-volume `issue_activities` collection. It records
workflow transitions rather than ordinary Event occurrences:

```javascript
{
  _id,
  project_id,
  issue_id,
  kind: "resolved" | "ignored" | "unignored" | "reopened" | "assigned",
  actor_id,
  event_id,
  created_at,
  metadata
}
```

Activity identifiers are deterministic where a command or regression Event provides
an idempotency key. The Issue document remains the source of truth for current
workflow state. Because transactions and an outbox are not initially required, a
crash between the Issue update and activity insertion may omit an activity record;
it must never roll back or corrupt the Issue state. A reliable outbox may be added
later if audit requirements demand guaranteed history.

## Consequences

- Late processing of an already accepted Event cannot create a false regression.
- Client clock skew does not affect workflow transitions.
- Ignoring an Issue does not discard observability data or alter its counters.
- Concurrent qualifying Events produce one reopening transition.
- Issue activity supports UI, MCP, and investigation history without adding a write
  for every Event.
- Activity history is best-effort until a durable outbox is introduced.

## Deferred questions

- Time-, count-, and release-based snooze behavior.
- Release-based resolution and automatic resolution policies.
- Guaranteed activity delivery through an outbox.
- Assignment ownership, teams, and notification interaction.
- Retention policy and indexes for `issue_activities`.
