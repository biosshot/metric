# Real SDK compatibility tests

This directory owns isolated, version-pinned compatibility harnesses for official
Sentry SDKs. Each SDK family has its own package/runtime dependencies and must send
through Faultkeep's public Sentry-compatible HTTP surface. SDK packages are never
linked into the Faultkeep server or Vue Web application.

`../compatibility/sentry-sdk-matrix.toml` is the single retained machine-readable
result matrix. A row is marked `pass` only after the referenced real-process or
immutable captured-fixture gate passes. Validate the inventory and evidence with
`python scripts/validate-compatibility.py`; release gating adds `--require-all`.

## Rules

- Pin the exact official SDK version and commit its lockfile.
- Use safe synthetic Events without credentials or production data.
- Start Faultkeep on an ephemeral local port and terminate every SDK/server process.
- Assert the SDK-reported Event ID and the accepted domain Event payload.
- Record the exact runtime and SDK version in the test output or assertion.
- Keep performance and compatibility tests separate.
- Add a new directory for each SDK family; do not share package dependency graphs.

## Node SDK

Install its isolated dependencies:

```text
cd sdk-tests/node
npm ci
```

Run the real SDK gate from the repository root:

```text
cargo test -p faultkeep-server --test sdk_compatibility_e2e \
  real_node_sdk_sends_an_error_event_without_blob -- --ignored --nocapture
```

The test starts the real Faultkeep HTTP router on an ephemeral port, invokes the
official `@sentry/node` process with a real DSN, waits for `captureException` and
`flush`, and verifies the accepted Event ID, exception, release, environment and
exact SDK metadata. The base Error Event also proves that no BlobStore object is
created when the SDK does not attach one. A separate
`real_node_sdk_sends_an_attachment_event` gate verifies blob-first attachment
metadata and the exact bytes read back from BlobStore.

## Browser SDK

Install and bundle its isolated dependencies:

```text
cd sdk-tests/browser
npm ci
npm run build
npx playwright install chromium
```

Run `real_browser_sdk_sends_an_error_event` from the same Rust E2E target. The harness
serves the bundled official `@sentry/browser` 10.66.0 client from the ephemeral
Faultkeep router, launches the pinned Playwright Chromium, sends through a real DSN,
waits for `flush`, and verifies the accepted Error Event and absence of attachment
blobs. The browser and server are closed before the test returns.

## Go SDK

The isolated `go/` module pins `sentry-go` 0.48.0. Verify the lock data and run the
real process gate from the repository root:

```text
cd sdk-tests/go
go mod verify
go test ./...
cd ../..
cargo test -p faultkeep-server --test sdk_compatibility_e2e \
  real_go_sdk_sends_an_error_event -- --ignored --exact --nocapture
```

The sender has a 15-second process deadline and an 8-second SDK flush deadline.

## Rust SDK

The isolated `rust/` workspace pins `sentry` 0.48.5 in its own `Cargo.lock`. Build
and run the real-process gate from the repository root:

```text
cargo build --locked --manifest-path sdk-tests/rust/Cargo.toml
cargo test -p faultkeep-server --test sdk_compatibility_e2e \
  real_rust_sdk_sends_an_error_event -- --ignored --exact --nocapture
```

The official SDK uses a hyphenated UUID in the envelope header. Faultkeep accepts
that wire representation at the protocol adapter and retains its compact 32-hex
domain identifier. Both Go and Rust gates verify metadata, exception content, Event
identity and the absence of blob objects for a base Error Event.

## Sentry CLI debug files

`sentry-cli/` pins current 3.x (`3.6.2`) and retained 2.x (`2.58.6`) contracts.
Install with `npm ci`, verify both binaries with `npm run versions`, then point a
real upload at Faultkeep:

```powershell
$env:SENTRY_URL = "http://127.0.0.1:4001/"
$env:SENTRY_AUTH_TOKEN = "<personal token with debug_file:write>"
$env:SENTRY_ORG = "<organization slug>"
$env:SENTRY_PROJECT = "<project slug>"
npx sentry-cli debug-files upload fixtures/faultkeep.sym
```

`symbolicator/26.6.0-native-contract.json` pins the external native HTTP schema
used by the adapter test. The external process remains separately deployed and is
never bundled into Faultkeep.
