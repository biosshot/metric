# Contributing to Metric

Thank you for helping improve Metric.

## Before you start

- Search existing issues before opening a new one.
- Use the bug report template for broken behavior.
- Use the feature request template for new ideas.
- Keep each issue and pull request focused on one topic.

## Development setup

Metric uses Rust 1.88, Node.js 24 and MongoDB 8.

Start the development database:

```bash
docker compose -f deploy/compose.dev.yml up -d
```

Copy the local environment example:

```bash
cp .env.local.example .env.local
```

Replace the `SCRUB_HMAC_KEY` placeholder with 64 lowercase hexadecimal
characters.

Run the server:

```bash
cargo run -p metric-server --bin metric-server -- --env-file .env.local --config config/metric.example.toml
```

Run the main checks:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

For web changes:

```bash
cd web
npm ci
npm run format:check
npm run lint
npm test
npm run build
```

## Pull requests

Explain:

- what changed;
- why it changed;
- how you tested it.

Update documentation when behavior, configuration or setup changes. By submitting
a contribution, you agree that it is licensed under the MIT License used by this
project.
