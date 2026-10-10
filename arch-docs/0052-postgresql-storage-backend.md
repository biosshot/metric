# ADR-0052: Independent PostgreSQL storage backend

- Status: Accepted; implementation in progress on `feat/postgres`
- Date: 2026-10-08
- Delivery contract: [PostgreSQL implementation plan](module-contracts/0052-postgresql-implementation-plan.md)
- Implementation branch: `feat/postgres`
- Scope: PostgreSQL adapter, backend selection, minimal shared plumbing and the
  `MongoWriter` -> `EventWriter` rename
- Default backend: MongoDB, including existing downloads and installers

## Intent and compatibility

One Metric installation selects exactly one operational database at startup:
MongoDB or PostgreSQL. PostgreSQL must operate with no MongoDB server, MongoDB URI
or MongoDB credentials. Changing the configured backend selects a different data
store; it does not transfer existing data. Live switching, replication between
backends, dual writes, Mongo-to-PostgreSQL import and mixed storage are out of scope.

This is an infrastructure adapter, not a new business domain. Existing domain
models, application services, public APIs and capability-specific ports remain the
shared contract. The supported topology remains one active `--role=all` process.
Migration concurrency protection does not introduce active-active support.

This plan extends ADR-0001's future-backend boundary. It records the existing
generic and trait-object dispatch used by the executable code rather than requiring
the historical conceptual enum design to be implemented first. ADR-0034's dependency
direction and ADR-0049's maintenance/readiness guarantees remain applicable.
MongoDB's schema generation, physical data, indexes and migration registry do not
change as a consequence of this work.

Implementation is deliberately PostgreSQL-only. MySQL, MariaDB, SQLite, a generic
SQL adapter, PostgreSQL-compatible distributed databases, new release profiles,
installer selection UI and PostgreSQL Helm packaging are separate future work.
Development and CI Docker fixtures are part of this delivery.

## Existing integration points

The current composition root is `crates/server/src/lib.rs::execute`:

1. `if let Some(uri) = secrets.mongodb_uri.take()` selects Mongo runtime assembly.
2. `MongoProjectStore::connect` creates the Mongo client/database and pings it.
3. `bootstrap_or_migrate` prepares or upgrades the Mongo schema.
4. Factory methods create capability stores sharing the Mongo database handle.
5. `MongoWriter::start(event_store, ...)` starts Event batching.
6. `let writer: Arc<dyn EventSink> = writer` exposes the common application boundary.

`crates/application/src/writer.rs` already declares
`MongoWriter<S: EventStore>` and implements `EventSink` for it. Its queue contains
`S::Prepared`; the Mongo-specific prepared representation belongs to the adapter.
It does not import the Mongo driver. Other services already consume ports such as
`Arc<dyn IssueStore>` and `Arc<dyn ExploreStore>`.

## Ownership and minimal changes

| Location | Permitted responsibility |
| --- | --- |
| `crates/postgres` (new) | Pool, row mapping, PostgreSQL statements, schema, migrations, adapter errors, store implementations and PostgreSQL tests |
| `crates/application/src/writer.rs` and its callers | Rename the Event batching types; preserve batching and acknowledgement behavior |
| `crates/server` | Typed backend config, selected secrets, initialization, shared service assembly and readiness |
| `crates/ports` | Existing operations; narrowly shared startup-progress/configuration types only when necessary |
| Small shared codec module/crate, if needed | Exact existing Event body encoding shared by both adapters, without either driver |
| `crates/testkit` and server tests | Reusable behavioral fixtures and backend-selectable integration setup |
| Workspace/dependency checker/CI/development Docker | Register and validate the new adapter |
| `crates/mongo` | Zero behavioral change; narrowly justified imports/re-exports if a shared codec or startup type must move |

`metric-postgres` depends on `metric-domain` and `metric-ports`, never on
`metric-mongo` or `metric-application`. No SQLx type enters a domain object or
ordinary storage port. Do not copy application logic into the new adapter, copy the
whole server startup block, introduce a generic CRUD API, or duplicate the writers.

Before each necessary edit under `crates/mongo`, record its exact reason in the
commit description. Pure extraction must retain physical bytes, validators, indexes,
error semantics and existing public exports where practical. All Mongo regression
gates remain mandatory. Do not refactor Mongo opportunistically.

### EventWriter naming

Rename `MongoWriter`, `MongoWriterConfig`, `MongoWriterStartError` and
`MongoWriterTask` to `EventWriter`, `EventWriterConfig`, `EventWriterStartError`
and `EventWriterTask`. Keep `writer.rs` and the existing generic contract:

