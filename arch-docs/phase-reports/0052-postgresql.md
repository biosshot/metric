# PostgreSQL implementation verification

- Date: 2026-10-08
- Status: In progress; complete PostgreSQL runtime parity is not claimed
- Branch: `feat/postgres`
- Base: `15a5cf0f46a6f315105d3c566bd0a8b0468bc6c2`
- Plan: [implementation gates](../module-contracts/0052-postgresql-implementation-plan.md)

## Environment and dependency decision

Windows host. Initial baseline used Rust/Cargo 1.88.0; the user authorized raising
workspace MSRV and the development/CI/Docker toolchains to Rust 1.94 on 2026-10-08
to use upstream SQLx 0.9.0 without a local fork. Docker Desktop 4.44.2, Engine
28.3.2, Compose 2.39.1, `desktop-linux` context.

PostgreSQL image: `postgres:18.6-bookworm`, pinned to
`sha256:afc7e2d441324c0388fa80c3d24f733b4194a4eb7f47dd8ee2b08eb1a24a647c`.
Task-owned fixture: project `metric-pg-dev`, `deploy/compose.postgres.dev.yml`,
host port `127.0.0.1:54329`, synthetic credentials and a persistent Docker volume.

SQLx is pinned to 0.9.0 with default features disabled; PostgreSQL, Tokio, Rustls
ring/webpki, migrations, JSON, time and UUID enabled. The initial 0.8.6 preflight
compiled on Rust 1.88 but failed the Docker `verify-ca` hostname-mismatch test with
Rustls 0.23.42. SQLx 0.9.0 includes upstream fix #3861 and requires Rust 1.94.
Adapter all-target compilation and all 22 tests passed on the new compiler and
SQLx version, including that regression. Only the PostgreSQL driver is compiled.
Optional MySQL/SQLite dependencies can appear in Cargo's lock resolution; this
does not enable those backends.

SQLx 0.9's `SqlSafeStr` is used for embedded static migrations. Migration-table
arguments are fixed to `metric._sqlx_migrations`, never operator-controlled SQL.
The only `AssertSqlSafe` uses are test-database CREATE/DROP statements with
internally generated and validated identifiers. User data remains bound.

Applicable advisory review so far: SQLx's RUSTSEC-2024-0363 is fixed in the selected
version (patch >=0.8.1). The existing locked time 0.3.54 is newer than the fix for
RUSTSEC-2026-0009 (>=0.3.47). Full applicable dependency audit remains a delivery
gate; this review is not a claim that every dependency has been audited.

The follow-up [Rustls advisory RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html)
requires >=0.23.45. The lockfile now selects Rustls 0.23.45 and rustls-webpki
0.103.15 (also above the fixes for RUSTSEC-2026-0049, -0099 and -0104).
The final graph passed all 22 PostgreSQL tests, including verified TLS and
corruption checks. On 2026-10-08 the user also authorized mechanical Clippy fixes
for the compiler upgrade, including two nested-if expressions in Mongo artifact
and session code. Those expressions now use equivalent let chains; strict lint
settings remain enabled. Whole-workspace/all-target/all-feature Clippy passed with
warnings denied. Ordinary workspace tests passed: 352 successful, 0 failures,
100 infrastructure/SDK/performance cases ignored in that particular run.

The full Docker image built with Rust 1.94 and the final lockfile. Its version
command succeeded with Docker networking disabled. This proves the existing
runtime image builds offline at startup; PostgreSQL server runtime selection is
still pending and is not exercised by that image yet. Parallel Windows tests and
the release image build exceeded available host memory on the first attempt;
the serial `-j1` workspace test retry succeeded.

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

Before the explicit active-owner exception, no files in `crates/mongo` were changed.
Mongo schema remains generation 21;
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

## Project identity store

