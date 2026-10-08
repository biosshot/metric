# PostgreSQL implementation plan and acceptance gates

- Date: 2026-10-08
- Status: Plan only; no PostgreSQL implementation or runtime verification claimed
- Owning decision: [ADR-0052](../0052-postgresql-storage-backend.md)
- Required implementation branch: `feat/postgres`
- Commit convention: Conventional Commits

## Scope controls

Deliver the complete current PostgreSQL backend plus the `EventWriter` rename and
the minimum supporting configuration/composition/test changes. Most new code belongs
to `crates/postgres`. Keep Mongo behavior, data and migrations unchanged. Every Mongo
file edit needs a specific extraction/compatibility reason and regression coverage.
Do not introduce another SQL backend, cross-database migration, new installation
profiles or a PostgreSQL release/Helm deployment in this implementation.

The installer, bundled production Compose configuration, existing profile TOMLs and
default backend remain MongoDB. Add isolated developer/test Docker fixtures for
PostgreSQL, including a containerized Metric test configuration. These are validation
assets, not a new default or a promise of finished PostgreSQL distribution packaging.

### Branch and commit discipline

Before runtime edits, inspect status and current branch, preserve unrelated work,
and create `feat/postgres` from the agreed current base. If that branch already
exists, inspect and reuse it only if it is the same implementation; never reset or
overwrite it. Do not implement on `main`. This documentation-only planning change
does not claim the implementation branch has already been created.

Use reviewable Conventional Commits, for example:

```text
refactor(application): rename MongoWriter to EventWriter
refactor(server): isolate storage initialization from service assembly
feat(postgres): add connection policy and schema migration framework
feat(postgres): implement project and authentication stores
feat(postgres): implement durable events and issue finalization
feat(postgres): implement query and signal stores
feat(postgres): implement retention and lifecycle stores
test(postgres): add Docker fault and security conformance gates
docs(postgres): document backend configuration and verification
```

Split these further as needed. Each commit must compile and retain applicable
passing gates. Keep an incomplete backend inaccessible or fail explicitly until the
runtime path is complete; never install fake success stores or silently fall back to
Mongo. Do not enable a public mode that silently omits advertised functionality.

## Stage 0: Inventory and baseline

- Record base commit, compiler, architecture and current Docker versions.
- Inventory all current port methods and map each to its PostgreSQL implementation
  and shared/adapter test. Check the source at implementation time; ADR-0052's
  snapshot is not permission to omit later-added methods.
- Record current Mongo schema generation and migration registry, and establish a
  real Docker Mongo integration baseline before shared-code changes.
- Verify the SQLx candidate, Rust 1.88 build, lockfile, selected TLS features and
  applicable advisories. Record the exact PostgreSQL image tag/digest. Resolve
  dependency incompatibilities explicitly before implementation proceeds.
- Inventory Mongo assumptions in server/SDK/browser harnesses, configuration,
  serialization, startup progress and writer metrics. Do not merely run tests that
  silently skip because only a Mongo URI was understood.

Exit: dependency decision and capability inventory recorded; baseline failures are
identified rather than misreported as PostgreSQL regressions or passing checks.

## Stage 1: Rename and common composition

- Rename the four writer types and their active callers to `EventWriter*`.
  Update port comments and current architecture references without rewriting old
  phase reports or recorded benchmark data. Document writer metric-name changes.
- Extract one common service assembly function and the private `StorageBundle<E>`
  described in ADR-0052. Keep Event preparation generic and the existing outward
  `EventSink` trait object. Add no universal database interface.
- Share startup progress through a driver-free type and compatibility exports if
  required. Map backend settings at the composition root; move only truly shared
  policy types or codecs when mapping would duplicate substantive logic.
- If body codec extraction is necessary, preserve its version, bytes, bounds and
  round-trip fixtures, plus Mongo imports/exports. Do not make PostgreSQL depend on
  `metric-mongo` just to reuse the codec.
