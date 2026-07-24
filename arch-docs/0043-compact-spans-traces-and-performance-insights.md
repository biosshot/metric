# ADR-0043: Compact Spans, virtual Traces and Performance Insights

- Status: Accepted
- Date: 2026-07-24

## Context

Phase 25 adds Sentry-compatible Transactions and Spans after Structured Logs. Phase 26
adds performance aggregates and explicit Insights. These capabilities must reuse the
accepted vertical signal pipeline while isolating Span volume from Errors and Logs.

A Sentry Transaction is a local root/segment operation containing or relating to
child Spans. A distributed Trace is not one atomic SDK item: its segments may arrive
from different services/projects, out of order, be sampled, expire independently or
never arrive.

Persisting separate Transaction, Span and Trace copies would duplicate data and create
cross-document consistency work. Conversely, embedding every child Span forever
inside one Transaction document would prevent individual search, correlation,
retention and bounded Trace investigation.

## Decisions

1. Transactions normalize to root/segment Span records.
2. Root and child Spans live in one `spans` collection.
3. There is no `transactions` collection.
4. There is initially no `traces` or `trace_summaries` collection.
5. A Trace is a bounded authorized view assembled from records sharing a Trace ID.
6. One terminal Span is one individually addressable MongoDB document.
7. A transaction Envelope item is durably accepted as one pending root record and is
   expanded idempotently by `SpanProcessor`.
8. High-resolution start time and duration use signed BSON `int64` nanoseconds.
9. Arbitrary attributes remain in an optional versioned residual body.
10. Only bounded accepted exact dimensions receive search tokens.
11. Performance aggregates live in rebuildable `span_stats_hourly`.
12. Per-segment Insight flags are optional derived enrichment on the root Span.
13. Initial automatic Insights operate within one transaction/segment and its accepted
    children, not an eventually arriving global distributed Trace.
14. OpenTelemetry ingestion remains disabled.

## Implementation amendment: terminal idempotent expansion

Phase 25 found an explicit contradiction between the proposed pending-root lifecycle
and the implemented bounded synchronous normalizer. The root and bounded child array
can be validated, scrubbed and expanded before acknowledgement without an external
processor dependency.

The accepted implementation writes terminal child/root Span records directly and
acknowledges only after every insert succeeds. Deterministic project/Trace/Span
identities make a retry complete a partially written expansion without duplicates; an
identity conflict fails closed. The pending `q` state, recovery index and separate
`SpanProcessor` finalization described below are superseded for Phase 25. Derived
hourly aggregates remain best effort after Span durability and are repaired by the
bounded rebuild operation.

## Collections

Phase 25 creates:

```text
spans
```

Phase 26 additionally creates:

```text
span_stats_hourly
```

Both collections contain all projects in the one accepted application database.
Neither is created per project.

## Domain model

The normalized domain record is conceptually:

```rust
struct SpanRecord {
    id: SpanRecordId,
    project_id: ProjectId,
    received_at: Timestamp,
    started_at_ns: UnixNanoseconds,
    duration_ns: DurationNanoseconds,
    trace_id: TraceId,
    span_id: SpanId,
    parent_span_id: Option<SpanId>,
    is_segment: bool,
    operation_class: SpanOperationClass,
    status: SpanStatus,
    name: SpanName,
    search_tokens: BoundedSearchTokens,
    insight_flags: InsightFlags,
    residual: Option<SpanBody>,
}
```

Supported wire-format fields are fixed by exact Sentry SDK/Relay fixtures during
Phase 25. The internal domain is not shaped around OpenTelemetry payloads.

## Identity

The natural identity is:

```text
project ID + Trace ID + Span ID
```

MongoDB `_id` is a deterministic 16-byte domain-separated BLAKE3 digest of that
natural identity:

```text
SpanRecordId = BLAKE3(
  "span-record/v1" ||
  project_id ||
  trace_id ||
  span_id
)[0..16]
```

The product name is not embedded in the domain-separation literal. The neutral schema
literal is stable after Phase 25 writes data.

On duplicate `_id`, the adapter verifies `p`, `g` and `n`:

- identical natural identity is an idempotent duplicate;
- different natural identity is a critical integrity collision and fails closed;
- an existing record is never overwritten on mismatch.

The digest layout and byte order have permanent golden fixtures. Span ID alone is not
a MongoDB identity because it is not project/Trace scoped.

## Terminal Span BSON

The conceptual processed document is:

```javascript
{
  _id, // 16-byte deterministic SpanRecordId
  p,   // project ID, BSON int32
  r,   // server receive time, BSON date

  o,   // start as Unix nanoseconds, BSON int64
  d,   // duration nanoseconds, BSON int64

  x,   // hot TTL time, only when eligible
  h,   // archive due time, only while awaiting archive
  z,   // archive segment ID, only after archive commit

  g,   // 16-byte Trace ID
  n,   // 8-byte Span ID
  a,   // 8-byte parent Span ID, optional

  t,   // segment/root marker, present only when true
  c,   // normalized operation-class code
  v,   // non-default normalized status code, optional
  m,   // normalized display name

  k,   // bounded exact-search tokens, optional
  i,   // Performance Insight bitset, optional
  s,   // non-default PII policy revision, optional
  q,   // pending/retry/permanent-failure state, absent after success
  b    // optional versioned residual body
}
```

Optional values are omitted, not BSON `null`. The collection already identifies the
signal, so no generic event-type field exists.

`t` means local root/segment, not necessarily the root of the complete distributed
Trace. A segment may have a remote parent and therefore contain both `t` and `a`.

## Time representation

Trace waterfalls require finer precision than BSON Date milliseconds:

- `o` stores checked Unix nanoseconds as BSON `int64`;
- `d` stores checked non-negative duration nanoseconds as BSON `int64`;
- end time is `o.checked_add(d)`;
- `r`, `x` and `h` remain BSON dates because they serve operational ordering,
  retention and archive scheduling.

Admission rejects timestamps/durations outside accepted ranges before arithmetic or
allocation. Clock-drift correction preserves bounded source metadata in `b` and cannot
bypass old/future item limits.

## Operation class

`c` is an append-only compact enum used for safe common filtering and Insights:

```rust
enum SpanOperationClass {
    Other = 0,
    HttpServer = 1,
    HttpClient = 2,
    Database = 3,
    Cache = 4,
    Queue = 5,
    File = 6,
    Rpc = 7,
    Function = 8,
    Task = 9,
    Ui = 10,
    Resource = 11,
}
```

The original bounded `span.op` remains in `b` and may receive an exact token in `k`.
Numeric codes are never reused. New classes require schema/fixture review; arbitrary
SDK operation strings do not become new enum values.

## Status

`v` stores an append-only normalized Sentry Span status code. The physical default
and distinction between unset, unknown and success are fixed by exact supported SDK
fixtures. Unknown input is not silently changed to success.

Original accepted status metadata may remain in `b` where the compatibility contract
requires preservation.

## Name

`m` contains the bounded normalized transaction/span display name used by Trace and
performance views. It is not duplicated inside `b`.

Normalization, source-specific transaction-name rules, PII handling, maximum bytes,
empty-name behavior and truncation/rejection semantics are part of the Phase 25
contract.

## Exact-search tokens

`k` follows ADR-0023 and ADR-0042:

- tokens are domain-separated by Span attribute/dimension;
- original key/type/value remains in `b`;
- a token result is verified after body decode;
- token count and bytes are bounded before write;
- arbitrary attributes are not automatically indexed.

Initial candidates include:

```text
environment
release
service.name
span.op
transaction name
http.request.method
http.response.status_code
db.system
db.operation.name
server.address
```

The final built-in list and optional per-project allowlist are accepted only after
index/storage measurements. Arrays, nested values and high-cardinality values do not
silently multiply index entries.

## Residual Span body

`b` uses a Span-specific versioned binary header:

```text
byte 0: Span body format version
byte 1: body codec
remaining bytes: encoded residual body
```

Initial codecs are canonical JSON and adaptive Zstandard using the same fail-closed
decoded-size principles as ADR-0022/ADR-0042.

`b` may contain:

- bounded attributes and measurements;
- original `span.op`;
- environment, release and service metadata;
- SDK/resource metadata;
- bounded Span links;
- sampling/dynamic-sampling context accepted from Sentry SDKs;
- profile/reference metadata;
- protocol fields preserved for forward compatibility;
- Phase 26 Insight explanations.

`b` does not duplicate:

- project, Trace, Span or parent IDs;
- start/duration;
- normalized operation class/status/name;
- retention/archive/processing state.

If no residual data remains, a terminal Span omits `b`.

## Pending standalone Span

A standalone Span is durably accepted as:

```javascript
{
  _id,
  p,
  r,
  o,
  g,
  n,
  q: {
    s, // pending/retry or permanently failed
    a, // attempts
    n, // next attempt time
    c  // optional numeric failure code
  },
  b // scrubbed accepted source payload
}
```

The typed RAM lane avoids an immediate reread. The dispatcher reloads `b` after
restart or when foreground lane admission was unavailable.

## Pending Transaction and child expansion

A supported Sentry transaction item is accepted as one pending root record. Its `b`
contains the scrubbed accepted root and bounded child array. The transaction's root
Trace/Span identity determines `_id`, and `t` identifies the record as a segment.

Before durable acknowledgement, ingest enforces:

- compressed and decoded item bytes;
- maximum children per transaction;
- maximum aggregate expanded child bytes;
- per-Span attribute/link/measurement limits;
- maximum total attributes/tokens implied by expansion.

`SpanProcessor`:

1. validates and normalizes the root and children;
2. calculates deterministic child `SpanRecordId` values;
3. bulk-inserts/upserts terminal child Span documents with identity verification;
4. computes accepted per-segment Phase 26 enrichment when enabled;
5. updates rebuildable aggregate work according to its separate policy;
6. replaces the pending root body with terminal residual data;
7. sets retention/archive state and removes `q`.

If the process fails after only some children are inserted, retry derives the same
identities, verifies existing children and inserts the missing set. A standalone Span
that duplicates a child follows the same identity rule.

The implementation publishes deterministic conflict semantics when two deliveries
with the same natural identity contain different normalized content. It never
last-write-wins silently.

## Lanes and micro-batching

Spans use a dedicated bounded lane and policy:

```text
queue documents
queue bytes
max_wait
max_documents
max_bytes
max_in_flight_batches
foreground/backlog scheduling weight
```

Values are configurable within accepted bounds and fixed by Phase 25 load tests. Span
load cannot borrow Error/Log lane capacity, Error Symbolicator reservations or Blob
processing reservations.

One terminal Span remains one document. Bulk operations combine writes, not logical
Span ownership.

## Initial `spans` indexes

### Trace assembly

```javascript
{ p: 1, g: 1, o: 1, n: 1 }
```

All Trace queries include an authorized bounded set of project IDs and Trace ID.

### Root/segment feed

```javascript
{ p: 1, t: 1, o: -1, _id: -1 }
```

with a partial filter for `t = true`, so child Spans do not occupy the index.

### Exact dimensions

The multikey token index:

```javascript
{ p: 1, k: 1, o: -1 }
```

is enabled only when its bounded feature and benchmark pass. Operation-class indexing
is added only for a measured accepted query shape.

### Pending recovery

```javascript
{ "q.n": 1, _id: 1 }
```

with a partial pending/retry filter.

### Retention

Retention/archive uses `x`, `h` and `z` under the accepted Scheduler/archive protocol.
Span archive output has its own project/day schema and namespace.

No wildcard index is created.

## Virtual Trace assembly

There is initially no durable Trace document. A Trace is assembled by:

1. authorizing the organization/project scope;
2. querying `spans` by the bounded project set and Trace ID;
3. ordering by start time and stable identity;
4. constructing a bounded `span_id -> record` map;
5. linking accepted parent IDs;
6. preserving orphan/missing-parent segments visibly;
7. breaking malformed cycles with diagnostic status;
8. fetching bounded correlated Error and Log references;
9. returning a partial marker/count when response limits truncate data.

Conceptually:

```rust
struct TraceView {
    trace_id: TraceId,
    spans: Vec<TraceSpan>,
    errors: Vec<ErrorReference>,
    logs: Vec<LogReference>,
    partial: bool,
    omitted_spans: u32,
}
```

Configuration bounds:

- Spans per Trace response;
- response bytes;
- correlated Error references;
- correlated Log references;
- authorized projects per cross-project query;
- body decodes and concurrent Trace queries.

A missing/sampled/expired Span is normal partial telemetry, not database corruption.
`trace_summaries` is considered only after Trace read/load measurements prove the
indexed virtual view insufficient.

## Error and Log correlation projections

Phase 25 extends `error_events` with optional:

```text
g = 16-byte Trace ID
n = 8-byte Span ID
```

and a project/Trace partial index accepted by benchmark. These fields are present only
when valid correlation context exists and are not duplicated in the Error body.

ADR-0042 already reserves the same physical concepts in `logs`. Each collection owns
its physical constants, but domain Trace/Span ID types and byte order are shared.

Trace lookup never queries a Trace ID without authorized project/organization scope.
No retroactive correlation is promised for expired or historical records that lack
the new projection.

## `span_stats_hourly`

Phase 26 adds rebuildable hourly aggregates. A conceptual bucket is:

```javascript
{
  _id, // deterministic bucket identity
  p,   // project
  h,   // UTC hour
  t,   // accepted dimension/rollup type
  k,   // bounded dimension identity
  v,   // bounded display value where required
  c,   // count
  f,   // failure count
  s,   // summed duration nanoseconds
  d,   // versioned bounded duration histogram/sketch
  x    // retention deadline
}
```

Exact compact names/codecs are fixed by the Phase 26 storage fixtures.

Accepted rollups are finite and configuration-bounded, initially selected from:

- project total;
- environment;
- release;
- transaction name;
- service;
- operation class;
- service plus operation class.

No bucket is automatically created per arbitrary user attribute or arbitrary
dimension combination.

