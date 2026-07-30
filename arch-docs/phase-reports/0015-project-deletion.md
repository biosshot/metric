# Phase 15 report: project deletion and core capacity protection

> Historical test evidence, not an upgrade runbook. Its development-database
> recreation language must never be applied to data-bearing installations. The
> current binary requires generation 19; see
> [the current upgrade runbook](../../docs/upgrading.md).

- Status: exit gate passed; Phase 16 not started
- Date: 2026-07-23
- Scope: ADR-0039 Phase 15 only
- Implementation commit: `44ac7a189f4d7dacb6caf0c958733ca67342adb8`

## Contract and implementation

The accepted contract is
`module-contracts/0015-project-deletion-phase-15.md`.
`application::deletion` owns the command workflow, exact confirmation, local
durable-ingest fence/drain, worker timing, timeout, and bounded retry.
`ports::ProjectDeletionStore` is a typed lifecycle/batch boundary.
`mongo::deletion` owns the durable job, append-only dataset codes, cursors,
collection classification, indexes, cleanup, release detachment, and tombstone.

The implemented lifecycle is:

```text
active | disabled
        |
        v
pending_delete -- cancel --> previous project state
        |
        v
     purging
        |
        v
deleted permanent tombstone
```

Delete requires `project:admin`, CSRF for Web, an exact project slug, and a
16-byte idempotency key. The job is durable before active keys are changed to
`suspended_by_deletion`. Repeating the operation returns the same job and
repairs a partial fence. Cancellation is accepted only during grace, restores
only keys suspended by deletion, and never re-enables keys that were already
disabled.

The local work registry rejects new durable ingest after the fence and drains
already-entered writes up to the configured bound. Processor's existing
authoritative project-state load stops work for non-active projects. The Mongo
worker processes at most one finite `_id` page per tick. It deletes Events,
Issue activity, hourly statistics, Issues/search projection, Environments, and
keys; shared Releases lose only the deleted project association. It then
repeats the complete plan before compacting the project in place.

`project_deletions` persists phase, plan version, stable numeric dataset code,
cursor, reconciliation marker, grace/retry/completion timestamps, attempts,
bounded error code, terminal retention, and slug reservation. The deleted
project keeps a non-TTL tombstone. The old slug is reserved for the configured
period and then removed. Request/cancel actions append retained audit records.
Capabilities and status expose whether the worker is running and the effective
bounded policy.

The dataset registry classifies all 17 bootstrap-owned collections exactly
once. The filesystem namespace registry is explicitly empty because Phase 15
has no filesystem producer. The existing ingest backlog count/age guard remains
the admission-capacity fence. Phase 16 Blob/filesystem behavior was not started.

Schema generation is now 3. This is an initial-schema revision, not a migration:
generation-2 databases fail closed and must use a fresh database name or
explicitly recreated development data.

## Exit gate

| ADR-0039 Phase 15 gate | Evidence | Result |
| --- | --- | --- |
| Delete/cancel authorization | Native route permission matrix pins DELETE, status, and cancel to `project:admin`; Native API calls tenant/project authorization before the deletion service; Web hides the danger zone without that permission | pass |
| Exact confirmation and idempotency | Application requires the current exact slug; HTTP requires a 32-hex `Idempotency-Key`; real Mongo test repeats the same operation and receives the same durable operation | pass |
| DSN key restoration rules | Real Mongo test proves active -> suspended -> active on cancel while an already disabled key remains disabled | pass |
| Crash/restart at every phase | Real Mongo test reconstructs a new adapter before every persisted dataset/cursor step and reaches the same tombstone | pass |
| Idempotent batch repetition | Deletes, shared-release detachment, cursor updates, fence repair, and tombstone completion are identity-fenced; the restart test repeats persisted boundaries safely | pass |
| In-flight ingest fence and drain | Deterministic application test blocks an entered durable sink, proves delete drain waits, then proves new ingest is rejected until cancel unfences it | pass |
| Processor fence and final rescan | Processor's project-state gate remains active; real Mongo test inserts a late Event after pass one starts and proves pass two removes it before tombstone | pass |
| Large deletion with another active project | The single release-mode performance test deletes 20,000 project Events while 2,000 Events are concurrently inserted for another active project; all active-project data remains | pass |
| Complete collection/namespace classification | Unit test compares the versioned registry with every required schema collection and fails on missing/duplicate classification; filesystem namespace ownership is explicitly empty until Phase 16 | pass |
| Bounded cleanup and permanent tombstone | Every worker call handles at most one configured page; the integration test verifies Event/key cleanup, shared Release preservation, compact tombstone, and later slug release | pass |
| Operational status and audit | API status returns phase/code/reconciliation/attempt/next/error fields; component status/capabilities expose the worker; request/cancel use retained audit actions | pass |
| Capacity protection | Existing bounded backlog hysteresis remains in front of durable admission; deletion adds finite batches, one non-terminal job per project, timeouts, retry cap, and no filesystem work | pass |

Milestone D is complete: the core Error tracking product now has bounded
retention, maintenance, deletion, overload behavior, and standard operability.

## Performance baseline

Exactly one Phase 15 performance test was run:

```text
target: bounded deletion throughput in RPS
dataset: 20,000 Events for the deleted project
batch: 1,000 documents
interference: 2,000 concurrent Event writes for another active project
storage: native local MongoDB on Windows
```

| Metric | Result |
| --- | ---: |
| Purge RPS | 16,445.32 |
| Deleted documents | 20,000 |
| Elapsed | 1,216 ms |
| Batch p95 | 64 ms |
| Deleted-project documents remaining | 0 |
| Concurrent active-project documents | 2,000 |
| Active-project loss | 0 |

The regression artifact is
`performance/baselines/project-deletion/ryzen-5600h-windows-mongodb-v1.json`.
This is a local AMD Ryzen 5 5600H Windows baseline, not a server-tuned
production capacity claim.

## Verification

The final gate passed:

```text
cargo fmt --all -- --check
cargo dep-graph --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --quiet
cargo test -p metric-mongo --test project_deletion \
  infrastructure_project_deletion_cancel_restart_rescan_and_tombstone \
  -- --ignored --exact --nocapture
cargo test --release -p metric-mongo --test project_deletion \
  performance_project_deletion_bounded_purge_rps \
  -- --ignored --exact --nocapture

npm run format:check
npm run lint
npm test
npm run build
npm run test:e2e
```

Results:

- full Rust workspace passed; Phase 15 adds two application tests, two registry
  tests, one real-Mongo functional test, and one ignored release benchmark;
- ADR-0034 dependency direction passed;
- real native-Mongo deletion/restart/final-rescan isolation passed;
- Web passed 10 unit/component tests;
- Playwright passed 12 scenarios across Chromium and Firefox;
- production Web bundle is 61.54 KiB gzip JavaScript and 4.71 KiB gzip CSS;
- one and only one performance test was run in this pass;
- no Cargo, rustc, k6, Metric server, Playwright, or Vite process started by
  the pass remained after verification; the pre-existing local MongoDB service
  was not stopped.

## Known limits

- Version one runs the deletion worker in the all-in-one role with one local
  process. A distributed lease belongs to a future multi-role architecture.
- Terminal job status expires after the configured retention, while the project
  tombstone and retained audit remain permanent.
- The Phase 15 filesystem namespace registry is empty. BlobStore capacity,
  attachment/minidump cleanup, disk reserve, and filesystem failure matrices
  belong to Phase 16 and were not preimplemented.
- Generation-2 databases are intentionally incompatible because migrations
  remain out of scope.
