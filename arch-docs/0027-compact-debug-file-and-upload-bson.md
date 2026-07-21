# ADR-0027: Compact debug-file and upload BSON

- Status: Accepted
- Date: 2026-07-21

## Context

ADR-0025 introduces one durable whole-file assembly job instead of per-chunk MongoDB
documents. ADR-0026 introduces indexed project-private lookup by debug ID and code ID.
Their conceptual metadata still contains verbose names, string identifiers, repeated
format fields, and an ordered array whose BSON representation would repeat element
headers for every SHA-1 chunk.

Debug-file records are much less numerous than Events, but their lookup indexes are
latency-sensitive and upload manifests can contain hundreds of chunks. The physical
codec should be compact without hiding fields needed by Symbolicator behind a
compressed document.

## Decision

### Codec ownership

Rust domain types keep descriptive field and enum names. Only the MongoDB adapter owns
the physical one-character keys, binary subtypes, bit layout, default omission rules,
and migrations defined here. BSON null is forbidden in both collections.

Collection validators, round-trip property tests, malformed-codec tests, and golden
BSON byte-size tests protect the physical representation. Raw in-memory bytes of a
Rust or third-party type are never persisted as a stable format.

### Canonical DebugId binary

`DebugId` is stored as BSON Binary with the application-reserved DebugId subtype. The
payload length is its canonical variant discriminator:

```text
16 bytes  UUID; appendix is zero
20 bytes  UUID followed by big-endian u32 appendix
 8 bytes  PDB 2.0 big-endian u32 timestamp followed by big-endian u32 age
```

A nonzero appendix must use the 20-byte form; a zero appendix must use the 16-byte
form. This preserves PDB age and PDB 2.0 identity while avoiding the 33--40 byte text
form and the version-dependent 32-byte in-memory `debugid::DebugId` representation.

Nil or malformed Debug IDs are rejected or omitted according to the caller contract;
they never receive an alternate noncanonical encoding.

### Canonical CodeId binary

`CodeId` is canonical lowercase hexadecimal in the domain and packed into BSON Binary
with the application-reserved CodeId subtype:

```text
payload[0] bit 0  0 for an even, 1 for an odd number of hex digits
payload[0] bits 1..7 must be zero
payload[1..]      two hexadecimal digits per byte, high nibble first
```

For an odd digit count, the unused low nibble in the final byte must be zero. Payload
length plus the parity bit reconstructs the exact canonical digit count. Empty,
non-hexadecimal, over-limit, noncanonical-case, flag, and padding representations are
invalid.

### Ready debug-file document

`debug_files` contains only ready, immutable files:

```javascript
{
  _id, // 16-byte deterministic DebugFileId
  p,   // project ID, BSON int32
  d,   // canonical DebugId binary, optional
  c,   // canonical CodeId binary, optional
  y,   // type, architecture, features, and codec revision
  x,   // complete BLAKE3, 32 binary bytes
  h,   // sentry-cli complete SHA-1, 20 binary bytes
  z,   // original size, BSON int64
  n,   // sanitized original basename
  u    // upload completion time, BSON UTC datetime
}
```

At least one usable debug or code identifier is required. `n` is valid UTF-8, contains
only a basename rather than an uploader path, and is bounded to 255 encoded bytes.
`z` is nonnegative and within the configured upload limit.

There are no organization, BlobStore key, status, object-format, file-kind,
architecture, feature-array, last-used, or explicit schema-version fields.
Organization is derived from the authorized project. Only ready records exist;
uploading and failure state belongs to `debug_uploads`. Exact use tracking is omitted
to avoid symbolication write amplification.

### Packed metadata word

`y` is a nonnegative BSON `int32` with this stable bit allocation:

```text
bits  0..5   symbol/file type code
bits  6..11  architecture code; zero means unknown
bits 12..19  feature flags
bits 20..23  metadata codec revision
bits 24..30  reserved and zero
bit  31      zero
```

Initial feature bits represent symbol table, debug information, unwind information,
and embedded/source-bundle availability. Numeric type and architecture registries are
append-only. An unsupported future value requires a new codec revision or explicit
escape representation rather than reinterpreting an existing code.

The compatible `symbolType`, `cpuName`, response headers, and `data.features` are
derived from `y`; their strings are not stored.

### File identity and final object key

The immutable ID is:

```text
DebugFileId = first_128_bits(BLAKE3(
    "debug-file-id-v1" || project_id_be_u32 || complete_blake3
))
```

An existing `_id` is accepted as an idempotent duplicate only after comparing `p`,
the complete 32-byte `x`, size, and parsed metadata. A mismatch is a collision error.
The same bytes in another project intentionally receive a different ID and object.

