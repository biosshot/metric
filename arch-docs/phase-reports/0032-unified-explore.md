# Phase 32 report: Unified Explore

- Date: 2026-07-27
- Result: complete
- Governing decision: ADR-0045
- Module contract: `module-contracts/0032-unified-explore-phase-32.md`

## Delivered

- One storage-independent `ExploreQuery` AST covers exactly one of `errors`, `logs`
  or `spans`. It supports typed exact, presence and numeric/time range predicates,
  raw table pages, bounded aggregates, at most two low-cardinality groups and fixed
  timeseries intervals.
- Project scope is absent from the HTTP body. The authorized path `project_id` is
  injected by `ExploreService` before validation, cost estimation or MongoDB work.
  Unknown body fields are rejected, including `project_id`, `$match` and other raw
  MongoDB syntax.
- Stable v1 normalization includes dataset, range, predicates, aggregate aliases,
  groups, interval and row limit. Raw cursors are bound to project, normalized query
  and dataset.
- The deterministic estimator checks the 30-day range, eight predicates, four
  aggregates, two finite-cardinality groups, 1,000 buckets, 100 raw rows and exact
  maximum aggregate fan-out against a 10,000-unit budget before storage work.
- A dedicated four-permit query semaphore uses non-waiting reservation. Explore
  cannot consume Error, Log, Span, Session or Feedback writer reservations.
- Every accepted storage operation also carries the validated five-second default
  MongoDB `maxTimeMS`; the application config type accepts only 100 ms through 30 s.
- `ExploreStore` receives only an already validated typed plan. The Mongo adapter
  selects one existing physical collection and builds the closed pipeline locally.
  No collection, raw-payload copy, BSON codec, writer lane or ingest path changed.
- Raw pages use the existing `(project, time, id)` order and deterministic cursor.
  Aggregates provide `count`, `sum`, `min`, `max`, `avg` and accepted
  `p50/p75/p90/p95/p99` percentiles over typed numeric/time fields.
- Vue adds an adaptive Explore workspace with custom selects, typed exact filters,
  table, number and timeseries results, visible estimated cost and explicit API
  errors. The browser never constructs a MongoDB expression.
- `/explore` is a known SPA GET route and `unified_explore` plus its fixed limits are
  published in capabilities.
- Schema generation remains 13. Phase 32 adds no collection, validator or index, so
  no breaking schema generation or migration is required.

## Exit gate

| Gate | Evidence |
| --- | --- |
| Tenant/project scope cannot be overridden | The route body has no scope field and denies unknown fields. `NativeApiService` authorizes the path project, then `ExploreService::plan` injects that `ProjectId`. Rust tests reject both `project_id` and `$match` bodies. |
| Unsafe/high-cost queries fail before partial work | Domain/application tests cover invalid dataset fields and adversarial one-minute/two-group cardinality. Validation and cost estimation run before the query semaphore and `ExploreStore`. |
| Stable normalized AST and cost | A golden normalization test pins v1 text; the estimator is integer-only and the same accepted query produces the same cost. Duplicate aliases and unbounded group fields are rejected. |
| Correct during ingest and TTL deletion | Real local MongoDB E2E queried while 100 Logs were being durably inserted, proved cross-project isolation after acknowledgement, deleted the first 20 rows as retention would and observed exactly 80. |
| Search-under-ingest and adversarial cardinality | The real MongoDB concurrency test and the pure planner adversarial interval/group test both pass. |
| Web sends no raw MongoDB syntax | `ExploreView` constructs a closed JSON DTO from custom selects. Chromium E2E submits the typed number query and renders count plus cost. The HTTP parser denies unknown/raw-expression fields. |
| API/Web | Rust permission/body/SPA tests, 23 Vitest tests, ESLint, Vue type-check/build and the focused Chromium E2E pass. |
| Performance | The single retained release-mode planner scenario completed 250,000 plans in 263 ms: **947,919 RPS**, above the 100,000 RPS local gate. |
| Process cleanup | Timed-out Cargo invocations and their exact `rustc` children were terminated. Playwright stopped its Vite/Chromium children. Final inspection found no Phase 32 Cargo, Rust, Node, Vite or Playwright process. |

## Verification

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib
cargo test -p metric-mongo --test explore_store -- --ignored --nocapture

cd web
npm run lint
npm test
npm run build
npx playwright test tests/e2e/application.spec.ts \
  --grep "Explore submits" --project=chromium

cargo test -p metric-application --release \
  --test explore_planner_perf -- --ignored --nocapture
```

The performance artifact is
`performance/baselines/unified-explore/ryzen-5600h-windows-v1.json`. Exactly one
performance test was run.

## Known limits and next phase

- Explore is intentionally one project and one dataset per query. It has no joins,
  cross-organization scope, arbitrary tags, regex, scripts or raw MongoDB syntax.
- Raw Errors expose compact indexed projections only; Explore does not decode every
  Event body to simulate arbitrary fields.
- Results are queried directly from the retained source collections. No derived
  result cache or raw-data collection exists.
- Phase 33 Saved Queries and Dashboards is next and may persist these normalized
  queries under its own bounded widget and authorization contract.
- Phase 27 remains explicitly deferred and incomplete. MCP, NATS, sharding, disk
  spool and online migrations remain deferred.
