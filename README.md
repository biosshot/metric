# faultkeep

Phase 0 provides the bounded Cargo workspace foundation described by ADR-0039. It
does not implement Event ingestion or persistence.

## Local checks

```text
cargo fmt --all -- --check
cargo dep-graph --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test-fast --locked
```

Additional risk-tier commands are `cargo test-infrastructure`, `cargo test-fuzz`,
and `cargo test-performance`. Infrastructure tests use the exact MongoDB image in
`deploy/compose.dev.yml`; Phase 0 itself has no database integration tests.

Validate configuration without starting the server:

```text
cargo run -p faultkeep-server -- --config config/faultkeep.example.toml --check-config
```

The example secret reference expects `MONGODB_URI` to be present. Effective
configuration can be printed with `--print-effective-config`; secret values are
always redacted.
