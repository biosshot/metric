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
  real_node_sdk_sends_an_error_event -- --ignored --nocapture
```

The test starts the real Faultkeep HTTP router on an ephemeral port, invokes the
official `@sentry/node` process with a real DSN, waits for `captureException` and
`flush`, and verifies the accepted Event ID, exception, release, environment and
exact SDK metadata. The same real Envelope includes a safe JSON attachment; the gate
also verifies blob-first metadata and the exact bytes read back from BlobStore.

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
