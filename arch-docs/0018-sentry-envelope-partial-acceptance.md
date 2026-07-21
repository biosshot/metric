# ADR-0018: Sentry Envelope partial acceptance and item capabilities

- Status: Accepted
- Date: 2026-07-21

## Context

A Sentry Envelope can contain several independent or related Items. The first product
version processes Error Events and policy-approved Event attachments, while
transactions, sessions, profiles, replays, check-ins, standalone spans, and StatsD
remain disabled. Rejecting an entire mixed Envelope because one optional Item is
disabled would lose a valid error and cause SDK retry loops.

Transport compatibility with an SDK must therefore be distinguished from feature
support for every product carried by the transport.

## Decision

### Compatibility layers

The server provides Sentry Envelope transport compatibility independently of its
advertised item capabilities. A compatible SDK can submit a mixed Envelope without an
unsupported secondary Item breaking a supported Error Event. An Item is reported as
feature-supported only when its data is actually processed and exposed by the product.

Capabilities are available through shared application services to Web, API, and MCP.

### Initial item matrix

Initial behavior is:

```text
error/default event                 accept and process
event attachment                    accept according to attachment policy
client_report                       parse and aggregate
transaction                         discard: feature_disabled
session/session aggregates          discard: feature_disabled
profile/profile_chunk               discard: feature_disabled
replay_event/replay_recording       discard: feature_disabled
check_in                            discard: feature_disabled
standalone span                     discard: feature_disabled
statsd/metric buckets               discard: feature_disabled
known but otherwise disabled type   discard: feature_disabled
unknown future type                 discard: unknown_item_type
```

Configuration exposes these capabilities explicitly:

```toml
[ingest.items]
error = true
client_report = true
transaction = false
session = false
profile = false
replay = false
check_in = false
span = false
statsd = false
```

Attachment acceptance remains controlled by the independent policy from ADR-0011.
Enabling a configuration key without an implemented backend is a startup validation
error; it cannot silently pretend that the feature is active.

### Bounded framing before side effects

Ingest authenticates the project, enforces compressed and decompressed request limits,
and parses Envelope and Item framing with bounded memory. It first discovers Item
metadata and dependency relationships, then validates supported content, and only then
publishes BlobStore objects or submits MongoDB writes.

Declared Item length is authoritative for locating the next Item. Unknown payloads
with valid framing are skipped as opaque bounded byte ranges and are never parsed or
stored for possible future use.

An unsafe framing error rejects the complete Envelope because subsequent Item
boundaries cannot be trusted. Whole-Envelope failures include:

- malformed Envelope or Item headers;
- invalid or inconsistent declared lengths;
- payload ending outside the request boundary;
- conflicting Envelope and Event IDs;
- more than one primary Event;
- an invalid supported Error Event;
- request, decompression, Item-count, or applicable Item-size limit violations.

Existing HTTP status meanings remain `400` for invalid structure, `413` for size or
count limits, `401/403` for authentication/authorization, `429` for active admission
limits, and `503` for temporary durable-storage or system overload.

### Primary and dependent Items

The first version permits one primary Error Event in an Envelope. Policy-approved
attachments, screenshots, view hierarchies, and similar Event-owned Items are
dependent on that Event and use the BlobStore commit protocol in ADR-0012.

A dependent Item is discarded when its parent is unsupported, invalid, filtered, or
rate-limited. Reasons distinguish `parent_unsupported`, `parent_invalid`,
`parent_rate_limited`, and `attachment_policy_disabled`.

An attachment without a primary Event remains unsupported. ADR-0020 defines the
special standalone minidump endpoint that synthesizes an Event. A profile dependent
on a disabled transaction is discarded with its parent.

### Mixed and unsupported-only responses

A mixed Envelope containing a durably accepted supported Event and discarded disabled
Items receives HTTP `200` only after MongoDB confirms the Event and every required
enabled BlobStore object is committed.

An Envelope containing only well-framed disabled Items also receives HTTP `200`. This
means the transport request was handled and intentionally discarded, not that an Event
was durably stored. Hourly outcomes and capability responses expose the difference and
prevent permanent client retry storms.

Known disabled categories are returned in a Sentry-compatible category backoff header:

```http
X-Sentry-Rate-Limits: 3600:transaction;session:project:feature_disabled
```

```toml
[ingest.unsupported]
category_backoff_seconds = 3600
```

The backoff is configurable and defaults to one hour so enabling a feature becomes
visible to SDKs without an effectively permanent stale limit. Unknown future types do
not receive invented category names.

Per-item `unsupported` and dependency outcomes are aggregated in
`ingest_outcomes_hourly`; no per-request discard document is created.

### Client reports

`client_report` is supported from the first version. Its bounded category, reason, and
quantity records are normalized into the existing hourly outcome projection with
`source = "sdk"`. Individual reports and arbitrary raw content are not retained.

Client reports are lossy diagnostic telemetry. A failure to update their approximate
aggregate does not fail an otherwise durably accepted Event and does not turn a
client-report-only Envelope into a retry requirement.

### Processing order

The logical order is:

```text
authenticate and apply request limits
    -> parse bounded framing and Item metadata
    -> establish primary/dependent relations
    -> validate supported Error Event
    -> classify disabled and unknown Items
    -> scrub and commit enabled dependent blobs
    -> durably insert Event in MongoDB
    -> aggregate outcomes without delaying acknowledgement
    -> respond with category capability backoff
```

A disabled attachment does not prevent an otherwise valid Event from being accepted.
A temporary failure of an attachment that policy requires to be accepted preserves the
ADR-0012 behavior: no Event insert and HTTP `503`, allowing the SDK to retry the whole
Envelope.

## Consequences

- Official SDKs can continue reporting errors while their other product Items are
  disabled.
- Unsupported-only traffic does not create an endless retry storm or raw-data store.
- A framing error cannot make the parser interpret attacker-controlled bytes as later
  headers.
- Known disabled categories can be suppressed temporarily at the SDK through the
  standard rate-limit mechanism.
- Client-side loss becomes visible through bounded hourly aggregates.
- HTTP `200` means the Envelope was handled; durable Event acceptance is defined per
  supported Event and recorded separately.

## Deferred questions

- Exact Sentry compatibility corpus for every Envelope header and Item type.
- Feature-specific processing when transaction, session, replay, profile, check-in,
  span, metrics, logs, or feedback is enabled.
- Envelope response-body compatibility across SDK generations.
- Relationships for future nested or container Item types.
