# Architecture document index and current status

This file is the canonical navigation entry for `arch-docs`. It prevents historical
phase contracts and superseded proposals from being mistaken for current runtime
behavior.

## Current execution status

Status as of 2026-07-28:

| Scope | Status | Canonical source |
| --- | --- | --- |
| MVP Phases 0-22 | Complete | ADR-0039 and Phase 22 report |
| Phase 23 Dark Web | Complete | ADR-0040/0041 and Phase 23 report |
| Phase 24 Structured Logs | Complete | ADR-0042 and Phase 24 report |
| Phase 25 Transactions/Spans/Traces | Complete | ADR-0043 and Phase 25 report |
| Phase 26 Performance Insights | Complete | ADR-0043 and Phase 26 report |
| Phase 27 Production readiness | Accepted, execution deferred | ADR-0044/0045 |
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
| Phase 38 Session Replay | Next | ADR-0046 |
| Profiling | Desired, execution deferred and unnumbered | ADR-0040/0046 |
| Later product capabilities | Deferred, unnumbered backlog | ADR-0040/0045/0046 |

Phase 27 is not complete and no production-ready claim follows from deferring it.
By explicit owner decision, deferred Phase 27 remains incomplete. Phases 28-37 are
complete, and Phase 38 Session Replay is the next implementation phase.
ADR-0045 owns the completed lightweight wave; ADR-0046 owns the post-Phase-36
Metrics/Replay sequence and the deferred Profiling boundary.

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

Logs and Spans do not have a pending Processor backlog. Their successful HTTP
response is issued only after every submitted terminal record is durable.
Repeating the same formed writer record is idempotent. Span natural identities also
survive SDK redelivery; a separately redelivered Log is at least once and may
duplicate because its ID contains the server receive time.

## Current physical storage names

The active schema generation is 18. The Error occurrence collection is
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
```

`replays` and `profiles` are future names only. ADR-0046 makes completed Application
Metrics Phase 37, Session Replay Phase 38 and keeps Profiling deferred.

## Current deployment boundary

The supported application shape remains one active `--role=all` process. NATS,
split roles, multiple active application replicas, sharding, disk spool, MCP and a
second storage backend are deferred. ADR-0044 defines the evidence required before
the current release may claim production readiness.

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
- `0044-production-readiness-program.md`: accepted but deferred Phase 27 and
  production launch gate.
- `0045-lightweight-product-wave-phases-28-36.md`: completed Phases 28-36.
- `0046-application-metrics-session-replay-and-deferred-profiling.md`: completed
  Phase 37 bucket model, accepted Phase 38 Replay and the deferred Profiling boundary.
- `module-contracts/0037-application-metrics-phase-37.md`: Phase 37 implementation
  boundary and exit gate.
- `phase-reports/0037-application-metrics.md`: Phase 37 implementation and
  verification evidence.
