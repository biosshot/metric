# ADR-0025: Sentry CLI-compatible debug-file upload

- Status: Accepted
- Date: 2026-07-21

## Context

ADR-0013 accepts persistent project-private debug files and a replaceable
symbolication backend, but leaves their upload protocol open. Existing build systems
already use `sentry-cli debug-files upload`; requiring a custom uploader would weaken
the intended Sentry compatibility.

Large ELF, dSYM, and PDB files must not pass through MongoDB or be retained wholly in
RAM. The implementation must also remain simple in the initial single-process
`--role=all` runtime. TCP provides ordered, reliable transport to the peer, but does
not itself mean that an HTTP handler validated and durably published an object. The
normal successful HTTP response is sufficient after that publication; a second
application acknowledgement system per chunk is unnecessary.

## Decision

### Compatibility boundary

Version one implements the wire contract used by `sentry-cli debug-files upload`.
The legacy hidden `upload-dif` alias uses the same client implementation and is
therefore supported by the same endpoints:

```text
GET  /api/0/organizations/{organization_slug}/chunk-upload/
POST /api/0/organizations/{organization_slug}/chunk-upload/
POST /api/0/projects/{organization_slug}/{project_slug}/files/difs/assemble/
```

The GET response advertises only implemented capabilities. Initial defaults are:

```json
{
  "url": "organizations/example/chunk-upload/",
  "chunkSize": 8388608,
  "chunksPerRequest": 64,
  "maxFileSize": 2147483648,
  "maxRequestSize": 33554432,
  "concurrency": 4,
  "hashAlgorithm": "sha1",
  "compression": ["gzip"],
  "accept": ["debug_files", "pdbs", "portablepdbs", "artifact_bundles"],
  "maxWait": 300
}
```

Size, concurrency, wait, and temporary-retention limits are configurable with safe
validated bounds. `artifact_bundles` is advertised only with ADR-0028 enabled.
`sources`, `bcsymbolmaps`, `il2cpp`, `proguard`, `release_files`, and unimplemented
artifact-bundle variants are not advertised until their formats are implemented.

Compatibility is enforced by contract tests that execute pinned real `sentry-cli`
binaries. The matrix includes the selected current 3.x release and the retained 2.x
compatibility release; it does not claim compatibility with every historical or
future untested binary.

### Organization and project slugs

The compatible routes require stable slugs in addition to the numeric identifiers
already accepted by ADR-0019. An organization slug is globally unique. A project
slug is unique within its organization. Both are canonical lowercase ASCII,
hyphen-separated, bounded to 63 bytes, and immutable in version one. Display names
may change independently.

Slugs are control-plane lookup keys only. Events, Issues, hourly buckets, and other
high-volume project-owned documents continue to store only the numeric project ID.

### Assemble contract

The assemble request is a map keyed by the SHA-1 checksum of each complete file:

```json
{
  "complete-file-sha1": {
    "name": "application.pdb",
    "debug_id": "optional-debug-id",
    "chunks": ["chunk-sha1-1", "chunk-sha1-2"]
  }
}
```

The chunk list is ordered and may contain the same checksum more than once. The
response is a map with the compatible per-file states `not_found`, `created`,
`assembling`, `ok`, and `error`. `not_found` returns `missingChunks`; `ok` returns a
compatible DIF metadata object.

Each file is independent. One malformed file does not rewrite the state of another
file in the same assemble request.

### Chunk write path

Temporary chunks are BlobStore objects and are scoped to an organization:

```text
debug-chunks/{organization_id}/{sha1}
```

The POST handler streams each multipart part, performs bounded gzip decompression
when the field is `file_gzip`, computes its SHA-1, rejects a mismatch, and publishes
the object atomically only after validation. It then returns the ordinary successful
HTTP status expected by `sentry-cli`.

There is deliberately no MongoDB document, status row, acknowledgement queue,
reference count, or BLAKE3 value for an individual temporary chunk. BlobStore object
existence is the sole source of truth. An idempotent retry of an already existing
organization/checksum object succeeds.

