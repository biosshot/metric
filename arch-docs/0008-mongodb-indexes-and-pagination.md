# ADR-0008: Initial MongoDB indexes and cursor pagination

- Status: Accepted
- Date: 2026-07-20

## Context

At high event volume, every secondary index increases insertion cost, disk use,
replication traffic, and retention work. Indexes must therefore be justified by
accepted query shapes rather than added for every potentially searchable field.

The initial database must efficiently serve durable backlog refill, project and issue
event timelines, issue lists, hourly charts, retention, and optional archival.

## Decision

### Processor readiness field

ADR-0022 maps pending/retry state to compact `q`. Its `q.n` next-attempt date is
always populated while `q.s == 0`. A newly accepted event uses server `r`; a temporary
processing failure moves `q.n` into the future according to retry policy.

Dispatcher queries pending events whose `q.n` is not later than the current time and
orders them by `q.n`, `r`, and `_id`.

### Event indexes

In addition to MongoDB's automatic `_id` index, the initial `events` indexes are:

```javascript
// Global Processor backlog; partial on q.s == 0
{ "q.n": 1, r: 1, _id: 1 }

// Project event timeline
{ p: 1, o: -1, _id: -1 }

// Events belonging to one issue
{ p: 1, u: 1, o: -1, _id: -1 }

// Exact Search v1 tokens; partial on k existing and non-empty
{ p: 1, k: 1, o: -1, _id: -1 }

// Absolute event expiration; expireAfterSeconds == 0
{ x: 1 }

// Archive work; partial on h exists
{ h: 1, _id: 1 }
```

The Processor and archive indexes are partial so processed or archive-ineligible
events do not pay their ongoing storage cost. The global Processor query deliberately
does not use `project_id` as a prefix because one Dispatcher services all projects.
MongoDB 8.0 rejects `$exists: false` inside a partial-index filter, so the archive
index uses only `{ h: { $exists: true } }`. ADR-0022's state invariant removes `h`
when `z` is installed, making `z` absence implied for every indexed document.

The deterministic composite event `_id` remains the only index required for exact
project-and-Sentry-event-id lookup. Event index definitions use physical names from
ADR-0022; other collection indexes below keep descriptive fields.

### Project and DSN-key indexes

`projects` uses its random positive 31-bit `_id` as both primary storage identity and
Sentry-compatible project ID. `project_keys` uses the 16-byte DSN key as its `_id`.
The only additional initial key-management index is:

```javascript
// Project key administration and rotation history
{ project_id: 1, status: 1, created_at: -1 }
```

Ingest lookup uses only the automatic `project_keys._id` index.

### Issue indexes

In addition to `_id`, the initial `issues` indexes are:

```javascript
// All issues in a project
{ p: 1, l: -1, _id: -1 }

// Issues filtered by workflow status
{ p: 1, s: 1, l: -1, _id: -1 }
```

Assignment, release, environment, level, and regression indexes are not created
until their exact UI and API query shapes are accepted.

Notification expansion adds a partial Issue backlog index:

```javascript
// Partial on compact notification-ready j == true
{ j: 1, _id: 1 }
```

ADR-0024 additionally creates the collection's one compound text index
`{ p: 1, t: "text" }` with `default_language: "none"` for bounded Issue titles.

### Notification-delivery indexes

In addition to `_id`, `notification_deliveries` uses:

```javascript
// Global due-delivery backlog; partial on status == "pending"
{ next_attempt_at: 1, _id: 1 }

// Project delivery history
{ project_id: 1, created_at: -1, _id: -1 }

// Absolute terminal expiration; expireAfterSeconds == 0
{ delete_at: 1 }
```

### Hourly-statistics indexes

In addition to `_id`, `issue_stats_hourly` uses:

```javascript
// One issue's time series
{ project_id: 1, issue_id: 1, bucket_start: 1 }

// Project-wide time range and aggregation
{ project_id: 1, bucket_start: 1, issue_id: 1 }

// Absolute bucket expiration; expireAfterSeconds == 0
{ expire_at: 1 }
```

### Release and environment catalog indexes

In addition to their deterministic `_id` indexes, catalog queries use:

```javascript
// Organization Release timeline
{ organization_id: 1, last_seen: -1, _id: -1 }

// Releases associated with a project in the organization
{ organization_id: 1, project_ids: 1, last_seen: -1, _id: -1 }

// Project Environment catalog
{ project_id: 1, hidden: 1, last_seen: -1, _id: -1 }
```

