# ADR-0024: Compact Issue BSON and title search

- Status: Accepted
- Date: 2026-07-21

## Context

Issues are less numerous than Events, but every processed Event updates its owning
Issue. The hot update must not decode and rewrite a complete compressed document, and
frequently queried Issue-list fields must remain directly indexable. At the same
time, verbose workflow fields, duplicated Event references, grouping metadata, and
notification state should not be repeated unnecessarily.

ADR-0023 also reserves first-version full-text search for the smaller Issue
projection rather than indexing every Event message.

## Decision

### Physical representation

As with Events, Rust domain types retain descriptive names and the MongoDB adapter
alone owns short physical field names. The version-one Issue is conceptually:

```javascript
{
  _id, // 16-byte Issue ID
  p,   // project ID
  g,   // complete 34-byte GroupingKey
  t,   // title
  q,   // culprit, optional
  f,   // first seen
  l,   // last seen
  e,   // first Sentry Event ID
  v,   // latest Event ID when different from e
  r,   // representative Event ID when different from latest
  c,   // approximate occurrence count
  s,   // non-open status, optional
  a,   // assignee, optional
  w,   // current non-open workflow transition, optional
  d,   // regression summary, optional
  fr,  // first release, optional
  lr,  // last release when different from fr
  m,   // latest occurrence has no release while fr is present, optional true
  j,   // notification-outbox-ready flag, transient
  n,   // compact notification transitions, transient
  b    // versioned grouping detail
}
```

Issue schema version 1 has no separate outer schema-version field; `v` is reserved
exclusively for the optional latest Event ID described below. The Issue-specific body
in `b` carries its own format version. BSON null is forbidden in optional physical
fields; absence and default values have precise meanings. Collection validators and
byte-size golden tests enforce the codec.

### Identity and grouping

`_id` is the 16-byte Issue ID defined by ADR-0014. `p` is the positive BSON `int32`
project ID required by scoped indexes. `g` is the complete 34-byte GroupingKey: its
first two bytes already contain grouping revision, so revision is not repeated.

Issue creation compares stored `g` with the complete computed key, making a
theoretical truncated-ID collision detectable.

Grouping strategy, bounded explanation, and reconstruction detail live in `b`. Its
header and adaptive raw-JSON/Zstandard codec follow the versioned pattern from
ADR-0022, but it is an Issue-specific body format. `b` changes only on Issue creation,
representative change, explicit regrouping, or explanation-format migration. An
ordinary occurrence never rewrites it.

### Display summary

`t` is the directly readable Issue title and is mandatory. It is valid UTF-8 and
bounded to 512 encoded bytes after deterministic title construction. `q` is an
optional culprit bounded to 256 encoded bytes. Neither may contain an unbounded
message or stack trace.

Title remains stable after creation unless the representative Event deliberately
changes. Ordinary occurrences therefore do not rewrite the text index.

### Event references

Since `p` is already known, `e`, `v`, and `r` contain only the 16-byte Sentry Event ID,
not the 20-byte project/Event composite.

Default rules are:

```text
e is always the first Event ID
v absent  => latest Event ID equals e
r absent  => representative Event ID equals latest Event ID
```

`v` appears after a different latest occurrence. `r` appears only when an algorithm
or user pins a representative other than the current latest occurrence.

### Hot timestamps and count

`f` and `l` are always BSON UTC datetimes. Although equal at creation, both are
stored because `l` is the principal Issue timeline and index sort key.

`c` is always BSON `int64` and follows the approximate occurrence-count contract of
ADR-0005. Treating missing `c` as one would make the hottest `$inc` require a more
complex update pipeline, so its small fixed cost is accepted.

### Status, workflow, and assignment

Status codes are:

```text
s absent  open
s = 1     resolved
s = 2     ignored
```

Collection validation prohibits explicit null. The current non-open transition is:

```javascript
w: {
  t, // transition time
  a  // compact ActorRef
}
```

