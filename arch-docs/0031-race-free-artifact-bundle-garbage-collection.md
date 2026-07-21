# ADR-0031: Race-free Artifact Bundle garbage collection

- Status: Accepted
- Date: 2026-07-21

## Context

ADR-0029 deduplicates immutable Source Bundle content inside an organization and
stores mutable project/Release/dist bindings in `artifact_bundles.b`. ADR-0030 removes
those bindings during project deletion. When the last binding disappears, the
MongoDB document and BlobStore object become reclaimable.

A direct `if bindings are empty, delete blob` sequence is unsafe. A concurrent upload
can rescue the same content after the check but before BlobStore deletion, leaving a
new ready binding that points to a missing deterministic object key. MongoDB and
BlobStore do not share a transaction, and a stale deletion worker must not be able to
delete a later republication of identical content.

## Decision

### State stays with the bundle

Version one does not add a generic `blob_gc` collection. Rare garbage-collection
fields live in the existing `artifact_bundles` document. Ready bundles do not store
state, eligibility, lease, claim, or deletion-operation fields and do not enter GC
indexes.

The physical shapes are:

```javascript
// Ready; b is nonempty
{
  _id, o, b, g, k, x, h, z, u,
  v // physical object generation, optional; absence means zero
}

// Orphan; content is still readable and may be rescued
{
  _id, o, g, k, x, h, z, u,
  v,
  e, // deletion eligibility time
  j  // originating project-deletion operation, optional
}

// Deleting
{
  _id, o, g, k, x, h, z, u,
  v,
  s: 1,
  e, // claim lease deadline
  c, // random claim token
  j  // optional
}

// Physically deleted tombstone
{
  _id, o, h, v,
  s: 2,
  e // tombstone expiration
}

// Reserved for republication by a durable artifact_uploads job
{
  _id, o, h, v,
  s: 3,
  e // bounded publication/recovery deadline
}
```

BSON null is forbidden. `b` is omitted rather than stored as an empty array. `v` is
a nonnegative BSON `int32`; absence means generation zero and zero is not written.
`c` and `j` are fixed-size binary operation identifiers present only while needed.

The ready-state codec from ADR-0029 remains unchanged for the common first
publication except for the optional `v`, which appears only after content has been
physically deleted and republished.

### Atomic last-binding transition

Binding removal uses one MongoDB update pipeline that computes the new canonical
`b`. If at least one binding survives, only `b` changes. If none survives, the same
atomic update removes `b` and sets `e` to the applicable garbage-collection deadline.
No observer can see a ready document with an empty array or an orphan without its
eligibility time.

An ordinary explicit association removal initially uses:

```toml
[artifact_gc]
orphan_grace_period = "24h"
```

The grace period is configurable. It reduces delete/re-upload churn and allows an
identical authorized upload to rescue the still-readable object without transferring
its bytes again.

### Atomic rescue before claim

An authorized upload that finds an orphan with matching `o`, `h`, complete content
identity, and no `s` atomically adds its canonical binding and removes `e` and `j`.
The document becomes ready again without BlobStore work. The affected project's
`artifact_revision` is incremented before the assemble operation reports `ok`.

An upload cannot add a binding while `s == 1`. It returns the compatible in-progress
state and polls or retries. After deletion finishes, the durable upload job may claim
the tombstone for republication.

### GC claim and recovery

Scheduler claims an eligible orphan with one conditional `findOneAndUpdate` requiring
`b` and `s` to be absent and `e <= now`. It sets `s = 1`, a random `c`, and a bounded
lease deadline in `e`.

Initial settings are:

```toml
[artifact_gc]
claim_lease = "5m"
scan_batch_documents = 100
max_concurrency = 4
```

They are configurable with safe bounds. A worker conditions every metadata mutation
on `_id`, `s == 1`, and its exact `c`. Before dispatching BlobStore deletion it
revalidates the claim and requires remaining lease time to exceed the configured hard
BlobStore operation timeout. Expired claims are recoverable by Scheduler.

Duplicate or repeated deletion of the same physical generation is success. The claim
token prevents a stale worker from compacting metadata owned by another claim; the
physical generation described below prevents it from deleting a later publication.

### Fenced physical object generations

Generation zero preserves ADR-0029's compact original key:

```text
a/{organization_id_base36}/{artifact_bundle_id_base64url_no_pad}
```

After a physical deletion, republication increments `v` and uses:

```text
a/{organization_id_base36}/{artifact_bundle_id_base64url_no_pad}/{generation_base36}
```

The application always derives the key from ready metadata and never accepts a
client-supplied generation or raw key. A GC worker captures the generation it claimed
and can delete only that key. If it resumes late, a newer ready bundle uses another
key and cannot be damaged by the stale delete.

Generation increment is checked and cannot wrap. Exhaustion is a permanent
administrative error rather than key reuse; in practice it would require billions of
delete/republication cycles for the same content.

### Blob-first deletion and compact tombstone

The claimed worker performs:

1. reconstruct and delete the claimed generation's BlobStore key;
2. treat an absent object as success;
3. conditionally replace metadata with the compact `s = 2` tombstone;
4. remove `j` as proof that physical deletion completed;
5. retain the tombstone for the configured safety period.

It never deletes ready metadata before BlobStore deletion. A crash after the blob
delete leaves `s = 1`; recovery repeats the missing-object delete and compacts the
document. Lookup never returns deleting, tombstone, or publishing documents as ready
candidates.

The initial tombstone retention is 24 hours. It is configurable but must exceed the
maximum claim lease and BlobStore operation lifetime by a validated safety margin.
A partial TTL index removes only `s == 2` tombstones; TTL is storage cleanup, not a
state transition.

### Republication after physical deletion

An `artifact_uploads` job must exist durably before it changes a tombstone. It
atomically claims `s == 2`, increments `v`, changes the state to `s = 3`, and then
publishes the reassembled, validated bundle under the new generation key.

After publication it replaces the tombstone with complete ready metadata and
bindings, unsets `s` and `e`, and performs the required project revision increments.
The assemble call is not `ok` before all those writes are acknowledged. A crash in
`s == 3` is resumed from the existing artifact-upload job; no second publication
generation is allocated for an ordinary retry of that job.

The retained `{o, h}` unique identity lets a repeated `sentry-cli` request find the
tombstone or publishing state before ready metadata exists.

### Project-deletion completion

When ADR-0030 removes the final binding during irreversible project purge, the same
atomic orphan transition sets `j` to that deletion operation and makes it immediately
eligible:

```toml
[artifact_gc]
project_deletion_orphan_grace_period = "0s"
```

The project-deletion job does not store Bundle IDs. It waits until the partial index
contains no document with its `j`:

- physical deletion compacts the bundle and removes `j`;
- rescue by another authorized, non-deleting project adds a real binding and removes
  `j`, because the shared content must remain;
- a failed or stuck GC retains `j`, keeping project deletion visibly pending.

If other bindings already survived the project's removal, no orphan and no `j` are
created: the immutable bytes are legitimately still owned by those associations.
Thus project deletion guarantees physical removal of content that became truly
unreferenced, without destroying content still referenced by another project.

### GC-only indexes

Ready documents do not participate in these partial indexes:

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

Index migrations use distinct explicit names and validators assert their partial
predicates. `s == 3` recovery is driven by `artifact_uploads`, not a second bundle
poller.

### Scope

This decision covers immutable final Artifact Bundle objects. Organization-scoped
temporary chunks keep ADR-0025's age-based cleanup and can be retransmitted by
`sentry-cli`. Project-private debug files and event-owned blobs do not share bundle
bindings; their explicit deletion and retention paths may reuse the generation
pattern later but are not silently changed here.

## Consequences

- Ready bundles pay no GC state or GC-index cost.
- A repeat upload can cheaply rescue an orphan before deletion begins.
- MongoDB/BlobStore crashes cause retry or extra tombstones, not ready references to
  a deleted generation.
- Stale deletion workers cannot remove a newer republication.
- Project deletion can await physical cleanup without storing an unbounded Bundle ID
  list.
- Repeated physical deletion/republication adds one optional generation integer and
  a key suffix to that bundle only.

## Deferred questions

- Reusing the generation/tombstone protocol for explicit debug-file deletion.
- Generalizing the state machine if another organization-shared immutable blob type
  is introduced.
- Production measurements for orphan churn, claim concurrency, and tombstone volume.
