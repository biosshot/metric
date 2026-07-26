# ADR-0042: Compact Structured Log MongoDB model

- Status: Accepted
- Date: 2026-07-24

## Context

Phase 24 adds Sentry-compatible Structured Logs as the first new high-volume signal
after the Error Monitoring MVP. Logs need independent indexes, retention and writer
resource isolation, but must reuse the accepted bounded durable-ingest shape without
copying the Error grouping pipeline.

At the 100-million-record/day design target, expanded descriptive BSON and automatic
indexing of arbitrary attributes would dominate storage and index cost. At the same
time, storing the complete Log only as an opaque binary body would make message
search, time feeds and Trace correlation impractical.

The model therefore separates a small deliberately queryable projection from an
required versioned bounded accepted body.

## Implementation amendment: synchronous terminal durability

Phase 24 found an explicit contradiction between the proposed pending lifecycle and
the actual bounded Log work: validation, PII scrubbing and projection are completed
inside the request before a single MongoDB insert. There is no asynchronous external
stage comparable to Error symbolication/grouping. Persisting a second pending shape
would add a write and recovery index without protecting acknowledged data.

The accepted implementation therefore writes one terminal idempotent Log document and
acknowledges only after that insert is durable. A crash before the insert has no
acknowledgement; a lost response is safely retried using the deterministic identity.
Terminal documents enter a dedicated bounded Log writer and are combined into
unordered MongoDB `insert_many` operations. Synchronous terminal durability removes
the pending/finalization state; it does not remove the independent lane or
micro-batching requirement.
The earlier proposed `q` pending fields, pending recovery index and `LogProcessor`
finalization are superseded and intentionally absent from the accepted model below.
They require a new ADR only if Log processing later gains asynchronous work that
cannot execute before acknowledgement.

## Collection and domain identity

Structured Logs use one collection for all projects:

```text
logs
```

There is no collection or database per project and no `event_type` field. The
collection name already identifies the signal.

The domain model uses a distinct identity:

```rust
struct LogId([u8; 16]);
```

`LogId` is a server-derived time-sortable deterministic 128-bit identifier:

```text
bytes 0..8   = received_at Unix milliseconds, big endian
bytes 8..16  = BLAKE3(
                 "structured-log/v1" ||
                 project_id ||
                 occurred_at_ns ||
                 accepted item payload
               )[0..8]
```

It is not a Sentry Error `event_id`, Trace ID or Span ID. Retrying the same already
formed `LogRecord` inside the writer derives the same identity; an existing record is
verified rather than overwritten silently. A separate SDK redelivery receives a new
server `received_at` and may therefore create a second Log. External Log delivery is
at least once, not an exactly-once claim. The domain-separation literal and byte
layout are fixed by Phase 24 golden tests.

MongoDB stores `LogId` as 16-byte binary `_id`. Stable feed order is
`occurred_at, LogId`, not timestamp alone.

## Terminal BSON document

The conceptual processed document is:

```javascript
{
  _id, // 16-byte LogId
  p,   // project ID, BSON int32
  r,   // server receive time, BSON date
  o,   // Log occurrence time, Unix nanoseconds, BSON int64
  x,   // hot TTL time, BSON date
  l,   // normalized severity code
  g,   // 16-byte Trace ID, optional
  n,   // 8-byte Span ID, optional
  m,   // normalized display/search message
  e,   // environment, optional
  v,   // release, optional
  j,   // service, optional
  b    // required versioned bounded accepted Log body
}
```

An ordinary terminal Log may be as small as:

```javascript
{
  _id,
  p,
  r,
  o,
  x,
  l,
  m,
  b
}
```

Optional values are omitted rather than stored as BSON `null`. A terminal document
does not contain Error-only Issue, grouping, symbolication or platform projections.

Physical field names are private MongoDB-adapter constants. Descriptive Rust/domain
names do not leak into each BSON document.

## Severity

The accepted normalized domain enum is append-only:

```rust
enum LogSeverity {
    Trace = 1,
    Debug = 2,
    Info = 3,
    Warn = 4,
    Error = 5,
    Fatal = 6,
}
```

