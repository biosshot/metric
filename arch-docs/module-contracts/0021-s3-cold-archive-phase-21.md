# Phase 21 module contract: S3 blob storage and cold Event archive

Status: accepted implementation contract
Owner: Blob adapter and archive application module
Owning decisions: ADR-0001, ADR-0007, ADR-0012, ADR-0022, ADR-0030
Sequential gate: ADR-0039 Phase 21

## Boundary

`BlobStore` remains the only application-facing object-storage boundary. The local
filesystem and S3 implementations obey the same immutable-object contract; callers
do not branch on the selected backend. S3 configuration and credentials are owned
by server composition and never enter domain values, logs, metrics, or MongoDB.

`ArchiveService` owns segment construction and the archive state machine.
`ArchiveStore` owns bounded MongoDB claims, durable manifests, and the terminal Event
transition. Neither the Event API nor search reads Parquet objects in this phase.

## Object publication contract

- Logical keys are validated domain values. Event archives use deterministic
  `projects/{project}/archives/events/YYYY/MM/DD/{segment}.parquet` keys.
- S3 writes first target a unique `metric-temporary/` key. Objects larger than one
  configured part use multipart upload.
- Every part is bounded in memory and retryable. An interrupted part may be repeated;
  an incomplete multipart upload is aborted best-effort.
- Publication completes the temporary object, server-side copies it to the final
  immutable key, verifies size and BLAKE3 metadata plus streamed content, and only
  then removes the temporary key.
- Repeating publication of identical bytes succeeds. A different object at the same
  logical key is corruption and is never overwritten.
- Missing objects, denied access, checksum mismatch, size mismatch, and unavailable
  storage have distinct stable adapter failures. Credentials and endpoint data are
  redacted.

## Archive state machine and ordering

Archival is disabled by default and requires MongoDB when enabled. A successful or
terminally failed Event that reaches retention receives `h` (archive due), not `x`
(hot expiry).

For each bounded project/day segment the service performs:

1. claim or resume a durable manifest;
2. encode the selected canonical scrubbed Events as Parquet with Zstd;
3. publish and verify the immutable blob;
4. mark the manifest complete with exact key, size, checksum, schema and count;
5. set the source Events' archive segment `z`, set hot expiry `x`, and remove `h`;
6. mark the manifest sources committed.

No source Event receives `x` before the complete verified manifest exists. Failure
before step 4 leaves every source Event queryable with `h`. Failure between steps 4
and 6 resumes from the complete manifest and never rewrites a conflicting segment.
All steps are idempotent and safe to retry after process termination.

The Parquet schema is version 1 and contains only Event identity, project identity,
received/occurred time, optional Issue identity, and canonical scrubbed Event JSON.
It contains no API keys, authorization headers, raw attachments, minidumps, debug
files, source archives, or unsanitized diagnostics.

## Limits, cleanup, and cancellation

- A claim contains at most 10,000 Events; the default is 500.
- Target uncompressed segment size is configurable up to 512 MiB and cannot exceed
  the configured maximum blob size.
- Parquet generation runs on the bounded blocking pool. Blob writes use configured
  chunks and S3 holds at most one multipart part plus SDK transport buffers.
- Polling and cleanup are bounded. Shutdown stops new work and waits through the
  ordinary server cancellation fence.
- Project deletion owns archive manifest purge. The archive cleanup pass removes old
  unreferenced archive objects after the configured grace period; it does not infer
  ownership from untrusted object contents.

## Conformance and compatibility

The shared immutable-object conformance suite runs against both local storage and
the S3 adapter. The deterministic S3 emulator covers multipart interruption/retry,
missing objects, permission denial, immutable conflicts, listing, and verification.
An ignored environment-driven test runs the same conformance against a dedicated
AWS S3 or compatible service bucket:

```text
METRIC_S3_TEST_ENDPOINT
METRIC_S3_TEST_REGION
METRIC_S3_TEST_BUCKET
METRIC_S3_TEST_ACCESS_KEY_ID
METRIC_S3_TEST_SECRET_ACCESS_KEY
```

The bucket must be disposable and isolated because the test writes and removes its
own fixed conformance keys.

## Stable diagnostics and observability

Archive outcomes are closed labels such as `archived`, `no_work`, `retry`, and
`cleanup`. Metrics report Event count and input/stored bytes. Performance evidence
reports archive Events/s (RPS), input MiB/s, stored MiB/s, and concurrent foreground
work. Object bytes, Event payloads, endpoint URLs, credentials, and arbitrary remote
responses are never logged or used as metric labels.

## Deliberately deferred

Archive search, restore, rehydration, lifecycle-policy management, cross-region
replication, NATS, MCP, sharding, disk spool, online migrations, and Phase 22
capabilities remain outside Phase 21.
