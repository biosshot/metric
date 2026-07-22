# faultkeep

Phase 7 adds the pure revision-pinned Grouper after Normalizer and the Symbolication
boundary. It selects SDK fingerprint, exception stack, native module-relative stack,
or normalized-message components; encodes them canonically; and derives a fixed
GroupingKey and project-scoped Issue ID with BLAKE3.

The real Processor starts in Phase 10. Until then production still composes a deferred
`WorkHandler` that never falsely completes pending work. Grouper performs no storage,
network, clock, Issue mutation, finalization, or automatic revision upgrade.

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
