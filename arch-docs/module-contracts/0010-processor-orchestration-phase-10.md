# Phase 10 contract: bounded Processor orchestration

- Status: accepted for implementation
- Date: 2026-07-23
- Owners: `application::processor` (orchestration and error policy), child
  application stages (algorithms), `ports` (project fence and processing-state
  capabilities), `mongo::event` (compact retry/failed mutation), `server`
  (configuration and composition)

## Responsibilities and exclusions

Processor owns the ordered per-Event lifecycle:

```text
project fence
-> Normalizer
-> Symbolication
-> Grouper
-> IssueService occurrence preparation
-> Finalizer durable batch
```

It owns bounded concurrency, stage and total deadlines, cancellation observation,
stable temporary/permanent error classification, retry/backoff policy, terminal
failure, processing latency/outcome metrics and graceful drain through Dispatcher's
existing WorkHandler lifecycle.

Processor does not implement child algorithms, persist an Issue separately from
Finalizer, deliver notifications, schedule periodic work, add distributed claims,
or implement MCP, migrations, NATS, sharding, disk spool, external symbolication,
debug files or source maps.

## Typed scheduling and ports

`AcceptedEvent` remains the durable-acceptance value and does not gain processing
metadata. Dispatcher schedules a domain-owned `PendingEvent` containing the accepted
Event plus the compact nonnegative attempt count read from `q.a`; a fresh handoff
starts at zero. This keeps retry state out of the ingest contract while allowing
Processor to make an explicit bounded retry decision.

`ProcessingProjectStore` returns only the project state, Error-Event capability and
pinned grouping revision needed by Processor. It exposes no DSN key, scrub secret,
MongoDB document or unrestricted project repository.

`ProcessingStateStore` conditionally changes only a still-pending Event whose
`q.a` equals the scheduled attempt. A retry increments `q.a`, sets bounded numeric
`q.c` and moves `q.n` into the future. A permanent failure increments `q.a`, sets
`q.s = 1` and `q.c`, and removes `q.n`. A missing/stale match is success-like:
another retry or successful Finalizer already changed durable eligibility.

Returning from `WorkHandler::handle` still means Processor attempted a durable
eligibility transition. If the state store is unavailable, the unchanged Event
remains discoverable; Dispatcher may remove only its local key.

## Ordering, classification and retry

Every stage receives the successful output of the previous stage. IssueService is a
pure preparation step in this path; Finalizer is the only production owner of Issue,
hourly, catalog and terminal Event writes. Finalizer receives the already prepared
occurrence, preventing a duplicate Issue count increment.

Malformed/over-limit normalized input, unsupported grouping revision, inconsistent
Issue identity, output bounds and identity collisions are permanent. Project lookup,
symbolication retry disposition, stage timeout, cancellation and Finalizer
unavailability are temporary. A fenced/nonactive project or disabled Error
capability becomes a terminal failed Event without invoking transformation stages.

Error codes are a stable append-only numeric registry with bounded static names used
only in logs/metrics. Backend text, payload fields, project/Event/Issue identifiers
and release/environment values are never labels or error strings.

Attempts include the failed execution being recorded. Before `max_attempts`,
temporary failures use deterministic capped exponential backoff:

```text
min(retry_base * 2^(new_attempt - 1), retry_max)
```

At `max_attempts`, the same temporary error becomes permanently failed. Timestamp
overflow fails closed as terminal retry exhaustion. Permanent errors never receive a
future due time.

## Bounds, cancellation and shutdown

Processor configuration bounds concurrent Events, maximum attempts, retry base/max,
project lookup, every stage and total Event duration. Zero values, inverted retry
bounds, excessive concurrency/attempts or a stage timeout above the total deadline
fail startup validation.

The Processor semaphore bounds direct callers independently of Dispatcher. Stage
futures observe the Processor cancellation token and per-stage deadline. Pure
bounded CPU stages check cancellation/deadline before and after their deterministic
work; they create no detached tasks. One lifecycle-owned Finalizer batch task
coalesces prepared results through a bounded channel using finite wait/event limits;
it is the Phase 9 `FinalizeBatch` owner and is drained after Dispatcher.

Dispatcher fences new/refill work on root shutdown, then drains buffered/running
Processor futures until its configured deadline. Completed work persists normally;
aborted work keeps its previous pending state and is recovered after restart.
Processor cancellation is used for explicit forced cancellation tests and records a
retry when storage remains available.

## Operability and verification

Metrics use only fixed stage/outcome/error-code labels and cover active/concurrency
wait, per-stage/total latency, processed/retry/failed/stale/state-unavailable
outcomes, attempt exhaustion, project fences and FinalizeBatch occupancy. Readiness
requires the Dispatcher and its lifecycle-owned Finalizer batch task; neither is an
independent durable queue.

Verification requires the full stage outcome matrix with fake capability ports,
deadline/cancellation/concurrency/drain tests, real MongoDB retry/failed/restart
recovery, project/capability fences, backlog growth/guard/recovery, no Event left
pending after terminal classification, cumulative official-SDK-to-Issue/hourly E2E,
production pending-query behavior, and one retained real-MongoDB RPS baseline.

The local recovery gate reports explicit RPS and must exceed 1.5 times the
like-for-like measured accepted rate for the same fixture and hardware. If the
machine cannot satisfy the ADR-0037 reference rate, the artifact records that known
limit rather than relabeling a module-only result as full capacity.
