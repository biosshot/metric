# ADR-0030: Resumable project deletion across MongoDB and BlobStore

- Status: Accepted
- Date: 2026-07-21

## Context

All projects share one MongoDB database, while large event-owned objects, debug
files, and archives live in BlobStore. A project can own millions of documents and
objects, and some organization-scoped records such as Releases and Artifact Bundles
can be associated with several projects.

One HTTP handler cannot atomically delete both stores. A process can crash after any
step, MongoDB TTL cannot delete BlobStore objects, and an Ingest or Processor task
that races with deletion could otherwise recreate data after a collection has been
scanned. Project deletion therefore needs a durable, bounded, idempotent workflow.

## Decision

### Lifecycle

A project follows this deletion state machine:

```text
active
  -> pending_delete
  -> purging
  -> deleted tombstone
```

`pending_delete` is reversible during a configurable grace period. `purging` is
irreversible. Repeating a delete command returns the existing operation rather than
starting another one.

The initial configuration is:

```toml
[project_deletion]
grace_period = "24h"       # zero starts purging immediately
delete_batch_documents = 5000
completed_job_retention = "30d"
slug_reservation = "30d"
```

All values are configurable within safe bounds. `delete_batch_documents` bounds one
MongoDB write and is not a promise that every document has the same deletion cost.

### Authorization, confirmation, and response

Project deletion requires `project:admin`, the same explicit destructive-command
confirmation for Web/API and a future MCP adapter, and an idempotency key. The command
writes the normal bounded audit entry from ADR-0021 without copying project payloads
or secrets.

The HTTP command returns `202 Accepted` with a deletion-operation identifier and a
status URL after the durable job exists and the ingest fence has been acknowledged.
The UI/API expose the phase, grace deadline, retrying failure code, and whether the
operation remains cancellable. A future MCP adapter exposes the same application
query.

### Immediate ingest and processing fence

Deletion first creates a `project_deletions` job, changes the project to
`pending_delete`, changes only currently active project keys to
`suspended_by_deletion`, and invalidates their local authorization-cache entries.
Previously disabled keys remain disabled. Unauthenticated ingest receives the same
generic credential failure used for missing, disabled, or deleted projects.

From this point:

- Ingest does not accept new project data;
- Dispatcher does not refill this project's pending Events;
- queued RAM items for the project are discarded;
- Scheduler does not create ordinary project work;
- Web/API reject project mutations; a future MCP adapter must do the same;
- only deletion status and cancellation remain administratively visible.

The version-one `--role=all` runtime uses a local per-project in-flight counter. Before
purging, it stops new work, drains tasks that already crossed the fence, and performs
a final reconciliation pass after deletion. If the process crashes, the durable
project state prevents backlog refill on restart and residual writes are found by
the resumed scan.

A future split-role runtime requires a distributed drain/epoch acknowledgement
before it can claim equivalent purge semantics; that protocol is not simulated in
version one.

### Cancellation during grace

Cancellation is accepted only while the project is `pending_delete`. It changes only
keys in `suspended_by_deletion` back to active, restores the project to active,
invalidates local cache entries, records an audit event, and terminally marks the job
cancelled. A key that had already been disabled or revoked is never re-enabled.

Cancellation does not attempt to restore data after `purging` begins. A zero grace
period therefore makes the operation immediately irreversible.

### One durable job, not one job per object

`project_deletions` stores one bounded resumable job per deletion operation. Its
logical fields are:

```javascript
{
  _id,
  project_id,
  organization_id,
  phase,
  dataset_code,
  cursor,
  blob_namespace,
  blob_continuation,
  grace_until,
  attempt,
  next_run_at,
  created_at,
  updated_at,
  error_code,
  completed_at,
  expire_at
}
```

Fields unused by the current phase are omitted rather than stored as null. Phase and
dataset codes are stable append-only numeric registries. The cursor is opaque to the
job runner and decoded only by the owning Storage adapter. A job document never
contains an unbounded list of Event IDs, blob keys, or collection names.

Temporary failures increment `attempt`, set bounded exponential backoff in
`next_run_at`, and resume the same phase. Active jobs have no TTL. Completed and
cancelled jobs receive `expire_at`; the permanent audit entry and project tombstone
remain after the operational job expires.

The Scheduler indexes due work by `next_run_at` and `_id`, with a partial predicate
for nonterminal jobs, and uses an absolute TTL index on `expire_at` only for terminal
job cleanup.