```rust,ignore
pub struct EventWriter<S: EventStore> {
    store: Arc<S>,
    // Existing bounded channel and lifecycle state.
}
impl<S: EventStore> EventSink for EventWriter<S> { /* unchanged behavior */ }
```

`EventWriter` names the data being written, consistently with `LogWriter` and
`MetricWriter`. Backend-specific serialization and batch execution stay in stores.
Preserve `EventStore::Prepared`; do not add boxing or an enum around every prepared
record solely for runtime backend selection.

Rename current writer instrumentation from `metric_mongo_writer_*` to
`metric_event_writer_*`, updating live consumers and documenting the metric-name
change. Do not emit every observation twice. Mongo driver operation metrics remain
Mongo-specific. Preserve historical performance result files and reports; annotate
their old names rather than rewriting recorded evidence.

### Composition without duplicated services

Add a private server-owned `StorageBundle<E>` containing the concrete
`Arc<E>` Event store and the other capability ports as their current trait objects.
The Event generic satisfies `EventStore`, `EventBacklog`, `ProcessingProjectStore`,
`ProcessingStateStore` and `BlobReferenceStore` where assembly requires them.
Both initialization paths return the appropriate bundle; one generic assembly
function starts the common services and yields the existing `RuntimeModules`.

```rust,ignore
let runtime = match config.storage.backend {
    StorageBackend::MongoDb => {
        let stores = initialize_mongo(/* selected config/secrets */).await?;
        assemble_runtime(stores, /* shared dependencies */).await?
    }
    StorageBackend::Postgres => {
        let stores = initialize_postgres(/* selected config/secrets */).await?;
        assemble_runtime(stores, /* shared dependencies */).await?
    }
};
```

The two bundle types need not be assignable to one variable before assembly. Each
branch returns the same non-generic runtime result, including `Arc<dyn EventSink>`.
Move startup progress to a small driver-free representation if necessary, retaining
a compatibility export in Mongo. Convert adapter configuration at composition where
that avoids unnecessary movement of Mongo-owned types.

## PostgreSQL runtime and dependency choice

Use native PostgreSQL connections through SQLx `PgPool`, Tokio and Rustls, with
`default-features = false` and only needed PostgreSQL/runtime/TLS/migration/type
features. Enable checked-query macros only with an offline build strategy. Do not
enable SQLx `Any` or unused database drivers.

The first target server is PostgreSQL 18. Pin an actual patch image and preferably
its digest when implementation starts; record the tested identity. Avoid floating
`latest` images in CI. No mandatory database extensions or hosted service account.

SQLx 0.9.0 and Rust 1.94 are the selected dependency/compiler baseline. On
2026-10-08 the user authorized raising MSRV rather than maintaining a local SQLx
patch: the 0.8.6 candidate incorrectly rejects a valid CA with a mismatched server
name in `verify-ca` with current Rustls. SQLx 0.9.0 includes upstream fix #3861 and
requires Rust 1.94. Apply the compiler baseline consistently to workspace, toolchain,
CI and Docker. Verify the locked graph, TLS behavior and applicable security
advisories; no vendored fork, certificate-validation bypass or TLS downgrade.

After provisioning dependencies/images, runtime startup, migrations and SQL
execution require no HTTP/REST endpoint, API key, external certificate download or
Internet access. Use the native PostgreSQL wire protocol over TCP. Docker service
names, loopback and private-network hosts are valid database endpoints. Do not apply
the user-controlled webhook/uptime SSRF allowlist to the trusted operator DB config.
SQLx query macros, if used, consume checked-in offline metadata for ordinary builds;
only its controlled CI regeneration needs a disposable PostgreSQL instance.

## Proposed configuration contract

These keys are planned and are not accepted by the current binary. Keep existing
`[mongodb]` names and defaults. Omission of `[storage]` means MongoDB, including the
current no-Mongo-URI startup behavior. Existing shipped profiles and installers
remain unchanged. PostgreSQL mode requires its own URI and the existing shared
application secrets, but must not resolve unused Mongo secret references.

