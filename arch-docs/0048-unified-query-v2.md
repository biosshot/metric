# ADR-0048: Unified Query v2 over existing storage

- Status: Accepted
- Date: 2026-08-01
- Implementation: Complete (2026-08-02)
- Amends: ADR-0009, ADR-0023, ADR-0024, ADR-0040, ADR-0045 and ADR-0047
- Storage effect: none; schema generation remains 19

## Context

Metric already has two substantial bounded query implementations:

- Boolean Event search used by the Issues investigation surface;
- Unified Explore over Errors, Logs, Spans and Metrics with typed predicates,
  aggregates, timeseries, deterministic cost, query reservations and cursors.

Later vertical product phases also added separate list/filter HTTP endpoints for
Issues, Logs, Transactions, Replays, Feedback and Releases. Their physical stores
remain intentionally separate, but exposing unrelated query controls and transports
has fragmented the Web investigation workflow. `/explore` also remains a valid SPA
route while no longer appearing in primary navigation.

The product requires one public query language and one public query endpoint without
adding a search service, another database, a shared physical signal collection or a
retained-data migration.

## Decision

### Extend the existing engine

Unified Query v2 is an extension and consolidation of the current Search and Explore
implementations. It is not a third engine.

The current Boolean expression parser becomes the common filter language. Existing
Explore planning supplies typed result shapes, aggregates, timeseries, deterministic
cost estimation, concurrency isolation and MongoDB adapter patterns:

```text
existing Boolean Search parser and expression AST
+ existing Explore planner, estimator, reservation and result shapes
-> Unified Query v2
```

Parser, source-aware validation, planning and physical compilation remain separate.
User input never becomes a MongoDB field path, collection name or raw BSON operator.

### One public query endpoint

All Web list, search and aggregate reads use:

```http
POST /api/v1/projects/{project_id}/query
```

The authorized path supplies project scope. A request selects exactly one logical
source:

```rust
pub enum QuerySource {
    Issues,
    Errors,
    Logs,
    Traces,
    Metrics,
    Replays,
    Feedback,
    Releases,
}
```

The transport contains a query string, optional accepted time range, result
specification, cursor and row limit. Result specifications initially cover:

```text
records
number
timeseries
values       // bounded autocomplete values
```

`records` returns the source-specific stable DTO rather than one union document with
many absent fields. Aggregate shapes remain available only for source/field pairs
already accepted by the typed planner.

Phase 40 extends this same endpoint with an optional bounded `output` selection
for JSON/CSV downloads. Absence of `output` preserves the ordinary JSON query
response. Download output is permitted only with `records`, reuses the same parsed
and authorized request, and does not introduce `/query/export`, another query DTO or
another query language. ADR-0047 owns the export limits and serialization contract.

One endpoint does not mean one cross-collection MongoDB operation. Enum dispatch
selects one source adapter and one existing physical collection/query path. Normal
requests do not use `$unionWith`, joins or cross-source ranking. A later global Web
preview may issue several small requests to the same endpoint and render grouped
source sections; it must not invent a heterogeneous merged cursor.

### Sources and existing physical data

| Query source | Existing physical/query source | Default free-text target |
| --- | --- | --- |
| `issues` | `issues` | Issue title |
| `errors` | `error_events` | no arbitrary body text; accepted Event fields only |
| `logs` | `logs` | normalized message `m` |
| `traces` | root/transaction records in `spans` | name/operation where accepted |
| `metrics` | `metric_buckets` | metric name |
| `replays` | `replays` metadata | Replay ID/URL metadata where accepted |
| `feedback` | `feedback` | bounded feedback message where accepted |
| `releases` | `releases` | exact/version title data |

Each adapter advertises a closed field and operation matrix based on current compact
projections and current bounded query paths. A field without an efficient current
plan is rejected with a stable capability error. Unified syntax does not authorize
arbitrary residual-body JSON search, recording-content search or unindexed custom
tags.

### Query language and aliases

Whitespace is implicit `AND`. The language retains explicit `AND`, `OR`, `!`, quoted
values, parentheses and typed comparisons. A bare phrase targets the source's
declared default text field.

Web accepts long canonical field names and short aliases. The parser resolves aliases
before validation and normalization:

| Alias | Canonical field |
| --- | --- |
| `env` | `environment` |
| `rel` | `release` |
| `svc` | `service` |
| `msg` | `message` |
| `trace` | `trace_id` |
| `span` | `span_id` |
| `dur` | `duration_ms` |
| `op` | `operation` |
| `issue` | `issue_id` |
| `replay` | `replay_id` |
| `user` | `user.id` |
| `metric` | `metric_name` |
| `kind` | `metric_kind` |

Examples:

```text
level:error svc:payments env:production
msg:"connection refused" rel:"backend@1.4.2"
dur:>500 op:http.server
level:error AND (svc:api OR svc:worker)
!env:development
```

Canonical normalization is stable and uses typed canonical fields, not the spelling
chosen by the user.

### High structural ceilings and operational bounds

Hard parser limits exist only as abuse and accidental-complexity guards. They are
deliberately high enough not to constrain ordinary investigation:

```text
maximum query bytes       32 KiB
maximum AST nodes         256
maximum predicates        128
maximum OR alternatives   64
maximum nesting depth     16
default record rows       50
maximum record rows       500
maximum autocomplete rows 20
```

The compiler must not expand the whole expression into an exponentially growing DNF.
It compiles Boolean structure recursively where the source supports it. A source that
requires branch execution accounts for every bounded branch explicitly in cost.

