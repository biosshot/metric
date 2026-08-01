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
7. A transaction Envelope item is normalized into a bounded terminal root/child set
   before acknowledgement and written idempotently by the dedicated Span writer.
8. High-resolution start time and duration use signed BSON `int64` nanoseconds.
9. Arbitrary attributes remain in a required versioned bounded accepted body.
10. The model introduced in generation 8 and retained by current generation 19
    promotes only fixed query fields and has no arbitrary-attribute search-token
    index.
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
identity conflict fails closed. The earlier proposed pending `q` state, recovery index
and separate `SpanProcessor` finalization are superseded and intentionally absent from
the accepted model below. Derived hourly aggregates remain best effort after Span
durability and are repaired by the bounded rebuild operation.

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
    operation: Box<str>,
    status: Box<str>,
    name: Box<str>,
    environment: Option<Box<str>>,
    release: Option<Box<str>>,
    service: Option<Box<str>>,
    insight_flags: u32,
    body: SpanBody,
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

  x,   // hot TTL time, BSON date

  g,   // 16-byte Trace ID
  n,   // 8-byte Span ID
  a,   // 8-byte parent Span ID, optional

  t,   // segment/root marker, present only when true
  c,   // normalized operation-class code
  w,   // bounded original operation string
  v,   // bounded normalized/original status string
  m,   // normalized display name
  e,   // environment, optional
  u,   // release, optional
  j,   // service, optional
  i,   // Performance Insight bitset, BSON int64
  b    // required versioned bounded accepted Span body
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
- `r` and `x` remain BSON dates because they serve operational ordering and
  retention.

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

The original bounded `span.op` is projected to `w` and also remains in the accepted
body. Numeric `c` codes are never reused. New classes require schema/fixture review;
arbitrary SDK operation strings do not become new enum values.

## Status

`v` stores the bounded status string and is required, using an empty string when the
accepted input has no status. Unknown input is not silently changed to success.

Original accepted status metadata may remain in `b` where the compatibility contract
requires preservation.

## Name

`m` contains the bounded normalized transaction/span display name used by Trace and
performance views. The complete accepted item remains in `b`, so the projection is a
deliberate bounded duplication.

Normalization, source-specific transaction-name rules, PII handling, maximum bytes,
empty-name behavior and truncation/rejection semantics are part of the Phase 25
contract.

## Query projections and arbitrary attributes

The model introduced in generation 8 and retained by current generation 19 promotes
operation class `c`, original operation `w`, name `m`, environment `e`, release `u`
and service `j`. It does not store `k` exact-search tokens and does not create an
arbitrary-attribute multikey index. Other attributes remain only in `b`.

Adding indexed arbitrary attributes requires a later schema-generation, storage and
query-cost decision.

## Residual Span body

`b` uses a Span-specific versioned binary header:

```text
byte 0: Span body format version
byte 1: body codec
remaining bytes: encoded residual body
```

Current generation 19 retains body format `1`, codec `0` and bounded accepted JSON
introduced in generation 8. Span-body compression is not enabled.

`b` may contain:

- bounded attributes and measurements;
- original `span.op`;
- environment, release and service metadata;
- SDK/resource metadata;
- bounded Span links;
- sampling/dynamic-sampling context accepted from Sentry SDKs;
- profile/reference metadata;
- protocol fields preserved for forward compatibility.

`b` contains the complete bounded scrubbed accepted Span/Transaction item and is
required. Top-level query/display projections are deliberately duplicated and their
cost is pinned by BSON fixtures.

## Terminal normalization and child expansion

Before entering the Span writer, ingest enforces:

- compressed and decoded item bytes;
- maximum children per transaction;
- maximum aggregate expanded child bytes;
- per-Span attribute/link/measurement limits;
- maximum total expanded child/body bytes implied by expansion.

Ingest validates and scrubs the root and children, calculates deterministic
`SpanRecordId` values, builds terminal residual bodies and computes bounded
per-segment Insight enrichment. The dedicated bounded Span writer combines terminal
records by `max_wait`, `max_documents` and `max_bytes`, then issues unordered MongoDB
`insert_many`.

A request succeeds only after every submitted root/child record is durable. If a
connection fails after only some children are inserted, the SDK retry derives the
same identities, verifies existing children and inserts the missing set. A standalone
Span that duplicates a child follows the same identity rule.

The implementation publishes deterministic conflict semantics when two deliveries
with the same natural identity contain different normalized content. It never
last-write-wins silently.

## Lanes and micro-batching