```toml
[storage]
backend = "postgres"                 # Default remains "mongodb".

[postgres]
uri = { env = "POSTGRES_URI" }        # Native postgres:// or postgresql:// URI.
bootstrap_timeout = "10s"             # Initial pool connection/schema inspection.
acquire_timeout = "5s"
min_connections = 0
max_connections = 10
statement_timeout = "10s"             # Normal SQL operations; query budgets may lower it.
lock_timeout = "2s"
idle_in_transaction_timeout = "30s"
ssl_mode = "verify-full"
allow_insecure_connection = false
# ssl_root_cert = "/run/certs/postgres-ca.pem"

[postgres.migrations]
mode = "auto"                        # "auto" or "check".
lock_timeout = "30s"                 # One bounded attempt to acquire migration ownership.
statement_timeout = "60s"            # DDL default; explicit migration overrides allowed.
# uri = { file = "/run/secrets/postgres-migration-uri" }
```

| Key | Default and validation |
| --- | --- |
| `storage.backend` | `mongodb`; closed enum `mongodb`, `postgres` |
| `postgres.uri` | Unset; required only when PostgreSQL selected; existing `SecretReference` env/file handling |
| `postgres.bootstrap_timeout` | `10s`; positive, at most `60s`; not a whole-migration deadline |
| `postgres.acquire_timeout` | `5s`; positive, at most `60s`; bounded pool wait |
| `postgres.min_connections` | `0`; no larger than `max_connections` |
| `postgres.max_connections` | `10`; range `1..=128`; measured resource budget still applies |
| `postgres.statement_timeout` | `10s`; positive, at most `5m` |
| `postgres.lock_timeout` | `2s`; positive, no larger than normal statement timeout |
| `postgres.idle_in_transaction_timeout` | `30s`; positive, at most `5m` |
| `postgres.ssl_mode` | `verify-full`; modes specified below |
| `postgres.allow_insecure_connection` | `false`; explicit acknowledgement for weaker modes |
| `postgres.ssl_root_cert` | Unset; optional readable local PEM CA bundle; no remote URL |
| `postgres.migrations.mode` | `auto`; `check` validates without applying missing transitions |
| `postgres.migrations.lock_timeout` | `30s`; positive, at most `5m`; retry with bounded backoff while in maintenance |
| `postgres.migrations.statement_timeout` | `60s`; positive, at most `30m`; individual larger DDL needs a documented bounded override |
| `postgres.migrations.uri` | Unset; optionally separate migration credentials for the same host/port/database; default uses main URI |

Follow CLI -> `APP__...` -> TOML -> defaults precedence. Examples are
`APP__STORAGE__BACKEND` and `APP__POSTGRES__SSL_MODE`; URI values remain secret
references. `--check-config` performs static validation and selected secret
resolution without network access. Effective config redacts both URIs. Unknown
keys are errors, and no supplied inactive-backend key may become a secret-resolution
or connectivity requirement for the selected backend.

The adapter owns one fixed `metric` SQL schema and uses qualified table names.
Do not accept arbitrary schemas or driver `options`/`search_path` strings. Parse a
documented allowlist of URI fields; reject duplicate/unknown policy options. TLS,
timeouts and migration target are controlled by typed settings. Explicit Metric
settings take precedence over the corresponding driver defaults. Preserve the
driver's environment defaults, including `PGOPTIONS`, `PGSSLROOTCERT`, `PGSSLCERT`
and `PGSSLKEY`; their presence is not a configuration error. These are trusted
operator inputs, accepted by the user on 2026-10-08. Emit a warning naming applicable
variables without their values; certificate paths and option contents must not enter
logs. Invalid inherited options can still produce an ordinary connection failure,
but do not reject startup merely because the variables exist. Do not mutate process
environment or silently strip these options. Reject conflicting URI
TLS settings with a redacted error. Keep SQLx connect-option Debug/URL formatting
out of error paths.

### Plain TCP and operator-controlled TLS

TLS is transport encryption for the native database protocol, not HTTP. Ordinary
self-hosted PostgreSQL without TLS is an explicitly supported deployment:

```toml
[storage]
backend = "postgres"

[postgres]
uri = { env = "POSTGRES_URI" }
ssl_mode = "disable"
allow_insecure_connection = true
```

The referenced URI can target `postgres:5432/metric` inside Docker or a private IP.
This mode must work in release builds and must not require development mode,
interactive approval, a paid certificate or a hosted database. Authentication and
authorization remain enabled. The acknowledgement specifically accepts exposure of
traffic to anyone who can observe or modify that network connection.

| Mode | Behavior | Acknowledgement |
| --- | --- | --- |
| `verify-full` | TLS with trusted CA and hostname verification; local CA supported | Not required |
| `verify-ca` | TLS with CA verification but no hostname verification | Required |
| `require` | TLS required, hostname not verified; honor CA verification when a CA is configured | Required |
| `disable` | Plain native TCP, no TLS negotiation | Required |