Operational protection remains more important than the structural ceilings:

- deterministic source-aware cost estimation;
- project and source scope before storage work;
- explicit or safe default time ranges for high-volume sources;
- bounded candidate work for text/post-verification paths;
- query-only concurrency reservation;
- validated MongoDB `maxTimeMS`;
- deterministic cursors bound to project, source, normalized query and result shape.

Unsupported or over-budget semantics fail explicitly. They never silently drop a
predicate or fall back to an unbounded collection scan.

### Web query surface and autocomplete

One reusable `UnifiedQueryBar` owns the query text, source-aware field schema,
parse/validation feedback, URL synchronization and compact condition chips. It is
used by Issues, Logs, Metrics Query, Traces, Replays, Feedback, Releases and Explore.

The first delivery includes autocomplete rather than adding it later:

- source-specific field aliases and canonical names;
- operators valid for the selected field;
- static enum values such as level, status and metric kind;
- known environments and releases;
- bounded recent/known services and metric names where the existing query plan can
  provide them safely;
- keyboard navigation, debouncing, cancellation and a maximum of 20 values.

Dynamic values use the same `/query` endpoint with the `values` result kind. No
autocomplete collection, catalog worker or extra public search endpoint is created.
The Web keeps the query in a shareable route query parameter such as `?q=...`.

`/explore` returns to primary `Observe` navigation. It remains the source-selectable
workspace for tables, numbers, timeseries and grouping. Dedicated product pages use
the same engine with their source fixed and retain their specialized row/detail UI.

### Replace old public list/search transports

The released implementation contains the new endpoint only for query/list work. Once
all in-tree Web callers, Saved Queries, Dashboards and Alerts use Unified Query v2,
the replaced HTTP methods and their unused Web client DTOs/handlers are deleted in
the same release:

- `GET /projects/{project_id}/issues`;
- `GET /projects/{project_id}/events` and `/events/search`;
- `GET /projects/{project_id}/logs`;
- `GET /projects/{project_id}/transactions`;
- `GET /projects/{project_id}/replays`;
- `GET /projects/{project_id}/feedback`;
- `GET /projects/{project_id}/releases`;
- `POST /projects/{project_id}/explore`.

Method-specific creation, detail, mutation, attachment, replay-segment, Trace
composition, Release Health and Issue evidence endpoints remain specialized. For
example `POST /releases` continues to create a Release while `GET /releases` is
replaced by Query v2.

Internal typed store/service methods may remain as source-adapter implementation
details. The decision removes duplicate public query transports, not useful domain
ports.

Metric ships Backend and the bundled Web from one release artifact, so no released
compatibility bridge between old and new query endpoints is retained. An already-open
browser tab can still hold the previous JavaScript across a container update; build
version mismatch handling may require a reload and must not be misreported as data
loss.

### Saved Query compatibility without migration

Existing Saved Queries retain their documents. Their nested `query` property is
already validated only as an object, so Query v2 uses a versioned nested encoding:

```text
v1 predicates array -> decode as And(predicates)
v2 expression tree  -> decode directly
new writes           -> encode as v2
```

Old documents are read in place. There is no eager rewrite, background conversion or
backfill. Dashboards and Alerts revalidate the decoded current expression through the
same planner before execution.

## Storage and migration invariant

Unified Query v2 must ship with all of the following unchanged:

- MongoDB schema generation **19**;
- `schema_meta`;
- collection set and names;
- collection validators;
- index definitions;
- compact signal BSON codecs;
- retained documents and Blob objects;
- ingest, writer, Processor, retention and archive behavior.

It adds no collection, field projection, index, token array, query cache, materialized
relation, migration command, database rewrite or retained-data backfill. If a desired
predicate later requires any of those, it needs a separate schema decision and is not
silently included in this ADR.

## Implementation boundary

Implementation is one atomic product change even if developed through internal
commits:

1. promote/generalize the current Boolean parser and expression AST;
2. add source schemas, aliases and common Query request/result contracts;
3. extend the existing Explore planner, estimator and adapters to accept the common
   expression;
4. add record-only adapters for Issues, Replays, Feedback and Releases using current
   stores and indexes;
5. version Saved Query decoding/encoding in place;
6. add `/query` and migrate all Backend-owned consumers;
7. add the shared Web query bar, autocomplete and restored Explore navigation;
8. migrate every listed Web page;
9. delete replaced HTTP handlers, client methods and dead DTOs;
10. run cumulative parser, authorization, cost, cursor, MongoDB, Web, Dashboard,
    Alert and E2E gates.

There is no intermediate public release with two supported query APIs.

## Completion evidence

The implementation reuses the generalized Boolean parser, source-aware Query v2
adapters and one project-scoped `/query` endpoint for Issues, Errors, Logs, Traces,
Metrics, Replays, Feedback and Releases. The bundled Web pages, Saved Queries,
Dashboards and Alerts use that contract; replaced public list/search transports were
removed. Parser, authorization, cursor, source-adapter, Web client and route coverage
verify the replacement. MongoDB schema generation remains 19 and no collection,
validator, index, migration or retained-data rewrite was introduced.

## Consequences

- Users learn one compact search language across the product.
- Specialized physical collections and query plans remain efficient and isolated.
- Explore, dedicated investigation pages, Saved Queries, Dashboards and Alerts share
  one filter semantics.
- Adding a source means adding a typed adapter, not a new public filter endpoint.
- Schema generation 19 data remains directly usable without migration.
- Query features remain limited to what current storage can execute predictably.
