# Phase 16 exit-gate report

Date: 2026-07-23
Status: complete

## Implemented

- Added typed domain blob keys, object IDs, BLAKE3 checksums, kinds, sanitized
  filenames, content types, and event-owned attachment metadata.
- Added a local filesystem BlobStore with bounded chunk sessions, capacity reservation,
  a protected disk reserve, temporary-file sync, non-replacing atomic publication,
  idempotent retry verification, chunked reads, bounded-page scans, and startup cleanup
  of crash-left temporary objects.
- Added bounded Sentry Envelope attachment parsing. The enabled safe policy accepts
  UTF-8 `application/json` after the ordinary recursive PII scrub and conservative
  `text/plain`; unsupported binary or unsafe text is intentionally dropped with the
  attachment capability signal.
- Implemented attachment blob-first acceptance. Every accepted reference is published
  before EventSink/MongoDB acceptance. Blob failure prevents the Event write; a later
  Event failure leaves a reconcilable orphan.
- Added streaming raw and multipart standalone minidump ingestion at
  `POST /api/{project_id}/minidump/`, including compressed/decompressed limits,
  bounded retained headers, `MDMP`/directory validation, deterministic checksum-based
  Event IDs, atomic blob publication, and sanitized synthetic native fatal Events.
  The capability remains disabled by default.
- Added bounded orphan reconciliation against both pending and processed MongoDB Event
  bodies. Referenced parent blobs are preserved; missing parent Events make old objects
  eligible for deletion. The project-owned filesystem namespace is classified in the
  deletion registry.
- Added authorized Event attachment/minidump metadata and streaming download routes.
  Blob keys are never bearer credentials: project authorization and the exact
  project/Event/object relation are checked before opening bytes.
- Added typed TOML configuration for local blob capacity/reserve, safe attachments,
  cleanup bounds, and the explicit minidump disclosure switch.
- Extended the pinned real `@sentry/node` 10.66.0 compatibility harness with a JSON
  attachment and exact BlobStore readback.

S3, debug-file upload, external Symbolicator processing, source maps, MCP, migrations,
NATS, sharding, and disk spool remain deferred.

## Exit gate

| Gate | Evidence | Result |
|---|---|---|
| BlobStore conformance, crash publication, traversal | chunked write/read and checksum; first-write-wins conflict test; dropped/aborted and restart temporary cleanup; typed traversal rejection | Pass |
| Streaming memory, size, decompression | bounded HTTP compressed/decompressed readers; attachment count/item/aggregate limits; streamed minidump source with 64 KiB retained header; 100 MiB configurable maximum | Pass |
| Mongo/blob failure matrix | blob capacity failure makes zero EventSink calls; EventSink failure leaves one published orphan; no accepted missing reference | Pass |
| Minidump multipart compatibility | raw octet-stream and quoted-boundary multipart `upload_file_minidump`; malformed header/directory rejection; disabled-default intentional drop | Pass |
| Bytes/s, concurrency, disk-full, slow filesystem | one retained release-mode local filesystem workload, concurrency 8, protected reserve exhaustion, delayed producer | Pass |
| SDK attachment/minidump to authorized metadata/download | real Node SDK attachment gate passed; real local Mongo cumulative SDK → Processor → authorized metadata/readback gate passed; synthetic minidump metadata/readback corpus passed | Pass |

## Retained performance baseline

File:
`performance/baselines/blob-store/ryzen-5600h-windows-localfs-v1.json`

- 256 objects × 256 KiB, concurrency 8;
- 263.76 object RPS;
- 65.94 MiB/s;
- delayed-I/O workload: 160.43 object RPS;
- disk-full/reserve gate: pass;
- scoped leftover processes after the run: 0.

The machine is a Windows development workstation and is not server-tuned; the result
is a regression baseline, not a production capacity promise.

## Verification

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass.
- `cargo test --workspace`: pass.
- `cargo run -p faultkeep-server --bin faultkeep-server -- --check-config`: pass.
- Web Prettier, ESLint, Vitest (10 tests), type-check, and production build: pass.
- Node SDK Prettier and ESLint: pass.
- Real `@sentry/node` compatibility test: pass.
- Real local Mongo cumulative attachment/authorization test: pass.
- One Phase 16 performance test: pass.
- Final scoped process check: no Faultkeep test server, SDK sender, benchmark, or k6
  process remained.
