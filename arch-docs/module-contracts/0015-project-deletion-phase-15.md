# Phase 15 contract: project deletion and core capacity protection

> Historical phase contract, not an upgrade runbook. Do not recreate a data-bearing
> installation based on this document. The current binary requires generation 19;
> see [the current upgrade runbook](../../docs/upgrading.md).

- Status: accepted for implementation
- Date: 2026-07-23
- Owners: `application::deletion`, `ports::ProjectDeletionStore`,
  `mongo::deletion`, native API/Web, server composition/configuration

## Responsibilities and exclusions

The application owns authorization-facing commands, exact slug confirmation,
local durable-ingest fencing/drain, worker timing, bounded timeout/retry policy,
and low-cardinality metrics. `ProjectDeletionStore` exposes only typed lifecycle
commands and one bounded purge step. The MongoDB adapter owns BSON, collection
classification, indexes, durable job state, dataset cursors, cleanup, and the
permanent project tombstone.

Phase 15 does not add migrations, MCP, NATS, sharding, disk spool, BlobStore,
attachments, minidumps, object storage, or a distributed worker lease. No
project-owned filesystem producer exists before Phase 16, so the versioned
filesystem namespace registry is explicitly empty. Adding a namespace requires
classification, capacity protection, and purge behavior in the same change.

## Lifecycle and command contract

The lifecycle is `active|disabled -> pending_delete -> purging -> deleted`.
`pending_delete` is cancellable until `purge_after`; `purging` and `deleted` are
not. The delete command requires `project:admin`, CSRF for Web sessions, an
exact project-slug confirmation, and a 16-byte idempotency key. HTTP accepts the
durable operation with 202 and returns its operation ID and status URL.

The durable job is written before the DSN fence. Repeating the same operation
repairs an interrupted fence and returns the same job. A distinct active
operation conflicts. The fence changes only active keys to
`suspended_by_deletion`; already disabled keys stay disabled. Cancellation
restores the prior project state and changes only deletion-suspended keys back
to active. Native project mutations fail with conflict while the project is not
active.

The in-process `ProjectWorkRegistry` rejects new durable ingest after the fence
and waits for already-entered durable writes up to a configured bound. On
restart the authoritative project/key state rejects fresh ingest without
reconstructing RAM state. Processor already reloads project state before its
stages and records a permanent project-fenced outcome. A complete second purge
pass catches writes that crossed the durable boundary immediately before the
fence.

## Durable job, registry, and purge

`project_deletions` stores the append-only plan version, operation/project/
organization/actor identity, prior project state, phase, numeric dataset code,
cursor, reconciliation-pass marker, grace and retry timestamps, bounded attempt
state, terminal marker, completion/TTL timestamps, and slug-reservation state.
Due and terminal-expiration indexes are exact bootstrap-owned schema; only one
non-terminal operation may exist per project.

Dataset codes are append-only and never reused:

| Code | Dataset | Classification | Action |
| ---: | --- | --- | --- |
| 10 | `events` | project-owned | delete by `p` in `_id` pages |
| 20 | `issue_activities` | project-owned | delete by `p` in `_id` pages |
| 30 | `issue_stats_hourly` | project-owned | delete by `project_id` |
| 40 | `issues` | project-owned | delete by `p`; removes search projection |
| 50 | `environments` | project-owned | delete by `project_id` |
| 60 | `releases` | organization-shared | detach project; delete only empty association |
| 70 | `project_keys` | project-owned | delete after data reconciliation |
| 80 | `projects`, `project_deletions` | control plane | tombstone/status ownership |

ADR-0040 later renamed dataset code 10 from the physical `events` collection to
`error_events` without reusing the code. Phases 24-26 added codes 11 `logs`, 12
`spans` and 13 `span_stats_hourly`; the runtime registry is authoritative for the
current complete set.

`audit_log` is retained audit. Organizations, users, memberships, sessions,
tokens, setup tokens, and `schema_meta` are global. The compile-time registry
must classify every bootstrap-owned collection exactly once; adding an
unclassified collection fails its schema test.

Each worker invocation handles at most one configured MongoDB document batch.
The acknowledged cursor advances only after the dataset mutation succeeds.
Repeating a batch is idempotent. After code 70, the cursor and code reset to 10
for a full final reconciliation. Only then are keys absent and the project
compacted in place to a permanent tombstone retaining identity,
organization, deletion operation, deletion time, and reserved slug. The slug
is released after the configured reservation; the tombstone is never TTL
deleted. Terminal jobs have bounded status retention.

## Capacity and operability

The existing ingest backlog guard continues to reject before durable admission
when pending count/age crosses its bounded hysteresis threshold. Project
deletion adds finite job cardinality per active project, finite batches,
one-step worker polling, operation timeouts, exponential bounded retry, and no
unbounded filesystem work. Status exposes phase, dataset code, reconciliation,
attempts, next attempt, bounded error code, completion, and status URL.

Metrics contain only static outcomes and counts: fenced ingest and deletion
worker progress/idle/error/timeout. IDs, slugs, collection cursors, payloads,
and database errors are forbidden metric labels. Request/cancel actions append
retained administrative audit records.

Phase 15 advances the initial schema generation to 3. In accordance with
ADR-0035 and the explicit no-migrations scope, generation-2 databases are
rejected rather than modified implicitly; development data must use a fresh
database or be explicitly recreated.