Organization scoping prevents chunk reuse or poisoning across tenants. The accepted
first version does not try to defend mutually authorized users inside one
organization from a deliberately constructed SHA-1 collision; adding an internal
secondary digest for temporary chunks remains possible without changing the wire
protocol.

### Missing-chunk discovery

On an assemble request, the service checks the required BlobStore keys. If any are
absent, it returns `not_found` with exactly those ordered checksums and creates no
durable assembly job. `missingChunks` is retained because it is part of the
`sentry-cli` resume and deduplication protocol, not as an additional transport
acknowledgement mechanism.

### One durable job per complete file

After every required chunk exists, the service idempotently creates one
`debug_uploads` document for the whole file and enqueues it on a bounded in-process
assembly queue. Its conceptual contents are:

```javascript
{
  _id,                 // deterministic from project and complete-file SHA-1
  organization_id,
  project_id,
  complete_sha1,       // 20 binary bytes, not hexadecimal text
  name,
  optional_debug_id,
  chunks,              // ordered array of 20-byte binary SHA-1 values
  state,               // pending, assembling, complete, or failed
  attempt,
  next_attempt_at,
  created_at,
  updated_at,
  error_code
}
```

This is an assembly/recovery record, not a record of transport delivery. A retry of
the same project/checksum observes the same job. Scheduler recovers pending and stale
assembling jobs after restart, so a crash after the HTTP request cannot permanently
strand an upload.

The worker concatenates chunks in manifest order through a bounded streaming reader,
recomputes the complete SHA-1, computes BLAKE3 for internal persistent-blob integrity,
parses the debug-file metadata, atomically publishes the final project-private
BlobStore object, and idempotently creates `debug_files` metadata. It never loads the
complete file into RAM.

The compatible states map as follows:

```text
missing chunk                    -> not_found
durable job newly accepted       -> created
pending or actively processing   -> assembling
debug_files record committed     -> ok
permanent validated failure      -> error
```

### Retention and cleanup

Temporary chunks expire after 24 hours by default, based on BlobStore metadata. The
duration is configurable. Assembly is expected to finish well inside this window;
if required chunks disappear, a later assemble request truthfully returns them as
missing and `sentry-cli` can upload them again.

Completed customer debug files retain the no-automatic-expiry rule from ADR-0013.
Failed and completed assembly-job cleanup is separately configurable. Orphaned final
temporary objects are reconciled using the same publish-then-metadata pattern as
ADR-0012.

### Authorization

Debug-file endpoints require a personal API token from ADR-0021, never a DSN key.
The token organization must equal the organization resolved by the URL, and the
project must belong to that organization. Stable scopes are:

```text
debug_file:read
debug_file:write
debug_file:delete
```

Upload and assemble require `debug_file:write`. API and UI listing require
`debug_file:read`; explicit deletion requires `debug_file:delete`. Role permissions
and token scopes are intersected normally.

### Reprocessing

Successful upload does not automatically enqueue every historical Event. The bounded
explicit project/time-range reprocessing contract from ADR-0013 remains unchanged.

After final metadata becomes ready, ADR-0026 increments the project's debug-file
revision and exposes the immutable file through the private Symbolicator source. An
assembly job is complete only after that publication step is acknowledged.

## Consequences

- Existing build pipelines can use the real `sentry-cli` upload command.
- MongoDB receives one recoverable job per file, not one document per chunk.
- Interrupted uploads resume by BlobStore existence without a custom acknowledgement
  subsystem.
- Large files remain streaming and bounded in memory.
- Local BlobStore works in the all-in-one deployment; shared S3-compatible BlobStore
  is required before multiple application nodes can assemble the same uploads.
- SHA-1 remains a compatibility identifier while final stored files receive an
  internal BLAKE3 integrity digest.

## Deferred questions

- Native source bundles, BCSymbolMaps, IL2CPP, and ProGuard uploads.
- Automatic affected-Event discovery after a debug-file upload.
- Multi-node assembly claims and leases if application roles are later separated.
