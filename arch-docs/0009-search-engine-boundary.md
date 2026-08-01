# ADR-0009: Search engine boundary with an initial MongoDB implementation

- Status: Accepted
- Date: 2026-07-20
- Extended by: ADR-0048 Unified Query v2

## Context

Issue workflow search and event filtering must work in the simple deployment without
requiring another search process. At the same time, full-text search, arbitrary tags,
facets, and Sentry-like analytics may eventually justify a specialized engine.

The first implementation must remain optimized for ordinary MongoDB queries and must
not promise an abstraction that hides unsupported or unpredictably expensive query
behavior.

## Decision

### Search boundary and dispatch

Web, HTTP API, and MCP call one application-level `SearchService`. They do not build
MongoDB filters or query collections directly.

Search engine dispatch uses an enum rather than `dyn` dispatch. The initial shape is:

```rust
pub enum SearchEngine {
    Mongo(MongoQueryEngine),
}
```

Configuration exposes the engine choice even though only one value is initially
implemented:

```toml
[search]
backend = "mongodb"
```

Adding a future enum variant is allowed, but there is no stable search plugin ABI in
the first version.

### Initial implementation

`MongoQueryEngine` uses ordinary MongoDB Community Server queries and the targeted
indexes accepted in ADR-0008. It does not require or use:

- the separate MongoDB Search `mongot` process;
- MongoDB `$text` indexes on Events; ADR-0024 permits one bounded Issue-title index;
- wildcard indexes over event payload or tags;
- ClickHouse, OpenSearch, Elasticsearch, or another external search database.

ADR-0023 defines the exact first Event-search grammar, compact token projection,
project scope, execution budgets, and indexes. Event full-text search, arbitrary
unconfigured tags, tag facets, and Discover-like analytics are not part of the initial
implementation.

### Query representation

Public search text is parsed into a validated, typed search expression before it
reaches a backend. User input is never interpreted as a MongoDB field path or raw BSON
operator.

The logical representation can express Boolean composition and typed predicates:

```rust
pub enum SearchExpr {
    And(Vec<SearchExpr>),
    Or(Vec<SearchExpr>),
    Not(Box<SearchExpr>),
    Predicate(SearchPredicate),
}
```

The parser, validator, and engine compiler remain separate. This keeps one language
available to Web, API, and MCP and permits a future engine to support more predicates
without changing the public query transport.

### Search projection

ADR-0022 keeps compact level/platform metadata outside the body. ADR-0023 adds only a
bounded array of verified 64-bit exact tokens for environment, release, user ID, and
explicitly configured custom tag pairs. It does not duplicate Event message, title,
culprit, or arbitrary payload structures.

Tag keys and values are data, not BSON query paths. User input is canonicalized into
a typed token predicate and is never appended to a MongoDB field path.

### Capabilities and failure behavior

Every engine reports supported predicates and operations. A query requiring an
unsupported capability returns a structured error such as
`search_capability_unavailable`.

The service must not silently ignore an unsupported clause, silently return partial
semantics, or fall back to an unbounded collection scan. Query time ranges, Boolean
complexity, page size, and execution time are bounded independently of the selected
engine.

### Future engines

A future decision may add another enum variant for MongoDB Search or another engine.
That decision must include deployment topology, consistency, index synchronization,
retention, resource isolation, and conformance tests. Merely adding an enum variant
does not make the current implementation engine-independent.

## Consequences

- The simple installation requires no search component beyond ordinary MongoDB.
- Search behavior stays predictable and cannot unexpectedly scan the full event
  collection for an unsupported predicate.
- Web and MCP observe identical search semantics and capability errors.
- A future engine can extend the typed query language without bypassing application
  authorization or storage boundaries.
- Initial search is intentionally narrower than Sentry Discover and arbitrary-tag
  search.

## Deferred questions

- Conditions that justify implementing another search engine.
- Search over cold Parquet archives.
