# Phase 40 report: bounded JSON/CSV query export

- Status: Complete
- Date: 2026-08-02
- Owner: Unified Query v2 HTTP/Web presentation boundary
- Decisions: ADR-0047 and ADR-0048

## Delivered

- JSON and CSV download mode on the existing project-scoped `/query` endpoint.
- Records export for Issues, Errors, Logs, Traces, Metrics, Replays, Feedback and
  Releases through their existing Query v2 adapters and signed cursors.
- A bounded in-memory response buffer capped at 16 MiB, 10,000 rows and 15 seconds,
  with at most two concurrent exports and 500 rows per adapter page.
- Deterministic source-specific CSV schemas, nested DTO encoding and spreadsheet
  formula-injection protection.
- Existing authorization and scrubbed stable DTO representations are reused.
- Existing audit storage records success, failure, capacity and timeout outcomes.
- Shared Vue Query controls expose JSON/CSV downloads for all record sources and
  preserve the active source, query and optional time range.
- Capability output advertises formats and hard server ceilings.

## Verification evidence

- typed transport accepts only the closed download output and rejects storage escape
  fields;
- every Query v2 source has a non-empty unique deterministic CSV schema;
- CSV golden coverage verifies header order, escaping and formula neutralization;
- authorization audit coverage verifies the existing validator-compatible metadata;
- Web client coverage verifies endpoint reuse, organization/CSRF context, date alias
  normalization, filename and output selection;
- Rust format, Clippy/test gates and Web format/lint/unit/build gates pass.

Phase 40 changes no MongoDB collection, validator, index or schema generation and
does not start Phase 42.
