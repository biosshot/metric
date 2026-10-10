# PostgreSQL / Mongo functional parity — 2026-10-10

Scope: ordinary functional checks against independent real databases in Docker.
The user deferred load testing; no soak, throughput or performance gate is required
for this run. Backends run sequentially with synthetic namespaces and credentials.
The shipped default remains MongoDB. No data import or dual writing is exercised.

## Evidence layers

- Adapter gates use the public storage ports with real PostgreSQL or MongoDB.
- Shared service/unit gates exercise normalization, authorization, batching,
  finalization, recovery and domain constraints independently of storage selection.
- The common native-server gate starts the actual executable against either real
  backend, uses authenticated HTTP, sends actual SDK payloads, restarts the process
  and verifies physical project-owned row/document deletion.
- Real SDK router gates additionally inspect normalized content and metadata using
  controlled sinks. They do not independently prove database durability.

Counts below are test cases or scenario groups, not the number of HTTP requests.
A passing port-signature coverage validator is structural evidence; it does not
replace the behavioral checks in this report.

## Record and lifecycle coverage

| Family | Basic operations and boundaries | PostgreSQL adapter evidence | Mongo adapter evidence |
| --- | --- | --- | --- |
| Organizations, projects, DSN keys | Creation, identity uniqueness, lookup, organization/project scope, key use/disable, deletion fencing | `crates/postgres/src/projects.rs` tests, `deletion/tests.rs` | `crates/mongo/tests/project_identity.rs`, `project_deletion.rs` |
| Users, memberships, setup tokens | Bootstrap, credentials, enabled owner counting, membership/tenant authorization, disabled identity | `crates/postgres/src/auth.rs` tests | `crates/mongo/tests/auth_store.rs` |
| Login sessions, API tokens, audit | Session access/expiry, token scopes/revocation, audit append/read; token lists redact secrets | Auth adapter gates plus common HTTP token CRUD | Auth adapter gates, `crates/server/tests/auth_e2e.rs`, common HTTP token CRUD |
| Error events | Insert/read, duplicates, durable processing state, retry identity, bounds/corruption, project fence | `crates/postgres/src/events/tests.rs` | `crates/mongo/tests/event_store.rs` |
| Issues, activities, statistics | Grouping, atomic occurrence counts, resolve/reopen, command idempotency, search/pagination/scope | `issues/tests.rs`, `api/tests.rs`, `finalization/tests.rs` | `issue_store.rs`, `finalizer_store.rs`, `crates/server/tests/native_api_e2e.rs` |
| Finalization and dispatcher recovery | State transitions, restart/retry, derived catalogs/counters once, fenced projects | Finalization/events gates and native server restart | `finalizer_store.rs`, `crates/server/tests/durable_ingest_e2e.rs` |
| Logs, spans, trace statistics | Durable ingest/read, bounded queries, retention, hourly rebuild including intersected hours | `signals/tests.rs`, `explore/tests.rs` | `signals.rs`, `explore_store.rs` |
| Metric buckets | Aggregation, cardinality, retention/archive holds, project scope | `metrics/tests.rs`, `archive/tests.rs` | `metrics.rs`, `metric_archive.rs` |
| Sessions and release health | Lifecycle merge, deduplication, health totals, expiry/repair, scope | `sessions/tests.rs` | `sessions.rs` |
| Cron and uptime monitors | Definitions/runs, terminal merge, timeout/missed checks, leases, retention, same-batch reversed definition order | `monitors/tests.rs`; shared `monitor_conformance.rs` | `monitors.rs`; the same shared conformance checks |
| Feedback and attachments | Metadata workflow, content reference/readback, private scope, cleanup | `feedback/tests.rs`; existing services with PostgreSQL | `feedback.rs`; native SDK attachment/feedback readback |
| Replay recordings | Ordered segments, duplicates, metadata/scope, retention/archive references; real browser privacy and playback | `replays/tests.rs`; common browser SDK path | `replays.rs`; the same browser SDK path |
| Saved queries and dashboards | Create/read/update/delete, optimistic revisions, name uniqueness, project scope | `dashboards/tests.rs`; common HTTP CRUD | `dashboards.rs`; common HTTP CRUD |
| Releases, deployments, environments | Organization-shared identities, project associations, idempotent deploy, health and cleanup | `releases/tests.rs`, `finalization/tests.rs` | `finalizer_store.rs`, `crates/server/tests/releases_e2e.rs` |
| Notification rules, recipients and deliveries | Durable aggregate datasets, claiming/completion, duplicate immutability, history, project cleanup | `notifications/tests.rs`; existing dispatcher/service checks | `notifications.rs`, `crates/server/tests/notifications_e2e.rs` |
| Debug files and artifact/source bundles | Upload/publication, bindings, private lookup, quotas, recovery, logical deletion/reference GC | `debug_files/tests.rs`, `artifacts/tests.rs` | `maintenance_store.rs`, `crates/server/tests/debug_files_e2e.rs` |
| Archives and blob references | Claim/publication/manifest completion, actual Parquet families, crash/retry, hold-before-hot-expiry, reference cleanup | `archive/tests.rs`, `maintenance/tests.rs` | `archive_store.rs`, `metric_archive.rs`, `crates/server/tests/archive_e2e.rs` |
| Project deletion | Request/cancel, fencing, bounded purge, restart/rescan, tombstone, shared data preservation | `deletion/tests.rs`; common HTTP physical purge | `project_deletion.rs`; common HTTP physical purge |
| Maintenance and schema | Expiry/repair/counters, migrations/ownership, schema generation, bounded batches | `maintenance/tests.rs`, `crates/postgres/src/migrations.rs` tests | `crates/mongo/src/migrations.rs` tests, `maintenance_store.rs` |

