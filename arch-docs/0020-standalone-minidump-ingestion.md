# ADR-0020: Standalone minidump ingestion

- Status: Accepted
- Date: 2026-07-21

## Context

Crashpad, Breakpad, Electron, and other native crash reporters can submit a minidump
without an ordinary Sentry Error Event. The minidump is therefore both the processing
input and the source from which a server-owned native Event must be synthesized.

Minidumps can be large and can contain sensitive process memory. They must not be
buffered in the Processor RAM queue or treated as ordinary scrubbed attachments. The
design must preserve retry idempotency and optional reprocessing after symbols become
available.

## Decision

### Endpoint and authentication

The server implements the Sentry-compatible endpoint:

```text
POST /api/{project_id}/minidump/
```

It uses the DSN key and project validation from ADR-0019. The first parser accepts:

- a raw minidump in an `application/octet-stream` request body;
- `multipart/form-data` with a required `upload_file_minidump` field.

HTTP content encoding is handled by the bounded common ingest layer. Compressed file
containers, legacy nested Electron multipart, Unreal packages, and Apple crash reports
are deferred format adapters.

### Separate capability and policy

Minidump ingestion is separate from the generic attachment policy:

```toml
[native_crash.minidump]
enabled = false
raw_retention = "event"
max_bytes = "100 MiB"
```

It is disabled by default because a minidump can contain stack and heap memory that
cannot be reliably scrubbed without corrupting the processing input. A project or
native-project setup flow must explicitly enable it with that disclosure.

Enabling minidumps does not enable arbitrary binary attachments. Supplemental generic
attachments still follow ADR-0011.

### Streaming acceptance and validation

Ingest streams the raw or multipart minidump to a temporary BlobStore object while
computing BLAKE3, enforcing compressed/decompressed and minidump limits, and retaining
only bounded header bytes in memory. It validates minidump magic and enough structural
header/directory bounds to reject data that cannot safely be handed to the backend.

Full stackwalking and symbol lookup never occur in the HTTP request path.

The acceptance order is:

```text
authenticate and enforce request limits
    -> stream temporary minidump and compute checksum
    -> validate bounded structure
    -> determine Event ID
    -> atomically publish final BlobStore object
    -> insert pending synthetic Event in MongoDB
    -> return Event ID
```

MongoDB is the authoritative acceptance record. A published object without a winning
Event document is an orphan handled by the ADR-0012 cleanup protocol.

### Event identity

Event ID selection is:

1. a valid ID from compatible Sentry event metadata;
2. a valid `sentry[event_id]` or recognized native reporter field;
3. a deterministic 16-byte ID derived from project ID and minidump checksum.

The fallback digest is converted to a valid Sentry-compatible UUID representation.
This makes a byte-identical upload without a supplied ID idempotent when a response is
lost. Identical bytes in one project represent the same occurrence; first-write-wins
and conflicting-retry behavior follow ADR-0012.

### Synthetic accepted Event

Before Processor runs, Ingest persists a minimal sanitized Event:

```javascript
{
  _id,
  project_id,
  event_id,
  platform: "native",
  level: "fatal",
  occurred_at: received_at,
  received_at,
  mechanism: { type: "minidump" },
  native_crash: {
    kind: "minidump",
    blob_key,
    size,
    checksum
  },
  pipeline: {
    state: "pending",
    attempts: 0,
    next_attempt_at: received_at
  }
}
```

Compatible bounded multipart metadata may supply release, distribution, environment,
tags, user, contexts, and breadcrumbs after the ordinary PII policy. It cannot supply
server-owned project, BlobStore, pipeline, grouping, Issue, or symbolication state.

Processor may replace the initial occurrence-time fallback with a valid crash time
extracted from the report while retaining server `received_at` for retention and
regression semantics.

### Stackwalking through the replaceable backend

Processor detects `native_crash.kind = "minidump"`, reads the BlobStore object without
placing its bytes in the ordinary RAM work queue, and invokes the domain-owned
`SymbolicationService`. The first adapter uses the license-gated Symbolicator backend
from ADR-0013 and its minidump operation.

Only server-approved symbol sources, project scope, platform, and rewrite rules are
sent. A client request cannot inject a symbol URL, source credential, or backend
configuration.

The normalized derived result includes bounded architecture, OS, signal/exception,
crashed-thread identity, modules, raw stack traces, symbolicated stack traces, missing
debug IDs, diagnostics, and completion time. Raw and derived representations remain
separate.

Native grouping uses signal/exception plus module debug IDs and module-relative
addresses under ADR-0014, so later symbol availability does not silently move the
Event to another Issue.

### Backend pending and retries

A backend may return an ephemeral pending request ID and retry delay. Event processing
stores bounded backend request metadata and schedules another attempt. On a later 404
or lost backend state, Processor resubmits the retained minidump instead of treating
the Event as permanently lost.

Temporary failures retry with the ordinary persistent pending state. Missing symbols
or an exhausted backend retry budget produce a terminal partial result where usable
raw crash data exists; an Event cannot remain pending forever.

```toml
[native_crash.processing]
max_concurrent_minidumps = 4
timeout = "20s"
max_attempts = 5
```

These values are configurable. A dedicated semaphore isolates heavy stackwalking from
ordinary Error Event normalization and symbolication concurrency.

### Raw minidump retention

Two explicit modes exist:

```text
event             retain with the parent Event; supports reprocessing
until_processed   delete after terminal extraction; no later reprocessing
```

`event` is recommended when Incident Capsule, symbol upload, and re-symbolication are
required. It follows Event deletion or archival policy. `until_processed` minimizes
sensitive-data and BlobStore retention but permanently gives up raw reprocessing.

Deletion after processing is an idempotent scheduled BlobStore operation; Event state
must record that raw input is unavailable.

### Response behavior

Response semantics are:

```text
200 + plain Event ID   accepted, idempotent duplicate, or intentionally dropped
400                    malformed multipart or invalid/missing minidump
401/403                invalid project credentials
413                    request or minidump limit exceeded
503                    required BlobStore or MongoDB durability unavailable
```

A disabled or category-rate-limited minidump is intentionally dropped with HTTP 200,
an appropriate `X-Sentry-Rate-Limits` capability header, and an ingest outcome. This
avoids native reporter retry storms. Durable acceptance is still recorded only after
BlobStore publication and MongoDB insertion.

## Consequences

- Native crash reporters can create Events without constructing an ordinary Envelope
  Event Item.
- Minidump bytes never enter MongoDB or the normal Processor RAM queue.
- A lost response does not create another Event for a byte-identical dump.
- Stackwalking remains behind the replaceable symbolication boundary.
- Secure default configuration requires explicit acceptance of unsrubbable memory
  content.
- Retaining raw dumps enables later symbols but materially increases sensitive storage.

## Deferred questions

- Packed Unreal Crash Reporter ingestion.
- Apple crash-report and Breakpad text adapters.
- Legacy nested Electron multipart and compressed file containers.
- Exact native reporter event-ID compatibility vectors.
- Minidump-specific metadata fields and supplemental-file allowlist.
- Archive/delete coordination for raw native crash objects.

