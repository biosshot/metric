# ADR-0002: Durable ingestion and in-process processing

- Status: Accepted
- Date: 2026-07-20

## Context

The first version runs as one process in the `all` role. It needs to absorb short
traffic bursts without issuing one MongoDB request per event, must not acknowledge
events that exist only in memory, and must keep the processing queue bounded.

The design must remain simple enough for the first version while preserving a clear
place for a future local disk spool. NATS, distributed workers, and networked role
communication are outside the current scope.

## Decision

### Runtime scope

The first version supports only:

```text
--role=all
```

The ingest, processing, symbolication, web, and scheduling components remain separate
internal modules and Tokio task groups, but they run inside one process. NATS is not
used. Communication between the modules uses bounded in-process channels.

The internal code does not include the product name in variable, type, or internal
metric identifiers. This avoids unnecessary renames if the public product name
changes.

### Durable acceptance

MongoDB is the durable acceptance point and the source of truth for the processing
backlog.

An SDK request is acknowledged only after its event has been successfully written to
MongoDB. An accepted document starts with:

```javascript
pipeline: {
  state: "pending",
  attempts: 0,
  next_attempt_at: received_at,
  last_error: null
}
```

The initial write uses an idempotent identifier derived from `project_id` and
`event_id`. A duplicate identifier is treated as a successful retry, but the payload
from the duplicate request is not scheduled as a new event.

If MongoDB rejects the write or is unavailable, the server does not return a success
response. If MongoDB may have committed a write but the acknowledgement was lost, a
client retry is resolved by the same idempotent identifier.

The first version cannot accept events while MongoDB is completely unavailable. This
is an explicit consequence of deferring a local disk spool and any external durable
queue.

### Ingest micro-batching

Accepted events are collected into a short micro-batch before `insert_many` is sent
to MongoDB.

```toml
[ingest.batch]
max_wait_ms = 20
max_documents = 250
max_bytes = "8 MiB"
```

- `max_wait_ms` is configurable and defaults to `20` milliseconds.
- `max_documents` is configurable in the range `100..=500`.
- The default value of `max_documents` is provisional and will be selected by a
  production-shaped benchmark.
- `max_bytes` is configurable and defaults to `8 MiB` of encoded MongoDB event
  documents. Attachments and other BlobStore payloads are not counted in this value.
- The batch is flushed as soon as any limit is reached.
- Input request and item limits are separate security and compatibility controls
  defined by ADR-0010.

The insert operation is unordered so that a duplicate or invalid document does not
prevent independent documents in the same batch from being attempted. Results are
mapped back to the individual HTTP requests.

### Processor queue

The Processor queue is bounded and its capacity is configurable. It is an acceleration
cache, not a durable queue.

After a successful MongoDB batch insert, each newly inserted event is offered to the
Processor queue:

- If the queue has capacity, the existing in-memory accepted payload is moved into
  the queue together with its event key.
- The payload is not copied and is not immediately reloaded from MongoDB. An owned
  value or `Arc` may be used according to the final ownership model.
- If the queue is full, the in-memory payload is released after the HTTP request is
  completed. The MongoDB document remains in the `pending` state.
- A duplicate insert is acknowledged but its request payload is not enqueued.

Reducing the configured queue capacity is the initial mechanism for installations
with limited RAM. Queue capacity is counted in events; byte-based queue accounting is
deferred.

### Backlog refill

A single in-process dispatcher owns queue refill behavior. It maintains a local set
of event keys that are queued or currently running.

The dispatcher loads complete pending event payloads from MongoDB when:

- the process starts;
- the queue becomes idle; or
- the queue falls below a configurable low watermark.

It fills the queue up to a configurable refill target. Events already present in the
local queued/running set are skipped. Because the first version has only one process,
distributed claims and leases are not required.

After a process restart the local set is empty, and all eligible pending events can be
discovered again from MongoDB. A future multi-process design must replace this local
coordination with atomic claims and expiring leases.

### Processing lifecycle

The persistent states are deliberately small:

```text
pending -> processed
pending -> pending with next_attempt_at in the future
pending -> failed
```

Queued and running are local in-memory conditions and are not persistent states in the
single-process design.

Processing performs normalization that was not required for acceptance,
symbolication, grouping, issue updates, derived fields, and downstream actions. On
success the event becomes `processed`. Temporary failures remain `pending` with a
future retry time. Permanent failures or exhausted retries become `failed` with a
machine-readable error.

The exact strategy for batching final event updates and issue aggregates is deferred
to the MongoDB data-model and benchmark decisions.

### Future disk spool seam

A local disk spool is not implemented in the first version, but the ingestion flow
keeps an explicit durable-acceptance boundary:

```text
validate -> durable acceptance -> processor queue -> processing
```

Currently, durable acceptance means a confirmed MongoDB write. A future mode may
insert a local append-only disk journal at this boundary and acknowledge an event
after the journal is durable, allowing MongoDB delivery to happen asynchronously.
HTTP handlers and Processor workers must not depend directly on which durability mode
is active.

No spool file format, public configuration, compaction scheme, or recovery protocol is
defined yet.

## Consequences

- Fresh events normally reach Processor without a second MongoDB read or a duplicate
  payload allocation.
- Events that do not fit in the RAM queue remain safe in MongoDB and are processed
  later.
- Queue capacity directly trades RAM use for the amount of immediately available
  work.
- A process crash loses only the in-memory acceleration state; pending MongoDB events
  are recoverable.
- Sustained processor lag can grow the MongoDB pending backlog. Backlog admission
  limits and overload behavior require a separate decision.
- MongoDB unavailability causes ingestion failures until a disk spool or another
  durable acceptance mode is implemented.

## Deferred questions

- Disk spool format and activation policy.
