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
| 40 | Bounded JSON/CSV query export | Accepted, next focus |
| 41 | English/Russian Web localization | Complete |
| 42 | Bounded cold-archive search | Accepted after/reusing Phase 40 contracts |

Only Phase 40 and Phase 42 remain active product work after the completed
localization. Profiling, SQLite, MCP/AI, distributed roles and the other broad Sentry
parity backlog remain unnumbered and unselected.

## Phase 40: bounded JSON/CSV query export

Phase 40 exports the result of an already authorized, validated and bounded current
query. It does not expose MongoDB documents and does not create a second query
language.

Accepted first scope:

- JSON and CSV formats;
- Error, Log, Span and Application Metric datasets already supported by the current
  investigation/Explore contracts;
- project, dataset, time range and current filters are mandatory inputs;
- streaming generation with explicit row, byte, duration and concurrency limits;
- stable DTO fields rather than compact storage names;
- CSV formula-injection protection and deterministic UTF-8 output;
- the existing scrubbed/authorized representation only;
- an audit record for each export;
- cancellation stops generation without leaving a partial durable export object.

The first implementation is a bounded response and creates no export-job collection,
background worker or Blob object. Incident Capsule remains the richer Issue-specific
evidence bundle and is not replaced by tabular export.

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
