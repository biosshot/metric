# Phase 13 contract: Minimal Web

Status: implemented and verified by the completed Phase 13 exit gate.

## Boundary

`web/` is a Vue 3 and TypeScript client of the stable native `/api/v1` HTTP
contract from ADR-0036. It has no MongoDB dependency, server-side application
service import, Sentry-compatible `/api/0` dependency, or private bypass.

The Rust server serves the production build from `web/dist` (or the explicit
`METRIC_WEB_DIR`) on known SPA routes only. Unknown API paths are not
rewritten to `index.html`.

## Session and mutation rules

- The HttpOnly session cookie remains owned by the server.
- The current organization is sent through `x-metric-organization-id`.
- The login response CSRF token is retained in origin-scoped `localStorage` for
  the lifetime of the HttpOnly session cookie. This allows a browser restart or
  a new same-origin tab to keep performing protected mutations without exposing
  the opaque session identifier.
- Legacy per-tab `sessionStorage` CSRF state is migrated once and removed.
- A cookie session is not restored into the authenticated UI when matching CSRF
  state is absent. The UI requires a new login instead of creating an unusable
  read-only half-session or sending an unsafe request.
- Personal API tokens are never stored by the Web client.
- Permission-dependent controls improve clarity but do not replace server-side
  authorization.

## Initial routes

- `/`: login and one-time bootstrap;
- `/issues`: first-project onboarding when the organization has no accessible
  project; otherwise Issue list, status filtering and bounded Event Search v1;
- `/issues/{issue_id}`: Issue statistics, activity, events and lifecycle;
- `/events/{event_id}`: exact derived body, bounded stack rendering and raw
  normalized Event;
- `/project/setup`: active DSNs and official SDK setup guidance;
- `/project/settings`: PII policy, accepted item types, ingestion limits and
  DSN key administration;
- `/system`: authenticated component state and build capabilities.

Retention is shown as unavailable until Phase 14 owns and exposes that policy.
The UI does not render a placeholder control that implies a value was saved.

## Error contract

Every failed API view exposes:

- a safe human explanation;
- stable error code;
- HTTP status when available;
- server `request_id` when available;
- an explicit retry only for network, rate-limit, or server failures.

Malformed non-JSON proxy failures retain their HTTP status and are not reduced
to an unspecified client error.

## Bounded rendering

Stack traces initially render at most 40 frames and state the complete frame
count. A user can explicitly render all frames. Search remains bounded by the
server limits and opaque keyset cursors are passed through without decoding.

## Verification

- API client unit tests cover organization context, cookie credentials, CSRF
  refusal and complete error diagnostics.
- Component tests cover retry presentation and a 120-frame stack.
- Playwright covers Chromium and Firefox session-cookie behavior, CSRF
  mutation, permission-dependent controls, deterministic loading/empty/error
  states, the Issue-to-Event investigation path, bounded large-stack rendering,
  and accessibility smoke checks.
- Rust tests cover known SPA routing, CSP/security headers, and preservation of
  unknown API 404 responses.
- A real browser/Rust/MongoDB integration creates the first project and DSN
  through `/api/v1`, verifies the HttpOnly session and CSRF mutation, and checks
  the resulting project policy through the authoritative application service.

Web load or RPS testing is not a Phase 13 gate. Server query baselines remain
owned by Phase 12.
