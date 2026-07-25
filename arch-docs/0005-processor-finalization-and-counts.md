# ADR-0005: Processor finalization and approximate occurrence counts

- Status: Accepted
- Date: 2026-07-20

## Context

Processor finalization updates both the event being processed and the issue that owns
the event. Making every event-to-issue update and counter increment exact across
multiple MongoDB documents would require transactions or an additional idempotency
ledger in the hottest write path.

Metric is an error tracker rather than a financial ledger. A rare one-time counter
drift after a process crash is acceptable when the underlying event, grouping result,
and user-controlled issue state remain correct.

## Decision

### Correctness classes

The following data remains authoritative and requires strict behavior:

- an event acknowledged to an SDK;
- event ingestion idempotency;
- deterministic event-to-issue identity;
- issue status, assignment, ignore, and resolve commands;
- event payload and event processing state.

The following data is derived and may be eventually corrected:

- lifetime occurrence counts;
- affected-user estimates;
- charts and trend statistics;
- other rebuildable aggregates.

### FinalizeBatch

Completed processing results are collected into a FinalizeBatch and grouped by issue.
For each issue, Finalizer performs one idempotent upsert/update containing the batch's
minimum timestamp, maximum timestamp, and occurrence count contribution.

The logical issue update is:

```javascript
{
  $setOnInsert: {
    project_id,
    grouping_key,
    status: "open",
    created_at: first_timestamp
  },
  $min: {
    first_seen: first_timestamp
  },
  $max: {
    last_seen: last_timestamp
  },
  $inc: {
    occurrence_count: batch_count
  }
}
```

The same single-document Issue update conditionally reopens `resolved` Issues using
the server receipt-time rule in ADR-0015. Regression state remains strict even though
the occurrence increment in that update has the accepted approximate-count contract.

After issue updates, Finalizer associates each still-pending event with its issue and
marks the event processed using the compact ADR-0022 representation.

```javascript
{
  $set: {
    u: issue_id,
    b: canonical_processed_body
  },
  $unset: { q: "" }
}
```

Event finalization filters on `q.s == 0` so that an already completed event is not
reverted or finalized as a different occurrence. Absence of `q` is the durable
processed state; attempt count and `processed_at` are not repeated in terminal Event
documents.

The same FinalizeBatch also groups processed occurrences by issue and UTC hour and
upserts `issue_stats_hourly` documents with a batched `$inc`. These hourly documents
are a materialized projection and use the same approximate-count contract as the
lifetime issue counter.

### No transaction in the hot path

Processor FinalizeBatch does not require a multi-document MongoDB transaction. A
standalone MongoDB deployment therefore remains supported by this part of the design.

If the process stops after an issue counter increment but before the corresponding
events become processed, retrying those pending events can increment the counter a
second time. This narrow failure window and its resulting overcount are accepted.

The failure must not create another issue because issue identity is deterministic. It
must not corrupt first-seen or last-seen because those values use idempotent minimum
and maximum updates.

### Counter semantics

The issue field is named `occurrence_count`, not `event_count`.

`occurrence_count` is an approximate lifetime count of known occurrences. It is not
the number of raw event documents currently retained in MongoDB. Raw-event retention
may delete event documents without decrementing the issue's lifetime count.

The counter:

- must use an integer type suitable for long-lived high-volume issues;
- may contain a small positive drift after a crash in the Finalizer failure window;
- must not be used for billing, quotas, or another financial decision requiring exact
  accounting;
- may be reconciled or rebuilt when sufficient retained or archived source data is
  available.

Hourly bucket counts follow the same rules. Summing buckets is suitable for charts
and trend analysis but does not create an exact accounting ledger.

### Reconciliation

Counter reconciliation is not part of the ingestion or Processor hot path. Scheduler
may later support scoped recalculation for recent or explicitly selected issues. An
administrative rebuild operation may also be added for migrations and repair.

Exact lifetime reconstruction is impossible after raw source events are deleted
unless aggregates or archives preserving that history exist. This limitation is part
of the counter's approximate contract.

### Alerts

Alert delivery does not use `occurrence_count` as an exactly-once ledger. ADR-0016
defines deterministic notification transitions and delivery identifiers independently
of approximate occurrence counters.

## Consequences

- Processor avoids transaction overhead and transaction-only deployment requirements
  in its hottest write path.
- A hot issue receives one issue update per FinalizeBatch rather than one update per
  event.
- A hot issue-hour receives one statistics update per FinalizeBatch rather than one
  update per event.
- A rare crash may overcount a batch of occurrences, while accepted events and issue
  workflow state remain correct.
- Issue counts survive raw-event retention and remain useful as lifetime indicators.
- Exact analytics, billing, and alert delivery require separate mechanisms rather
  than reusing the approximate issue counter.

## Deferred questions

- FinalizeBatch wait, size, and parallelism limits.
- Reconciliation triggers and retained-history limits.
- Hourly bucket retention, indexes, and reconciliation scope.
