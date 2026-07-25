# ADR-0004: Shared MongoDB event and issue model

- Status: Accepted
- Date: 2026-07-20

## Context

The MongoDB layout must support an installation with many projects without creating
a database or collection for every project. It must also distinguish an individual
error occurrence from the durable workflow state of a group of equivalent errors.

Precomputed time buckets are required from the first version so charts, recent-rate
queries, and aggregate history do not depend on repeatedly scanning retained raw
events.

## Decision

### One shared database

The first version uses one MongoDB database for all organizations and projects. The
initial collection set is:

```text
organizations
users
organization_memberships
web_sessions
api_tokens
password_setup_tokens
audit_log
projects
project_keys
error_events
logs
spans
span_stats_hourly
issues
issue_activities
alert_rules
notification_destinations
notification_deliveries
issue_stats_hourly
debug_files
debug_uploads
artifact_bundles
artifact_uploads
releases
environments
archive_manifests
project_deletions
schema_meta
```

ADR-0021 defines the identity, authentication, authorization, and audit semantics of
the added control-plane collections.

Project-owned documents contain `project_id`. Project-scoped storage operations must
always include it in their filter, authorization context, and applicable compound
indexes.

ADR-0019 and ADR-0022 define that `projects._id` and every project reference use the
same random positive 31-bit integer stored as BSON `int32`. There is no second
internal project identifier. Rotatable DSN keys are separate documents whose binary
key is their `_id`.

Collections and databases are not created per project. A dedicated database for an
exceptionally large or specially isolated organization or project may be considered
later, but it is not part of the first storage model.

### Event identity

An event represents one concrete occurrence received from an SDK. Its MongoDB `_id`
is the 20-byte deterministic binary composite defined by ADR-0022: a four-byte
big-endian project identifier followed by the 16-byte Sentry event identifier.

This makes ingestion retries idempotent without requiring a separate unique index on
`(project_id, event_id)`.

The physical Event shape is deliberately compact:

```javascript
{
  _id,
  p, // project_id
  r, // received_at
  o, // occurred_at
  x, // expire_at, optional
  h, // hot/archive due time, optional
  z, // archive segment, optional
  u, // 16-byte issue_id after processing
  l, // non-default level enum, optional
  a, // platform enum
  s, // non-default scrub-policy revision, optional
  q, // pending/retry/failed only
  k, // compact search tokens, optional
  b  // versioned, adaptively compressed canonical body
}
```

ADR-0022 is authoritative for the physical codec. Domain code continues to use
descriptive logical names. The body contains the scrubbed accepted data while pending
and is atomically replaced by the normalized canonical and derived representation
during finalization. Both complete representations are never retained together.

The Event ID is recovered from `_id` and is not duplicated. Exact release,
distribution, environment, tags, title inputs, exception data, contexts,
breadcrumbs, native detail, and symbolication detail live once in the compressed
body. Only accepted query projections exist outside it.

An event does not duplicate issue workflow state, issue counters, assignment, full
issue summary, GroupingKey, grouping strategy, or grouping explanation.

### Issue identity and purpose

An issue represents a group of equivalent events and the user's workflow state for
that group. Its 16-byte identifier is the truncated domain-separated derivation from
the project identifier and complete versioned grouping key defined by ADR-0014.

ADR-0024 defines the compact physical Issue shape:

```javascript
{
  _id,
  p, g, t, q,
  f, l,
  e, v, r,
  c, s, a, w, d,
  fr, lr, m,
  j, n,
  b
}
```

Issue state includes creation and lookup identity, first/last occurrence, resolution,
ignore and regression behavior, assignment, representative events, and other
group-level workflow data. ADR-0015 defines the initial `open`, `resolved`, and
`ignored` lifecycle and the separate low-volume `issue_activities` history.

Physical defaults, workflow packing, Event reference omission, and title search are
defined by ADR-0024. `c`/`occurrence_count` is derived data rather than a source of
truth; its approximate update and reconciliation contract is defined by ADR-0005.

### Event-to-issue relation

Processor computes the grouping key, derives the issue identifier, idempotently
creates or updates the issue, and stores only the compact Issue ID as Event field `u`.
The complete key and selected strategy are kept on the Issue rather than copied into
every Event. ADR-0014 defines revision pinning, strategy selection, native stability
after symbolication, and Issue-level explanation.

### Statistical buckets

`issue_stats_hourly` is part of the initial schema. It contains one rebuildable
projection per project, issue, and UTC hour:

```javascript
{
  _id,             // deterministic from project_id, issue_id, and bucket_start
  project_id,
  issue_id,
  bucket_start,    // start of an hour in UTC
  occurrence_count
}
```

Processor derives the bucket from the normalized event occurrence time and groups
increments by `(project_id, issue_id, bucket_start)` inside a FinalizeBatch. A bucket
therefore receives one increment per relevant batch rather than one update per event.

The initial bucket intentionally contains only `occurrence_count`. Additional
dimensions, unique-user estimates, or breakdowns require separate decisions because
they can multiply collection cardinality.

The hourly projection supports:

- issue and project occurrence charts without scanning raw events;
- recent-frequency sorting and future spike detection;
- aggregate history after raw-event retention removes event documents;
- daily or longer-range queries by summing hourly documents.

`issue_stats_hourly` is derived and may have the same rare positive drift accepted
for `issues.occurrence_count` after a Finalizer crash. It is not the authoritative
source of event membership and must not be used as an exactly-once ledger.

Future StatsD-compatible metrics must not be stored in `issue_stats_hourly`. Their
metric identity, types, tags, cardinality limits, and rollups require a separate
metric-bucket model when that subsystem is designed.

### Index direction

Logical index descriptions use domain names for readability, while MongoDB migrations
map them to the compact ADR-0022 physical keys. Project-scoped compound indexes use
`p` as their equality prefix where the deterministic `_id` is not sufficient.
Candidate access patterns include:

```javascript
{ p: 1, o: -1 }
{ p: 1, u: 1, o: -1 }
{ "q.n": 1, r: 1, _id: 1 } // partial: q.s == pending
{ project_id: 1, last_seen: -1 }
```

These are candidate shapes, not an accepted final index set.

## Consequences

- Schema creation and index migrations run once instead of once per project.
- Project isolation is enforced by application services and storage filters rather
  than MongoDB database boundaries.
- Event queries can use project-prefixed compound indexes without scanning unrelated
  project ranges.
- Issue list and workflow queries operate on one group document instead of grouping
  raw events on every request.
- Hourly issue charts and recent-rate queries read bounded aggregate documents rather
  than scanning raw event payloads.
- The hourly projection survives raw-event deletion and uses the configurable
  retention policy defined by ADR-0007.
- Deleting one project requires scoped deletion rather than `dropDatabase`.

ADR-0030 defines the resumable project-deletion workflow in the shared database.
