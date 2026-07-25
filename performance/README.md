# Performance regression artifacts

Phase 17 retains the real `sentry-cli` upload, private debug index hit/miss RPS,
and open-circuit backend-failure RPS in `baselines/debug-files/`. Compare a newly
captured JSON result with:

```text
node performance/compare-debug-files.mjs <baseline.json> <candidate.json>
```

The comparator rejects a regression greater than 20% in any retained RPS metric.
The short local profile is a regression signal, not a server-capacity claim.
Capture at most one candidate per pass by setting `METRIC_PHASE17_PERF=1` before
running the ignored `debug_files_e2e` test; without that opt-in the compatibility
and recovery assertions run without a load profile.

`k6/ingest-fake.js` measures the Phase 1 black-box path through HTTP, bounded body
decode, Envelope/auth parsing, mandatory scrubbing, and the deterministic benchmark
`EventSink`. It does not measure MongoDB durability and must not be quoted as the
ADR-0037 end-to-end capacity result.

Build and start the target:

```text
cargo build --release --bin ingest-bench
target/release/ingest-bench
```

Run k6 from the repository root:

```text
k6 run -e METRIC_RPS=2500 -e METRIC_DURATION=15s -e METRIC_RESULT=performance/results/ingest-fake-2500.json performance/k6/ingest-fake.js
```

Every run writes a JSON artifact under `performance/results`. Set commit, toolchain,
k6 version and hardware through `METRIC_COMMIT`, `METRIC_RUST`,
`METRIC_K6`, and `METRIC_HARDWARE`. Reviewed reference results are copied to
`performance/baselines/ingest-fake`; baseline changes require an explanation in the
Phase report. Short local runs find regressions, while the 5,000/s steady and
20,000/s burst gates run on controlled hardware.

Compare a candidate with a like-for-like baseline (same fixture, mode and target
RPS). The command exits non-zero on dropped iterations, a higher error rate, or an
RPS/p95 regression greater than the percentage budget:

```text
node performance/compare-k6.mjs performance/baselines/ingest-fake/ryzen-5600h-windows-k6-v1.json performance/results/ingest-fake-2500.json 10
```

For a saturation probe, set `METRIC_MODE=max-throughput` and
`METRIC_VUS=64`. Saturation results document a limit; they do not replace the
fixed-arrival-rate regression baseline.

## Phase 2 project resolver

The retained Phase 2 runner measures two deliberately separate paths: a warm
application authorization-cache hit and a direct MongoDB lookup that bypasses that
cache. Start `mongo:8.0.12` from `deploy/compose.dev.yml`, then write a candidate:

```text
node performance/run-project-resolver.mjs
```

The runner prints the ignored release-test output into a timestamped JSON file under
`performance/results`. Compare it with the reviewed baseline using the same hardware
and MongoDB topology:

```text
node performance/compare-project-resolver.mjs performance/baselines/project-resolver/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 10
```

The comparator fails when warm-cache RPS drops below 20,000 or when warm-cache/direct
MongoDB RPS or latency regresses beyond the supplied percentage budget. k6 remains
the black-box HTTP load tool; Phase 2 uses the in-process runner so resolver RPS is
not conflated with Envelope parsing or a fake/durable Event sink.

## Phase 3 durable writer and HTTP path

The module runner writes a comparable real-MongoDB artifact containing RPS, batch
occupancy, p95/p99, duplicate retries, and acknowledged-loss count:

```text
node performance/run-mongo-writer.mjs
node performance/compare-mongo-writer.mjs performance/baselines/mongo-writer/<baseline>.json performance/results/<candidate>.json 10
```

For the black-box durable path, build and start `durable-ingest-bench` with fresh,
explicit benchmark database settings, then run the retained fixed-arrival k6 case:

```text
cargo build --locked --release --bin durable-ingest-bench
METRIC_BENCH_MONGODB_URI=<uri> METRIC_BENCH_DATABASE=<fresh-db> target/release/durable-ingest-bench
k6 run -e METRIC_RPS=5000 -e METRIC_DURATION=15s -e METRIC_RESULT=performance/results/ingest-mongodb-5000.json performance/k6/ingest-mongodb.js
```

