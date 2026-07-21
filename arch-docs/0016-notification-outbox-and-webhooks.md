# ADR-0016: Durable notification outbox and webhook delivery

- Status: Accepted
- Date: 2026-07-21

## Context

External notification systems can be slow, unavailable, or ambiguous about whether
they accepted a request. Processor must never wait for webhook or email delivery, and
a process failure between an Issue transition and creation of a delivery job must not
silently lose a new-Issue or regression notification.

MongoDB transactions remain disabled in the initial hot path. At the same time,
ordinary Issue activity history is only best-effort under ADR-0015 and therefore
cannot serve as a reliable notification handoff.

## Decision

### Initial triggers and suppression

The first version supports notification rules for exactly these transitions:

```text
new_issue
regression
```

It does not create a notification for every Event. Frequency, threshold, spike,
unique-user, release, and arbitrary query alerts are deferred until their windowing
and state models are designed.

An `ignored` Issue creates no notification transitions. Its Events and aggregates
continue to be stored as defined by ADR-0015.

### Issue-owned transition outbox

Creation of a new Issue or atomic reopening of a resolved Issue appends the compact
ADR-0024 transition to an outbox embedded in that same Issue document:

```javascript
{
  j: true,
  n: [
    {
      i, // 16-byte transition_id
      k, // numeric new_issue or regression kind
      e, // 16-byte event_id
      t  // created_at
    }
  ]
}
```

`transition_id` is a domain-separated BLAKE3-128 derivation from the project, Issue,
transition kind, and Event that caused it. The workflow change and outbox append are
one atomic MongoDB single-document update. No external call or notification-rule
evaluation occurs in Processor.

The outbox normally contains zero or one element. New-Issue occurs once, and another
regression for the same Issue requires a user to resolve it again. Outbox depth is
measured and workflow command rate limits prevent abusive state thrashing; transitions
are not silently dropped to enforce a small fixed array cap.

### Dispatcher expansion

A continuous `NotificationDispatcher` runs in the `all` process. A partial index on
compact `j == true` provides durable backlog discovery, while an in-memory
signal lets fresh transitions be considered without polling delay.

For each transition the Dispatcher:

1. loads the applicable enabled project rules;
2. expands every matching rule destination into one delivery;
3. idempotently upserts all delivery documents;
4. atomically removes the expanded transition from the Issue;
5. removes both `j` and `n` only when the embedded outbox is empty.

If no rule matches, the transition is still marked expanded and removed. If the
process stops before removal, deterministic delivery identifiers make the repeated
expansion harmless.

Each action delivery identifier is:

```text
delivery_id = BLAKE3(transition_id || rule_id || action_id)
```

One rule with multiple destinations therefore creates independently retryable
deliveries.

### Collections and logical models

The shared database adds:

```text
alert_rules
notification_destinations
notification_deliveries
```

An initial rule is project-scoped:

```javascript
{
  _id,
  project_id,
  name,
  enabled,
  triggers: ["new_issue", "regression"],
  destination_ids,
  created_at,
  updated_at
}
```

A delivery is a stable snapshot of the approved, scrubbed notification payload but
references its destination so secret material is not copied into every job:

```javascript
{
  _id,
  project_id,
  issue_id,
  transition_id,
  rule_id,
  action_id,
  destination_id,
  backend: "webhook",
  payload,
  status: "pending" | "delivered" | "dead",
  attempts,
  next_attempt_at,
  last_error,
  created_at,
  delivered_at,
  delete_at
}
```

Payloads contain only fields permitted by the post-scrubbing event model. Raw secrets,
authorization headers, and full unbounded Event payloads are not embedded.

### Delivery queue and first backend

The first delivery backend uses enum dispatch:

```rust
pub enum DeliveryBackend {
    Webhook(WebhookDelivery),
}
```

Email, Slack, Telegram, and other native integrations can become later variants. MCP
is a query and command interface rather than a push delivery backend; it may inspect
delivery history and request retries through the shared application services.

Delivery workers use a separately configurable bounded in-memory queue. MongoDB is
the durable delivery backlog. A full queue, unavailable destination, or process
restart does not block Ingest or Processor. Pending jobs are refilled by
`next_attempt_at` using the same durable-backlog principle as the Processor queue.

```toml
[notifications.queue]
capacity = 1000
```

The value is configurable and its production default remains benchmarkable.

### Delivery semantics and retries

External HTTP delivery is at-least-once, not exactly-once. A destination can process
a request while its response is lost; retrying is then the only safe sender action.
Every attempt includes the stable delivery identifier in `Idempotency-Key` and
`X-Delivery-Id` headers so a receiver can deduplicate it.

Initial configurable retry defaults are:

```toml
[notifications.retry]
max_attempts = 8
initial_delay = "5s"
max_delay = "1h"
timeout = "10s"
```

Backoff is exponential with jitter and approximates:

```text
5s -> 30s -> 2m -> 10m -> 30m -> 1h
```

Network errors, timeouts, HTTP 408, HTTP 429, and HTTP 5xx are retryable. A bounded
valid `Retry-After` value may move the next attempt later. Other HTTP 4xx responses
and rejected redirects are permanent for that attempt sequence. Exhausting the
configured attempts moves the delivery to `dead`; an authorized user may explicitly
retry it after correcting the destination.

A crash after the receiver accepted a request but before MongoDB records `delivered`
can cause a duplicate HTTP request. This is part of the documented at-least-once
contract.

### Webhook authentication and network safety

Webhook bodies are signed with HMAC-SHA256 over a versioned canonical signing input
that includes the delivery ID, attempt timestamp, and exact body bytes. The request
contains a timestamp and signature header to allow replay-window validation.

Destination secrets are stored encrypted and are resolved only by the delivery
adapter. Exact encryption-key management and rotation format require a later security
decision, but plaintext secrets in logs, payload snapshots, or delivery documents are
forbidden.

Outbound requests:

- use HTTPS by default;
- reject loopback, link-local, reserved, and private network destinations unless an
  administrator explicitly enables a restricted private-network mode;
- validate resolved addresses and every permitted redirect target to prevent DNS and
  redirect SSRF bypasses;
- disable redirects by default;
- bound connection and total time, response bytes, and diagnostic-body storage;
- never forward arbitrary inbound or internal authorization headers.

### Terminal retention

Terminal delivery documents receive an absolute `delete_at` used by a MongoDB TTL
index. Pending deliveries do not have `delete_at` and cannot expire before reaching a
terminal state.

```toml
[notifications.retention]
delivered_days = 30
dead_days = 90
```

Both values are configurable. Delivery history is operational evidence, not the
authoritative Event or Issue record.

## Consequences

- External network latency cannot slow ingestion or event processing.
- A new-Issue or regression transition survives a process failure without requiring
  a multi-document transaction.
- Dispatcher retries cannot create duplicate delivery documents.
- A receiver may still observe repeated HTTP requests and should honor the provided
  idempotency key.
- Only generic webhooks are implemented initially, while backend dispatch remains
  extensible without dynamic dispatch.
- Reliable transition handoff adds a small embedded outbox and a partial Issue index.

## Deferred questions

- Alert thresholds, rolling windows, spike detection, and query-based conditions.
- Email and native messaging integrations.
- Destination configuration revisions during an in-flight retry sequence.
- Encryption key storage, rotation, and secret migration format.
- Notification template customization and localization.
- Fairness and concurrency limits per project and destination.
