# ADR-0038: Incident Capsule format and bounded export

- Status: Accepted
- Date: 2026-07-21

## Context

An Issue investigation often requires more than one Event JSON response: the first
and latest occurrences, raw and symbolicated frames, grouping explanation, release
context, activity, breadcrumbs, tags, and aggregate history belong together. A
portable Incident Capsule can support offline debugging, bug-report attachment, and a
future MCP/agent workflow without granting database access.

Capsules can also concentrate sensitive data and become large if they embed
attachments, source bundles, or debug files. The first format must be bounded,
streamable, versioned, and generated only from already authorized scrubbed data.

## Decision

### Export, not a new ingest/storage model

An Incident Capsule is generated on demand from one authorized Issue and selected
retained Events. It is not an SDK Envelope item, an Event replacement, or a new
persistent MongoDB collection.

Version one streams the capsule to the requesting client and does not retain a copy
in BlobStore. Client disconnect or deadline cancels generation. Durable share links,
background jobs, expiry, and capsule catalogs can be added later without changing the
versioned archive contents.

### Container and media type

The version-one artifact is a ZIP64 archive with fixed safe UTF-8 paths and extension
`.incident.zip`. Its media type is:

```text
application/vnd.incident-capsule+zip; version=1
```

ZIP is chosen for broad offline/tooling support. Entries use deterministic ordering,
normalized timestamps, and either stored or deflate compression selected by content
type. User filenames never become archive paths.

The archive layout is:

```text
manifest.json
issue.json
statistics/hourly.json
activity.json
events/{event_id}.json
diagnostics/capabilities.json
README.txt
```

Absent optional data omits its entry and is explained by the manifest. No empty/null
placeholder files are required.

### Manifest and integrity

`manifest.json` contains:

```javascript
{
  format: "incident-capsule",
  version: 1,
  generated_at,
  organization_id,
  project_id,
  issue_id,
  selection,
  entries: [
    { path, media_type, uncompressed_size, blake3 }
  ],
  omissions: [
    { code, safe_detail }
  ]
}
```

JSON entries use the accepted canonical JSON encoder and stable descriptive field
names. Entry BLAKE3 and size are computed while streaming entry content. The ZIP
writer emits the final manifest after entry metadata is known and records its path in
the central directory; readers must locate by name rather than assume physical first
position.

Identifiers are encoded losslessly as strings. Archive paths are generated only from
validated binary IDs rendered by the server. Checksums provide corruption detection,
not authenticity or authorization after a user redistributes the downloaded file.
Signing/encryption is deferred.

### Issue and Event contents

`issue.json` includes bounded Issue identity, title, project, lifecycle status,
assignment if authorized, first/last occurrence, representative Event IDs, release
pairs, regression/grouping revision and the human-readable grouping explanation.

Each Event entry is the same versioned investigation DTO used by the native API and
can include:

- received/occurred timestamps, platform, level and logger;
- message and validated fingerprint;
- exception chain and both raw and stored symbolicated frames;
- module/debug identifiers and symbolication diagnostics;
- release, dist and environment;
- scrubbed user/request/context, tags and breadcrumbs;
- server processing diagnostics useful for investigation;
- attachment metadata without attachment bytes.

It does not contain compact BSON keys, compressed body bytes, Mongo retry state,
internal BlobStore keys, service credentials, cache keys, or raw backend protocol.

Capsule generation reads the stored Event result and does not trigger reprocessing,
new symbolication, grouping changes, Issue mutations, or network scraping. A missing
or retention-deleted selected Event becomes a stable omission rather than an
unbounded replacement search.

### Default Event selection

The default bounded selection is:

```text
first retained Event
latest retained Event
current representative Event if distinct
up to seven most recent additional Events
```

IDs are deduplicated and ordered deterministically. The caller may request a bounded
explicit Event ID list, but every Event must belong to the authorized Issue and
project. Version one never means "all Events in the Issue".

Initial limits are:

```toml
[incident_capsule]
max_events = 10
max_activities = 100
max_total_uncompressed_bytes = "100MiB"
max_entry_bytes = "16MiB"
generation_timeout = "30s"
max_concurrency = 4
```

Limits are configurable within safe server maxima. Statistics are restricted to a
bounded requested time range and the retained hourly buckets.

### Source, symbols, and attachments

Version one includes symbolication results and debug/source identifiers but never
embeds complete debug files, Artifact Bundles, source maps, source archives, or
Symbolicator cache objects. Those can be gigabytes, contain proprietary code, and
have separate authorization/retention.

Attachment metadata can explain what was captured, but attachment bytes, minidumps,
screenshots, view hierarchies, and arbitrary uploaded files are excluded from the
initial format. A later explicit opt-in attachment export requires its own permission,
per-item selection, content-type policy, and tighter byte limits.

Small source-context snippets already stored inside the scrubbed Event DTO may be
included as Event data. Capsule export never fetches new source from the network.

### Authorization and audit

Capsule generation requires both `issue:read`/`event:read` and a stable
`incident:export` permission. The effective organization/project scope is derived
from the authenticated Issue; caller-supplied organization or project values cannot
redirect selection.

The export command is audited with actor, project, Issue, selected Event count,
result size class, timestamp and request ID. Audit never stores the capsule, Event
payload, messages, filenames, tags, user fields, or checksums that could become a
content fingerprint.

Only canonical data already processed by pre-storage PII policy is eligible. Export
applies a final DTO allowlist so internal or newly added storage fields cannot appear
automatically. Authorization is checked before headers begin, and loss of membership
during an already authorized bounded stream follows the same request-snapshot
semantics as an Event download.

### Native API and future MCP

The native API exposes a bounded command such as:

```text
POST /api/v1/projects/{project_id}/issues/{issue_id}/capsule
```

The request chooses only accepted selection/time options. The response streams the
archive with safe content disposition and no server filesystem path.

A future MCP tool calls `IncidentCapsuleService` with the same `AuthContext` and may
return a download handle or bounded decoded summary. MCP does not receive raw MongoDB,
BlobStore, Symbolicator, or archive-reader access.

### Reader and compatibility tests

The repository contains a small independent capsule reader/validator in `testkit` or
a standalone test utility. Golden archives verify path safety, canonical DTOs,
checksums, omission behavior, deterministic entry order, unknown optional manifest
fields, truncation and corruption.

Writers never emit a changed meaning under version 1. Readers ignore unknown safe
manifest fields and reject unsupported major versions, duplicate paths, traversal,
oversized entries, invalid checksums and archive bombs. Import into production Events
is not supported.

## Consequences

- One bounded portable file contains the useful state of an Issue investigation.
- Capsules reuse stable API/domain DTOs instead of exporting database documents.
- Default exports do not duplicate gigabyte symbol/source files or arbitrary
  attachments.
- Streaming avoids another persistent job/collection and respects cancellation.
- Future MCP integration has a safe high-level artifact without privileged storage
  access.

## Deferred questions

- Durable expiring share links and background generation for larger policies.
- Explicit attachment/source-context extensions with independent permissions.
- Optional recipient encryption and publisher signatures.
- A richer standalone offline viewer; production Event import remains out of scope.