`ingest-mongodb.js` uses a unique Event ID per iteration. Compare the k6 iteration
count with the fresh database's Event count before removing it; equality proves
zero acknowledged loss for that run. Store reviewed JSON under
`performance/baselines/ingest-mongodb`. The 20,000/s bounded burst remains a
separate controlled-hardware capacity gate and must not be inferred from a lower
local rate.

The durable artifact separates transport and overload failures into TCP errors,
total HTTP responses, and explicit `200`, `429`, `503`, and other-status counters.
This keeps listener saturation distinct from intentional application backpressure.

## Phase 4 Dispatcher refill

The Dispatcher runner preloads 20,000 compact pending Events into MongoDB 8.0.12,
then measures the indexed `q.n,r,_id` query plus strict body decode and project
fence. Its minimum recovery gate is 7,500 Events/s, 1.5 times the 5,000/s steady
acceptance target:

```text
node performance/run-dispatcher-refill.mjs
node performance/compare-dispatcher-refill.mjs performance/baselines/dispatcher-refill/<baseline>.json performance/results/<candidate>.json 10
```

The separate deterministic Dispatcher soak exercises queue/refill/dedup scheduling;
the MongoDB runner deliberately excludes future Processor computation and finalizer
writes so the Phase 4 refill boundary remains measurable.

## Phase 5 Normalizer

The retained Normalizer test runs one release process across the ADR-0037 1, 4, 16,
and 128 KiB input classes. It reports per-class and 60/30/9/1 weighted RPS plus
canonical output bytes as an allocation-oriented proxy. The proxy deliberately does
not claim allocator call counts or bookkeeping bytes.

Run one candidate and compare it only on the same hardware, Rust toolchain, fixture,
weights, and Normalizer limits:

```text
node performance/run-normalizer.mjs
node performance/compare-normalizer.mjs performance/baselines/normalizer/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 10
```

The comparator fails on a per-size or weighted RPS regression over the budget,
canonical-output growth over the budget, or weighted throughput below 7,500 Events/s.
The 128 KiB fixture also verifies that compatible unknown strings remain bounded;
release, dist, and environment are governed separately by exact identity bounds and
are never truncated.

## Phase 6 Symbolication boundary

The Phase 6 baseline measures only deterministic no-work/native/JavaScript
classification, raw-frame preservation, and the disabled production fallback. It
performs no HTTP, debug-file lookup, source-map work, or backend cache access; those
belong to later adapter phases and require separate workloads.

Run at most one candidate per local performance pass, then verify that no Cargo/test
process remains:

```text
node performance/run-symbolication-baseline.mjs
node performance/compare-symbolication-baseline.mjs performance/baselines/symbolication/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 10
```

The comparator requires identical hardware, toolchain, fixture, backend mode, and
iteration count. It fails below 20,000 Events/s or beyond the supplied regression
budget. External backend latency is intentionally not inferred from this CPU result.

## Phase 7 Grouper

The Grouper baseline measures pure revision-1 canonical component selection,
length-delimited encoding, BLAKE3 GroupingKey hashing, and project-scoped Issue-ID
derivation. Its round-robin corpus covers message, exception stack, native
module-relative stack, and SDK fingerprint strategies without storage or network.

Run one candidate per local performance pass and check that its Cargo/test process
has exited before continuing:

```text
node performance/run-grouper.mjs
node performance/compare-grouper.mjs performance/baselines/grouper/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 10
```

The comparator requires identical hardware, toolchain, fixture, hash contract, and
iteration count. It fails below 20,000 Events/s, beyond the RPS regression budget, or
when the revision-1 corpus component size changes. Exact key and Issue-ID bytes are
protected separately by immutable Rust golden vectors.

## Phase 8 IssueStore

The retained MongoDB 8.0.12 benchmark executes the same atomic aggregation-pipeline
upsert used in production. One profile contends on a single Issue; the second spreads
the same operation count across 250 Issues. Both report explicit RPS with 64 in-flight
operations. It excludes Event finalization, hourly buckets, Release catalogs and HTTP,
which remain owned by later phases.

Run at most one candidate per local performance pass, then verify that Cargo and the
test executable have exited:

```text
node performance/run-issue-store.mjs
node performance/compare-issue-store.mjs performance/baselines/issue-store/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 15
```

