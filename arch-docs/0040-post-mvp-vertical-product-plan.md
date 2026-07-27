# ADR-0040: Post-MVP vertical product plan

- Status: Accepted
- Date: 2026-07-24
- Supersedes: the post-MVP exclusions of ADR-0039 only

## Objective

Extend the completed Error Monitoring MVP toward broad Sentry product compatibility
without turning every new signal into a separate application architecture and without
making the existing Error path pay for unused fields, indexes, queues, or processors.

Each capability is implemented as a complete vertical slice:

```text
Sentry SDK item
-> bounded protocol parser
-> shared authentication, limits and PII boundary
-> typed accepted record
-> signal-specific bounded lane
-> signal-specific processor
-> typed MongoDB collection and optional BlobStore objects
-> retention/archive/search
-> native API and Web
-> compatibility, fault, load and cumulative E2E gates
```

The initial deployment remains one Rust process in `--role=all`. Separate lanes and
collections are resource-isolation boundaries inside that process, not services and
not an inter-role protocol.

## Current execution status

Status as of 2026-07-27:

| Phase | Capability | Status |
| ---: | --- | --- |
| 23 | Dark Web redesign | Complete |
| 24 | Structured Logs | Complete |
| 25 | Transactions, Spans and Traces | Complete |
| 26 | Performance Insights | Complete |
| 27 | Production readiness program (ADR-0044) | Accepted, execution deferred |
| 28 | Signal Inbound Filters | Complete |
| 29 | Releases and Deploys | Complete |
| 30 | Sessions and Release Health | Complete |
| 31 | User Feedback | Complete |
| 32-33 | Lightweight product wave | Complete |
| 34-36 | Lightweight product wave | Planned in ADR-0045 |

Phase 23 evidence is published in
`arch-docs/phase-reports/0023-dark-monochrome-web.md`. Phase 24-26 implementation
evidence is published in the corresponding reports under `arch-docs/phase-reports/`.
Phase 28-33 evidence is published in their corresponding reports; Phase 34 is next.

ADR-0044 originally paused product ordering after Phase 26. The 2026-07-26 owner
decision deferred execution of Phase 27 without completing it. ADR-0045 now owns the
selected Phase 28-36 ordering; all unselected items below remain unnumbered backlog.

## Accepted direction

1. One MongoDB database contains a bounded set of collections by workload class.
   Databases and collections are never created per project.
2. Error Events, Logs, Spans, metric buckets, Profiles and Replays do not share one
   physical collection.
3. Shared code is reused through typed records and ports. Generation 8 uses
   `Arc<dyn LogSink>`, `Arc<dyn SpanSink>` and `Arc<dyn SignalStore>` at
   application/composition boundaries; bounded writer loops and Mongo codecs remain
   signal-specific. ADR-0044 measures this before any speculative dispatch rewrite.
4. Each high-volume signal owns a bounded RAM lane and micro-batcher. A Log flood
   cannot consume the Error queue.
5. Signals with asynchronous post-acknowledgement work use a durable pending record
   as their fallback and recovery source. Terminal Logs and Spans instead return
   success only after their bounded writer has durably stored the complete batch;
   lane saturation or dependency failure produces an explicit retryable response.
6. Micro-batching combines MongoDB operations, not logical records. One Log and one
   Span remain individually addressable records.
7. Transactions are root Spans and live in `spans`; there is no separate
   `transactions` collection.
8. Large Replay/Profile/attachment payloads live in BlobStore. MongoDB contains
   bounded searchable metadata and immutable object references.
9. Metrics are stored as bounded time buckets, not one MongoDB document per raw
   measurement.
10. Retention, archive eligibility, quotas, indexes and failure policy are
    signal-specific.
11. Sentry SDK wire compatibility is implemented first. OpenTelemetry ingestion is
    deliberately deferred.
12. AI analysis and automatic fixes are deliberately deferred.
13. MCP remains a later adapter over accepted application services; it does not
    define domain or storage contracts.
14. Every existing and future Web surface follows the ADR-0041 dark-neutral minimal
    design system with only its approved muted semantic/syntax accents.

## Naming the current Event collection

Before Phase 24, the `events` collection contained occurrences of errors, while
`issues` contained their groups. Once other signal types existed, the physical name
`events` became ambiguous.

