# Phase 26 report: Performance Insights

- Date: 2026-07-24
- Result: complete

## Delivered

- Root segments update rebuildable `span_stats_hourly` derived state after the Span is
  durable. Aggregate failure never changes a durable Span acknowledgement.
- Buckets have deterministic bounded identities over project/hour/transaction,
  service, environment, release and normalized operation class.
- Each bucket stores count, failure count, summed duration and at most 2,048 recent
  duration samples.
- Native API exposes throughput, failure rate, average and p50/p75/p90/p95/p99 with
  explicit approximate/sample-limit metadata and a representative Trace link.
- Queries are bounded by project, time, result limit, service, environment and
  release. Arbitrary attributes never become aggregate dimensions.
- Retention is independently configurable through
  `retention.span_stats_hourly_days`; project deletion registers the collection.
- A bounded range rebuild deletes only the selected project/hour range and replays
  root Spans as the source of truth.
- Deterministic local rules flag slow segments, N+1 database candidates, repeated
  HTTP/cache operations, slow database/queue work, long tasks and failed downstream
  calls.
- Web provides summary cards, filters, approximate percentile disclosure and links to
  representative Traces.

## Approximation semantics

Foreground durability is ordered before aggregate work. A crash or temporary MongoDB
failure can omit a derived update; retry after an acknowledged Span does not
double-apply it. A bounded rebuild restores the selected range from durable root
Spans. Percentiles use the nearest-rank value from the most recent 2,048 samples, so
they are investigative indicators rather than billing/accounting values.

## Functional evidence

- Unit tests pin percentile behavior, aggregate dimensions and decoded approximation
  inputs.
- Application tests pin deterministic Insight rules.
- Mongo schema validation, deletion registration and workspace compile checks include
  `span_stats_hourly`.

## Gate amendment

No performance/load query run was executed under the ADR-0040 owner amendment. The
functional and golden tests are retained so later changes can be compared without
leaving test processes running.