The comparator requires the same hardware, Rust toolchain, MongoDB topology, fixture,
operation count and concurrency. The local Windows gates are 250 hot-Issue RPS and
500 distributed-Issue RPS; they are regression sentinels, not ADR-0037 end-to-end
capacity claims.

## Phase 9 Finalizer

The retained Finalizer benchmark measures the complete real-MongoDB durable fence:
one bounded batch replaces pending Event bodies, updates 100 Issues and hourly
buckets, materializes one Release and Environment, and removes pending state from
1,000 Events. It reports explicit Event RPS. It excludes Processor orchestration and
HTTP, so k6 would conflate boundaries and is intentionally not used for this module
test.

Run at most one candidate per local performance pass. The runner writes a comparable
artifact, and the comparator enforces fixture identity, a 150 Event/s local Windows
gate, and the configured regression budget:

```text
node performance/run-finalizer.mjs
node performance/compare-finalizer.mjs performance/baselines/finalizer/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 15
```

After the run, verify and terminate any lingering Cargo, Rust test, or k6 processes.
This local result is a regression sentinel, not the Phase 10 end-to-end recovery-rate
claim.

## Phase 10 Processor recovery

The retained recovery benchmark starts with 1,000 durable pending Error Events in one
hot Issue and measures the complete baseline Processor chain through terminal Event,
Issue and hourly-stat writes. Processor concurrency is 256; prepared results enter the
same bounded Finalizer batcher used by production composition. The artifact reports
explicit recovery RPS and its ratio to the ADR-0037 1,158 Event/s accepted-average
target.

This is an in-process completion benchmark with real MongoDB, so k6 is not used:
HTTP response throughput cannot prove that Events reached terminal processing state.
The separate retained k6 ingest suite continues to cover TCP and HTTP
`200`/`429`/`503` behavior.

Run at most one candidate per local performance pass:

```text
node performance/run-processor.mjs
node performance/compare-processor.mjs performance/baselines/processor/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 15
```

The comparator requires identical fixture, hardware, toolchain and MongoDB topology,
and fails below the 1.5x recovery ratio. After the run, terminate any lingering
Cargo, Rust test or k6 processes.

## Phase 11 login rate limiter

The retained authentication benchmark saturates the in-process account-and-network
login limiter with 500,000 attempts after its configured allowance is exhausted. It
reports explicit RPS, requires every measured attempt to be rejected and confirms the
bounded 10,000-entry state limit. It intentionally excludes Argon2id: rate-limited
attempts must be rejected before password work.

This is a security-control regression sentinel, not an ingest or successful-login
capacity claim. k6 is not used because Phase 11 has no HTTP login route; the native
API transport starts in Phase 12.

Run at most one candidate per local performance pass:

```text
node performance/run-auth-rate-limit.mjs
node performance/compare-auth-rate-limit.mjs performance/baselines/auth-rate-limit/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 15
```

The comparator requires identical hardware, toolchain, fixture and limiter
configuration, enforces 100,000 RPS and rejects regressions beyond the supplied
budget. After the run, terminate any lingering Cargo, Rust test or k6 processes.

## Phase 12 native API query

The retained real-MongoDB benchmark uses 2,000 finalized Events and executes 1,000
newest-first project Event-list queries at page size 50. It records explicit query
RPS plus p95/p99 latency. Search grammar, exact token post-verification, concurrent
insert pagination and accepted query-plan indexes are covered by the separate Phase
12 integration test; this benchmark isolates the stable timeline query shape.

Run at most one candidate per local performance pass:

```text
node performance/run-native-api-query.mjs
node performance/compare-native-api-query.mjs performance/baselines/native-api-query/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 15
```

The local Windows regression sentinel requires 100 RPS, p95 below 100 ms and p99
below 250 ms. It is not a server-tuned production capacity claim. The existing k6
ingest suites remain the HTTP overload/TCP/`200`/`429`/`503` baselines; this pass
deliberately runs only the one Phase 12 database-query performance test. After the
run, verify and terminate any lingering Cargo, Rust test, server or k6 processes.

## Phase 14 foreground ingest with maintenance

Phase 14 reuses the real-MongoDB durable HTTP workload while enabling Scheduler
against the same database. Event retention scans foreground writes in bounded `_id`
pages while due-backlog observation, counter reconciliation, and disabled
future-module hooks continue on their configured intervals. This is an interference
and regression sentinel; it does not claim that local Windows is a tuned server.

