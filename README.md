# faultkeep

Faultkeep provides Sentry-compatible Error Event ingestion, durable processing,
native investigation APIs, and a minimal Vue Web interface. The authoritative
identity boundary uses Argon2id passwords, opaque Web sessions with CSRF, scoped
personal API tokens, organization roles/permissions, final-owner protection, and
bounded audit records.

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

The retained k6 workloads, JSON baselines, and regression comparators are documented
in `performance/README.md`.

Real official SDK compatibility harnesses and the versioned result matrix live in
`sdk-tests/`. Each SDK has an isolated dependency graph. The initial Node gate is:

```text
cd sdk-tests/node
npm ci
cd ../..
cargo test -p faultkeep-server --test sdk_compatibility_e2e real_node_sdk_sends_an_error_event -- --ignored --nocapture
```

## Web development

The Phase 13 Vue 3 client lives in `web/` and consumes only `/api/v1`. Run
`npm install` once, then `npm run dev` in that directory while Faultkeep listens
on `127.0.0.1:3000`.

`npm run build` creates `web/dist`, which the Rust server serves on the supported
Web routes. `FAULTKEEP_WEB_DIR` can point to an alternative production asset
directory.

Web checks are `npm run format:check`, `npm run lint`, `npm test`,
`npm run build`, and `npm run test:e2e`.

Validate configuration without starting the server:

```text
cargo run -p faultkeep-server --bin faultkeep-server -- --config config/faultkeep.example.toml --check-config
```

For local development, copy `.env.local.example` to `.env.local`, replace the
placeholder secret, and pass it explicitly:

```text
cargo run -p faultkeep-server --bin faultkeep-server -- --env-file .env.local --config config/faultkeep.example.toml
```

Faultkeep never searches for dotenv files implicitly. The named file must be a
regular file no larger than 256 KiB and must parse completely. Existing process
environment variables take precedence over values in the file; `APP__...`
values then override matching TOML settings. Effective configuration and errors
never print secret values.

The example secret reference expects `MONGODB_URI` to be present. Effective
configuration can be printed with `--print-effective-config`; secret values are
always redacted.

On the first MongoDB-backed startup, the server prints one
`FAULTKEEP_BOOTSTRAP_TOKEN`. It is shown only when its digest is first persisted;
store the plaintext securely. The Web first-setup form consumes it once.
Insecure session cookies require both `auth.secure_cookie = false` and
the explicit local-only `development.allow_insecure_cookies = true`.

Phase 14 maintenance uses the typed `[retention]` and `[scheduler]` sections in
`config/faultkeep.example.toml`. New processed Events use those durations
immediately. Existing processed Events and hourly statistics are adjusted gradually
in bounded keyset pages; pending Events never receive retention expiry. The
all-in-one server starts Scheduler before becoming ready and stops it through the
ordinary graceful-shutdown fence.
