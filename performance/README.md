# Performance regression artifacts

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
k6 run -e FAULTKEEP_RPS=2500 -e FAULTKEEP_DURATION=15s -e FAULTKEEP_RESULT=performance/results/ingest-fake-2500.json performance/k6/ingest-fake.js
```

Every run writes a JSON artifact under `performance/results`. Set commit, toolchain,
k6 version and hardware through `FAULTKEEP_COMMIT`, `FAULTKEEP_RUST`,
`FAULTKEEP_K6`, and `FAULTKEEP_HARDWARE`. Reviewed reference results are copied to
`performance/baselines/ingest-fake`; baseline changes require an explanation in the
Phase report. Short local runs find regressions, while the 5,000/s steady and
20,000/s burst gates run on controlled hardware.

Compare a candidate with a like-for-like baseline (same fixture, mode and target
RPS). The command exits non-zero on dropped iterations, a higher error rate, or an
RPS/p95 regression greater than the percentage budget:

```text
node performance/compare-k6.mjs performance/baselines/ingest-fake/ryzen-5600h-windows-k6-v1.json performance/results/ingest-fake-2500.json 10
```

For a saturation probe, set `FAULTKEEP_MODE=max-throughput` and
`FAULTKEEP_VUS=64`. Saturation results document a limit; they do not replace the
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
FAULTKEEP_BENCH_MONGODB_URI=<uri> FAULTKEEP_BENCH_DATABASE=<fresh-db> target/release/durable-ingest-bench
k6 run -e FAULTKEEP_RPS=5000 -e FAULTKEEP_DURATION=15s -e FAULTKEEP_RESULT=performance/results/ingest-mongodb-5000.json performance/k6/ingest-mongodb.js
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
