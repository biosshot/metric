# Phase 37 module contract: Application Metrics

## Scope

Phase 37 accepts only the pinned Sentry SDK `trace_metric` Envelope item with
`application/vnd.sentry.items.trace-metric+json`. Legacy StatsD and Sentry
`metric_buckets` items remain disabled. Profiling and Session Replay are outside this
contract.

## Data flow and ownership

```text
Envelope parser
  -> RawMetricContainer { item_count, borrowed payload boundary }
  -> request-local streaming JSON visitor
  -> MetricDeltaBatch keyed by (series_id, bucket_start)
  -> dedicated MetricSink / MetricWriter bounded channel
  -> cross-request delta merge
  -> MetricStore
  -> metric_buckets atomic upsert
  -> durable HTTP acknowledgement
```

The visitor materializes at most one JSON measurement at a time. It never retains a
normalized measurement array, copies the complete raw container into a queue, or
stores the container. `item_count` must match the number of visited rows. A malformed
container rejects the item; malformed individual rows are counted and discarded
without hiding valid siblings.

## Series and bucket identity

A series is `project + name + kind + unit + normalized tags`. The 16-byte series ID
and 16-byte bucket ID are deterministic BLAKE3 projections. The first version uses
60-second buckets:

- counter: sum and measurement count;
- gauge: last, minimum, maximum, sum and count;
- distribution: minimum, maximum, sum, count and a fixed 64-bin logarithmic sketch.

`trace_id` is optional correlation metadata and is not part of series identity.
Sequence/internal/user attributes are excluded. Attribute keys containing `.` or `$`
are normalized before MongoDB; a normalization collision discards the row.

## Bounds and isolation

- one dedicated bounded queue; no Error, Log or Span permits are consumed;
- 2,000 maximum compact deltas in one writer flush by default;
- names, units, tag count, tag keys and tag values are bounded before admission;
- per-project series cardinality is checked against existing series before any new
  bucket upsert;
- the one-process `--role all` writer serializes cardinality admission and hot-series
  updates;
- schema generation 18 is an intentional breaking empty-database schema with no
  migration from generation 17.

## Durability and retry semantics

The ingest response waits for `MetricStore::persist_metrics`. MongoDB uses atomic
`$inc`, `$min`, `$max` and `$set` upserts. The writer does not retry an ambiguous
storage operation internally.

Application Metrics are explicitly **at least once**, not idempotent: if the client
does not receive the durable response and retries the same SDK container, counter,
gauge sum/count and distribution aggregates may be applied again. This is the only
accepted overcount boundary. A server-side retry after an ambiguous write would make
that boundary invisible and is forbidden.

## Retention, archive and deletion

`retention.metrics_days` controls hot retention. With metric archive disabled,
MongoDB TTL removes `metric_buckets` through the `metric_retention` index. With global
archive enabled and `retention.metric_archive = true`, new buckets receive an archive
due timestamp instead; the existing archive service writes
`projects/{project}/archives/metrics` Parquet/Zstd segments and only then marks the
hot source for expiry.

Project deletion owns both `metric_buckets` and the metric archive namespace.

## Query/product reuse

`ExploreDataset::Metrics` reuses the existing Explore planner, Mongo adapter, Saved
Queries, Dashboards and aggregate Alert evaluator. Supported fixed fields are metric
name, kind, unit, trace ID, count, sum, minimum and maximum. Web project policy and
Explore expose the same dataset; no raw MongoDB syntax or arbitrary tag query is
added.
