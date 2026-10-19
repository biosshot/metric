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

## Ruby and PHP SDKs

Ruby pins `sentry-ruby` 7.1.0 and Bundler 2.3.27; install on Ruby 3.1.7 with
`BUNDLE_PATH=.deps BUNDLE_FROZEN=true bundle install` from `sdk-tests/ruby`.
PHP pins `sentry/sentry` 4.34.0; use PHP 8.3 with curl, mbstring, OpenSSL and zip,
then run `python sdk-tests/php/prepare.py` from the repository root. Composer
2.10.3 is downloaded into the fixture and checked against its published SHA256;
the install uses the committed lockfile, with plugins and scripts disabled.
The Windows PHP sender supplies its own extension settings and does not edit php.ini.

Both real-process Error Event gates are in `sdk_compatibility_e2e`, named
`real_ruby_sdk_sends_an_error_event` and `real_php_sdk_sends_an_error_event`.
They verify accepted identity, SDK metadata, exception, environment/release and
absence of unexpected blobs. Ruby closes its worker before returning; the Go
sender also joins its telemetry worker with Client.Close after Flush to prevent
process exit while a scheduler-owned batch is still in flight.

The composed-server coordinator can execute the same 16 SDK paths and six CRUD
groups against either database:

```text
python scripts/test-postgres.py --suite server --backend postgres --sdk --browser --verify-purge
python scripts/test-postgres.py --suite server --backend mongodb --sdk --browser --verify-purge
```

Supply `--binary` to reuse a built server. Start the selected synthetic Docker
fixture first. Run each backend sequentially; the Mongo fixture is the existing
`metric-pg-baseline` project using `deploy/compose.dev.yml`. Each invocation owns
a fresh UUID database and Blob directory, verifies restart/readback, and drops
only that namespace after terminating its server. Purge checks run after SDK
readback with a fixture-only three-second grace, then require zero owned records
in all eleven populated primary families. Physical orphan Blob deletion follows
the configured GC grace and is tested separately by storage/service gates.

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

`sentry-cli/` pins current 3.x (`3.6.2`), retained 2.x (`2.58.6`), and
getsentry/cli (`sentry` on npm, `0.44.1`) contracts. Install with `npm ci`, verify
the pinned versions with `npm run versions`, then point a real upload at Metric:

```powershell
$env:SENTRY_URL = "http://127.0.0.1:4001/"
$env:SENTRY_AUTH_TOKEN = "<personal token with debug_file:write>"
$env:SENTRY_ORG = "<organization slug>"
$env:SENTRY_PROJECT = "<project slug>"
npx sentry-cli debug-files upload fixtures/metric.sym
```

## Real symbolication contracts and local demo

`scripts/test-symbolication.py` runs the actual Symbolicator 26.6.0 image pinned by
digest, the built Metric server and private lookup/download callbacks. It uploads
files with the existing real CLI 3.x, ingests events and reads finalized results
through Metric's public API. No new SDK family is added.

Prepare the existing CLI/browser dependencies and the synthetic database fixture:

```text
npm ci --prefix sdk-tests/sentry-cli
npm ci --prefix sdk-tests/browser
docker compose -p metric-pg-dev -f deploy/compose.postgres.dev.yml up -d --wait postgres
cargo build -p metric-server --bin metric-server --locked
python scripts/test-symbolication.py
```

Linux needs GCC, `nm` and `readelf` to build the small ELF/DWARF fixture. The real
Windows PDB fixture needs `clang`, `lld-link` and `llvm-pdbutil` on PATH, on both
Linux and Windows (`apt-get install clang lld llvm` on Ubuntu). It is cross-compiled
without a Windows SDK or CRT. On Windows the coordinator invokes the ELF tools in
the Ubuntu WSL distribution and the LLVM tools natively. Docker must
support `host.docker.internal:host-gateway`; Metric and the callback recorder bind
on host interfaces so the container can reach them. Arbitrary source scraping and
`connect_to_reserved_ips` stay disabled. Private downloads use project credentials.

The mandatory suite covers Breakpad, ELF/DWARF and a genuine compiled Windows PDB.
It uploads the PDB through `sentry-cli debug-files upload`, then repeats with
`--include-sources` and requires a published `sourcebundle`. Three PDB frames must
resolve to the expected callers and exact source snippets (five lines before and
after each location). PDB-only and foreign-project events must not acquire source
context from another project. Native frame addresses come from the PDB's line
records; the freestanding executable is not run and this is not a minidump test.

The suite also covers native partial and multiple traces, source maps by Debug ID and release/dist,
missing files, warm and negative caches, revisions and project isolation. Tests
also verify that the real backend fetched the private index and downloaded files.
JavaScript positions are captured from an actual three-call minified stack; all
three original functions and their exact source context are checked. Exact output
values come from fixture source, not a fake response. A missing
dependency or unexecuted required case fails the command.

JavaScript regression cases also insert a frame without a source location between
resolved frames and require the original indexes, coordinates and source context
to survive. A trace with no sendable frames must retain its raw stack and report
`missing`. These cases cover the interaction between incomplete-stack handling
and source-context preservation.

Use `--backend mongodb` with the existing Mongo fixture for the same contracts:

```text
docker compose -p metric-pg-baseline -f deploy/compose.dev.yml up -d --wait mongodb
python scripts/test-symbolication.py --backend mongodb
```

`--faults` adds a controlled HTTP 503 test through the complete server, checking
raw fallback after retries and recovery of subsequent successful symbolication.
`--sdk` additionally runs real Browser/Node source-map errors and the existing
nine pinned SDK senders. Prepare their runtimes and dependencies using the sections
above. For Chromium, run `npx playwright install chromium` in `sdk-tests/browser`.

Build the Web UI to verify visible three-call stacks, highlighted source context and
the original-frame toggle. To leave the synthetic installation available locally:

```text
npm ci --prefix web
npm run build --prefix web
python scripts/test-symbolication.py --sdk --ui --keep --port 4001
```

Open `http://localhost:4001`. The command prints synthetic login credentials and
direct native/JavaScript event URLs. They are also saved in the run's
`target/symbolication-tests/<run>/DEMO-RU.txt`. `--ui` saves `native-event.png` and
`javascript-event.png` after checking caller order, eleven source lines around the
failure, the highlighted line, and the original-frame toggle. Ctrl+C or creating an empty `STOP` file in
that run directory stops the demo and drops its owned database. Normal test runs
clean up automatically; shared development database containers remain available.
The kept demo runs a private copy of the built binary so Windows does not lock
Cargo's `target/debug/metric-server.exe` during subsequent builds.

Each run retains `report.json` with case results, finalized event IDs, sanitized
real HTTP contracts, callback observations and the image digest. No DSN or service
token is written into this report. The external Symbolicator remains separately
deployed and is never bundled into Metric.

The `symbolication` job in `.github/workflows/ci.yml` runs the mandatory real suite
on PRs, main pushes and release Checks. Release publication depends on Checks;
the job has no successful skip or `continue-on-error`. The small JSON files under
`symbolicator/` are decoder unit fixtures; they do not establish real integration
success by themselves. Portable PDB, Mach-O and minidump processing are not covered
by the new native fixtures. PostgreSQL migration 0022 adds the native source bundle
file type; existing event JSON and debug files require no data conversion.