Phase 24 performs a deliberate breaking physical rename to:

```text
error_events
```

There is no migration, collection alias, dual-read, dual-write or automatic rename.
Schema generation 8 and all runtime/test adapters use `error_events` exclusively.
An existing generation-7 database is intentionally incompatible and must be dropped
or recreated by its operator. Data in a legacy `events` collection is intentionally
not imported.

This owner-directed breaking decision supersedes the earlier conditional compatibility
language in this ADR. Existing native API routes keep `/events` because they expose
Error occurrences belonging to an Issue; the route name does not select a MongoDB
collection.

## Physical collection map

Collections are created only when their owning phase is enabled:

| Collection | Purpose | Primary write shape |
| --- | --- | --- |
| `error_events` | individual Error Events and processing state | insert, then bounded finalization |
| `issues` | Error groups and lifecycle | aggregate updates |
| `issue_stats_hourly` | rebuildable Error aggregates | bucket updates |
| `logs` | structured Logs | bounded terminal batch insert |
| `spans` | root transactions and child Spans | bounded terminal batch insert |
| `span_stats_hourly` | rebuildable performance aggregates | bucket updates |
| `metric_buckets` | counters, gauges and distributions | bounded bucket merge |
| `profiles` | searchable Profile metadata and Blob references | append/finalize |
| `replays` | Replay session metadata and Blob references | append/session finalize |
| `sessions` | bounded release-health session state | upsert/terminal transition |
| `session_stats_hourly` | rebuildable release-health aggregates | bucket updates |
| `feedback` | user feedback linked to project/Event/Issue | append and workflow updates |
| `monitors` | Cron monitor definitions and current state | CRUD/state transition |
| `check_ins` | immutable Cron executions | append |
| `uptime_monitors` | active HTTP monitor definitions | CRUD/state transition |
| `uptime_results` | immutable bounded check results | append |
| `dashboards` | saved dashboard definitions | low-volume CRUD |
| `alert_rules` | issue/query alert definitions and state | low-volume CRUD/state |
| `releases` | release lifecycle | low-volume CRUD |
| `deploys` | release deployments | append |
| `repositories` | source integration metadata | low-volume CRUD |
| `commits` | bounded release commit metadata | append/upsert |

This map is not permission to pre-create unused collections or adapters.

## Shared vertical extension contract

### Accepted signal records

The Error path continues to use `AcceptedEvent`. The shared Envelope parser carries
non-Error JSON through an exhaustive accepted kind:

```rust
enum PendingSignalKind {
    Log,
    Transaction,
    Span,
}
```

Ingest validates/scrubs each enabled kind into typed `LogRecord` or `SpanRecord`
values before calling its typed sink. Future kinds are added only in their owning
selected backlog phase. Disabled known Envelope items continue to use the accepted
partial-discard behavior.

### Routing and lanes

The shared ingest service performs an exhaustive `match` and sends typed records to
the independent Log or Span sink. A lane owns:

- channel document capacity and a derived byte ceiling from bounded record size;
- micro-batch `max_wait`, `max_documents` and `max_bytes`;
- one in-flight batch per current writer task;
- retry and permanent-failure policy;
- metrics and readiness contribution.

Foreground/backlog scheduling weight applies only to a future signal that actually
accepts asynchronous durable pending work; terminal Log/Span writers have no backlog
queue.

The lane implementation may be generic and monomorphized. Signal-specific policies
are static typed configuration, not a generic runtime workflow engine.

### Processing

The shared dispatcher and cancellation framework are reused, but processing remains
typed:

```text
ErrorProcessor   -> normalize, symbolicate, group, finalize Issue
LogWriter        -> batch prevalidated terminal Log records durably
SpanWriter       -> batch prevalidated terminal Span records durably
MongoSignalStore -> best-effort rebuildable bucket update after a new root Span
ProfileProcessor -> validate metadata, commit Blob reference, finalize
ReplayProcessor  -> validate segment/session metadata, commit Blob reference
```

No empty processor hook, optional mega-context or signal branch is added to the Error
hot loop merely to claim reuse.

### Storage

The MongoDB adapter owns signal-specific codecs and collection/query implementations.
Log and Span writer tasks call their typed sink ports independently; there is no
mixed-signal MongoDB batch. Application services never construct collection names or
MongoDB query documents.

