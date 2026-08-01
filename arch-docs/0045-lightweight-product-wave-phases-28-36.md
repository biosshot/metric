# ADR-0045: Lightweight product wave, Phases 28-36

- Status: Accepted
- Date: 2026-07-26
- Amends: ADR-0040 product ordering and ADR-0044 execution order

## Context

Phases 23-26 completed the Error, Log, Trace and Performance core. ADR-0044 remains
the accepted production-readiness program, but the owner explicitly deferred its
execution on 2026-07-26 in order to add a bounded group of relatively inexpensive
product capabilities first.

At acceptance, this decision did not mark Phase 27 complete or grant a
production-ready claim. Phase 27 retained its number and could resume later; Phase 28
was the next product implementation phase.

Current amendment (2026-08-01): ADR-0047 later closed Phase 27 as obsolete without
claiming its gates. The paragraph above remains the historical state when this ADR
was accepted; Phase 27 no longer resumes or blocks current work.

The selected wave is:

| Phase | Capability | Status |
| ---: | --- | --- |
| 27 | Production readiness | Closed as obsolete by ADR-0047 |
| 28 | Signal Inbound Filters | Complete |
| 29 | Releases and Deploys | Complete |
| 30 | Sessions and Release Health | Complete |
| 31 | User Feedback | Complete |
| 32 | Unified Explore | Complete |
| 33 | Saved Queries and Dashboards | Complete |
| 34 | Alerts and notification destinations | Complete |
| 35 | Cron Monitoring | Complete |
| 36 | Uptime Monitoring | Complete |

ADR-0046 makes Application Metrics Phase 37 and Session Replay Phase 38. Profiling
remains desired but deferred and unnumbered. Organization workflow, provider
integrations, MCP, online migrations and horizontal scaling also remain unnumbered
backlog.

## Shared implementation rules

1. Every phase remains a complete vertical slice: protocol, domain, application,
   storage, API, Web, authorization, retention and tests.
2. Existing Error, Log and Span codecs and writer lanes are not generalized merely
   to host a new capability.
3. Low-volume capabilities may share a collection only when their query, retention
   and operational behavior are genuinely alike.
4. Every collection and queue is shared across projects and scoped by `project_id`;
   no per-project database or collection is introduced.
5. New SDK compatibility is claimed only for pinned executable rows and fixtures.
6. Missing configuration disables the new capability without weakening existing
   signals.
7. Every limit is configurable within a validated safe range and has a bounded
   default.
8. Storage-changing phases must explicitly own their schema-generation decision.
   This wave does not silently introduce online migration support.
9. Each phase passes its module, unit/property, real MongoDB, API/Web E2E and
   proportional load/fault gates before the next phase starts.

## Phase 28: Signal Inbound Filters

### Scope

Phase 28 adds bounded project-level filtering for every currently implemented
high-volume ingest signal:

```text
Error Events
Structured Logs
Transactions/root Spans
child Spans
```

Each signal evaluates its policy after bounded parsing/normalization needed by the
matcher and before its first durable write. Transactions use the Span policy and do
not create a fourth filter pipeline.

Phase 28 deliberately excludes:

- Issue merge and split;
- server-side fingerprint or stacktrace grouping rules;
- historical regrouping;
- user-provided regular expressions or executable scripts;
- placeholder filters for Sessions, Feedback, Metrics or other future signal
  classes. A future phase may add a typed policy only when that signal exists.

The existing revision-1 grouping algorithm and `IssueId` derivation remain unchanged.
When no filters are configured, the accepted Error, Log and Span paths are
behaviorally identical to Phase 26.

### Rules

Shared rule fields, where the signal owns them, are:

- release;
- environment;
- service.

Signal-specific fields are:

| Signal | Accepted fields |
| --- | --- |
| Error | normalized message, exception type, logger, request host/path |
| Log | normalized message, severity |
| Transaction/root Span | name, operation, status, duration |
| child Span | name, operation, status, duration |

