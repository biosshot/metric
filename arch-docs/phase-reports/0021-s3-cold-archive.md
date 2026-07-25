# Phase 21 report: S3-compatible storage and cold Event archive

Status: complete
Date: 2026-07-24
Contract: `arch-docs/module-contracts/0021-s3-cold-archive-phase-21.md`

## Implemented

- Added an S3-compatible implementation of the existing `BlobStore` port. The local
  and S3 adapters expose the same immutable publication, open, delete, scan and
  capacity boundary; application code does not select adapter-specific operations.
- Added bounded multipart upload through a unique temporary key, retry of interrupted
  parts, completion followed by server-side copy, streamed BLAKE3/size verification,
  immutable idempotent final keys, and best-effort abort/temporary cleanup.
- Added typed redacted S3 configuration with explicit endpoint, region, bucket,
  path-style mode, multipart size, and `env`/`file`/development-only `literal`
  credential references. Local storage remains the default.
- Added deterministic project/day archive segment identities and keys, compact
  archive domain state, and explicit `ArchiveStore` operations. No raw MongoDB
  filters cross the port.
- Added Parquet schema version 1 with Zstandard level 3 for bounded canonical scrubbed
  Event records. Encoding runs on the blocking pool and publication uses bounded
  chunks.
- Added durable `archive_manifests` and the retry-safe ordering
  `claim -> object publish/verify -> manifest complete -> Event z/x -> source commit`.
  Finalizer and retention set archive-due `h` instead of hot expiry `x` when archival
  is enabled. A failure before complete leaves Events queryable.
- Added bounded orphan archive cleanup and registered both manifests and the Event
  archive blob namespace in project deletion. No archive object is interpreted as
  an authorization credential.
- Composed the archive worker into the all role with readiness, cancellation and
  graceful-shutdown tracking. Archival is disabled by default and requires MongoDB.
- Advanced the empty-database schema generation from 6 to 7. Metric still has no
  migration framework and does not alter an existing generation-6 database.

The AWS SDK packages are pinned to compatible versions because unconstrained
transitive releases require a newer compiler than the workspace's Rust 1.88.0. The
pins preserve the accepted workspace toolchain and remain contained in the S3
adapter crate.

## Exit gate

| ADR-0039 Phase 21 gate | Evidence | Result |
|---|---|---|
| Conformance suite shared with local BlobStore | `local_and_s3_emulator_share_blobstore_conformance` executes the same helper against both adapters; immutable retry/conflict/open/scan/delete all pass | Pass |
| Emulator plus selected real-compatible service matrix | Deterministic Axum emulator passes; the ignored env-driven matrix was executed against temporary standalone MinIO `RELEASE.2025-09-07T16-13-09Z` on Windows and passed | Pass |
| Multipart interruption, retry, missing object and permission failures | Emulator injects one failed part and observes successful retry; missing maps to `NotFound`, denied access to `Unavailable`; test passes | Pass |
| Archive manifest crash points and checksum verification | Real local MongoDB test stops after manifest completion, resumes source commit idempotently, rejects checksum/size conflicts, and preserves an Event when publication is incomplete | Pass |
| Foreground load with archive work and bounded memory | One retained release benchmark encodes 12,000 Events while a foreground BLAKE3 worker runs; peak input segment is 1,072,000 bytes under the 64 MiB gate | Pass |
| E2E archive completion before hot Event expiry | Real local MongoDB plus local BlobStore E2E verifies a `PAR1` object and complete/source-committed manifest before `h` is removed and `z`/delayed `x` appear | Pass |

The cumulative rung is:

```text
Event -> archive manifest -> verified object -> hot retention
```

## Performance baseline

Exactly one Phase 21 performance run was executed on AMD Ryzen 5 5600H, 15.9 GiB
RAM, Windows and Rust 1.88.0. The fixture contains 24 segments, 500 Events per
segment and 2 KiB canonical payloads, with concurrent foreground hashing.

```text
events: 12,000
elapsed: 216 ms
archive RPS: 55,479.17 Event/s
input throughput: 113.44 MiB/s
stored Parquet throughput: 0.73 MiB/s
foreground throughput: 700,729.64 ops/s
peak input segment: 1,072,000 bytes
local RPS gate: >= 25,000 Event/s
```

The reviewed artifact is
`performance/baselines/archive/ryzen-5600h-windows-v1.json`. Future candidates use
`performance/run-archive.mjs` and `performance/compare-archive.mjs`; the comparator
requires like-for-like metadata and rejects archive/input/foreground RPS regressions
over 20 percent or a segment above 64 MiB.

k6 is not used because archival has no public HTTP endpoint. An HTTP workload would
measure ingestion rather than the archive writer. The retained test still reports
the required explicit RPS and byte throughput.

## Verification

- `cargo fmt --all -- --check`: pass.
- `cargo run -p dependency-check`: pass; ADR-0034 direction is preserved.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass.
- `cargo test --workspace --all-targets --all-features`: pass.
- S3 shared emulator conformance and failure matrix: pass.
- Selected real-compatible MinIO matrix: pass.
- Real local MongoDB archive crash-point integration: pass.
- Real local MongoDB cumulative archive-before-expiry E2E: pass.
- Baseline comparator self-validation: pass.

The single performance test exited. The temporary MinIO instance was forcibly
stopped in `finally`; its binary, data, temporary directory and generated `mc`
configuration were removed. Final scoped process inspection found no Cargo, rustc,
Metric server, MinIO, `mc`, or k6 process. The user's MongoDB process was not
stopped.

## Known limits and deferred work

- Archive search, restore and rehydration are not implemented. MongoDB remains the
  query authority until hot expiry.
- The selected real-service matrix is environment-driven and uses a dedicated
  disposable bucket. CI needs credentials to execute that ignored row.
- S3 lifecycle rules, replication and provider-specific administration remain
  deployment concerns outside the `BlobStore` contract.
- MCP, NATS, split roles, sharding, disk spool and online migrations were not added.
- Phase 22 was not started.
