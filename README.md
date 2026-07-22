# faultkeep

Phase 4 adds a bounded Dispatcher between durable Event acceptance and the future
Processor. It deduplicates queued/running Event keys and refills due pending work
from MongoDB on startup, idle polls, and a low watermark. MongoDB remains the only
durable backlog, so queue saturation or process restart cannot undo an acknowledged
acceptance.

The real Processor starts in Phase 7. Until then production composes a deferred
`WorkHandler` that never falsely completes pending work; deterministic completing
handlers remain limited to Phase 4 tests.

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
