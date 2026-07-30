# Supported capabilities

The runtime exposes the authoritative machine-readable view at
`GET /api/v1/capabilities`. This document summarizes version-one boundaries; exact
SDK claims remain in `compatibility/sentry-sdk-matrix.toml`.

Enabled core capabilities:

- Sentry DSN/store/Envelope Error Event ingest with deterministic idempotency;
- structured logs, transactions and spans through Sentry Envelopes with bounded
  batch writers;
- mandatory pre-storage PII scrubbing, bounded normalization and Issue grouping;
- durable MongoDB processing, Issue/statistics/activity, Search v1 and native API;
- Vue investigation UI using only `/api/v1`;
- safe JSON/text attachments through the BlobStore;
- project lifecycle, retention, deletion, audit, health and bounded Scheduler work;
- Incident Capsule export;
- releases, deploys, Sessions and Release Health;
- User Feedback with exact Error/Trace/Replay identifiers when supplied;
- Unified Explore, Saved Queries and project-shared Dashboards;
- count-based Alerts with Telegram and SMTP Email destinations;
- Cron and GET/HEAD Uptime Monitoring;
- Application Metrics counters, gauges and distributions through pinned Sentry
  `trace_metric` containers, with compact buckets and Unified Explore;
- signed webhook notifications when configured.

Optional capabilities:

- standalone minidump ingest, disabled by default because process memory cannot be
  reliably scrubbed;
- debug-file and JavaScript Artifact Bundle upload;
- separately operated external Symbolicator;
- S3-compatible BlobStore;
- Parquet/Zstd cold Event/Log/Span archive, disabled by default and without
  search/restore;
- Session Replay for the pinned `@sentry/browser` contract, disabled per project by
  default, with compact MongoDB manifests and immutable BlobStore segments.

Explicitly disabled:

- legacy StatsD and Sentry `metric_buckets` Envelope items;
- profiles; desired but deliberately deferred.

An absent optional route is represented by capabilities rather than a placeholder
success response.
