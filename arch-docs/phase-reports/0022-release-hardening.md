# Phase 22 report: full-system verification and packaging

Status: complete
Date: 2026-07-24
Contract: `arch-docs/module-contracts/0022-release-hardening-phase-22.md`

## Implemented in this pass

- Consolidated SDK and CLI claims into one version-3 machine-readable matrix. Every
  ADR-0036 family is present, and an executable Python validator prevents a `pass`
  row without exact version/runtime, fixture/test evidence and a passing build.
- Selected Python, Java and .NET as the explicit version-one release-required SDK
  families. Their real-process Error Event gates pass. Browser JavaScript, Node, Go
  and Rust remain additional passing claims; seven non-required families remain
  honestly `untested`.
- Added a non-root multi-stage container image with the Vue production build, pinned
  Rust 1.88 builder, one Faultkeep `--role all` binary, local BlobStore volume and
  healthcheck.
- Added a static-valid all-in-one compose deployment with pinned MongoDB 8.0.12,
  persistent Mongo/Blob volumes, runtime secrets, read-only Faultkeep root filesystem,
  graceful stop and no bundled Symbolicator.
- Added a PID-tracked Windows durable k6 runner. It uses only validated fresh
  `faultkeep_phase22_*` databases, records TCP/HTTP status classes, compares every
  HTTP 200 with the durable Event count, stops its server in `finally` and removes
  only its fresh database.
- Added a bounded read-only MongoDB capacity report using server-side `$bsonSize` and
  `collStats`. It reports aggregate logical BSON, collection storage and index bytes
  without Event content or identifiers.
- Added configuration, operations, compatibility, capability, capacity and
  known-limit documentation plus CI verification for Rust, Vue, Node SDK, pinned CLI
  versions, compatibility claims and the container build.

## Verification completed

- Release contract profile: format, ADR-0034 dependency graph, strict all-feature
  clippy and complete workspace test suite pass.
- Retained fuzz regressions for bounded primitives, Envelope framing and structured
  normalization pass.
- Real `@sentry/node` 10.66.0 Error Event plus safe attachment passes.
- Real `@sentry/browser` 10.66.0 Error Event through Playwright Chromium passes.
- Real `sentry-go` 0.48.0 and Rust `sentry` 0.48.5 Error Events pass with pinned
  dependency graphs and finite process/flush deadlines.
- Real Python `sentry-sdk` 2.32.0, `sentry-java` 8.50.1 and .NET `Sentry` 6.7.0
  Error Events pass with pinned dependency/lock material and finite process/flush
  deadlines.
- The official Rust SDK's hyphenated envelope-header UUID is accepted only at the
  Sentry protocol boundary and remains a compact domain `EventId`.
- Base Node and Browser Error Events prove that no BlobStore object is created
  without an SDK attachment; the Node attachment is covered separately.
- Vue format/lint, ten unit tests and production build pass.
- `sentry-cli` 3.6.2 and 2.58.6 version contracts pass.
- A dedicated temporary MongoDB 8.3 instance with `enableTestCommands=1` passes all
  16 `infrastructure_` real-adapter/security/browser/orchestration tests.
- Cumulative project -> SDK -> Issue/API, Issue -> signed webhook and Event -> archive
  -> hot retention rungs pass.
- Finalizer acknowledged-step crash recovery and the real pinned `sentry-cli`
  debug/source artifact contract pass.
- Compose static validation and compatibility/capacity script syntax pass.
- The real multi-stage image builds as `faultkeep:phase22-smoke` from commit
  `96fc4b6`. Image manifest
  `sha256:9ec50b57c6c2111faf0fbd9108a180f6389161add279540890f724956ee0d100`
  passes embedded config and Web-bundle checks.
- A real container smoke against an isolated MongoDB 8.0.12 container passes
  `/live` and `/ready` with HTTP 200 while Faultkeep runs as non-root UID 999.
  Both smoke containers and their temporary network were removed.

## Performance evidence

Exactly two short local performance profiles were executed on AMD Ryzen 5 5600H,
15.9 GiB RAM, Windows, Rust 1.88.0, k6 2.1.0 and local MongoDB 8.3.7.

| Target | Achieved | p95 / p99 | Dropped | TCP / 429 / 503 / other | HTTP 200 = durable | Result |
|---|---:|---:|---:|---:|---:|---|
| 5,000/s for 15s | 4,983.16 RPS | 27.69 / 31.33 ms | 190 | 0 / 0 / 0 / 0 | 74,809 = 74,809 | correctness pass; zero-drop release threshold fail |
| 20,000/s for 15s | 7,307.42 RPS | 296.83 / 342.38 ms | 188,444 | 0 / 0 / 0 / 0 | 111,565 = 111,565 | saturation evidence; arrival/latency release gates fail |

Both runs prove zero acknowledged loss and zero duplicate durable identities for the
accepted responses. They do not pass the ADR-0037 controlled-hardware duration or
capacity gates and are not presented as production capacity.

A later capacity-evidence pass executed one additional 1,158/s for 10 seconds:
11,581 HTTP 200 responses equal 11,581 durable Events, with zero drops, TCP failures,
429, 503 or other responses; p95/p99 were 24.94/46.78 ms. The retained result is a
regression/capacity-seeding baseline, not a third release-load profile.

The resulting capacity report samples 10,000 of 11,581 Events and is marked
representative for the bounded synthetic Error fixture. Average BSON is 446.548
bytes, average observed index allocation is 129.802 bytes/Event, and fixed
collection/index allocation no longer dominates the sample. This fixture shape is
still not a substitute for a production payload distribution.

## ADR-0039 release gate

The product-owner closure scope accepts the retained short Windows RPS evidence and
bounded-resource/restart corpus without running new load or a long soak. This closes
the development release gate but does not claim ADR-0037 controlled-hardware
production capacity.

| Release row | Current evidence | Status |
|---|---|---|
| Zero lost acknowledged Events and duplicate identities | All three retained k6 artifacts have exact HTTP-200/durable equality and zero duplicate IDs | Pass |
| No unbounded queue/task/cardinality | Bounded queue/task/cardinality, restart, backlog-drain and enabled-module integration corpus passes; long production soak is explicitly deferred | Pass for selected scope |
| Security and tenant isolation | Real Mongo auth, project isolation, deletion, Web CSRF and adversarial suites pass | Pass |
| Compatibility rows match published claims | Required Python/Java/.NET real-process rows pass; Browser/Node/Go/Rust and CLI claims also have evidence; seven non-required rows remain `untested` | Pass |
| Performance identifies hardware and exposes overload | Retained artifacts identify hardware and include latency, dropped, TCP/200/429/503/other; no production-capacity claim is made | Pass for selected scope |
| Every collection/Blob namespace registered | Schema/deletion registry completeness test passes | Pass |
| Container packaging and smoke | Non-root image build, embedded config/Web checks and isolated Mongo `/live` + `/ready` smoke pass | Pass |
| No unresolved critical/high enabled defect | No critical/high defect remains in the enabled selected scope | Pass |

## Deferred non-blocking evidence

1. Add conformance rows before claiming Kotlin/Android, PHP, Ruby, Cocoa, React
   Native, Flutter/Dart or native C/C++ support.
2. Run controlled-hardware 5,000/s for 60 minutes, 20,000/s for 5 minutes and a long
   production-shaped enabled-addon soak before making a production-capacity claim.
3. Validate the representative synthetic capacity fixture against a real production
   payload distribution.

Phase 22 is complete for the explicitly selected version-one development release
scope. Milestone G is claimed without a production-capacity guarantee.
