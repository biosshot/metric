# Phase 14 report: Scheduler, retention, counters, and narrow reconciliation

- Status: exit gate passed; Phase 15 not started
- Date: 2026-07-23
- Scope: ADR-0039 Phase 14 only
- Implementation commit: `1710d99993d65437765971a768b9fe5ea0db3da5`
- Preserved user changes:
  `8d3961f76b448dcb140c986141b9752cba6208f9`,
  `cab4edcdc46f179b8a5adb09ac9ba7f5257fd735`

## Contract and implementation

The accepted contract is
`module-contracts/0014-scheduler-retention-phase-14.md`.
`application::scheduler` owns only timing, static task registration, one-process
exclusion, timeout, bounded retry/backoff, cursor advancement, cancellation, and
low-cardinality metrics. `ports::MaintenanceStore` exposes typed bounded work.
`mongo::maintenance` owns BSON, indexes, scans, and mutations.

The six registered tasks are:

```text
retry backlog observation
Event retention
hourly-statistics retention
Issue/environment counter reconciliation
disabled upload/chunk expiry hook
disabled Blob-orphan registration hook
```

All work uses finite batches and opaque cursors. Event and hourly policy reductions
walk `_id` keyset pages. Processed Events receive a corrected absolute TTL date.
Expired permanently-failed Events are deleted by an exact-identity fenced operation.
Pending `q.s == 0` Events are neither assigned an expiration nor deleted. MongoDB TTL
remains responsible for processed Event and hourly-bucket deletion.

Issue reconciliation only raises the approximate lifetime count to a bounded retained
Event lower bound; it never lowers lifetime history after raw retention. Environment
quota usage is reconciled through its project-prefixed index. Release/day quota
reconstruction remains excluded because the accepted index set cannot safely
reconstruct it without a potentially unbounded scan.

The all-in-one composition starts Scheduler before readiness and joins it during
shutdown. `[scheduler]` and `[retention]` are validated, redacted effective
configuration. Finalizer and Scheduler consume the same retention snapshot. No
collection, validator, schema generation, migration, BlobStore, upload service,
archive, MCP, NATS, sharding, or disk spool was added.

`/api/v1/capabilities` now exposes the effective safe global retention policy and
reports retention enabled only when the Mongo-backed Scheduler is running. Project
Settings shows the read-only effective Event/hourly durations, receipt-time clock,
gradual reductions, and pending-Event protection without implying a per-project
override was saved.

## Exit gate

| ADR-0039 Phase 14 gate | Evidence | Result |
| --- | --- | --- |
| Fake-clock deterministic schedules | Scheduler tests advance an atomic fake clock and verify normal intervals, incomplete-pass polling, and retry due times without wall sleeps | pass |
| Retention never deletes pending Events | Real MongoDB test scans with batch size 1; a 40-day-old pending Event remains with `q.s == 0` and no `x`, while processed TTLs are reduced and an expired permanent failure is removed | pass |
| Task failure isolation | A scripted task panic is caught; other due task kinds complete and the failed kind retries after bounded backoff | pass |
| Process restart | A new Scheduler reconstructs empty leases/cursors and safely repeats idempotent task work against the same store | pass |
| Foreground ingest during maintenance | One fixed-arrival k6 run writes to real native MongoDB while Scheduler scans the same Event collection every 100 ms | pass |
| Bounded scans and no unindexed unbounded work | Real MongoDB test asserts per-call scan bounds and execution plans for `_id_`, `event_issue_timeline`, and `environment_project_timeline` | pass |
| Counters and quota reconciliation | Real MongoDB test raises an Issue retained lower bound from 1 to 2 and restores project environment usage from absent to 1 | pass |
| Hooks for deferred modules | Upload/chunk expiry and Blob-orphan task kinds return an explicit disabled disposition and create no optional-module state | pass |
| Standard operability | Static task/outcome metrics cover lag, duration, scanned/changed items, retry delay, timeout, panic, store failure, local lease contention, and disabled hooks | pass |

## Performance baseline

Exactly one Phase 14 performance test was run:

```text
target: 1,158 RPS for 15 seconds
path: k6 -> HTTP Envelope -> MongoWriter -> native MongoDB
interference: concurrent Phase 14 Scheduler on the same database
```

Recorded result:

| Metric | Result |
| --- | ---: |
| Achieved RPS | 1,156.73 |
| Iterations | 17,370 |
| Average latency | 15.36 ms |
| p95 | 26.00 ms |
| p99 | 30.31 ms |
| Dropped iterations | 0 |
| TCP errors | 0 |
| HTTP responses | 17,370 |
| HTTP 200 | 17,370 |
| HTTP 429 | 0 |
| HTTP 503 | 0 |
| Other HTTP status | 0 |
| MongoDB Events after the run | 17,370 |
| Acknowledged loss | 0 |

The reviewed artifact is
`performance/baselines/scheduler-maintenance/ryzen-5600h-windows-k6-v1.json`.
This is a native-MongoDB Windows regression sentinel on an AMD Ryzen 5 5600H, not a
server-tuned production capacity claim. The benchmark process was stopped, the fresh
database was dropped, and no k6/server/Cargo/Rust test process remained.

## Verification

The final gate passed:

```text
cargo fmt --all -- --check
cargo dep-graph --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --quiet
cargo test -p faultkeep-mongo --test maintenance_store \
  infrastructure_retention_pending_safety_reconciliation_and_bounded_plans \
  -- --ignored --nocapture
cargo test -p faultkeep-server --test web_e2e \
  infrastructure_browser_login_session_csrf_and_project_isolation \
  -- --ignored --nocapture

npm run format:check
npm run lint
npm test
npm run build
npm run test:e2e
```

Results:

- 4 focused deterministic Scheduler tests passed;
- full Rust workspace tests passed;
- real native-MongoDB Phase 14 integration passed;
- real browser/Rust/native-MongoDB cumulative integration passed;
- 9 Web unit/component tests passed;
- 12 Playwright scenarios passed across Chromium and Firefox;
- production Web bundle is 60.66 KiB gzip JavaScript and 4.67 KiB gzip CSS;
- the real `@sentry/node` compatibility gate still passed after preserving the
  user's async sender entrypoint;
- configuration validation and redacted output include the Scheduler/retention
  sections and preserved local port `4001`.

## Known limits

- Version one currently exposes one global startup retention policy. A per-project
  override requires an explicit typed project-policy/schema decision and was not
  invented in Phase 14.
- Retained Event lower-bound reconciliation cannot reconstruct deleted lifetime
  history and intentionally cannot correct accepted positive overcount drift.
- Release/day catalog usage is not rewritten until an accepted bounded indexed query
  exists.
- Disabled upload/chunk and Blob hooks are registrations only. They do not begin
  Phases 16-18.

Phase 15 may now start with project deletion and core capacity protection. No Phase
15 module was introduced.