Do not expose opportunistic `prefer`/`allow` downgrade modes in this initial
contract. Explicit `disable` provides compatibility without silent fallback from a
failed verified connection. Wrong certificates or unsupported server versions yield
bounded, useful errors, not an indefinite connection loop. Compatibility does not
promise support for arbitrary obsolete PostgreSQL versions or insecure TLS ciphers.
Emit one redacted startup warning for an acknowledged weaker mode, not one per
connection or query. Tests must cover the actual selected SQLx TLS semantics.

When a selected protected connection fails TLS negotiation or certificate
verification, startup exits with a redacted permanent error. It does not remain
in the temporary-availability retry loop. Malformed TLS client configuration also
fails startup. Ordinary network unavailability retains the cancellable maintenance
retry policy; failure to verify TLS never enables plaintext fallback.

## Data and query safety

- Bind all values using SQLx parameters, including search text, IDs, timestamps,
  JSON values, limits and cursor components. Never interpolate them with `format!`
  or `QueryBuilder::push`.
- Derive SQL identifiers, sort direction, functions and operators exclusively from
  closed domain enums and static adapter mappings. Parameters cannot stand in for
  SQL identifiers. Explore compiles the already validated `ExplorePlan`; it never
  accepts raw SQL. Escape LIKE wildcard characters when literal matching is required.
- Preserve trusted project/organization scope in every applicable read, write,
  join, aggregation, claim and deletion. Test unparenthesized-OR escape attempts.
  Cross-project links and compound foreign keys include tenant ownership where
  needed; global background operations have explicit internal-only contracts.
- Preserve pre-storage PII scrubbing, password/token hashing and notification-secret
  encryption. The existing HMAC/encryption material is backend-independent. No raw
  ingest bypass, unbounded body/JSON decoding, or payload-containing SQL logs.
  Replay recording privacy limitations remain those of ADR-0046.
- Map SQLSTATE categories to existing port errors; do not expose DB messages,
  conflicting key values, credentials or statements with sensitive values to clients,
  startup progress, ordinary logs or metrics. Metrics use bounded labels.
- Fixed schema qualification and trusted `search_path` prevent writable schemas from
  substituting database objects. Runtime credentials need no superuser/CREATEDB
  privileges. Support a migration owner plus a restricted runtime role, or one
  schema-owning role for a small installation; document the different privileges.
- SQL TLS does not encrypt disks or backups. Document volume/backup protection and
  preserve BlobStore/key consistency in restore instructions. Do not add automatic
  database deletion, reset-on-error, key regeneration or general backup tooling.

## Physical representation and durability

Design tables from operation contracts, not a BSON-shaped JSONB copy of each Mongo
document. Use typed columns for scope, IDs, time, status and indexed projections;
bounded JSONB for suitable flexible metadata; binary Event body encoding in `bytea`.
Domain-valid free strings can contain U+0000, which PostgreSQL text/JSONB cannot
represent. Preserve these exact UTF8 bytes in bounded `bytea` columns and flexible
metadata in the versioned binary JSON codec. Keep normalized search projections
separate from original values. This adapter-owned representation is approved;
do not strip original characters or change shared validation to fit SQL types.
The user approved one title-search edge difference: Mongo's native text index
stops at U+0000, while PostgreSQL searches the complete normalized projection.
Keep Mongo unchanged and do not emulate truncation in PostgreSQL. Normal Issue
creation already replaces controls with spaces in `application::issues`.
Keep nanosecond values as integers where required, without rounding through SQL
timestamp precision. Preserve exact ID bytes, case sensitivity, missing/null
semantics, signed token bit patterns and public cursor ordering.

Create primary/unique/foreign/check constraints with matching adapter error mappings.
Record each index's supported query shape. Search token representation, Issue-title
search and Explore aggregation must be tested against shared behavior; do not assume
PostgreSQL full-text tokenization matches Mongo's title search.

`insert_batch` returns one ordered status per input, including duplicates within a
batch. A failed PostgreSQL transaction cannot report uncommitted rows as inserted.
Prevalidate records and explicitly design atomic batches/savepoints or partitioned
statements as needed for the existing rejection contract. A confirmed commit yields
success; a connection loss while commit may have succeeded yields `Ambiguous`.
Retry only where identity/state guards make it safe. Never disable normal WAL commit
durability to make benchmarks pass.

