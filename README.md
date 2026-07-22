# faultkeep

Phase 5 adds a pure deterministic Error Event Normalizer after the bounded Dispatcher
foundation. It converts scrubbed accepted JSON into a stable adapter-independent
domain model with canonical timestamps, exception/frame structures, tags, contexts,
breadcrumbs, SDK identity fields, compatible unknown data, and bounded diagnostics.

The real Processor starts in Phase 10. Until then production still composes a deferred
`WorkHandler` that never falsely completes pending work; Normalizer has no database,
network, retry, symbolication, grouping, or finalization side effects.

## Local checks

```text
cargo fmt --all -- --check
cargo dep-graph --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test-fast --locked
```

Additional risk-tier commands are `cargo test-infrastructure`, `cargo test-fuzz`,
and narrowly selected ignored performance tests. Infrastructure tests use the exact MongoDB image in
`deploy/compose.dev.yml`, published on local port `27018` to avoid colliding with a
developer MongoDB on its standard port.

The retained k6 workload, JSON baselines, and regression comparator are documented
in `performance/README.md`.

Validate configuration without starting the server:

```text
cargo run -p faultkeep-server -- --config config/faultkeep.example.toml --check-config
```

The example secret reference expects `MONGODB_URI` to be present. Effective
configuration can be printed with `--print-effective-config`; secret values are
always redacted.
