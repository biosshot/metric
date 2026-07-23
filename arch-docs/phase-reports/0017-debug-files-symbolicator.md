# Phase 17 exit-gate report

Date: 2026-07-23
Status: complete

## Implemented

- Added canonical compact DebugId, CodeId, DebugFileId, DebugUpload, and DebugFile
  domain values plus isolated DebugChunk/DebugFile Blob namespaces.
- Added compact `debug_uploads` and `debug_files` MongoDB codecs, validators, indexes,
  byte/count quota counters, project debug-file revision, recovery queries, exact
  deletion, and project-deletion classification.
- Added the bounded Sentry CLI discovery, chunk upload, and assembly APIs. Assembly
  verifies SHA-1, streams through BLAKE3 without buffering a complete file, validates
  a bounded header, publishes immutable bytes, and then commits metadata.
- Added durable assembly recovery, expired chunk cleanup, assembled orphan cleanup,
  and metadata-first exact-ID deletion with project isolation.
- Added a private project Symbolicator index/download callback with project-scoped
  rotating HMAC bearer verification, bounded results, ETag, and streamed bytes.
- Added an optional external native Symbolicator adapter with pinned wire fixtures,
  timeout, concurrency and response limits, circuit breaking, raw-frame preservation,
  and cache invalidation through the project debug-file revision.
- Added two isolated real `sentry-cli` versions: current 3.6.2 and retained 2.58.6.

JavaScript Artifact Bundles/source maps, MCP, migrations, NATS, sharding, and disk
spool remain deferred. Phase 18 has not started. Schema generation is now 4; because
migrations are intentionally out of scope, an older development database must be
recreated instead of being modified silently.

## Exit gate

| ADR-0039 Phase 17 gate | Evidence | Result |
|---|---|---|
| Pinned real `sentry-cli` DIF upload | Real 3.6.2 and 2.58.6 executables upload the retained Breakpad fixture through the public HTTP API | Pass |
| DebugId/CodeId codecs and malformed corpus | Canonical binary round trips, textual parsing, deterministic file IDs, invalid length/flags/base64 corpus | Pass |
| Chunk retry, expiry, crash recovery | Real CLI missing-chunk negotiation/retry; durable pending upload recovered after simulated interruption; expired chunk and unreferenced final blob removed | Pass |
| Private authorization and isolation | Correct project token indexes and downloads exact bytes; the same token is rejected for another project | Pass |
| Fake and pinned external contract | Fake HTTP process validates revision/private source fields against the retained Symbolicator 26.6.0 native fixture | Pass |
| Cache hit/miss and backend-failure load | 500 Mongo hit + 500 miss samples and 10,000 circuit-open failure samples report explicit RPS | Pass |
| Native symbols to derived frames | Real native DIF is uploaded and resolved through the private source; pinned external response maps symbolicated frames through the existing Processor derived-frame boundary | Pass |

## Retained performance baseline

File:
`performance/baselines/debug-files/ryzen-5600h-windows-mongodb-v1.json`

- private index hit: 1,604 RPS;
- private index miss: 1,735 RPS;
- open-circuit backend failure: 2,775,003 RPS;
- real CLI upload, recovery, cleanup, private download, isolation, and exact delete:
  pass;
- scoped leftover processes: 0.

This Windows development machine is not server-tuned. The numbers are local
regression sentinels, not production capacity claims. The retained comparator rejects
more than a 20% RPS regression under the same fixture, hardware, build, and MongoDB
topology.

## Verification

- `cargo fmt --all -- --check`: pass.
- `cargo check --workspace --all-targets`: pass.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass.
- `cargo test --workspace --all-targets`: pass.
- real pinned Sentry CLI + MongoDB integration test: pass.
- final scoped process check: no Faultkeep test server, Sentry CLI, Cargo, Rust
  compiler, k6, or benchmark process remained.