The initial Event index set does not add release, distribution, or environment
indexes. Corresponding Issue filters require a separately accepted query projection.

### Debug-file and upload-job indexes

ADR-0027 defines compact physical fields and accepts these ready-file indexes:

```javascript
// Exact Symbolicator lookup; partial on d existing
{ p: 1, d: 1, u: -1, _id: -1 }

// Exact Symbolicator lookup; partial on c existing
{ p: 1, c: 1, u: -1, _id: -1 }

// Permanent sentry-cli checksum idempotency
{ p: 1, h: 1 } // unique

// Project debug-file list
{ p: 1, u: -1, _id: -1 }
```

Transient `debug_uploads` adds:

```javascript
// Pending/retry recovery
{ s: 1, r: 1, _id: 1 }

// Absolute terminal expiration; expireAfterSeconds == 0
{ e: 1 }
```

There is no MongoDB index or document per temporary chunk.

### JavaScript artifact-bundle indexes

ADR-0028 accepts one ready bundle rather than one document per internal source file.
ADR-0029 defines its compact physical fields and indexes:

```javascript
// Organization-bound Debug ID token candidates
{ k: 1 }

// Exact legacy project/Release/dist binding candidates
{ "b.p": 1, "b.r": 1, "b.d": 1, u: -1, _id: -1 }

// Permanent organization-scoped sentry-cli checksum idempotency
{ o: 1, h: 1 } // unique

// Project artifact listing
{ "b.p": 1, u: -1, _id: -1 }
```

`k` contains only deduplicated JavaScript Debug ID tokens. Internal bundle URLs do
not create one index entry each. Binding queries use `$elemMatch` on the common `b`
array path so project, Release, and dist come from one association.

Transient `artifact_uploads` adds:

```javascript
// Pending/retry recovery
{ s: 1, r: 1, _id: 1 }

// Absolute terminal expiration; expireAfterSeconds == 0
{ e: 1 }
```

ADR-0031 adds partial indexes containing only non-ready Artifact Bundle states:

```javascript
// Due orphans: partial on e existing, b absent, and s absent
{ e: 1, _id: 1 }

// Expired deletion claims: partial on s == 1
{ s: 1, e: 1, _id: 1 }

// Project-deletion completion: partial on j existing
{ j: 1, _id: 1 }

// Tombstone cleanup: partial on s == 2, expireAfterSeconds == 0
{ e: 1 }
```

Ready bundles do not enter these indexes. Publishing-state recovery is owned by the
durable `artifact_uploads` job.

### Cursor pagination

Event and issue list APIs use keyset/cursor pagination. They do not expose deep
offset pagination implemented with MongoDB `skip`.

Every ordered query includes `_id` as a deterministic tie-breaker. Public cursors are
opaque, versioned values that encode the last sort tuple. Their representation is an
API concern and does not expose MongoDB query construction to clients.

### Project-deletion job indexes

ADR-0030 adds one durable job per deletion operation:

```javascript
// Due grace/retry/purge work; partial on a nonterminal state
{ next_run_at: 1, _id: 1 }

// Absolute cleanup of completed/cancelled jobs; expireAfterSeconds == 0
{ expire_at: 1 }
```

Dataset deletion uses each project-owned collection's existing project-prefixed
index and stable `_id` cursor. It does not add a second generic deletion index to
every collection.

### Arbitrary fields

ADR-0023 permits only bounded exact tokens for configured search fields. The index set
does not index arbitrary body data, unconfigured tags, contexts, extra, request data,
stack traces, or Event messages. It does not create a wildcard or MongoDB text index.

### Verification

Every accepted query shape must have a production-shaped benchmark and an
`explain("executionStats")` assertion or recorded baseline. Relevant observations
include the selected index, returned documents, examined keys and documents,
execution time, and whether an in-memory or disk-backed sort occurred.

An index is added only with its supported query contract. Index creation is not an
automatic response to an isolated slow query.

## Consequences

- Fresh event insertion maintains a bounded, explainable set of indexes.
- The durable Processor backlog index shrinks as events leave `pending`.
- Project and issue timelines support stable newest-first cursors.
- Deep pages do not become progressively slower because of offset scanning.
- Arbitrary tag and full-text search are not accidentally promised by the initial
  operational indexes.
- New filters may require deliberate additional projections or indexes.

## Deferred questions

- Fairness across projects in the global Processor backlog.
- Exact index names and migration mechanics.