Define transaction boundaries and lock order for finalization, issue counters,
notification outbox, session/metric upserts and claims. Persist finalization fences
and related authoritative updates consistently so interruption/retry cannot lose or
double-apply those effects. Preserve each signal's existing idempotency contract;
do not claim global exactly-once SDK ingestion (Logs retain their documented rules).
Do not hold SQL transactions across network delivery or BlobStore transfers.

Retention needs an explicit PostgreSQL bounded cleanup implementation: keyset pages,
indexed due times, per-pass work limits, archive holds and project-deletion fences.
Measure delete/vacuum effects. Start with ordinary tables; partitioning is deferred
until evidence justifies it and uniqueness semantics have a reviewed design.
Blob publication, reference commits and orphan cleanup retain their crash windows
and recovery rules; do not pretend SQL transactions also transact blob files.

On 2026-10-09 the user explicitly accepted the confirmed late-Metrics archive
window as a current simplicity tradeoff. A metric bucket can accept updates after
its published snapshot and lose those changes when the hot copy expires. Document
this limitation in both backend acceptance evidence and user-facing known limits;
do not add bucket revisions, automatic re-archival or versioned replacement
segments in this implementation. Its production frequency/volume is unmeasured.
This decision removes that specific discrepancy as a PostgreSQL implementation
blocker. It does not relax verify-before-source-expiry, archive holds on failed
publication, tenant isolation or other durability requirements. Archive defaults
remain disabled, and no additional waiting protocol is introduced.

## Migration interfaces from the first PostgreSQL commit

Use SQLx's embedded SQL migrations for DDL and its immutable applied-version/checksum
ledger. PostgreSQL version 1 is independent of Mongo generation 21. Ship a real
initial schema and test future upgrades with test-only transitions; do not invent
production data transformations to exercise the framework.

A PostgreSQL-owned `PostgresSchemaManager` coordinates inspection, ordered execution,
target verification and safe progress. Its internal migration descriptor carries a
stable version/name/checksum, source preconditions, transaction mode, bounded
execution/retry policy and verification. Provide an extension seam for Rust data
steps with `preflight`, `run_batch(checkpoint, limits)` and `verify` operations.
Checkpoints are bounded and contain no source payloads. A test-only Rust transition
must prove recovery, even if version 1 only uses SQL DDL.

SQLx's DDL ledger and Metric's data-step/checkpoint metadata have distinct roles:
the runtime-ready schema generation advances only after all associated SQL/data
steps and final verification succeed. Never stamp a generation complete just because
the SQL portion finished. All execution and publication share migration ownership on
a dedicated connection; avoid self-deadlocking nested advisory locks. Losing that
connection fences the runner. Checkpoint and data mutation commit atomically where
possible; otherwise steps must be verifiably idempotent on replay.

Transactional DDL rolls back on failure. Any nontransactional step must explicitly
declare detection, verification and repair/retry rules (including invalid indexes).
Newer schemas, changed checksums, unknown steps, an incomplete transition chain or a
nonempty unmarked Metric schema block startup without deleting or adopting data.
The schema manager must not modify other applications' schemas in the same database.

Use the existing maintenance listener: `/live` remains meaningful, readiness stays
false, ordinary routes cannot access the changing schema, and application workers
start only after verification. Transient errors retry with bounded backoff and
cancellable waits; permanent incompatibility reports a redacted stable error.
There is no whole-migration deadline, automatic downgrade or claim of rollback for
arbitrary data transformations. `check` mode fails when an upgrade is needed.

## References

- [SQLx 0.9.0 release and MSRV](https://github.com/transact-rs/sqlx/blob/v0.9.0/CHANGELOG.md)
- [SQLx query binding and raw-fragment warning](https://docs.rs/sqlx/0.9.0/sqlx/struct.QueryBuilder.html)
- [SQLx migration runner](https://docs.rs/sqlx/0.9.0/sqlx/migrate/struct.Migrator.html)
- [SQLx PostgreSQL TLS modes](https://docs.rs/sqlx/0.9.0/sqlx/postgres/enum.PgSslMode.html)
- [Upstream verify-ca Rustls fix](https://github.com/transact-rs/sqlx/pull/3861)
- [PostgreSQL native connection options](https://www.postgresql.org/docs/18/libpq-connect.html)
- [PostgreSQL schema/search-path security](https://www.postgresql.org/docs/18/ddl-schemas.html)

External documentation informs implementation choices; it does not replace the
Docker-backed verification contract linked above.
