# ADR-0023: MongoDB Search v1 and compact Event tokens

- Status: Accepted
- Date: 2026-07-21

## Context

ADR-0022 deliberately places the complete Event in compressed binary field `b`.
MongoDB cannot query inside that body, and copying every tag, message, context, and
user value into expanded BSON or indexes would defeat the storage design. At the same
time, the first deployment must provide useful predictable search through ordinary
MongoDB Community Server without another search process.

Search v1 therefore needs an explicit small capability set, compact projections, and
hard execution budgets. Unsupported queries must fail visibly instead of causing an
unbounded collection scan.

## Decision

### Scope and query paths

Event search is always scoped to one authorized project. Organization-wide and
cross-project Event search are deferred. Four paths exist:

1. exact Event ID lookup through the 20-byte composite Event `_id`;
2. project Event timeline through `(p, o, _id)`;
3. one Issue's Event timeline through `(p, u, o, _id)`;
4. structured exact filtering through compact search tokens in `k`.

Exact Event ID and direct Issue lookup do not require a time range. All ordinary
timeline and token searches have an explicit or default time range.

### Public grammar

The shared parser accepts a bounded Sentry-like expression language:

```text
field:value
field:"value with spaces"
!field:value
(expression OR expression)
expression expression              implicit AND
timestamp:>=2026-07-20T00:00:00Z
timestamp:<2026-07-21T00:00:00Z
```

Initial Event predicates are:

```text
event.id
issue
timestamp
level
platform
environment
release
user.id
<configured-custom-tag>
```

Known built-in fields always win name resolution. An unknown field name is treated as
a custom tag only when it is active in the project's indexed-tag allowlist. Otherwise
the parser/validator returns `search_field_not_indexed`.

Bare free text, arbitrary payload paths, numeric Discover expressions, aggregations,
facets, `has:custom_tag`, relevance sorting, and Event-message full text are not
supported. They return a structured capability error and are never ignored.

### Exact token representation

`k` is an array of BSON `int64` values. A token is the first 64 bits of a
domain-separated BLAKE3 digest over a canonical length-delimited input. The bit
pattern is stored through the corresponding signed BSON `int64`; signedness has no
semantic meaning.

Token domains are distinct, for example:

```text
search/environment/v1
search/release/v1
search/user-id/v1
search/tag-pair/v1
```

Release, environment, tag key, and tag value identity remains exact and
case-sensitive unless the relevant protocol normalization decision explicitly says
otherwise. The canonical source value remains only inside `b`.

A token match produces candidates. Before returning a candidate, SearchService
decodes `b` and compares the exact typed source value. This makes a theoretical
64-bit collision a performance event rather than an incorrect result.

### Token policy and cardinality

The default token projection is:

```toml
[search.mongo]
indexed_fields = ["environment", "release", "user.id"]
indexed_tags = []
max_indexed_tags = 8
max_tokens_per_event = 16
```

Missing values create no token. A normalized tag pair creates one token; a separate
tag-key token is not stored. Consequently `has:custom_tag` is not available.

Custom tags must be explicitly configured per project. Configuration validation
limits the allowlist to eight distinct keys and ensures the maximum token projection
cannot exceed sixteen values. Built-in tokens take deterministic priority over custom
tokens if a future projection revision approaches the limit.

Adding an indexed tag to a project is a two-phase operation. New Events receive the
token while Scheduler performs a bounded retained-Event backfill. The predicate is
reported as `building` and cannot be queried until a watermark proves the projection
complete. Removing a tag disables the predicate immediately; obsolete tokens may be
removed by bounded background cleanup or disappear with Event retention.

### MongoDB indexes

Search v1 adds one partial multikey Event index:

```javascript
// Partial on k existing and non-empty
{ p: 1, k: 1, o: -1, _id: -1 }
```

It retains the already accepted timeline indexes:

```javascript
{ p: 1, o: -1, _id: -1 }
{ p: 1, u: 1, o: -1, _id: -1 }
```

No Event index is added solely for `l` or `a` initially. Level and platform are
compact outer fields and can be applied while checking bounded candidates. A future
index requires measured query selectivity and storage cost.

For a conjunction containing several token predicates, the compiler chooses a
positive token anchor and verifies remaining predicates against the compact metadata
or decoded body. It may use a proven MongoDB intersection plan, but correctness and
budgets do not depend on the optimizer intersecting multikey bounds. Each OR branch
must have a usable project/time or token anchor.

### Boolean and execution limits

Implicit AND, explicit OR, NOT, quoted values, and parentheses are supported within
these configurable defaults:

```toml
[search.limits]
max_query_bytes = 4096
max_predicates = 16
max_or_branches = 8
max_nesting = 4
default_time_range = "24h"
max_time_range = "30d"
default_page_size = 50
max_page_size = 100
max_candidates = 10000
timeout = "2s"
```

Limits may be lowered by deployment or project policy. The effective time range also
cannot exceed data actually retained in hot MongoDB.

NOT is a post-filter and must be combined with a positive project/time or token
anchor. Search stops after the candidate or time budget. If it cannot fill a page
within that budget, it returns `search_too_broad` with machine-readable narrowing
hints; it does not return an unlabeled partial page.

### Pagination and sorting

Search v1 supports only newest-first ordering by `(o DESC, _id DESC)`. It uses
keyset/cursor pagination and never MongoDB `skip` for deep pages.

The opaque versioned cursor contains the last sort tuple and a digest of the
authorized project, normalized query, and effective ordering. A cursor from another
query cannot silently change semantics. Default page size is 50 and maximum is 100.

### Full text

Event `message` is not copied into an outer `m` field. Events do not receive a
MongoDB text index or n-gram tokens. N-grams would multiply multikey index entries,
while a duplicated bounded message and text index would materially increase every
Event's storage and write amplification.

The first product full-text capability is the bounded Issue-title text index defined
by ADR-0024. Event detail still returns the complete message after decoding `b`. A
future external SearchEngine may add Event full text, but only through a separate
topology, consistency, retention, and conformance decision.

### Shared boundary and observability

Web, API, and MCP use the typed SearchService from ADR-0009. Raw MongoDB operators,
field paths, regular expressions, and sort documents never enter through public query
text.

Search emits bounded metrics for selected path/index, candidates examined, bodies
decoded, timeout, too-broad rejection, projection-building state, and collision
verification failures. Production-shaped tests record `explain("executionStats")`
baselines for every accepted query shape.

## Consequences

- Useful exact Event search works with one optional multikey index and no external
  search process.
- Arbitrary tag and message indexing cannot silently multiply storage.
- Hash truncation reduces BSON and index keys without sacrificing result correctness.
- Some broad queries require a narrower time range or explicitly indexed field.
- Adding a searchable custom tag requires an observable retained-data backfill.
- Full-text Event search remains unavailable until its storage and operational cost
  is deliberately accepted.

## Deferred questions

- Approximate facets and token-frequency statistics.
- Organization-wide and cross-project Event search.
- Conditions and conformance suite for another SearchEngine.
- Search over cold Parquet archives.