The first implementation uses ordinary MongoDB collections. A future benchmark may
justify MongoDB time-series storage for `logs`, `spans` or `uptime_results`; that
physical decision stays inside the adapter and must pass the owning port conformance
suite.

### Cross-signal correlation

Shared value types are added only when a real vertical capability needs them:

- project ID;
- occurrence and receive timestamps;
- environment and release;
- trace ID, span ID and parent span ID;
- user/session identity after PII policy;
- Blob reference;
- source SDK identity.

Correlation IDs use bounded binary representations. Their absence is represented by
an omitted BSON field, never BSON `null`.

### Extension checklist

Every new signal phase must explicitly provide:

1. supported Sentry Envelope item types and exact fixture versions;
2. parser limits before allocation or durable side effects;
3. PII classification and scrub behavior;
4. accepted domain value and stable errors;
5. queue, byte and concurrency budgets;
6. durable identity and idempotency rule;
7. MongoDB codec, validator, indexes and byte-budget fixtures;
8. retry, ambiguous-response and permanent-failure behavior, plus durable backlog
   recovery only when the signal actually has asynchronous work;
9. retention, quota, deletion and archive registration;
10. search fields and query cost limits;
11. API authorization and cursor pagination;
12. minimal useful Web investigation flow;
13. real SDK E2E row and capability advertisement;
14. unit/property/fuzz tests selected by risk;
15. load, burst, restart and dependency-failure results, plus backlog recovery for
   asynchronous signals;
16. confirmation that Error ingest and investigation baselines did not regress.
17. ADR-0041 visual, accessibility and built-asset-size gates for every Web change.

## Stage H: dark monochrome product identity

### Phase 23 — Dark monochrome Web redesign

The existing MVP Web predates ADR-0041 and uses a purple/color visual system. Phase 23
migrates the complete existing interface to the accepted dark monochrome design before
Logs or any other product adds new shared components.

Implement:

- the ADR-0041 dark neutral token palette as the only initial theme;
- migrate every existing screen and primitive;
- remove gradients, decorative shadows, remote/decorative asset pressure and
  color-only states;
- replace colored status meaning with explicit labels, icon/shape, border and
  luminance;
- update navigation, forms, buttons, badges, alerts, stack traces, tables, timelines,
  authentication and system-status views;
- preserve a small system-font/local-icon dependency surface;
- centralize tokens and reusable primitives before Logs-specific components exist;
- add reference renders for every route at desktop and narrow widths.

Phase 23 changes presentation only. It does not change DTOs, API routes, permissions,
storage or application behavior.

Exit gate:

- all existing routes pass visual review with the ADR-0041 dark-neutral base;
- no saturated decorative palette, gradient or color-only state remains; approved
  muted semantic/syntax accents stay subordinate to labels, icons and luminance;
- keyboard navigation, visible focus, accepted contrast and reduced motion pass;
- grayscale screenshot/print review preserves every semantic state;
- production CSS/JS asset sizes and their delta are published;
- existing Web unit/E2E behavior remains unchanged.

## Stage I: high-volume observability signals

### Phase 24 — Structured Logs end to end

This phase creates the reusable typed-lane extension while delivering a complete
user-visible feature. There is no separate horizontal framework phase. ADR-0042
defines the accepted compact `logs` BSON model, synchronous terminal durability,
indexes and query limits.

Implement:

- accepted Sentry SDK Log Envelope fixtures and capability flag;
- `PendingSignalKind::Log` -> `LogRecord`, a dedicated bounded `LogWriter` and Log
  lane;
- ADR-0042 typed `logs` codec, validator and indexes;
- compact severity, timestamps, message/body, environment, release, SDK and bounded
  attributes;
- optional trace/span correlation when supplied by the SDK;
- configurable Log retention, project item enablement and shared bounded
  request/rate limits;
- bounded message search, time/level/environment/release/service/Trace filters and
  cursor pagination;
- Logs list/detail Web views, current-page severity distribution and links to
  correlated Error/Trace data;
- ingest outcome accounting for accepted, disabled, capacity and storage failures;
- project-deletion registration.

Phase 24 also performs the breaking `events` to `error_events` physical rename defined
above.

