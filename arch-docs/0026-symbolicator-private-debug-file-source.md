# ADR-0026: Project-private debug-file source for Symbolicator

- Status: Accepted
- Date: 2026-07-21

## Context

ADR-0013 selects a replaceable external Symbolicator backend and ADR-0025 defines how
customer debug files reach the application's BlobStore. Symbolicator still needs a
project-isolated way to discover those files by debug/code identifier and download
them regardless of whether BlobStore is a local filesystem or S3-compatible storage.

Direct S3 access would give Symbolicator broad storage credentials, require a
filesystem-style object layout, and would not support the default local BlobStore.
A shared Docker volume has the same topology coupling and is not a supported
project-indexed Symbolicator source. Symbolicator already has a `sentry` source type
for listing customer debug files through an index endpoint and downloading a selected
immutable file by ID.

## Decision

### Use Symbolicator's Sentry source

Every native symbolication request contains one application-generated private source
for the authorized project:

```json
{
  "id": "private-project-18273645",
  "type": "sentry",
  "url": "http://app:8080/internal/symbolicator/projects/18273645/debug-files/?revision=7",
  "token": "sym1.project-bound-mac"
}
```

The callback origin comes only from trusted application configuration. No Event,
SDK, user-supplied URL, project setting, or request header can alter it. External
administrator-approved symbol sources remain separate from this private source.

Processor calls Symbolicator with `scope={project_id}`. The project ID is globally
unique within the installation and isolates Symbolicator caches without introducing
another identifier.

### Internal index and download endpoint

The application implements a private streaming endpoint with the behavior expected
by Symbolicator's Sentry source.

An index lookup is:

```text
GET /internal/symbolicator/projects/{project_id}/debug-files/
    ?revision={revision}
    &debug_id={optional_debug_id}
    &code_id={optional_code_id}
```

At least one of `debug_id` or `code_id` is mandatory. Lookup is always constrained by
the project from the path and returns at most 20 ready immutable candidates:

```json
[
  {"id": "01J...", "symbolType": "pdb"}
]
```

`symbolType` uses the exact Symbolicator values for the stored object type, including
`pe`, `pdb`, `portablepdb`, `macho`, `elf`, and `breakpad` in the accepted initial
scope. The application never returns an uploading, failed, deleted, or other-project
file.

A file download is:

```text
GET /internal/symbolicator/projects/{project_id}/debug-files/
    ?revision={revision}
    &id={immutable_debug_file_id}
```

The application resolves the ID under the same project, opens the approved BlobStore
object, and streams it with a bounded buffer. It does not load the file into RAM,
return a local path, expose a BlobStore credential, or redirect to a presigned URL in
version one. Missing, deleted, non-ready, or cross-project IDs return the same generic
not-found response.

Debug-file IDs are immutable and never reused. The BLAKE3 digest from ADR-0025 is
returned as an ETag and verified through the BlobStore integrity contract; the HTTP
response remains the original file bytes.

### Project-bound service credential

The private source does not use a personal API token, Web session, or DSN key. Its
stable bearer credential is derived without a database record:

```text
mac = HMAC-SHA256(
    symbolicator_source_secret,
    "symbolicator-project-source-v1" || project_id_be_u32
)

token = "sym1." || base64url_no_pad(mac)
```

The configured secret contains at least 32 random bytes. The endpoint recomputes the
MAC for the project in the path and compares it in constant time. A token for one
project cannot read another project's index or files.

The credential is intentionally stable because Symbolicator includes source
credentials in its index-cache key. It is accepted only by these internal read-only
routes and is never treated as an application API token. Logs, traces, error bodies,
and candidate diagnostics redact it.

Rotation may configure current and previous secrets during a bounded grace period.
New requests use only the current secret; removal of the previous secret completes
rotation without MongoDB writes.

### Cache identity and revision

Symbolicator caches Sentry index results by URL and credentials, including negative
results, while downloaded object and derived caches use stable source/file identity.
The application therefore separates the two identities:

```text
source.id               = "private-project-{project_id}"   // stable
Symbolicator scope      = project_id                       // stable
source.url revision     = projects.debug_files_revision   // changes
debug file id           = immutable                        // stable
```

`projects.debug_files_revision` is a monotonically increasing bounded counter,
logically zero when absent. ADR-0027 defines its physical nonnegative BSON `int64`
representation. It is incremented after a debug-file metadata record becomes ready
and after a ready file is deleted. Project command/cache state is invalidated locally
at the same time.

Changing only the URL forces a fresh index lookup after upload or deletion. Keeping
the source ID and file IDs stable preserves existing downloaded-object, symcache, and
CFI cache entries.

An assembly job from ADR-0025 is not reported as complete until the ready metadata is
visible and a revision increment has been acknowledged. A crash may cause a retry to
increment the revision more than once; gaps are harmless because the value is a cache
generation, not a business counter.

### Access and resource limits

The internal endpoint is reachable through the configured Docker/private network URL
and is not advertised as a public API. The bearer check remains mandatory even when
network isolation exists. Symbolicator's `sentry` source is used specifically so
private/reserved network access does not require enabling reserved destinations for
arbitrary HTTP sources.

Index responses are bounded to 20 candidates because Symbolicator's Sentry downloader
does not paginate that result. Deterministic ranking prefers an exact debug ID, then
an exact code ID, ready native debug information, and newest upload as the final
tie-breaker. The exact file-type registry is versioned with the storage codec.

File downloads have independent concurrency, response-size, inactivity-timeout, and
total-time budgets. Symbolicator caches originals and derived artifacts locally, so
the application normally streams a particular immutable object only on a cache miss.

### Failure behavior

MongoDB or BlobStore unavailability returns a retryable server error to Symbolicator.
An absent candidate returns an empty index or not found and is classified by the
existing symbolication policy. Symbolicator unavailability or callback failure never
allows an Event to remain pending forever; ADR-0013's bounded retry and partial/raw
fallback still applies.

## Consequences

- One source adapter works with local filesystem and S3-compatible BlobStore.
- Symbolicator receives neither user credentials nor general BlobStore credentials.
- Project isolation is enforced both by cache scope and by the callback credential.
- The first object download passes through the application, but subsequent work can
  use Symbolicator's object and derived caches.
- New uploads invalidate negative index results without discarding valid symcaches.
- Replacing Symbolicator later affects only the existing `SymbolicationService` and
  private-source adapter boundaries.

## Deferred questions

- Direct S3 download or presigned redirect as a measured optimization.
- Candidate ranking beyond the bounded initial rules.
- Multi-node propagation of `debug_files_revision` cache invalidation.
