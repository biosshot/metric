# ADR-0033: Single-process debug-file deletion and orphan cleanup

- Status: Accepted
- Date: 2026-07-21

## Context

Debug files are project-private optional symbolication inputs, not the primary Event
storage path. Version one exposes only `--role=all`, so upload, explicit deletion,
retention, and orphan reconciliation execute in one process. A multi-node deletion
lease/generation protocol would add metadata states and implementation work for a
topology that is explicitly deferred.

Deletion must still stop new private downloads immediately, survive a crash without
leaving an accessible metadata record, and avoid racing a repeat upload or orphan
reconciler in the single process.

## Decision

### Exact target and authorization

An explicit command deletes one exact `(project_id, DebugFileId)`. It never deletes
all candidates sharing a Debug ID, Code ID, filename, or SHA-1 implicitly. A bulk
operation must contain a bounded explicit list of file IDs.

The command requires `debug_file:delete`, project authorization, destructive-command
confirmation, idempotency behavior, and a bounded audit entry. The compatible HTTP
adapter may expose the Sentry-style project DIF route, but it calls the same typed
application command.

### No deletion state in ready metadata

ADR-0027's ready `debug_files` shape remains unchanged:

```javascript
{ _id, p, d?, c?, y, x, h, z, n, u }
```

Version one adds no generation, tombstone, deletion status, lease, claim token, or
separate `debug_file_deletions` collection. The permanent BlobStore key remains:

```text
d/{project_id_base36}/{debug_file_id_base64url_no_pad}
```

This simplicity depends on the accepted single-process runtime. A split writer/GC
topology must replace this decision before concurrent processes are enabled.

### Shared in-process keyed exclusion

Final publication, explicit deletion, retention deletion, and orphan cleanup share a
bounded keyed mutex registry keyed by `(project_id, DebugFileId)`. Entries disappear
when no task owns or waits on them, so arbitrary IDs cannot permanently grow RAM.

The lock is an implementation coordination primitive, not durable truth. After a
process crash there is no surviving stale worker. MongoDB ready metadata remains the
authoritative publication record, and scheduled orphan reconciliation is the crash
recovery backstop.

### Metadata-first logical deletion

Under the keyed lock, the command:

1. re-reads and validates the exact ready file and project scope;
2. increments `projects.dr` to invalidate private-source lookup caches;
3. conditionally deletes the ready MongoDB metadata by `_id` and `p`;
4. attempts idempotent deletion of the reconstructed BlobStore key;
5. decrements ADR-0032's debug byte/count counters after confirmed physical deletion;
6. emits success and failure metrics without copying debug metadata into audit logs.

Incrementing `dr` before metadata deletion may create a harmless revision gap if the
process fails between the two writes. It avoids losing cache invalidation evidence
after metadata has disappeared. The command is not reported successful before the
revision increment and metadata deletion are acknowledged.

Once metadata is gone, private lookup and download endpoints cannot authorize or
serve the file. BlobStore deletion is best effort in the request path: an unavailable
backend leaves an inaccessible orphan for Scheduler rather than recreating metadata
or keeping the HTTP request open without bound. A missing blob is success.

The delete response therefore guarantees logical removal from authoritative lookup.
Physical removal normally occurs in the same operation and otherwise completes by
orphan reconciliation. Project deletion retains ADR-0030's stronger workflow and
waits for its scoped BlobStore purge.

### Repeat upload

The final assembly worker computes the deterministic DebugFileId before acquiring the
same keyed lock. It then rechecks ready metadata and the final object.

If metadata is absent but the deterministic object remains from an interrupted
delete or publish, the worker verifies complete size and BLAKE3 before reusing or
atomically replacing it. It then inserts full ready metadata and performs the normal
quota and `dr` updates. Unverified existing bytes are never trusted merely because a
key exists.

A terminal `debug_uploads` document is not sufficient for an `ok` response. The
assemble path always verifies that its `f` identifies a current ready `debug_files`
document with matching project and complete identity. If deletion removed it, the
deterministic upload job may be reset to a fresh pending assembly after required
chunks are validated; old terminal fields do not resurrect the file.

### Orphan reconciliation

Scheduler incrementally lists the typed project debug-file namespace. An object is an
orphan candidate when no matching ready metadata exists. It must be older than:

```toml
[debug_files.deletion]
orphan_grace_period = "24h"
max_concurrency = 4
```

Both values are configurable with safe bounds. The grace covers ambiguous MongoDB
acknowledgements and active upload time.

Before deleting a candidate, Scheduler acquires the same keyed lock and rechecks
ready metadata. If a repeat upload published metadata, cleanup skips the object. If
metadata is still absent, it deletes the object idempotently. Capacity counters are
periodically reconciled from ready metadata plus physical namespace scans, so a crash
between blob deletion and counter adjustment cannot permanently corrupt quota state.

### Symbolicator cache boundary

Deletion removes the authoritative private file, changes `dr`, and prevents future
index/download responses from returning it. Already derived symbolicated frames in
Events are not rewritten. External Symbolicator cache files are rebuildable,
non-authoritative deployment data with a bounded operator-configured lifetime;
targeted backend cache purge is not required for this version's delete command.

## Consequences

- Ordinary ready debug-file documents gain no bytes or indexes for deletion.
- Single-process upload/delete/reconciliation races are serialized without a durable
  distributed protocol.
- A crash may leave an inaccessible orphan or an extra revision increment, both
  repairable by existing reconciliation.
- Repeat upload cannot treat an old completed job or unverified object as ready.
- Enabling split application roles requires a new durable fencing decision first.

## Deferred questions

- Multi-process claim, fencing, and physical generations if debug-file writers and GC
  are separated.
- Targeted Symbolicator cache purge if a chosen backend exposes a reliable API and a
  stricter erasure contract is required.
