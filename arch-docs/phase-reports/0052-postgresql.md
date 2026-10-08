# PostgreSQL implementation verification

- Date: 2026-10-08
- Status: In progress; complete PostgreSQL runtime parity is not claimed
- Branch: `feat/postgres`
- Base: `15a5cf0f46a6f315105d3c566bd0a8b0468bc6c2`
- Plan: [implementation gates](../module-contracts/0052-postgresql-implementation-plan.md)

## Environment and dependency decision

Windows host, Rust/Cargo 1.88.0, workspace MSRV unchanged. Docker Desktop 4.44.2,
Engine 28.3.2, Compose 2.39.1, `desktop-linux` context.

PostgreSQL image: `postgres:18.6-bookworm`, pinned to
`sha256:afc7e2d441324c0388fa80c3d24f733b4194a4eb7f47dd8ee2b08eb1a24a647c`.
Task-owned fixture: project `metric-pg-dev`, `deploy/compose.postgres.dev.yml`,
host port `127.0.0.1:54329`, synthetic credentials and a persistent Docker volume.

SQLx is pinned to 0.8.6 with default features disabled; PostgreSQL, Tokio, Rustls
ring/webpki, migrations, JSON, time and UUID enabled. An isolated dependency
preflight and the actual crate both compile on Rust 1.88. Only the PostgreSQL
driver is compiled. Optional MySQL/SQLite dependencies can appear in Cargo's lock
resolution; this does not enable those backends.

Applicable advisory review so far: SQLx's RUSTSEC-2024-0363 is fixed in the selected
version (patch >=0.8.1). The existing locked time 0.3.54 is newer than the fix for
RUSTSEC-2026-0009 (>=0.3.47). Full applicable dependency audit remains a delivery
gate; this review is not a claim that every dependency has been audited.

## Shared-code regression

`MongoWriter` and related types are now `EventWriter`. Current writer metrics use
`metric_event_writer_*`; active benchmark parsers were updated, historical reports
and stored performance measurements retained. Existing queues/batching/handoff
behavior and generic Event preparation are preserved.

Private `StorageBundle<E>` and one generic service assembly function isolate
backend construction. Mongo's existing factories construct its complete bundle.
No universal CRUD interface or driver dependency was added to application/domain.

Completed commits before adapter implementation:

- `51d0e01 docs(postgres): record adapter architecture and implementation gates`
- `ce760fe refactor(application): rename MongoWriter to EventWriter`
- `227d3a4 refactor(server): isolate storage initialization from service assembly`

Real Docker Mongo baseline (`metric-pg-baseline` project, existing development
fixture, Mongo 8.0.12): 19 integration/migration cases passed, performance targets
excluded. Writer tests: 22 passed, 2 performance cases ignored. After rename and
after common assembly extraction, the real Mongo server suites each passed all
5 selected cases (3 durable-ingest E2E, 2 native-API E2E).

Commands use `METRIC_TEST_MONGODB_URI` with synthetic fixture credentials:

```text
cargo test -p metric-mongo --all-targets --all-features --locked --no-fail-fast -- --ignored --skip performance_ --skip reports_rps --test-threads=1
cargo test -p metric-application writer:: --locked
cargo test -p metric-server --test durable_ingest_e2e --test native_api_e2e --locked -- --ignored --skip performance_ --test-threads=1
cargo check -p metric-server --all-targets --locked
cargo clippy -p metric-server --all-targets --locked -- -D warnings
```

No files in `crates/mongo` have been changed. Mongo schema remains generation 21;
the PostgreSQL schema has its own generation, unrelated to Mongo's registry.

## PostgreSQL foundation evidence

The new adapter owns the native pool, redacted errors, bounded connection settings,
strict URI option validation, explicit weak-transport acknowledgement and embedded
SQL migrations. Normal statement logging is disabled. User values are bound.

One dedicated migration connection owns a session advisory lock across SQL,
transactional data batches and final generation publication. SQLx's nested lock is
disabled on that already-owned connection. SQL ledger and data checkpoints live
under `metric`. Data writes and checkpoints share a transaction; readiness generation
is published only after verification. Check mode does not create schema/tables.

On 2026-10-08 the user accepted SQLx's native operator environment defaults
`PGOPTIONS`, `PGSSLROOTCERT`, `PGSSLCERT`, `PGSSLKEY`. Their presence is accepted;
warnings include names only. Explicit Metric settings override their corresponding
driver defaults. A subprocess test verifies an inherited server option actually
takes effect, all four names produce warnings, and their values do not enter logs.

Real Docker command (`METRIC_TEST_POSTGRES_URI` set to the synthetic fixture):

```text
cargo test -p metric-postgres --lib --locked -- --include-ignored --test-threads=1
```

First foundation run: 10 passed, 0 failed, 0 ignored, including 8 real Docker cases:
empty/current/check schema, checksum/newer rejection, unknown schema protection,
concurrent migration ownership and deadline, failed transactional DDL rollback,
synthetic future SQL + Rust upgrade/checkpoint recovery, environment acceptance and
redaction, pool exhaustion/SQL timeout/transaction-local setting reset. Two static
tests cover URI option privacy and weak-mode acknowledgement.

These are foundation checks. Verified TLS, separate restricted credentials,
SIGTERM/container faults, all capability stores, full PostgreSQL server/SDK/UI,
Docker image/offline runtime and measured 30-minute load/soak gates are still pending.
The application cannot select a partially implemented PostgreSQL backend yet.
