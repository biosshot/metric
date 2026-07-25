# Phase 24 report: Structured Logs

- Initial implementation: 2026-07-24
- Closure verification: 2026-07-25
- Result: complete

## Delivered

- Sentry Envelope `log` items and the official Node SDK version-2 Log container are
  accepted with bounded item sizes.
- Logs are normalized and scrubbed before acknowledgement, receive deterministic
  16-byte identities, and are durably stored in the compact `logs` collection.
- The projection contains occurrence/receive time, severity, message,
  environment/release/service and optional binary Trace/Span IDs. The versioned body
  preserves bounded structured values.
- Project policy independently enables or disables Logs.
- A dedicated bounded `LogWriter` owns Log channel capacity, maximum batch wait,
  documents, estimated bytes, operation timeout and graceful shutdown drain.
- Full Log lane capacity fails immediately as `log_lane_capacity`/HTTP 429. It cannot
  consume the Error writer channel.
- MongoDB writes each bounded Log batch with unordered `insert_many`; deterministic
  duplicates are verified against the existing identity projection.
- MongoDB owns the validator, project/time and Trace indexes, and configurable Log
  TTL (`retention.logs_days`).
- Native API list/detail routes provide bounded time windows, signed cursor
  pagination, severity, message, environment, release, service and Trace filters.
- Web provides Logs navigation, current-page severity distribution, filtering,
  detail, Trace correlation and direct SDK setup from the empty state.
- Project deletion registers `logs`.
- The physical Error collection is exclusively `error_events`; generation-7
  databases and legacy `events` data are intentionally incompatible.

## Pipeline boundary

The Error and Log pipelines deliberately share authentication, Envelope limits,
project policy and PII scrubbing, but differ after normalization:

```text
Error -> Event MongoWriter -> pending error_events -> Dispatcher
      -> symbolication/grouping/finalization -> Issues

Log   -> LogWriter -> terminal logs
```

Logs do not need Error-only symbolication, grouping, pending recovery or Issue
finalization. This difference is intentional. The earlier semaphore plus sequential
`insert_one` implementation was not an accepted lane and was replaced during closure.

## Functional evidence

- Protocol tests cover mixed Error/Log/Transaction/Span Envelopes.
- Application tests use the real Node SDK Log container shape and verify correlation
  and secret scrubbing.
- Log writer tests cover timer/document/byte batching, full-lane rejection and
  graceful shutdown drain.
- Mongo codec tests pin compact BSON bounds, round-trip behavior and independent TTL.
- `sdk-tests/node/send-signals.mjs` remains the saved official-SDK smoke program.
- Web formatting, lint, unit tests and production build pass.

## Performance evidence

Exactly two retained profiles were used, per the owner-directed Phase 24 amendment:

| Profile | Result | Gate |
| --- | ---: | --- |
| In-process Log writer | 31,965 Log RPS | >= 20,000 RPS |
| Writer average batch occupancy | 169.49 documents across 118 batches | >= 100 |
| Mixed k6 Log lane | 998.00 RPS, p95 23.64 ms, p99 24.94 ms | target 1,000/s, p95 < 150 ms, p99 < 300 ms |
| Mixed k6 Error lane | 249.48 RPS, p95 22.24 ms, p99 22.84 ms | target 250/s, p95 < 150 ms, p99 < 300 ms |
| Mixed durability | 10,001/10,001 Logs and 2,500/2,500 Errors | zero acknowledged loss |
| Failure counters | TCP 0, HTTP 12,501, 200 12,501, 429 0, 503 0, other 0 | pass |
| Generator | dropped iterations 0 | pass |

Reviewed baselines:

- `performance/baselines/log-writer/ryzen-5600h-windows-v1.json`;
- `performance/baselines/structured-logs/ryzen-5600h-windows-k6-v1.json`.

The mixed profile used MongoDB local standalone on the Ryzen 5 5600H Windows
development machine. It is a regression sentinel, not a production sizing claim.
The runner dropped its fresh `metric_phase24_*` database and no benchmark server,
k6, Cargo or Rust compiler process remained.

## Exit gate

| Requirement | Evidence | Result |
| --- | --- | --- |
| Official SDK structured values and correlation | Node v2 fixture and saved real-SDK smoke | Pass |
| One accepted Log is one durable record and retry-safe | terminal document, deterministic identity, duplicate verification | Pass |
| Log saturation cannot consume Error lane | independent bounded writers; mixed HTTP profile keeps Error p95 at 22.24 ms | Pass |
| BSON/index cost is bounded | compact codec fixtures below 512 bytes and named bounded indexes | Pass |
| Performance baseline | owner-amended two-profile evidence above | Pass |
| Browser investigation | Logs list/detail/filter/correlation and SDK setup flow | Pass |

Phase 24 is complete. Phase 25 remains the owner of a dedicated Span writer and Span
pipeline performance closure.
