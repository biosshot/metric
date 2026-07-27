# Phase 34 report: Alerts and notification destinations

- Date: 2026-07-27
- Result: complete
- Governing decision: ADR-0045, amended by the owner to select Telegram and SMTP Email
- Module contract: `module-contracts/0034-alerts-notification-destinations-phase-34.md`

## Delivered

- Extended the Phase 20 Issue outbox with an atomic `resolved` transition while
  retaining new-Issue and regression delivery.
- Added Error/Log/Span count-threshold rules evaluated by bounded periodic work
  through the existing typed Explore service. Log/Span rules accept exact
  environment/release predicates; unsupported Error predicates fail before storage.
- Added persisted cooldown, previous-threshold state, recovery notification and
  hourly storm budget. Aggregate deliveries use the persisted scheduled window for
  restart-stable identities.
- Generalized the delivery port from webhook-specific to provider-neutral without
  changing its lease/retry/dead semantics. Existing Phase 20 webhook documents
  remain deliverable.
- Added Telegram Bot API delivery with dynamic HTML escaping, token-shape validation,
  bounded response reads and `429 retry_after` handling.
- Added user-configured SMTP Email with TLS/STARTTLS, authentication, bounded
  recipients, private-address policy checks and transport error classification.
- Bot tokens and SMTP passwords are sealed with the existing authenticated secret
  box. Native API responses expose only `has_secret`.
- Added ProjectAdmin native routes for destination/rule list/upsert, durable test
  sends and the latest 100 delivery-history records.
- Added `/alerts` Vue workflow with custom provider/rule/dataset/security selects,
  Telegram and SMTP forms, multi-destination rules, Issue and aggregate conditions,
  send-test controls, visible delivery state and responsive styling.
- Mongo schema generation 15 updates the three existing notification collections
  and adds the aggregate-due index. No new collection or migration layer was added.

## Exit gate

| Gate | Evidence |
| --- | --- |
| Repeated evaluation/restart cannot create unbounded duplicates | Issue identity remains transition/rule/destination. Aggregate claim persists the original scheduled window; retry after restart produces the same transition and delivery IDs, and Mongo uses `$setOnInsert`. |
| One alert targets multiple destination kinds without duplicate rule logic | Rules contain only destination IDs. Provider selection happens after one shared delivery claim and payload path. |
| Provider outage cannot block ingest, Processor or reads | All Telegram/SMTP I/O runs only in `ProviderDeliveryAdapter` after durable claim; aggregate evaluation is isolated behind Explore and its own bounded tick. |
| Telegram escaping/rate limit and SMTP TLS failures are tested | Unit tests cover HTML escaping/token rejection, a controlled Telegram 429 with 17-second retry, and a local SMTP endpoint that cannot complete TLS and is classified retryable/timeout. |
| Secrets never enter payload history, logs or API | Sealed secret types have redacted `Debug`; API DTOs emit `has_secret`; provider payloads contain only bounded alert summary fields. |
| Alert storms are bounded | Per-rule persisted hourly budget and cooldown bound emissions; dispatcher queue, worker concurrency and fair project/destination claiming bound global work. |
| Event and aggregate delivery traverse the provider-neutral outbox | Issue transition expansion and real-Mongo performance E2E cover durable fan-out; aggregate and test sends create the same delivery records consumed by Telegram/SMTP adapters. Controlled provider tests cover both adapters without external credentials. |

## Verification

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib
cargo test -p metric-server --test webhook_adapter
cargo test -p metric-server --test notifications_e2e --no-run

cd web
npm run lint
npm test
npm run build
```

One local-Mongo notification performance run was executed:

```text
transitions=300
expansion_rps=350.07
elapsed_ms=856
```

The benchmark database was dropped by the test. No k6 or external Telegram/SMTP
load was run because provider latency and rate limits are external to Faultkeep and
would not measure the durable local path.

## Next phase

Phase 35 Cron Monitoring is next. Phase 27 remains deferred. Web Push, new webhook
configuration, MCP, NATS, migrations, sharding and disk spool remain deferred.
