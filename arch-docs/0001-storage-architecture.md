# ADR-0001: Storage architecture and the first database backend

- Status: Accepted
- Date: 2026-07-20

## Context

Faultkeep must keep storage-specific code out of ingestion, grouping, symbolication,
web, and scheduling logic. At the same time, the first implementation should be
optimized for one database instead of trying to support several engines before the
real query patterns and bottlenecks are known.

The initial database backend is MongoDB. Its document model fits normalized Sentry
events, and it provides a path from a small unsharded deployment to a replica set and,
if it is needed later, native sharding.

Large binary objects and optional cold archives have different storage requirements
from operational metadata and events. They are therefore not stored as MongoDB BSON
documents.

## Decision

### Logical storage boundaries

Storage is split into the following logical responsibilities:

- `ControlStore`: organizations, users, memberships, sessions, API tokens, projects,
  project keys, settings, releases, issue state, audit records, and other control-plane
  data. Authentication and authorization semantics are defined by ADR-0021.
- `EventStore`: batch ingestion, idempotent event writes, event retrieval, issue
  occurrences, search, aggregation, and retention.
- `BlobStore`: debug symbols, source maps, minidumps, and attachments.
- `ArchiveManifestStore`: archive segments, checksums, schema versions, and archive
  state transitions.

`JobBus` is not part of storage. Background job delivery is a separate subsystem.

The interfaces expose domain operations such as `ingest_event_batch`,
`list_project_issues`, and `transition_issue_state`. They do not expose a generic
SQL, collection, or CRUD abstraction.

### Dispatch model

Storage implementations use enum dispatch instead of `dyn` dispatch. The intended
shape is:

```rust
pub enum EventStorage {
    Mongo(MongoEventStorage),
}

pub enum ControlStorage {
    Mongo(MongoControlStorage),
}
```

Dispatch happens once per domain operation or event batch, not once per event field.
Consistent constructors create all database-backed components together so that a
configuration cannot accidentally combine event storage from one database with
control storage from another.

Adding another enum variant is allowed in the future, but the first version does not
define a stable storage plugin ABI.

### Database backend

MongoDB is the only database backend implemented in the first version.

```toml
[storage]
backend = "mongodb"
uri = "mongodb://localhost:27017"
database = "faultkeep"
```

The generic name `embedded` is not used. SQLite is not required or planned for the
first version. A second database backend will only be considered after the MongoDB
implementation, data model, and conformance behavior are stable.

MongoDB Community Server and its SSPL licensing are accepted project dependencies.
Faultkeep connects through the MongoDB driver but does not embed or redistribute
`mongod` in the Faultkeep binary or container image.

Initial deployments are unsharded. A standalone MongoDB instance may be used for
development and small installations; a replica set is the production topology when
high availability or multi-document transactions are required. Native MongoDB
sharding may be evaluated later.

Faultkeep will not implement application-level routing across independent MongoDB
servers. Doing so would require custom balancing, failover, scatter/gather queries,
and cross-partition consistency.

### Partitioning

ADR-0007 selects one regular event collection with TTL-based retention for the first
version. Time-series collections are excluded for events, and physical time
partitions remain a benchmark-driven future extension.

### Blobs and archives

`BlobStore` uses the local filesystem by default. S3-compatible storage is optional
for clustered or externally managed deployments. This keeps debug symbols, source
maps, minidumps, and attachments out of BSON documents.

Cold event archival is optional and disabled by default. With no archive configured,
events are permanently deleted after their retention period.

When archival is enabled, old events are written as versioned Apache Parquet files
with Zstandard compression to either a filesystem or S3-compatible storage. An
archive is checksummed and verified before the corresponding source data becomes
eligible for deletion.

Database-to-database live migration is outside the first-version scope. A future
migration path will use an offline export/import process with versioned metadata,
Parquet events, blob objects, and checksummed manifests.

## Consequences

- The first implementation can use MongoDB-specific batch writes, indexes,
  atomic operations, and aggregation features without being limited by a weakest
  common backend.
- The code retains explicit domain boundaries that make testing and a future second
  backend possible.
- A MongoDB deployment is an external runtime dependency, so MongoDB mode is not a
  literal single-container installation.
- Small installations can run without S3, MinIO, or an archive service.
- Native sharding and time partitioning remain deliberate future decisions rather
  than assumptions embedded in the first schema.

## Deferred questions

- Future native shard key and resharding plan.
- Conditions that would justify implementing a second database backend.
