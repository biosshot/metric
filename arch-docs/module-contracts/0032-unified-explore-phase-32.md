# Phase 32 module contract: Unified Explore

## Ownership

- `metric-domain::explore` owns the closed dataset/field/predicate/aggregate AST,
  stable v1 normalization, typed result values and raw cursor shape.
- `metric-application::explore` owns scope injection, validation, deterministic cost
  estimation and the Explore-only concurrency reservation.
- `metric-ports::ExploreStore` accepts only a validated `ExplorePlan`. It exposes no
  collection name, raw filter, projection, sort or aggregation expression.
- `metric-mongo::explore` owns the three dataset adapters and translates the closed
  plan into operations on existing `error_events`, `logs` or `spans`.
- Native API owns JSON parsing, authorization and cursor encoding. Vue owns a typed
  query builder and table, number and timeseries rendering.

## Query boundary

Every request has exactly one dataset and receives project scope from the authorized
URL path:

```text
authenticated path ProjectId
-> typed body parse (no scope/raw expression fields)
-> inject ProjectId
-> validate normalized AST and deterministic cost
-> reserve one Explore query permit
-> one dataset adapter
-> one existing physical collection
```

The initial query language supports exact/presence/range predicates, raw cursor
pages, `count/sum/min/max/avg`, accepted percentiles, up to two declared
finite-cardinality groups (`level`, `platform`, `operation_class`, `is_segment`) and
`1m/5m/1h/1d` intervals. Range, predicate, aggregate, bucket, row and exact maximum
group fan-out are checked before storage.

## Isolation and correctness

Explore has a dedicated non-waiting semaphore. It does not use or modify ingest
queues, batch writers or processing reservations. Reads operate on acknowledged
source documents; concurrent ingest may be visible before or after its atomic
MongoDB insertion, while a later query observes the acknowledged write. TTL deletion
has the same document-level visibility. Every plan carries a validated storage
timeout, which the Mongo adapter applies as `maxTimeMS`.

No result cache or background job is created. A rejected query therefore has no
partial work to clean up.

## Storage decision

Phase 32 adds no collection, validator, index, raw payload or derived result. Schema
generation remains 13. Adding a future dataset requires a new field map/adapter and
does not change existing Error, Log or Span BSON codecs.

## Explicit exclusions

Phase 32 does not add joins, cross-project or cross-organization queries, arbitrary
tags, regex, scripts, raw MongoDB syntax, saved queries, dashboards, alerts, MCP,
NATS, migrations, sharding or disk spool.
