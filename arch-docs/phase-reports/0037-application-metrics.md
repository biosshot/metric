# Phase 37 report: Application Metrics

Phase 37 is complete for the ADR-0046 scope. Phase 38 was not started.

## Exit gate

| Exit-gate item | Result | Evidence |
| --- | --- | --- |
| Pinned SDK counter, gauge and distribution end to end | Pass | Real `@sentry/node` 10.66.0 `send-metrics.mjs` HTTP E2E reaches the dedicated sink with all three kinds. |
| No retained normalized array or queued raw container | Pass | Custom Serde container/sequence visitor folds one row at a time directly into `MetricDeltaBatch`. |
| 1,000 same-series measurements become one delta | Pass | `one_thousand_same_series_measurements_cross_as_one_delta`; retained perf artifact also asserts `deltas_per_container = 1`. |
| High-rate increments collapse into bounded bucket writes | Pass | `hot_series_collapses_across_concurrent_requests` merges 100 concurrent requests into one store batch and one series delta. |
| Retry overcount boundary is explicit | Pass | Module contract documents at-least-once behavior; writer performs no internal ambiguous retry. |
| Cardinality attack rejected before growth | Pass | Real MongoDB integration admits two series, rejects the third with `Capacity`, and confirms only two bucket documents. |
| Hot-series concurrency and recovery | Pass | Concurrent collapse test plus `storage_failure_does_not_kill_metric_lane`. |
| Retention removes old buckets | Pass | `metric_retention` is a zero-delay TTL index; real MongoDB integration verifies TTL/archive markers and indexes. |
| Explore, Dashboard and Alert reuse | Pass | `metrics` is a first-class Explore dataset accepted by native API, Web Explore, Saved Queries, Dashboards and aggregate Alerts; Mongo integration executes a grouped metric query. |
| Metrics overload is isolated | Pass | Metrics owns `MetricSink`, `MetricStore`, `MetricWriter` and its own bounded channel; no Error/Log/Span reservation is shared. |

## Performance evidence

Exactly one Phase 37 performance scenario was retained:

- fixture: 500 containers × 1,000 same-series counter measurements;
- result: **281 container RPS** and **281,227 measurement RPS**;
- collapse: exactly one delta per container;
- local minimum gate: 100,000 measurement RPS;
- scope: release-mode streaming fold only, excluding HTTP and MongoDB.

Artifact:
`performance/baselines/application-metrics/ryzen-5600h-windows-v1.json`.
Future candidates use `performance/compare-application-metrics.mjs`.

No Metric server, k6, Cargo, Rust compiler or Node process remained after the run.

## Storage and compatibility

- MongoDB schema generation advances from 17 to 18 intentionally and without
  migrations.
- `metric_buckets` is compact and project-scoped.
- Legacy StatsD and `metric_buckets` Envelope items remain disabled.
- Session Replay and Profiling remain absent.
