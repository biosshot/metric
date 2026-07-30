# Phase 20 report: notification outbox and webhook delivery

> Historical test evidence, not an upgrade runbook. Its fresh-database language
> describes that phase's development environment only. The current binary requires
> generation 19; preserve data-bearing databases and follow
> [the current upgrade runbook](../../docs/upgrading.md).

Status: complete
Date: 2026-07-24
Contract: `arch-docs/module-contracts/0020-notification-outbox-webhooks-phase-20.md`

## Implemented

- Preserved the Issue-owned atomic `new_issue`/`regression` intents introduced by
  Phase 9 and connected Finalizer success to a non-durable wake signal. MongoDB
  partial-index discovery remains the recovery authority.
- Added bounded notification domain values and explicit `NotificationStore` /
  `WebhookDeliveryAdapter` ports.
- Added validated `alert_rules`, `notification_destinations` and
  `notification_deliveries` collections, due/history/TTL indexes, schema generation
  6 registration and project-deletion classifications.
- Expansion loads only enabled matching rules, creates deterministic BLAKE3 delivery
  IDs, upserts all actions, then removes the embedded transition. A repeated
  post-upsert crash window is harmless.
- Added a bounded due queue, finite claim leases, attempt accounting, project and
  destination rotation, restart recovery and terminal retention.
- Added configurable retry classification and deterministic bounded jitter around the
  ADR-0016 schedule. `Retry-After` can move an attempt later but never bypass the
  default delay or maximum.
- Added the generic reqwest webhook adapter: no redirects, pinned validated DNS
  address, HTTPS by default, literal/resolved SSRF denial, total timeout, bounded
  response consumption and no inbound/internal header forwarding.
- Added canonical HMAC-SHA256 request signing, stable delivery headers and
  ChaCha20-Poly1305 destination-secret storage. The encryption key is
  domain-separated from the configured 32-byte project scrub master key; plaintext
  and endpoint values are redacted from Debug output.
- Added a typed `project:admin` rule/destination mutation boundary and allowlisted
  audit actions. No raw Mongo filter or secret-bearing log surface was added.
- Added redacted typed `[notifications]` configuration, all-role task composition,
  readiness/component status and capability discovery.

Stable application errors are:

```text
notification_invalid_configuration
notification_temporarily_unavailable
notification_invalid_data
notification_forbidden
```

## Limits and cancellation

The production defaults are a 1,000-item queue, eight workers, 100-transition
expansion batches, a 100-document fairness scan, 10-second attempts, a 30-second
claim lease and eight external attempts. Rule fan-out is capped at 32 destinations,
payloads at 16 KiB and response diagnostics at 64 KiB. Delivered history is retained
30 days and dead history 90 days.

Shutdown stops new polling and worker acquisition. Work already claimed but not
completed stays durable and becomes due after its finite lease. Ingestion, Processor
and Finalizer never await external HTTP.

## Correctness, integration and adversarial results

- Domain ID, redaction and payload golden tests: pass.
- Retryable/permanent HTTP matrix and bounded fake-clock jitter tests: pass.
- Stable HMAC vector and authenticated-encryption corruption tests: pass.
- Literal/resolved SSRF corpus for loopback, private, link-local, reserved and IPv6
  addresses: pass.
- Controlled webhook receiver: redirects are not followed, bounded `Retry-After` is
  parsed, oversized response bodies are discarded at the cap and timeout is
  retryable: pass.
- Project/destination fairness rotation: pass.
- Real local MongoDB cumulative E2E:
  `Issue new_issue intent -> rule expansion -> one deterministic delivery -> signed
  controlled webhook -> delivered history`: pass.
- The E2E repeats the post-upsert/pre-removal crash window and verifies one delivery
  document, no remaining Issue outbox marker and valid signature/idempotency headers.
- Workspace dependency graph: pass.
- `cargo test --workspace --all-targets`: pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass.

## Performance baseline

Exactly one Phase 20 performance run was executed. The release runner used local
MongoDB 8.3.7 on an AMD Ryzen 5 5600H, Rust 1.88.0 and 15.9 GiB RAM. Setup created
300 distinct Issue-owned `new_issue` intents; timing included matching-rule lookup,
deterministic delivery upsert and transition removal in batches of 100.

```text
transitions: 300
elapsed: 698 ms
expansion RPS: 429.74
local gate: >= 50 RPS
20% regression comparator: pass
```

The artifact is
`performance/baselines/notifications/ryzen-5600h-windows-v1.json`. This is a local
durable-expansion regression sentinel, not an external webhook capacity promise.
k6 was not used because Phase 20 has no public notification ingress and receiver
latency is outside the durable expansion boundary.

After the run, scoped Node/Cargo/Rust processes exited. No Metric server, k6 or
notification test process remained; the user's MongoDB process was not stopped.

## Metrics, health and safe diagnostics

The module reports transition backlog observations, expanded transitions and
delivery attempts with the closed outcomes `delivered`, `retry`, `dead` and
`exhausted`. Readiness now requires the notification task and component status
reports it explicitly. Stored/loggable terminal codes are bounded and never contain
URL, response body, payload or secret text.

## Known limits and deferred work

- External delivery is intentionally at-least-once. Receivers must deduplicate the
  stable delivery ID.
- Thresholds, rolling windows, spike/query alerts, custom templates, email/chat
  backends, key rotation/migration and distributed fairness remain deferred.
- Rule/destination commands are an authorized application boundary; a dedicated Web
  settings screen is not part of Phase 20.
- Online migrations remain prohibited by ADR-0039. Schema generation 5 databases are
  not modified in place; this development build requires a fresh empty generation 6
  database (or a separately accepted future migration decision).
- MCP, NATS, split roles, sharding and disk spool were not added. Phase 21 was not
  started.
