# ADR-0022: Compact Event BSON and compressed body codec

- Status: Accepted
- Date: 2026-07-21

## Context

At 100 million Events per day, one repeated byte costs 100 MB of logical BSON per
day before storage compression and replication. Ordinary descriptive BSON field
names, duplicated identifiers, terminal pipeline metadata, grouping data, and a
fully expanded nested SDK payload would consume material disk, cache, network, and
index capacity.

WiredTiger block compression reduces on-disk size, but collection data is expanded in
the WiredTiger internal cache. The Event representation must therefore be compact in
its own right while domain and application code remain readable.

## Decision

### Physical and domain models are separate

Rust domain types use descriptive names. Only the MongoDB Event codec maps them to a
stable compact physical representation. MongoDB field literals are centralized in
the storage module and are not spread through application services.

The normal processed Event document is conceptually:

```javascript
{
  _id,       // 20-byte composite of project ID and Sentry Event ID
  p,         // project ID
  r,         // server received time
  o,         // occurrence time
  x,         // TTL time, when eligible
  h,         // archive due time, only while awaiting archive
  z,         // archive segment ID, only after archive commit
  u,         // 16-byte Issue ID
  l,         // non-default level code
  a,         // platform code
  s,         // non-default PII policy revision
  q,         // processing state, only for pending/retry/failed
  k,         // BSON int64 exact-search tokens selected by ADR-0023
  b          // versioned canonical Event body
}
```

All fields except `_id`, `p`, `r`, `o`, `u`, `a`, and `b` are state- or
value-dependent. Optional values are omitted rather than stored as BSON null.
Storage tests assert both semantic round trips and encoded BSON byte budgets.

### Project and Event identity

Project identity changes from a positive `i63` to a cryptographically random positive
`i31` in `1..=i32::MAX`. MongoDB stores it as BSON `int32`. Project creation retries
on the final unique `_id` collision check. A project ID is routing identity, not a
secret; the DSN key remains the ingest credential.

The Event `_id` is exactly 20 binary bytes:

```text
4-byte big-endian positive project_id || 16-byte Sentry event_id
```

The fixed big-endian representation is canonical across languages. It retains
project-local SDK idempotency and lets the API recover the Sentry Event ID. The Event
does not store another `event_id` field. It still stores `p` because project-prefixed
secondary indexes and scoped queries cannot efficiently index a substring of `_id`.

Organization and user IDs are not repeated in every Event and remain separate from
this project-ID optimization.

### Compact Issue identity and Event grouping reference

The Issue `_id` is the first 16 bytes of the domain-separated BLAKE3 derivation over
the project ID and complete 34-byte versioned GroupingKey. The full GroupingKey,
strategy, and explanation remain on the Issue.

An Event stores only `u`, its 16-byte Issue ID. It does not duplicate the GroupingKey,
grouping revision, strategy, or explanation. Reprocessing can recompute grouping from
the canonical body, and ordinary inspection can resolve grouping metadata through
the Issue.

### Canonical binary body

The complete scrubbed SDK-compatible Event and bounded server-derived detail are
stored once in `b` as BSON generic binary. The body begins with:

```text
byte 0: body format version
byte 1: codec
remaining bytes: encoded body
```

Initial codecs are:

```text
0  canonical UTF-8 JSON
1  Zstandard-compressed canonical UTF-8 JSON
```

The initial body format is JSON because it preserves supported unknown Sentry SDK
fields, has mature Rust serializers, and remains operationally recoverable. A custom
positional format, MessagePack, or CBOR is not foundational before corpus benchmarks
show a meaningful gain after compression.

Compression is adaptive. The writer tries the configured fast Zstandard level and
uses codec 1 only when the complete stored body is at least 64 bytes smaller than
codec 0. The threshold and compression level are configurable within safe bounds.
The uncompressed size remains subject to Event and derived-output limits.

Fresh Events passed from MongoWriter to Dispatcher retain their parsed in-memory
value, so ordinary immediate processing does not decompress the body just written.
Backlog refill and reprocessing decode it through the same versioned codec.

### Pending, processed, and failed bodies

At acceptance, `b` contains the scrubbed accepted SDK representation and `q` marks
the document pending. Processor builds the canonical normalized and derived body in
memory, replaces `b` atomically, sets `u` and searchable metadata, and removes `q`.
The accepted and normalized bodies are never retained as two complete copies.

