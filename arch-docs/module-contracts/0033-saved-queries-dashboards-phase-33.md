# Phase 33 module contract: Saved Queries and Dashboards

## Ownership

- `metric-domain::dashboards` owns bounded identifiers, names, revisioned shared
  records, widget shapes, fixed refresh choices and refresh result envelopes.
- `metric-application::dashboards` owns lifecycle validation, audit actors,
  optimistic revisions, variable application, total cost enforcement and the
  dashboard-only refresh semaphore.
- `metric-ports::DashboardStore` persists project-scoped configuration only.
- `metric-mongo::dashboards` owns strict BSON codecs, validators and indexes for
  `saved_queries` and `dashboards`.
- Native API owns ProjectRead/IssueWrite authorization and closed JSON DTOs. Vue owns
  the shared project workspace and explicit partial-widget error rendering.

## Refresh boundary

```text
authorized project path
-> load one bounded dashboard (maximum 8 widgets)
-> load each referenced typed saved query
-> shift its stored lookback to the current clock
-> apply optional environment/release exact variables
-> revalidate against current Explore schema and estimate every query
-> reject if total cost exceeds 25,000
-> reserve one dashboard refresh permit
-> execute valid widgets sequentially through Explore
-> return a result or stable visible error for every widget
```

Project is an implicit trusted variable from the authorized route. Environment and
release are optional exact predicates. A dataset that does not support a supplied
variable fails only that widget with `dashboard_variable_unsupported`.

## Isolation and failure behavior

Dashboard refresh has two non-waiting permits and then uses Phase 32's four-permit
Explore query reservation. It never acquires Error, Log, Span, Session or Feedback
writer permits. Sequential widget execution prevents one dashboard from creating
parallel MongoDB fan-out.

Missing saved queries, fields rejected by the current Explore schema, shape changes
and individual Explore failures are returned on the affected widget. There is no
derived result cache, background refresh worker, signal-row copy or hidden fallback.

## Storage and authorization

`saved_queries` and `dashboards` are shared project collections with unique
`(project_id, name)` indexes and revision checks. They retain `created_by`,
`updated_by`, `created_at` and `updated_at` for audit. These actor fields are not
ownership ACLs.

ProjectRead may list, load and refresh. IssueWrite may create, update and delete.
Deletion of a referenced saved query is allowed so the dashboard reports the
explicit `saved_query_missing` partial failure. Project deletion purges both
collections through dataset codes 17 and 18.

Schema generation is 14. This is an intentional breaking empty-schema generation;
no migration framework or online migration is introduced.

## Explicit exclusions

Phase 33 does not add private dashboards, per-member copies, cross-project widgets,
raw MongoDB syntax, cached/derived results, scheduled server refresh jobs, alerting,
MCP, NATS, migrations, sharding or disk spool.