For `resolved`, `w` represents `resolved_at/resolved_by`; for `ignored`, it represents
`ignored_at/ignored_by`. Returning to open removes `s` and `w`. Historical changes
remain in `issue_activities`, so the Issue does not retain parallel resolved,
ignored, and status-changed fields.

ActorRef is a canonical binary value containing a one-byte actor kind and the compact
user, API credential, or system identifier. `a` outside `w` is the current optional
assignee; teams remain deferred.

### Regression summary

Only an Issue that has regressed contains:

```javascript
d: {
  t, // last regression time
  e, // 16-byte last regression Event ID
  c  // regression count when greater than one
}
```

Absence of `d` means no regression. Presence of `d` with no `d.c` means exactly one
regression. A later regression writes the explicit count. The atomic transition rules
and receipt-time clock remain those of ADR-0015.

### Release pairs

`fr` contains the exact first-seen release when present. `lr` is absent while the
last-seen release equals `fr`, and appears only after it differs. If `fr` is absent,
the Issue has no first release. `m: true` appears only when `fr` is present and the
latest occurrence has no release; it distinguishes that state from an absent `lr`
that defaults to `fr`. `lr` and `m` are mutually exclusive. Exact strings are retained
because Issues are much less numerous than Events and catalog lookups would
complicate hot updates.

### Compact notification outbox

The durable Issue-owned notification transition is:

```javascript
n: [{
  i, // 16-byte transition ID
  k, // numeric new_issue or regression kind
  e, // 16-byte Event ID
  t  // creation time
}]
```

`j: true` exists exactly while `n` is non-empty and drives a small global partial
index. After Dispatcher expands the final transition, both `j` and `n` are removed.
The ordinary stable Issue therefore pays no outbox bytes.

Transition IDs use a domain-separated BLAKE3-128 derivation over project, Issue,
transition kind, and Event ID. The complete inputs make retry derivation deterministic.

### Indexes

The initial Issue indexes use physical keys:

```javascript
{ p: 1, l: -1, _id: -1 }
{ p: 1, s: 1, l: -1, _id: -1 }
{ j: 1, _id: 1 } // partial on j == true
```

Open status is queried as missing/null under the invariant that explicit null is
invalid. Assignment, culprit, release, and regression indexes are not created before
an accepted query contract justifies them.

### Issue title full text

The only initial MongoDB text index is:

```javascript
{ p: 1, t: "text" }
```

It is created with simple collation, `default_language: "none"`, and title weight
one. Project equality is mandatory for every `$text` query, satisfying the compound
text-index prefix rule. Language `none` retains stop words and disables suffix
stemming, which is more predictable for exception types, function names, error
codes, and mixed-language titles.

Title search matches terms and quoted phrases, not arbitrary substrings. It sorts by
text score with deterministic Issue ID tie-breaking, has the common Search timeout,
and returns at most 100 candidates/results. It does not promise deep cursor
pagination or last-seen index ordering because MongoDB text indexes cannot provide
that sort or a covered query.

Trigrams, regular-expression scans, Event message duplication, and another text index
are not added. A future SearchEngine may replace these semantics through explicit
capabilities.

## Consequences

- Ordinary occurrence processing updates only compact hot scalar fields.
- Grouping revision, workflow timestamps, and Event project prefixes are not
  duplicated.
- Open, never-regressed, unassigned Issues omit nearly all workflow fields.
- Issue lists read title and summary without decompressing `b`.
- Useful title term search is available without indexing every Event message.
- MongoDB text-search limitations are an explicit product capability rather than a
  hidden approximation of substring or relevance search.

## Deferred questions

- Assignment/team semantics and indexes.
- Title search behavior in a future alternate SearchEngine.
- Bounded title regeneration when representative selection changes.
- Exact `ActorRef`, numeric strategy, transition-kind, and status codec registries.
- Issue-activity retention and compact physical schema.
