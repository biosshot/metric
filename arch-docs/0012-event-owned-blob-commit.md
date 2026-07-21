# ADR-0012: Commit protocol for event-owned blobs

- Status: Accepted
- Date: 2026-07-21

## Context

An SDK Envelope may contain an event and event-owned binary items such as attachments,
screenshots, view hierarchies, or minidumps. Event metadata is stored in MongoDB while
bytes are stored in BlobStore. There is no distributed transaction between MongoDB
and a local filesystem or S3-compatible object store.

The system must not acknowledge an event whose accepted attachment references can
point to missing or partially written objects. Retries and crashes must remain
idempotent without requiring a cross-storage transaction.

## Decision

### Blob-first commit order

For every attachment permitted by the active policy, Ingest performs:

1. bounded parsing and project authentication;
2. event and attachment metadata scrubbing;
3. format-specific content scrubbing when required;
4. streaming write to a temporary BlobStore object while computing size and checksum;
5. verification and atomic publication of the final object;
6. MongoDB event insertion containing final blob references;
7. HTTP success only after MongoDB confirms the event insert.

The MongoDB event is the authoritative commit record. Every blob referenced by an
accepted event must already be durably readable. An object without a referencing
event is an orphan and is safe to remove after reconciliation.

### Blob identity and paths

An event-owned attachment identifier is deterministically derived from project ID,
event ID, Envelope item position, and content checksum. User-controlled filenames are
not used as storage paths.

The logical final key is project and event scoped:

```text
projects/{project_id}/events/{event_id}/{attachment_id}
```

Checksum calculation occurs while streaming the scrubbed bytes. The checksum is used
for verification, retry identity, corruption detection, and archive metadata. The
first version does not deduplicate blobs across different events and therefore does
not require cross-event reference counting.

### Atomic BlobStore publication

BlobStore exposes a domain operation that does not make a partial final object
visible. A local-filesystem implementation writes and syncs a temporary file before
an atomic rename on the same filesystem. An S3-compatible implementation completes
and verifies its upload before reporting the final object as committed.

The exact filesystem and object-store mechanics are backend-specific, but successful
publication guarantees that subsequent reads of the final key return the complete
scrubbed bytes.

### Event metadata

Blob bytes are never embedded in event BSON. The event contains bounded metadata:

```javascript
attachments: [
  {
    attachment_id,
    blob_key,
    filename,
    content_type,
    attachment_type,
    size,
    checksum,
    created_at
  }
]
```

Display filenames are length-limited, stripped of path traversal and control
characters, and remain untrusted metadata. API download authorization is always
resolved from the project and event relation rather than possession of `blob_key`.

### Permanent item rejection

An attachment disabled by project policy, or unsupported by `scrubbable_only`, does
not prevent an otherwise valid event from being accepted. It produces an attachment
outcome and the Sentry-compatible category capability signal defined by ADR-0018.

Request, event, or item size-limit violations retain the ADR-0010 behavior: the
Envelope is rejected with HTTP `413`.

### Temporary failure

If an enabled and otherwise valid attachment cannot be durably stored because of a
temporary BlobStore failure, the event is not inserted and the request returns HTTP
`503` with retry guidance. This preserves the SDK's opportunity to resend the whole
Envelope rather than accepting the event while silently losing an expected file.

### Duplicate and conflicting retries

An already accepted deterministic event ID uses first-write-wins semantics. A retry
with the same event ID is acknowledged as a duplicate and does not append or replace
attachments. A different payload or attachment checksum for that ID produces a
conflict outcome for observability but cannot mutate the accepted event.

Concurrent requests may publish different candidate objects before one MongoDB event
insert wins. Objects not referenced by the winning event become ordinary orphans.

### Orphan cleanup

Temporary and unreferenced objects are reconciled by Scheduler after a configurable
grace period:

```toml
[attachments.cleanup]
orphan_grace_period = "24h"
```

The grace period covers ambiguous MongoDB acknowledgements, request retries, and
process crashes. Runtime code may eagerly remove known failed uploads, but scheduled
reconciliation is the correctness backstop.

### Retention

Event-owned blobs follow their parent event's retention and archival outcome. MongoDB
TTL cannot remove BlobStore objects, so scheduled blob archival or deletion is
coordinated with event expiration. Temporary lag may leave an orphaned blob after its
event is deleted, but a retained accepted event must not reference a prematurely
deleted blob.

Standalone minidumps that synthesize an Event follow the specialized acceptance and
retention contract in ADR-0020 while reusing this orphan-cleanup principle.

Debug symbols and source maps are not event-owned. They have separate identities,
many-to-many event relationships, and a separate retention decision.

## Consequences

- An accepted event cannot contain a committed reference to a partially written
  attachment.
- A crash can create extra orphan objects but not a missing accepted blob.
- MongoDB remains the single acceptance commit point for the complete accepted event
  manifest.
- BlobStore failure affects only Envelopes that require enabled blob persistence;
  event-only ingestion remains available.
- Duplicate SDK retries cannot mutate an existing event or replace its files.
- Orphan cleanup is required operational work but does not require distributed
  transactions.

## Deferred questions

- Exact supported text, JSON, minidump, and view-hierarchy scrubbers.
- Attachment list and download authorization API.
- Object-store checksum and multipart-upload compatibility details.
- Blob archival transition and deletion-job idempotency.