Every terminal document stores `l`, including `Info`. Codes are never reused with
another meaning. Input aliases and unknown values are decided by exact supported SDK
fixtures:

- accepted aliases normalize to one known code;
- an unknown source value is never silently presented as `Info`;
- if the owning compatibility contract permits preservation, the bounded original
  value remains in `b`;
- otherwise the item is rejected with a stable protocol error/outcome.

## Timestamps

`r` is server receive time as a BSON date. `o` is the accepted occurrence time as
signed Unix nanoseconds in BSON `int64` and is used by feed/range indexes. `x` is the
server-controlled BSON-date retention deadline derived from `r`.

The nanosecond representation preserves the supported SDK precision. Feed ordering
remains deterministic through `(o, _id)`.

Clock-drift correction, if implemented, preserves both the accepted corrected
occurrence time and enough bounded source metadata to explain the correction. It does
not allow a client timestamp to bypass age/future-time admission limits.

## Message projection

`m` contains the normalized display/search body of the Log. It is the canonical stored
copy of the message and is not repeated inside `b`.

The owning protocol contract defines:

- maximum encoded message bytes;
- invalid UTF-8 behavior;
- newline/control-character normalization;
- empty-message behavior;
- PII scrubbing before terminal visibility;
- truncation versus rejection semantics.

The limit is configurable within accepted safe bounds and enforced before durable
allocation grows beyond the item/request budgets.

Keeping `m` outside `b` enables:

- Log feed previews without body decompression;
- bounded message search;
- safe response projection;
- storage-byte measurement independent of attributes.

## Trace correlation

When supplied and valid:

- `g` stores the 16-byte Trace ID;
- `n` stores the 8-byte Span ID.

They are not hex strings and are not duplicated inside `b`. A Span ID without a Trace
ID follows the supported Sentry SDK normalization contract; it cannot create an
unscoped cross-project lookup.

All correlation queries include project scope. Logs remain valid and queryable when
the related Span/Error has not arrived, has expired or was sampled out.

## Query projections and arbitrary attributes

Generation 8 promotes only environment `e`, release `v` and service `j` as bounded
optional exact-filter projections. It does not create `k` exact-search tokens or a
multikey attribute index. Arbitrary user-controlled attributes remain inside `b` and
never create MongoDB field paths, indexes or collections.

Adding indexed arbitrary attributes remains a deferred measured decision and requires
a schema-generation change, storage/index budget and explicit query-cost gate.

## Residual body

`b` is required. It stores the complete bounded scrubbed accepted Log item as
versioned canonical JSON, including fields also projected for query/display.

The binary header is:

```text
byte 0: Log body format version
byte 1: body codec
remaining bytes: encoded residual body
```

The generation-8 codec is:

```text
0 = canonical JSON
```

No Log-body compression codec is currently enabled. The decoded byte limit is
enforced before durable storage.

`b` may contain:

- bounded structured attributes;
- supported SDK/source metadata;
- original sub-millisecond timestamp representation;
- bounded original severity text when the compatibility contract preserves it;
- supported unknown forward-compatible fields;
- context not selected as a top-level query projection.

Keeping the complete accepted item in `b` preserves structured attributes and
forward-compatible fields. The small projection duplication is deliberate and is
measured by BSON fixtures.

## Terminal write lifecycle

Before entering the Log writer, ingest:

1. validates and scrubs the bounded accepted payload;
2. extracts the canonical message into `m`;
3. maps severity to `l`;
4. extracts binary Trace/Span IDs into `g` and `n`;
5. extracts bounded environment/release/service projections;
6. encodes the required versioned accepted body;
7. assigns the hot-retention deadline.

The dedicated bounded Log writer combines terminal documents by `max_wait`,
`max_documents` and `max_bytes`, then issues unordered MongoDB `insert_many`.
A request succeeds only after every record in its submitted set is durable. Queue
saturation, timeout or MongoDB failure returns an explicit retryable failure. A lost
MongoDB response can be reconciled for the same submitted `LogRecord`: an existing
identical record is success and conflicting content fails closed. If the SDK
redelivers the Log as a new request, at-least-once semantics permit a duplicate.

