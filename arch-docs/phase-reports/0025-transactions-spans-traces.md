# Phase 25 report: Transactions, Spans and Traces

- Initial implementation: 2026-07-24
- Span lane closure verification: 2026-07-25
- Result: complete

## Delivered

- Sentry `transaction` and streamed `span` Envelope items are accepted.
- Trace IDs are fixed 16-byte values, Span IDs are fixed 8-byte values, and Mongo
  record IDs are deterministic BLAKE3 identities over project/Trace/Span.
- Transactions expand into one root segment and bounded child Span records. Duplicate
  delivery verifies the natural identity and does not duplicate records.
- Span documents preserve nanosecond start/duration, parent, normalized operation
  class, status, name, service, environment, release, Insight flags and a bounded
  versioned body.
- Project policy and independent Span admission prevent disabled or saturated Span
  work from borrowing the Error/Log semaphore.
- `spans` has a project/Trace index, a partial segment-feed index and configurable
  `retention.spans_days`. Project deletion registers `spans`.
- Error finalization projects optional Trace/Span IDs into `error_events`.
- The bounded virtual Trace query assembles authorized Spans, Logs and Error IDs,
  reports partial Span/Log results, and never repairs missing parents.
- Web provides a Transaction feed and a bounded Trace waterfall with Span detail,
  Logs and Error links.
- A dedicated bounded `SpanWriter` owns Transaction/Span channel capacity, maximum
  batch wait, documents, estimated bytes, operation timeout and graceful shutdown
  drain. It cannot consume the Error or Log writer queues.
- MongoDB writes each bounded Span batch with unordered `insert_many`; deterministic
  duplicate identities are verified against project, Trace ID and Span ID.

## Functional evidence

- Domain tests pin identifier width, zero rejection and stable record identities.
- Application tests pin transaction expansion, retry-stable IDs, Insight flags, and
  malformed identity/time rejection.
- Mongo codec tests cover compact round-trip storage.
- Span writer tests cover timer/document/byte batching, full-lane rejection and
  graceful shutdown drain.
- Native/Web compile and production-build checks cover the new routes and DTOs.

## Span lane performance evidence

Exactly one retained in-process profile closes the sequential `insert_one` debt:

| Metric | Result | Gate |
| --- | ---: | --- |
| Span writer throughput | 32,639 Span RPS | >= 20,000 RPS |
| Average batch occupancy | 169.49 documents across 118 batches | >= 100 |
| Durable records | 20,000/20,000 | zero acknowledged loss |

Baseline:

- `performance/baselines/span-writer/ryzen-5600h-windows-v1.json`.

The profile measures bounded actor admission and batch occupancy on the Ryzen 5 5600H
Windows development machine. It is a regression sentinel, not a production sizing
claim. It starts no HTTP server, MongoDB process or SDK process, and no Cargo/Rust
benchmark process remained after completion.

The broader sustained, mixed-ingest, Trace-read and two-process SDK load profiles
remain exempt under the accepted ADR-0040 Phase 24-26 performance amendment. The saved
official-SDK smoke program continues to cover a local transaction, child Span and
correlated Log without leaving a background process.
