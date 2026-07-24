# Supported capabilities

The runtime exposes the authoritative machine-readable view at
`GET /api/v1/capabilities`. This document summarizes version-one boundaries; exact
SDK claims remain in `compatibility/sentry-sdk-matrix.toml`.

Enabled core capabilities:

- Sentry DSN/store/Envelope Error Event ingest with deterministic idempotency;
- mandatory pre-storage PII scrubbing, bounded normalization and Issue grouping;
- durable MongoDB processing, Issue/statistics/activity, Search v1 and native API;
- Vue investigation UI using only `/api/v1`;
- safe JSON/text attachments through the BlobStore;
- project lifecycle, retention, deletion, audit, health and bounded Scheduler work;
- Incident Capsule export;
- signed webhook notifications when configured.

Optional capabilities:

- standalone minidump ingest, disabled by default because process memory cannot be
  reliably scrubbed;
- debug-file and JavaScript Artifact Bundle upload;
- separately operated external Symbolicator;
- S3-compatible BlobStore;
- Parquet/Zstd cold Event archive, disabled by default and without search/restore.

Explicitly disabled:

- transactions and spans;
- profiles, sessions and replays;
- check-ins;
- metrics/StatsD and log ingestion;
- user feedback.

An absent optional route is represented by capabilities rather than a placeholder
success response.
