# Architecture document index and current status

This file is the canonical navigation entry for `arch-docs`. It prevents historical
phase contracts and superseded proposals from being mistaken for current runtime
behavior.

## Current execution status

Status as of 2026-08-01:

| Scope | Status | Canonical source |
| --- | --- | --- |
| MVP Phases 0-22 | Complete | ADR-0039 and Phase 22 report |
| Phase 23 Dark Web | Complete | ADR-0040/0041 and Phase 23 report |
| Phase 24 Structured Logs | Complete | ADR-0042 and Phase 24 report |
| Phase 25 Transactions/Spans/Traces | Complete | ADR-0043 and Phase 25 report |
| Phase 26 Performance Insights | Complete | ADR-0043 and Phase 26 report |
| Phase 27 Production readiness | Closed as obsolete; historical gates not claimed | ADR-0047 closes ADR-0044 |
| Phase 28 Signal Inbound Filters | Complete | ADR-0045 and Phase 28 report |
| Phase 29 Releases and Deploys | Complete | ADR-0045 and Phase 29 report |
| Phase 30 Sessions and Release Health | Complete | ADR-0045 and Phase 30 report |
| Phase 31 User Feedback | Complete | ADR-0045 and Phase 31 report |
| Phase 32 Unified Explore | Complete | ADR-0045 and Phase 32 report |
| Phase 33 Saved Queries and Dashboards | Complete | ADR-0045 and Phase 33 report |
| Phase 34 Alerts and destinations | Complete | ADR-0045 and Phase 34 report |
| Phase 35 Cron Monitoring | Complete | ADR-0045 and Phase 35 report |
| Phase 36 Uptime Monitoring | Complete | ADR-0045 and Phase 36 report |
| Phase 37 Application Metrics | Complete | ADR-0046 and Phase 37 report |
| Phase 38 Session Replay | Complete | ADR-0046 and Phase 38 report |
| Phase 39 Investigation UX | Cancelled as a phase; ideas returned to backlog | ADR-0047 |
| Phase 40 JSON/CSV export through Unified Query v2 | Complete | ADR-0047/0048 and Phase 40 report |
| Phase 41 Web localization | Complete | ADR-0047 and Phase 41 report |
| Phase 42 Cold archive search | Accepted after/reusing Phase 40 | ADR-0047 |
| Unified Query v2 | Accepted, unnumbered cross-cutting replacement | ADR-0048 |
| Profiling | Desired, execution deferred and unnumbered | ADR-0040/0046 |
| Later product capabilities | Deferred, unnumbered backlog | ADR-0040/0045/0046/0047 |

Phase 27 did not pass its historical gates. By the 2026-08-01 owner decision it is
closed as obsolete and is no longer a release or roadmap gate. Phases 28-38 and 41
are complete. Phase 42 remains selected numbered product work. The
unnumbered Unified Query v2 replacement is also accepted by ADR-0048 and precedes
any new independent hot-data search surface.
ADR-0045 owns the completed lightweight wave; ADR-0046 owns the post-Phase-36
Metrics/Replay sequence and the deferred Profiling boundary. ADR-0047 owns current
post-release focus and closes Phase 27.

## Document precedence

When documents differ, use this order:

1. a later accepted ADR that explicitly supersedes or amends an earlier decision;
2. the owning accepted ADR's implementation amendment;
3. the phase report for evidence about what was actually delivered;
4. the original phase module contract for historical scope;
5. earlier conceptual text.

Phase reports and module contracts are intentionally historical. Statements such as
"the next phase has not started" describe the boundary at the report date, not the
current roadmap.

## Schema-generation safety

The current binary requires schema generation **19 exactly**. The runtime constant
[`SCHEMA_GENERATION`](../crates/mongo/src/lib.rs) is the implementation source of
truth. Generation numbers in older ADR amendments, module contracts and phase
reports describe the schema those phases tested; they are historical evidence, not
current upgrade targets.

Metric has no online or automatic migration and no supported data-preserving
generation-18-to-19 conversion. Operators must not edit `schema_meta`, delete a
data-bearing database or recreate it after an incompatibility error. The current
upgrade decision table and backup warning, including MongoDB/BlobStore consistency
for Session Replay, are in [`docs/upgrading.md`](../docs/upgrading.md).