Exit gate:

- official SDK fixtures preserve supported structured values and correlation IDs;
- one accepted Log becomes one terminal durable record; retrying the same formed
  writer record is idempotent, external SDK redelivery is explicitly at least once,
  and restart loses no acknowledged Log;
- a saturated Log lane neither consumes Error queue capacity nor delays Error
  acknowledgement beyond the published regression budget;
- BSON and index cost is pinned for representative Logs;
- the amended retained writer/mixed regression profiles below pass; production
  sustained/soak/search evidence moves to ADR-0044;
- the browser can send, find, filter and inspect a real SDK Log.

### Phase 25 — Transactions, Spans and basic Trace investigation

ADR-0043 defines Transactions as root/segment Spans, the compact `spans` collection,
deterministic identities, idempotent child expansion and bounded virtual Trace
assembly.

Implement:

- supported Sentry transaction and standalone span Envelope items;
- bounded binary trace/span identifiers and validation;
- transaction normalization into a root `SpanRecord`;
- `PendingSignalKind::{Transaction, Span}` -> `SpanRecord`, dedicated bounded
  `SpanWriter` and `spans`;
- parent-child relationships, operation, status, duration, service, environment,
  release and bounded attributes;
- trace sampling metadata accepted from Sentry SDKs without implementing
  OpenTelemetry;
- trace-by-ID retrieval and deterministic parent/child assembly;
- a minimal Trace Web view with Errors and Logs linked by trace/span IDs;
- independent hot retention and project-deletion registration.

Exit gate:

- malformed trees and missing parents remain queryable without unbounded repair;
- duplicate delivery does not create duplicate durable Span identities;
- one trace can be reconstructed with bounded queries and response size;
- Span saturation cannot starve Error or Log lanes;
- the saved official-SDK smoke covers a local transaction, child Span and correlated
  Log; multi-process distributed SDK verification moves to ADR-0044;
- ingest/storage functional gates and the retained Span-writer regression pass;
  production Trace-read, mixed-load and restart evidence moves to ADR-0044.

### Phase 26 — Performance aggregates and Insights

ADR-0043 defines rebuildable `span_stats_hourly`, bounded rollups, approximation
semantics and deterministic segment-local Insight enrichment.

Implement:

- `span_stats_hourly` as rebuildable derived state;
- throughput, failure rate and duration percentiles;
- transaction/service/operation summaries;
- bounded detection of slow HTTP/database/cache/queue operations;
- initially explicit rules for N+1 and repeated slow operations;
- Web performance views and links back to representative traces.

Exit gate:

- aggregate replay/rebuild is idempotent within the documented count semantics;
- high-cardinality attributes cannot create unbounded bucket keys;
- percentile and extrapolation semantics are documented and golden-tested;
- aggregate work remains behind foreground signal processing;
- representative performance queries are functionally bounded; production-shaped
  latency evidence moves to ADR-0044.

### Phase 24-26 performance-gate amendment

By explicit owner decision on 2026-07-24, Phase 24-26 do not run sustained, burst,
mixed-ingest, search-under-ingest or latency/load tests in this implementation pass.
The Envelope endpoint is shared, and these phases add protocol fixtures, functional
tests and saved real-SDK smoke programs instead. This amendment does not permit a
background server, SDK or test process to remain after a test run. Performance
baselines may be added later when separate measured regressions justify them.

On 2026-07-25 the owner reopened the Phase 24 performance evidence after identifying
that Logs were using a semaphore followed by sequential `insert_one` calls rather
than the accepted bounded micro-batch lane. Phase 24 is therefore exempted from the
earlier no-load amendment and closes with exactly two retained local regression
profiles:

1. an in-process Log writer RPS/batch-occupancy run;
2. one short mixed HTTP k6 run against real MongoDB with concurrent Logs and Errors,
   explicit TCP/HTTP/200/429/503 counters and acknowledged-versus-durable counts.

These short Windows profiles replace the original sustained, burst, retention and
search-under-ingest requirements for Phase 24 closure on the current development
machine. They are regression sentinels, not production-capacity claims. The earlier
amendment remains in force for Phases 25-26 until their owner-directed closure pass.

## Product backlog and ADR-0045 selections