The duration distribution format has versioned merge/golden fixtures and supports the
published percentile set. Approximation/extrapolation semantics are visible in API
metadata.

## Aggregate consistency

`span_stats_hourly` is derived and rebuildable; `spans` is the source of truth.

The one-process no-transaction policy may rarely double-apply or miss a bucket update
around a crash depending on finalization order. Phase 26:

- chooses and documents one ordering;
- publishes the resulting approximation semantics;
- records aggregate version/watermark where required;
- provides bounded range rebuild/reconciliation;
- keeps aggregate work behind foreground Span durability;
- never rejects or corrupts a durable Span because a derived bucket failed.

No claim of exact billing/accounting semantics is made from these buckets.

## Performance Insights

Phase 26 derives:

For root/segment Spans:

- throughput;
- failure rate/count;
- average duration;
- accepted p50/p75/p90/p95/p99;
- slow-segment rate;
- release/environment/service comparisons.

For child Spans:

- slow database/cache/HTTP/queue operations;
- repeated operations within one segment;
- total time by operation class;
- bounded N+1 database candidates;
- repeated downstream-call candidates;
- long task/queue-latency candidates where input data is sufficient.

These are explicit deterministic rules, not ML.

## Insight flags and explanations

`i` is an optional append-only bitset on a root/segment Span. Initial candidate flags
include:

```text
slow segment
N+1 database candidate
repeated HTTP candidate
slow database operation
cache-miss/repetition candidate
queue latency
long task
failed downstream operation
```

Exact bit assignments receive permanent golden fixtures and are never reused.

When a flag is present, a bounded explanation may be stored in the root residual
body, for example type, count, representative operation and total duration. `i` is
absent when there is no accepted Insight.

Newly processed transaction items can derive segment-local Insights before root
finalization because their accepted children are available together. Phase 26 may
provide a bounded versioned backfill for retained roots. It does not wait for or
repeatedly rescan an unbounded global distributed Trace.

Cross-service/global Trace Insights require a later accepted trace-finalization design
and are deliberately deferred.

## Query and Web surfaces

Phase 25 provides:

- root transaction feed;
- Trace lookup by ID;
- bounded waterfall/tree;
- orphan/partial diagnostics;
- links among Span, Error and Log records;
- Span detail with decoded attributes/measurements;
- project/environment/release/time filters supported by accepted projections/tokens.

Phase 26 provides:

- transaction/service/operation summaries;
- throughput, failure and percentile timeseries;
- slow/repeated operation views;
- segment Insight list/detail;
- representative Trace links;
- explicit approximation/partial-data indicators.

Phase 27 Unified Explore later supplies the common query AST. Phase 25/26 do not
expose arbitrary MongoDB fields, aggregations, regex or unbounded group-by.

## Retention, archive and deletion

Spans and hourly aggregates have independent configurable retention. Project deletion
registers both namespaces. Archive uses Span-specific project/day segments:

```text
archive/spans/<project>/<day>/...
```

Hot expiry is assigned only after a complete accepted archive manifest/object.
Trace views naturally become partial as constituent retention periods expire.

## Test and performance gates

Phase 25 fixtures cover:

- minimal root and child Spans;
- remote-parent segment;
- high-resolution ordering;
- every operation/status code;
- missing parent, orphan and cycle inputs;
- duplicate and conflicting natural identities;
- transaction child expansion crash points;
- maximum bounded transaction;
- compressed/uncompressed/absent residual body;
- pending/retry/permanent-failure and archive states.

Phase 25 publishes:

- BSON and every index byte contribution;
- expansion CPU/memory;
- steady/burst Span ingest;
- mixed Error/Log/Span isolation;
- backlog/restart recovery;
- small/large/partial Trace read latency and memory;
- distributed real-SDK E2E across at least two services;
- Error and Log regression results.

Phase 26 fixtures/gates cover:

- bucket identity and merge;
- histogram/sketch accuracy and compatibility;
- cardinality attacks;
- crash approximation/rebuild;
- deterministic Insight rules;
- performance queries during ingest;
- aggregate/Insight work remaining behind foreground durability.

## Consequences

- Transaction and Span storage is unified without a generic all-signal collection.
- Individual child Spans are searchable and independently retainable.
- Distributed Traces require no mutable cross-service Trace document.
- Trace reads are naturally partial and explicitly bounded.
- Compact deterministic identities reduce `_id` and secondary-index weight.
- Nanosecond waterfall precision does not require duplicating end timestamps.
- Arbitrary attributes remain compact but are not immediately arbitrary group-by
  dimensions.
- Performance statistics are useful and rebuildable, not exact accounting.
- Global cross-service automatic Insights remain deferred until measurements justify
  a trace-finalization subsystem.