Run exactly one fixed-arrival test per local pass:

```text
cargo build --locked --release --bin durable-ingest-bench
$env:METRIC_BENCH_MAINTENANCE = "1"
$env:METRIC_BENCH_MONGODB_URI = "mongodb://127.0.0.1:27017/?retryWrites=false"
$env:METRIC_BENCH_DATABASE = "metric_phase14_bench_<fresh-id>"
target\release\durable-ingest-bench.exe

k6 run -e METRIC_RPS=1158 -e METRIC_DURATION=15s `
  -e METRIC_FIXTURE_REVISION=error-event-v1-mongodb-maintenance `
  -e METRIC_DURABILITY="MongoWriter plus concurrent Phase 14 Scheduler" `
  -e METRIC_RESULT=performance/results/phase14-maintenance-ingest.json `
  performance/k6/ingest-mongodb.js
```

The artifact records achieved RPS, p95/p99, dropped iterations, TCP errors, all HTTP
responses, and explicit `200`, `429`, `503`, and other statuses. Verify the MongoDB
Event count against `status_200`, stop the benchmark server even after a k6 failure,
drop the fresh database, and confirm that no benchmark, k6, Cargo, or Rust compiler
process remains. Compare later candidates with `performance/compare-k6.mjs` and the
reviewed Phase 14 baseline using the same fixture, RPS, hardware, and MongoDB topology.

## Phase 18 JavaScript Artifact Bundles

The retained Phase 18 benchmark uses the same ignored integration gate as the real
`sentry-cli` compatibility test. It uploads Source Bundles with pinned CLI 3.6.2 and
2.58.6, then measures 300 sequential real-MongoDB lookups for modern Debug-ID hit,
legacy release/dist hit, and miss. It also measures the already-open external
Symbolicator circuit in explicit requests per second. Upload, parsing, GC, and HTTP
remain functional assertions outside the lookup timing window.

Run at most one candidate per local performance pass:

```text
node performance/run-artifact-bundles.mjs
node performance/compare-artifact-bundles.mjs performance/baselines/artifact-bundles/ryzen-5600h-windows-mongodb-v1.json performance/results/<candidate>.json 20
```

The runner has a hard timeout and each real CLI child has its own kill timeout. After
the run, verify that no scoped `sentry-cli`, Cargo test, Metric server, or k6
process remains. The local Windows result is a regression sentinel, not a
server-tuned capacity claim; k6 is not used because this profile isolates MongoDB
lookup plans rather than browser or ingest HTTP stability.

## Phase 19 Incident Capsule

The retained Phase 19 benchmark isolates the bounded ZIP64 streaming writer after
authorization and database reads. Each sample contains one Issue, statistics,
activity, seven 8 KiB Event DTOs, capabilities, README and a final manifest. It
measures complete Capsule responses per second and compressed MiB/s in release mode.
Correctness, authorization, MongoDB reads and HTTP backpressure remain covered by
the separate E2E and failure suites.

Run at most one candidate per local performance pass:

```text
node performance/run-incident-capsule.mjs
node performance/compare-incident-capsule.mjs performance/baselines/incident-capsule/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 20
```

The local regression sentinel requires at least 20 complete Capsule responses per
second. The runner has a hard timeout and force-kills its Cargo child on expiry.
After the run, verify that no Cargo, Rust test, Metric server or k6 process
remains. k6 is not used because this benchmark has no persistent HTTP server and
the Web UI is not in the Phase 19 export timing path.

## Phase 20 notification expansion

The retained Phase 20 baseline measures the durable real-MongoDB handoff from 300
Issue-owned `new_issue` intents to deterministic delivery documents. One enabled
rule expands to one destination, in batches of 100. Setup is outside the timing
window; the measured path includes rule lookup, idempotent delivery upsert and
atomic removal of the embedded transition.

Run at most one candidate per local performance pass:

```text
node performance/run-notifications.mjs
node performance/compare-notifications.mjs performance/baselines/notifications/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 20
```

