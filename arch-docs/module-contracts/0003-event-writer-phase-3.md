# Phase 3 contract: compact Event storage and MongoWriter

- Status: accepted for implementation
- Date: 2026-07-22
- Owners: `mongo` (Event BSON codec/schema/store), `application::writer`
  (bounded micro-batching), `server` (configuration and lifecycle composition)

## Responsibilities and exclusions

Phase 3 makes a scrubbed `AcceptedEvent` durably and idempotently writable. The
MongoDB adapter owns the ADR-0022 compact pending document, adaptive body codec,
`events` validator/indexes and unordered insertion result classification.
`MongoWriter` owns bounded command admission, wait/document/encoded-byte flush
thresholds, per-request completion, shutdown fencing/drain and the newly-inserted
payload handoff seam.

It does not implement Dispatcher refill, Processor, normalization, Issue creation,
terminal Event updates, attachments, backlog admission, migrations, disk spool,
NATS, sharding or another durable backend. The handoff receiver may discard fresh
payloads until Phase 4 supplies Dispatcher; MongoDB remains authoritative.

## Inputs, outputs, ports and stable errors

The deterministic Event key is exactly the four-byte big-endian positive project ID
followed by the 16-byte Sentry Event ID. `EventStore` prepares an adapter-owned
opaque value once, exposes only its encoded byte size/key to `MongoWriter`, and
returns that owned value with `inserted` or `duplicate` status. This lets an inserted
payload be offered through `AcceptedEventHandoff` without another payload copy and
without leaking BSON into `application`.

`EventSink` remains the Ingest-facing port. Inserted and duplicate results are
durable success. Definite pre-acceptance capacity/codec/storage failures are
unavailable; a network, timeout or write-concern result that may have committed is
ambiguous. Both failures map to retryable SDK failure, and a retry uses the same key.
Backend strings and documents never cross the port.

## Persistent shape and idempotency

Pending Event documents contain `_id`, `p`, `r`, `o`, `a`, `q` and `b`; default
level and policy revision are omitted. `q` is `{s: 0, a: 0, n: r}`. Body format 1
stores canonical compact JSON as codec 0 or Zstandard codec 1 only when compression
saves at least the configured threshold. Unknown body/outer versions, malformed
headers, invalid compressed data, oversized decoded bodies and inconsistent
composite IDs fail closed.

MongoDB `_id` is the final uniqueness check. Unordered batches classify duplicate
key errors per item while allowing independent documents to commit. Non-duplicate
write errors fail only their item when MongoDB supplies a complete indexed result.
Write-concern, connection and incomplete results are ambiguous for every unresolved
item. A duplicate never enters the fresh-payload handoff.

## Bounds, cancellation and shutdown

Writer channel capacity, maximum waiting HTTP requests, batch documents, encoded
bytes, maximum wait, MongoDB operation timeout and shutdown drain are finite.
`max_documents` is `100..=500`; defaults are 20 ms, 250 documents and 8 MiB. One
already input-bounded document may form a batch even when configured byte cap is
smaller than it. Cancellation before enqueue has no side effect. Cancellation after
enqueue may still commit and is therefore safely retried by deterministic identity.

The shutdown fence rejects new submissions, flushes already queued work within the
drain deadline and completes every accepted waiter with a durable or retryable
result. Deadline exhaustion rejects remaining work; it cannot manufacture success.

## Operability and verification

Metrics use only fixed operation/outcome dimensions and cover queue rejection,
batch documents/bytes/wait/latency, inserted/duplicate/partial/ambiguous outcomes and
handoff acceptance. Safe logs contain bounded counts, durations and stable codes,
never payloads, DSN keys, Event bodies, MongoDB URI/database or backend text.

Required verification is codec semantic round trip, malformed/golden/byte-budget
tests; threshold/timer/byte/cancellation/shutdown simulations; real MongoDB schema,
unordered duplicate partial result and failpoint ambiguity tests; retry proving one
durable identity; cumulative official-SDK HTTP-to-MongoDB E2E; and recorded writer
RPS, occupancy, p95/p99 latency with zero acknowledged loss on declared hardware.
