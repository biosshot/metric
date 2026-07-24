# Phase 22 report: full-system verification and packaging

Status: in progress
Date: 2026-07-24
Contract: `arch-docs/module-contracts/0022-release-hardening-phase-22.md`

## Implemented in this pass

- Consolidated SDK and CLI claims into one version-2 machine-readable matrix. Every
  ADR-0036 family is present, and an executable Python validator prevents a `pass`
  row without exact version/runtime, fixture/test evidence and a passing build.
- Preserved honest support: Node and captured Python Error Events plus pinned
  `sentry-cli` rows pass; twelve SDK families remain `untested`.
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
- Vue format/lint, ten unit tests and production build pass.
- `sentry-cli` 3.6.2 and 2.58.6 version contracts pass.
- A dedicated temporary MongoDB 8.3 instance with `enableTestCommands=1` passes all
  16 `infrastructure_` real-adapter/security/browser/orchestration tests.
- Cumulative project -> SDK -> Issue/API, Issue -> signed webhook and Event -> archive
  -> hot retention rungs pass.
- Finalizer acknowledged-step crash recovery and the real pinned `sentry-cli`
  debug/source artifact contract pass.
- Compose static validation and compatibility/capacity script syntax pass.
- Docker image build is not locally executed because Docker Desktop daemon is not
  running. CI owns an explicit image-build job.

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

The local capacity report sampled all eight existing Events: average BSON is
2,048.25 bytes. It is deliberately marked non-representative because fixed
collection/index allocation dominates a dataset this small.

## ADR-0039 release gate

| Release row | Current evidence | Status |
|---|---|---|
| Zero lost acknowledged Events and duplicate identities | Both k6 profiles have exact HTTP-200/durable equality and zero duplicate IDs | Pass for executed profiles |
| No unbounded queue/task/cardinality in soak | Module bounds pass; long enabled-addon soak not executed | Blocked |
| Security and tenant isolation | Real Mongo auth, project isolation, deletion, Web CSRF and adversarial suites pass | Pass |
| Compatibility rows match published claims | Passing Node/Python/CLI rows have evidence; twelve required SDK families remain untested | Blocked |
| Performance identifies hardware and exposes overload | Artifacts include hardware, latency, dropped and TCP/200/429/503/other; 20k gate fails | Blocked |
| Every collection/Blob namespace registered | Schema/deletion registry completeness test passes | Pass |
| No unresolved critical/high enabled defect | No critical/high defect found in executed suites; full release matrix is incomplete | In progress |

## Remaining work before Phase 22 completion

1. Add and pass real-process/captured conformance harnesses for browser JavaScript,
   Java, Kotlin/Android, .NET, Go, Rust, PHP, Ruby, Cocoa, React Native, Flutter/Dart
   and native C/C++.
2. Run controlled-hardware 5,000/s for 60 minutes and 20,000/s for 5 minutes with a
   generator that does not saturate first.
3. Run backlog recovery/restart and long enabled-addon soak with retention,
   Scheduler, Web queries, archive and notification work.
4. Capture a production-shaped capacity dataset where fixed index allocation does
   not dominate.
5. Build and smoke the container image locally or in CI and attach that evidence.

Phase 22 is intentionally not marked complete and Milestone G is not claimed.
