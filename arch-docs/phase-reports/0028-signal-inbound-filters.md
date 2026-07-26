# Phase 28 report: Signal Inbound Filters

- Date: 2026-07-27
- Result: complete
- Module contract: `module-contracts/0028-signal-inbound-filters-phase-28.md`

## Delivered

- Project policy now owns at most 32 typed Error, Log, Transaction or Span rules with
  exact, prefix, suffix, contains and bounded glob operations.
- Rules compile once on a Mongo/project-cache miss. `ProjectSnapshot` shares the
  immutable compiled matcher through `Arc`, so cache hits do not recompile or clone
  pattern state.
- Error matching runs after mandatory scrub and before Event-owned attachment or
  Event persistence.
- Log and Span matching runs on typed normalized records before the independent
  `LogSink` and `SpanSink` writer lanes.
- Transaction root records use the Transaction target; transaction children and
  streamed/standalone records use the Span target.
- Filtered items return a handled outcome with no durable outcome and emit only
  bounded `signal` and `reason` metrics. Payload values never enter metrics or logs.
- The native project-policy API and responsive Web project settings expose the same
  bounded rule set through custom selects and explicit validation guidance.
- MongoDB schema generation advances from 9 to 10 for the strict project-policy
  validator. No collection, index or online migration was introduced.

## Exit gate

| Gate | Evidence |
| --- | --- |
| No-policy regression equivalence | Empty compiled policy is the default; full workspace suite passed and the pinned Node SDK accepted Error/Log/Span row passed before the filtered row. |
| Deterministic matcher tests and complexity | Exact/prefix/suffix/contains/glob tests plus exhaustive small-alphabet glob/reference comparison passed. KMP contains and bounded non-recursive glob complexity are declared in the module contract. |
| Before signal/Blob durable effects | HTTP E2E sent Error + attachment + Log + Transaction + child Span and observed empty Event, Log, Span and Blob stores. |
| Revision-safe cache invalidation | Existing generation-fenced project-cache invalidation suite passed; compiled policy is an `Arc` inside the invalidated snapshot. |
| No filtered bodies in storage/logs/diagnostics | E2E observed no durable objects; metrics use only closed signal/field enums and the generic outcome reason is static. |
| Signal isolation | Typed field validation rejects unavailable fields; Error, Log and Span adapters remain separate and existing writer lanes are unchanged. |
| CPU/allocation budget | Release worst-case 32-rule/8,192-byte non-match baseline passed at **3,651 RPS**, above the 1,000 RPS gate. Matching uses borrowed fields and performs no hot-path heap allocation. |
| Real SDK accepted and filtered rows | Pinned `@sentry/node` 10.66.0 `send-signals.mjs` row passed once with durable Error/Logs/Spans and once with all three signal classes filtered before storage. |

## Verification

```text
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
cargo test -p metric-mongo --test project_identity \
  infrastructure_project_identity_schema_uniqueness_and_authorization -- --ignored
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_node_sdk_filters_errors_logs_and_spans_before_storage -- --ignored
cargo test -p metric-domain --release performance_worst_case_policy_rps \
  -- --ignored --nocapture
npm run lint
npm test -- --run
npm run build
```

Results:

- workspace application tests: 95 passed, 9 ignored;
- domain tests: 38 passed, 2 ignored;
- server tests: 36 passed; ingest E2E 19 passed;
- real MongoDB policy/schema integration: passed against the configured local MongoDB;
- real Node SDK accepted/filtered compatibility row: passed;
- Web: 23 tests passed, including the Project settings rule editor; lint and
  production build passed;
- performance: 10,000 iterations in 2,739 ms, 3,651 RPS.

The first workspace run exceeded the tool call deadline and its exact Cargo process
tree was terminated before the successful bounded rerun. No server, SDK, Cargo,
rustc or performance process is intentionally retained.

## Known limits and deferrals

- Matching is case-sensitive.
- Glob treats `*` and `?` as UTF-8 byte wildcards and does not provide escaping.
- Duration supports exact integer milliseconds only.
- Built-in localhost, browser-extension and crawler presets remain optional future
  policy presets; equivalent explicit rules work now.
- Issue merge/split, grouping rules, historical regrouping and user regular
  expressions remain excluded by ADR-0045.
- Generation 10 is a breaking empty-schema bootstrap generation. Existing generation
  9 databases require an explicit rebootstrap because online migrations remain
  outside the accepted architecture.
