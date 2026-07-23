# Phase 16 contract: local BlobStore, attachments, and standalone minidumps

Status: implemented and verified by the Phase 16 exit gate.
Owning ADRs: ADR-0001, ADR-0002, ADR-0007, ADR-0010, ADR-0012, ADR-0018,
ADR-0020, ADR-0034, ADR-0035, ADR-0036, ADR-0037, ADR-0039

## Responsibility and boundaries

The domain owns validated logical blob keys, object identity, checksums, kinds, and
event-owned metadata. The ports crate owns streaming BlobStore sessions and reference
queries. The local adapter owns filesystem containment, capacity reservation,
temporary writes, durable sync, and atomic publication.

Ingest owns the blob-first acceptance protocol. It may persist a MongoDB Event only
after every enabled attachment or minidump object is durably readable. MongoDB remains
the authoritative acceptance record; a published object without a winning Event is
an orphan, never an accepted event with missing bytes.

This phase does not add S3, debug-file upload, Symbolicator minidump processing,
source maps, MCP, migrations, NATS, sharding, or disk spool.

## BlobStore contract

Logical keys have the fixed event-owned form:

```text
projects/{project_id}/events/{event_id}/{object_id}
```

User filenames never enter paths. Writes accept bounded chunks, account capacity
before writing, compute BLAKE3 incrementally, sync a temporary object, and publish it
without replacing an existing final object. An identical retry is idempotent; a
different object for the same key fails closed. Reads are chunked and final objects
only. The configured reserve is unavailable to new writes.

## Attachment policy and acceptance

The first safe policy accepts only explicitly scrubbable UTF-8 `text/plain` and
`application/json` event attachments. It bounds item count, per-item bytes, aggregate
bytes, filename, content type, and metadata. Unsupported attachments are permanently
dropped while an otherwise valid Event may be accepted.

Accepted metadata is stored in the sanitized Event body; bytes never enter BSON.
BlobStore temporary/publish failure returns a retryable failure and prevents Event
insertion. A MongoDB failure after publication leaves a safe orphan for reconciliation.

## Standalone minidump

`POST /api/{project_id}/minidump/` accepts raw `application/octet-stream` and a
bounded `multipart/form-data` field named `upload_file_minidump`. The capability is
disabled by default and must be explicitly enabled in configuration. Input is capped
at 100 MiB, written incrementally, and only bounded header bytes are retained for
magic and stream-directory validation.

A supplied compatible Event ID is preferred; otherwise the ID is derived from project
and minidump checksum. A minimal sanitized native fatal Event references the final
object. Raw bytes are never placed in MongoDB or the Processor RAM queue.

## Cleanup, retention, and authorization

Reconciliation scans bounded pages older than the configured orphan grace period,
checks the project/Event/object relation through a typed port, and deletes only
unreferenced objects. Event retention owns object lifetime; lag may leave an orphan,
but cleanup must never delete a blob referenced by a retained Event.

Attachment metadata and download routes authorize through the existing project and
Event relationship. A blob key is not an authorization credential.

## Gate

- local BlobStore conformance, atomic/crash publication, traversal, checksum, reserve,
  concurrency, and slow-filesystem tests;
- bounded request, envelope item, attachment, decompression, multipart, and minidump
  validation tests;
- BlobStore/EventSink failure matrix proving no accepted Event references missing data;
- real SDK attachment and representative raw/multipart minidump compatibility tests;
- authenticated metadata/download E2E with project isolation;
- one retained local baseline reporting object RPS and MiB/s;
- workspace format, lint, tests, and explicit verification that test/server processes
  have exited.
