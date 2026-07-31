# Real SDK compatibility tests

This directory owns isolated, version-pinned compatibility harnesses for official
Sentry SDKs. Each SDK family has its own package/runtime dependencies and must send
through Metric's public Sentry-compatible HTTP surface. SDK packages are never
linked into the Metric server or Vue Web application.

`../compatibility/sentry-sdk-matrix.toml` is the single retained machine-readable
result matrix. A row is marked `pass` only after the referenced real-process or
immutable captured-fixture gate passes. Validate the inventory and evidence with
`python scripts/validate-compatibility.py`; release gating adds `--require-all`.

## Rules

- Pin the exact official SDK version and commit its lockfile.
- Use safe synthetic Events without credentials or production data.
- Start Metric on an ephemeral local port and terminate every SDK/server process.
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
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_node_sdk_sends_an_error_event_without_blob -- --ignored --nocapture
```

The test starts the real Metric HTTP router on an ephemeral port, invokes the
official `@sentry/node` process with a real DSN, waits for `captureException` and
`flush`, and verifies the accepted Event ID, exception, release, environment and
exact SDK metadata. The base Error Event also proves that no BlobStore object is
created when the SDK does not attach one. A separate
`real_node_sdk_sends_an_attachment_event` gate verifies blob-first attachment
metadata and the exact bytes read back from BlobStore.

Run the pinned SDK against an already running Metric instance to verify Structured
Logs and a transaction with one child Span:

```powershell
$env:METRIC_DSN = "http://<dsn-key>@localhost:4001/<project-id>"
node sdk-tests/node/send-signals.mjs
```

The sender waits for both SDK buffers to flush and then closes the SDK, so it leaves
no receiver or worker process behind.

Verify Cron Monitoring with the same pinned real SDK:

```powershell
node sdk-tests/node/send-cron.mjs `
  "http://<dsn-key>@localhost:4001/<project-id>" metric-node-cron

node sdk-tests/node/send-metrics.mjs `
  "http://<dsn-key>@localhost:4001/<project-id>"
```

It sends one `in_progress` and one terminal `ok` check-in with an interval monitor
configuration, flushes both envelopes, and terminates within a hard 15-second deadline.

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
Metric router, launches the pinned Playwright Chromium, sends through a real DSN,
waits for `flush`, and verifies the accepted Error Event and absence of attachment
blobs. The browser and server are closed before the test returns.

Phase 38 additionally pins the real Session Replay path:

```text
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_browser_sdk_records_uploads_retrieves_and_plays_replay \
  -- --ignored --exact --nocapture
```

The browser uses `@sentry/browser` 10.66.0 with client-side text/input masking,
compression and media blocking. The test records an interaction in Chromium,
uploads the paired Replay items, retrieves the exact stored segment and mounts it
with `rrweb-player` 2.1.1. A synthetic secret entered before the flush must not occur
in the decompressed recording. This proves the pinned client configuration, not a
server-side DOM privacy guarantee.

For a manual visible recording, build and serve the interactive demo:

```powershell
cd sdk-tests/browser
npm ci
npm run build
python -m http.server 4173
```

Open `replay-demo.html?dsn=<URL-encoded DSN>`, interact with the counter, tabs, form,
theme, modal and scroll area, then click **Flush & stop Replay**. Use
**Send test Feedback** to submit the fixed browser Feedback fixture with a safe text
attachment; the page shows the returned Feedback ID. This action requires attachments
to be enabled, as they are in the supplied Low, Medium and High profiles. The page
uses the same pinned official `@sentry/browser` package and stops recording after a
successful flush, so the demo tab does not keep appending segments. Ordinary
interface text remains visible, while input values are masked before transport.

## Python SDK

`python/requirements.lock.txt` pins `sentry-sdk` 2.32.0 and its transport
dependencies. Install and run the real-process gate:

```text
python -m pip install -r sdk-tests/python/requirements.lock.txt
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_python_sdk_sends_an_error_event -- --ignored --exact --nocapture
```

Default framework integrations are disabled because this gate isolates the official
transport and Error Event schema.

## Java SDK

`java/prepare.mjs` downloads the standalone `sentry-java` 8.50.1 JAR, verifies its
pinned SHA-256 and compiles the sender with Java 25:

```text
node sdk-tests/java/prepare.mjs
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_java_sdk_sends_an_error_event -- --ignored --exact --nocapture
```

## .NET SDK

The isolated .NET 9 project pins `Sentry` 6.7.0 and commits NuGet lock data:

```text
dotnet restore --locked-mode sdk-tests/dotnet/MetricSdkCompatibility.csproj
dotnet build --configuration Release --no-restore \
  sdk-tests/dotnet/MetricSdkCompatibility.csproj
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_dotnet_sdk_sends_an_error_event -- --ignored --exact --nocapture
```

All three gates verify Event identity, SDK/release/environment metadata, exception
content, bounded process completion and zero blob objects for a base Error Event.

## Go SDK

The isolated `go/` module pins `sentry-go` 0.48.0. Verify the lock data and run the
real process gate from the repository root:

```text
cd sdk-tests/go
go mod verify
go test ./...
cd ../..
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_go_sdk_sends_an_error_event -- --ignored --exact --nocapture
```

The sender has a 15-second process deadline and an 8-second SDK flush deadline.

## Rust SDK

The isolated `rust/` workspace pins `sentry` 0.48.5 in its own `Cargo.lock`. Build
and run the real-process gate from the repository root:

```text
cargo build --locked --manifest-path sdk-tests/rust/Cargo.toml
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_rust_sdk_sends_an_error_event -- --ignored --exact --nocapture
```

The official SDK uses a hyphenated UUID in the envelope header. Metric accepts
that wire representation at the protocol adapter and retains its compact 32-hex
domain identifier. Both Go and Rust gates verify metadata, exception content, Event
identity and the absence of blob objects for a base Error Event.

## Sentry CLI debug files

`sentry-cli/` pins current 3.x (`3.6.2`) and retained 2.x (`2.58.6`) contracts.
Install with `npm ci`, verify both binaries with `npm run versions`, then point a
real upload at Metric:

```powershell
$env:SENTRY_URL = "http://127.0.0.1:4001/"
$env:SENTRY_AUTH_TOKEN = "<personal token with debug_file:write>"
$env:SENTRY_ORG = "<organization slug>"
$env:SENTRY_PROJECT = "<project slug>"
npx sentry-cli debug-files upload fixtures/metric.sym
```

`symbolicator/26.6.0-native-contract.json` pins the external native HTTP schema
used by the adapter test. The external process remains separately deployed and is
never bundled into Metric.