A rule declares its signal target and cannot reference a field unavailable in that
signal schema. Trace IDs, Span IDs, Event IDs, user IDs and arbitrary attributes are
not initial filter fields because they invite high-cardinality policies.

Accepted match operations are:

```text
exact
prefix
suffix
contains
glob
```

Built-in opt-in filters may cover localhost, browser-extension frames and known
crawler/bot traffic. Rules are bounded by count, pattern bytes and total compiled
policy bytes. Glob syntax has a deterministic linear-time implementation; it is not
translated to an unbounded regular expression.

### Contract and storage

`InboundFilterPolicy` is a bounded part of the project policy and therefore adds no
collection. The project cache holds the compiled immutable policy revision. The
matcher language and compiler are shared, while Error, Log and Span adapters expose
only their typed fields; the three writer lanes remain independent.

```text
Envelope/auth/limits
-> bounded item parse/normalization required by its typed adapter
-> InboundFilter(signal, fields)
   -> matched: handled discard outcome, no signal body stored
   -> unmatched: existing Error, Log or Span durability path
```

A filtered item returns the accepted Sentry-compatible handled outcome so an SDK
does not retry it forever. Mixed Envelopes filter each supported item independently.
Aggregate low-cardinality counters include signal class and rule reason; no
per-discard MongoDB document or filtered payload is retained.

### Exit gate

- absence of a policy is regression-equivalent to current Error, Log and Span paths;
- every matcher has deterministic property/fuzz tests and declared complexity;
- filtering occurs before signal/Blob durable side effects;
- policy-cache invalidation is revision-safe;
- filtered bodies do not appear in MongoDB, BlobStore, logs or diagnostics;
- one signal's rules cannot inspect or slow another signal's adapter;
- a worst-case policy stays inside each accepted ingest CPU/allocation budget;
- real SDK E2E covers accepted and filtered Errors, Logs and Spans.

## Phase 29: Releases and Deploys

### Scope

Phase 29 turns the existing implicit `releases` catalog into an explicit release and
deployment workflow:

- release create, finalize and detail APIs;
- the accepted `sentry-cli` release subset;
- deploy start/finish records by environment;
- first-seen, last-seen, new and regressed Issue summaries;
- Error, Log, Span and artifact links by release;
- release/deploy Web views.

Full commit ingestion, diffs, changed-file inventories, suspect commits, CODEOWNERS
and source ownership are deferred. They are not required for this phase.

### Storage

The existing `releases` collection is extended with bounded explicit metadata.
Phase 29 adds only:

```text
deploys
```

A release may optionally store bounded repository references and `commit_from` /
`commit_to` hashes. Metric does not mirror Git history or source contents.

Deploy writes are idempotent by a client operation/deploy identity. A release exists
without a repository integration and remains useful for Issue regression and
environment comparison.

A Deploy is not an uploaded binary or a second copy of a Release. It is a small
timestamped fact that a particular Release was installed into an environment:

```text
release backend@2.4.0
-> deployed to staging at 12:00
-> deployed to production at 14:30
-> rolled back or replaced later
```

This gives regression analysis a precise environment timeline instead of assuming
that release creation time equals production rollout time.

Events, Issues, Logs and Spans do not store a `deploy_id`. Investigation combines
their existing `release`, `environment` and occurrence timestamps with the indexed
Deploy timeline. The UI may state that an Issue first appeared after a Deploy, but
must not claim that the Deploy caused it.

### Exit gate

- implicit Event-created and explicit CLI-created release identities converge;
- repeated release/finalize/deploy requests are idempotent;
- release summaries are derived without scanning an unbounded Event history;
- absent repository metadata never breaks Error or artifact investigation;
- real `sentry-cli` release/finalize/deploy E2E passes;
- release and deploy queries use bounded indexed project/organization timelines.

## Phase 30: Sessions and Release Health

### Scope

These are Sentry application sessions, not Metric Web authentication sessions.
Phase 30 accepts pinned Sentry `session` and supported session-aggregate Envelope
items and exposes:

