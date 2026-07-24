# Phase 18 contract: JavaScript Artifact Bundles and source maps

Status: accepted for implementation.
Owning ADRs: ADR-0013, ADR-0026, ADR-0028, ADR-0029, ADR-0031, ADR-0032,
ADR-0034, ADR-0035, ADR-0039

## Responsibility and boundaries

The domain owns ArtifactBundleId, canonical project/Release/dist bindings, upload
state, bundle metadata, lookup requests, and stable error codes. The ports crate owns
artifact metadata persistence and GC state transitions. MongoDB owns compact BSON,
indexes, quota counters, revision increments, atomic binding transitions, leases, and
generation fencing. BlobStore owns immutable chunks and complete Source Bundle bytes.

The application service authorizes every selected project, streams assembly, validates
the Source Bundle, publishes immutable content, coordinates recovery and association
changes, and runs bounded GC. The server owns only Sentry CLI and private Symbolicator
HTTP DTOs. The Symbolicator adapter owns `/symbolicate-js` wire translation. Archive
metadata is descriptive and cannot create organization, project, Release, dist,
credential, callback, or BlobStore authority.

This phase does not add a source-map engine, web scraping, per-file persistent
documents, Hermes extensions, automatic event reprocessing, S3, migrations, MCP,
NATS, sharding, disk spool, Incident Capsules, or any Phase 19 behavior.

## Inputs, outputs, and errors

Artifact assembly accepts an authenticated organization slug, a nonempty canonical
project-slug set, complete SHA-1, an ordered chunk manifest, and optional exact release
and dist. It returns compatible `not_found`, `ok`, or `error` state. Stable internal
failures are invalid request, malformed bundle, unsupported
compression, archive limit, missing resolution identity, forbidden, not found,
conflict, quota exhausted, busy, and temporarily unavailable.

Private lookup accepts repeated canonical Debug IDs plus optional
release/dist. It returns at most 20 authorized immutable bundle candidates. Download
requires an exact project/bundle relation and streams the original bytes.

## Persistence and retry behavior

`artifact_uploads` is the durable owner of assembly retries. Its deterministic
organization/SHA-1 identity preserves ordered packed chunks and atomically merges
compatible authorized bindings. Ready content is deduplicated once per organization.
The final Blob key is reconstructed from organization, bundle ID, and physical
generation; no client supplies a key or generation.

Publication is not `ok` until the immutable blob, ready metadata, requested bindings,
organization quota reservation, and every affected project artifact revision are
acknowledged. Repeated compatible publication is idempotent. Crash recovery may leave
revision gaps or conservative counter overcount, never an acknowledged ready binding
to absent bytes.

Association removal atomically produces either another nonempty ready binding set or
an eligible orphan. Rescue is conditional on the orphan state. GC claims an orphan
with a random token and lease, revalidates before Blob deletion, deletes only the
captured generation, and compacts to a tombstone. Republication requires a durable
upload, allocates exactly one later generation, and cannot be deleted by a stale
worker.

## Resource, cancellation, and shutdown bounds

- complete compressed bundle: initially at most 64 MiB and at most 64 chunks
  (configurable with a hard 512 MiB ceiling);
- logical archive: initially at most 512 MiB, 10,000 entries, 16 MiB per entry, and
  100:1 ratio;
- manifest: at most 4 MiB; path 1,024 bytes; URL 4,096 bytes;
- usable Debug IDs: at most 20,000; bindings per bundle/upload: at most 512;
- at most two concurrent assembly/validation jobs per process;
- parse deadline 30 seconds with cancellation between archive entries;
- private lookup at most 20 candidates and download uses 64 KiB chunks;
- GC scans at most 100 documents per pass with at most four workers, a five-minute
  claim lease, and a bounded BlobStore operation timeout.

All limits are typed startup configuration with hard ceilings. Parsers never extract
to the filesystem, recurse into archives, trust ZIP paths, or retain all entry bodies.
Shutdown stops new work, lets bounded in-flight state reach a recoverable boundary,
and leaves durable uploads/claims for restart recovery.

## Side effects, health, metrics, and safe logging

MongoDB side effects are limited to `artifact_uploads`, `artifact_bundles`, project
`ar`, organization `ab/ac`, and the existing project-deletion registry. Blob effects
are limited to the shared organization chunk namespace and typed artifact-bundle
namespace. Metrics use only
operation, outcome, state, and stable error code. Organization/project IDs, release,
dist, URLs, paths, Debug IDs, source content, tokens, and archive diagnostics are not
metric labels or log fields.

Symbolicator is optional and degraded independently from Event-ingest readiness.
Artifact APIs expose bounded temporary failure when required storage or the adapter
is unavailable. GC/recovery lag and failure contribute stable component status.

## Gate

- current and retained pinned real `sentry-cli sourcemaps upload` contracts;
- valid modern Debug-ID and legacy release/dist bundles;
- malicious ZIP/manifest/path/compression/ratio/size regression corpus;
- compact codec round trips, malformed BSON, golden sizes, index validation, and
  same-array `$elemMatch` explains;
- duplicate publication, shared binding, removal, rescue, quota, crash recovery,
  claim expiry, stale worker, tombstone, and republication tests;
- private service credential, download, and cross-project isolation tests;
- fake and pinned `/symbolicate-js` contract with raw/generated frame preservation;
- one retained RPS profile for modern hit, legacy hit, miss, and open-circuit failure;
- E2E minified JavaScript Event -> real uploaded Source Bundle -> readable derived
  mapped frame;
- workspace format, strict lint, tests, committed report, and zero scoped leftover
  test/server/CLI/k6 processes.
