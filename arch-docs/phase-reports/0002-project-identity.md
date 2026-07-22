# Phase 2 report: Project identity and DSN resolution

- Status: exit gate passed; Phase 3 not started
- Date: 2026-07-22
- Scope: ADR-0039 Phase 2 only
- Implementation commit: `3851321fc95658ed056a5ca242c0f59e94862c68`

## Contract and implementation

The accepted contract is `module-contracts/0002-project-identity-phase-2.md`.
`domain` owns bounded IDs, canonical slugs, project/key states and the immutable
ingest snapshot. `application::projects::ProjectService` owns typed creation/state
commands, cryptographic generation, bounded collision retries and the local cache.
The `ProjectStore` port contains only domain-oriented project identity operations.
The MongoDB adapter alone owns BSON documents, validators, indexes and authoritative
key lookup; server composition supplies configuration, HMAC material and startup
deadlines.

The cache has bounded entries and distinct in-flight misses, separate positive and
negative TTLs, approximate LRU eviction, same-key miss coalescing and generation-
fenced invalidation. Unavailable lookups are never cached. A resolved snapshot must
have both an active project and an active key before Ingest can accept it. Public
missing, disabled, deletion-fenced and path-mismatched credentials remain the same
generic unauthorized response.

## Exit gate

| ADR-0039 Phase 2 gate | Evidence | Result |
| --- | --- | --- |
| Real MongoDB identity/uniqueness and authorization | MongoDB 8.0.12 test verifies idempotent marker/bootstrap, strict validators, exact indexes, duplicate IDs/slugs/keys and active/disabled/pending-delete behavior | pass |
| Cross-project mismatch, disabled/deleted key, collision paths | HTTP E2E rejects path mismatch and invalidated disabled key; unit/integration tests cover generated ID/key collisions and deletion fencing | pass |
| Cache coalescing, TTL, invalidation and bounded capacity | Unit tests cover same-key coalescing, positive/negative expiry, approximate-LRU capacity, max distinct in-flight misses and immediate invalidation | pass |
| Lookup load at/above ADR-0037 burst rate | Warm cache: 3,549,983 RPS, 282 ns average, minimum gate 20,000 RPS | pass |
| E2E real resolver plus fake EventSink | `HTTP -> Ingest -> ProjectService cache -> MongoDB -> fake EventSink -> response` integration test | pass |

## Performance baseline

Recorded on AMD Ryzen 5 5600H, 15.9 GiB RAM, Windows; Rust 1.88.0 release
profile; MongoDB 8.0.12 standalone in Docker Desktop; Rust driver 3.8.0.

| Path | Iterations | RPS | Average | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Warm bounded application cache | 200,000 | 3,549,983 | 282 ns | n/a | n/a | n/a |
| Direct MongoDB lookup, no application cache | 1,000 | 366 | 2,736 us | 2,606 us | 3,816 us | 4,798 us |

The direct MongoDB number is a local cold-path reference and is not presented as a
20,000 RPS result. The hot-path gate is the cached resolver mandated by ADR-0019.
The JSON baseline, release benchmark tests, candidate runner and percentage-budget
comparator are retained under `performance/` for future improvement/regression
evaluation.

## Verification

Passing commands on Rust 1.88.0:

```text
cargo fmt --all -- --check
cargo dep-graph --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test-fast --locked
cargo test-fuzz --locked
cargo test --locked --workspace infrastructure_ -- --ignored --nocapture
cargo test --locked --release --workspace performance_ -- --ignored --nocapture
```

The test MongoDB image is pinned to `mongo:8.0.12` and published on host port 27018
to avoid accidentally reaching a developer MongoDB on the standard port.

## Deferred scope and next phase

No migrations, Event BSON/storage, MongoWriter, MCP, NATS, sharding, disk spool,
users, audit, purge worker or distributed cache invalidation was added. Production
composition still cannot claim durable Event success because the EventSink remains
unavailable. Phase 3 may now start with the ADR-0022 Event codec, EventStore port,
MongoDB adapter and bounded MongoWriter; its first gate must preserve the cumulative
HTTP and real ProjectResolver behavior closed here.