- `started`, `ok`, `exited`, `crashed` and `abnormal` lifecycle;
- crash-free sessions;
- approximate crash-free users;
- release/environment timeseries and summaries;
- an optional exact signal link only when a pinned accepted payload carries the same
  stable session identity.

### Storage and lane

```text
sessions
session_stats_hourly
```

Sessions use a dedicated bounded `SessionWriter`; they do not enter Error, Log or
Span queues. `sessions` is the retained session source of truth.
`session_stats_hourly` is rebuildable derived state.

One logical Session remains one compact document because it receives idempotent and
out-of-order state updates. Packing multiple Sessions into one BSON array is rejected:
it creates write contention, makes individual expiry/update expensive and approaches
MongoDB's document-size limit under burst traffic.

The initial compact physical shape contains only indexed/lifecycle projections, not
the original SDK JSON:

```javascript
{
  _id, // 16-byte project-scoped ID derived from Project ID + SDK Session ID
  p,   // Project ID
  r,   // compact Release ID
  e,   // compact Environment ID
  s,   // started_at
  l,   // last accepted update time
  q,   // numeric state code
  n,   // optional SDK sequence
  f,   // optional finished_at
  d,   // optional duration
  u,   // optional bounded pseudonymous user digest
  h,   // optional archive-due time
  z,   // optional completed archive segment
  x    // optional absolute TTL time
}
```

Optional fields are omitted, never stored as `null`. Release/environment strings,
arbitrary attributes, SDK metadata and an unbounded raw body are not duplicated in
every Session document. The exact compact codec and representative BSON/index byte
budgets are pinned by the Phase 30 storage fixtures.

An accepted SDK session-aggregate item is never expanded into synthetic individual
Session documents. Its bounded counters are normalized as one idempotent aggregate
contribution and folded into `session_stats_hourly` under a separately pinned
durability/retry contract. Compatibility for an aggregate form is enabled only when
the pinned SDK fixture supplies enough identity for the declared duplicate
semantics; otherwise that form remains explicitly unsupported rather than silently
overcounting.

The required hot indexes are limited to:

- MongoDB `_id`;
- bounded project deletion/detail ownership by `(p, _id)`;
- single-field TTL on `x`;
- an archive-due partial index only when the archive path requires it.

Release Health charts query `session_stats_hourly`, not raw `sessions`; the hot
collection therefore does not receive speculative release/environment/timeseries
indexes. Exact Session detail uses `_id` and verifies project ownership.

Session identity and state precedence make duplicate and out-of-order updates
deterministic. Active sessions expire after a configured maximum age. User identity
is absent or stored only as an accepted pseudonymous digest. Unique-user counts use
a documented bounded mergeable sketch rather than an unbounded set of user IDs.

Session correlation is exact-only and never stores growing arrays of signal IDs
inside a Session. A link exists only when an accepted SDK payload carries the same
stable session identity. Project/release/environment/time proximity is not exposed
as a Session-to-Error/Log/Span/Feedback relationship. Release Health may aggregate
those dimensions independently, but does not claim that individual records belong
to a Session. Replay retains its independent `replay_id`; Error/Trace/Feedback links
to Replay remain authoritative only when that ID is actually present.

Exact Session investigation is assembled at query time, following the existing
virtual-Trace pattern:

```text
UI Session detail request
-> authorized bounded application query
-> Session source-of-truth lookup
-> parallel typed lookups in signal collections that actually carry the same ID
-> one bounded response with explicit truncation counters
```

The browser does not query MongoDB or join datasets itself. No background join,
duplicated signal body, Session-owned signal-ID array or precomputed relationship
document is created. An optional Session-ID projection/index is added to a signal
collection only when a pinned compatibility fixture proves that the signal carries
that identity; the initial phase does not add empty correlation fields or indexes to
every document speculatively.

### Retention and optional cold archive

Detailed Sessions and hourly health buckets have independent configurable retention:

```toml
[retention]
sessions_days = 7
session_stats_hourly_days = 400
```

These are starting defaults to validate during Phase 30, not hard-coded product
promises. Small installations may shorten detailed Session retention while retaining
compact health history.