- Add `metric-postgres` to the workspace and dependency-check graph. Share reusable
  conformance fixtures without introducing dependency cycles or duplicating services.

Exit: normal Mongo behavior and existing config still work; writer tests and Docker
Mongo integration pass. Review `git diff -- crates/mongo` for accidental changes.

## Stage 2: Connection policy, configuration and migrations

- Implement the complete TOML contract in ADR-0052 with typed bounded values,
  selected-backend secret resolution, redaction and clear static errors.
- Establish `PgPool` plus a dedicated migration connection when needed. Bound pool
  wait, normal operations, locks and idle transactions. Connection-local settings
  must be reset safely; use transaction-local overrides so a query's deadline cannot
  leak into the next pool borrower. Server-side deadlines enforce query budgets
  even when a Rust future is cancelled.
- Prove plain TCP with explicit acknowledgement, verified TLS with a local CA,
  weak TLS acknowledgement, and rejection of conflicting URI options. Prove no
  outbound HTTP or public certificate service is required.
- Preserve native driver environment defaults (`PGOPTIONS`, `PGSSLROOTCERT`,
  `PGSSLCERT`, `PGSSLKEY`) as approved operator inputs. Warn with variable names
  only; do not fail just because a variable exists. Explicit typed settings retain
  precedence. Verify acceptance and redaction in isolated child processes so tests
  do not mutate the environment of parallel Rust tests.
- Implement initial schema, immutable SQL migration registry and the schema manager
  extension interface for bounded Rust data steps. Hold migration ownership for SQL,
  data and final publication without nested-lock deadlocks.
- Cover empty/current/older/newer/unknown schema, checksum mismatch, concurrent
  startup, interrupted DDL, interrupted data batches, retry and SIGTERM shutdown.
  Use test-only upgrades to prove interfaces for future migrations.
- Support restricted runtime credentials with separate same-target migration
  credentials; with one credential, require only the privileges actually needed.
  `check` mode validates a pre-migrated schema without DDL privileges.

Exit: real PostgreSQL starts without Mongo, migrations recover without data loss,
maintenance probes report honestly, and all connection-policy cases have evidence.

## Stage 3: Core identity and durable Event path

- Implement organization/project/key/DSN mapping, authentication, sessions, tokens,
  membership and audit contracts first. Preserve project IDs and authorization
  decisions; test owner scope for every mutation and lookup.
- Implement Event preparation, batch writes and per-record outcomes. Test duplicate
  input keys, conflicts, rejected records, rollback and ambiguous commit responses.
- Implement backlog discovery, processing-project reads and failure-state writes.
  Verify restart refill with the fresh-event handoff unavailable.
- Implement issue operations and finalization, including hourly/catalog updates and
  notification transition state. Define transaction boundaries, state guards and
  deterministic lock order. Verify retries/concurrency do not duplicate authoritative
  effects or acknowledge missing writes.
- Extend the cumulative E2E path: SDK/HTTP -> EventWriter -> PostgreSQL -> Dispatcher
  -> Processor -> Issue/finalization -> API/UI read.

Exit: the core path is durable and tenant-isolated, including DB/container restart
and connection loss. SQL-specific errors remain behind the port boundary.

## Stage 4: Complete capability coverage

Implement and check every capability in the source inventory, including:

| Area | Ports and behaviors to cover |
| --- | --- |
| Identity | `ProjectStore`, `AuthStore`, DSN mappings and project/key lifecycle |
| Error ingestion | `EventStore`, `EventBacklog`, `ProcessingProjectStore`, `ProcessingStateStore`, `BlobReferenceStore` |
| Issues | `IssueStore`, `FinalizationStore`, counters, states and notifications |
| Investigation | `InvestigationStore`, query/search cursors, issue title search, release/environment catalogs |
| Unified querying | `ExploreStore`, boolean expressions, aggregates, exact filters, export and existing hot/cold orchestration |
| Administration | `ReleaseStore`, `DashboardStore`, saved queries, destinations and audit |
| Other signals | `SignalStore`, `MetricStore`, `SessionStore`, `ReplayStore`, `MonitorStore`, `FeedbackStore` and `FeedbackSink` where used |
| Delivery | `NotificationStore`, claims, retry schedules, outbox and terminal outcomes |
| Binary metadata | `DebugFileStore`, `ArtifactStore`, quotas, recovery and GC references |
| Lifecycle | `ArchiveStore`, `MaintenanceStore`, `ProjectDeletionStore`, retention and cancellation |

