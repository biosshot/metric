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