Cold Session archival is optional. With no archive backend, a terminal Session gets
absolute `x` and MongoDB TTL eventually removes it. A stale active Session is first
deterministically terminalized by Scheduler; ordinary TTL never silently removes
mutable active state.

When Session archival is enabled, terminal documents receive `h` rather than `x`.
The existing archive coordinator writes many compact canonical Session rows into one
project/day, size-bounded, versioned Parquet/Zstandard segment in the configured
local or S3-compatible BlobStore:

```text
projects/{project_id}/sessions/{year}/{month}/{day}/{segment_id}.parquet
```

There is never one Blob object per Session. The existing manifest/checksum commit
order applies: only after the archive segment is complete and verified does the
source Session receive `z` and `x`. Archive failure leaves the source in MongoDB and
raises an operational failure instead of deleting unarchived data.

`session_stats_hourly` remains the normal long-term query source and uses its own
TTL. Cold Session objects are for export/restore, not transparent Web search. At
this Phase's original generation-8 boundary Logs and Spans had TTL deletion only;
their cold-archive path was added later and is present in current generation 19.

### Exit gate

- duplicate/out-of-order fixtures produce the same terminal state;
- acknowledged updates meet the declared durable or explicitly at-least-once rule;
- active sessions cannot remain unbounded;
- user sketches have published error and byte bounds;
- representative Session BSON plus required index bytes meet the Phase 30 budget;
- TTL-only and optional archive modes both preserve terminal/active safety;
- archive batching, retry, checksum, restore and failure-without-delete E2E passes;
- session traffic cannot consume Error/Log/Span reservations;
- real SDK session lifecycle/crash-state and any claimed exact-link E2E passes.

## Phase 31: User Feedback

### Scope

Phase 31 accepts the pinned current Sentry Feedback/widget payload and provides:

- bounded message, name and contact metadata under the PII policy;
- optional Event, Issue, Trace and future Replay links;
- optional screenshot/attachments through the existing BlobStore commit contract;
- feedback list, detail and status workflow in Web.

Legacy feedback endpoints are added only when an exact compatibility row requires
them. Feedback is not a general form builder, chat system or ticket tracker.

### Storage

```text
feedback
```

One feedback document stores bounded searchable metadata and Blob references. Blob
bytes never live in the BSON document. Anonymous submission is project-policy
controlled and has separate body, attachment, rate and abuse limits.

### Exit gate

- feedback never becomes visible before all owned Blob references are committed;
- anonymous and authenticated authorization tables are explicit;
- HTML/script content is never rendered as trusted markup;
- spam, request, text and attachment limits apply before durable side effects;
- deletion and retention cover metadata plus owned Blobs;
- real browser SDK/widget E2E passes.

## Phase 32: Unified Explore

### Scope

Phase 32 introduces one bounded typed query language over:

```text
errors
logs
spans
```

It does not combine their physical collections. Dataset adapters translate an
accepted `ExploreQuery` AST into existing typed storage/search operations.

The first version supports:

- exactly one dataset per query;
- mandatory project and bounded time range;
- typed exact, presence and numeric/time range predicates;
- deterministic raw-row cursor pagination;
- `count`, `sum`, `min`, `max`, `avg` and already accepted percentiles;
- at most two bounded `group_by` fields;
- fixed timeseries intervals;
- table, number and timeseries Web results.

Arbitrary MongoDB expressions, joins, scripts, arbitrary regex, unrestricted tags
and cross-organization queries are excluded.

### Cost boundary

A deterministic estimator validates dataset, range, predicates, group cardinality,
interval count, row limit and aggregate fan-out before storage work begins. Each
request consumes a bounded query budget and concurrency reservation. Adding a
dataset requires a new adapter, not changes to existing BSON codecs.

Explore adds no raw-data collection.

### Exit gate

- tenant/project scope is injected before planning and cannot be overridden;
- unsafe/high-cost queries fail before partial background work;
- every accepted query has a stable normalized AST and cost;
- query results remain correct during ingest and TTL deletion;
- search-under-ingest and adversarial-cardinality suites pass;
- Web never submits raw MongoDB syntax.

