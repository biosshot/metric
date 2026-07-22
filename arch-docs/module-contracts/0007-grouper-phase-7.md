# Phase 7 contract: versioned deterministic Grouper

- Status: accepted for implementation
- Date: 2026-07-22
- Owners: `domain::grouping`

## Responsibilities and exclusions

Grouper is a pure deterministic domain algorithm. It consumes a normalized Event,
the optional backend-independent symbolication result, the project ID, and the
project's explicitly pinned grouping revision. It selects one strategy, builds a
canonical typed length-delimited component stream, derives one 34-byte GroupingKey,
derives one 16-byte Issue ID, and returns bounded selected components plus a compact
explanation.

Grouper performs no storage, network, clock, randomness, configuration lookup,
Issue mutation, title persistence, regroup scheduling, or backend-specific work.
Phase 7 does not implement IssueService, BSON codecs, indexes, aliases, merge/split,
or historical regrouping.

## Revision 1 and strategy order

Revision 1 is registered explicitly and remains available while referenced. Unknown
or zero revisions are errors and never select the latest implementation implicitly.
Its first-applicable strategy order is:

1. exact ordered SDK fingerprint;
2. exception type plus up to eight significant application frames, newest first;
3. native exception/signal plus up to eight module debug identities and
   module-relative instruction addresses, newest first;
4. logger plus conservatively normalized message.

`{{ default }}` inside an SDK fingerprint embeds the selected default strategy and
its complete default digest as a typed component. User, environment, timestamp,
request, tags, release, dist, severity, and absolute addresses never enter default
components.

## Canonical encoding and identities

Every component has an append-only numeric kind, a four-byte big-endian length, and
UTF-8 or fixed binary value. The stream starts with a length-delimited strategy
domain separator containing the revision. JSON, locale formatting, concatenated
strings, map iteration order, and standard-library hashers are forbidden.

GroupingKey is `revision_be_u16 || BLAKE3-256(component_stream)`. Issue ID is the
first 16 bytes of a separate BLAKE3 derivation over a length-delimited `issue-id/v1`
domain, project ID, and the complete 34-byte GroupingKey. Stored Issue creation must
compare the complete key; helpers detect an ID/key mismatch.

## Stability and bounds

Native components use debug ID (code ID only as a fallback) plus parsed
module-relative address. Symbolicated names/files/lines never affect native identity.
Absolute addresses without a containing identified module are unusable and cannot
be hashed directly. JavaScript/ordinary exception grouping may use valid mapped
derived frames; raw frames remain the fallback.

Frame, fingerprint, component, message, path, logger, exception and explanation
bytes have fixed revision-owned limits. Message normalization replaces recognized
UUIDs, long hexadecimal addresses, long numeric/request-like tokens, folds ASCII
case and whitespace, while preserving short meaningful numbers such as HTTP status
codes. Empty Error Events receive one explicit bounded message component rather than
an unbounded or nondeterministic fallback.

## Operability and verification

Safe metrics contain only revision, strategy, outcome and duration. Component values,
messages, paths, debug IDs, project/Event/Issue IDs and digests are forbidden labels.

Required verification includes exact key/Issue-ID golden vectors across SDK and
platform strategies, canonical encoding properties, ignored-field merge and semantic
separation regressions, native stability across derived-symbol changes, SDK default
placeholder behavior, unknown-version and corrupt-key fixtures, full-key/Issue-ID
verification, and one CPU/output-size RPS baseline with no adapter dependency.
