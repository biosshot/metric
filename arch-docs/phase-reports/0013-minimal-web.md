# Phase 13 report: Minimal Web

- Status: exit gate passed; Phase 14 not started
- Date: 2026-07-23
- Scope: ADR-0039 Phase 13 only
- Implementation commits: `7a2b1a7276f2d20baa6ecf9c289c3fa4086ed5d6`,
  `b3286f3062c9f196eddc6d3d701772ab0eda0ddd`

## Contract and implementation

The accepted contract is `module-contracts/0013-minimal-web-phase-13.md`.
`web/` is a Vue 3 and TypeScript client of the Phase 12 `/api/v1` boundary.
It has no MongoDB access, Rust application-service import, private server route,
or Sentry-compatible control-plane dependency. The Rust adapter serves only the
known SPA routes and assets with CSP, content-type and referrer protections;
unknown API routes keep their API 404 behavior.

The completed onboarding path is:

```text
first owner bootstrap
-> session login
-> POST /api/v1/projects with organization context and CSRF
-> authoritative project plus first DSN
-> refreshed project navigation
-> SDK setup
```

An organization administrator with no accessible project receives an explicit
creation form for project identity, pre-storage IP policy, accepted item types
and bounded ingest limits. A user without organization administration permission
receives a read-only explanation instead of a non-functional control. Project
creation reuses the existing Phase 12 command and does not add a Web-only bypass.

## Exit gate

| ADR-0039 Phase 13 gate | Evidence | Result |
| --- | --- | --- |
| Browser login/session/CSRF E2E | Chromium and Firefox verify login, HttpOnly cookie invisibility, per-tab CSRF, first-project creation and policy mutation | pass |
| Project isolation and permission-dependent controls | Browser fixtures reject a stale project selection, verify authoritative organization context, and hide lifecycle writes from a viewer | pass |
| Deterministic empty/error/loading/large-stack states | Playwright fixtures cover empty and delayed Issues, explicit 503 diagnostics, and a 120-frame Event initially bounded to 40 frames | pass |
| Key investigation flow on supported browsers | Both configured browsers traverse Issue -> lifecycle -> Event -> full stack and project settings | pass |
| Accessibility smoke and bounded rendering | Axe reports no serious/critical violations in the primary investigation view; the stack frame bound is asserted | pass |
| No path outside `/api/v1` | API client and browser route interception use only `/api/v1`; Rust static routing preserves unknown API 404 responses | pass |
| Cumulative browser -> project -> SDK rung | Real Chromium, Rust server and MongoDB create the project and DSN through HTTP, load SDK setup, save policy, then verify authoritative Mongo-backed state | pass |

## Verification

The final corrective gate passed:

```text
npm run format:check
npm run lint
npm test
npm run build
npm run test:e2e
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --quiet
cargo test -p faultkeep-server --test web_e2e \
  infrastructure_browser_login_session_csrf_and_project_isolation \
  -- --ignored --nocapture
```

Results:

- 9 Web unit/component tests passed;
- 12 Playwright scenarios passed across Chromium and Firefox;
- the real browser/Rust/MongoDB scenario passed;
- the production bundle contains 60.52 KiB gzip JavaScript and 4.67 KiB gzip
  CSS;
- no test server, Node, Chromium or Firefox process remained after the gate.

Web load and RPS testing is intentionally not a Phase 13 gate. Phase 12 retains
the native query latency baseline, while hot ingest and durable paths retain
their existing RPS baselines.

## Resource, cancellation, observability and errors

The Web adds no background task or server queue. Vue Query requests are bounded
by server page limits and opaque cursors. Stack traces render at most 40 frames
until the user explicitly requests the full bounded payload. Mutations disable
their submit control while in flight, and a tab without its CSRF token refuses
the mutation locally.

Failures preserve a safe explanation, stable error code, HTTP status and request
ID. Retry is offered only for network, rate-limit and server failures. The real
browser harness logs only failed API status and safe response envelopes; it does
not log credentials, cookies or request authorization headers. Component health
and enabled capabilities remain visible on `/system`; Phase 13 adds no competing
health owner or metric namespace.

## Known limits and deferred work

- The onboarding form creates the first accessible project. Additional project
  creation remains available through the typed Phase 12 API but is not a Phase 13
  administration screen.
- User invitations, membership administration and personal API-token management
  are not required Minimal Web screens.
- Retention is displayed as unavailable until Phase 14 implements and exposes
  the owning scheduler policy.
- Transactions, spans, profiles, replays, metrics, logs, MCP, NATS, sharding,
  disk spool and migrations remain outside the accepted scope.
- Web hosting is not split from the Rust application: the supported deployment
  remains the all-in-one server serving the production bundle.

Phase 14 may now start with Scheduler, retention, counters and narrow
reconciliation. No Phase 14 module was introduced by this corrective work.
