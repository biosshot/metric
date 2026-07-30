# ADR-0010: Ingest limits and overload protection

- Status: Accepted
- Date: 2026-07-20

## Context

Durable ingestion must remain stable under oversized payloads, compression bombs,
traffic spikes, slow MongoDB writes, and a Processor that is persistently slower than
incoming traffic. Bounded micro-batches alone do not protect HTTP, decompression,
parsing, or the durable pending backlog.

Size limits used for input safety are distinct from the MongoDB micro-batch byte
flush threshold.

## Decision

### Input limits

The initial configurable defaults are:

```toml
[ingest.limits]
max_compressed_request_bytes = "20 MiB"
max_decompressed_request_bytes = "100 MiB"
max_event_bytes = "1 MiB"
max_envelope_items = 100
```

Compressed and decompressed bytes are counted independently while streaming. The
implementation does not hold complete compressed, decompressed, and parsed copies of
the same request at once. Crossing a request, item, or event size limit returns HTTP
`413 Payload Too Large` and the rejected data is not acknowledged as durable.

Attachment and minidump bytes stream to temporary BlobStore objects rather than
MongoDB or an unbounded RAM buffer. Their commit relationship with the parent event
is defined by ADR-0012.

### Bounded concurrency

The initial configurable defaults are:

```toml
[ingest.concurrency]
max_active_requests = 512
max_parsing_tasks = 0
max_waiting_for_storage = 512
request_timeout_ms = 10000
```

`max_parsing_tasks = 0` means derive the parsing concurrency from available CPU. HTTP
request admission, decompression/parsing work, and requests awaiting durable storage
use separate bounded permits.

If an event has not reached confirmed durable storage before its bounded wait or
request deadline, the server does not acknowledge it. Ambiguous MongoDB outcomes are
safe to retry because event identity is deterministic.

### Project rate limits

An optional per-project in-memory token bucket can limit events per second and burst
size. User-configured project rate limiting is disabled by default. In the initial
single-process runtime, limiter state does not need durable or distributed
coordination and may reset on restart.

Exceeding a project or item-category limit returns HTTP `429 Too Many Requests` with
Sentry-compatible `Retry-After` and `X-Sentry-Rate-Limits` headers.

### System overload

Temporary system-wide inability to accept durable work returns HTTP
`503 Service Unavailable` with `Retry-After`. Causes include unavailable MongoDB,
exhausted bounded storage-wait capacity, shutdown, or a critical durable backlog.

The Processor RAM queue becoming full after a confirmed MongoDB write is not an
ingest failure. The event remains `pending` in MongoDB and the SDK receives success.

### Durable backlog guard

The initial defaults are:

```toml
[ingest.backlog]
max_pending_events = null
max_oldest_pending_age = "1h"
```

The system observes pending count, oldest pending age, arrival and processing rates,
MongoDB latency and errors, and available deployment storage where that information
is exposed. Crossing the configured critical backlog guard stops new durable
acceptance with HTTP `503` until recovery. Warnings are emitted before the critical
state.

An exact pending count is not required on every request. Runtime counters and
periodic reconciliation may provide a conservative admission signal without adding a
MongoDB count query to the hot path.

### Response contract

The initial response meanings are:

```text
200       confirmed durable insert or idempotent retry duplicate
400       invalid envelope or event structure
401/403   invalid or unauthorized project credentials
413       request or item exceeds a size limit
429       project, quota, or category rate limit
503       temporary system, durable-storage, or BlobStore-capacity overload
```

Unsupported Items are handled by the mixed-Envelope contract in ADR-0018. HTTP `200`
can acknowledge an intentionally handled unsupported-only Envelope, while durable
Error Event acceptance still requires a confirmed MongoDB insert and is recorded as
an `accepted` outcome.

### Ingest outcomes

This section records the original design. The current schema generation 19 does not
create `ingest_outcomes_hourly`: the production server currently wires a no-op
`OutcomeSink`, while bounded internal counters are emitted through the metrics
facade. Durable outcome aggregation remains deferred to the Phase 27 observability
gate and must not be presented as an existing MongoDB collection.

If durable approximate outcome aggregation is later selected, the proposed shape is:

```javascript
{
  _id,
  project_id,
  bucket_start,
  category,
  outcome,
  reason,
  quantity,
  expire_at
}
```

Initial outcome values include `accepted`, `duplicate`, `invalid`, `too_large`,
`rate_limited`, `unsupported`, `storage_unavailable`, and `filtered`.

`accepted` means MongoDB confirmed the durable event write. Outcome buckets are
observability projections, not an accounting ledger, and failure to persist an
outcome must never change or delay the client response. In particular, MongoDB
unavailability may leave `storage_unavailable` observable only in runtime metrics and
logs.

The initial outcome-bucket retention follows the configurable hourly-statistics
retention unless a separate policy is introduced later.

## Consequences

- Oversized and highly compressed input cannot allocate unbounded application memory.
- Slow parsing or MongoDB writes cannot create unbounded request tasks.
- One configured project limit can protect capacity for other projects.
- A full Processor RAM queue does not discard an already durable event.
- Persistent Processor lag eventually protects MongoDB by stopping new acceptance.
- Local BlobStore capacity reserves from ADR-0032 prevent attachment and symbol
  writes from filling the filesystem completely.
- Operators can distinguish accepted, invalid, rate-limited, and unavailable traffic
  without a per-request outcome write.

## Deferred questions

- Exact attachment and minidump size limits.
- Runtime-counter reconciliation intervals for backlog and outcomes.
- Future distributed rate limiting for multiple ingest processes.
- Spike-protection algorithm beyond explicit project token buckets.
