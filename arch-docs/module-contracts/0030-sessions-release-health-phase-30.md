# Phase 30 module contract: Sessions and Release Health

## Ownership

- `metric-domain::sessions` owns compact Session identity, lifecycle precedence,
  deterministic merge rules and bounded health values.
- `metric-sentry-protocol` classifies pinned `session` items and preserves only the
  bounded payload required by the application normalizer.
- `metric-application::session_writer` owns the dedicated bounded queue,
  micro-batching, duplicate coalescing, durable acknowledgements and drain.
- `metric-mongo::sessions` owns the compact `sessions` and rebuildable
  `session_stats_hourly` codecs, bulk upserts, indexes and retention timestamps.
- Server composition wires the Session lane independently from Error, Log and Span
  reservations. Web/API reads Release Health through application ports only.

## Accepted input and identity

Phase 30 initially accepts the pinned individual Sentry `session` item. Aggregate
items remain explicitly unsupported until a pinned fixture supplies a stable
duplicate identity; they are never expanded into synthetic Sessions.

The compact Session `_id` is a deterministic 16-byte digest of Project ID and the
validated SDK session ID. Release and Environment are stored only as their existing
compact IDs, derived by the existing release/environment helpers. Raw release or
environment strings, arbitrary attributes and original JSON are not persisted.

## Lifecycle merge

- `started` is represented by an active `ok` state without `finished_at`.
- Terminal precedence is `crashed > abnormal > exited > ok`.
- `started_at` is the minimum accepted start and `last_update` is the maximum
  accepted update timestamp.
- Sequence is monotonic. A lower sequence cannot replace equal-precedence terminal
  state. A higher-precedence crash outcome may replace a lower-precedence outcome
  even when delivery is out of order.
- Duplicate updates are no-ops. Conflicting Release, Environment or user digests for
  one Session identity are rejected rather than silently reassigned.

## Durability and derived health

`SessionWriter` acknowledges only after the source Session bulk upsert succeeds.
The source is therefore durable at acknowledgement. Hourly health is rebuildable
derived state. The normal path applies only actual source transitions; duplicate
retries and any failed incremental stats write trigger an exact bounded rebuild of
the affected hours. This repairs ambiguous source commits and process failures
between source and derived writes without requiring a cross-collection transaction.

Approximate users use a fixed 128-byte mergeable linear-counting sketch. With 1024
bits its standard error is approximately 3.25% away from saturation. The published
single-bucket saturation estimate is 7,098 users; accuracy degrades near that bound
instead of allocating more memory. No raw user ID or unbounded set is retained.

## Storage and retention

`sessions` uses only MongoDB `_id`, `(p, _id)`, TTL `x`, and the archive-due partial
index when archive mode is enabled. `session_stats_hourly` is keyed by
Project/Release/Environment/hour and has its own TTL.

Terminal Sessions receive absolute TTL in TTL-only mode. Active Sessions never
silently receive ordinary TTL; Scheduler deterministically terminalizes them after
the configured maximum active age. Optional archive mode reuses the existing
project/day, size-bounded archive coordinator and never creates one object per
Session.

Defaults are seven days for detailed Sessions and 400 days for hourly health.

## Explicit exclusions

Phase 30 does not change Error, Log or Span writers, add speculative Session fields
or indexes to other signals, infer proximity links, store raw Session payloads, or
claim aggregate-item compatibility without deterministic fixtures.
