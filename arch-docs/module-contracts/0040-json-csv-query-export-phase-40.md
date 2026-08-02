# Phase 40 module contract: bounded JSON/CSV query export

## Scope

Phase 40 is a download mode of Unified Query v2:

```http
POST /api/v1/projects/{project_id}/query
```

`output: { "kind": "download", "format": "json|csv" }` is accepted only with
`result.kind = records`. Ordinary query requests are unchanged. An input cursor is
rejected; the server starts at the normalized query boundary and follows only cursors
issued by the selected source adapter.

## Bounds and isolation

- maximum 10,000 rows per response;
- maximum 16 MiB serialized response;
- maximum 15 seconds of generation;
- maximum two concurrent exports per process;
- each adapter page remains capped by the existing 500-row Query v2 ceiling;
- smaller caller limits are honored and never raise a server ceiling.

The export semaphore is separate from ingest writers. Query v2 parsing,
source-aware validation, authorization, estimation, reservation and physical
adapters remain authoritative.

## Serialization and security

JSON contains the stable scrubbed record DTO array. CSV has a deterministic closed
column list per source; nested stable DTO values use compact JSON cells. CSV text
beginning with a spreadsheet formula prefix is neutralized before RFC-style quoting.
Output is deterministic UTF-8 and carries an attachment filename, `no-store` and an
explicit bounded-truncation response header.

Every accepted export outcome is written through the existing bounded audit store.
The audit uses the existing validator fields and therefore requires no schema
change. Project/source/format/outcome, row count and response size class remain
bounded audit metadata.

## Storage invariant

The implementation adds no endpoint, collection, validator, index, migration,
backfill, export job, worker, Blob object, query cache or materialized result. MongoDB
schema generation remains 19. Cancellation cannot leave a durable partial export
because the bounded response has no durable export object.