The result reports explicit transition expansion RPS and requires at least 50 RPS.
Webhook receiver latency is intentionally excluded because it is external and
unbounded; delivery correctness, signing, retry and timeout behavior use a controlled
receiver test. k6 is not used because there is no public notification HTTP ingress.
The runner has a hard timeout. After every run, verify that no Cargo, Rust test,
Metric server or k6 process remains.

## Phase 21 cold Event archive writer

The retained Phase 21 benchmark encodes 24 project/day segments containing 12,000
canonical scrubbed Events while a foreground BLAKE3 worker remains active. It
reports explicit archive Event RPS, input and compressed MiB/s, foreground ops/s,
and the maximum input segment bytes. MongoDB claims and S3 network latency are
covered by functional integration tests and intentionally excluded from this
portable writer regression sentinel.

Run at most one candidate per local performance pass:

```text
node performance/run-archive.mjs
node performance/compare-archive.mjs performance/baselines/archive/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 20
```

The local gate is 25,000 archived Events/s and a maximum 64 MiB input segment. The
comparator requires identical fixture, hardware, Rust toolchain and storage boundary,
then rejects archive, input-throughput, or foreground-throughput regressions beyond
the supplied budget. k6 is not used because Phase 21 has no public archive HTTP path;
using it would measure unrelated ingestion. The runner has a hard timeout and kills
its Cargo child on expiry. After every run, verify that no Cargo, Rust test,
Metric server, MinIO or k6 process remains.

## Phase 22 release load and saturation

`run-release-load.ps1` is the Windows durable-load wrapper for the cumulative release
path. It accepts only the ADR-0037 average, steady and burst rates, creates a
validated fresh `metric_phase22_*` database, starts one tracked benchmark server,
runs k6, and compares `status_200` with the durable Event count before cleanup.

```powershell
./performance/run-release-load.ps1 -Rps 5000 -Duration 15s
./performance/run-release-load.ps1 -Rps 20000 -Duration 15s
```

Every artifact includes explicit TCP, total HTTP, `200`, `429`, `503`, other-status,
dropped-iteration and latency values. A threshold failure still retains the artifact
and performs the durable-count verification. The server is stopped in `finally`, and
only the validated fresh database is dropped; the user's MongoDB process is never
stopped.

The reviewed local Windows artifacts under `baselines/release-hardening/` are short
smoke/saturation evidence, not passing release-capacity baselines. The 5,000/s run
achieved 4,983.16 RPS with p95 27.69 ms, no response failure, zero acknowledged loss
and 190 generator-dropped iterations. The 20,000/s saturation run achieved 7,307.42
RPS with p95 296.83 ms, no response failure, zero acknowledged loss and 188,444
generator-dropped iterations. The latter documents the current machine/generator
ceiling and fails the arrival-rate and latency gates.

Phase 22 remains blocked until controlled hardware passes 5,000/s for 60 minutes,
20,000/s for 5 minutes, backlog recovery/restart and the long soak. Run at most two
performance profiles per local implementation pass.

## Phase 24 Structured Logs

Phase 24 retains exactly two local regression profiles. The first isolates the
bounded Log writer, including batch occupancy and acknowledgement completion:

```text
node performance/run-log-writer.mjs
node performance/compare-log-writer.mjs performance/baselines/log-writer/ryzen-5600h-windows-v1.json performance/results/<candidate>.json 20
```

The local gate is 20,000 Log RPS, average batch occupancy of at least 100 documents,
and zero acknowledged loss. This is an in-memory writer boundary benchmark; the
second profile supplies real HTTP and MongoDB evidence.

The Windows runner creates a validated fresh `metric_phase24_*` database, starts
one PID-tracked benchmark server on port 3124, runs concurrent Log and Error k6
scenarios, verifies HTTP 200 counts against `logs` and `error_events`, then stops the
server and drops only the fresh database:

```powershell
./performance/run-structured-log-load.ps1 `
  -LogRps 1000 -ErrorRps 250 -Duration 10s

node performance/compare-structured-logs.mjs `
  performance/baselines/structured-logs/ryzen-5600h-windows-k6-v1.json `
  performance/results/<candidate>.json 20
```

The artifact retains Log/Error RPS and p95/p99 plus TCP, total HTTP, 200, 429, 503,
other-status and dropped-iteration counters. Run no more than these two profiles in
one Phase 24 pass. The short Windows baseline is a regression sentinel, not a
production-capacity claim.
