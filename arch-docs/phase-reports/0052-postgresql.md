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