ADR-0045 selects and narrows nine items from this broader backlog as Phases 28-36.
Where this older text conflicts with ADR-0045, the newer scope and exclusions win.
All other items remain unnumbered.

### Phase 32 selection — Unified Explore query surface

Implement one bounded query language and native API over accepted datasets:

```text
errors
logs
spans
```

Support:

- dataset selection;
- typed fields and exact predicates;
- time, project, environment and release scope;
- `count`, `sum`, `min`, `max`, `avg` and accepted percentiles;
- bounded group-by and timeseries intervals;
- deterministic cursor pagination for raw rows;
- explicit cost estimation and rejection before executing unsafe queries;
- table and chart Web views.

This is a shared query surface, not a shared physical collection. Dataset adapters
translate the accepted query AST to typed storage/search ports.

Exit gate:

- tenant isolation is proven before query execution;
- invalid/high-cost queries fail with stable errors and no partial background work;
- query results remain correct during concurrent ingest and retention;
- dataset addition does not require changing existing dataset codecs;
- search-under-ingest and adversarial-cardinality suites pass.

### Phase 33 selection — Saved queries and Dashboards

Implement:

- saved Explore queries;
- dashboards with bounded widget count;
- table, number and timeseries widgets;
- fixed refresh intervals and concurrency budgets;
- environment/release/project variables;
- dashboard sharing within accepted authorization scope;
- cached derived results only where correctness and invalidation are explicit.

ADR-0045 narrows initial visibility to one shared project scope: all authorized
project readers see the same saved queries and Dashboards, write-capable members may
mutate them, and no private/per-member copies or cross-project Dashboard are added.

Exit gate:

- one dashboard cannot fan out into unbounded MongoDB operations;
- concurrent refreshes are coalesced or bounded;
- malformed or deleted fields fail visibly;
- dashboard load does not violate ingest/query resource reservations.

### Phase 34 selection — Alerts and notification destinations

Extend the existing notification outbox rather than adding delivery to processors.

Implement:

- new Issue, regression and frequency rules;
- affected-user rules after accepted user counting exists;
- Explore aggregate thresholds for Logs/Spans;
- environment/release/tag predicates;
- cooldown, deduplication and storm limits;
- rule evaluation history, test mode and stable failure reasons;
- webhook actions first; future integrations consume the same outbox contract.

Exit gate:

- rule evaluation cannot perform outbound I/O in Processor;
- repeated processing/restart does not create unbounded duplicate notifications;
- high-cardinality rules are rejected before activation;
- alert bursts respect per-project and global delivery budgets;
- E2E covers Event/Log/Span -> rule -> outbox -> webhook.

## Deferred backlog group: Error workflow and release intelligence

### Backlog item — Issue collaboration

Implement:

- user assignment and unassignment;
- comments with cursor pagination;
- subscriptions and participants;
- mentions with bounded parsing;
- activity and notification-outbox integration;
- `assigned:me`, `assigned:none` and subscription filters.

Store comments and subscriptions outside the bounded Issue document. Teams are not
introduced implicitly in this phase.

Exit gate:

- authorization and tenant-isolation suites cover every mutation;
- comments cannot make the Issue document grow without bound;
- subscription and assignment operations are idempotent;
- Issue feed does not introduce per-row assignee queries;
- browser E2E covers assignment, comment, mention and notification.

### Phase 28 selection — Signal Inbound Filters only

The completed Error pipeline already accepts bounded SDK fingerprints, applies the
`{{ default }}` placeholder semantics, versions the grouping strategy and exposes a
stored grouping explanation. This backlog item does not reimplement those features.

ADR-0045 selects bounded typed project inbound filters for the currently implemented
Error, Log and Span/Transaction signals. Each lane exposes only its accepted fields
and filters before its first durable write. Server grouping rules, Issue merge/split
and all historical regrouping remain unnumbered backlog.

Exit gate:

- filter evaluation is bounded before durable signal storage;
- filtered bodies are not stored;
- absence of filters is regression-equivalent to existing Error, Log and Span paths;
- ingest throughput remains within each accepted signal regression budget.

### Phase 29 selection — Releases and Deploys

ADR-0045 narrows this item to:

- release create/finalize APIs compatible with the accepted `sentry-cli` subset;
- deploy records;
- release authors and first/last/new/regressed Issue summaries;
- optional bounded repository plus `commit_from`/`commit_to` references.

Full commit ingestion, diffs, suspect commits, source links, CODEOWNERS and ownership
assignment remain unnumbered backlog.

Exit gate:

- release/artifact identity remains consistent for source maps and debug files;
- release and deploy ingestion is bounded and idempotent;
- missing integrations degrade to stored metadata rather than broken Issues;
- real `sentry-cli` release/deploy E2E passes.

### Phase 30 selection — Sessions and Release Health

Implement:

- accepted Sentry session and session-aggregate items;
- dedicated Session lane and `sessions`;
- bounded session state transitions;
- `session_stats_hourly`;
- crash-free sessions/users and release/environment summaries;
- independent configurable detailed/bucket retention and rebuild behavior;
- optional project/day size-bounded Parquet/Zstandard cold archive through the
  existing BlobStore/archive coordinator, never one object per Session.

Exit gate:

- out-of-order and duplicate session updates have deterministic semantics;
- active sessions cannot grow without expiry;
- user cardinality is bounded/approximated according to a documented algorithm;
- Session-to-signal links are exposed only when a pinned accepted payload carries the
  same stable identity; release/environment/time proximity is never presented as an
  individual-record relationship.

### Phase 31 selection — User Feedback

Implement:

- accepted current Sentry feedback item/widget payloads;
- legacy feedback only if an exact SDK/API compatibility row requires it;
- feedback text/contact metadata under PII policy;
- links to Error Event, Issue, Trace and Replay when present;
- screenshots/attachments through BlobStore;
- feedback list/detail/status Web workflow.

Exit gate:

- attachment commit and feedback visibility follow the existing Blob ownership rule;
- anonymous and authenticated feedback authorization is explicit;
- spam/rate/body limits apply before durable side effects;
- real browser SDK/widget E2E passes.

## Deferred backlog group: reliability monitoring

### Phase 35 selection — Cron Monitoring

Implement:

- accepted Sentry `check_in` items;
- monitor definitions, schedules and environment state;
- in-progress, success, error, missed and timeout outcomes;
- Scheduler integration;
- check-in history, duration and alerts;
- monitor-specific retention.

Exit gate:

- scheduler restart does not duplicate missed outcomes without bound;
- clock skew and late/out-of-order check-ins have deterministic semantics;
- a check-in flood cannot starve Error/Log lanes;
- SDK Cron E2E covers success, error, timeout and missed.

### Phase 36 selection — Uptime Monitoring

Implement:

- HTTP/HTTPS monitor definitions;
- bounded request method, headers, timeout, redirect and body policies;
- SSRF-safe destination validation and redirect revalidation;
- scheduled execution with global/per-host concurrency limits;
- `uptime_results`, latency/history and alert integration;
- secret redaction and safe operational logs.

Exit gate:

- private/link-local/metadata destinations are blocked according to policy;
- DNS rebinding and redirects are adversarially tested;
- scheduler overload delays checks visibly without affecting ingest readiness;
- result retention and alert deduplication pass restart tests.

## Deferred backlog group: metrics and high-volume Blob products

### Backlog item — Metrics

Implement:

- accepted Sentry metric item types still supported by target SDK versions;
- counters, gauges and distributions;
- bounded metric name/unit/tag normalization;
- `metric_buckets` with fixed bucket widths;
- cardinality budgets, denied-tag policy and explicit discard accounting;
- Explore dataset and Dashboard/Alert integration.

Raw measurements are combined before durable storage where idempotency permits; the
durable model is a metric bucket, not an Event-shaped document.

Exit gate:

- retries cannot silently overcount outside documented at-least-once semantics;
- cardinality attacks are bounded before collection/index growth;
- bucket merge contention and recovery baselines pass;
- Metrics do not share the Span or Log queue.

### Backlog item — Profiling

Implement:

- accepted Sentry profile/profile-chunk variants for selected SDKs;
- bounded metadata in `profiles`;
- immutable compressed payload in BlobStore;
- stack/sample validation and symbolication through the existing backend boundary;
- profile-to-trace/release/environment correlation;
- flamegraph/call-tree API and Web investigation;
- derived function summaries for bounded Explore queries.

