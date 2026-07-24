# Phase 25 report: Transactions, Spans and Traces

- Date: 2026-07-24
- Result: implemented

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

## Functional evidence

- Domain tests pin identifier width, zero rejection and stable record identities.
- Application tests pin transaction expansion, retry-stable IDs, Insight flags, and
  malformed identity/time rejection.
- Mongo codec tests cover compact round-trip storage.
- Native/Web compile and production-build checks cover the new routes and DTOs.

## Gate amendment

No Span saturation/load, latency or mixed-signal performance run was executed under
the ADR-0040 owner amendment. The saved official-SDK smoke program covers local
transaction, child Span and correlated Log generation without running a background
process as part of this report.
