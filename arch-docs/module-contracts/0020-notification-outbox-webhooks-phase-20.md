# Phase 20 module contract: notification outbox and webhooks

Status: accepted implementation contract
Owner: Notification application module with MongoDB and HTTP adapters
Owning decision: ADR-0016
Sequential gate: ADR-0039 Phase 20

## Boundary

Issue persistence remains the only owner of `new_issue` and `regression` intent
creation. It appends the compact deterministic transition to the Issue document in
the same atomic update as creation or regression. Processor and Finalizer never load
rules, resolve secrets, or perform external I/O. Successful Finalizer work emits only
an in-memory wake signal; the durable partial-index scan remains authoritative after
a missed signal or restart.

`NotificationDispatcher` owns transition expansion, bounded due work and retry
classification. `NotificationStore` exposes domain operations rather than MongoDB
filters. `WebhookDeliveryAdapter` owns URL/DNS validation, secret decryption, exact
body signing and HTTP limits. Administrative rule/destination mutations pass through
`NotificationAdminService`, require `project:admin`, and append allowlisted audits.

## Durable contract

- Supported triggers are exactly `new_issue` and `regression`; ignored Issues do not
  create transitions.
- Delivery identity is BLAKE3-128 over the transition, rule and action/destination
  identities. Expansion upserts every delivery before removing the Issue transition.
- Repeating any crash window may repeat an upsert or external request but cannot
  create a second delivery document.
- Pending work has no TTL. `delivered` and `dead` documents receive absolute TTL
  timestamps. Delivery history is operational evidence, not an Event/Issue authority.
- Claims atomically increment attempts and move `next_attempt_at` past the attempt
  lease. A crashed claim becomes due again. The adapter sends stable
  `Idempotency-Key` and `X-Delivery-Id` headers.
- Delivery payload version 1 is a bounded allowlist containing transition, project,
  Issue, Event, title, rule and destination identifiers. It contains no Event body,
  inbound authorization headers, plaintext webhook secret, attachment, source or
  debug data.

## Limits and cancellation

- Queue capacity: configurable, default 1,000, hard maximum 100,000.
- Workers: default 8, at most 1,024 and never above queue capacity.
- Transition batch: default 100, maximum 10,000.
- Due fairness scan: default 100, maximum 1,000. Selection rotates away from the last
  destination when another due destination is present.
- Rule expansion: at most 256 matching rules per transition and 32 destinations per
  rule. Payload is at most 16 KiB.
- Attempt timeout: default 10 seconds; claim lease must be strictly longer and defaults
  to 30 seconds. Redirects are disabled.
- Response diagnostic read: default 64 KiB, hard maximum 1 MiB; excess bytes are
  discarded and never stored.
- Shutdown stops polling and workers. Already claimed but unfinished work stays
  pending behind its finite lease and is recovered from MongoDB after restart.

## Retry and network policy

HTTP 408, 429 and 5xx, timeouts and network failures retry. Other 3xx/4xx statuses,
invalid/disabled destinations, SSRF rejection and secret-authentication failure are
permanent. The default maximum is eight external attempts. Backoff with deterministic
bounded jitter approximates `5s, 30s, 2m, 10m, 30m, 1h`; a valid bounded
`Retry-After` can only move the attempt later. Exhaustion becomes `dead`.

HTTPS is required by default. Literal and resolved loopback, private, link-local,
unspecified, multicast, documentation and reserved addresses are rejected unless the
administrator enables restricted private-network mode. Every request pins an already
validated resolved address, disables redirects, bounds total time and response bytes,
and never forwards inbound headers. Webhook secrets use ChaCha20-Poly1305 with a
domain-separated key derived from the configured 32-byte project scrub master key;
only authenticated ciphertext is stored and Debug output is redacted.

## Stable application errors

| Code | Meaning |
|---|---|
| `notification_invalid_configuration` | configured bounds are invalid |
| `notification_temporarily_unavailable` | durable storage/auth dependency is unavailable |
| `notification_invalid_data` | a bounded domain or stored representation is invalid |
| `notification_forbidden` | project administration was not authorized |

Per-delivery terminal diagnostics use a closed allowlist such as `http_rejected`,
`http_retryable`, `network_error`, `timeout`, `ssrf_rejected`, `invalid_secret`, and
`attempts_exhausted`. Endpoint, secret, response body and arbitrary remote text are
never diagnostic labels or logs.

## Observability and health

The module emits transition backlog observations, expanded-transition totals and
delivery-attempt totals with the closed outcomes `delivered`, `retry`, `dead`, and
`exhausted`. The all-role readiness gate includes the notification task. Safe
identifiers may be logged; endpoint URLs, ciphertext/plaintext secrets, payloads and
remote bodies may not.

## Deliberately deferred

Threshold/window/spike/query alerts, email and native chat backends, custom templates,
secret-key rotation/migration, distributed destination fairness, MCP, NATS, split
roles and online MongoDB migrations remain outside Phase 20.