The first complete capability is `ProjectStore` (all 13 methods, including the
port's default methods). A source inventory is tracked in
[the capability table](../module-contracts/0052-postgresql-capability-inventory.md);
the delivery gate is `python scripts/validate-postgres-coverage.py --require-complete`.
At this stage the table contains 164 methods, 13 implemented, 151 pending. This
source count does not claim behavior coverage for pending capabilities.

Schema generation 2 adds organizations/projects, native keys and DSN aliases.
Native keys and aliases share one unique reservation namespace, closing concurrent
shadowing races. Foreign keys validate ownership. Project-first transaction locks
serialize bounded key creation, policy CAS updates and lifecycle state changes.
Values, including names and malicious filter patterns, are bound SQL parameters.
No changes were made to Mongo's schema or source.

Real Docker run after these changes: 13 passed, 0 failed/ignored (11 real database
cases, 2 static policy checks). New cases cover identity round trips and organization
scope, wrong-project key mutation, state/suspension restoration, duplicate identities
and slugs, concurrent key limit/CAS enforcement, inbound filter persistence, alias
target scope and concurrent alias/native reservation collisions. All-target adapter
Clippy with warnings denied also passed. The earlier foundation tests were rerun
against the enlarged registry; future synthetic upgrades use the next generation.

## Authorized active-owner exception and authentication

On 2026-10-08 the user authorized one Mongo behavior change: disabled users must
not count toward the last-active-owner guard. Mongo changes are limited to
`src/auth.rs`, regression wiring in `tests/auth_store.rs`, and a test-only
`metric-testkit` dependency. Mongo schema, migrations, other stores and production
composition remain unchanged.

Under the existing Mongo organization auth lock, the guard now finds another owner
joined to an enabled user, scoped to the same organization and excluding the target.
The indexed user lookup stops after one qualifying owner. Removing/demoting or
re-disabling an already-disabled owner does not reduce the active-owner count.
Failed checks release acquired organization locks. PostgreSQL applies the same
rule inside its existing identity transaction.

One shared real-adapter conformance scenario covers sequential and concurrent
disabling, wrong-organization owners, last-owner demotion/removal, repeated disabling,
disabled-owner cleanup and restoration. It passed against both Docker databases.
Mongo's original authentication integration and the new case both passed (2/2).

`PostgresAuthStore` implements all 28 methods. Schema generation 3 adds users,
memberships, setup-token digests, web sessions, personal API-token digests and audit
records. Multi-record bootstrap, invitation/password setup and owner changes are
transactional. Control-plane identity changes share a bounded transaction advisory
lock; hot Event writes, reads and credential lookups do not take that lock.

PostgreSQL adapter run: 20 passed, 0 failed/ignored (18 real database cases plus
2 static policy checks). Authentication cases cover bootstrap competition/retry and
rollback, canonical email, invitation reissue, one-use/expired setup credentials,
password changes, owner concurrency, session/token round trips and revocation,
organization/user scope, digest-only persistence contracts and idempotent audit.
Capability signatures now stand at 41/164; remaining capabilities and full runtime
integration are still pending.

## SQLx 0.9 / Rust 1.94 follow-up verification

After the dependency/security update and authorized compiler-style fixes, the
real Docker PostgreSQL suite passed 22/22 with no skipped cases. That includes
the two added TLS cases: verified local CA, actual hostname mismatch rejection,
`verify-ca` hostname tolerance with CA validation, wrong/missing CA rejection,
`require` with an explicit CA, and inherited native client certificate settings.
The audit decoder also rejects unknown/duplicate metadata keys and overlong values.

The Mongo Docker regression passed 20/20: three migration cases and seventeen
store cases, with performance targets deliberately excluded. Schema generation
remains 21. The server Docker regression also passed 5/5 (three durable-ingest
cases and two native API cases). The real Rust SDK fixture built with its existing
lockfile on Rust 1.94, and `real_rust_sdk_sends_an_error_event` passed 1/1 against
the shared protocol harness. This is a toolchain/protocol regression check, not
a PostgreSQL server/SDK acceptance claim. The Rust compatibility row now records
this tested toolchain and server build. A separate PostgreSQL integration/TLS CI job now invokes the actual
ignored database tests with ephemeral local certificates and task-owned cleanup;
the remote GitHub Actions job itself has not been executed in this local session.

```text
cargo test -p metric-postgres --all-targets --all-features --locked -j1 -- --include-ignored --test-threads=1
cargo test -p metric-mongo --all-targets --all-features --locked --no-fail-fast -j1 -- --ignored --skip performance_ --skip reports_rps --test-threads=1
cargo test --workspace --all-targets --all-features --locked -j1
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
python scripts/validate-documentation.py
python scripts/validate-deployment-profiles.py
python scripts/validate-compatibility.py
docker build --tag metric:postgres-rust194 .
docker run --rm --network none metric:postgres-rust194 --version
```

The verified authentication/TLS and dependency upgrade milestone is committed as
`642929e feat(postgres): add authentication and verified TLS with SQLx 0.9`.

## Durable Event capability family

Schema generation 4 adds project processing revisions, durable Error Events and
one project/Event/object-scoped Blob reference registry. `PostgresEventStore`
implements every method of `EventStore`, `EventBacklog`, `ProcessingProjectStore`,
`ProcessingStateStore` and `BlobReferenceStore`. No application/domain/port or Mongo
changes are required for these capabilities. The existing `EventWriter` is used
directly in a real adapter test; the application dependency is test-only.

Preparation validates payload identity and bounded canonical JSON, adaptively
compresses with a versioned PostgreSQL-owned zstd format, bounds decompression,
deduplicates owned Blob references and accounts for reference bytes in batch size.
Bulk arrays keep SQL parameter count constant. Deterministic project/key lock order
and one transaction protect insertion, references and deletion fencing. Duplicate
keys preserve the first committed body and references. A lost COMMIT reply returns
`Ambiguous`; a retry finds the existing scoped identity rather than overwriting it.
Processing failures use an attempts/state CAS. Disabled projects remain processable;
deletion-fenced projects are excluded from refill and reject new writes.

The refill page bound matches the existing Dispatcher maximum (32,768); its
exclusion bound separately covers the supported maximum of 100,000 queued keys
plus 4,096 running workers. Tenant predicates also remain present in key/reference
lookups. Only bounded global backlog discovery and observation operate globally.

The enlarged Docker PostgreSQL suite passed 30/30, with no ignored cases (28 actual
database cases and two static connection-policy cases). Eight Event cases cover
conflicting/repeated identities, same Event ID in different organizations/projects,
missing/fenced projects, a fresh pool recovering durable backlog, compression and
identity corruption, reference/batch size bounds, concurrent attempts CAS,
permanent failure, real EventWriter use with a rejected handoff, and project locks.
A PostgreSQL trigger injects failure after Event insertion and proves both Event and
reference writes roll back. A bounded native TCP proxy drops an actual PostgreSQL
`COMMIT` completion response after the server has committed, proving ambiguous
acknowledgement and idempotent recovery with both body and references intact.

`EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` on 10,000 real prepared synthetic rows
(9,900 completed and 100 pending) uses `event_pending_due` for the bounded refill
and returns the requested ten rows. The first assertion incorrectly compared an
EXPLAIN JSON floating-point row count to an integer JSON value; it was corrected
to numeric comparison, and the entire 30-case suite then passed. This test does
not set a throughput or production resource claim. A shared two-organization
fixture now avoids duplicating identity setup across PostgreSQL capability tests.

Follow-up Event checks also passed 8/8 after accepting an exclusion list of 40,000
keys from a supported Dispatcher configuration and guarding encoded payload size
in SQL before client-side materialization. An oversized corrupt row produces a
static `InvalidData` result without transferring that body's bytes to the adapter.

The source inventory is now 48/164, with 116 methods still pending. Issue/finalizer,
other signals, querying, lifecycle, restricted credentials, full PostgreSQL runtime,
container/SDK/browser acceptance and measured soak remain required delivery gates.

## Next representation decision: valid NUL-bearing strings

Implementation stopped before starting Issue/finalizer storage to honor the user's
explicit instruction to stop and ask about newly discovered compatibility issues.
`domain::issue::IssueRelease::new`, `domain::releases::validate_version` and
`application::normalizer::identity_string` validate byte bounds but accept U+0000
within release/environment strings. Mapping all such values directly to native
SQL `text` or `jsonb` would reject otherwise valid existing input. The already
implemented binary Event body format does not have this limitation. Existing
project display names, inbound filters and bounded audit metadata reject control
characters in their domain constructors and do not require the same adjustment.

Two read-only probes on the task-owned PostgreSQL 18.6 Docker fixture confirmed
the mismatch: converting bytes `610062` to SQL text returns an invalid UTF8 byte
sequence at `0x00`; casting `{"release":"a\u0000b"}` to `jsonb` returns an
unsupported Unicode escape error. This matches the
[PostgreSQL JSON documentation](https://www.postgresql.org/docs/18/datatype-json.html)
and [character-type documentation](https://www.postgresql.org/docs/18/datatype-character.html).
No user database data, constraints or schemas were changed by those probes.

The user approved the following approach: preserve unconstrained original
UTF8 values and canonical metadata in bounded binary/escaped representations owned
only by `crates/postgres`, with parameter binding and indexed exact-match keys.
Keep searchable projections separate from original values and verify their public
search behavior against shared adapter fixtures. Continue using native text/JSONB
where existing domain validation makes it suitable. Do not strip characters,
change domain/application validation or modify Mongo to accommodate SQL types.
Issue/finalizer work resumes with this representation. The decision adds no shared
validation change, Mongo change or public backend configuration.

The user also approved keeping Mongo unchanged and searching the full normalized
title projection in PostgreSQL instead of copying Mongo text-index truncation at
U+0000. A disposable Mongo 8.0.12 fixture confirmed that `panic\u0000cafe` is found
by `panic` but not `cafe`. The original title remains lossless in either adapter.
The ordinary application path already replaces controls with spaces when building
Issue titles and culprits; the difference concerns direct storage inputs. Keep
this explicit exception separate from ordinary term/phrase search conformance.

## Issue capability family

Schema generation 5 adds scoped Issues, critical notification transitions and
best-effort deterministic activities. `PostgresIssueStore` implements all four
Issue methods. The adapter-owned canonical JSON/zstd codec is shared privately
with Event storage; the domain, ports, application and Mongo implementation are
unchanged. Exact titles, culprits, release pairs and grouping metadata preserve
NUL-bearing UTF8 through bounded bytes. Native search projections remain separate.

Project locks precede Issue locks. Creation and full-key collision checks, stable
creation summaries, timestamp/Event-ID pairs, count overflow and receipt-time
regression rules are transactional. Critical transitions commit with Issue state;
activity writes follow that commit and cannot roll back state if they fail. The
private occurrence batch helper covers the existing Finalizer maximum of 10,000
events, even when all events share one Issue. Standalone occurrence retries retain
the accepted approximate count drift; finalized Event retries will use the Event
fence in the upcoming finalization transaction.

Seven real PostgreSQL Issue cases passed: concurrent creation/update/regression;
workflow, assignment and retry semantics; exact NUL/Unicode round trips and scoped
search; injected critical rollback versus best-effort activity failure; identity,
UTF8, canonical codec/decompression and overflow corruption; multi-occurrence
pairs; and selective GIN search on 10,000 unrelated synthetic rows. An initial
secondary unique full-key constraint caused a concurrent creation conflict before
the primary-key retry path; it was removed from this unpublished migration.
The project/Issue primary key plus locked complete-key verification retains the
identity invariant. The query-plan fixture also needed VACUUM/ANALYZE to flush
the GIN pending list before collecting representative planner evidence.

The shared `testkit/fixtures/issue-title-search.json` corpus contains 16 ordinary
term/phrase, phrase-plus-term, multiple-phrase, negation, Unicode, case/diacritic,
stop-word and no-stemming scenarios. It passed on both Docker PostgreSQL 18.6 and
Mongo 8.0.12. The disposable Mongo probe is emitted by
`python scripts/issue-title-search-mongo-probe.py` and piped into mongosh on the
task-owned container; it deletes only its generated synthetic fixture database.
This tests result membership, not identical database-native relevance scores or
every Unicode tokenizer edge. The approved NUL-search exception is tested apart.

The complete PostgreSQL suite passed 37/37 with no ignored cases. Remaining
capabilities and full runtime/SDK/browser/security/fault/soak gates are still pending;
PostgreSQL is not yet selectable in the public runtime configuration.

## Transactional finalization capability

Schema generation 6 adds finalized Event projections, hourly statistics, exact
binary Release/Environment catalogs, scoped Release project links and bounded
project catalog usage. `PostgresFinalizationStore` implements the existing port
with the shared Issue mutation helper and binary JSON codec. No shared production
contract, application or Mongo implementation changes are needed.

One transaction locks projects, separate project catalog-usage rows, Event keys,
Issue keys, hour tuples, organization/Release IDs and Environment IDs in stable
order. Separate usage rows avoid upgrading shared project locks. Finalized Event
state commits together with Issues, authoritative hourly increments, catalog
admission and notification transitions. Activities remain best effort after that
commit. Missing/fenced/failed/completed Events are skipped; a fully skipped batch
rolls back incidental usage-row creation. Disabled projects remain processable.

The unchanged port has no `Ambiguous` variant, so a lost COMMIT reply returns
`Unavailable`; a subsequent attempt observes the committed Event state and skips
its effects. Retention either publishes hot expiry or an archive hold, preserves
integer timestamps and signed 64-bit search tokens, and projects valid trace/span
pairs without exposing their payloads in logs. Release/project composite foreign
keys reject cross-organization associations. Catalog limits skip materialization
without losing the Event; existing identities remain usable and only creation
reserves capacity. Stored metadata retains exact NUL-bearing UTF8 bytes.

Ten new Docker cases passed: complete row/counter/catalog/trace/token/retention
behavior; injected failure at the final Event update with rollback of every
derived effect; a real lost COMMIT reply; concurrent identical and opposing
project batches; scoped catalog admission and day rollover; shared organization
Release creation and deterministic full Event-key pairs; the supported maximum
10,000-event single-Issue batch and oversized-batch rejection; archive holds and
project fences; input/corruption/overflow rejection; and post-commit best-effort
regression activity failure. The first shared-organization test accidentally reused
an already occupied project slug; its synthetic fixture was corrected before the
ten-case run passed. No production slug behavior changed.

The existing sparse-backlog EXPLAIN fixture now genuinely finalizes its 9,900
completed rows through the adapter, instead of manually assigning a completed
status without required metadata. Its partial-index query evidence still passes.
The whole PostgreSQL suite passed 47/47, with no ignored cases, on the generation-6
schema. Formatting, dependency boundaries and documentation checks passed.
Three abandoned generated databases from earlier failed assertions were removed
after verifying the task-owned Docker project and their exact fixture identifiers;
normal successful fixtures clean themselves up.

Current source coverage is 53/164. Remaining capabilities and full deployment,
restricted-credential, SDK/browser, security, recovery and soak gates are pending.

## Release and Deploy control plane

Schema generation 7 adds organization-scoped Deploys and ordered project links.
It replaces the existing Release/Issue indexes through a new immutable migration
so their order matches the public first-seen and regression cursors. All eight
ReleaseStore methods are implemented without changing shared contracts or Mongo.
Explicit Release creation supplements an implicit catalog entry while retaining
its creation time, occurrence pairs and existing metadata. Metadata and names use
bounded lossless bytes; every SQL value is bound. Composite foreign keys and
project locks prevent cross-organization links and fence deleted projects.

Deploy retries compare the complete command, including original project order.
Concurrent identical inserts converge on one record; conflicting commands return
Conflict. Finishing a Deploy is conditional and idempotent. Lists use bounded
keyset pages and stable timestamp/identity ties. Lost COMMIT replies remain
Unavailable under the existing port, with deterministic retries.

Six new Docker tests passed, covering resolution and scope, implicit-to-explicit
Release metadata, concurrent Deploy creation/finish and ordered associations,
stable pagination and selective indexes over 10,000 unrelated rows, exact binary
Release/Issue queries, injected transactional rollback, lost COMMIT replies and
bounded corruption rejection. The complete generation-7 suite passed 53/53 with
no ignored tests. Strict workspace Clippy passed with warnings denied.

Current source coverage is 61/164. The remaining capabilities and full runtime,
restricted-credential, SDK/browser, recovery, security and soak gates remain
pending; PostgreSQL is not yet selectable through public configuration.

## Native investigation queries

All eight InvestigationStore methods now use scoped native queries, bounded
keyset pages and the existing adapter-owned binary JSON codec. Issue point reads,
locked mutations and investigation pages share one server-bounded projection and
decoder. SQL builders contain static fragments only; cursor, ID, time and token
values are bound. Candidate searches preserve the existing bounded branch union
and examined-count behavior without duplicating rows or exposing pending/failed
Events. Custom platforms continue to come from the processed payload projection.
Schema generation 8 adds the partial visible-Environment timeline index through
a separate immutable migration.

Six new Docker cases passed: project isolation, stable timestamp ties and status
filters; all search anchors, signed token extremes and foreign-project cursors;
ordered activities and hourly statistics; binary Release/Environment catalogs,
hidden entries and catalog identity checks; encoded/decompressed bounds and
malformed bodies; and real EXPLAIN evidence for project/Issue Event timelines,
selective token GIN and visible-Environment indexes over 10,000 unrelated rows.
The initial EXPLAIN assertion compared a JSON floating-point row count with an
integer representation; the fixture now compares their numeric value. No query
or production behavior changed for that correction.

Current source coverage is 69/164. Runtime selection and the remaining full
backend acceptance gates are pending.
The full generation-8 Docker suite passed 59/59 with no ignored cases. The failed
EXPLAIN fixture's exact generated database was removed after the suite completed
and the task-owned Compose project was verified.

## Saved Query and Dashboard configuration

Schema generation 9 adds one adapter-private configuration table with separate
Saved Query/Dashboard kinds, project-scoped identities and name reservations,
revision CAS and a bounded binary body. All ten DashboardStore methods share
native CRUD helpers and preserve the current application contracts. A Dashboard
can retain a missing Saved Query reference; the existing refresh service reports
that widget's failure. List reads skip malformed entries under the same bounded
database limit, while direct loads return InvalidData, matching current behavior.

The versioned private query codec preserves typed integer versus floating-point
values, exact NUL-bearing query text, legacy predicate arrays and expressions.
Flat expression nodes support the domain's maximum 256 nodes without recursive
JSON nesting; decoders reject cycles, shared children, orphans and unknown kinds.
The existing canonical JSON/zstd codec provides encoded and decompressed bounds.
Project locks fence writes; all SQL data is bound and driver logging is disabled.

Six Docker cases and one static malformed-expression case passed: scoped CRUD,
unique names and dangling widget references; concurrent CAS and name admission;
all datasets and typed queries, 256-node expressions and maximum control-bearing
predicates; validation and project fences; ordered bounded lists, corruption,
decompression limits and a selective configuration index over 10,000 unrelated
rows; and injected update rollback versus real lost COMMIT replies. Under the
existing port, the latter returns Unavailable and a direct load exposes the
durable revision; retrying the old revision returns Conflict. The full
generation-9 Docker suite passed 66/66, with no ignored cases. Strict workspace
Clippy passed with warnings denied.

Current source coverage is 79/164. PostgreSQL runtime selection and the remaining
backend acceptance gates are still pending.

## Session rebuild boundary decision

Implementation was stopped before the Session capability family for an explicit
user decision, as requested for newly discovered behavioral disagreements.
Mongo's `sessions.rs::rebuild_inner` selects Session starts in the raw `[from,until)`
range, deletes hourly rows whose bucket starts lie in that same raw range, then
inserts rebuilt buckets without a transaction. If a boundary falls inside an
hour, a rebuilt bucket may lie outside the deleted range. This can conflict with
an existing bucket and partially change another bucket to an incomplete count.
The existing Session test exercises an input whose bucket start is still within
the raw range, so it does not cover this boundary case.

`python scripts/session-health-range-mongo-probe.py`, piped into mongosh in the
task-owned Docker Mongo 8.0.12 fixture, reproduced the current source-selection,
delete and insert algorithm on synthetic data. A half-hour through one-and-a-half
hour range produced duplicate-key failure and changed whole-hour counts `[2,2]`
to `[2,1]`. This is an algorithm reproduction, not an invocation of the Rust port.
Its generated fixture database was dropped in finally; Mongo code and real data
were not modified.

Proposed PostgreSQL behavior was to expand a rebuild range to all intersected whole
hours, derive counters and sketches from all Sessions in those hours, and replace
the corresponding statistics transactionally while excluding concurrent Session
mutations for that project. Keep Mongo unchanged. This avoids reproducing a data
consistency defect but differs from the existing adapter for unaligned bounds;
On 2026-10-09 the user authorized correcting the range semantics in both adapters,
with a minimal Mongo change. Mongo's existing method now rounds the lower bound
down and the exclusive upper bound up, retaining an exactly aligned upper edge.
Both source selection and deletion use these same bounds, including negative
timestamps and the last exclusive hour boundary of the domain. No Mongo schema,
migration, deployment or write-mechanics changes were made. A shared Testkit
fixture exercises half-hour boundaries, an exact upper hour, a raw interval
containing no Sessions, whole-hour bounds, pre-epoch dates and repeated repair.
The real Mongo Rust Session suite passed both its existing case and the new
shared five-range fixture in Docker, with no ignored cases.

## Transactional PostgreSQL Sessions

Schema generation 10 adds compact scoped Sessions, fixed 1024-bit health sketches,
hourly counters and project-local Session coordination. All five SessionStore
methods use existing domain merge semantics. Session updates and authoritative
counter changes commit together; retries after an actual lost COMMIT reply observe
the merged record and avoid applying counters twice. Unsigned sequence numbers
retain their full domain range through eight-byte encoding. TTL and archive holds
remain distinct; project fences block mutations while disabled projects remain
processable. An optional Release/Environment catalog record is not required for
Session admission, and health reads retain the existing environment-ID fallback.

Project shared locks precede separate Session coordination locks and Session
identities. Rebuild holds that same project-local coordination lock, replaces all
intersected hours in one transaction, and aggregates counters and user sketches
natively from Sessions without transferring an unbounded source list into Rust.
The supported domain sketch bit index is preserved by fixed native bit aggregation.
Stale terminalization is bounded to 10,000 candidates and rechecks their state and
last update under locks; a refreshed Session cannot be terminalized from an old
worker snapshot. Concurrent Event/Issue traffic uses its existing separate locks.

Eight new real Docker cases plus one static timestamp-boundary case passed: the
shared Mongo/Postgres five-range regression; lifecycle ordering, duplicates,
scope, full unsigned sequence and retention; concurrent retries, merges and
repairs; injected counter/update and rebuild rollback with subsequent recovery;
real lost COMMIT reply; stale-state rechecks and project fences; invalid identity,
input limits and counter overflow; and selective Session range/stale indexes
over 10,000 unrelated rows. Rebuild also removes orphaned statistics when no
source remains and repairs injected count/sketch drift. Current source coverage
is 84/164; full runtime selection and the remaining acceptance gates are pending.

The complete generation-10 PostgreSQL Docker suite passed 75/75 with no ignored
cases, including native TLS checks. Strict workspace Clippy passed for all targets
and features with warnings denied; formatting, dependency boundaries and repository
documentation validation also passed.

Docker Desktop had stopped after a crash before these checks. Only the three
existing task-owned PostgreSQL/TLS/Mongo fixture containers were started; their
images remained available. Native/authenticated fixture connections succeeded.
No image prune, volume removal or Docker credential changes were performed.

## Mongo Replay regression fixture correction

After the Session correction, the complete real Mongo integration/migration run
executed 21 cases: 20 passed, including both Session tests; the existing Replay
case failed at `crates/mongo/tests/replays.rs:56`, observing segment IDs `[0]`
instead of `[0,2]`. An isolated unchanged rerun passed. This is not a passing
complete Mongo gate and is not attributed to the Session correction.

The Replay fixture hardcodes November 2023 timestamps and uses a native TTL index
with a 30-day retention period. Its records are already expired at the current
Docker clock. The failed fixture subsequently contained zero Replays, and the
native TTL monitor reported deleted documents. These observations support a TTL
race as the cause; the original failing operation was not instrumented, so they
do not prove which exact monitor deletion coincided with the assertion.

The user authorized a minimal test-only correction on 2026-10-09. The fixture
now captures one current timestamp per exercise and derives every segment from
it, including the duplicate submission and archive case. The production Replay
adapter, TTL index, retention behavior and assertions remain unchanged. The
isolated real Mongo Replay test passed with the corrected fixture. PostgreSQL
runtime selection and the remaining 80 capability methods and acceptance gates
are still unfinished.

The complete selected real Mongo integration/migration suite then passed 21/21
with no failed or ignored selected cases, using the unchanged Docker fixture and
native TTL behavior. Retained performance baselines were excluded as before.
Formatting and documentation validation passed. Clippy for the modified Replay
integration target passed with all features and warnings denied.

## PostgreSQL Replay manifests

Schema generation 11 implements all four ReplayStore methods. Existing ReplayWriter
and BlobStore ownership stay unchanged: recording bytes are never stored in SQL.
Scoped manifests and compact segment references commit together. A shared project
lock fences ingestion; the manifest row serializes concurrent first and subsequent
segments while unrelated Replay identities remain independent. The adapter verifies
the immutable recording key, size and checksum on duplicate admission, matching the
existing ReplayWriter contract for conflicting bytes. Accepted metadata retains
the existing optional-value and correlation replacement semantics; retention stays
anchored to the first accepted segment. TTL and archive holds are distinct.

One read-only repeatable-read transaction covers each bounded page and its segment
sets. Segment identities and correlation arrays retain their existing limits and
ordering, including repeated correlations. Native project/time keyset and array
indexes serve list/filter queries; exact Blob reference checks use a unique key
index. Manifest deletion cascades to references without touching recording bytes.
All values are bound, stored metadata is validated, and driver errors are redacted.

Seven real Docker cases passed: segment ordering, duplicates/conflicting bytes,
metadata and retention; scoped filters/keyset ties; concurrent first admissions and
consistent snapshots while filling all 100 segments; injected rollback followed
by recovery and reference removal; actual lost COMMIT reply with safe retry;
invalid inputs, project fences and corrupt decoding; and native indexed timelines,
correlation searches and references with 10,000 unrelated manifests/segments.
The complete generation-11 PostgreSQL suite passed 82/82 with no ignored cases,
including TLS/migration checks. Source coverage is now 88/164. PostgreSQL runtime
selection and the remaining capabilities and acceptance gates are still pending.
No Mongo source or tests changed in this milestone.

## PostgreSQL Application Metrics

Schema generation 12 adds compact native Metric aggregates and all MetricStore
admission behavior, reusing the existing MetricWriter. Sorted project locks and
separate project-local Metric coordination make cardinality admission and the
entire batch atomic across processes. A capped native DISTINCT/UNION counts
existing/requested series; bounded indexed lateral lookups validate complete
series metadata rather than trusting hash identity. Native upserts add counters,
sum/count/min/max and fixed 64-bin sketches. Gauge last/received values and initial
trace/retention metadata retain the existing semantics. TTL and archive holds
remain separate. Numeric overflow rejects and rolls back the batch.

Adapter bounds are 10,000 deltas, 16 MiB prepared batch bytes, 512 KiB metadata
per delta and 1,000-row SQL chunks. Free metadata remains lossless binary UTF-8
and canonical JSON. Name equality uses a fixed SHA-256 projection plus full value
verification, avoiding oversized native index keys. A shared private project
admission helper removes duplicate guards in Session, Replay and Metric stores;
it introduces no public/domain contract changes.

The Phase 37 at-least-once Metric contract is preserved: neither the store nor
MetricWriter retries an unknown COMMIT internally. An explicit client retry can
apply the same delta again. Seven real Docker cases passed: all aggregate kinds,
lossless metadata/large names and retention; concurrent scoped cardinality and
batch rollback; concurrent additive writes with the existing MetricWriter;
failure after the first 1,000-row chunk and integer/float overflow; actual lost
COMMIT through MetricWriter followed by an explicit additive client retry;
invalid inputs, project fences and complete-identity collision guards; and the
maximum 10,000-delta batch with native scoped time/series/name index evidence.
PostgreSQL Clippy for all targets/features passed with warnings denied; formatting,
source inventory and documentation validation passed. Source coverage is 89/164;
runtime selection and the remaining backend gates are still pending. Mongo was
not changed in this milestone.

The complete generation-12 PostgreSQL Docker suite passed 89/89 with no ignored
cases, including the Session/Replay regressions after the private guard extraction
and native TLS/migration checks.

## PostgreSQL Feedback

Schema generation 13 adds all four FeedbackSink/FeedbackStore methods. The scoped
parent, ordered at-most-ten attachments and existing Blob reference registry are
written in one transaction. Submission retries compare immutable identity and
cannot reset workflow, receive time, attachment creation time or retention.
Project guards reject missing/fenced targets while retaining disabled-project
behavior. Status changes are serialized on the scoped parent; stale different
statuses conflict, while a repeated current status preserves its original time.
The existing NativeApi continues deriving the current Issue through the associated
scoped Event. No public contract or application service was changed.

Bounded list/load queries read parent rows and attachments in one repeatable-read
snapshot. Every filter and keyset value is bound. Native project/time, status and
correlation indexes serve these queries. Binary UTF-8 storage preserves accepted
attachment metadata, including NUL; corrupt stored filenames, content types and
foreign Blob relationships fail closed. Metadata limits and integer ranges are
checked before writes. Parent deletion cascades attachments and removes only its
Feedback references in the same transaction; Error/other-project references and
physical Blob bytes remain under their existing lifecycle owners.

Six real Docker cases passed: ordered attachments, immutable retry/conflict and
concurrent admission; scoped workflow, conjunctive filters and keyset pagination;
injected reference/status failures and scoped deletion; actual lost COMMIT replies
for submission and workflow followed by safe retries; invalid inputs, project
fences and corrupt stored attachments; and native indexed query plans with 10,000
unrelated synthetic Feedback records. PostgreSQL Clippy for all targets/features
passed with warnings denied. Source coverage is 93/164; 71 signatures, runtime
selection and the remaining backend acceptance gates are still pending. Mongo
source/tests were not changed in this milestone.

The complete generation-13 PostgreSQL Docker suite passed 95/95 with no ignored
cases, including native TLS, migrations, fault injection and all earlier adapter
regressions. Formatting, dependency graph and documentation validation passed.

## Span statistic rebuild boundary decision

Before implementing the SignalStore family, source review found the same raw-hour
boundary mismatch in `crates/mongo/src/signals.rs::rebuild_inner`. This is a
separate method from the previously authorized Session repair. It deletes buckets
whose hour starts are in the raw `[from,until)` interval, selects only Spans in
that raw interval, then calls incremental `apply_stat`. A partially intersected
first hour survives and receives duplicate increments; the last hour is recreated
from incomplete source data. Repeating repair increases the surviving first count.

`python scripts/span-stats-range-mongo-probe.py`, piped into the task-owned Mongo
8.0.12 Docker fixture with `mongosh --file /dev/stdin`, reproduced this exact
selection/deletion/increment algorithm on four synthetic Spans dated 2026-10-09.
For 00:30–01:30, correct whole-hour counts `[2,2]` became `[3,1]`, then `[4,1]`
after a second repair. This is an algorithm reproduction, not an invocation of
the Rust port. Its generated database was dropped in finally. No real data,
Mongo code, schema or running deployment was changed.

SignalStore implementation is stopped for the requested user decision: authorize
a minimal whole-hour range correction in Mongo's separate Span repair method and
matching complete-hour transactional PostgreSQL repair, or correct PostgreSQL only
and retain this known Mongo boundary behavior. No broader Mongo write-mechanics
change is proposed. Feedback completion and verification remain independently
reviewable on `feat/postgres`.

On 2026-10-09 the user authorized this separate confirmed Span bug fix. Mongo's
`signals.rs::rebuild_inner` now rounds both bounds to the same intersected full
hours and keeps an exactly aligned exclusive upper edge. Nanosecond conversion
is checked before deleting statistics. Source selection, delete/upsert mechanics,
schemas, migrations and ingest behavior otherwise remain unchanged.

A shared Testkit fixture runs four ranges twice through the real Rust SignalStore:
half-hour edges, exact upper edge, an empty raw source window and a whole-hour
interval. It checks complete counts, failures, averages, percentiles, representative
trace and dimensions after each repair; invalid/overflowing ranges leave existing
statistics unchanged. Synthetic received/start dates are generated inside current
retention, avoiding the earlier fixed-date TTL fixture race. The new real Mongo
Docker case passed; all 22 selected real Mongo cases then passed with no ignored
cases. Mongo/Testkit strict Clippy passed. This is the sole new Mongo production
change and PostgreSQL Signal implementation resumes with the approved semantics.

## PostgreSQL Signals

Schema generation 14 implements all eight SignalStore methods, retaining the
existing LogWriter/SpanWriter services and public contracts. Raw Log/Span batches
are admitted transactionally under ordered project guards and separate scoped
Signal coordination. Native 1,000-row chunks preserve original admission order;
bounded primary-key joins verify the existing duplicate identity rules. Retries
retain the first payload and metadata. Missing/fenced projects reject writes,
disabled-project storage behavior is retained, and ambiguous COMMIT replies are
reported without internal retry.

Raw JSON payloads use an adapter-owned versioned binary codec with optional Zstd
and bounded 1 MiB decompression; original JSON byte formatting is preserved.
Native binary UTF-8 projections retain accepted NUL metadata. Literal message
substring search uses lowercase binary values with a native GIN trigram candidate
index followed by exact binary substring verification. Values and filters are
bound; schema identifiers and SQL fragments are application-owned constants.
Malformed versions/UTF-8, inconsistent body sizes, dimension projections and
foreign/scoped identities fail closed without exposing payloads in driver errors.

Batch bounds are 10,000 records and 16 MiB decoded record bytes. Keyset pages retain
the existing 200-record bound and additionally cap decoded records to 8 MiB, with
a continuation cursor when the byte budget fills. Native window budgets run
before payload transfer/decompression. Trace reads reuse the same budget per
signal, select bounded per-project branches, and share one repeatable-read
snapshot with at-most-100 finalized Error correlations. Truncated traces expose
the existing partial/omitted flags; scoped keys and correlations remain binary.

Hourly segment statistics group by a canonical typed dimension tuple, using a
fixed SHA-256 index projection with full-value verification. Native aggregates
retain count, failures, duration sum and the existing last-2,048 sample percentile
contract. Statistics and source processing markers commit together; online
rollups select at most 10,000 pending sources. A rollup savepoint preserves raw
Span admission if derived-state updates fail, leaving markers available for
retry/new admission or repair. The later Maintenance implementation must drain
pending rollups before expiring their source Spans. Full-hour repair uses the
approved bounds, rebuilds natively and commits statistics plus source markers
under the same project coordination as concurrent admission. Failure rolls back
the entire repair; repeated repair preserves all complete-hour data.

Eight real Docker cases passed: scoped identities, original payloads and NUL
metadata, literal search/filters/keysets; shared full-hour repair and injected
repair/derived-state failure recovery; concurrent duplicate/new admission with
repair; actual lost COMMIT replies and second-chunk rollback; large-payload byte
budgets and separate archive retention; invalid values, fences and corrupted
decoding; maximum batches, 2,048-sample cap and native scoped time/trace/GIN plans;
and existing writers with real Event finalization and scoped Error correlation.
The GIN plan fixture uses ordinary VACUUM/ANALYZE after its 10,000-row bulk load,
retaining default fastupdate/autovacuum, planner and durability settings. This
proves the steady-state native index shape, not the still-pending burst/soak gate.

PostgreSQL strict Clippy, formatting, dependency graph, compatibility/deployment
profile validators and source inventory passed. Source coverage is 101/164; 63
signatures, runtime selection and the remaining integration/load acceptance gates
remain pending. No normal dependency, configuration, installer or service
composition change was introduced; Mongo remains the runtime/default backend.
No Mongo production change beyond the separately committed authorized Span-range
fix was made.

The complete generation-14 PostgreSQL Docker suite passed 103/103 with no ignored
cases, including native TLS/migrations, fault injection, previous capabilities
and all eight Signal cases. The two disposable databases left by the initial
test-fixture failures were inspected and removed by their exact generated names
only after that test process ended. Successful test fixtures cleaned themselves;
no installation data, volumes, images or credentials were removed.

## PostgreSQL Explore

Schema generation 15 adds `PostgresExploreStore` through the existing ExploreStore
port. Errors, Logs, Spans and Metrics use one closed typed SQL compiler; adapter
constants select tables, columns and operators, and bounded ordinal output slots
replace SQL aliases. Caller aliases are only output-map keys. Every filter value,
scope, interval, cursor and limit is bound. No application, domain, port or Mongo
production change is needed. Raw reads use native projections, not payload decoding
or a Rust scan of stored sources; aggregates and percentiles execute in PostgreSQL.

Queries retain finalized-only Errors, project scope outside boolean expressions,
legacy AND predicates, expression precedence, nested AND/OR/NOT, missing-field
semantics, case-sensitive literal substring/prefix/suffix matching and lossless NUL
metadata. Positive predicates remain native index conditions; only NOT converts
SQL unknown to the contract's false missing-field value. An initial EXPLAIN test
identified that wrapping every predicate in coalesce prevented the exact Metric
name index from being used. The compiler was corrected, without planner forcing,
and the real query then used its scoped hash-plus-full-name index.

Native bigint time/identity keysets retain the public 20-byte Error key and 16-byte
signal/Metric keys. Errors order by the native indexed event identity internally;
the external project prefix is handled without widening the trusted query scope.
Generation 15 adds only the descending Metric time/identity index; existing Error,
Log and Span timeline indexes are reused. Raw result rows preserve integer versus
floating types, coded labels, binary identities and nullable projections. Metric
count predicates use numeric comparison to avoid rounding bigint source values
above 2^53. Native integer sums retain integers when possible and promote overflow
to a finite floating result instead of truncating or rejecting valid counters.

UTC interval grouping first converts source nanoseconds to millisecond dates with
truncation toward zero, then floors the interval. A read-only Mongo `$documents`
probe verified negative sub-millisecond behavior without writing any data. A second
read-only probe verified suffix matching before a final LF; PostgreSQL preserves
that existing anchor behavior using binary equality. PostgreSQL percentiles use
native percentile_cont interpolation; Mongo retains approximate percentiles. The
common port specifies percentile selection, not identical approximation samples.
Tests check finite values and the PostgreSQL native interpolation result.

One read-only transaction owns each query and its local statement_timeout; an
outer cancellable deadline also bounds pool acquisition and completion. Neither
timeouts nor cancellation leave transaction settings in the pool. Defensive
validation bounds expression nodes, OR alternatives, predicates, output rows,
groups, aggregates, identifiers and typed values before recursive SQL compilation.
Fixed aliases and no free-form expressions prevent SQL injection. Errors remain
stable/redacted; query values and stored payloads are not logged.

At most 501 raw projections or 500 aggregate rows are selected. A native cumulative
8 MiB projection-byte budget suppresses oversized field transfer before decoding.
Raw pages return a continuation cursor at the last delivered record; an oversized
aggregate result fails explicitly rather than silently returning partial totals.
No unbounded body transfer, decompression or client-side grouping is introduced.

The targeted suite passed eight cases (one compiler unit case and seven real Docker
cases): all four datasets and stable scoped keysets; literal/boolean/null/number
filters; native aggregates, all five percentiles, UTC intervals and empty sources;
large-result budgets; native blocked-query cancellation/pool recovery; exact bigint
comparison and sum promotion; and real compiled Metric timeline/name index plans.
PostgreSQL strict Clippy, formatting, dependency graph, compatibility/deployment
profile validators and source inventory passed. Coverage is 102/164 signatures;
62 signatures plus complete runtime selection and remaining acceptance gates are
still pending. Mongo remains the default and current runtime backend.

The complete generation-15 PostgreSQL suite passed 111/111 with no ignored cases
in 103.84 seconds, including TLS, migrations, previous capabilities and all eight
Explore cases. The initial invalid synthetic Event fixture was corrected before
the successful run. Its database and the failed initial index-plan fixture were
inspected and dropped by their exact generated names after their test processes
ended and no connections remained. Successful fixtures cleaned themselves. No
installation database, persistent volume, image or credential was removed.

## Monitor lifecycle boundary: awaiting a separate Mongo exception

Before implementing MonitorStore, an isolated real-Rust-port Mongo probe confirmed
a temporal merge defect. Two individually valid SDK updates for the same run can
persist in this order: in_progress with started_at=T, then success with its own
started_at=T-2000 ms and finished_at/received_at=T-1000 ms. The second message may
have been received before the first, while queue/concurrent request processing
persists it later. Both MonitorUpdate::validate calls succeed. Mongo acknowledges
both writes as Accepted, preserves the first stored start, and replaces the finish
with T-1000 ms. The resulting persisted finish precedes the persisted start.
list_monitor_runs subsequently fails InvalidData when decode_run validates it;
one such record can prevent the entire history page from being read.

The real probe passed its expected-defect assertions: both valid input messages,
two accepted acknowledgements, the incompatible stored time pair, and the failed
typed history read. It generated T from current server/test time to remain inside
retention and used its own metric_pg_monitor_order_<pid>_<nonce> database. That
database was dropped before the assertions completed. The temporary Rust test
target was removed after the test process ended; no Mongo production source,
schema, existing fixture or installation data was changed.

Suggested minimal correction: retain the original stored started_at and reject an
in_progress-to-terminal transition whose finish precedes it, before updating the
run. Mongo needs one guard inside its existing terminal-transition branch:

```rust
if incoming.finished_at.is_some_and(|finish| finish < existing.started_at) {
    return Err(SignalStoreError::InvalidData);
}
```

The PostgreSQL adapter should validate the merged record and enforce the same
ordering with a native CHECK. This needs no port/domain change or implicit
timestamp rewriting. A shared generated-date regression fixture should verify
that the rejected transition leaves a readable original run, a subsequent valid
terminal transition succeeds, and duplicate terminal messages stay immutable.

Monitor implementation is stopped for user authorization of this new, separate
minimal Mongo exception. Earlier authorization concerned Span-hour repair and
does not automatically cover another production change. Explore remains committed
and verified at generation 15; PostgreSQL coverage is still 102/164 signatures.

## Authorized monitor temporal merge correction

The user authorized the preceding Mongo exception on 2026-10-09. Mongo now rejects
a terminal finish earlier than the original stored start in its existing
in_progress-to-terminal branch. The production change is one six-line guard;
schema, migration, timeout scheduler and stored timestamp semantics are unchanged.

The shared generated-date regression checks rejected-update immutability, readable
history, a subsequent valid completion, duplicate terminal immutability, rejection
of the invalid transition inside one batch, and equality at the stored-start edge.
It passed on the real Docker Mongo adapter. The selected complete Mongo regression
run passed 23 cases (ignored real-adapter cases, excluding performance_/reports_rps)
with one test thread. These are selected infrastructure checks, not a claim that
the entire workspace or performance suite was run again.

## Transactional PostgreSQL Cron and Uptime monitors

Schema generation 16 adds typed monitor definitions, immutable run identities,
native project/history/deadline/alert/expiry indexes and per-project monitor lane
locks. PostgresMonitorStore explicitly implements all eleven MonitorStore methods.
Project guards are acquired in sorted order before lane locks; background workers
recheck project fences, due times, status and lease ownership after acquiring the
same locks. Admission, definition/projection changes and run merges commit in one
transaction, using bulk writes of at most 1000 rows rather than one write per run.
Missed-slot existence is also loaded once per worker batch.

In-progress admission fills the configured runtime deadline. Terminal transitions
keep the first stored start/source/identity metadata and validate the actual merged
run before writing it. A native CHECK additionally rejects finishes before the
stored start. Terminal duplicates do not replace the first completion. Web-owned
definitions win over SDK configuration; SDK-owned definitions retain their creation
time and increment revision. Stable scoped keysets retain equal-timestamp rows and
the existing inclusive run until bound. Monitor deletion cascades to its scoped
history. Uptime claims are transactional leases; completion releases the lease and
advances the next expected run. Existing MonitorWriter and UptimeScheduler use the
unchanged ports without another queue/scheduler implementation.

All values are bound. Closed fixed SQL clauses provide pagination, filters and
worker selection; neither names nor endpoints become SQL text. Configuration uses
the existing canonical/versioned/compressed adapter JSON codec, limited to 64 KiB,
with strict enum/shape/domain validation. Sealed Uptime header ciphertext remains
opaque; no plaintext secret is produced or logged. Run fields are native bounded
columns. Admission caps at 10000 updates and 16 MiB of decoded input/configuration;
loaded configuration has a separate 16 MiB budget. Definition pages apply a native
cumulative 8 MiB decoded-byte budget before configuration transfer and continue
from the last delivered record. Fixed-width history pages cap their row count to
the same response budget. Scheduler decoding is bounded and leaves excess eligible
work for later calls. Retention timestamps/indexes are present; the shared
MaintenanceStore cleanup implementation is still pending and is not claimed here.

Nine targeted cases passed (eight real Docker cases and one configuration codec
unit case), including the shared Mongo/Postgres temporal fixture and native CHECK;
scoped CRUD/keysets/filters; lifecycle immutability and atomic missing-definition
rejection; SDK/web ownership; opaque headers, malformed configuration and limits;
timeouts/missed runs/alert markers/project fences/retention timestamps;
concurrent duplicate/deadline/lease ownership; an actual lost COMMIT response and
second-chunk SQL failure with full rollback; and the existing writer/scheduler.
The large-boundary case admitted 10000 runs, paged 260 large configurations without
omissions/duplicates, and verified native history, timeout, Cron, Uptime and alert
index plans with additional representative data. No optimizer index forcing was
used.

The complete generation-16 PostgreSQL suite passed 120/120, without ignored cases,
in 149.93 seconds, including TLS, migrations and all earlier capabilities. The
initial Uptime test fixture used an uppercase header name rejected by the domain;
only that fixture was corrected before the successful targeted and full runs.
Its three failed own fixture databases were inspected, verified connection-free
and dropped by exact generated name after the failed process ended. Successful
fixtures cleaned themselves; both PostgreSQL containers have no remaining own
test databases. No installation data, volume, image or credential was removed.

Strict Clippy passed for Postgres, Mongo and Testkit with all targets/features and
warnings denied. The first lint run requested a boolean simplification in the new
Postgres timeout recheck; the equivalent is_none_or expression was applied without
disabling the lint. Coverage is now 113/164 signatures, with 51 pending. Mongo
remains the installation default and only composed runtime backend until the
remaining capabilities and end-to-end acceptance gates are complete.
Formatting, ADR-0034 dependency graph, documentation, deployment profiles,
compatibility matrix and the regenerated source inventory also passed. After the
lint-only boolean simplification, the real deadline/alert/fence/retention case was
rerun successfully (1/1); broad testing was not repeated for that equivalent edit.

## Notification delivery history: awaiting a separate Mongo exception

While inspecting the next capability family, a real public-port Docker Mongo
probe confirmed an existing terminal-delivery read defect. Enqueue a valid pending
delivery, read its history successfully, then call mark_delivered or mark_dead.
Both terminal methods acknowledge success and persist the corresponding final
status and terminal timestamp (field l). Their shared terminal_update removes the
next-attempt field n. The validator permits that absence, but decode_delivery still
requires timestamp(document, "n") for every status. Subsequent list_delivery_history
returns NotificationStoreError::InvalidData. One such row fails its entire history
page; delivery finalization itself succeeded and no stored payload was lost.
The existing authorized HTTP service uses this same history port.

The isolated Rust probe tested both delivered and dead in separate synthetic
project scopes: readable pending history, successful terminal acknowledgement,
correct persisted final status, missing native n and failed typed history read.
It passed all expected-defect assertions (1/1 real case, 1.96 seconds). The test
generated current times and created only its own
metric_pg_notification_terminal_<pid>_<nonce> database. It dropped that database
before asserting the captured outcomes. After the process ended, the temporary
test target was removed and the Docker catalog confirmed no such databases remain.
Mongo notification production source, schemas, migrations and installation data
were not changed by this probe.

Suggested minimal repair: when decoding a delivered/dead row with no n, use its
persisted terminal timestamp l to satisfy the existing non-optional domain field.
Keep pending rows strict and reject malformed present fields. This also repairs
reading already-written final rows, without a data migration or schema change:

```rust
let next_attempt_at = if document.contains_key("n")
    || status == NotificationDeliveryStatus::Pending
{
    timestamp(document, "n")?
} else {
    timestamp(document, "l")?
};
```

PostgreSQL NotificationStore should implement readable terminal history with the
same domain representation and share a real-adapter regression for both terminal
outcomes, retry/duplicate immutability and scoped pages. The notification adapter
has not yet been implemented; no defect-compatible behavior or port/domain change
was introduced. Implementation is stopped for the user's separate decision on this
new minimal Mongo exception, as explicitly requested for newly found discrepancies.
The earlier exception covered monitor-run time ordering only. Monitors are committed
and verified at generation 16, with 113/164 signatures implemented; Mongo remains
the default/current runtime backend and the remaining 51 signatures and full
end-to-end acceptance gates are pending.

## Authorized terminal notification history correction

The user separately authorized the preceding terminal-history correction. Mongo
decode_delivery now uses a persisted terminal time only when the next-attempt field
is absent and the status is delivered/dead. Pending records still require a valid
next-attempt date, and a malformed present date remains InvalidData. This reads
already-written terminal records without changing writes, schemas or migrations.
Mongo production edits are one six-line fallback plus replacement of the field
initializer; no unrelated notification behavior was changed.

The new shared Testkit fixture passed on the real Mongo adapter (1/1 targeted case,
7.47 seconds). It checks readable delivered/dead history, pending retry dates,
diagnostic validation, first terminal result/payload/expiry preservation across
late retries, repeated enqueue and opposing terminal calls, project isolation,
creation-time ordering and page limits. A separate codec unit case passed (1/1),
checking missing pending dates, malformed present dates, terminal fallback,
preservation of existing terminal next-attempt dates and missing terminal dates.
The fixture uses current synthetic times and its own generated database, which
was dropped; the Docker catalog confirms no remaining fixture databases. The
same fixture is ready for the pending PostgreSQL NotificationStore implementation.
The selected complete Mongo infrastructure run passed 24 cases (real ignored
adapter cases, excluding performance_/reports_rps, one test thread), including the
new terminal-history fixture. Strict Clippy for Mongo/Testkit, formatting,
documentation, deployment profiles, compatibility and source inventory passed.
PostgreSQL source/runtime remained unchanged at generation 16 and 113/164 methods.

## Metrics aggregate rule decoder: awaiting a separate Mongo exception

Review of the next notification-rule capability found one missing supported enum
arm in Mongo decode_rule. Domain AggregateAlert::validate accepts Metrics without
environment/release filters; the application aggregate evaluator also explicitly
maps Metrics to QuerySource::Metrics. Mongo encode_rule writes dataset.as_str(),
so a valid Metrics rule is acknowledged and stored with g.d="metrics". The reader
only recognizes errors/logs/spans and rejects the stored value as InvalidData.
This makes list_rules fail for a page containing the rule and prevents the aggregate
scheduler from obtaining its claimed rule. Mongo's validator already accepts the
aggregate object, so this is a decoder defect, not a migration requirement.

An isolated public-port Rust Docker probe verified valid domain input, successful
upsert_rule acknowledgement, the stored native Metrics dataset, and InvalidData
from both list_rules and claim_due_aggregate_rule. All expected-defect assertions
passed (1/1 case, 1.05 seconds). It used only its generated current-time
metric_pg_notification_metrics_<pid>_<nonce> database, dropped it before checking
captured outcomes, and removed the temporary target after the process ended. The
Docker catalog confirmed no such databases remain. No production rule code was
modified; the previous terminal-history fix is committed separately.

The proposed Mongo production correction is one additional existing-enum arm:

```rust
"metrics" => ExploreDataset::Metrics,
```

Add a shared real-adapter rule round-trip/claim regression for all four datasets.
Implement the PostgreSQL rule codec with those existing four variants; no new
feature, domain/port change, Mongo schema or data migration is needed. Work on the
PostgreSQL notification adapter is stopped for the user's separate authorization,
following the explicit instruction to stop on newly discovered discrepancies.
The terminal-history decoder exception does not cover this rule decoder change.
Coverage remains 113/164 at PostgreSQL generation 16; the overall backend and
end-to-end acceptance gates are still incomplete and Mongo remains the default.

## Authorized Metrics aggregate rule decoder correction

The user explicitly authorized the missing Metrics decoder arm. The Mongo
production change is exactly one additional existing enum variant in decode_rule.
It does not change the encoder, validator, schema generation or migrations. The
shared Testkit fixture now round-trips, claims and completes valid aggregate rules
for Errors, Logs, Spans and Metrics and checks project isolation and next evaluation
state. Both Mongo notification Docker cases passed (2/2, 2.01 seconds), including
the earlier terminal-history regression. The fixture uses current synthetic dates
and drops its own generated database; installation data is not involved.
The selected complete Mongo infrastructure run also passed 25 cases (real ignored
adapter/migration cases, excluding performance_/reports_rps, one test thread),
including both notification fixtures. The production diff remains one enum arm.

## Transactional PostgreSQL notification control plane and outbox

Schema generation 17 adds typed destinations, rules, delivery history, per-project
notification lanes and a persisted identifier-only fairness cursor. The new
PostgresNotificationStore explicitly implements all eighteen unchanged methods.
Webhook, Telegram and SMTP destination metadata round-trip with opaque sealed
ciphertext; issue, aggregate and monitor rules retain their existing DTOs. All four
aggregate datasets are supported, and the PostgreSQL codec preserves the full u64
threshold. The adapter uses existing canonical/versioned/bounded JSON compression
for configuration and native bounded columns for scope, schedules, status, attempts
and lease ownership. SQL values are bound; only closed application-owned clauses
select table/query shapes. Driver diagnostics and secret values do not cross ports.

Project guards precede sorted notification lane locks. Worker candidates contain
only bounded native identity metadata; eligibility and project fences are rechecked
after locking. Delivery claims increment attempts and publish one durable lease;
the persisted cursor provides project/destination fairness across worker instances.
Concurrent claimers do not claim the same delivery or aggregate lease. Missing or
corrupt destinations leave the acknowledged attempt lease, allowing other work to
progress. Aggregate evaluation stores lease ownership separately from its original
scheduled slot; stale completion cannot enqueue deliveries or overwrite current
state. Monitor and aggregate state updates commit with their delivery outbox in
one transaction. Existing dispatcher and both evaluators remain unchanged.

Transition consumption commits with all delivery inserts. Stable delivery identity
permits retry after an unknown COMMIT even when the transition has already been
consumed; identity conflicts reject the whole operation. Duplicate enqueue and late
retry/terminal calls preserve the first payload and completed outcome. Delivered/dead
history uses the persisted terminal date for the non-optional next-attempt DTO field,
matching the separately authorized shared Mongo/Postgres regression. Scoped reads
and mutations prevent foreign rules/destinations from exposing another project's
ciphertext; deleted targets can still be retained as historical outbox identities.

Writes cap at 10000 deliveries and 16 MiB of decoded payload plus metadata, with
bulk SQL chunks of at most 1000 rows. Native cumulative response budgets bound
configuration, endpoint/ciphertext and payload before transfer/decompression.
The budget uses the greater of declared decoded and actual encoded configuration
size and reserves fixed metadata for every selected row, including suppressed
body rows; a corrupt stored size cannot defeat its transfer bound.
List limits stay within the existing port bounds; an over-budget non-paged response
fails explicitly rather than silently omitting rows. Native partial due indexes and
scoped history/control-plane indexes support selection; expiry indexes prepare for
the still-pending MaintenanceStore implementation.

Twelve real Docker cases passed: the two shared Mongo/Postgres fixtures; scoped CRUD,
all destination/rule kinds and corruption rejection; transition-consumption and
second-chunk SQL faults; concurrent fairness/lease claims and project fences; stale
aggregate completion; rule-state/outbox rollback for both evaluator kinds; actual
lost COMMIT responses for expansion, delivery claim and aggregate completion;
10000-row expansion and response/batch bounds; native due/history/destination index
plans without optimizer forcing; and the existing dispatcher, all-dataset aggregate
evaluator and monitor evaluator using real PostgreSQL stores. Delivery adapters in
tests are in-memory receipts; no external notification was sent.

The first monitor evaluator fixture incorrectly expected an issue-style payload
type. It was corrected to assert the existing cron_monitor/firing/error fields;
application and adapter behavior were unchanged. The failed own fixture database
was inspected, confirmed connection-free and dropped by its exact generated name
after the failed test process ended. Successful fixtures cleaned themselves.

The complete generation-17 PostgreSQL suite passed 132/132 with no ignored tests
in 142.28 seconds, including local verified TLS, weak-transport acknowledgement,
operator environment handling, schema ownership/recovery and all earlier families.
Source inventory now reports 131/164 signatures, with 33 pending. Archive,
Maintenance, ProjectDeletion, DebugFile and Artifact stores and full runtime/E2E/
load acceptance remain pending. Mongo is still the default and only composed
runtime backend; no incomplete PostgreSQL mode has been enabled.

After tightening the native notification transfer budget to account for corrupt
declared sizes and all selected metadata, the complete notification family was
rerun successfully (12/12 real Docker cases, 16.65 seconds). The large-boundary
case also inspects the native returned byte sum for 1000 corrupt maximum-sized
configurations with misleading size metadata; the 8 MiB budget still holds and
the public read rejects the malformed configuration.
Strict Clippy passed for Postgres, Mongo and Testkit with all targets/features and
warnings denied. Formatting, ADR-0034 dependency graph, documentation, deployment
profiles, compatibility and the regenerated source inventory passed. Both
PostgreSQL Docker catalogs have no remaining own fixture databases, and Mongo has
no remaining notification or archive-probe fixture databases. No installation
data, credential, image or volume was removed.

## Metrics archive validators: awaiting a separate Mongo exception

Review of the next ArchiveStore capability found two existing schema mismatches
on the supported Metrics path. They are independent of the authorized aggregate
notification-rule decoder correction, which is now committed and verified.

First, ArchiveKind::Metric is selected and serialized by MongoArchiveStore, but
archive_manifest_validator allows only event/log/span/session in kind. A valid
due metric bucket can be persisted with archive retention enabled, yet its public
ArchiveStore::claim returns Unavailable: the server rejects the writing manifest
with Mongo validation code 121. No manifest is committed and the source retains
its archive hold h, without an expiry x or segment z.

Second, after adding only the missing manifest kind to the validator in an isolated
throwaway database, the same public claim and complete succeed. Public
commit_sources then returns Unavailable. The generic archive commit writes a binary
segment ID to z, expiry date to x and removes h. metric_validator currently requires
z to be a date, so the server rejects that update with validation code 121 as well.
The complete manifest remains uncommitted and the original metric still retains h;
the probe did not delete a source or lose its measurements.

Two isolated real Docker probes passed expected-defect assertions. The first
(1/1, 1.07 seconds) verified public rejection, native code 121, unchanged held
source, absence of a manifest and acceptance of an otherwise identical manifest
when only its kind was changed to session. The extended probe (1/1, 0.88 seconds)
also verified public claim/complete after the temporary manifest-validator
adjustment, public source-commit rejection, native code 121 for its exact update
and preservation of the source. Each used current synthetic dates and its own
metric_pg_archive_metrics_probe_<pid>_<nonce> database. The collMod diagnostic
affected only that generated database, which was dropped before assertions were
evaluated. The temporary Rust target was removed after the process ended; the
Docker catalog confirms no probe databases remain. Mongo production archive,
metric, validator, index and migration code was not changed.

A narrowly scoped repair needs more than another decoder arm: allow metric
manifests and explicitly distinguish the legacy ordinary-retention z date from
the archive-segment z binary plus x expiry. Keep the legacy z-date TTL index and
add a separate x-date expiry index for archived metric hot copies. Source inspection
shows the current four metric indexes contain no x expiry index; merely admitting
binary z would leave those hot copies without automatic expiry. This retention
consequence is a source finding, not a claimed passing end-to-end deletion test.
Use a successor registered Mongo schema migration to install corrected validators
and the new index on existing databases; do not rewrite installed migration steps
or user metric payloads. Add real fresh/upgraded-database archive publication,
commit retry, integrity, expiry/hold and preservation regressions before declaring
the path fixed. PostgreSQL should use its existing separate native archive_segment,
archive_due and expire_at columns and unchanged archive service/ports.

Implementation of the next capability is stopped for the user's separate decision,
following their explicit requirement to stop on new discrepancies. The earlier
authorization covered the notification Metrics decoder only and does not authorize
these Mongo validator, index or migration changes. Completed notification work is
committed on feat/postgres; PostgreSQL remains generation 17 with 131/164 methods,
33 pending, and no exposed incomplete runtime backend. Mongo remains the default.

## Authorized minimal Metrics archive schema repair

After reviewing the two validator failures and the existing configuration gates,
the user authorized this separate, narrowly scoped Mongo exception. Metrics
archival remains optional: both archive.enabled and retention.metric_archive must
be enabled. All supplied profiles retain their disabled archive defaults.

The manifest validator now admits metric. The Metrics validator preserves the
ordinary z-date retention representation and also admits a committed archive
source with a 16-byte binary z segment, date x expiry and no h archive hold.
Malformed segment lengths, missing/invalid expiry and simultaneous archive holds
are rejected by Mongo itself. The ordinary z TTL index is retained; a separate
partial x TTL index applies only to binary-z archive hot copies. Ordinary metrics
with an incidental expired x are not captured by that new index.

A registered schema-only generation 21 to 22 migration installs those two
validators and the new index. Existing generation 19 to 20 and 20 to 21 production
steps are unchanged. Preflight accepts only the known legacy/current validator
shapes and a compatible named index, blocking unknown validators or conflicting
indexes before modifying the schema. DDL steps can be replayed after interruption;
the migration does not read, transform or replace user metric payloads. Fresh
databases bootstrap generation 22, and current operational/update documentation
now describes that generation and the complete transition chain.

A new storage-independent Testkit fixture exercises the public MetricStore and
ArchiveStore lifecycle on fresh and upgraded Mongo databases: persistence, claim
retry, refusal to commit before completion, idempotent completion, checksum/source
identity conflicts, complete-manifest resume, idempotent source commit and blob
reference preservation. The upgrade test also preserves ordinary/held metric
documents and unrelated DSN data, retains all previous metric index names and
checks startup after partially applied DDL. The real TTL-monitor case expires
both an ordinary metric and an archived hot copy, preserving an archive hold and
an ordinary metric with future z expiry; it does not change global Mongo settings.
The existing server archive E2E now includes Metrics alongside events, logs, spans
and sessions, checking publication to the real local BlobStore and Parquet bytes.

The selected Mongo Docker regression suite passed 28/28 cases, including all
three new cases and registered migrations from supported older generations.
The focused fresh Metrics lifecycle/validator/TTL run also passed separately
(1/1, 22.51 seconds); the complete-suite TTL rerun passed in 44.81 seconds.
The real server archive E2E passed (1/1, 1.95 seconds), publishing all five telemetry
families through ArchiveService and LocalBlobStore. Mongo's catalog contains no
remaining databases owned by these Metrics archive/migration or server E2E cases.
The ordinary Mongo library suite passed 32/32 cases; its five Docker-only cases
were exercised by the selected infrastructure run above.
Strict Clippy passed for Mongo, Testkit and Server with all targets/features and
warnings denied. Formatting, ADR-0034 dependency graph, documentation, deployment profiles, compatibility
and the regenerated PostgreSQL source inventory passed.
PostgreSQL remains generation 17 with 131/164 methods and 33 pending. This repair
does not expose an incomplete runtime backend or change the default Mongo choice.

## Archive mutable Metrics source: confirmed separate discrepancy

Resuming ArchiveStore implementation after the authorized schema repair exposed a
separate data-preservation issue. Metric buckets remain mutable after archive
publication and source commitment. MongoMetricStore::apply_delta updates the
existing aggregate without checking archive ownership/segment state, while
MongoArchiveStore::commit_sources checks source identities and segment conflicts
but does not compare the current contents with the published snapshot. A successful
late delta therefore can remain only in a hot copy whose TTL was already assigned.
The current PostgreSQL MetricStore likewise updates an existing bucket without an
archive_segment guard; its ArchiveStore has not yet been implemented. PostgreSQL
must not reproduce this loss as part of the next capability.

A temporary isolated real-server Docker probe passed expected-defect assertions
(1/1, 25.53 seconds). It used current synthetic dates, an archive-enabled one-day
MetricRetention and a synthetic bucket from two days earlier. The ordinary
ArchiveService published and verified a real LocalBlobStore Parquet object from a
sum-2 counter snapshot. A test-only ArchiveStore wrapper deterministically
interleaved a normal accepted sum-3 delta after completion and before source
commit, then an accepted sum-4 delta immediately after source commit. Both writes
used the public MetricStore path. The resulting native hot record had sum 9,
measurement count 3, no archive hold and the original ten-second hot-copy expiry.
Another public archive claim found no work. The actual Mongo TTL monitor deleted
that hot source, while the referenced immutable archive bytes and checksum still
matched the original sum-2 Parquet exactly. This was a synthetic loss reproduction,
not a finding about the user's stored telemetry.

The fixture used only its generated metric_pg_archive_metric_late_probe_<ObjectId>
database and crates/server/target/metric-pg-archive-probes/<UUID> blob directory. Cleanup dropped
that database and checked the canonical blob-directory parent before removing
its own files. The Docker catalog confirms no matching fixture databases remain.
The temporary test target was removed only after its Cargo process completed;
no Mongo/PostgreSQL production code, schema, installed migration or deployment
configuration was changed during this diagnostic.

Source review also confirms Metric ingest normalization bounds timestamp validity
but does not reject measurements merely for being older than the metric retention
window. The storage interleaving is therefore not ruled out by a documented
upstream age guard. The probe confirms Metrics only; no equivalent Session failure
is claimed without a separate reproduction.

Implementation is stopped for the user's decision, as explicitly required for a
new discrepancy. The previous exception authorized schema admission and TTL only;
changing foreground Metric writes or archive source-consistency semantics requires
separate authorization. The recommended acceptance behavior is to preserve
successfully accepted late values, retain/requeue a changed source, and publish a
new immutable segment instead of expiring unarchived changes or overwriting the
old object. The exact revision/claim guards and retry identity must be designed
before coding so interruption and concurrent writes cannot reopen the same gap.
Archive defaults remain disabled, PostgreSQL remains generation 17 with 131/164
methods and no exposed incomplete runtime backend, and Mongo remains the default.

## User decision: accept the late-Metrics archive limitation

After reviewing the real loss reproduction and proposed revision/re-archival
protocol, the user explicitly chose to document the issue and accept this current
loss window in exchange for simpler code. This supersedes the preceding stop and
recommendation: the late-Metrics discrepancy is no longer an implementation
blocker. PostgreSQL should retain the existing snapshot/expiry semantics, with
the limitation reported honestly instead of claiming loss-free late updates.

No bucket revision, automatic re-archival, replacement-segment versioning or extra
waiting protocol is added. Existing archive publication verification and retention
holds on failure remain required. The acceptance is specific to late updates of
an old Metrics bucket between snapshot creation and hot-copy removal; it does not
authorize other data-loss windows or unrelated Mongo changes. Its frequency and
volume in real workloads remain unmeasured, so the synthetic reproduction is not
an estimate of production loss. Archive and Metrics archive defaults stay disabled.

User-facing known limits now describe the enabling flags, possible late-update
loss and asynchronous deletion even with a zero hot-copy delay. The configuration
table now correctly describes hot_copy_delay as an expiry delay after publication,
rather than a wait before archive work. ADR-0052 and the implementation plan record
the accepted scope and deferred mechanism. This decision changes documentation
only; PostgreSQL remains generation 17 with 131/164 methods and 33 pending.

## 2026-10-09 — Archive implementation and confirmed TLS provider blocker

Uncommitted PostgreSQL work adds all four ArchiveStore methods and append-only
SQL migration 18. Generated source inventory is 135/164 with 29 remaining; this
is implementation presence, not completion of all validation or application
composition. Mongo sources, installed Mongo schema and default configuration
are unchanged. The shared application ArchiveService and actual LocalBlobStore
publish the existing Parquet formats; PostgreSQL adds no second blob publisher.
Session and Metric JSON payload fields are adapter-owned logical representations,
while their enclosing shared Parquet schema stays unchanged.

The real Docker Archive group initially passed 8/8 tests in 9.52 seconds. The
subsequent full ignored PostgreSQL library run passed 134/136 in 135.32 seconds,
including nine Archive tests, actual Parquet publication for all five families,
cross-project equal source identities, concurrent claims/completions/commits,
native committed transactions with deliberately lost replies, failed resume
holds, corrupted metadata, oversized encoded Event bodies, terminal/Pending
selection, and native EXPLAIN ANALYZE/BUFFERS evidence. Representative plans
used log_archive_due, log_archive_project_day and archive_manifests_pkey without
disabling sequential scans. Real migration from an actually installed generation
17 to 18 passed with byte-for-byte logical preservation of a held Metrics row;
repeat bootstrap was idempotent. A tenth Archive test for the one-record byte
budget exception has compiled but has not yet executed. The previously accepted
late-Metrics snapshot/expiry limitation is retained and explicitly exercised.

That full run initially failed two TLS tests because the invocation mistakenly
used localhost, absent from the fixture certificate SAN. Retrying the connection
group with its intended 127.0.0.1 endpoint passed 3/4 tests in 5.19 seconds,
including inherited client certificate settings and the verified full connection.
The remaining failure is a newly confirmed SQLx/Rustls provider-selection panic
in the explicitly acknowledged VerifyCa connection. It is not an expired test
certificate or a reason to disable certificate verification.

Archive's real LocalBlobStore dev dependency brings the existing S3 client's
aws-lc-rs Rustls provider into the same test binary as SQLx's configured ring
provider. SQLx 0.9.0's normal handshake uses ClientConfig::builder_with_provider,
but its accept_invalid_hostnames path constructs WebPkiServerVerifier::builder
without a provider. Rustls 0.23.45 then cannot auto-select between the two enabled
providers and panics. Existing public ConnectionPolicy::VerifyCa reaches this
path; Require with an explicit root intentionally maps to VerifyCa as well.
This combination also matters for future server composition with the S3 adapter;
isolating or deleting the actual blob integration test would conceal the issue.

Implementation is stopped for the user's decision on this newly confirmed
discrepancy. Proposed narrow repair: explicitly select the already configured
ring provider inside metric-postgres before driver TLS initialization, retaining
an existing process-level provider if one has already been installed. This would
require a direct Rustls dependency in the PostgreSQL crate and actual Docker
regressions for VerifyFull, VerifyCa and Require with a root, including negative
CA/name tests. No driver fork, Mongo edit or weakened trust policy is proposed.
No provider repair has been applied. Strict Clippy and final Archive verification
remain pending; the current capability work has not been committed as delivered.

Commands used real fixtures metric-pg-dev-postgres-1 and
metric-pg-dev-postgres-tls-1, with METRIC_TEST_POSTGRES_URI and the fixture-only TLS
URI/root/client certificate variables. Cargo was serialized with --locked -j1:
`cargo test -p metric-postgres archive::tests:: --lib --locked -j1 -- --ignored --test-threads=1`,
`cargo test -p metric-postgres --lib --all-features --locked -j1 -- --ignored --test-threads=1`,
and `cargo test -p metric-postgres connection::tests:: --lib --all-features --locked -j1 -- --ignored --test-threads=1`.
Source coverage report, documentation, SDK compatibility and deployment-profile
validators passed. Fixtures use generated disposable databases and a canonical
task-owned LocalBlobStore directory; no global Docker cleanup was performed.

## 2026-10-09 — Archive and compatible TLS initialization verified

The user approved the narrow provider-selection repair. ConnectionPolicy now
initializes Rustls's existing ring provider before a native TLS connection when
no process provider exists. The SQLx policy and trust checks are unchanged;
plaintext connections do not initialize a TLS provider. A racing/existing
application provider is retained. This applies equally to pooled, migration and
native diagnostic connections through the shared connect_options boundary.
The PostgreSQL crate references the already locked Rustls 0.23.45 with default
features disabled; neither SQLx nor any dependency version was upgraded/forked.

The corrected full real Docker command above passed 138/138 ignored PostgreSQL
library tests in 163.11 seconds, including all ten Archive tests and migration
17→18. VerifyFull, VerifyCa and Require with an explicit root passed; wrong CA
and certificate-name rejection still passed. A fresh child process installed
aws-lc-rs before any PostgreSQL connection, then proved actual encrypted native
connections in all three TLS modes retain the exact installed provider Arc.
Actual plaintext connection, operator-environment redaction, transaction reset
and exhausted-pool timeout regressions passed as well.

`cargo clippy -p metric-postgres -p metric-testkit --all-targets --all-features --locked -j1 -- -D warnings`
passed. Ordinary `cargo test -p metric-postgres -p metric-testkit --all-targets --all-features --locked -j1`
passed six PostgreSQL unit tests and three testkit unit tests; its 138 PostgreSQL
ignored cases were separately executed in the Docker run, not treated as passes
by ordinary Cargo. Formatting, dependency graph, documentation, compatibility,
deployment profiles and source coverage report passed. Provider regression was
performed with both providers enabled through the actual Blob/S3 dependency.

ArchiveStore's four methods are now verified on PostgreSQL generation 18, with
135/164 source methods implemented and 29 remaining. No Mongo file or common
public port, scheduler, archive publisher, blob implementation or wire contract
changed. Only existing Session/Signal decoder visibility was widened inside the
PostgreSQL crate to reuse validation. The accepted late-Metrics loss window stays
documented and covered without bucket revisions or an extra waiting protocol.
Native expiry execution belongs to the next MaintenanceStore capability; this
Archive phase proves expiry assignment and retention holds, not automatic native
deletion. Final application composition, whole-workspace/container/SDK/offline
and soak gates remain pending until the remaining capabilities are implemented.

## 2026-10-09 — Native MaintenanceStore verified

PostgresMaintenanceStore implements the existing single run capability and all
eight Scheduler task dispositions. RetryBacklog reports the native bounded due
probe; EventRetention repairs terminal-event deadlines while never expiring a
Pending event or overwriting a completed archive's hot-copy deadline. HourlyRetention
repairs Issue hour deadlines. Native expiry phases replace Mongo's background TTL
for Error, Log, Span, Session, Metrics, Feedback, Replay, Monitor-run and terminal
notification records, authentication-token/session metadata and the three hourly
statistics families. No database-wide delete, raw query port or new Scheduler task
was added. Previously configured binary UploadExpiry/BlobOrphanRegistration probes
explicitly remain Disabled, matching the existing Scheduler adapter behavior;
actual binary metadata lifecycle belongs to the remaining binary stores.

Each expiry statement selects at most batch_size rows via its native expiry index,
locks with SKIP LOCKED and deletes exactly that selection. ctid is local to one SQL
statement and never appears in a persisted cursor. Source/reference removal is
atomic in the same transaction; foreign-key cascades remove the owning Feedback
attachments and Replay segment metadata. Archive holds, live Sessions, Pending
Events and non-terminal Monitor/notification records are excluded. A lost COMMIT
reply returns Unavailable; a repeated call observes the durable state without
releasing another record's hold. Physical removal is scheduled, not immediate at
assignment of expire_at, including a zero hot-copy delay.

Cursor tags distinguish tasks/phases; identifier widths, scope, batch sizes,
retention bounds and deadline overflow are checked. SQL values are bound and all
table/projection/order identifiers are adapter-owned literals. Event/Issue/hourly
repairs hold sorted project guards before source mutations. Issue reconciliation
only raises lifetime counts to a bounded retained-event lower bound and never
decreases them after hot-event deletion. Environment quota reconciliation uses an
exact native project-scoped aggregate under an exclusive project guard, rather
than replacing a count with a truncated page. At most 32 projects/Issues are
reconciled per call; unusually large exact catalog aggregates remain bounded by
the connection's statement timeout and Scheduler cancellation, not by a claim
that a complete catalog always fits in one row page.

Seven real Docker Maintenance tests passed in 7.75 seconds. They verify actual
removal of all five archived hot-source families after their delay, preservation
of equal IDs belonging to another held project, Pending/failed-event behavior,
hourly policy repair, lifetime counts, exact quota repair with more environments
than batch_size, actual concurrent expiry and native dropped COMMIT replies,
malformed cursors/limits/corrupt Pending metadata/deadline overflow, partial-index
EXPLAIN ANALYZE/BUFFERS evidence and bounded deletion counts. An induced Blob-reference
DELETE trigger failure proved both source and references roll back; retry then
removed both. The actual existing application Scheduler accepted and ran the
PostgreSQL adapter through its public MaintenanceStore port.

The full real Docker PostgreSQL library command with all TLS fixture variables,
--all-features --locked -j1 -- --ignored --test-threads=1 passed 145/145 tests
in 156.33 seconds. Strict PostgreSQL/testkit Clippy with all targets/features and
-D warnings passed. Documentation, compatibility, deployment profiles and source
coverage report passed. Existing archive test fixtures are reused within the
PostgreSQL test build; no application, domain, port, configuration or Mongo
production source was changed. No new SQL migration is needed; generation stays
18. Source capability count is 136/164 with 28 pending: ProjectDeletionStore (4),
DebugFileStore (10) and ArtifactStore (14). Whole-application composition and final
container/SDK/offline/soak delivery gates remain pending.

## 2026-10-09 — Confirmed Mongo catalog-counter truncation; stopped for scope decision

After the approved TLS repair and the PostgreSQL Maintenance implementation,
boundary parity review found a pre-existing Mongo Maintenance quota bug.
MongoMaintenanceStore::reconcile_environment_quota limits a project's environment
read to request.batch_size, counts only that prefix, then writes the prefix count
as catalog_usage.ec. That value is subsequently used by MongoFinalizationStore's
reserve_environment admission guard against max_implicit_environments_per_project.
The row-page limit is therefore incorrectly treated as the full cardinality.

A temporary isolated real Docker diagnostic invoked the actual public Mongo
MaintenanceStore and its normal continuation loop. It bootstrapped a fresh
generated metric_phase14_maintenance_test_<ObjectId> database, inserted one valid
synthetic project and five schema-valid distinct environments, and initialized
the correct stored counter to five. A full CounterReconciliation pass with
batch_size=1 left all five environments present but changed catalog_usage.ec to
one. The expected-defect diagnostic passed 1/1 in 1.24 seconds; this is proof of
the bug, not a correctness pass. Command:
`cargo test -p metric-mongo --test maintenance_store diagnostic_catalog_reconciliation_above_page_size --locked -j1 -- --ignored --exact --nocapture --test-threads=1`
with the existing task-owned Mongo Docker fixture URI.

This does not demonstrate removal of user environments or an incident in the
user's database. Its demonstrated effect is corruption of the quota counter;
because foreground environment admission reads that counter, exceeding the
configured cardinality limit becomes possible. Trigger condition is that a
project has more environments than the maintenance row-page size. PostgreSQL's
exact project-scoped quota repair already passes the analogous above-page-size
test. The native Mongo counter's truncation behavior is not an intended backend
contract to duplicate.

The generated diagnostic database was dropped successfully before final
assertions. The temporary test function was removed after its Cargo process
finished, restoring the Mongo test file to its committed contents. No Mongo
production code, schema, migration or deployment configuration was changed.
Implementation is stopped for the user's requested decision before any additional
Mongo exception. Proposed scope is a minimal repair of this counter behavior
with explicit bounded-query and foreground-admission concurrency checks; do not
introduce a broad Mongo refactor or replace the counter with a truncated result.
No repair or new exception has yet been authorized.

Completed implementation commits are b84c2a9 (ArchiveStore and compatible TLS
initialization) and cc2e92d (native MaintenanceStore/expiry), both on feat/postgres.
PostgreSQL remains generation 18, source signatures 136/164 with 28 pending.
The most recent full PostgreSQL Docker run passed 145/145; strict adapter/testkit
Clippy, formatting, dependency graph, documentation, compatibility and deployment
profile checks passed. Ordinary PostgreSQL tests passed 6/6 and explicitly skipped
the 145 real cases already executed separately. Only this diagnostic report
remains as an additional documentation change.

## 2026-10-09 — Approved minimal Mongo catalog-counter repair

The user authorized this specific Mongo exception after reviewing its trigger,
quota consequences and ordinary configuration. Only Mongo maintenance production
code and its existing integration-test target changed; no Mongo schema, migration,
finalizer, deployment configuration or shared port was changed.

Environment reconciliation now obtains the exact project-scoped cardinality using
count_documents with the existing environment_project_timeline index hint and a
10-second native maxTimeMS budget. batch_size still bounds the outer project page;
an unusually large complete catalog aggregate is time-bounded rather than treated
as a bounded row prefix. Checked conversion rejects counter overflow. Atomic $max
repairs underestimated counts while retaining foreground quota reservations that
can precede materialization. Reconciliation intentionally does not lower an
overestimated reservation count or claim recovery of failed reservations; this
avoids overwriting concurrent admission increments.

The original maintenance regression and new above-page-size/reservation test
passed 2/2 against the real isolated Mongo Docker fixture in 2.12 seconds. Evidence
covers five environments with batch_size=1, correct and previously truncated
counters, a missing counter, a reserved sixth slot before materialization, twenty
foreground increments concurrent with twenty complete reconciliation passes, and
an indexed native aggregate EXPLAIN. All five environment records remain present.
Each generated fixture database was dropped. The initial test's zero-valued
counter was correctly rejected by the existing validator; the fixture was fixed
to represent zero usage as the absent field without changing the adapter/schema.
Strict Mongo maintenance-target Clippy with --locked -j1 -- -D warnings passed.

Commands used the existing task-owned METRIC_TEST_MONGODB_URI:
`cargo test -p metric-mongo --test maintenance_store --locked -j1 -- --ignored --test-threads=1`
and `cargo clippy -p metric-mongo --test maintenance_store --locked -j1 -- -D warnings`.
PostgreSQL's exact quota reconciliation already passed its Docker boundary case;
the remaining PostgreSQL binary/lifecycle stores and application gates resume next.

## 2026-10-09 — PostgreSQL DebugFileStore and schema generation 19

Added all ten DebugFileStore methods in the PostgreSQL crate. Append-only migration
0019 adds bounded native debug-file/upload metadata and project byte/file quota
counters; migrations 1–18 remain immutable. The existing PostgreSQL ProjectView
decoder is reused with crate-only visibility. No Mongo production/test source,
common port, service, configuration or filesystem Blob implementation changed in
this step. DebugFileQuota retains the existing adapter default (10 GiB and 10,000
files per project), with checked SQL bigint bounds and zero quotas denying new
publication. Configuration/runtime composition is still pending.

Project guards precede upload/file mutations. Publication atomically inserts
metadata, charges quota, increments the symbolication revision and completes the
upload. A duplicate SHA1/content identity does not charge or increment twice;
conflicting content and wrong-project/organization uploads fail closed. Deletion
and quota/revision release share one transaction and preserve another project's
equal local file ID. Native commit ambiguity maps to Unavailable, with durable
metadata observable by a retry. Driver errors never become returned SQL/secret
strings. Name normalization and DebugId/CodeId decoding reuse domain rules. Chunk
manifests are bounded to 256 SHA1 entries, recovery to 1,000 rows and lookup to 20;
variable names/chunks/error strings are bounded before transfer. Every SQL value
is bound and all identifier/query branches are adapter literals.

Seven new real Docker cases passed 7/7 in 6.31 seconds: full lifecycle/scope/lookup,
concurrent quota admission and eight duplicate publications, native dropped COMMIT
replies for publication/deletion, induced upload-completion failure rolling back
file/quota/state, invalid bounds and corrupted manifest reads, project fencing,
and native indexed EXPLAIN ANALYZE/BUFFERS on 4,096 synthetic records. The existing
DebugFileService and actual LocalBlobStore assembled a SHA1-verified chunk,
published/reopened/read the same bytes, handled repeat assembly and removed the
metadata/file with the expected revision. Its generated target-only Blob directory
was verified and removed. Two isolated databases left by initial test-setup SQL
errors were positively identified by generation 19 and these synthetic fixtures
and dropped; the prepared multi-command test SQL was corrected to raw static SQL.

A real migration test upgraded generation 18 to 19 twice, preserving prior project
fields, initializing counters to zero and retaining archive infrastructure. The
existing generation-17 held-Metric upgrade also still passes through the current
registry. Full PostgreSQL real Docker library run with all local TLS variables,
--all-features --locked -j1 -- --ignored --test-threads=1 passed 153/153 in 162.73
seconds. Strict PostgreSQL/testkit Clippy, formatting, dependency graph,
documentation, compatibility and deployment-profile validators passed.

Source inventory is 146/164; remaining methods are ArtifactStore (14) and
ProjectDeletionStore (4). Final server/container/SDK/offline/soak gates remain
pending; this step does not claim a complete PostgreSQL application runtime.

## 2026-10-09 — PostgreSQL ArtifactStore and schema generation 20

Added all fourteen ArtifactStore methods inside the PostgreSQL crate. Append-only
0020 adds organization-scoped bundle/upload metadata, normalized exact project/
release/dist associations, quota counters, debug-token lookup indexes and GC
state/generation/claim deadlines. Previous migrations remain immutable. SHA1 and
ZIP are test-only dependencies already locked in the workspace; Cargo.lock changes
only the PostgreSQL dependency list, with no version upgrades. Shared services,
source-bundle parsing, BlobStore, authorization and symbolication remain unchanged.
Zero organization quotas retain the existing unlimited convention, within checked
native bigint bounds.

Sorted project guards precede organization, upload and bundle mutations. Native
transactions make association merge, publication/rescue, quota reservation,
symbolication revisions and upload completion atomic. Duplicate publication does
not charge or increment again. Exact removal preserves other associations and
only the final removal enables GC. Same-organization projects share one content
identity; wrong-organization reads/associations are fenced. Stable application-
derived organization/SHA1 upload identity is enforced natively. Repeatable-read
transactions keep returned content and association scope consistent.

GC claims recheck native state under sorted organization locks and row locks.
Publication is blocked during a physical-delete claim; stale tokens/generations
cannot finish a later claim. Post-tombstone publication reserves one incremented
generation; finish and quota release commit together. Tombstones retain bounded
identity metadata internally until expiry and are hidden from lookup. Blob
deletion remains owned by the existing GC service. Native maintenance adds bounded
expiry of terminal uploads and tombstones, preserving Pending/Assembling uploads
and active/claimed/reserved bundles.

SQL values are bound and identifier/operator branches are static allowlists.
Domain codecs validate IDs/bindings. Bounds are 64 SHA1 chunks, 512 canonical
associations, 20,000 tokens, 20 lookup candidates, 1,000 recovery rows and 100 GC
candidates. Chunk/token fields are bounded before driver transfer; recovery/GC
accumulation has a 4 MiB budget. Invalid/corrupt data, quota overflow and driver
failure return static port errors without SQL/secret strings.

Eight real Docker Artifact cases passed 8/8 in 9.56 seconds; an additional concurrent
association case passed 1/1 in 1.47 seconds. Evidence includes merge/scope/lookup/
exact detach/revisions, concurrent quota admission/duplicate publication, orphan
rescue, expired-claim takeover, post-tombstone generation, actual dropped COMMIT
replies at publication/removal/claim/finish/reservation, native rollback of induced
upload-completion failure, bounds/corruption/fences/expiry, and indexed EXPLAIN
ANALYZE/BUFFERS on 4,096 synthetic rows. Existing ArtifactService assembled/parsed
an actual SYSB/ZIP bundle via the shared DebugFileService chunk uploader and
LocalBlobStore, reopened the same bytes, reused completed upload metadata, resolved
its debug token and physically garbage-collected its content. Concurrent callers
retained all 32 new associations plus the original, with one quota charge and
33 revisions.

A real 19-to-20 upgrade verification preserves published Debug metadata, project
fields/counters and prior organization fields while initializing artifact quotas.
The initial uncommitted migration's NULL dist check was corrected before delivery.
Seven failed-test databases were positively verified as generated generation-20
synthetic fixtures with zero uploads/bundles and dropped. The failed service test's
single synthetic Blob was inspected and SHA1-verified before non-recursive removal;
successful tests remove their own generated databases/directories. Strict adapter/
testkit Clippy and documentation/compatibility/deployment validators passed.
The complete real PostgreSQL Docker library suite with all local TLS variables,
--all-features --locked -j1 -- --ignored --test-threads=1 passed 163/163 in 190.64
seconds. All nine new Artifact cases and the real migration upgrade passed in
that run. The failed-test Blob directory was also removed non-recursively using
explicit literal paths after its verified synthetic file was removed; no fixture
database or Blob directory from this step remains.

Source signatures are 160/164. ProjectDeletionStore's four methods and final
server/container/SDK/offline/soak gates remain pending; full runtime/configuration
support is not yet advertised as delivered.

## 2026-10-09 — Confirmed Mongo Artifact association race; stopped for scope decision

Parity review found a separate pre-existing MongoArtifactStore::publish race. Its
active-content branch reads the old association list, merges additions, then
replaces the bundle using only ID/state as the guard. Another publisher can change
the list meanwhile. Both return success, but the later replacement erases an
already acknowledged association. Revisions can advance while metadata is lost.

A temporary real Docker diagnostic used existing public ports in a newly generated
metric_phase14_maintenance_test_<ObjectId> database. It bootstrapped synthetic
organization/project metadata, published one bundle with one association, then
issued 32 concurrent publications of the same content, each adding a distinct
valid release association. All 32 returned success. Expected count was 33; actual
stored count was 3. The expected-defect assertion passed 1/1 in 1.36 seconds; this
is bug evidence, not a correctness gate. Command:
`cargo test -p metric-mongo --test maintenance_store diagnostic_artifact_binding_concurrency --locked -j1 -- --ignored --exact --nocapture --test-threads=1`.
No artificial delay, database failpoint or user database was involved.

The diagnostic database was dropped before assertions. Its temporary test was
removed, restoring Mongo source to the approved catalog-counter repair; working
and staged Mongo diffs are empty. A database listing confirmed no diagnostic
fixture remained. PostgreSQL retains all 33 associations in the analogous public-
port case. This did not delete raw events or Blob bytes; the risk is losing the
association used to discover a source bundle for project/release symbolication.

Further implementation stops at this new confirmed discrepancy, as requested.
The catalog-counter exception does not authorize this separate Mongo repair.
Proposed minimal scope: include the previously read association list in the
active-content update guard, including the empty/orphan case, so a stale publisher
receives the existing Busy/retry outcome instead of acknowledging an overwrite.
Prove concurrent preservation and orphan rescue on Docker. No repair of this race
has been implemented or authorized, and no broader Mongo refactor is proposed.


## 2026-10-10 — Approved minimal Mongo Artifact association repair

The user approved guarding the active-content replacement with its previously
read association array. The guard uses the original BSON value; absent b uses
$exists:false so an active orphan can be rescued. Native ID/state guards and the
existing reserved-generation/quota path remain intact. No schema, shared service,
HTTP, configuration or PostgreSQL changes are part of this exception.

Publication now permits at most 16 attempts (one initial attempt and 15 retries),
matching existing Mongo association merge/removal bounds. Only a definite active
association CAS miss is retried: re-read the document, validate content identity,
and merge the original incoming associations into the latest stored list. A
candidate from an earlier failed attempt is never carried forward; otherwise an
association concurrently removed by another caller could be restored. Driver
errors, unavailable/claimed/tombstoned content and reserved-generation misses
retain existing outcomes; ambiguous writes and post-publication revision/upload
errors are not replayed by this loop. Exhaustion returns ArtifactStoreError::Busy,
not a success. No sleeps or new TOML controls are introduced.

Existing application behavior is unchanged: Busy from incomplete assembly is
transient and restores Pending. Recovery processes at most 100 eligible uploads
per cleanup tick; the server uses artifacts.gc_interval (default 15m). There is
no global attempt limit for that pre-existing background recovery. Complete-upload
fast-path Busy propagates to the caller; completed uploads are not picked by the
Pending/Assembling recovery scan. The current HTTP mapping returns 409 for Busy,
so this report does not claim an automatic client retry or guaranteed completion.

A permanent real Mongo Docker regression bootstraps a fresh synthetic database,
publishes one association, then starts 32 public-port publishers together. Callers
may retry Busy at most eight calls in the regression; every successful addition
must remain observable. All 33 associations survive, each new association advances
one project revision, and duplicate publication changes neither revision nor
organization quota. Detaching all associations removes field b; re-publication
rescues that orphan, removes its due deadline, blocks GC selection and retains
one quota charge. Every worker is joined before cleanup. The generated database
is dropped before final error propagation; no user data or Blob files are involved.

Command: cargo test -p metric-mongo --test maintenance_store --locked -j1 --
--ignored --test-threads=1 --nocapture, with METRIC_TEST_MONGODB_URI pointing to
the isolated metric-pg-baseline Docker fixture. All three tests passed 3/3 in
3.52 seconds, including the previously approved exact catalog-counter regression
and bounded maintenance case. Strict Mongo library/maintenance-target Clippy with
all features, --locked -j1 and -D warnings passed. The correction is scoped to
Mongo Artifact publication and its regression; PostgreSQL remains at 160/164
methods, with ProjectDeletionStore and final runtime/acceptance gates pending.


## 2026-10-10 — Complete PostgreSQL project-deletion lifecycle

Generation 21 appends project deletion jobs, a scrubbed project tombstone with
independent slug reservation, bounded purge indexes and an Artifact GC operation
marker. Installed migrations are unchanged. The 20-to-21 upgrade is tested twice
and preserves existing project/Artifact rows and associations.

PostgresProjectDeletionStore implements all four current operations. Requests
atomically fence the project and suspend active keys; pending cancellation restores
its previous state and keys. Project-before-job locking serializes cancellation,
purge and ingest. Dataset mutation and progress commit together; definite failures
roll back and record only a static error category with bounded exponential delay.
Unknown commit results are observable through the same operation/status identity.
Native table/column names come only from the adapter-owned append-only registry;
request values are bound. Owned data uses project-scoped ctid batches. Shared
Artifact/Release/Deploy associations use bounded parent-ID batches and preserve
other projects. A second full pass catches remaining records. Artifact orphans
retain quota until fenced GC finishes, and project deletion waits for that marker.
Tombstones remove user-facing names/policy payloads; slug release remains possible
when the completed job has already expired. Terminal job expiry is appended to
the existing PostgreSQL maintenance registry. Mongo files are unchanged.

Seven real Docker cases cover lifecycle/cancellation/key states, every registered
project FK, existing archive source families/dependencies, shared binary metadata
and Release/Deploy links, concurrent request/cancel/purge and ingest fencing,
actual COMMIT-reply loss, transactional failure/redacted retry, restart/rescan and
slug release independent of job history. A representative native EXPLAIN
(ANALYZE, BUFFERS) verifies project-indexed bounded Event selection. Two initial
synthetic direct SQL fixtures violated existing Event constraints; their fixtures
were corrected, without changing those adapter constraints. Failed-test databases
were individually inspected and removed; successful fixtures clean themselves.

Strict PostgreSQL all-target/all-feature Clippy with --locked -j1 -D warnings
passed. Targeted deletion tests passed 7/7 in 13.07 seconds. The complete native
PostgreSQL Docker library suite, including local TLS and schema upgrades, passed
171/171 in 186.52 seconds using --all-features --locked -j1 -- --ignored
--test-threads=1. The six ordinary tests also passed. The generated fixture
DB listing is empty. Source signatures pass --require-complete at 164/164;
documentation, compatibility and deployment-profile validators passed.

This completes adapter capability implementation. Server backend selection,
maintenance startup progress, CLI/container/offline/SDK and resource/soak
acceptance remain in progress; this entry does not claim finished delivery.


## 2026-10-10 — Native backend selection and shared server composition

The composition root now selects storage.backend (mongodb by default, postgres
explicitly). PostgreSQL resolves only its selected URI/migration credentials,
validates the same migration target, ensures the schema and constructs the complete
StorageBundle<PostgresEventStore>. It reuses the existing generic service assembly,
EventWriter, dispatcher, scheduler, signal writers, identity and HTTP services.
No EventStore::Prepared/port change, Mongo factory change or implicit fallback is
introduced. The no-Mongo-URI legacy mode and original shipped profiles remain.
Archive configuration accepts the selected PostgreSQL URI instead of requiring
an unused Mongo URI.

Typed PostgreSQL TOML covers native TCP, verified/acknowledged weaker TLS, CA path,
pool bounds, bootstrap/acquire/statement/lock/idle deadlines, migration auto/check
mode and separate credentials. URI policy rejects unknown/conflicting options
before driver parsing. Only selected database secret references are validated and
resolved; shared scrub HMAC remains required. Effective config and Debug redact
both URIs and CA locations. Existing approved driver environment inputs remain
operator-controlled; their values are not logged.

The native schema manager publishes a driver-free bounded phase/step report under
its existing dedicated ownership. It uses SQLx transactional apply/ledger for each
validated immutable SQL step and reports completion only after SQL/data verification
and generation publication. It retains checkpointed Rust data-step recovery.
Waiting ownership is cancellable and performs no schema mutation. Server startup
maps that report into the existing maintenance listener; unavailable/timeout/busy
attempts retry with 250ms-to-5s capped delay. Permanent incompatibility/permission/
invalid-data categories exit redacted. SQL syntax/data migration faults are not
classified as indefinite temporary startup errors. Runtime workers start only
after verification. Shutdown cancels ownership/retry waits.

Six new ordinary config tests cover native selection without Mongo, inactive secret
references, URI/HMAC requirements, weak-mode acknowledgement/bounds/unknown keys,
redaction/separate target and archive enablement. The first draft fixtures used
unsupported bare literal strings; they were corrected to the existing shared
{ literal = value } representation. No shared secret contract was expanded.

Gates: strict PostgreSQL/server all-target/all-feature Clippy --locked -j1
-D warnings passed. All server library tests passed 69/69, original CLI tests
passed 7/7 (including all production Mongo profiles). Real Docker migration tests
passed 12/12 in 11.34 seconds, including monotonic progress, no Complete on failed
DDL, cancellable ownership and the existing checkpoint/upgrade suite.

The new acceptance coordinator is being developed separately. Its current actual
server case passed 10 checks: CLI selected secrets/redaction, ownership maintenance
(live 200, readiness/routes 503, schema untouched), auth/bootstrap, native Event
write/scrub/finalization, duplicate counter preservation, scope/anonymous denial,
query and same-DB process restart. The explicit ignored postgres_runtime_e2e target
passed 1/1 in 9.55 seconds. Real Node/Python/Java/.NET/Go/Rust sender fixtures also
flushed and finalized through the PostgreSQL-composed server; their records were
read back through the authenticated API. Printed SDK UUIDs are normalized by the
test coordinator to the native API's hex32 representation. These checks use newly
generated synthetic DBs and Blob directories and clean them after joined workers;
the fixture DB listing is empty. No Mongo source edit is included.

Container/offline/browser SDK/UI, CI registration and 30-minute resource/load
acceptance remain pending. The coordinator/Docker fixtures are not yet advertised
as completed by this server composition stage.


## 2026-10-10 — Composed runtime, SDK, container and offline acceptance

Added the fail-closed standard-library coordinator `scripts/test-postgres.py` and
an explicit ignored `postgres_runtime_e2e` group. Docker availability and healthy
native fixtures are required; Cargo results must contain nonzero executed cases.
Each native invocation creates a UUID-named synthetic database and Blob directory,
joins its own workers and cleans only those exact resources. Container runs use
UUID-named Compose projects; volume labels and name ownership are verified before
project-scoped deletion. No user database, installation volume or global Docker
prune is involved. Captured server/sender diagnostics, tokens and DSNs are withheld;
resource reports contain only bounded synthetic counters and resource metadata.

The authenticated status response now uses the selected database name. A private
composition-root Extension preserves existing HTTP constructors and Mongo's
`mongodb` response key. Two explanatory UI strings now say database rather than
MongoDB. The dependency graph explicitly permits the server's PostgreSQL edge.
There are no new Mongo source/schema/migration changes in this stage.

The complete real native adapter suite passed 173/173, including local-CA TLS,
wrong CA/hostname, driver-environment policy, pool/deadlines, immutable SQL/Rust
upgrades, COMMIT reply faults, scope/privacy, all current capabilities and project
purge. Source inventory remains 164/164 with --require-complete. Workspace strict
all-target/all-feature Clippy --locked -j1 -D warnings, rustfmt, dependency graph,
Web changed-file formatting and documentation/deployment/compatibility validators
passed. Existing ProjectSettingsView tests passed 2/2.

Actual native and Docker-composed servers both passed their core checks and all
seven real SDKs: Node, Python, Java, .NET, Go, Rust and Browser. The reused Node
fixtures additionally prove attachment metadata and exact Blob download bytes,
Session health, cron run history, Metric buckets, Logs and Traces. Reused Browser
fixtures prove Feedback metadata/attachment bytes and Replay metadata, masked
recording retrieval and rrweb playback. Real Web UI login, project creation and
privacy-policy mutation persist through the selected PostgreSQL backend.

Test-only assumptions corrected during expansion: Sessions need an explicitly
registered release before the management API exposes its health; the installed
Node SDK sends three unique Session identities (an implicit exited Session plus
the explicit healthy lifecycle and crash). A separate synthetic HTTP collector
confirmed those identities; persisted Session rows and health counters agree.
The SDK's `ok` cron status is exposed as the domain API's `success`. Replay must
be explicitly enabled on the synthetic project. These are fixture corrections,
not adapter or shared production behavior changes. The existing login policy
revokes this user's previous sessions, so the coordinator reauthenticates after
its independent browser login.

The current production Dockerfile was reused in full, with only the generated
ignored test copy serializing Cargo with -j1. Current image manifest:
`sha256:8d706bd780145191e1f8760710517f96489a2a5683aad2ae036d9e284729906f`.
The container suite passed 12 checks, including authenticated ingest/readback,
native DB restart, same-volume app restart and graceful SIGTERM exit 0 (4.171s
in the full-SDK run). The offline suite passed 24 checks: 12 each for explicitly
acknowledged plaintext TCP and verify-full TLS with a local synthetic CA. All app
networks were inspected as internal-only; neither runtime transport requires
external HTTP, paid certificate services or Mongo. On Docker Desktop, offline API
probes run through container loopback because published host ports are unavailable
on this internal network. App restart targets the exact container, avoiding a
Compose dependency restart that would regenerate the ephemeral CA during the test.

Developer fixtures are `deploy/metric.postgres.test.toml`, the app profile in
`deploy/compose.postgres.dev.yml` and `deploy/compose.postgres.offline.yml`.
The developer app selects the fixture's existing `metric_test` database; coordinator
runs override this with a fresh project-owned `metric_app_test` database. Production
Compose, installers, bundled profile defaults and production Dockerfile remain
unchanged. CI now has separate bounded native adapter/runtime/all-SDK and
container/offline jobs, including the source completeness gate. They are registered
but have not been executed by GitHub Actions in this local unpushed branch.

Reproduce the completed gates:

```powershell
python scripts/test-postgres.py --suite adapter
cargo test -p metric-server --test postgres_runtime_e2e --all-features --locked -j1 -- --ignored --test-threads=1
python scripts/test-postgres.py --suite server --binary target/debug/metric-server.exe --sdk --browser
python scripts/test-postgres.py --suite container --build-image
python scripts/test-postgres.py --suite container --sdk --browser
python scripts/test-postgres.py --suite offline
```

The coordinator requires existing SDK/browser installations for --sdk/--browser;
it does not silently skip missing senders. The native adapter suite also requires
four local test certificate files under target/postgres-test-tls. CI prepares the
same existing fixtures and copies their certificates explicitly.

A short load diagnostic passed (136 unique Events, 17 duplicate requests, concurrent
Explore, accepted-backlog drain, exact Issue counts and Scheduler expiry under
write load). Its 10-second target and 37.171-second total do not satisfy the required
30-minute acceptance gate. Full workspace/Mongo regression and measured 30-minute
load acceptance remain pending; this entry does not declare delivery complete.


## 2026-10-10 — Acceptance pause: permanent connection errors during startup

The ordinary all-target/all-feature locked workspace run completed: 363 passed in
48 test groups, 264 intentionally ignored cases. PostgreSQL's 173 ignored native
adapter cases were executed separately above. Mongo's final ignored Docker
regression and the 30-minute measured load gate have not yet run.

Read-through of ADR-0052's bounded wrong-certificate error requirement found a
startup discrepancy. `PostgresError::from(sqlx::Error)` currently maps TLS errors
and authentication SQLSTATEs into Unavailable. The composition root retries
Unavailable/Timeout/Busy indefinitely while in maintenance. A real composed-server
reproduction against the local TLS fixture with wrong-ca.crt confirmed six startup
retries in 10 seconds, a still-running process and closed readiness. The connection
continues to reject the certificate; no downgrade, accepted Event or schema write
occurred. The synthetic reproduction was stopped and its own directory removed.

Implementation is paused under the user's instruction to stop and ask when a
confirmed discrepancy is found. Proposed correction, not yet implemented: keep
redacted permanent native connection categories inside crates/postgres for verified
TLS/configuration failures, invalid credentials (SQLSTATE class 28) and missing
configured database (3D000). Those categories must exit the existing server startup
loop through its permanent-error branch. Retain bounded cancellable maintenance
retry for actual temporary network outages and migration ownership contention.
Add real adapter and composed-server regressions proving wrong CA/hostname/auth
fail without repeated retries and preserve plaintext acknowledgement, valid local
CA startup and temporary availability recovery. No Mongo edit, port change, TLS
fallback or confidential driver-message logging is proposed. Resume the remaining
Mongo/30-minute gates only after the user resolves this pause.

## 2026-10-10 — Approved permanent TLS startup errors

The user approved immediate startup failure when an explicitly protected native
connection cannot negotiate TLS or verify its certificate. Implementation resumed
with this narrower TLS/configuration correction; the proposed authentication and
missing-database classification changes above are not included.

`crates/postgres` now exposes a redacted TlsFailure category. SQLx's explicit TLS
errors and IO errors carrying a concrete rustls::Error map to it; ordinary network
IO errors remain temporary. Error-message matching is deliberately avoided.
Malformed driver configuration maps to InvalidPolicy. The existing server startup
loop already exits for permanent categories, so neither server startup logic nor
Mongo needs a production edit. No plaintext fallback is introduced.

Seven ordinary PostgreSQL tests and all 174 real Docker adapter cases passed.
The additional case requires TLS against a plaintext server, checks both runtime
connection and migration entry points, and proves failed negotiation performs no
schema mutation. Existing wrong-CA and wrong-hostname cases now assert the permanent
category. Composed-server exit checks and final acceptance gates are in progress.

The updated real-server integration group passed 2/2. Its protected-startup case
executes five actual process runs: wrong CA, wrong hostname, server without TLS,
malformed CA PEM and malformed inherited client private key. Each exits nonzero
within 15 seconds, exposes only its permanent boundary category, performs no
startup retry, and excludes credentials and certificate/key paths from diagnostics.
The original native-server auth/ingest/scope/restart case also passes.

TLS correction commit: `e5fee13`. Acceptance coordinator/fixtures/CI commit:
`ea1b22a`. Strict workspace all-target/all-feature Clippy, rustfmt and dependency
graph passed after the correction. The locked ordinary workspace run passed
364 tests in 48 groups; 266 intentionally ignored cases are accounted for by
separate real-DB groups. The original Mongo Docker regression passed 30 adapter
tests in 19 groups and 13 server cases in nine groups, using its existing URI and
test targets (performance/reporting cases excluded as in the existing CI policy).
No additional Mongo file was changed. Coverage, compatibility, deployment-profile
and documentation validators passed. Updated-image and measured load gates remain
in progress.

## 2026-10-10 — Updated-image acceptance pause: Go SDK readback

The release Docker rebuild succeeded with the pinned Rust compiler in 9m40s.
Updated image manifest list:
`sha256:32514fd19368623ec3346cd3130e4a755b7f616bac98c06c8c3f546644c63111`.
The subsequent full-SDK container gate failed at Go Event readback:
`go did not finalize before deadline`. The real Go sender returned flushed=true;
the coordinator could not observe its Event through the authenticated API within
the configured gate deadline. Node, Python, Java and .NET Event readback preceded
this failure successfully. Flush success alone does not prove server acceptance
or durable finalization. Neither an adapter defect nor a fixture defect has been
confirmed. The earlier full-SDK container/native passes above used the previous
image/source revision and do not replace this failed updated-image gate.

The coordinator removed only its exact synthetic Compose project and verified
disposable volumes. The shell stopped on the failed gate, so updated-image offline
and native full-SDK reruns did not execute. The actual 30-minute load gate is still
pending. Implementation is paused under the user's explicit stop-on-discrepancy
instruction; an isolated Go transport/finalization diagnostic was proposed before
any new fix. The separately verified permanent TLS correction remains committed.

## 2026-10-10 — Upstream PHP compatibility fix integrated

The user identified an already merged upstream correction rather than authorizing
a duplicate implementation. Fetching origin located PR #11, merged as `4485ebc`,
with implementation commit `7317c5c` (`fix: improve PHP and browser SDK compatibility`).
It was merged into feat/postgres as `9509117` without conflicts. All existing
uncommitted PostgreSQL documentation and Ruby/PHP harness work was restored and
verified against the temporary Git snapshot before removing that snapshot.

The upstream shared ingest accepts Event IDs from the body, item header or envelope
header, injects the canonical ID into the stored body and still rejects missing,
malformed or conflicting IDs. The same PR includes bounded indexed span-name
normalization, compatibility documentation and browser setup/detail corrections.
Neither database adapter was changed by this merge.

The real PHP 8.3.33 / sentry/sentry 4.34.0 process gate now passes, including accepted
Event ID, exception, release/environment, SDK metadata and absence of unexpected
blobs. This gate uses the actual public HTTP router with the test sink, not the
PostgreSQL database. The equivalent Ruby 3.1.7 / sentry-ruby 7.1.0 process gate also
passes after correcting the new sender to read event.event_id from the Event returned
by capture_exception. Expanded harnesses remain work in progress; CI preparation,
the evidence matrix and actual PostgreSQL readback must be completed before making
new release or database compatibility claims.

All application tests passed on a serial rerun: 139 passed, 11 intentionally ignored.
The first parallel run had one timing-sensitive EventWriter cancellation-test
failure at its fixed 40ms wait; that test passed in isolation and in the complete
serial rerun without a writer code change. The five new upstream ingest regressions
passed in both runs. The two changed browser test files passed all 11 tests.
Rustfmt and git diff --check passed. No new PostgreSQL image has been built after
this merge, and the expanded Docker/offline and measured 30-minute gates remain
pending. This entry does not declare PostgreSQL delivery complete.

## 2026-10-10 — Expanded functional parity; load testing deferred

The user explicitly deferred load testing and requested broad ordinary functional
coverage of both backends. No soak or performance gate was run for this request.
The current real PostgreSQL adapter suite passed all 174 cases. The independent
Mongo fixture passed 30 ordinary adapter cases in 19 groups, excluding performance
and RPS reporting cases. Logs are in the ignored target directory:
`postgres-comprehensive-adapter.log` and `mongo-comprehensive-adapter.log`.

The native PostgreSQL server passed all 16 real SDK paths across nine SDK families,
including Ruby/PHP errors, Node attachments/sessions/cron/metrics/logs/traces and
browser error/feedback/replay recording and playback. The same run passed six
management lifecycle groups: API token creation/scope/revocation; DSN key
creation/use/disabling; Issue resolution/idempotency/reopening; saved-query CRUD
with scope/revision checks; dashboard CRUD with scope/revision checks; project
deletion request/cancellation. It verified scrubbing, duplicate finalization,
authentication/scope isolation and authoritative Event readback after process
replacement. Five permanent TLS startup rejection cases also passed. These native
server results do not replace the pending updated shipping-image gates.

The full Mongo SDK run stopped at cron. An isolated diagnostic using three actual
Node SDK check-in payloads confirmed a deterministic ordering defect without any
adapter modification. Sending one envelope with `[ok, in_progress, error]` returns
HTTP 503 / `signal_storage_unavailable` and creates no monitors; sending the same
payloads as `[in_progress, ok, error]` returns HTTP 200, with one success run and
one error run readable through the API. Delaying only the terminal request in an
independent transport diagnostic also makes all three check-ins succeed.

`MongoMonitorStore::persist_inner` resolves definitions and checks each run's
monitor existence in the same loop, so a terminal check-in preceding its defining
update rejects the whole micro-batch before the later definition is considered.
PostgreSQL already resolves all definitions before processing runs. A proposed
minimal Mongo correction is a separate existence-validation pass after all batch
definitions have been resolved, with a shared regression for reversed definition
ordering. Production changes are paused pending the user's explicit approval for
this additional Mongo exception. Diagnostic evidence:
`target/mongo-cron-batch-diagnostic.log` and
`target/mongo-cron-ordered-diagnostic.log`.

The isolated successful Mongo ordering run also passed the six management groups
and restart readback, but is not a substitute for the failed unmodified full SDK
gate. Later Mongo SDK scenarios, the new runtime physical project-purge gate for
both backends, and the final complete regression remain pending. A concurrent
server-test compilation was cancelled before tests started to free memory; it
must be rerun serially and is not counted as a test failure or a successful gate.

## 2026-10-10 — Approved Mongo cron correction and functional rerun

The user approved the additional minimal Mongo exception after reviewing the
ordering defect and the limits of the proposed correction. The production change
adds a separate monitor-existence validation loop after all in-batch definitions
are merged. No additional database read, migration, retry policy, public contract
or run-order change is introduced. PostgreSQL production code is unchanged.

The new shared `definition_order_in_batch` conformance test failed on the original
Mongo code with `NotFound` and passed after the correction. It checks both batch
orders, terminal run/projection preservation after duplicate starts, and rejection
when a definition is absent from both the database and batch. Mongo's complete
ordinary adapter rerun passed 31 cases in 19 groups; explicit performance and RPS
cases were excluded. Evidence: `target/mongo-cron-regression-before.log`,
`target/mongo-cron-regression-after.log`, `target/mongo-comprehensive-adapter.log`.

The actual Mongo-backed executable passed all 16 SDK paths across nine families,
all six common HTTP CRUD groups and authoritative Event readback after process
replacement. The real deletion worker completed its normal lifecycle with a
three-second synthetic grace. All 11 previously populated families had zero
remaining project-owned documents: 12 errors, 8 issues, 3 logs, 6 spans, 9 sessions,
1 feedback, 2 monitors, 2 runs, 3 metric buckets, 1 replay and 2 DSN keys were
present before deletion. The old DSN rejected subsequent ingest. Evidence:
`target/mongo-comprehensive-sdk-http.log`. This is not a physical-blob immediate
erasure claim; existing reference GC retains its normal grace and retry lifecycle.

Ruby/PHP sender dependencies are locked and fixture-local. The Go sender now joins
the official SDK telemetry worker with `Client.Close()` after `Flush()` before its
short-lived process exits. Prior isolated diagnosis found 10 lost deliveries in
50 original sender executions and zero in 50 with the worker joined; a reported
Flush success alone was insufficient while worker requests were still in flight.
This changes the test sender only, without a SDK fork or application adapter fix.
The successful native Mongo and PostgreSQL SDK runs use that corrected sender.

Final fresh PostgreSQL, server/router SDK and ordinary workspace gates are still
in progress. The detailed record/lifecycle matrix is in
`0052-postgresql-functional-parity.md`. Load remains deferred by the user.

The subsequent fresh PostgreSQL adapter run passed 175 cases, including the new
shared cron test. All 16 native SDK paths and six HTTP CRUD groups passed, as did
the real-worker physical purge of the same 11 families with identical populated
counts to Mongo and zero remaining records. Five permanent TLS startup rejection
cases passed with regenerated two-day local fixture certificates. The dedicated
PostgreSQL runtime Cargo group passed both tests. The ordinary locked workspace
run passed 369 tests in 48 groups, with 270 intentionally ignored cases accounted
for by explicit real-DB/SDK gates or deferred performance scope. Strict all-target,
all-feature Clippy, rustfmt, dependency direction, coverage, compatibility,
deployment-profile and documentation validators passed. Correction commit:
`5adfbbf`; expanded SDK/functional coordinator and CI commit: `3c2aa93`.

The current-source release Docker image is being rebuilt for the remaining
ordinary container restart/shutdown and offline TCP/TLS gates. Those are functional
checks, not a capacity or load measurement; their results remain pending until
the new image passes. No load test is scheduled or run for this request.

The subsequent current-source image build succeeded: release compilation took
10m28s with pinned Rust 1.94.0 and serialized Cargo. Image manifest list:
`sha256:1090447f52eff0d724f2307bbf059c7eed68e6d1a8676d8de9d799ddedf5f68f`;
image config: `sha256:82b154878d7cbacb80af5ceb408679af0ebfb2750ad529cc8b741f3bef08321d`.
The new image's full SDK Compose gate passed all 16 paths, six shared CRUD groups,
all 12 baseline container assertions, native database restart, authoritative Event
readback after application restart and clean SIGTERM shutdown (exit zero, 4.156s).
Its independent Compose runtime has no Mongo service. This successful current-image
gate supersedes the earlier Go readback failure recorded above, with the official
SDK worker joined before its sender exits. Evidence:
`target/postgres-comprehensive-container.log`. Offline TCP passed; the separate
internal-only verified TLS gate is still finishing.

Both updated-image offline gates then passed: 24 total baseline assertions across
acknowledged plain TCP and verify-full TLS with a generated local CA, in Docker
networks explicitly verified as internal/outbound-blocked. Each includes native
database restart, exact acknowledged Event readback after app restart, six common
CRUD groups and graceful SIGTERM shutdown (0.797s TCP, 0.828s TLS, exit zero).
These probes execute curl on container loopback; database communication remains
native SQL over the isolated network. No HTTP/REST database gateway, remote CA
download or outbound integration is required during runtime. Evidence:
`target/postgres-comprehensive-offline.log`.

The current functional verification request is complete: all executed adapter,
native HTTP/SDK, original Mongo server, PostgreSQL runtime, ordinary workspace,
strict lint/structure/docs and updated Docker/offline gates passed. Load remains
explicitly deferred, with no capacity or 30-minute stability claim. The accepted
late-Metrics archival loss window and missing-definition-across-separate-batches
cron boundary remain documented. Default downloaded profiles/installers still
select Mongo. Expanded test/coordinator/SDK evidence is committed as `3c2aa93`.
No branch is pushed, no PR is created and no production deployment is performed.