The compact processing structure is:

```javascript
q: {
  s, // 0 = pending/retry, 1 = permanently failed
  a, // attempts
  n, // next attempt time; pending/retry only
  c  // bounded numeric error code; optional
}
```

The absence of `q` means successfully processed. A permanently failed Event retains
the scrubbed accepted body and a terminal `q`; a successful Event does not repeatedly
store `processed`, attempt count, or processing completion time.

There is no durable `processing` state in the single-process runtime. Local queue and
running sets prevent concurrent work; after a crash, pending work remains eligible.

### Query projection and defaults

Only fields required by accepted filters, ordering, retention, and background work
exist outside `b`.

`l` and `a` are stable numeric enums translated to protocol strings by the domain
codec. Codes are append-only within a physical format version and are never reused
with another meaning. The ordinary Error level is the default and omits `l`.

Title, culprit, exact release, distribution, environment, tags, request, user,
contexts, breadcrumbs, exception frames, native detail, and symbolication detail are
not duplicated as expanded BSON fields. Event detail APIs decode `b`; Issue lists use
the Issue projection.

Exact filters use `k`, the bounded array of domain-separated 64-bit BLAKE3 tokens
stored as BSON `int64` and defined by ADR-0023. Exact source values remain in `b`. A
token hit is a candidate and is verified against the decoded value before it is
returned, preserving correctness in the theoretical collision case.

Arbitrary fields inside `b` are never translated directly into MongoDB query paths.
Full-text message search is not implicitly provided by the body codec; its bounded
projection or alternate engine is a separate Search decision.

### Schema evolution

Absence of the outer `v` field means physical BSON schema version 1. A future
incompatible outer layout may set a short `v` code, so version 1 does not pay a
per-document version cost. Body version is always available in the first header byte.

MongoDB collection validators, migration metadata, Rust codecs, golden fixtures, and
byte-size tests define the schema outside each document. Readers support explicitly
listed older versions; they never interpret an unknown version as the newest one.

### Retention and archive fields

When cold archive is disabled, terminal Events use only `x` as the absolute TTL date.
Pending Events have no `x`.

When archive is enabled, `h` is present while a terminal Event awaits archival and
`x` is absent. After a verified archive commit, `z` identifies the archive segment,
`x` becomes the allowed hot-copy expiry, and `h` is removed. This avoids a nested
retention object and never stores both an ordinary hot deadline and unused archive
state for installations without archive.

### Collection compression and indexes

Per-body compression complements rather than assumes WiredTiger block compression:
the `b` value remains compressed in MongoDB's otherwise uncompressed collection
cache. The event collection's Snappy versus Zstandard block compressor is selected by
benchmark because the body is already incompressible while the BSON envelope still
benefits from block compression.

Indexes use physical field names inside the MongoDB adapter. Documentation may show
logical names for readability but must map them to physical keys in migrations.
Wildcard indexes and speculative indexes are prohibited. Covered query plans should
avoid fetching `b` for list operations that need only indexed projection fields.

The Event collection is not initially clustered. Its random 20-byte `_id` can reduce
clustered insert locality and enlarge every secondary index; this requires a separate
benchmark before accepting the missing standalone `_id` index as a net win.

## Consequences

- Descriptive Rust code does not impose descriptive field-name cost on every Event.
- Event ID, terminal pipeline state, grouping metadata, and default values are not
  duplicated.
- The large SDK payload remains compressed in WiredTiger cache and on the network.
- MongoDB can query only deliberately projected fields, which keeps index cost
  explicit but prevents arbitrary searches inside the body.
- Event-detail reads and backlog recovery pay decompression and JSON decode CPU.
- Body and BSON versions require permanent golden compatibility fixtures.
- Changing accepted earlier project and Issue identifier widths requires no migration
  because implementation and production data do not yet exist.

## Deferred questions

- Corpus benchmark of JSON, MessagePack, CBOR, and trained Zstandard dictionaries.
- Per-collection Snappy versus Zstandard results under ingest and retention load.
- Final numeric level/platform/error code registries.
- Exact canonical JSON rules and forward-compatible unknown-field fixtures.
- Whether a later sequential clustered key can outperform the composite Event ID.
