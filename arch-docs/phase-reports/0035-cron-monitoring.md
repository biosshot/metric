# Phase 35 report: Cron Monitoring

- Date: 2026-07-27
- Result: complete
- Governing decision: ADR-0045
- Module contract: `module-contracts/0035-cron-monitoring-phase-35.md`

## Delivered

- Added a dedicated bounded Sentry `check_in` envelope lane. It shares DSN/project
  resolution and scrubbing but cannot consume Error, Log or Span writer capacity.
- Added stable `(project, slug, environment)` monitor identity, deterministic SDK
  and missed-run identity, interval schedules and a bounded numeric five-field UTC
  crontab parser.
- Added `in_progress`, `success`, `error`, `timeout` and `missed` lifecycle rules.
  Server receipt time owns deadlines, terminal outcomes are first-wins, and late or
  out-of-order input cannot rewrite a terminal run.
- Added an independent `MonitorWriter` queue with bounded micro-batches, operation
  deadlines and shutdown drain.
- Added `monitors` for bounded definition/current projection and `monitor_runs` for
  compact immutable history. Run documents use binary IDs, integer tags, short BSON
  keys, no raw payload and no BlobStore object.
- Added configurable `retention.monitor_runs_days`, an absolute TTL index and
  project-deletion dataset coverage for both collections.
- Added bounded Scheduler passes for due timeout and missed materialization. A long
  outage emits one deterministic missed run and advances directly to the first
  future schedule instead of backfilling every elapsed period.
- Added monitor outcome rules to the existing Phase 34 Telegram/SMTP durable outbox.
  Provider I/O remains outside ingest and Scheduler.
- Added ProjectRead monitor/history APIs and ProjectAdmin monitor upsert. Added a
  responsive `/monitors` Vue view, project capability control, timeline, custom
  schedule select and monitor outcome selection in `/alerts`.
- Added a pinned real `@sentry/node` Cron sender and a reusable durable Mongo RPS
  runner/comparator.
- Advanced the intentional empty-schema generation to 16. No migration framework,
  Uptime execution, MCP, NATS, sharding or disk spool was added.

## Why two collections

`monitors` has cardinality equal to configured jobs and is updated as a small current
projection. `monitor_runs` has cardinality equal to executions and supplies duration,
history, alert identity and audit evidence. Embedding runs in `monitors` would create
unbounded arrays, hot-document contention and eventual MongoDB document-size failure.

The high-volume collection is therefore optimized independently: compact documents
remain below the retained 256-byte codec budget, TTL removes expired history, batch
writes use deterministic lifecycle ordering, and the current projection is updated
once per monitor per batch rather than once per run.

## Exit gate

| Gate | Evidence |
| --- | --- |
| Duplicate, late and out-of-order check-ins are deterministic | SDK identity derives from monitor plus `check_in_id`; same-batch `in_progress`/terminal writes are ordered; first terminal wins; the real-Mongo gate verifies late terminal duplication. |
| Restart does not create unbounded missed/timeout runs | Timeout is a compare-and-set on the existing in-progress run. Missed identity derives from monitor plus scheduled time, uses `$setOnInsert`, and overdue definitions advance directly past `now`. |
| Clock skew and grace windows are documented and tested | Client wall-clock values do not drive deadlines. Domain tests pin `scheduled + checkin_margin` and `received + max_runtime`; the module contract documents late heartbeat and timeout semantics. |
| Scheduler lag is visible and cannot make ingest unready | Timeout and missed are named maintenance tasks using existing lag/duration metrics. They use the backlog cadence and are excluded from ingest readiness. |
| Retention and deletion cover both collections | `monitor_runs_days` controls SDK and Scheduler expiry; TTL is on the compact absolute delete timestamp. Stable deletion codes cover `monitors` and `monitor_runs`, and the schema/deletion bijection test passes. |
| Real SDK Cron path covers success/error; Scheduler covers timeout/missed | The pinned Node 10.66.0 process sends real `in_progress`, `ok` and `error` items through HTTP. The real-Mongo lifecycle gate uses the same domain records and proves timeout and missed persistence, history and TTL indexes. |
| Check-in flood cannot starve Error/Log/Span | Cron has a separate item class, per-project admission window, queue, writer task, batch and storage permit path. |
| Phase 34 alert integration is durable | Rules select monitor ID plus `error/timeout/missed`; deterministic deliveries enter the existing outbox before a run is marked evaluated. Web exposes the same configuration. |

## Verification

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cargo test -p metric-mongo --test monitors \
  check_in_timeout_missed_and_compact_retention_are_durable \
  -- --ignored --exact --nocapture

cargo test -p metric-server --test sdk_compatibility_e2e \
  real_node_sdk_sends_cron_success_and_error_check_ins \
  -- --ignored --exact --nocapture

cd web
npm run lint
npm test
npm run build
```

One local-Mongo performance test was executed; no HTTP/k6 load was duplicated:

```text
runs=2000
batch=200
elapsed_ms=348
durable_rps=5740.91
```

The baseline is retained under `performance/baselines/cron-monitoring/`. The test
uses and drops a unique MongoDB database. The real SDK harness closes its HTTP task
and Node process; no Phase 35 process remained running.

## Next phase

At this report cutoff Phase 36 Uptime Monitoring was next and Phase 27 was deferred.
ADR-0047 later closed Phase 27 as obsolete without claiming its gates. Non-UTC Cron
timezone databases, MCP, NATS, migrations, sharding and disk spool remain unselected;
Uptime HTTP execution was subsequently delivered by Phase 36.