### Versioned deletion-plan registry

Every application-owned MongoDB collection and BlobStore namespace must register one
of these policies:

```text
project_owned       delete records/objects scoped to the project
organization_shared remove only the project association
retained_audit      retain bounded non-payload administrative history
global              contains no project-owned data
```

Schema tests fail if a new collection or namespace has no deletion policy. Each
`project_owned` dataset provides its project-scoped index, bounded cursor scan, and
idempotent deletion implementation. This prevents a newly introduced feature from
silently escaping project deletion.

Stable numeric dataset codes allow an in-progress job to resume after deployment.
New datasets may be appended to the plan. Before finalization, Scheduler reruns the
current registry from the beginning so a dataset added while a deletion was pending
is also cleaned.

### Bounded MongoDB purge

Project-owned collections are deleted in batches selected by project scope and a
stable `_id` cursor. The implementation does not issue an unbounded collection-wide
`deleteMany` for a large dataset. After each acknowledged batch, the job persists its
cursor and yields so ordinary ingestion and replication are not monopolized.

At minimum the registry classifies Events, Issues, hourly buckets, Environments,
Issue activity, alert and notification state, debug files and upload jobs, project
keys, project-owned archive manifests, and project-scoped ingest outcomes. Exact
physical field names remain owned by their respective MongoDB codecs.

Deleting an already absent document is success. A crash after deletion but before
cursor persistence merely repeats an idempotent batch. MongoDB transactions are not
required.

### BlobStore purge

Project-exclusive objects are deleted through typed BlobStore namespaces and
deterministic project prefixes. The deletion worker never accepts an arbitrary raw
filesystem path or shell command.

S3-compatible implementations list and delete bounded pages and persist the backend
continuation token. Local-filesystem implementations resolve and verify the target
under the configured BlobStore root before traversing it. A missing object is
success. Backends without a strong listing-consistency guarantee perform repeated
empty scans according to a bounded settle policy before the namespace is complete.

The project job does not contain one row per blob. Blob enumeration state remains the
namespace code and continuation token. Event metadata may disappear before an
exclusive orphan object is removed, but the project remains inaccessible and the job
cannot finalize that namespace until its required empty scans succeed.

### Shared Releases and Artifact Bundles

Organization-scoped Releases lose only the deleted project's association. A Release
used by another project remains intact.

`artifact_uploads` drops requested bindings for the project; a job with no remaining
binding cannot publish for that project. `artifact_bundles` atomically removes every
`b` member whose `p` is the deleted project. A bundle with surviving bindings and its
immutable blob remain available to those projects.

If no Artifact Bundle binding remains, ADR-0031 atomically marks the object orphaned
with the deletion-operation ID and makes it immediately eligible for its race-free
garbage collector. Project deletion waits until the bundle is physically deleted or
rescued by another authorized project. The job does not store an unbounded Bundle ID
list; a partial index on the temporary operation ID is the completion barrier.

### Permanent project tombstone

After all project-owned datasets and namespaces are complete and all shared
associations are removed, the existing `projects` document is compacted in place to a
minimal deleted tombstone. It retains only the numeric project ID, organization ID,
deleted state, deletion time, operation ID, and the old slug until its reservation
deadline. It contains no DSN key, policy, user content, secret, or display metadata.

Keeping the same `_id` permanently prevents numeric project ID reuse. The slug is
unset after the initial configurable 30-day reservation, allowing a future project
in the organization to use it without making old numeric routes address that new
project. The tombstone itself never receives TTL.

All `project_keys` documents are removed during irreversible purge. Their absence and
the deleted project state produce the same generic unauthenticated response, and old
keys can never be restored after completion.

## Consequences

- Large projects are removed without one long-running database write.
- A crash can delay deletion but cannot require restarting it from the beginning.
- Grace-period cancellation does not accidentally reactivate revoked keys.
- Numeric project IDs are never reused, while slugs can be reclaimed after a bounded
  safety period.
- Shared Releases and bundles belonging to other projects are not destroyed.
- A schema-registry test makes deletion support mandatory for every new dataset.
- Unreferenced shared blobs use a generation-fenced GC protocol and cannot be
  mistaken for content still bound to another project.

## Deferred questions

- Distributed worker drain and cache invalidation after split roles return.
- Organization deletion as an orchestrator of project deletion jobs.