Exit gate:

- decompression/sample/frame limits apply before unbounded allocation;
- incomplete multipart/chunk uploads are recoverable and cleaned;
- missing symbols preserve raw frames and visible status;
- profile processing cannot consume Symbolicator capacity reserved for Errors;
- representative native/runtime SDK E2E and storage/load results pass.

### Backlog item — Session Replay

Implement:

- accepted replay event/recording items for selected browser SDKs;
- Replay metadata/session index in `replays`;
- immutable recording segments in BlobStore;
- SDK-side and server-side privacy validation with safe defaults;
- bounded segment ordering, gap handling and session finalization;
- Replay player with links to Error, Feedback, Log and Trace context;
- independent quotas, retention, archive and deletion.

Exit gate:

- DOM/text/input privacy corpus passes before capability enablement;
- malformed/compression-bomb recordings fail within byte/CPU limits;
- partial uploads and orphan cleanup pass crash/restart suites;
- Replay bandwidth cannot consume Error/Log admission reservations;
- real browser E2E records, uploads, retrieves and plays a bounded session.

## Deferred backlog group: organization workflow and ecosystem

### Backlog item — Teams and advanced authorization

Implement:

- teams and project ownership;
- team Issue assignment;
- organization invitations and membership lifecycle;
- project/team-scoped roles;
- MFA/passkeys before external enterprise identity;
- audit coverage for new privileged actions.

OIDC/SSO and SCIM require separate accepted security designs and may follow as
sub-phases; they are not enabled by placeholder configuration.

Exit gate:

- authorization decision tables and tenant isolation are exhaustive;
- removal of the final owner/admin remains protected;
- identity changes invalidate affected sessions/tokens as specified;
- assignment and notification fan-out remain bounded.

### Backlog item — Source, chat and issue-tracker integrations

Implement through provider ports and separate accepted provider sub-phases:

1. GitHub or GitLab source integration;
2. one chat provider using the notification outbox;
3. one issue tracker with link/create/sync rules.

Reuse releases, commits, ownership, source links and the existing outbox. OAuth
secrets/tokens use accepted secret storage and redaction contracts.

Exit gate:

- provider outages never block ingest or Issue reads;
- webhook authenticity, replay prevention and tenant routing pass;
- retries and rate limits are bounded per installation;
- uninstall/revocation removes credentials and stops delivery deterministically.

### Backlog item — Log/Trace drains and export

Implement:

- optional outbound Log and Trace drains;
- bounded filters and redaction;
- durable cursor/checkpoint state;
- delivery retries, lag metrics and circuit breaking;
- explicit at-least-once semantics;
- bulk export jobs using existing BlobStore where appropriate.

Exit gate:

- drains never execute in foreground ingest/Processor;
- a slow destination has bounded disk/RAM impact;
- redaction is applied before outbound persistence/delivery;
- restart resumes from a durable checkpoint with documented duplicate semantics.

### Backlog item — MCP adapter

Implement MCP only over existing application services:

- project/Issue/Event/Log/Trace search;
- Incident Capsule export;
- bounded read-only investigation tools first;
- scoped authentication and audit records;
- explicit response-size/query-cost limits;
- mutation tools only in separately accepted sub-phases.

Exit gate:

- MCP cannot bypass native API authorization or query limits;
- tool output applies the same PII/redaction policy;
- transport disconnect/cancellation releases work;
- MCP remains removable without changing domain/application contracts.

AI diagnosis, code generation and automatic pull requests remain outside this plan.

## Deferred backlog group: compatibility breadth and operational evolution

### Backlog item — Extended SDK and platform pipelines

Expand only through exact compatibility rows and bounded provider sub-phases:

- additional official Sentry SDK/runtime versions;
- ProGuard/R8 mappings;
- Hermes source maps;
- IL2CPP, BCSymbolMap or platform-specific debug formats;
- additional minidump/native contexts;
- any required legacy Sentry API or `sentry-cli` endpoints.

Each format owns its parser corpus, security limits, artifacts, E2E and performance
gate. Protocol similarity is not advertised as compatibility.

### Backlog item — Online schema evolution, backup and reconciliation

Before rolling mixed-version deployment:

- versioned online migrations with resumable progress;
- compatibility rules for old/new readers and writers;
- backup/restore runbooks and tested automation;
- application-consistent recovery boundaries;
- collection/Blob reconciliation and orphan repair;
- archive restore/search policy if accepted.

Exit gate:

- upgrade, rollback, interrupted migration and restored-environment suites pass;
- no migration requires unbounded memory or a single unbounded collection lock;
- reconciliation is idempotent and rate-limited behind foreground work.

### Backlog item — Optional distributed roles and horizontal scale

This phase is triggered by measured single-process limits, not product completeness.

Design and implement only after accepting a new inter-role protocol:

- optional `ingest`, `processor`, `symbolicator`, `web` and `scheduler` roles;
- durable claims/leases or an accepted broker;
- independent autoscaling and backpressure propagation;
- MongoDB sharding strategy per high-volume collection;
- rolling deployment and mixed-version protocol compatibility;
- no requirement that small installations run more than `--role=all`.

NATS, another broker, disk spool and a second storage engine remain decisions, not
implicit requirements.

Exit gate:

- `--role=all` retains equivalent correctness and a simple deployment;
- role loss/restart/network partition tests preserve acknowledged durability;
- overload is propagated rather than hidden in unbounded broker/database backlog;
- scaling results publish the actual bottleneck and cost.

## Dependency order and optionality

The completed chain, deferred gate and selected product wave are:

```text
23 Dark monochrome Web
-> 24 Logs
-> 25 Spans/Traces
-> 26 Performance
-> 27 Production readiness (accepted, execution deferred)
-> 28 Inbound Filters (next)
-> 29 Releases/Deploys
-> 30 Sessions/Release Health
-> 31 Feedback
-> 32 Explore
-> 33 Saved Queries/Dashboards
-> 34 Alerts/Destinations
-> 35 Cron
-> 36 Uptime
```

ADR-0045 owns the exact Phase 28-36 scope, exclusions and gates. The remaining
unnumbered backlog keeps only capability dependencies:

- saved queries and Dashboards depend on Unified Explore;
- query Alerts depend on a bounded query surface and the existing outbox;
- Sessions/Feedback extend release and Error investigation independently;
- Cron and Uptime share Scheduler but are otherwise independent;
- Metrics may later extend Explore, Dashboards and Alerts;
- Profiling and Replay depend on BlobStore and correlation contracts;
- MCP and provider integrations remain removable application adapters;
- online migrations and distributed roles are operations-driven and require separate
  acceptance evidence.

A remaining backlog item receives a new phase number only if all of its accepted
dependencies already exist and the move does not introduce a placeholder abstraction
into an earlier hot path.
Profiling and Replay may be disabled indefinitely without weakening Error, Log or
Trace correctness.

## Cumulative post-MVP E2E ladder

Every completed product slice keeps earlier rungs in CI. The labels below describe
capabilities, not current or future phase numbers:

```text
SDK Log -> logs -> Logs Web
SDK Error + Log -> shared trace correlation
two-service SDK trace -> spans -> Trace Web
spans -> derived performance bucket -> Insights
future Explore -> table/timeseries across accepted datasets
future Dashboard -> bounded Explore queries
future signal -> Alert rule -> outbox -> webhook
future Issue -> assignment/comment/subscription
future release/deploy/commit -> suspect/source ownership
future Error -> Session -> Release Health
future browser Feedback -> attachment -> Issue
future SDK Check-in -> Monitor outcome -> Alert
future Scheduler -> Uptime result -> Alert
future SDK Metric -> bucket -> Explore/Dashboard/Alert
future SDK Profile -> Blob -> symbolication -> flamegraph
future browser Replay -> Blob segments -> player -> Error
future provider webhook/API -> integration action
future Log/Trace -> durable drain -> destination
future MCP -> authorized application query -> bounded result
```

## Completion meaning

Completing this roadmap does not mean duplicating Sentry's implementation or every
commercial/AI feature. It means:

- broad compatibility with explicitly tested Sentry SDK capabilities;
- complete Error, Log, Trace, Performance, Metrics, Profile, Replay, reliability and
  feedback investigation flows;
- predictable bounded operation in one-process installations;
- physical isolation of workloads that need different indexes, retention and storage;
- extension points proven by real vertical products rather than speculative generic
  frameworks.