Paths abbreviated after the first entries refer to their adapter's existing
`src` submodule (PostgreSQL) or `tests` directory (Mongo); gate logs contain the
actual case names. Retained audit and organization-shared datasets follow their
existing ownership rules. Revoking an API token does not mean deleting a project
must erase the global token or its audit record.

## Common native HTTP gate

`scripts/test-postgres.py --suite server --backend <postgres|mongodb> --sdk
--browser --verify-purge` uses the same service/API assertions for both backends.
It checks bootstrap/login, the selected dependency status, browser login and
project policy mutation, accepted error readback and scrubbing, duplicate
finalization, anonymous/foreign-project rejection, token/key lifecycle, issue
commands, saved-query/dashboard CRUD and revisions, project deletion cancellation,
and readback after actual process replacement.

Nine SDK families cover 16 paths: Node, Python, Java, .NET, Go, Rust, Ruby, PHP and
browser errors; Node attachment/session/cron/metric/log/trace signals; browser
feedback and replay recording/playback. Not every signal is supported or tested
by every language SDK; Ruby and PHP coverage here is Error Events.

After SDK readback and restart, a three-second synthetic deletion grace permits
the normal purge worker to complete. The gate verifies that all 11 populated
project-owned families have zero remaining rows/documents: errors, issues, logs,
spans, sessions, feedback, monitors, monitor runs, metric buckets, replays and DSN
keys. The old DSN must then reject new ingest. This is a metadata/data purge check;
unreferenced blob bytes are reclaimed by the existing GC lifecycle, not necessarily
at the instant the deletion job becomes complete.

## Current results

The shared cron regression failed on the old Mongo implementation with `NotFound`
and passes for both real adapters after the approved three-line Mongo correction,
committed as `5adfbbf`. No PostgreSQL production change was needed.

