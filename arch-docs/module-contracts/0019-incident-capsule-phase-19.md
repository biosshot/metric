# Module contract 0019: Incident Capsule (Phase 19)

- Status: Accepted for implementation
- Owner: `metric-application::incident_capsule`
- Architecture: ADR-0034, ADR-0035, ADR-0038, ADR-0039

## Responsibility

Generate one bounded, versioned, portable investigation archive from an already
authorized Issue and selected retained Events. The module owns selection,
allowlisted export DTOs, omissions, entry integrity, deterministic ZIP layout,
stream cancellation and safe export metrics.

The module does not ingest capsules, persist generated archives, read raw MongoDB
documents, expose BlobStore keys, fetch source, rerun processing or symbolication,
or embed attachment/debug/source bytes.

## Inputs and outputs

Input:

- `AuthContext`, `ProjectId`, `IssueId` and request correlation ID;
- default selection or at most ten explicit Event IDs;
- an optional bounded statistics range;
- typed Issue, Event, hourly-statistics and activity query capabilities;
- clock and request cancellation.

Output:

- `application/vnd.incident-capsule+zip; version=1`;
- a bounded stream of ZIP64 bytes with fixed safe paths;
- stable public errors:
  `invalid_request`, `forbidden`, `not_found`, `limit_exceeded`,
  `cancelled`, `generation_timeout`, `temporarily_unavailable`.

## Archive contract

Physical entry order is deterministic:

```text
issue.json
statistics/hourly.json
activity.json
events/{event_id}.json (ascending Event ID)
diagnostics/capabilities.json
README.txt
manifest.json
```

Optional empty datasets are omitted and recorded in `manifest.json`.
`manifest.json` is physically last because entry size and BLAKE3 metadata must be
known first. Readers locate it by name. Archive paths come only from fixed literals
and validated binary Event IDs.

Every JSON entry uses descriptive version-one fields and string identifiers.
Stored compact keys, retry state, credentials, internal cache keys and backend wire
types are not export DTO fields.

## Resource and cancellation contract

Initial defaults:

```text
max_events = 10
max_activities = 100
max_total_uncompressed_bytes = 100 MiB
max_entry_bytes = 16 MiB
generation_timeout = 30 s
max_concurrency = 4
stream_chunk_bytes = 64 KiB
stream_buffer_chunks = 4
statistics_range = at most 30 days
```

Configuration is fail-closed and cannot exceed the server-owned hard maxima.
Entry and aggregate limits are checked before response headers. The ZIP producer
runs in a blocking task and can hold only the configured bounded channel plus one
entry chunk. Receiver loss stops generation immediately. Request/root cancellation
and deadline terminate preparation or streaming.

## Authorization and audit

The service requires all of `issue:read`, `event:read` and `incident:export`, and
verifies the project organization through the identity boundary before reading the
Issue. Explicit Events must belong to the authenticated project and Issue.

The audit record contains only actor, project ID, Issue ID, selected Event count,
bounded result size class, timestamp and request ID. It never contains payload,
message, tag, filename, checksum or generated archive bytes.

## Metrics and safe diagnostics

Metrics use closed outcome labels only:

- `metric_incident_capsule_exports_total{outcome}`;
- `metric_incident_capsule_selected_events`;
- `metric_incident_capsule_uncompressed_bytes`;
- `metric_incident_capsule_generation_seconds`;
- `metric_incident_capsule_stream_disconnects_total`.

Safe logs may contain request ID, organization/project/Issue identifiers, counts,
size class and stable error code. They must not contain exported content.

## Deferred

- server-side persistence, catalogs, background jobs and share links;
- attachment, minidump, screenshot, debug-file, Artifact Bundle or source bytes;
- signing, encryption, import into production Events and offline viewer;
- MCP runtime/tool exposure.
