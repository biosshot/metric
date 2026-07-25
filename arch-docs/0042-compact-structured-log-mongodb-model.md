# ADR-0042: Compact Structured Log MongoDB model

- Status: Accepted
- Date: 2026-07-24

## Context

Phase 24 adds Sentry-compatible Structured Logs as the first new high-volume signal
after the Error Monitoring MVP. Logs need independent indexes, retention, quotas and
backlog isolation, but must reuse the accepted durable ingest/Processor shape without
copying the Error grouping pipeline.

At the 100-million-record/day design target, expanded descriptive BSON and automatic
indexing of arbitrary attributes would dominate storage and index cost. At the same
time, storing the complete Log only as compressed binary would make message search,
time feeds and Trace correlation impractical.

The model therefore separates a small deliberately queryable projection from an
optional versioned residual body.

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
The `q` pending fields, pending recovery index and `LogProcessor` finalization steps
described below are superseded for Phase 24. They remain a possible later design only
if Log processing gains an asynchronous dependency that cannot execute before
acknowledgement.

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

`LogId` is generated server-side as a time-sortable 128-bit identifier with sufficient
randomness for concurrent generators. It is not a Sentry Error `event_id`, Trace ID or
Span ID. Its exact bit layout, monotonicity behavior and golden fixtures are fixed by
the Phase 24 implementation contract before data is written.

MongoDB stores `LogId` as 16-byte binary `_id`. Stable feed order is
`occurred_at, LogId`, not timestamp alone.

## Terminal BSON document

The conceptual processed document is:

```javascript
{
  _id, // 16-byte LogId
  p,   // project ID, BSON int32
  r,   // server receive time, BSON date
  o,   // Log occurrence time, BSON date

  x,   // hot TTL time, only when eligible
  h,   // archive due time, only while awaiting archive
  z,   // archive segment ID, only after archive commit

  l,   // non-default normalized severity code
  g,   // 16-byte Trace ID, optional
  n,   // 8-byte Span ID, optional
  m,   // normalized display/search message
  k,   // bounded exact-search tokens, optional
  s,   // non-default PII policy revision, optional
  q,   // pending/retry/permanent-failure state, absent after success
  b    // optional versioned residual body
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
  m
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

`Info` is the physical default and omits `l`. Codes are never reused with another
meaning. Input aliases and unknown values are decided by exact supported SDK fixtures:

- accepted aliases normalize to one known code;
- an unknown source value is never silently presented as `Info`;
- if the owning compatibility contract permits preservation, the bounded original
  value remains in `b`;
- otherwise the item is rejected with a stable protocol error/outcome.

## Timestamps

`r` is server receive time and `o` is the accepted occurrence time. Both are BSON
dates for indexed range queries and retention scheduling.

If a supported SDK provides precision finer than BSON milliseconds, the bounded
original timestamp representation may remain in `b`. Feed ordering remains
deterministic through `_id`; sub-millisecond source precision is not turned into an
additional mandatory BSON field without a benchmarked query requirement.

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

## Exact-search tokens

`k` reuses the ADR-0023 domain-separated exact-token approach. A token represents an
accepted tuple such as:

```text
log-attribute domain + normalized attribute key + normalized type + normalized value
```

The original key and value remain in `b`. A token match is a candidate and is verified
against decoded residual data before returning the Log, preserving correctness in the
theoretical hash-collision case.

Tokens are not created for every attribute automatically. Phase 24 defines:

- a small built-in allowlist of useful dimensions;
- an optional bounded per-project allowlist;
- maximum indexed attributes per Log;
- maximum token count and bytes per Log;
- supported scalar value types;
- behavior for arrays and unsupported/nested values;
- cardinality/discard metrics.

Initial built-in candidates are environment, release, service name, logger name,
server address and database system. The final list is fixture- and benchmark-driven;
adding a field requires an index/storage cost decision.

Arbitrary user-controlled keys do not create MongoDB field paths, indexes or
collections.

## Residual body

`b` is optional. It stores structured attributes and supported metadata not already
represented by `_id`, `p`, `r`, `o`, `l`, `g`, `n` or `m`.

The binary header is:

```text
byte 0: Log body format version
byte 1: body codec
remaining bytes: encoded residual body
```

Initial codecs follow the proven Event-body policy:

```text
0 = canonical JSON
1 = Zstandard-compressed canonical JSON
```

Compression is used only when it saves at least the configured accepted threshold.
The canonical decoded byte limit is enforced before and during decompression. A
compression bomb fails closed.

`b` may contain:

- bounded structured attributes;
- supported SDK/source metadata;
- original sub-millisecond timestamp representation;
- bounded original severity text when the compatibility contract preserves it;
- supported unknown forward-compatible fields;
- context not selected as a top-level query projection.

`b` does not duplicate:

- message;
- normalized severity;
- Trace ID or Span ID;
- project ID;
- receive/occurrence timestamps;
- retention/archive state.

If no residual data remains, a terminal Log omits `b`.

## Pending and finalization lifecycle

The durable accepted/pending form is conceptually:

```javascript
{
  _id,
  p,
  r,
  o,
  q: {
    s, // 0 = pending/retry, 1 = permanently failed
    a, // attempts
    n, // next attempt time for pending/retry
    c  // optional bounded numeric failure code
  },
  b // scrubbed accepted source payload
}
```

The existing RAM lane may retain the accepted typed payload to avoid an immediate
MongoDB read. When the lane is full or after restart, the Log dispatcher loads the
pending `b` from MongoDB.

`LogProcessor`:

1. validates and normalizes the accepted payload;
2. extracts the canonical message into `m`;
3. maps severity to `l`;
4. extracts binary Trace/Span IDs into `g` and `n`;
5. creates only the accepted bounded `k`;
6. replaces `b` with the normalized residual body or removes it;
7. sets retention/archive fields;
8. removes `q` atomically with the terminal projection.

The accepted and terminal payload are not retained as two complete durable copies.
There is no `processing` state in the one-process runtime. Local lane/running sets
prevent concurrent work and pending durable records remain recoverable after a crash.

Logs are not grouped into Issues and never update `issues` or `issue_stats_hourly`.

## Micro-batching

One logical Log is one MongoDB document. Multiple Logs are combined only in bounded
MongoDB `insert_many`/bulk operations using the Phase 24 Log-lane policy:

```text
max_wait
max_documents
max_bytes
max_in_flight_batches
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
{ p: 1, g: 1, o: 1, _id: 1 }
```

with a partial filter for documents containing `g`.

### Exact attribute candidates

```javascript
{ p: 1, k: 1, o: -1 }
```

This multikey index is created only when the accepted indexed-attribute feature is
enabled and its storage/load benchmark passes. Token count is bounded before write.

### Message search

The initial Mongo-only implementation may use one compound text index:

```javascript
{ p: 1, m: "text" }
```

with technical-text configuration such as no natural-language stemming where the
supported MongoDB version permits it.

Its exact semantics, storage cost and limitations are published. Regex/substring
collection scans are not a fallback. If the text index violates the storage or
search-under-ingest gate, capability advertisement must expose the reduced search
surface rather than enabling an unsafe query.

### Pending recovery

```javascript
{ "q.n": 1, _id: 1 }
```

with a partial filter for documents containing pending/retry `q`.

### Retention

Retention uses `x` and the already accepted controlled Scheduler/TTL policy. Archive
mode does not assign hot expiry until the Log archive manifest/object is complete.

Every index definition is centralized in the MongoDB adapter and included in
schema-generation validation. Wildcard indexes are prohibited.

## Query surface in Phase 24

The initial Logs product supports:

- project and time range;
- normalized severity;
- message search within the documented MongoDB semantics;
- accepted exact indexed attributes;
- Trace ID and Span correlation;
- stable cursor pagination;
- bounded time histogram;
- Log detail with decoded residual attributes.

Phase 24 does not promise arbitrary group-by, arbitrary attribute regex/substring
search or arbitrary MongoDB paths. Phase 27 Unified Explore separately chooses
promoted dimensions, derived buckets or another accepted search/analytics backend
based on real Log/Span measurements. Future query convenience cannot force every
Phase 24 Log to carry an unbounded expanded BSON attribute object.

## Retention, archive and quotas

Logs have configuration independent from Error Events:

- accepted items/second and bytes/second;
- maximum message, residual body, attribute count/depth/key/value and total item bytes;
- RAM lane documents and bytes;
- micro-batch documents and bytes;
- hot retention duration;
- optional archive eligibility;
- per-project stored-byte policy;
- indexed-attribute/token budget.

Outcomes account for both rejected Log item count and bytes where supported by the
compatibility contract. A Log overload cannot borrow Error lane capacity or Error
admission reservations.

Archive output uses Log-specific project/day segments and schema:

```text
archive/logs/<project>/<day>/...
```

It does not place Logs and Error Events in one sparse Parquet schema.

## Storage-budget verification

Golden BSON fixtures cover at least:

- minimal `Info` Log with no attributes;
- non-default severity;
- Trace-correlated Log;
- small common attributes;
- maximum indexed-token set;
- uncompressed residual body;
- compressed residual body;
- pending/retry/permanent-failure forms;
- archive and ordinary retention forms.

The Phase 24 report publishes:

- logical BSON bytes per fixture;
- WiredTiger compressed collection bytes;
- `_id` and every secondary-index byte contribution;
- replication multiplier used in the estimate;
- CPU cost of body encode/decode/compression;
- sustained and burst write throughput;
- message/exact-search latency during ingest;
- retention/archive interference;
- Error ingest and investigation regression results.

No claimed byte saving may remove required durability, PII processing, individual Log
identity or documented search correctness.

## Consequences

- Common Logs remain small and do not pay for Error-only fields.
- Message feed/search and Trace correlation avoid residual-body decoding.
- Arbitrary attributes remain compact and forward-compatible.
- Exact custom-attribute search is bounded and explicit rather than automatic.
- MongoDB cannot immediately aggregate by every arbitrary attribute.
- Text-index cost may be material and must be measured rather than hidden.
- A future search backend or time-series collection can be introduced behind accepted
  ports without changing the Log domain or API identity.
