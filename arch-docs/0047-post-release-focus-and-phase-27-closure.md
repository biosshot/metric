# ADR-0047: Post-release focus and Phase 27 closure

- Status: Accepted
- Date: 2026-08-01
- Amends: ADR-0039, ADR-0040, ADR-0044, ADR-0045 and the current roadmap index

## Owner decisions

The first public release exists and the broad Phase 27 production-readiness program
no longer represents the owner's desired release process. Phase 27 is therefore
**closed as obsolete without executing or claiming its historical gates**. It is not
deferred, does not resume later and does not block releases or product phases.

This closure removes the following items as roadmap requirements:

- a built-in `metric backup` / `metric restore` workflow;
- a dedicated `--check-upgrade` command, because Git releases and their update notes
  own version compatibility;
- a Prometheus endpoint;
- the combined soak, canary, restore-drill and production-declaration program defined
  by ADR-0044.

These decisions do not change current runtime facts. Generation 19 is still required
exactly, Metric still has no automatic migration, and operators still retain MongoDB
and BlobStore with external tooling. Future schema evolution may use explicit native
migration or version-aware readers/codecs that understand older schemas. Neither
approach is claimed until implemented and tested.

Ordinary correctness, security, durability, load and regression tests remain part of
each module's exit gate. Closing Phase 27 removes a separate monolithic program; it
does not weaken an already accepted module contract.

## Current phase status

| Phase | Capability | Status |
| ---: | --- | --- |
| 27 | Historical production-readiness program | Closed as obsolete |
| 39 | Investigation UX expansion | Cancelled as a phase; ideas returned to backlog |
| 40 | Bounded JSON/CSV query export | Complete |
| 41 | English/Russian Web localization | Complete |
| 42 | Bounded cold-archive search | Accepted after/reusing Phase 40 contracts |

Only Phase 42 remains active numbered product work after the completed localization
and query export. Profiling, SQLite, MCP/AI, distributed roles and the other broad
Sentry parity backlog remain unnumbered and unselected.

## Phase 40: bounded JSON/CSV query export

ADR-0048 supersedes the earlier Explore-specific transport assumption for this
phase. Phase 40 is now a download mode of the existing Unified Query v2 endpoint:

```http
POST /api/v1/projects/{project_id}/query
```

The ordinary request remains unchanged when no download output is selected. A
download request uses the same `source`, query text, time range and `records` result
specification, and adds an explicit bounded output selection such as:

```json
{
  "source": "logs",
  "query": "level:error env:production",
  "from": 1785542400000,
  "until": 1785628800000,
  "result": { "kind": "records" },
  "output": { "kind": "download", "format": "csv" }
}
```

There is no `/query/export` endpoint and no second export query DTO. Download mode
passes through the same source-aware parser, validation, authorization, estimator,
reservation and physical adapter as the ordinary `/query` request. Export code owns
only bounded cursor iteration and stable JSON/CSV serialization of the resulting
record DTOs. It never accepts a MongoDB collection, field path, BSON predicate or
already-materialized client-supplied row set.

Accepted first scope:

- JSON and CSV formats;
- `records` output for every current Unified Query v2 source: Issues, Errors, Logs,
  Traces, Metrics, Replays, Feedback and Releases;
- project, source, query and the source-appropriate bounded time range are the same
  inputs used by ordinary Unified Query v2;
- `number`, `timeseries` and `values` remain inline query results and are not download
  formats in the first implementation;
- an input cursor is rejected in download mode; the server starts from the normalized
  query boundary and follows its own signed cursors only until an export limit is
  reached;
- streaming generation with explicit row, byte, duration and concurrency limits;
- the server hard limits always cap any smaller caller-requested export limit;
- stable DTO fields rather than compact storage names;
- source-specific deterministic CSV columns rather than one sparse union schema;
- CSV formula-injection protection and deterministic UTF-8 output;
- the existing scrubbed/authorized representation only;
- an audit record in the existing audit storage for each export attempt and outcome;
- cancellation stops generation without leaving a partial durable export object.

The first implementation is a bounded response and creates no export-job collection,
background worker, Blob object, query cache or materialized result. It adds no MongoDB
collection, validator, index, migration or backfill. Incident Capsule remains the
richer Issue-specific evidence bundle and is not replaced by tabular export.

## Phase 42: bounded cold-archive search

Phase 42 makes retained archive data discoverable without turning object storage into
an unbounded analytics engine.

Accepted first scope:

1. list authorized archive manifests by project, dataset and time range;
2. require an explicit bounded time range for every archive query;
3. support exact identifiers and the bounded promoted fields that physically exist
   in the archived format, initially environment and release where available;
4. enforce maximum objects, compressed bytes, decoded rows, wall time and concurrent
   scans before work starts;
5. return read-only results through the Phase 40 export DTOs and JSON/CSV generator;
6. report partial/limit outcomes explicitly rather than silently truncating;
7. preserve checksum verification, tenant authorization and PII rules.

Not included initially:

- full-text search across every archived payload;
- arbitrary tags or arbitrary JSON paths;
- transparent merge/pagination across hot MongoDB data and cold objects;
- automatic restore or rehydration into MongoDB;
- background indexing service or a second search database.

The Web surface uses an explicit `Hot` / `Archive` source choice. This keeps ordering,
deduplication and cost visible. A cold result may be inspected or exported without
being restored to the hot database.

## Consequences

- The roadmap is intentionally narrow after the first release.
- Historical ADR-0044 remains useful design material but is not a live gate.
- Localization is complete out of numeric order; phase numbers express accepted
  product identity, not mandatory sequential execution.
- Export becomes the reusable presentation boundary for cold search.
- Any future schema compatibility work receives a new focused decision instead of
  reopening Phase 27.

## Later amendment: Unified Query v2

ADR-0048 accepts an unnumbered cross-cutting replacement of the fragmented hot-data
list/search transports. It extends the existing Search/Explore implementation and
does not reopen Phase 27 or change the accepted Phase 40/42 storage scope. It keeps
schema generation 19, all collections, validators, indexes and retained data
unchanged and requires no migration or backfill.