The final physical BlobStore key is reconstructed and is not stored in BSON:

```text
d/{project_id_base36}/{debug_file_id_base64url_no_pad}
```

The full BLAKE3 in `x` remains the persistent end-to-end integrity value even though
the shorter DebugFileId appears in the key.

### Ready debug-file indexes

In addition to `_id`, the accepted indexes are:

```javascript
// Exact private candidates; partial on d existing
{ p: 1, d: 1, u: -1, _id: -1 }

// Exact private candidates; partial on c existing
{ p: 1, c: 1, u: -1, _id: -1 }

// Permanent sentry-cli checksum idempotency
{ p: 1, h: 1 } // unique

// Project debug-file listing
{ p: 1, u: -1, _id: -1 }
```

The SHA-1 uniqueness is a compatibility identity inside one project. BLAKE3 and
parsed metadata remain authoritative internal integrity checks.

### Assembly-job document

The transient `debug_uploads` shape is:

```javascript
{
  _id, // 16-byte deterministic ID from project and complete SHA-1
  p,   // project ID, int32
  o,   // organization ID, int64; required for temporary chunk keys
  h,   // complete SHA-1, 20 binary bytes
  n,   // sanitized basename
  d,   // uploader DebugId hint in canonical binary, optional
  c,   // ordered concatenated 20-byte chunk SHA-1 values
  s,   // non-pending state, optional
  a,   // retry attempt when nonzero
  r,   // next retry time, optional
  t,   // creation time
  u,   // update time when different from t
  f,   // final 16-byte DebugFileId on completion
  e,   // terminal absolute expiration time
  q    // stable numeric permanent error code, optional
}
```

The ID is the first 128 bits of a domain-separated BLAKE3 over project ID and the
complete 20-byte SHA-1. Stored inputs are compared on an existing ID. The organization
ID is retained in this short-lived document so a recovered worker can address
organization-scoped temporary chunks without another control-plane lookup.

State codes are:

```text
s absent  pending
s = 1     assembling
s = 2     complete
s = 3     permanently failed
```

`a` is absent for zero. `u` is absent while equal to `t`. `r`, `f`, `e`, and `q`
exist only in states that use them. Temporary failures return to pending and use `r`;
they are not encoded as permanent failure.

The uploader-provided `d` is a hint. The assembled-file parser is authoritative and
rejects an incompatible hint rather than publishing incorrect metadata.

### Packed chunk manifest

`c` is one generic BSON Binary payload:

```text
chunk_sha1[0] || chunk_sha1[1] || ... || chunk_sha1[n-1]
```

Its length must be a nonzero multiple of 20 and the count must not exceed the limit
implied by configured maximum file and chunk sizes. Order and repeated checksums are
preserved. No count or offsets are stored because both follow from the payload length.

At the initial 2 GiB/8 MiB limits, at most 256 hashes occupy exactly 5,120 payload
bytes. This avoids a BSON array's type byte, decimal index key, length, and subtype for
every element, and halves the checksum payload relative to hexadecimal strings.

### Job recovery, idempotency, and expiration

An assemble request first queries the unique ready-file `{p,h}` index. A hit returns
`ok` even if its old assembly job has expired. Otherwise `_id` locates the one active
or recent job.

The initial job indexes are:

```javascript
// Pending/retry recovery and a small assembling-state prefix
{ s: 1, r: 1, _id: 1 }

// Absolute terminal deletion; expireAfterSeconds == 0
{ e: 1 }
```

Pending and assembling jobs have no `e`. Completed jobs receive a configurable short
expiration, initially 24 hours. Permanently failed jobs initially remain seven days
for diagnostics. Scheduler classifies stale assembling jobs before terminal TTL.

### Project revision representation

The logical `projects.debug_files_revision` from ADR-0026 is physically `dr`.
Absence means zero. Rust may expose a bounded unsigned newtype, but MongoDB stores and
atomically increments a nonnegative BSON `int64`; values above `i64::MAX` are invalid.

## Consequences

- Hot private-source lookups stay directly indexable without decoding a body.
- Debug IDs preserve PDB age while avoiding strings and library-memory coupling.
- One `int32` replaces several repeated metadata fields.
- A 256-chunk manifest pays one BSON binary header rather than 256 array-element
  headers.
- Ready-file documents have no mutable status or use-tracking write amplification.
- Completed upload jobs may expire without losing permanent `sentry-cli` idempotency.

## Deferred questions

- Compact schemas for future source bundles, ProGuard, BCSymbolMap, and IL2CPP files.
- Measured candidate-index and manifest compression behavior under production data.
- Multi-process deletion fencing if the single-process ADR-0033 boundary is removed.
