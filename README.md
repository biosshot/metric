# faultkeep

Phase 6 adds the application-owned Symbolication boundary after the pure Normalizer.
It classifies no-work, native, and JavaScript/Node traces, preserves raw frames in
every outcome, validates bounded backend-independent derived frames, and exposes
typed retry/finalize-raw policy hooks.

The real Processor starts in Phase 10. Until then production still composes a deferred
`WorkHandler` that never falsely completes pending work. The Phase 6 production
baseline performs no network work and finalizes required symbolication with raw frames
plus an unavailable diagnostic; the external HTTP adapter remains deferred.

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
