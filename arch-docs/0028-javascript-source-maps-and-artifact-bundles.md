# ADR-0028: JavaScript source maps and artifact bundles

- Status: Accepted
- Date: 2026-07-21

## Context

JavaScript and Node.js error Events remain difficult to understand and group when
their stack frames contain only minified functions, generated filenames, and output
line/column positions. Current `sentry-cli sourcemaps upload` packages scripts,
source maps, original sources, URL metadata, and Debug IDs into one deterministic
Source Bundle and uploads it through the organization chunk service.

ADR-0025 already provides compatible chunk transport and temporary BlobStore
objects. ADR-0013 and ADR-0026 already isolate Symbolicator behind an application
adapter and a project-private callback. Source-map support should reuse these
boundaries rather than introduce per-file upload endpoints, MongoDB documents for
every `.js`/`.map`, or an application-owned source-map engine.

## Decision

### Sentry CLI artifact-bundle protocol

The server supports the current artifact-bundle path used by
`sentry-cli sourcemaps upload`:

```text
GET  /api/0/organizations/{organization_slug}/chunk-upload/
POST /api/0/organizations/{organization_slug}/chunk-upload/
POST /api/0/organizations/{organization_slug}/artifactbundle/assemble/
```

The first two routes and organization-scoped chunks are the same as ADR-0025. The
assemble request is:

```json
{
  "checksum": "complete-bundle-sha1",
  "chunks": ["chunk-sha1-1", "chunk-sha1-2"],
  "projects": ["frontend"],
  "version": "1.42.0",
  "dist": "web"
}
```

`version` and `dist` are optional according to the compatible protocol. `projects`
must be nonempty, every slug must belong to the URL organization, and the caller must
be authorized for every target. Duplicate projects are canonicalized.

The response is one compatible object with `state`, `missingChunks`, and `detail`.
States use the existing `not_found`, `created`, `assembling`, `ok`, and `error`
semantics. The chunk-options response advertises `artifact_bundles` only after this
path is enabled; unimplemented artifact-bundle variants are not advertised.

Pinned real `sentry-cli` contract tests cover source-map injection/upload, a bundle
with Debug IDs, a legacy release bundle, repeated upload, missing chunks, multiple
projects, optional dist, polling, and permanent validation errors.

### Whole-bundle storage

The application stores the original Source Bundle as one immutable BlobStore object.
It does not extract each script, source map, or original source into a separate
MongoDB document or persistent BlobStore object.

Two collections separate transient work from ready metadata:

```text
artifact_uploads
artifact_bundles
```

`artifact_uploads` contains one recoverable whole-bundle assembly job. Its worker
streams organization-scoped chunks in order, verifies complete SHA-1, computes
BLAKE3, validates the Source Bundle, builds its compact lookup projection, publishes
the immutable object, inserts ready metadata, and increments every affected project
revision. Its retry and terminal-expiration behavior follows the principles of
ADR-0025, but it remains a separate collection because its project array,
release/dist context, manifest validation, and output metadata differ from native
debug-file assembly.

`artifact_bundles` contains ready metadata conceptually shaped as:

```javascript
{
  _id,
  organization_id,
  project_ids,
  bundle_debug_id,
  release_id,
  dist,
  debug_id_tokens,
  complete_blake3,
  complete_sha1,
  size,
  uploaded_at
}
```

ADR-0029 defines the physical compact BSON codec, organization-level content
deduplication, and canonical project/Release/dist bindings. Bundle content and its
content-derived metadata are immutable; only authorized bindings may grow or shrink.
Bundle attributes
such as `org`, `project`, `release`, `dist`, and `note` are untrusted descriptive
metadata. The authenticated assemble request and resolved control-plane identities
are authoritative.

### Safe Source Bundle validation

The parser uses the compatible Symbolic Source Bundle format and never extracts the
archive into a general filesystem directory. It validates the header, ZIP directory,
manifest, entry paths, entry types, metadata, and Debug IDs through bounded streaming
or random-access readers.

Configurable limits cover compressed bundle bytes, total logical/uncompressed bytes,
entry count, individual entry bytes, manifest bytes, path and URL length, metadata
cardinality, compression ratio, parse time, and assembly concurrency. Duplicate
paths, conflicting Debug IDs, invalid UTF-8 where required, traversal paths,
unsupported compression, recursive archives, and malformed source maps receive
stable error codes. Unknown safe manifest fields may be ignored for forward
compatibility; they never become trusted routing or credentials.

At least one usable embedded Debug ID or an authorized release must exist. A bundle
with neither cannot later be resolved and is rejected with a compatible permanent
error rather than accepted as silently useless storage.

### Modern Debug ID lookup

Every unique usable JavaScript Debug ID in the bundle produces one domain-separated
64-bit lookup token:

