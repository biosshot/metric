# ADR-0046: Application Metrics, Session Replay and deferred Profiling

- Status: Accepted
- Date: 2026-07-27
- Amends: ADR-0040 and ADR-0045 after completed Phase 36

## Context

Phase 36 completed the accepted lightweight product wave. The next product gap in
the core observability path is Application Metrics. Session Replay follows Metrics
as a separate high-volume Blob product. Profiling must not delay either phase.

Application Metrics are not only counters. The accepted first version supports:

- counters: accumulated change during an interval;
- gauges: last, minimum, maximum, sum and sample count during an interval;
- distributions: count, sum, minimum, maximum and a bounded quantile sketch.

Storing one permanent MongoDB document per logical counter was considered and
rejected. Such a document can expose only a lifetime/latest value, becomes a hot
concurrent update target, cannot represent a bounded time range correctly and cannot
expire old history independently. It is a key/value counter service, not a metrics
time series.

## Decision

### Phase 37 is Application Metrics

Phase 37 is the next product phase. It accepts only pinned Sentry SDK metric payload
rows and implements a separate bounded Metrics lane.

### Wire container versus internal representation

The pinned Sentry SDK sends one `trace_metric` Envelope item with content type
`application/vnd.sentry.items.trace-metric+json`. Its payload is a JSON object whose
`items` member is an array. This container exists only at the compatibility edge and
is not an internal domain or storage format.

The wire container cannot be removed or replaced with one Envelope item per
measurement without breaking Sentry SDK compatibility. It also amortizes Envelope
headers and HTTP framing across many measurements. Phase 37 does not introduce a
second custom single-measurement protocol.

Metric does not deserialize the wire array into a retained
`Vec<MetricMeasurement>`, copy the complete container into a queue or store it in
MongoDB. A bounded streaming JSON visitor validates `item_count` and folds each
measurement immediately into a request-local `MetricDeltaBatch` keyed by normalized
series and time slot:

```text
trace_metric JSON container
-> validate one item
-> normalize series/time slot
-> merge into MetricDeltaBatch
-> discard the raw item
```

The request body remains subject to the existing compressed/decompressed byte limits,
but no second full normalized representation is created. Structurally invalid
containers are rejected. Invalid individual measurements produce bounded discard
outcomes without preventing valid measurements in the same container from being
aggregated.

Only the compact delta map crosses the dedicated Metrics queue. If 1,000 wire
measurements address ten series/time slots, the queue receives at most ten aggregate
deltas rather than 1,000 measurement objects. The successful HTTP response still
waits until every accepted delta is durably applied.

The durable unit is one compact document per normalized metric series and time
bucket, not one document per submitted measurement:

```text
series = project + metric name + kind + unit + bounded normalized tags
bucket = series + bucket start + configured bucket width
```

Thousands or millions of measurements for the same series and interval are combined
in RAM and written as one bucket update. The initial implementation uses one
validated configurable bucket width with a bounded default. It does not add raw
measurement documents, a generic Event representation or a second database.

The physical collection is:

```text
metric_buckets
```

Documents use the established compact BSON field-name policy. A unique identity over
project, normalized series identity and bucket start makes retry behavior explicit.
The stored aggregate depends on metric kind:

- counter: sum;
- gauge: last, minimum, maximum, sum and count;
- distribution: count, sum, minimum, maximum and bounded sketch bytes.

Metric names, units and tags are length/count bounded before queue admission.
Per-project series cardinality budgets are mandatory. Rejected measurements produce
bounded discard outcomes and never create a collection or index entry.

Metrics have:

- a dedicated bounded RAM queue of aggregate deltas and a micro-batch writer;
- independent admission, overload, retention and archive settings;
- no dependency on the Error Processor and no use of Log or Span queues;
- a `metrics` dataset in the existing Explore boundary;
- reuse of existing Saved Queries, Dashboards and Alerts;
- optional exact correlation attributes such as `trace_id` when present.

The first implementation does not add a separate metric-series catalog, long-term
rollup collection, cross-project query or arbitrary unbounded tags. A series catalog
or packed multi-resolution rollup is accepted only after measurements show that
repeated series metadata or bucket count is a material storage cost.

### Phase 37 exit gate

- pinned SDK counter, gauge and distribution fixtures ingest end to end;
- the pinned `trace_metric` container is folded without retaining a normalized
  measurement array or raw payload copy;
- 1,000 same-series measurements create one queued delta for their time slot;
- high-rate increments collapse into bounded bucket writes;
- retry semantics cannot silently overcount beyond the documented at-least-once
  boundary;
- cardinality attacks are rejected before collection and index growth;
- concurrent updates of a hot series pass load and recovery tests;
- retention removes old buckets without changing current values;
- Explore, Dashboard and Alert queries reuse existing application services;
- Metrics overload cannot consume Error, Log or Span admission reservations.

## Profiling

Profiling remains desired but deliberately deferred and unnumbered. No `profiles`
collection, profile ingest route, Blob payload, flamegraph API or Web placeholder is
added by Phase 37.

Profiling may receive a phase number only after a separate owner decision. It remains
optional and cannot block the Error, Log, Trace, Metrics or reliability products.

## Phase 38: Session Replay

Session Replay is an accepted desired product because it covers browser investigation
and Webvisor-like session playback cases found in systems such as Yandex Metrica.
It does not by itself claim full traffic-acquisition, attribution, funnel or product
analytics parity. Phase 38 follows completed Phase 37.

The Phase 38 storage boundary is:

- compact searchable metadata in `replays`;
- immutable rrweb-compatible recording segments in BlobStore;
- independent queue, quota, retention, archive and deletion;
- exact links to Error, Feedback, Log and Trace context;
- a bounded replay player and derived navigation/click metadata only when justified.

Privacy masking is a client responsibility. The supported path pins compatible
Sentry browser SDK/rrweb versions and documents their masking configuration. Metric
does not implement a second DOM-aware server-side masking or privacy-parsing layer.
Replay payload segments are treated as opaque untrusted bytes after bounded envelope,
compression, size and integrity validation.

This is an explicit future exception to the generic content-level pre-storage scrub
rule in ADR-0011: searchable Replay metadata still uses the ordinary scrubber, but
opaque rrweb segment contents do not. They are never indexed as searchable text.

This means server acceptance is not proof that a recording contains no sensitive
data. Operators remain responsible for enabling and configuring the supported SDK
masking policy. The Replay UI and documentation must state this boundary
explicitly.

### Phase 38 exit gate

The Replay security gate requires:

- strict compressed and decompressed byte limits;
- compression-bomb and malformed-segment rejection;
- bounded segment count, ordering and session duration;
- explicit per-project Replay enablement and retention;
- authorization, audit and project deletion coverage;
- no operational log or diagnostic dump of recording contents;
- real browser E2E using the pinned SDK/rrweb configuration.

## Consequences

- Phase 37 adds time-series history without one MongoDB document per increment.
- A single hot counter affects only its current bucket, while different time buckets
  and series remain independently retainable and distributable.
- Metrics extend existing query, dashboard and alert code vertically.
- Profiling does not expand the Phase 37 or Phase 38 implementation scope.
- Replay deliberately trusts configured client-side masking and makes that
  trust boundary visible instead of claiming server-side privacy enforcement.