Logs are not grouped into Issues and never update `issues` or `issue_stats_hourly`.

## Micro-batching

One logical Log is one MongoDB document. Multiple Logs are combined only in bounded
MongoDB `insert_many`/bulk operations using the Phase 24 Log-lane policy:

```text
max_wait
max_documents
max_bytes
one in-flight batch per writer task
```

Packing many Logs into an array inside one BSON document is rejected because it
breaks individual identity, cursor pagination, TTL, Trace correlation, search,
bounded updates and failure isolation.

## Initial indexes

Only measured query shapes receive indexes.

### Project time feed

```javascript
{ p: 1, o: -1, _id: -1 }
```

This supports stable cursor pagination. It is the principal unavoidable secondary
index.

### Trace correlation

```javascript
{ p: 1, g: 1, o: 1 }
```

with a partial filter for documents containing `g`.

### Message search

Generation 8 performs escaped case-insensitive contains matching on `m` inside a
required project/time window. There is no text index. MongoDB first uses the
`log_project_time` range and applies the message predicate to candidates.

ADR-0044 must measure keys/documents examined and search latency on
production-shaped retained data. If the production query-cost gate fails, the
capability must be bounded further or replaced by an accepted indexed/search-engine
design before the production declaration.

### Retention

Retention uses `x` and the `log_expiry` TTL index.

Every index definition is centralized in the MongoDB adapter and included in
schema-generation validation. Wildcard indexes are prohibited.

## Query surface in Phase 24

The initial Logs product supports:

- project and time range;
- normalized severity;
- message search within the documented MongoDB semantics;
- exact environment, release and service filters;
- Trace ID filtering/correlation and Span ID projection on detail;
- stable cursor pagination;
- Log detail with decoded residual attributes.

Phase 24 does not promise arbitrary group-by, arbitrary attribute regex/substring
search or arbitrary MongoDB paths. The deferred Unified Explore backlog item may
later choose promoted dimensions, derived buckets or another accepted
search/analytics backend based on production Log/Span measurements. Future query
convenience cannot force every Log to carry an unbounded expanded BSON attribute
object.

## Retention, deletion and current limits

Logs have an independent global hot-retention duration and are registered in bounded
project deletion. They share the accepted request/item size boundary and batch
settings while using an independent bounded writer channel. A Log overload cannot
consume the Error writer channel.

Per-project stored-byte quotas, separate Log rate/byte quotas and Log cold archival
are not implemented in generation 8. They remain production-hardening findings or
future backlog and must not be advertised as current capability.

## Storage-budget verification

Golden BSON fixtures cover at least:

- minimal `Info` Log with required body;
- every severity code;
- Trace-correlated Log;
- small common attributes;
- environment/release/service projections;
- maximum uncompressed accepted body;
- deterministic duplicate, ambiguous-response retry and identity-conflict forms;
- ordinary retention and project-deletion registration.

The Phase 24 report publishes:

- logical BSON bytes per fixture;
- WiredTiger compressed collection bytes;
- `_id` and every secondary-index byte contribution;
- replication multiplier used in the estimate;
- CPU cost of body encode/decode/compression;
- sustained and burst write throughput;
- bounded message-search latency during ingest;
- retention interference;
- Error ingest and investigation regression results.

No claimed byte saving may remove required durability, PII processing, individual Log
identity or documented search correctness.

## Consequences

- Common Logs remain small and do not pay for Error-only fields.
- Message feed/search and Trace correlation avoid residual-body decoding.
- Arbitrary attributes remain compact and forward-compatible.
- Exact custom-attribute search is not currently exposed.
- MongoDB cannot immediately aggregate by every arbitrary attribute.
- Bounded regex message-search cost must pass ADR-0044 or be replaced/limited.
- A future search backend or time-series collection can be introduced behind accepted
  ports without changing the Log domain or API identity.