Re-use existing Log/Span/Metric/Session/Replay/Monitor writers, parser, normalizer,
grouping, scheduler and services. SQL query compilation belongs only to PostgreSQL.
No new cold-search product surface is introduced by this work; reuse whatever is
implemented on the selected base.

Exit: source-based method inventory is complete, no placeholder/unsupported-success
paths exist, and full server/SDK/browser integration uses the selected backend.

## Stage 5: Retention, privacy, fault and performance gates

Validate the following against real Docker databases, not only mocks:

| Gate | Required evidence |
| --- | --- |
| SQL injection | Quotes, comments, semicolons, Unicode, wildcard and JSON input; every dynamic identifier/operator branch allowlisted; malicious OR expressions cannot escape scope |
| Tenant isolation | Same local IDs in different projects/orgs; wrong-scope read/write/delete/join/aggregate/link requests; background-global operations explicitly accounted for |
| Privacy | Scrubbing before persistence, hashes/ciphertext rather than credentials, bounded decoding, secret-free errors/logs/progress/effective config |
| Durability | Commit acknowledgement, duplicate/conflicting IDs, unknown commit result, retries, process kill, DB restart, rollback and no counter/outbox double-application |
| Pool/deadlines | Exhaustion, cancellation, connection reuse after timeout, bounded memory, no transaction/session setting leakage |
| Migrations | Empty bootstrap; real synthetic SQL and Rust upgrade; checkpoint recovery; competing starters; checksum/newer/unknown schema rejection; readiness false until verified |
| TLS/plaintext | Plain TCP accepted only with flag; verified local CA works; bad CA/hostname fails; weak modes need flag; URI cannot override policy |
| Archive/Blob | Verify-before-source-expiry, references and orphans, interrupted publication, retention holds and manual restore consistency |
| Deletion | Project fencing, concurrent ingest, cancellation/retry, all datasets and blob namespaces, bounded batches |
| Regression | Original Mongo Docker path, original TOML profiles, schema/migration behavior and default installer behavior |
| Resource/load | Sustained and burst writes, duplicates, concurrent Explore, cleanup under ingest, connection count, memory, p95/p99, DB/index size and vacuum impact |

Run a finite reproducible soak (initial target: 30 minutes) on the measured test
hardware and workload. Capture actual limits before claiming parity with Mongo.
Do not reuse ADR-0044's closed historical production gate as a new acceptance claim.
Require SQL `EXPLAIN (ANALYZE, BUFFERS)` evidence for hot query shapes on disposable
representative data. Execute write explains only in explicitly disposable/rolled-back
contexts. No unbounded scans/sorts or tuning away durability to obtain a score.

Use isolated synthetic data. Never publish real credentials, user events or raw
database dumps in CI artifacts. Retain redacted fault logs and reproducible seeds.

## Docker fixtures and execution procedure

Create `deploy/compose.postgres.dev.yml` with:

- `postgres`: exact pinned PostgreSQL 18 patch/digest, healthcheck, synthetic
  credentials and host binding `127.0.0.1:54329:5432`;
- `metric-postgres`: built application image and an explicitly selected test-only
  PostgreSQL TOML fixture, Blob volume, no Mongo service/dependency;
- a TLS test profile with a generated local test CA and server certificate, plus
  a plaintext profile; no paid/external certificate service;
