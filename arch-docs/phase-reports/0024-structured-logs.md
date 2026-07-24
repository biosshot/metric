# Phase 24 report: Structured Logs

- Date: 2026-07-24
- Result: implemented

## Delivered

- Sentry Envelope `log` items and the official Node SDK version-2 Log container are
  accepted with bounded item sizes.
- Logs are normalized and scrubbed before acknowledgement, receive deterministic
  16-byte identities, and are durably stored in the compact `logs` collection.
- The projection contains occurrence/receive time, severity, message,
  environment/release/service and optional binary Trace/Span IDs. The versioned body
  preserves bounded structured values.
- Project policy can independently enable or disable Logs. The Log semaphore is
  separate from Error and Span admission.
- MongoDB owns the validator, project/time and Trace indexes, and a configurable Log
  TTL (`retention.logs_days`).
- Native API list/detail routes provide bounded time windows, signed cursor
  pagination, severity, message, environment, release, service and Trace filters.
- Web provides Logs navigation, current-page severity distribution, filtering,
  detail, and Trace correlation.
- Project deletion registers `logs`.
- The physical Error collection is now exclusively `error_events`; generation-7
  databases and legacy `events` data are intentionally incompatible.

## Functional evidence

- Protocol tests cover mixed Error/Log/Transaction/Span Envelopes.
- Application tests use the real Node SDK Log container shape and verify correlation
  and secret scrubbing.
- Mongo codec tests pin compact BSON bounds, round-trip behavior and independent TTL.
- `sdk-tests/node/send-signals.mjs` is a saved official-SDK smoke program.
- Web formatting, lint, unit tests and production build pass.

## Gate amendment

No sustained, burst or search-under-ingest run was executed, by the explicit
Phase 24-26 performance-gate amendment in ADR-0040. No server or SDK process was
started for this report.