| Gate | Fresh result | Local evidence in ignored `target/` |
| --- | --- | --- |
| PostgreSQL real adapter suite | 175 passed | `postgres-comprehensive-adapter.log` |
| Mongo real adapter suite, excluding explicit performance/RPS cases | 31 passed in 19 groups | `mongo-comprehensive-adapter.log` |
| Original Mongo server integration targets | 13 passed in nine groups | `mongo-and-sdk-comprehensive-server.log` |
| Real SDK router/content gates, including Ruby/PHP and browser replay | 16 passed | Same server log, separate SDK group |
| Actual native Mongo executable, SDK/CRUD/restart/purge | All 16 SDK paths and six CRUD groups passed; all 11 populated families purged | `mongo-comprehensive-sdk-http.log` |
| Actual native PostgreSQL executable, SDK/CRUD/restart/purge | The same 16 paths and six groups passed; all 11 populated families purged | `postgres-comprehensive-sdk-http.log` |
| Protected PostgreSQL startup failures | Five bounded permanent-error cases passed | `postgres-comprehensive-startup.log` |
| Dedicated PostgreSQL runtime group | Two passed | `postgres-comprehensive-runtime.log` |
| Ordinary workspace, all targets/features, serialized | 369 passed in 48 groups; 270 intentionally ignored | `storage-functional-workspace.log` |
| Strict workspace Clippy, rustfmt, dependency graph | Passed | `storage-functional-clippy.log`; validator output |
| Current-source release Docker app with all real SDKs, DB/app restart and SIGTERM | 12 baseline checks and all 16 SDK paths passed; clean shutdown in 4.156s | `postgres-comprehensive-container.log` |
| Outbound-blocked internal Docker network, native TCP and verified local-CA TLS | 24 checks passed across both transports, including DB/app restart and clean shutdown | `postgres-comprehensive-offline.log` |

Both native purge gates had identical populated family counts before deletion:
12 errors, 8 issues, 3 logs, 6 spans, 9 sessions, 1 feedback, 2 monitors, 2 runs,
3 metric buckets, 1 replay and 2 DSN keys. Every count was zero after the worker
completed; subsequent ingest with the old DSN was rejected. Mongo schema generation
remains 22 and PostgreSQL generation remains 21. Default configuration/profile,
documentation, compatibility and 164/164 explicit-port coverage validators pass.
The ignored real-database/runtime/SDK cases are separately executed as described
above; the ordinary workspace count does not silently include them. Explicit
performance/soak cases remain deferred.

The current-source Docker image was built with the shipped stages and a serialized
release Cargo invocation, using Rust 1.94.0. Release compilation completed in
10m28s. Image manifest list:
`sha256:1090447f52eff0d724f2307bbf059c7eed68e6d1a8676d8de9d799ddedf5f68f`.
The updated image includes the upstream PHP/browser correction and the approved
Mongo cron change. Its independent PostgreSQL Compose runtime contains no Mongo
service. The old-image Go readback failure is superseded by the successful full
SDK current-image gate, with the corrected sender shutdown lifecycle.

CI retains the real adapter and original server gates. PostgreSQL's full SDK
native gate now requires physical purge. Mongo adds the same native HTTP/CRUD
and physical purge gate without SDK dependencies; its full 16-path SDK/database
readback has been verified locally. The independent router SDK CI gate includes
the newly pinned Ruby/PHP fixtures. GitHub Actions itself has not been run locally.

## Explicit limits

- Load and the previously discussed 30-minute run are deferred by the user.
- The accepted metric archive snapshot/late-write loss window remains documented
  in the main phase report and is not changed by these tests.
- A terminal cron update for an unknown monitor in a separate batch with no
  available definition is still rejected. The approved fix concerns definitions
  already present elsewhere in the same batch.
- The updated release image and offline transports passed as recorded above.
  Publishing, production deployment, and automatic PostgreSQL installer profiles
  are not performed by this functional verification request; Mongo remains default.
- This finite suite covers the documented basic operations and boundaries, not
  every possible ordering, deployment failure or SDK feature combination.

## Completion and cleanup

All gates executed for this functional request passed. No new failing scenario
remains after the approved cron correction. Report/source/fixture commits stay on
`feat/postgres`; nothing is pushed or published. Temporary UUID Compose projects
verify volume ownership and remove only their own disposable data. The main
task-owned developer fixtures are stopped with exact project/file arguments while
their persistent volumes are retained. Unrelated Docker projects are untouched.
