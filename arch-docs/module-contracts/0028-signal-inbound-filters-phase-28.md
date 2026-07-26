# Module contract 0028: Signal Inbound Filters

- Status: Accepted for Phase 28
- Owner: `domain::inbound_filter` with typed adapters in `application::ingest`
- Architecture: ADR-0039, ADR-0040 and ADR-0045

## Boundary

The module evaluates a project policy after the bounded parse, scrub and normalization
needed to expose an accepted typed field, and before the first durable side effect for
that signal.

```text
project.resolve(DSN)
-> Arc<CompiledInboundFilterPolicy> from the revision-safe project cache
-> typed Error, Log, Transaction or Span fields
-> match
   -> handled discard and bounded aggregate outcome
   -> or the existing signal-specific durability lane
```

The project service owns policy loading, compilation lifetime and cache invalidation.
It does not interpret signal payloads. Error, Log and Span adapters expose only the
fields accepted for their own signal. Transactions use the Span lane with a distinct
`transaction` target for root records; child records use `span`.

## Bounded policy

- at most 32 rules per project;
- at most 256 UTF-8 bytes per non-empty pattern;
- at most 8 KiB of compiled matcher state;
- accepted operations are `exact`, `prefix`, `suffix`, `contains` and `glob`;
- glob supports `*` and `?`, has no escaping or regular-expression expansion, and
  matches UTF-8 bytes case-sensitively;
- duration is an exact signed integer number of milliseconds;
- user regular expressions and executable rules are rejected.

Exact/prefix/suffix are linear in the inspected prefix or suffix. Contains uses a
compile-once KMP prefix table and is `O(candidate bytes + pattern bytes)`. Glob uses a
bounded non-recursive wildcard machine with constant memory; because pattern size is
hard-capped at 256 bytes, candidate evaluation is linear in candidate bytes under the
accepted policy bound. Matching allocates no heap memory.

## Durable and privacy contract

For Error Events the adapter filters the scrubbed payload before Event-owned
attachments are opened or committed and before `EventSink::persist`. For Logs and
Spans the adapter filters normalized records before `LogSink::persist_logs` or
`SpanSink::persist_spans`.

A filtered body, attachment and derived signal record must not enter MongoDB,
BlobStore, tracing fields, diagnostics or application logs. The handled response may
return the Error Event ID but has no durable outcome. Aggregate metrics contain only
the bounded signal and matched field:

```text
metric_inbound_filtered_total{signal,reason}
```

The generic outcome is `Filtered` with the static reason `inbound_filter`.

## Policy persistence and schema

The declarative rules are stored as `policy.inbound_filters` in the shared `projects`
collection. No collection or index is added. MongoDB schema generation advances from
9 to 10 because the strict project validator changes. This is an empty-schema
bootstrap generation, not an online migration.

The Mongo adapter validates and compiles persisted rules into independent Error, Log,
Transaction and Span slices while producing `ProjectSnapshot`. The snapshot holds
`Arc<CompiledInboundFilterPolicy>`, so a cache hit clones only the `Arc`, and a signal
never scans another signal's rules. Policy mutation uses the existing
expected-revision update and invalidates every DSN cache entry for the project.

## Errors and cancellation

Invalid targets, unavailable fields, operations, durations, patterns, rule counts or
compiled sizes are rejected by the native API as `400 invalid_request`. Invalid
persisted policy is treated as `ProjectStoreError::InvalidData` and fails closed.

Evaluation is synchronous, bounded and has no await point. Existing request
cancellation, request timeout, storage admission and graceful-shutdown contracts are
unchanged.

## Tests

- exhaustive small-alphabet glob comparison against a recursive reference;
- exact/prefix/suffix/contains/glob and typed-field validation tests;
- HTTP ingest E2E proving filtered Error attachments, Events, Logs and Spans create no
  Event, signal or BlobStore object;
- real MongoDB project policy round-trip and compiled snapshot match;
- native API DTO validation and Web type/lint/build tests;
- pinned real Node SDK accepted/filtered Error, Log and Span compatibility row;
- explicit release-mode worst-case matcher RPS baseline.
