# Phase 19 report: Incident Capsule

- Status: Complete
- Commit: recorded by the Phase 19 Conventional Commit
- Architecture: ADR-0038, ADR-0039

## Exit gate

- Accepted module contract and stable public errors are recorded in
  `module-contracts/0019-incident-capsule-phase-19.md`.
- `IncidentCapsuleService` enforces `issue:read`, `event:read` and
  `incident:export`, verifies project organization, applies a final DTO allowlist
  and writes an audit record before response headers.
- `POST /api/v1/projects/{project_id}/issues/{issue_id}/capsule` streams a
  deterministic ZIP64 archive through a four-chunk bounded channel. No generated
  capsule is persisted.
- Entries have fixed safe paths, descriptive version-one JSON, normalized
  timestamps, bounded sizes and BLAKE3 metadata in a final `manifest.json`.
- Default selection deduplicates first/latest/representative and recent Events,
  never exceeds ten Events, and records retention omissions without replacement
  scans.
- Attachment/minidump bytes, debug files, Artifact Bundles, source archives,
  credentials, compact BSON keys and internal BlobStore keys are excluded.
- An independent `faultkeep-testkit` reader accepts unknown safe manifest fields
  and rejects unsupported versions, traversal, duplicate/corrupt archives,
  checksum mismatches, truncation and compression bombs.
- Backpressure, client disconnect, shutdown and the complete 30-second generation
  deadline terminate the blocking writer without a detached process or unbounded
  queue.
- Real local-MongoDB cumulative E2E passed:
  SDK Event -> finalized Issue -> authenticated HTTP Capsule -> independent
  validation -> audit record. The same test verifies unauthenticated, missing
  permission and missing Issue responses.

## Resource defaults

```text
events: 10
activities: 100
statistics range: 30 days, at most 100 retained hourly buckets
total uncompressed bytes: 100 MiB
entry bytes: 16 MiB
generation timeout: 30 seconds
data-read concurrency: 4
stream chunks: 64 KiB x 4 buffered
```

All values are typed configuration and are rejected above server-owned hard
maxima.

## Verification

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace --all-targets`: pass.
- real local MongoDB cumulative Capsule E2E: pass.
- retained release benchmark on Ryzen 5 5600H / Windows / Rust 1.88:
  300 samples, 642 complete Capsule responses/s and 2.05 compressed MiB/s.
- regression comparator at 20 percent budget: pass.
- scoped Cargo, Rust test, Faultkeep and k6 process check: clean.

## Safe observability

Closed metrics record preparation outcome, selected Event count, uncompressed
bytes, generation latency and stream disconnects. Audit metadata contains only
actor, request ID, project, Issue, selected count and result size class.

## Deliberately deferred

No MCP, durable share link, background export job, capsule collection, BlobStore
copy, attachment/source opt-in, signing, encryption, import or offline viewer was
added. Phase 20 notification delivery was not started.