## Current signal durability

| Signal | Durable path |
| --- | --- |
| Error Event | pending `error_events` record -> Dispatcher -> ErrorProcessor -> Issue/finalization |
| Structured Log | validate/scrub/project -> bounded LogWriter -> terminal unordered `insert_many` |
| Transaction/Span | validate/scrub/expand -> bounded SpanWriter -> terminal unordered `insert_many` |
| Performance aggregate | best-effort derived `span_stats_hourly`, rebuildable from terminal root Spans |
| SDK Session | validate/scrub -> dedicated bounded SessionWriter -> compact lifecycle upsert |
| Release Health | rebuildable `session_stats_hourly` derived from durable Sessions |
| User Feedback | bounded accepted payload -> durable `feedback` record and exact telemetry links |
| Cron/Uptime | bounded Scheduler execution -> durable `monitor_runs` -> existing notification outbox |
| Application Metric | stream-fold pinned `trace_metric` container -> dedicated MetricWriter -> compact `metric_buckets` upsert |
| Session Replay | paired pinned browser Replay items -> dedicated ReplayWriter -> immutable BlobStore segment + compact `replays` manifest |

Logs and Spans do not have a pending Processor backlog. Their successful HTTP
response is issued only after every submitted terminal record is durable.
Repeating the same formed writer record is idempotent. Span natural identities also
survive SDK redelivery; a separately redelivered Log is at least once and may
duplicate because its ID contains the server receive time.

## Current physical storage names

The active schema generation is 19. The Error occurrence collection is
`error_events`; `events` is only a legacy generation-7 physical name. Native HTTP
routes may still contain `/events` because route names are product concepts, not
MongoDB collection selectors.

Current primary signal and product collections include:

```text
error_events
issues
issue_stats_hourly
logs
spans
span_stats_hourly
deploys
sessions
session_stats_hourly
feedback
monitors
monitor_runs
metric_buckets
replays
```

`profiles` remains a future name only. ADR-0046 owns completed Application Metrics
Phase 37 and Session Replay Phase 38, and keeps Profiling deferred.

## Current deployment boundary

The supported application shape remains one active `--role=all` process. NATS,
split roles, multiple active application replicas, sharding, disk spool, MCP and a
second storage backend are deferred. ADR-0044 is a closed historical program, not a
current production declaration gate. Current limits are stated directly in
`docs/known-limits.md`.

## Key documents

- `0034-module-contracts-dependencies-and-quality-gates.md`: permanent module/test
  discipline.
- `0035-configuration-startup-and-operability.md`: configuration, probes and
  operational boundary.
- `0037-capacity-model-for-100-million-events-per-day.md`: hardware-specific Error
  workload and correctness envelope.
- `0039-sequential-module-implementation-plan.md`: completed MVP Phases 0-22.
- `0040-post-mvp-vertical-product-plan.md`: completed Phases 23-26 plus the broad
  product backlog.
- `0042-compact-structured-log-mongodb-model.md`: current Log model and terminal
  writer.
- `0043-compact-spans-traces-and-performance-insights.md`: current Span/Trace and
  performance model.
- `0044-production-readiness-program.md`: closed historical Phase 27 program; its
  gates were not executed or claimed.
- `0045-lightweight-product-wave-phases-28-36.md`: completed Phases 28-36.
- `0046-application-metrics-session-replay-and-deferred-profiling.md`: completed
  Phase 37 bucket model, completed Phase 38 Replay and the deferred Profiling boundary.
- `0047-post-release-focus-and-phase-27-closure.md`: current roadmap, Phase 27
  closure, completed localization and accepted Export/Cold Search scope.
- `0048-unified-query-v2.md`: accepted one-endpoint Search/Explore consolidation,
  shared Web query language and explicit generation-19 no-migration invariant.
- `phase-reports/0041-web-localization.md`: Phase 41 implementation evidence.
- `module-contracts/0037-application-metrics-phase-37.md`: Phase 37 implementation
  boundary and exit gate.
- `phase-reports/0037-application-metrics.md`: Phase 37 implementation and
  verification evidence.
- `module-contracts/0038-session-replay-phase-38.md`: Phase 38 storage, privacy and
  isolation boundary.
- `phase-reports/0038-session-replay.md`: Phase 38 implementation and verification
  evidence.
