# ADR-0029: Compact artifact-bundle and upload BSON

- Status: Accepted
- Date: 2026-07-21

## Context

ADR-0028 stores one original JavaScript Source Bundle rather than one persistent
document per internal script or source map. Its conceptual metadata still repeats
descriptive field names and leaves unresolved how one immutable bundle is shared by
several projects or Release/dist associations in the same organization.

The ready lookup must remain cheap for modern Debug IDs and legacy Release lookup.
The transient assembly job must survive process restarts without creating a MongoDB
document per uploaded chunk. Both layouts should follow the compact-codec rules from
ADR-0027 without hiding indexed lookup data inside a compressed body.

## Decision

### Codec ownership and invariants

Rust domain types retain descriptive names. The MongoDB adapter alone owns the
one-character physical fields, numeric states, binary subtypes, default omission,
and migrations in this decision. BSON null is forbidden.

Collection validators, round-trip and malformed-input tests, and golden BSON-size
tests protect the representation. Arrays are sorted and deduplicated where their
order has no protocol meaning.

### Ready artifact-bundle document

`artifact_bundles` contains one content object for each organization and complete
bundle content:

```javascript
{
  _id, // 16-byte deterministic ArtifactBundleId
  o,   // organization ID, BSON int64
  b,   // canonical project/Release/dist bindings
  g,   // bundle DebugId in canonical binary
  k,   // sorted unique JavaScript Debug ID tokens, BSON int64 array
  x,   // complete BLAKE3, 32 binary bytes
  h,   // sentry-cli complete SHA-1, 20 binary bytes
  z,   // original bundle size, BSON int64
  u,   // upload completion time, BSON UTC datetime
  v    // physical object generation, optional; absence means zero
}
```

The original Source Bundle bytes and all content-derived fields are immutable. Only
`b` may grow or shrink as authorized project associations are added or removed. A
repeated upload of identical content in the same organization reuses the ready
document and BlobStore object and merges missing bindings. Identical bytes in another
organization deliberately create another private object.

There are no persisted BlobStore key, URLs, internal archive filenames, per-file
documents, status, manifest, note, uncompressed-size, or entry-count fields. These
are either reconstructed, validated transiently, or read from the immutable bundle.

### Binding representation

Each `b` element has this compact shape:

```javascript
{
  p, // project ID, BSON int32
  r, // exact organization-scoped Release ID, binary, optional
  d  // exact dist string, optional; requires r
}
```

Bindings are sorted lexicographically by `(p, r, d)` and deduplicated. Missing
Release and dist are represented by field omission. `d` cannot exist without `r`.
The authenticated assemble request and resolved Release identity are authoritative;
untrusted archive metadata cannot create a binding.

One bundle may have several bindings for the same project, for example a Debug-ID
association plus two legacy Release/dist associations. Queries that match multiple
binding members must use one `$elemMatch` on `b`, preventing fields from different
array elements from being combined into a false match.

### Debug ID representation and lookup tokens

`g` uses ADR-0027's canonical `DebugId` binary codec. Every usable embedded
JavaScript Debug ID produces ADR-0028's organization-bound, domain-separated 64-bit
lookup token in `k`.

`k` deliberately remains a BSON `int64` array rather than one packed Binary value:
MongoDB must create one multikey index entry per token. The array is bounded, sorted,
and deduplicated. A token collision only adds a candidate; the application enforces
organization/project binding and Symbolicator verifies the real Debug ID inside the
bundle.

### Identity and final object key

The immutable ID is:

```text
ArtifactBundleId = first_128_bits(BLAKE3(
    "artifact-bundle-id-v1" || organization_id_be_u64 || complete_blake3
))
```

An existing ID is an idempotent duplicate only after comparing `o`, full `x`, `h`,
size, bundle Debug ID, and the validated token projection. A mismatch is a collision
error. The permanent compatibility lookup `{o, h}` is also unique; the same
organization and SHA-1 with different authoritative content metadata is rejected as
a collision rather than silently reused.

The initial-generation BlobStore key is reconstructed and is not persisted:

```text
a/{organization_id_base36}/{artifact_bundle_id_base64url_no_pad}
```

