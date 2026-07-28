# Phase 38 report: Session Replay

Phase 38 is complete for the ADR-0046 scope. Profiling remains deferred and was not
started.

## Exit gate

| Exit-gate item | Result | Evidence |
| --- | --- | --- |
| Strict compressed and decompressed limits | Pass | Envelope has an independent Replay byte limit; `validate_replay_recording` streams zlib data into a bounded buffer and visits the rrweb array without retaining a second event array. |
| Compression bomb and malformed rejection | Pass | `replay_recording_limits_reject_bombs_malformed_data_and_excess_events` covers oversized decompression, invalid zlib/JSON and excess event count. |
| Segment count, ordering and duration bounds | Pass | Domain constructors cap 100 segments and 24 hours; MongoDB appends then sorts by segment ID; domain, writer-restart and real MongoDB tests pass. |
| Explicit project enablement and retention | Pass | `items.replay` defaults to false and is exposed in project setup/settings; `retention.replays_days` and `retention.replay_archive` are explicit configuration. |
| Authorization and audit | Pass | Replay list/detail/segment routes require project read; raw segment access records durable `replay.accessed`; the pinned permission matrix and audit unit test pass. |
| Project deletion | Pass | Deletion registry owns Mongo dataset code 23 and Blob namespace code 97; real MongoDB deletion integration verifies target-project purge and tenant isolation. |
| No recording contents in operational logs | Pass | Replay submission debug output contains only lengths; neither validation, writer, MongoDB nor HTTP diagnostics format recording bytes. |
| Partial upload and orphan recovery | Pass | Writer restart preserves the ordered partial set `[0, 2]`; orphan cleanup removes a Blob created before an interrupted manifest commit. |
| Replay bandwidth isolation | Pass | Replay owns a dedicated byte semaphore and bounded channel; `replay_byte_budget_rejects_before_blob_or_metadata_admission` proves rejection before Blob or metadata writes. |
| Pinned real-browser record/upload/retrieve/play | Pass | `real_browser_sdk_records_uploads_retrieves_and_plays_replay` uses `@sentry/browser` 10.66.0, Chromium and `rrweb-player` 2.1.1; masked secret absence and playback mount are asserted. |
| Bounded Web player and visible privacy boundary | Pass | Player loads on demand, caps 20 segments, 100,000 events and 50 MiB, and visibly states that server acceptance is not a privacy guarantee. |

## Storage and product result

- MongoDB schema generation advances from 18 to 19 intentionally and without
  migrations.
- `replays` contains compact searchable session metadata and ordered immutable
  BlobStore references; rrweb content is not stored in MongoDB or indexed.
- Replay has independent queue, queued-byte budget, retention/archive marker,
  readiness and deletion ownership.
- Native API and Vue Web provide list, detail, audited segment retrieval and exact
  Error/Feedback/Log/Trace navigation.
- `rrweb-player` is a lazy 227.12 kB production chunk; the main Web JavaScript chunk
  remains 333.79 kB.

## Performance evidence

Exactly one Phase 38 performance scenario was run:

- fixture: 20,000 raw rrweb segments with four events each;
- result: **3,592,857 validation RPS**;
- local minimum gate: 10,000 RPS;
- scope: release-mode request-local validation only, excluding HTTP, BlobStore and
  MongoDB.

The retained artifact is
`performance/baselines/session-replay/ryzen-5600h-windows-v1.json`. Future candidates
use `performance/compare-session-replay.mjs` with a default 15% regression budget.

No Metric, Cargo, Rust compiler, Node, k6, Chromium or Edge process associated with
the workspace remained after verification.

## Verification

- `cargo fmt --all`
- `cargo check --workspace --all-targets --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked --jobs 2`
- real MongoDB Replay manifest and project deletion integration tests
- real pinned browser Replay E2E
- Web format, lint, 23 unit tests and production build
- browser SDK fixture format, lint and production bundle
