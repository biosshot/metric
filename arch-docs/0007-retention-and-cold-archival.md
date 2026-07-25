# ADR-0007: Event retention and optional cold archival

- Status: Accepted
- Date: 2026-07-20

## Context

Raw events can grow much faster than issue metadata and aggregate statistics. The
first version needs predictable storage use for small installations while preserving
an optional path to cheap long-term storage. Archival must never be required for a
working installation and an archive failure must not silently delete unarchived
events.

MongoDB TTL deletion is asynchronous and is not an exact-time guarantee. Pending
events are also the durable Processor backlog and therefore must not become eligible
for retention deletion before reaching a terminal processing state.

## Decision

### Retention classes and defaults

The initial global defaults are:

```toml
[retention]
events_days = 30
issue_stats_hourly_days = 400
```

Both values are configurable globally and may be overridden per project. Project
configuration is resolved into absolute retention timestamps so MongoDB TTL does not
need to evaluate project settings.

The initial retention classes are:

- raw `error_events`: 30 days by default;
- `issue_stats_hourly`: 400 days by default;
- `issues`: no automatic retention deletion;
- `releases` and `environments`: no automatic retention deletion;
- `archive_manifests`: retained while their archive objects exist;
- event-owned attachments and minidumps: follow the parent event's retention or
  archival outcome;
- debug symbols and source maps: use ADR-0032; they have no automatic expiration by
  default and may use explicitly configured age and quota policies.

Deleting a project remains an explicit scoped purge and is not performed by ordinary
retention.

### Retention clock

Raw-event retention is calculated from server-controlled `received_at`, not from the
SDK-controlled `occurred_at`. Client clocks may be incorrect and cannot determine
when durable server storage expires.

ADR-0022 gives Event retention a compact state-dependent physical shape:

```javascript
{
  r, // received_at
  h, // archive due time while awaiting archive, optional
  z, // completed archive segment, optional
  x  // expire_at, optional
}
```

`x` is indexed by a single-field TTL index with `expireAfterSeconds: 0`.
MongoDB may delete a document after that absolute date, but deletion need not happen
immediately.

### Pending and terminal events

An event with pending `q.s == 0` does not receive `x`. Retention therefore cannot
delete the durable Processor backlog.

When archival is disabled, a terminal processed or permanently failed event gets `x`
derived from `r` and the effective event-retention setting.
If processing finishes after that deadline, the event may become immediately
eligible for deletion.

When archival is enabled, reaching `h` makes a terminal event eligible for archival
but does not set `x`. After its archive segment is durably completed and verified,
the Event receives segment `z` and TTL date `x`, and `h` is removed.

### Initial MongoDB layout

The current schema keeps raw Error Events in one regular `error_events` collection
and uses the `x` TTL index. It does not use MongoDB time-series collections because
Error Events
are mutable during processing and require regular identity, update, and indexing
behavior.

Physical time-partitioned event collections are not implemented initially. Internal
storage operations must avoid assuming that the collection can never be partitioned,
so production-shaped benchmarks can later justify an `EventPartition` routing layer.
Such a change must preserve project-scoped retention and pending-event recovery.

Large retention reductions for existing data are applied gradually by Scheduler
rather than making an unbounded set of documents expire at once.

### Optional cold archive

Cold event archival is disabled by default. If no archive backend is configured,
terminal events are permanently removed after their configured hot retention.

When enabled, the archive backend is either the local filesystem or S3-compatible
object storage. Events are written as versioned Apache Parquet segments with Zstandard
compression. Segments are project-scoped, cover no more than one day, and are also
bounded by a configurable target object size.

A segment contains stable searchable metadata columns and the decoded canonical,
scrubbed Event body. It never contains a pre-scrubbing durable copy.

The archive key layout is logically project and date scoped, for example:

```text
projects/{project_id}/events/{year}/{month}/{day}/{segment_id}.parquet
```

### Archive commit protocol

`archive_manifests` records at least:

```javascript
{
  _id,
  project_id,
  received_from,
  received_to,
  object_key,
  format,
  compression,
  schema_version,
  event_count,
  stored_bytes,
  checksum,
  state,
  created_at,
  completed_at
}
```

The archive state moves from `writing` to `complete`. The archiver uses deterministic
segment and object identities so a retry after a crash does not create a second
logical segment.

The required order is:

1. create or resume a `writing` manifest;
2. write the temporary file or object;
3. finalize the file or object;
4. verify its size and checksum;
5. mark the manifest `complete`;
6. associate source events with the completed segment as `z`;
7. set the source events' `x` values and remove `h`.

If archival is unavailable or verification fails, source events remain in MongoDB
without `x`. This favors durability over bounded disk use and must produce an
operational alert.

### Archive access

The first archive implementation is cold storage. Normal Web and MCP searches query
MongoDB and aggregate collections, not Parquet objects. Issue metadata and hourly
statistics remain available after raw events leave MongoDB.

Archive export and explicit range restoration may be added without promising
transparent online queries. Federated MongoDB-and-Parquet search, pagination, and
query execution are deferred.

### Hourly statistics

`issue_stats_hourly` uses its own configurable retention and absolute TTL date. Its
documents remain a rebuildable approximate projection as defined by ADR-0004 and
ADR-0005. The first version does not add daily rollups. A future
`issue_stats_daily` projection may be introduced if history beyond the configured
hourly retention is required.

## Consequences

- Small installations work with MongoDB and local blob storage only; S3 and archival
  are optional.
- Raw-event storage has a predictable configurable horizon.
- Pending backlog cannot disappear because of ordinary TTL retention.
- Archive failure can consume additional MongoDB disk space but cannot silently lose
  events awaiting archive completion.
- Issue identity and workflow state survive raw-event deletion.
- Hourly charts survive longer than raw stack traces under the default settings.
- The simple initial TTL model may require partitioning or sharding at very high
  sustained volumes, based on benchmarks and observed TTL lag.
- Archived data is not immediately searchable through the normal event API.

## Deferred questions

- Exact project-override configuration and validation limits.
- Attachment archival object layout and cleanup protocol.
- Archive segment target size and retry limits.
- Archive restoration workflow.
- Conditions for adding physical event partitions or daily statistics rollups.
