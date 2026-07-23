# Phase 1 report: Sentry HTTP Ingest with fake ports

- Status: exit gate passed; Phase 2 not started
- Date: 2026-07-21
- Scope: ADR-0039 Phase 1 only

## Contract and public errors

The accepted contract is `module-contracts/0001-ingest-phase-1.md`. The server owns
bounded HTTP streaming, gzip/deflate decode, admission and response mapping;
`sentry-protocol` owns Envelope, Item, DSN and auth parsing; `application::ingest`
owns project consistency, mandatory scrubbing and durable-outcome orchestration.
Domain values cross ports to `ProjectResolver`, `EventSink`, `OutcomeSink`, `Clock`
and `RandomSource`. Stable client codes do not expose payloads, credentials or
backend/parser text.

Production composition uses unavailable adapters and therefore cannot acknowledge
fake durability. The deterministic sink is compiled only in `testkit` and the
separate `ingest-bench` binary. No accepted ADR was changed.

## Correctness, resources, cancellation and recovery

Tests cover golden and captured Error Events, an official Python SDK 2.32.0 capture,
header/query/Envelope auth, project conflicts, authoritative Item lengths,
malformed/truncated input, mixed disabled and unknown Items, client reports,
gzip/deflate, decompression and Event limits, bounded Item counts, recursive secret
and IP scrubbing, duplicate outcomes, admission exhaustion, slow body/sink,
deadline, shutdown fencing and safe errors.

Compressed and decompressed byte limits are independent. Envelope headers, auth,
Event bytes, Item counts, client-report entries and fields are bounded. Active,
parsing and storage-wait permits are finite; the parsing permit is released before
waiting on durable storage. A deadline or shutdown fence cannot be reported as a
durable success. The Phase has no retry/recovery loop because the fake port returns
the same durable outcome contract required of the later real adapter.

## Cumulative E2E, operability and compatibility

The first cumulative black-box rung passes:

```text
HTTP -> authentication/framing/admission -> mandatory scrub -> fake durable outcome -> response
```

Request/outcome metrics have fixed labels, rejected outcomes are counted on early
HTTP exits, tracing spans are not held through an uninstrumented future, and logs do
not contain auth, DSNs, bodies, URLs or event fields. The compatibility matrix
records the Python SDK 2.32.0 fixture as passing; unrecorded SDK/version rows are not
claimed.

The later real-process compatibility gate also records `@sentry/node` 10.66.0 on
Node.js 26.5.0 as passing. That gate exposed the official SDK's optional-length,
newline-delimited JSON Item framing. The bounded parser now accepts that framing
while retaining exact declared-length validation and rejecting an unterminated
lengthless Item above the Event byte limit. The retained sender and matrix are under
`sdk-tests/`.

## Performance baseline

Recorded host: AMD Ryzen 5 5600H, 6 cores/12 threads, 16 GiB RAM, Windows, with k6
2.1.0 and the server colocated; Rust 1.88.0, release profile, fixture
`error-event-v1`, deterministic non-MongoDB sink.

| Workload | Result | Errors | p95 | Outcome |
| --- | ---: | ---: | ---: | --- |
| k6 fixed arrival, 2,500 RPS for 15 s | 2,499.92 RPS | 0% | 0.517 ms | pass, 0 dropped; p99 0.618 ms |
| k6 fixed arrival, 3,000 RPS repeat | 2,995.11 RPS | 0% | 0.522 ms | 74 dropped; excluded from stable baseline |
| k6 saturation, 64 VUs for 15 s | 5,451.64 RPS | 0% | 22.338 ms | recorded local ceiling |
| k6 fixed arrival, 5,000 RPS for 15 s | 4,972.99 RPS | 0% | 1.049 ms | 398 dropped; not hidden |
| Envelope parser, in-process | 3,929,829 RPS | n/a | n/a | pass >= 20,000 RPS |
| validation plus mandatory scrub, in-process | 135,113 RPS | n/a | n/a | pass >= 20,000 RPS |

The 2,500 RPS baseline transferred 3,092,401 request bytes/s and 372,488 response
bytes/s. The HTTP run and isolated hot-path gates show that framing/parsing is not the
expected 20,000 Event/s burst bottleneck. This development laptop does not establish
a 20,000 RPS black-box capacity claim: repeated 3,000 RPS and the 5,000 RPS attempt
exposed a colocated generator/server scheduling ceiling. The JSON baseline is retained under
`performance/baselines`; k6 writes candidate artifacts under ignored
`performance/results`, and `performance/compare-k6.mjs` performs like-for-like RPS,
p95, error-rate and dropped-iteration regression checks.

## Verification

Passing commands on Rust 1.88.0:

```text
cargo fmt --all -- --check
cargo dep-graph --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test-fast --locked
cargo test-fuzz --locked
cargo test-infrastructure
cargo test-performance
```

## Known limits and deliberately deferred cases

The black-box 20,000 RPS burst remains a controlled-reference-host capacity run, not
a claim from this machine. Only the recorded Python SDK row is claimed so far.
MongoDB durability and project/key storage begin in Phase 2; migrations, MCP, NATS,
sharding, disk spool, attachments, non-Error Event products and split roles remain
deferred. Phase 2 was not started because this report closes only Phase 1.