```text
BLAKE3-64(
    "js-artifact-debug-id-v1" || organization_id_be_u64 || canonical_debug_id
)
```

Tokens are deduplicated, sorted, bounded, and stored as BSON `int64` values in one
array. A single multikey `{ k: 1 }` index locates modern candidates. The application
then verifies organization and target-project membership before returning them.

A 64-bit collision can only add a candidate: Symbolicator validates the real Debug
ID inside the downloaded bundle. It cannot authorize another project's artifact or
produce a trusted false match.

### Legacy release, dist, and URL lookup

For Events without injected Debug IDs, the application resolves the exact
organization-scoped Release identity from ADR-0017 and queries ready bundles by
project, release, and optional exact dist. It does not persist or index one URL token
per internal file. Symbolicator receives the bounded candidate bundles and performs
the exact normalized URL/file-stem lookup inside their manifests.

Modern Debug ID candidates take priority. Release candidates are a compatibility
fallback. A supplied dist matches the exact dist first; absence and fallback behavior
are explicit and contract-tested rather than implemented as an unbounded query.

### Symbolicator JS adapter

For JavaScript and Node.js error stack traces, `SymbolicationService` calls:

```text
POST /symbolicate-js?scope={project_id}
```

The request contains raw stack traces, normalized source-map modules from
`debug_meta.images`, optional exact release and dist, project-private artifact source,
and bounded options. Symbolicator returns rewritten frames, raw frames, module
errors, used bundle IDs, and scraping diagnostics. Processor maps those into the same
backend-independent derived symbolication result model used by ADR-0013.

Missing or malformed maps do not make an Event permanently pending. Raw generated
frames remain available, and the existing bounded retry and partial-result policy
applies.

### Private artifact lookup callback

The application provides a project-private callback compatible with Symbolicator's
JS Sentry lookup:

```text
GET /internal/symbolicator/projects/{project_id}/artifact-lookup/
GET /internal/symbolicator/projects/{project_id}/artifacts/?id={bundle_id}
```

The lookup accepts the compatible repeated `debug_id` and `url` parameters plus
optional release and dist. It returns bounded `bundle` results containing immutable
IDs, authenticated internal download URLs, and exact `resolved_with` classifications
such as `debug-id` or `release`. Downloads stream the original bundle from BlobStore.

The source uses a distinct stable per-project HMAC credential domain:

```text
"symbolicator-project-artifacts-v1" || project_id_be_u32
```

It follows ADR-0026's constant-time validation, current/previous secret rotation,
redaction, configured callback origin, no user credential, and no BlobStore credential
rules.

### Independent artifact cache revision

Each project has a logical monotonically increasing `artifact_revision`, physically
stored later as compact field `ar`; absence means zero. Publication, project
association removal, and ready-bundle deletion increment every affected project.

The revision appears in the artifact source URL, while its source ID, project scope,
and immutable bundle IDs remain stable. This invalidates negative/index results after
changes without discarding downloaded bundles or derived source-map caches. Native
`debug_files_revision`/`dr` remains independent so JS uploads do not invalidate
native caches.

An artifact assembly job is not `ok` until ready metadata and all required revision
increments are acknowledged. Retry may create harmless revision gaps.

### Scraping policy

Symbolicator web scraping is disabled by default. Uploaded private artifacts are the
authoritative first-version source:

```toml
[symbolication.javascript.scraping]
enabled = false
```

An optional future administrator-defined origin allowlist may enable scraping with
strict DNS/IP/redirect/size/time controls. Events and ordinary project data can never
supply allowed origins, outbound credentials, or request headers.

### Authorization and retention

Personal API tokens use stable scopes:

```text
artifact:read
artifact:write
artifact:delete
```

Upload and assemble require write permission for all selected projects. Lookup and UI
listing require read permission; explicit removal requires delete permission.
Symbolicator uses only the separate internal service credential.

Ready bundles are project-private and have no automatic expiration in this decision.
Project deletion removes its association and deletes an unreferenced bundle. Release-
or retention-driven artifact removal requires a later explicit policy so historical
Events do not unexpectedly lose source context.

## Consequences

- Current Sentry build tooling uploads source maps without a custom client.
- Native and JavaScript uploads reuse one chunk protocol and BlobStore.
- One bundle and compact index replace potentially thousands of persistent files and
  documents.
- Modern Debug ID and legacy release/dist/URL Events are both resolvable.
- Source code stays project-private and web scraping is opt-in rather than implicit.
- Symbolicator performs the complex mapping while the application retains a
  replaceable backend boundary.

## Deferred questions

- Optional administrator-controlled web scraping.
- React Native/Hermes-specific validation corpus beyond the accepted wire path.
- Automatic bounded reprocessing after artifact publication.