## Phase 33: Saved Queries and Dashboards

### Scope and storage

Phase 33 persists Phase 32 queries and composes them into bounded dashboards:

```text
saved_queries
dashboards
```

Initial widgets are number, table and timeseries. A dashboard has bounded widget
count, fixed refresh choices and project/environment/release variables. It stores
configuration, not copies of signal rows.

No derived-result cache is introduced initially. Concurrent identical refreshes may
be coalesced in memory; each dashboard also has a total query-cost budget.

Saved queries and Dashboards are shared project resources, not personal member
documents. Every organization member with `ProjectRead` for the project sees them.
Existing write-capable members (`IssueWrite`) may create and edit them; Viewers are
read-only. `created_by` and `updated_by` are retained for audit, not ownership ACL.
There is no private visibility mode, per-user duplicate, or cross-project Dashboard
in the initial phase. Unsaved personal exploration remains transient client state.

### Exit gate

- stored queries are revalidated against the current typed schema on every update;
- one dashboard cannot fan out into unbounded queries;
- deleted/renamed fields fail visibly;
- authorization covers shared project view, create, edit and delete;
- dashboard refresh cannot consume ingest reservations;
- Browser E2E covers lifecycle, variables and partial widget failure.

## Phase 34: Alerts and notification destinations

### Scope

Phase 34 extends the existing alert rules, notification destinations, embedded Issue
transition outbox and delivery dispatcher. It does not send network requests from an
ingest writer or Processor.

Initial rule classes:

- new Issue and regression;
- Error frequency;
- Explore aggregate threshold for Error, Log or Span;
- environment/release predicates;
- cooldown, deduplication, storm budget and resolved transition.

Initial destination kinds:

```text
Telegram Bot API
SMTP Email
```

The generic webhook delivered by Phase 20 remains backward compatible, but Phase 34
does not add or expose new webhook configuration. Web Push and provider-specific
chat/issue-tracker integrations remain deferred. SMTP is selected instead of Web
Push because it is provider-neutral and can be configured by an organization owner
without a Metric-operated mail service.

### Storage and delivery

Phase 34 reuses:

```text
alert_rules
notification_destinations
notification_deliveries
```

`NotificationDestination` becomes a bounded tagged configuration. Telegram bot
tokens, webhook secrets and SMTP passwords follow the existing
sealed-secret/redaction boundary. SMTP permits only authenticated TLS or STARTTLS;
host resolution is checked against the existing private-network policy before
connection.

Issue rules create outbox transitions during existing finalization. Aggregate rules
are evaluated by bounded Scheduler work over Explore. All adapters consume claimed
deliveries and implement stable retry/dead semantics, provider rate limits and
idempotency where the provider permits it.

### Exit gate

- repeated evaluation/restart cannot create unbounded duplicate deliveries;
- one alert may target multiple destination kinds without duplicating rule logic;
- provider outages never block ingest, Processor or Issue reads;
- Telegram escaping/rate-limit handling and SMTP TLS/authentication failure
  classification are tested;
- secrets never enter payload history, logs or API responses;
- alert storms respect project, destination and global budgets;
- E2E covers Event and aggregate rule delivery through Telegram and SMTP Email;
  the retained Phase 20 webhook regression remains green.

## Phase 35: Cron Monitoring

### Scope

Phase 35 accepts pinned Sentry `check_in` items and provides:

- monitor definitions and environment schedules;
- `in_progress`, `success`, `error`, `timeout` and `missed` outcomes;
- check-in history and duration;
- Scheduler detection of timeout/missed runs;
- Phase 34 alert integration.

### Shared reliability storage

Phase 35 creates:

```text
monitors
monitor_runs
```

`monitors` stores a common bounded header plus a Cron configuration variant.
`monitor_runs` stores immutable/idempotent Cron outcomes. Phase 36 reuses these
collections for Uptime because ownership, timeline, status, duration, retention and
alert behavior are the same.

Cron check-ins have an independent admission budget and cannot enter Error/Log/Span
lanes. Schedule parsing supports an explicitly bounded subset with a minimum
frequency.

