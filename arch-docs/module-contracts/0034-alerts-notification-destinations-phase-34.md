# Phase 34 module contract: Alerts and notification destinations

## Ownership

- `metric-domain::notifications` owns bounded rule, destination, SMTP and delivery
  values. Secrets are opaque sealed bytes and have redacted `Debug`.
- `metric-application::notifications` owns Issue transition expansion, aggregate
  evaluation through Explore, cooldown/storm decisions, deterministic delivery
  identity, leases, retry and terminal history.
- `metric-ports::NotificationStore` owns durable claims and idempotent upserts.
- `metric-mongo::notifications` owns the three existing notification collections,
  compact tagged codecs, strict validators and due/history/TTL indexes.
- Server provider adapters own Telegram HTTP and SMTP TLS I/O after a delivery is
  claimed. Native API and Vue own ProjectAdmin configuration, test sends and
  redacted history.

## Signal and delivery boundary

```text
Issue new/regression/resolved -> embedded Issue outbox
Explore Error/Log/Span count -> bounded aggregate evaluator
  -> deterministic notification_deliveries upsert
  -> lease/claim
  -> Telegram or SMTP Email adapter
  -> delivered / retry / dead history
```

No provider request runs in ingest, Processor, writers, Finalizer or Issue reads.
Aggregate evaluation uses the existing Explore planner/reservation and claims at
most the configured bounded batch per tick.

## Idempotence and storm control

Issue delivery identity is `(transition, rule, destination)`. Aggregate identity is
`(rule, persisted scheduled window, threshold/recovery action, destination)`.
MongoDB stores the pre-claim scheduled window before moving the due time to a lease,
so restart after delivery upserts repeats the same identities.

Rules store a bounded cooldown, hourly storm budget, last-fired time, current storm
window/count and prior threshold state. Recovery is emitted only on a true-to-false
threshold transition. Dispatcher queue/concurrency and fair project/destination
claiming remain the global isolation boundary.

## Provider and secret boundary

Phase 34 exposes only Telegram and SMTP Email configuration. Phase 20 webhook
records remain readable/deliverable for backward compatibility but are not exposed
as a new Phase 34 destination.

Telegram always uses the fixed Bot API base in production, HTML-escapes dynamic
content, bounds response bytes and honors bounded `retry_after`. SMTP permits only
implicit TLS or STARTTLS, resolves and rejects forbidden addresses under the shared
private-network policy, bounds recipients to 16 and classifies permanent,
retryable and timeout failures.

Bot tokens and SMTP passwords use the existing ChaCha20-Poly1305 secret box. API
responses return only `has_secret`; payload history contains bounded alert summary
data and never credentials.

## Storage and schema

Phase 34 reuses `alert_rules`, `notification_destinations` and
`notification_deliveries`. Schema generation 15 is an intentional breaking
empty-schema generation; no migration framework is introduced.

## Explicit exclusions

Web Push, new webhook configuration, provider-specific chat/issue trackers, MCP,
NATS, migrations, sharding and disk spool remain deferred. Phase 35 is not started.
