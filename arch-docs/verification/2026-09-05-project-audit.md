# Project verification audit, 2026-09-05

Scope: repository quality gates, functional regression tests, real MongoDB adapters,
the 19-to-20 migration and crash recovery, real SDK compatibility, Web production
build and browser integration, documentation, and npm dependency advisories.
This is not a claim of a formal security review or a new capacity certification.

## Findings and corrections

- The notification E2E fixture omitted Telegram configuration and still selected a
  generic `Telegram` button. It now satisfies the API DTO type, selects the named
  recipient and checks the destination ID persisted in the alert rule.
- A Telegram destination without configuration was incorrectly labelled SMTP Email.
  The UI now falls back to its chat ID. Regression tests include absent/null
  configuration and a migrated configuration without identity snapshots.
- The real MongoDB dashboard test expected a legacy query representation and
  generation 14. It now verifies the documented v1-to-v2 expression normalization
  and compares the marker with the current runtime generation constant.
- Auth and live-browser fixtures created already-expired credentials using historic
  fixed clocks. Their fixed clocks now start at current wall time, avoiding races
  with MongoDB's independent TTL monitor.
- The real-browser script assumed the host locale and obsolete navigation/select
  controls. It now pins English and uses the current Settings/combobox controls.
  Its server fixture also wires the real dashboard services used after onboarding.
- The Python SDK fixture inherited the Windows system proxy for a loopback server
  and received HTTP 502 without reaching Metric. Proxy inheritance is disabled only
  in this test sender. Python and Go emit SDK diagnostics to stderr on failed gates.
- Compatible transitive dependency fixes were applied to npm lockfiles, without
  changing pinned SDK versions or forcing major upgrades.
- RustSec found the HTTP/2 empty-frame memory-exhaustion advisory
  [RUSTSEC-2026-0258](https://rustsec.org/advisories/RUSTSEC-2026-0258.html).
  Both Rust lockfiles now use the patched h2 0.4.16. The main workspace also replaces
  yanked chacha20 0.10.1 with compatible 0.10.2.
- The first expanded Linux CI run exposed Windows-only Sentry CLI executable paths
  in the retained debug-file/sourcemap test. The test now resolves the correct
  native executable through each pinned npm package's `getPath()` API, with a
  bounded resolver process and the existing native-process timeout preserved.
  Its cleanup fixture now checks that fresh blobs survive, then advances only the
  cleanup clock beyond the grace period instead of relying on a hard-coded future
  date. Other auth-bearing integration fixtures also use wall-relative timestamps.
- CI previously skipped the real MongoDB/migration gates. A dedicated integration
  job now runs them and the real-server scenarios sequentially; this also avoids
  interference from the intentionally injected MongoDB connection failure.
  Failed Web test traces are retained as CI artifacts.

No schema-generation change or new migration is introduced by these corrections.

## Local verification

Environment: Windows, Docker MongoDB 8.0.12 on port 27018, isolated per-test databases.
The user's system MongoDB on port 27017 was not used or modified.

| Gate | Result |
| --- | --- |
| Cargo formatting, ADR-0034 dependency graph, Clippy with warnings denied | Passed |
| Workspace/all-targets/all-features tests | 345 passed; 77 explicitly ignored by this command |
| Real MongoDB functional tests, including migration and checkpoint/lease recovery | 18 passed |
| Real-server integration, including login, ingest, native API, notifications, archive, Sentry CLI and live browser | 12 passed |
| Real SDK gates: Node, browser/replay, Python, Go, Java, .NET, Rust | 14 passed |
| Web Vitest | 108 passed in 33 files |
| Web Playwright, Chromium and Firefox | 34 passed; 2 reference-image generation cases skipped |
| Web formatting, lint, typecheck and production build | Passed |
| Browser/Node SDK fixture format/lint/build gates and pinned CLI versions | Passed |
| Java preparation; .NET restore/format/build; Go module verification/tests; Rust SDK build | Passed |
| Compatibility, deployment-profile and documentation validators; docs site build | Passed |

The 18 + 12 + 14 integration/SDK tests above are additional explicit executions of
tests ignored by the ordinary workspace command; ignored tests are not counted as
successes. The new GitHub Actions run must independently verify Linux, Node 24 and
Rust 1.88, container builds, deployment health checks and documentation publishing.

## Remaining boundaries

- Hardware-specific performance/load baselines and long soak/fuzz campaigns are
  not certified by this functional audit. Existing deterministic fuzz regressions
  remain part of the ordinary workspace tests.
- The external S3-compatible service matrix requires dedicated credentials and was
  not run; the self-contained S3 emulator tests run with the workspace suite.
- Screenshot-reference regeneration is deliberately opt-in, not a failing test.
- npm audit is clean for docs, Node SDK and Sentry CLI fixtures. Web and browser SDK
  fixtures retain the Svelte advisory group (moderate severity). npm proposes a
  Svelte 4-to-5 major upgrade; it was not forced into the rrweb playback stack.
  Compatibility and applicability of those advisories need a separate review.
- After patch updates, cargo-audit 0.22.2 reports zero entries in its vulnerability
  list. It still reports `paste` as unmaintained and the
  [lru panic-safety advisory](https://rustsec.org/advisories/RUSTSEC-2026-0253.html)
  as an informational `unsound` warning through pinned aws-sdk-s3 1.120.0. The
  reviewed S3 Express cache uses a String-backed key without a custom Drop and does
  not call the affected `pop()` method; the documented panic-on-key-drop trigger was
  not identified in that use. This is not a blanket safety guarantee for lru.
  Replacing the pinned AWS dependency stack was not forced into this regression fix.
- A passing test suite is evidence for covered behavior, not proof that every
  possible production input, migration interruption or deployment is fault-free.
