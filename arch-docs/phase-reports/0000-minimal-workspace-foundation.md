# Phase 0 report: minimal workspace foundation

- Status: exit gate passed
- Date: 2026-07-21
- Scope: ADR-0039 Phase 0 only; Phase 1 has not started

## Contract and public errors

The ADR-0034 crate graph is implemented with `domain`, `ports`, `sentry-protocol`,
`application`, `mongo`, `blob`, `symbolication`, `server`, and `testkit`. Product
behavior is absent. `domain` owns bounded ID, byte-size, duration, timestamp, and
stable error-code values. `application` owns the cancellation root and operability
facade. `server` is the configuration and composition root.

Configuration exposes stable, redacted startup diagnostics for invalid/unknown
configuration, invalid bounds, forbidden literal secrets, missing environment
secrets, unreadable secret files, oversized/empty/non-UTF-8 secrets, tracing setup,
and listener failures. No product API error model exists yet.

## Resource bounds and cancellation

- IDs, public error codes, durations, byte counts, timestamps, environment names,
  and secret contents have explicit bounds.
- Secret files and values are limited to 64 KiB. Only owning secret line endings are
  removed; general whitespace is retained.
- Shutdown grace is limited to five minutes. One root fences the `/live` probe and
  cancels the HTTP server; the server aborts after the configured grace.
- Empty adapter/protocol/ports crates create no objects, threads, async tasks, queues,
  or global allocations. The metrics facade is a zero-sized value and installs no
  exporter or worker.

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

Focused tests cover bounded primitives, cancellation fan-out, request ID shape,
dependency cycles/direction, `--check-config`, unknown TOML/environment fields,
literal-secret policy, effective-config redaction, `/live`, shutdown fencing, and
bounded graceful termination. The infrastructure command verifies that local
orchestration is pinned to `mongo:8.0.12`; no MongoDB adapter test is applicable in
Phase 0. The Docker daemon was not available in the implementation environment, so
the container itself was not started.

## E2E, metrics, health, and logging

The cumulative E2E ladder begins in Phase 1, so no product E2E is possible yet.
Phase 0 supplies `/live`, fixed low-cardinality HTTP/shutdown metrics, a fixed-shape
request ID, JSON tracing, and only bounded operation/outcome fields. Payloads,
secrets, raw URLs, project/user identifiers, and arbitrary error strings are not
metric labels or request trace fields.

## Known limits and deferred work

There is no Event behavior, ingestion, ProjectResolver, MongoDB client/schema,
generic repository, BlobStore, Symbolicator client, migration, MCP, NATS, sharding,
disk spool, readiness dependency graph, Prometheus exporter, or product worker.
These remain in their accepted later phases. The Phase 0 metrics facade intentionally
acts as a no-op until composition installs an exporter in a phase that owns metrics
exposure.
