# Phase 17 contract: debug files and external Symbolicator

Status: implemented and verified by the Phase 17 exit gate.
Owning ADRs: ADR-0013, ADR-0025, ADR-0026, ADR-0027, ADR-0032, ADR-0033,
ADR-0034, ADR-0035, ADR-0036, ADR-0037, ADR-0039

## Responsibility and boundaries

The domain owns compact DebugId, CodeId, DebugFileId, upload, file, and
symbolication request/result values. The ports crate owns debug metadata persistence
and symbolication backend contracts. MongoDB owns authoritative upload/file metadata,
quotas, project revisions, and indexes. BlobStore owns immutable chunk and assembled
file bytes.

The application service owns personal-token authorization, chunk validation, bounded
assembly, crash recovery, exact deletion, expiry, and orphan reconciliation. The
server owns the Sentry CLI transport and the private project-scoped Symbolicator
source. The symbolication adapter owns the external HTTP protocol, timeout,
concurrency bound, response bound, and circuit breaker.

This phase does not add JavaScript Artifact Bundles, source maps, MCP, migrations,
NATS, sharding, disk spool, or Phase 18 behavior.

## Upload and storage contract

The Sentry CLI surface exposes bounded discovery, SHA-1-addressed chunk upload, and
whole-file assembly. Requests have fixed chunk, body, file, and manifest limits.
Assembly streams from BlobStore through SHA-1 and BLAKE3 and retains only a bounded
header. A final object is published immutably before authoritative metadata becomes
ready.

`debug_uploads` is durable recovery state. `debug_files` stores compact identifiers,
checksums, type, size, and upload time. Publication updates project byte/count quota
counters and the debug-file revision. Existing schema generations are rejected
fail-closed; this phase intentionally provides no migration path.

## Private source and external adapter

Symbolicator receives only a private project source URL carrying the current
debug-file revision. Callback bearer tokens are HMAC-authenticated, project scoped,
and support current/previous key verification. Index queries are bounded and download
requires the exact project/file relation.

The external adapter implements the pinned native `/symbolicate` request/response
shape. It has a request deadline, bounded concurrency, bounded response bytes, and a
consecutive-failure circuit breaker. Raw traces remain authoritative; derived frames
are attached only through the existing symbolication result boundary.

## Cleanup and deletion

Chunk expiry and assembled-file orphan cleanup scan only their typed Blob namespaces.
Exact deletion removes metadata first under a keyed local mutex, updates counters and
revision, then removes the immutable object. Retrying a completed delete is
idempotently not-found. Project deletion owns both namespaces and Mongo collections.

## Gate

- real pinned current and retained `sentry-cli` uploads;
- compact identifier codecs and malformed corpus;
- missing-chunk retry, expiry, assembly recovery, and orphan cleanup;
- private authorization, exact download, and cross-project isolation;
- fake response plus pinned external Symbolicator protocol fixture;
- retained hit, miss, and backend-failure RPS;
- native upload, source resolution, and derived-frame processing boundary;
- workspace formatting, strict lint, tests, and scoped process cleanup.