- project-scoped disposable volumes and networks distinct from existing deployments;
- an internal-only runtime network for a no-Internet/no-HTTP-dependency test, with
  both images already loaded before that test. Optional outbound integrations are
  disabled for this case so only database startup/core behavior is measured.

Add a small `scripts/test-postgres.py` coordinator that fails if Docker/DB is absent,
assigns unique disposable test databases, injects test credentials without printing
them, performs TLS/fault/container scenarios and records results. Reuse shared
server/SDK test helpers parameterized by backend. Register an explicit ignored
PostgreSQL integration target group and fail if zero expected cases ran.

The following commands are the intended implementation-time procedure. The new
files/targets do not exist at the planning boundary; finalize exact target names
with the harness and record the actual commands in the verification report.

```powershell
docker version
docker compose version
docker compose -p metric-pg-dev -f deploy/compose.postgres.dev.yml config --quiet
docker compose -p metric-pg-dev -f deploy/compose.postgres.dev.yml up -d --wait postgres
python scripts/test-postgres.py --suite adapter
python scripts/test-postgres.py --suite server
python scripts/test-postgres.py --suite security
python scripts/test-postgres.py --suite migrations
python scripts/test-postgres.py --suite faults
python scripts/test-postgres.py --suite load --soak-duration 30m
docker build -t metric-postgres:local .
docker compose -p metric-pg-dev -f deploy/compose.postgres.dev.yml up -d --wait metric-postgres
python scripts/test-postgres.py --suite container
python scripts/test-postgres.py --suite offline
```

The container suite checks actual health/readiness, config validation, authenticated
ingest and query, restart persistence and graceful shutdown. The offline suite checks
native DB use with outbound networking blocked, including plain TCP and locally
verified TLS. Container restarts use the same synthetic persistent volume; a tmpfs-only
test does not prove restart persistence.

Run original Mongo regression targets using the existing Docker fixture and URI
convention in `.github/workflows/ci.yml`; do not alter Mongo setup to accommodate
PostgreSQL. The PostgreSQL CI job is separate, bounded, and explicitly executes the
ignored real-adapter tests. CI must not report success by omitting these gates.

At completion, stop only task-created Compose projects using the exact project/file
arguments. Keep persistent volumes unless their disposable ownership has been
verified. Do not use global Docker prune or delete an installation's data volumes.

### Repository gates

```text
cargo fmt --all -- --check
cargo dep-graph --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
python scripts/validate-compatibility.py
python scripts/validate-deployment-profiles.py
python scripts/validate-documentation.py
```

Also run the real adapter/server Docker targets, relevant SDK and browser paths,
the application Docker build, and offline SQLx metadata validation if macros are
used. Ordinary Cargo tests alone can skip ignored DB tests and are insufficient.
Use the pinned compiler/container build as the MSRV gate. Re-run applicable gates
after fixes; record unavailable or failing checks without labeling the work complete.

## Delivery and completion criteria

Record a report under `arch-docs/phase-reports/0052-postgresql.md` when implementing,
with commit/image identities, config fixtures, commands, test counts, fault evidence,
resource measurements and remaining limits. Update configuration docs only when the
new keys actually work; do not advertise this planned TOML as currently supported.
Report every changed Mongo file with its reason, and show the original backend still
passes with its original schema and configuration.

Completion requires all capabilities, future-migration interfaces, security and
Docker gates above; PostgreSQL alone must run the application. MongoDB remains the
download/install default. The implementation branch is `feat/postgres`, commits use
Conventional Commits, and merging/publishing is outside this planning request.

## Planning-time environment observation

On 2026-10-08, read-only checks in the user's workspace observed Docker Engine
28.3.2, Docker Desktop 4.44.2 with context `desktop-linux`, and Docker Compose
v2.39.1. The daemon responded and `docker ps` returned no running containers.
No PostgreSQL image was started and no database integration, migration or performance
test was run for this plan. Those remain mandatory implementation gates.