### Exit gate

- duplicate, late and out-of-order check-ins are deterministic;
- restart does not create unbounded duplicate missed/timeout runs;
- clock-skew policy and grace windows are documented and tested;
- Scheduler lag is visible and cannot make ingest unready;
- retention and project deletion cover both collections;
- real SDK Cron E2E covers success, error, timeout and missed.

## Phase 36: Uptime Monitoring

### Scope

Phase 36 extends the Phase 35 monitor model with server-originated HTTP/HTTPS checks:

- public HTTP/HTTPS URL;
- `GET` and `HEAD` only initially;
- bounded custom request headers;
- bounded timeout and at most three redirects;
- expected status-code range;
- latency and failure history;
- Phase 34 alert integration.

Arbitrary request bodies, browser execution, JavaScript, screenshots,
private-network monitoring and multi-step checks are excluded from the initial
phase.

Custom headers are bounded by count, name bytes, individual value bytes and total
encoded bytes. `Host`, `Content-Length`, `Transfer-Encoding`, connection/hop-by-hop,
proxy and forwarding headers cannot be overridden. Sensitive values such as
`Authorization`, API keys and cookies are stored through the sealed-secret boundary,
are write-only through the API and never appear in results, logs or audit payloads.
There is no environment-variable or template expansion in header values.

On redirect, sensitive headers are always removed. Non-sensitive custom headers may
be forwarded only to the same normalized origin; a cross-origin redirect receives
only Metric-owned safe defaults. Every redirect destination still passes the full
SSRF validation below.

### Security and execution

Every initial resolution and redirect target is revalidated. Loopback, private,
link-local, multicast, unspecified, metadata-service and disallowed address ranges
are rejected for both IPv4 and IPv6. DNS answers are pinned for the individual
request to prevent validation/connect disagreement.

Scheduler work uses global and per-host concurrency, response-byte, redirect,
timeout and rate budgets. Response bodies are drained only to a small configured
limit and are not stored. Secrets and query values are redacted from operational
diagnostics.

Uptime definitions and results reuse:

```text
monitors
monitor_runs
```

with tagged Uptime variants and shared project/timeline indexes.

### Exit gate

- SSRF, DNS rebinding, redirect, IPv4/IPv6 and metadata-address corpus passes;
- forbidden/header-size/sealed-secret and redirect-header stripping suites pass;
- a slow or hostile host cannot retain unbounded tasks, sockets or bytes;
- checks are fair globally and per destination host;
- Scheduler restart preserves deterministic due scheduling;
- Uptime load cannot affect ingest readiness or alert delivery reservations;
- Browser E2E covers monitor lifecycle, history and firing/resolved alerts.

## Dependency and execution order

The accepted order is sequential:

```text
28 Inbound Filters
-> 29 Releases/Deploys
-> 30 Sessions/Release Health
-> 31 Feedback
-> 32 Explore
-> 33 Saved Queries/Dashboards
-> 34 Alerts/Destinations
-> 35 Cron
-> 36 Uptime
-> 37 Application Metrics (ADR-0046)
-> 38 Session Replay (ADR-0046)
```

Strict capability dependencies are:

- Release Health depends on the Phase 29 release identity;
- Dashboards depend on Explore;
- aggregate Alerts depend on Explore;
- Cron/Uptime alerting depends on Phase 34;
- Uptime reuses the Phase 35 reliability-monitor model.

The sequential order is a delivery/test discipline, not permission to make earlier
modules depend on future phases.

## Deferred or selected after Phase 36

ADR-0046 removes Metrics and Replay from this unnumbered list and assigns them
Phases 37 and 38. The following remain deliberately unnumbered:

- Profiling;
- Issue collaboration, merge/split and custom grouping rules;
- commit history, suspect commits, CODEOWNERS and source ownership;
- Teams and advanced authorization;
- provider integrations beyond Phase 34 destinations;
- drains/export, MCP and AI;
- extended platform pipelines and OpenTelemetry;
- online migrations and distributed roles.