Spans use a dedicated bounded lane and policy:

```text
queue documents
derived byte ceiling from bounded record size
max_wait
max_documents
max_bytes
one in-flight batch per writer task
operation timeout
```

Values use validated ingest batch bounds in generation 8 and the Span writer has its
own channel/task. Span load cannot borrow Error/Log channel capacity, Error
Symbolicator reservations or Blob processing reservations.

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

### Retention

Current generation 19 uses `x` and the `span_expiry` TTL index. It also includes the
optional Span cold-archive state introduced after generation 8: with archival
enabled a new Span receives `h`, and only a completed, verified homogeneous Span
archive segment may replace it with `z` and `x`, following ADR-0007. Transactions
are root/segment Span records and use the same archive. Trace remains virtual and
has no separate archive.

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
  k,   // bounded transaction/root name
  v,   // service, optional
  e,   // environment, optional
  u,   // release, optional
  c,   // normalized operation-class code
  g,   // representative Trace ID
  n,   // count
  f,   // failure count
  s,   // summed duration nanoseconds
  d,   // at most 2,048 most-recent duration samples
  x    // retention deadline
}
```

Exact compact names/codecs are fixed by the Phase 26 storage fixtures.

Current generation 19 retains the deterministic bucket introduced in generation 8
for the combined bounded dimensions project, UTC hour, root name, service,
environment, release and operation class. It does not create buckets for arbitrary
user attributes.

Percentiles use nearest rank over the most recent at most 2,048 samples in `d`.
Approximation/sample-limit semantics are visible in API metadata. These samples are
investigative, not a mergeable billing-grade histogram.

## Aggregate consistency

`span_stats_hourly` is derived and rebuildable; `spans` is the source of truth.

The one-process no-transaction policy may rarely double-apply or miss a bucket update
around a crash depending on finalization order. Phase 26:

- chooses and documents one ordering;
- publishes the resulting approximation semantics;
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

## Insight flags

`i` is a required append-only bitset on every terminal Span; zero means that no rule
matched. Initial flags include:

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

Current generation 19 retains the compact flags-only model introduced in generation
8; Web/API render stable rule labels from them. There is no separate persisted
Insight-explanation object or automatic historical backfill. Newly processed
transaction items derive segment-local Insights while their accepted children are
available together. The implementation does not wait for or repeatedly rescan an
unbounded global distributed Trace.

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
- project/environment/time filters supported by fixed projections;
- release is stored in `u`; the generation-8 segment query currently targets `v`
  (status) and is an explicit ADR-0044 production blocker until corrected and
  regression-tested.

Phase 26 provides:

- transaction/service/operation summaries;
- throughput, failure and percentile timeseries;
- slow/repeated operation views;
- segment Insight list/detail;
- representative Trace links;
- explicit approximation/partial-data indicators.

The deferred Unified Explore backlog item may later supply a common query AST.
Phases 25/26 do not expose arbitrary MongoDB fields, aggregations, regex or unbounded
group-by.

## Retention and deletion

Spans and hourly aggregates have independent configurable retention. Project deletion
registers both namespaces. The next breaking schema generation also registers the
Span archive namespace.
Trace views naturally become partial as constituent retention periods expire.

## Test and performance gates

Phase 25 fixtures cover:

- minimal root and child Spans;
- remote-parent segment;
- high-resolution ordering;
- every operation/status code;
- missing parent, orphan and cycle inputs;
- duplicate and conflicting natural identities;
- transaction child expansion and deterministic identity;
- maximum bounded transaction;
- required uncompressed versioned body;
- partial batch insert, ambiguous-response retry and identity conflict.

Phase 25 publishes:

- BSON and every index byte contribution;
- expansion CPU/memory;
- retained in-process Span-writer throughput and batch occupancy;
- restart and ambiguous-response retry recovery;
- bounded small/large/partial Trace functional behavior;
- saved real-SDK smoke flow.

ADR-0044 originally assigned sustained/burst mixed ingest, production-shaped
Trace-read, dependency-failure and soak evidence to Phase 27. ADR-0047 later closed
that separate program as obsolete; it is no longer an active Phase 25 dependency.

Phase 26 fixtures/gates cover:

- bucket identity and merge;
- bounded recent-sample percentile behavior;
- cardinality attacks;
- crash approximation/rebuild;
- deterministic Insight rules;
- aggregate/Insight work remaining behind foreground durability.

ADR-0044 owns performance queries under ingest and production-shaped aggregate
rebuild/interference measurements.

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