ADR-0031 adds an optional compact generation suffix only after an object has been
physically deleted and republished. It also defines transient orphan/deletion fields;
ordinary ready generation-zero documents do not store them.

### Ready-bundle indexes

In addition to `_id`, the accepted indexes are:

```javascript
// Organization-bound Debug ID token candidates
{ k: 1 }

// Exact legacy binding candidates
{ "b.p": 1, "b.r": 1, "b.d": 1, u: -1, _id: -1 }

// Permanent sentry-cli checksum idempotency
{ o: 1, h: 1 } // unique

// Project artifact listing
{ "b.p": 1, u: -1, _id: -1 }
```

`b` is the only array traversed by the compound binding index. MongoDB permits a
compound multikey index over several fields of the same embedded-document array;
the accepted query shape uses `$elemMatch` on their common `b` path so the bounds are
compounded for one binding.

### Transient artifact-upload document

`artifact_uploads` contains one recoverable whole-bundle assembly job:

```javascript
{
  _id, // deterministic 16-byte ID from organization and complete SHA-1
  o,   // organization ID, BSON int64
  h,   // complete SHA-1, 20 binary bytes
  c,   // ordered concatenated 20-byte chunk SHA-1 values
  b,   // canonical requested bindings
  s,   // non-pending state, optional
  a,   // retry attempt when nonzero, optional
  r,   // next retry time, optional
  t,   // creation time
  u,   // update time when different from t, optional
  f,   // final 16-byte ArtifactBundleId, optional
  e,   // terminal absolute expiration time, optional
  q    // stable numeric permanent error code, optional
}
```

The job ID is the first 128 bits of a domain-separated BLAKE3 over organization ID
and the complete SHA-1. Stored identity inputs are compared on an existing ID. If
simultaneous compatible assemble calls select different authorized bindings, their
`b` values are merged atomically and canonicalized; they do not create parallel jobs.

`c` is one Binary payload using ADR-0027's packed chunk-manifest rules. Its length is
a nonzero multiple of 20, its count is bounded, and order and repeated checksums are
preserved. There is no MongoDB document per temporary chunk.

State and omission rules match `debug_uploads`:

```text
s absent  pending
s = 1     assembling
s = 2     complete
s = 3     permanently failed
```

Temporary failures return to pending with `r`. Completed jobs expire after 24 hours
and permanently failed jobs after seven days by initial configurable defaults.
Pending and assembling jobs have no `e`. The indexes are:

```javascript
{ s: 1, r: 1, _id: 1 }
{ e: 1 } // expireAfterSeconds == 0
```

### Publication, association, and revision ordering

An assemble request first queries unique `{o, h}`. If a ready bundle exists, the
application validates its identity, atomically adds missing bindings, and increments
`artifact_revision` for each newly affected project without rebuilding content.

The logical `projects.artifact_revision` is physically `ar`. Absence means zero.
MongoDB stores and atomically increments it as a nonnegative BSON `int64`; values
above `i64::MAX` are invalid.

An upload job is not reported as `ok` until ready metadata, every requested binding,
and every required `ar` increment are acknowledged. A crash can cause a harmless
extra revision increment, but cannot expose a successful response before the binding
is readable.

Removing a project/Release/dist association removes only its matching `b` member and
increments the affected project's `ar`. ADR-0030 defines project fencing and the
resumable cross-store deletion workflow. If no bindings remain, ADR-0031 performs the
generation-fenced shared-object garbage collection rather than deleting the blob in
a race-prone request path.

## Consequences

- One organization stores one immutable blob for identical Source Bundle content.
- Sharing across projects costs a small binding element rather than another archive.
- Modern lookup pays one multikey entry per unique Debug ID, not per internal file.
- Legacy Release/dist lookup remains exact through a same-element `$elemMatch`.
- Chunk-heavy upload jobs pay one Binary header rather than one BSON array element
  header per SHA-1.
- Content remains immutable while project associations can be changed safely.

## Deferred questions

- Maximum bindings and Debug ID tokens before an exceptionally shared bundle must be
  split or projected differently.
- Automatic bounded reprocessing after artifact publication.
