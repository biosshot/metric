# Phase 14 contract: bounded Scheduler, retention, and narrow reconciliation

- Status: accepted for implementation
- Date: 2026-07-23
- Owners: `application::scheduler`, `ports::MaintenanceStore`,
  `mongo::maintenance`, `server` composition/configuration

## Responsibilities and exclusions

Scheduler owns in-process timing, one-process exclusion, bounded retry, cancellation,
task timeout, lag/failure metrics, and registration of maintenance work. Data-owning
MongoDB adapters own every query and mutation. Scheduler never receives a raw
collection, BSON document, database client, or unrestricted repair surface.

The initial registered tasks are Event retention, hourly-statistics retention,
retained-Issue occurrence lower-bound reconciliation, environment-quota
reconciliation, durable-backlog observation, disabled upload/chunk expiry hooks, and
a typed disabled Blob-orphan hook. The disabled hooks make future ownership explicit
without creating upload, chunk, BlobStore, archive, or artifact implementations.

Phase 14 does not add cold archival, attachments, minidumps, debug files, source maps,
project deletion, migrations, MCP, NATS, sharding, disk spool, a distributed lease,
or a universal cross-store repair scanner.

## Typed work and bounds

`MaintenanceStore` accepts only a typed task, an opaque bounded scan cursor, an
absolute server time, and a finite batch size. It returns scanned/changed counts and
the next cursor. A completed collection pass returns no cursor; a later scheduled run
starts a new bounded pass so retention-policy reductions continue to be enforced.

Event and hourly-statistics scans use `_id` keyset order and fetch no more than the
configured batch. Adapter-side filtering never turns a single invocation into an
unbounded search. Event retention may adjust expiration only for processed Events;
pending `q.s == 0` Events never receive an expiration and are never deleted by the
task. Existing TTL indexes remain the deletion mechanism for processed Events and
hourly buckets.

Occurrence reconciliation raises an Issue count only to the number of currently
retained processed Events found through the accepted project/Issue Event index. It
does not lower the lifetime count because Events may already have expired. Environment
quota reconciliation uses the project-prefixed Environment index. Release/day quota
reconstruction is excluded until an accepted bounded query can distinguish retained
history without an unindexed scan.

## Scheduling, retry, and restart

Each static task has one local lease. A second tick cannot overlap a running task.
Different task failures are isolated, including panics. A failure or timeout keeps the
cursor at the last acknowledged batch and schedules an exponentially backed-off retry
bounded by configuration. Success advances the cursor and restores the ordinary
interval. Missed ticks do not accumulate.

Scheduler state is reconstructible RAM state. Process restart creates empty local
leases/cursors and safely starts another idempotent bounded pass over authoritative
MongoDB data. No durable scheduler collection or migration is introduced.

Shutdown fences new batches and waits only for the configured task timeout/drain
bound. Cancellation cannot make a partially acknowledged batch unsafe because every
adapter mutation is idempotent and a cursor advances only after a successful result.

## Operability and verification

Metrics use only static task and bounded outcome labels. They cover runs, scanned and
changed items, duration, schedule lag, retry delay, timeouts, failures, skipped local
lease attempts, and disabled hooks. IDs, payloads, database names, backend errors, and
cursor bytes are forbidden labels and logs.

The exit gate requires fake-clock deterministic schedules, no overlap, retry/backoff,
failure/panic isolation, process restart, real MongoDB retention safety proving
pending Events survive, gradual bounded passes, indexed query explanations, narrow
counter/quota reconciliation, cumulative foreground ingest while Scheduler runs, one
retained k6 RPS/error-type baseline, full format/lint/workspace tests, and explicit
test-process cleanup.
