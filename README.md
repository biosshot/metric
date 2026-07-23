# faultkeep

Phase 11 adds the authoritative self-hosted identity boundary: first-owner bootstrap,
Argon2id passwords, opaque Web sessions with CSRF, scoped personal API tokens,
organization roles/permissions, final-owner protection and bounded audit records.
The production Processor from Phase 10 remains the sole ordered path from durable
Events to finalized Issues and hourly statistics.

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

On the first MongoDB-backed startup, the server prints one
`FAULTKEEP_BOOTSTRAP_TOKEN`. It is shown only when its digest is first persisted;
store the plaintext securely. Phase 12/13 will expose the API/Web transport that
consumes it. Insecure session cookies require both `auth.secure_cookie = false` and
the explicit local-only `development.allow_insecure_cookies = true`.
