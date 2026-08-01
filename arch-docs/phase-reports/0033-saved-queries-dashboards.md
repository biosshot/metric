# Phase 33 report: Saved Queries and Dashboards

- Date: 2026-07-27
- Result: complete
- Governing decision: ADR-0045
- Module contract: `module-contracts/0033-saved-queries-dashboards-phase-33.md`

## Delivered

- Added project-shared `saved_queries` and `dashboards` configuration records with
  bounded names, stable 16-byte IDs, optimistic revisions and actor/time audit
  fields. They do not contain signal rows or derived query results.
- Saved queries retain the closed Phase 32 `ExploreQuery`. Create and every update
  call the current `ExploreService::plan`, so removed or renamed fields fail before
  persistence.
- Dashboards contain 1-8 number, table or timeseries widgets and one of the fixed
  `manual`, `30s`, `1m` or `5m` refresh preferences. Every dashboard update reloads
  and revalidates all referenced saved queries and verifies each widget shape.
- Refresh shifts each saved absolute range as a lookback ending at the current
  clock, applies optional exact environment/release variables and uses the
  authorized route project as the project variable.
- The complete valid plan set must fit the 25,000-unit dashboard budget before any
  Explore execution. Widgets execute sequentially under a dedicated two-permit
  dashboard semaphore and the existing Explore reservation.
- Missing saved queries, unsupported variables, current-schema rejection, shape
  mismatch and individual query failure remain visible on the affected widget while
  independent widgets continue.
- Mongo schema generation 14 adds strict `saved_queries` and `dashboards`
  validators, unique project/name indexes, project/update list indexes and project
  deletion dataset codes 17/18. This is an accepted breaking generation with no
  migration layer.
- Native API adds complete list/load/create/update/delete and refresh routes.
  ProjectRead covers shared viewing and refresh; IssueWrite covers every mutation.
- Vue adds `/dashboards`, a Lucide navigation icon, custom selects, responsive
  saved-query/dashboard builders, environment/release controls, number/table/
  timeseries-compatible output, compact edit/delete actions and explicit partial
  widget errors.
- Capabilities publish the fixed widget, total-cost, concurrency, refresh-variable
  and no-result-cache contract. `/dashboards` is a known SPA GET route.

## Exit gate

| Gate | Evidence |
| --- | --- |
| Stored queries revalidate against the current typed schema on every update | `DashboardService::create_saved_query` and `update_saved_query` call `ExploreService::plan`; dashboard create/update also reload and plan every referenced query. |
| One dashboard cannot fan out into unbounded queries | Domain and Mongo validators cap widgets at 8. Application checks the count, plans the complete set, enforces total cost 25,000 and executes widgets sequentially. |
| Deleted/renamed fields fail visibly | A deleted query returns per-widget `saved_query_missing`; current-schema planner codes and `widget_shape_mismatch` are returned on the affected widget. The focused Chromium test deletes a referenced query and renders the error. |
| Authorization covers shared project view/create/edit/delete | Native API methods use ProjectRead for list/load/refresh and IssueWrite plus the active-project mutation fence for create/update/delete. The pinned route permission matrix contains all 11 routes. Vue hides mutation controls from viewers. |
| Dashboard refresh cannot consume ingest reservations | Dashboard owns a two-permit semaphore and invokes only Phase 32 Explore, which owns its separate four-permit query semaphore. No writer/sink/ingest port is referenced. |
| Browser E2E covers lifecycle, variables and partial widget failure | Focused Chromium creates a saved query and dashboard, sends `environment=production`, renders a number, deletes the referenced query, renders `saved_query_missing`, and deletes the dashboard. |

## Verification

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib
cargo test -p metric-mongo --test dashboards -- --ignored --nocapture

cd web
npm run lint
npm test
npm run build
npx playwright test tests/e2e/application.spec.ts \
  --grep "Dashboard lifecycle" --project=chromium
```

No performance test was created or run for Phase 33, as explicitly requested.

## Known limits and next phase

- Initial dashboards are shared only within one project. There are no personal or
  cross-project dashboards.
- Refresh reads source collections directly and has no result cache. Vue honors the
  fixed stored interval while the view is open; the server creates no scheduled
  worker.
- Environment/release variables are exact predicates and fail visibly for datasets
  that do not expose those typed fields.
- At this report cutoff Phase 34 Alerts and notification destinations was next and
  Phase 27 was deferred. ADR-0047 later closed Phase 27 as obsolete without claiming
  its gates. MCP, NATS, migrations, sharding and disk spool remain unselected.
