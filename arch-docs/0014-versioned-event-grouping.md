# ADR-0014: Versioned deterministic event grouping

- Status: Accepted
- Date: 2026-07-21

## Context

Processor must deterministically assign equivalent event occurrences to an Issue
without a database round trip for ordinary issue lookup. Grouping must remain stable
when native symbols are uploaded later, while still allowing the grouping algorithm
to evolve without silently changing existing projects.

At the target event volume, repeating a verbose grouping-version field or a complete
grouping explanation in every event would create avoidable storage overhead.

## Decision

### Grouper boundary

Grouper is a pure, deterministic Processor component. It receives a normalized event
and the project's pinned grouping revision after any symbolication attempted during
initial processing, and returns one `GroupingKey` and a grouping strategy.

```text
normalized event
    -> select strategy
    -> build canonical typed components
    -> hash components
    -> GroupingKey
    -> deterministic Issue ID
```

Grouping performs no MongoDB queries and has no hidden dependency on the current
time, deployment version, or mutable project state other than the explicit pinned
configuration supplied to it.

### Compact versioned key

The logical key is:

```rust
pub struct GroupingKey {
    pub revision: u16,
    pub digest: [u8; 32],
}
```

MongoDB stores it as one fixed-width 34-byte BSON binary value. The two-byte
revision prefix is followed by a 32-byte BLAKE3 digest. It is exposed through a
domain type rather than passed through the application as an untyped byte slice.

The digest input is a canonical, typed, length-delimited representation with a
strategy domain separator, for example `exception_stack/v1`. JSON serialization,
locale-dependent text, ambiguous string concatenation, and Rust's unstable standard
hashers are not valid canonical encodings.

The Issue identifier is the first 16 bytes of a domain-separated BLAKE3 derivation
from the internal project identifier and the complete versioned grouping key:

```text
issue_id = BLAKE3-128("issue-id/v1" || project_id || grouping_key)
```

MongoDB stores the Issue ID as fixed-width 16-byte binary. The complete 34-byte key
remains on the Issue and is compared on creation, while the shorter ID is repeated in
Events and indexes. Identifier derivation uses canonical length-delimited input, not
literal ambiguous concatenation.

### Strategy priority

Grouper selects the first applicable strategy in this order:

1. `sdk_fingerprint`: honor an explicit fingerprint accepted from a compatible
   Sentry SDK envelope;
2. `exception_stack`: exception type plus a bounded sequence of significant
   application frames;
3. `native_stack`: signal or native exception plus a bounded sequence of module
   debug IDs and module-relative instruction addresses;
4. `message`: logger identity plus a conservatively normalized message when no
   usable stack exists.

The canonicalization rules, frame limit, frame order, path rules, and volatile-value
normalizers are part of the grouping revision and are covered by golden test vectors.

User, environment, timestamp, request ID, tags, release, deployment, severity, and
absolute memory addresses do not affect default grouping. Applications that need
custom grouping use the SDK fingerprint mechanism.

Message normalization may remove recognized UUIDs, request-like identifiers, memory
addresses, and sufficiently random or volatile numeric values. It must not erase all
numbers indiscriminately because values such as HTTP status codes can distinguish
different failures.

### Native stability across symbolication

The first revision groups native frames by module `debug_id` and module-relative
instruction address even when function symbols are available. Symbolicated function,
file, and line data improve display and investigation but do not alter the native
grouping key.

Consequently, uploading a PDB, dSYM, ELF debug file, or Breakpad symbol file and
re-symbolicating an event does not automatically move it to another Issue. A rebuilt
binary normally has a different debug ID and can therefore form another Issue; more
semantic cross-build grouping is deferred.

### Revision pinning and upgrades

Every project has an explicitly pinned `grouping_revision`. A server upgrade does
not change that value for an existing project. A new grouping implementation may be
the default for newly created projects, but an existing project moves to it only
through an explicit operation.

Old grouping implementations and their golden vectors remain available for as long
as retained events or projects depend on them. An unknown revision is an explicit
processing/configuration error rather than permission to use the latest algorithm.

Changing the revision affects newly processed events only unless the user separately
requests a bounded regroup operation. There is no automatic historical regroup.

### Persistence and explanation

Every processed Event stores only its compact Issue ID as grouping-specific data:

```javascript
{
  u: BinData(16) // compact physical field from ADR-0022
}
```

An Issue stores the complete GroupingKey, decoded revision, strategy, representative
event, and a compact human-readable explanation derived from that representative
event. Neither key nor strategy is duplicated into every Event. The canonical Event
body and the preserved implementation for its revision allow the key and a detailed
explanation to be reconstructed when necessary.

### One key in the first version

An Event has exactly one grouping key. Multiple candidate hashes, alias hashes,
automatic Issue merging, and an `issue_group_keys` mapping collection are not part of
the first version. This keeps issue derivation local and avoids an additional MongoDB
lookup in the hot processing path.

A future mapping collection may associate additional keys with an existing Issue
without changing the `GroupingKey` format. Manual merge and semantic cross-build
grouping require a separate decision before implementation.

## Consequences

- Ordinary Issue identity is computed locally and deterministically.
- The grouping revision exists once per Issue rather than being repeated in every
  Event; Events pay only for a 16-byte Issue reference.
- Symbol uploads and derived-frame changes cannot unexpectedly rewrite Issue
  membership.
- Algorithm upgrades are controlled per project and are reproducible.
- New native builds can split an otherwise equivalent failure until cross-build
  semantic grouping is designed.
- Multiple hashes and manual Issue aliases remain unavailable in the first version.

## Deferred questions

- Exact canonical binary component encoding and golden-vector corpus.
- SDK fingerprint edge cases and compatibility vectors for each SDK protocol.
- Explicit bounded regroup workflow and operational limits.
- Multiple grouping hashes, manual Issue merge/split, and alias persistence.
- Semantic native grouping across different build debug IDs.
