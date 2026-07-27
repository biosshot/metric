# Architecture document index and current status

This file is the canonical navigation entry for `arch-docs`. It prevents historical
phase contracts and superseded proposals from being mistaken for current runtime
behavior.

## Current execution status

Status as of 2026-07-27:

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
| Phase 30 Sessions and Release Health | Next | ADR-0045 |
| Phases 31-36 lightweight product wave | Planned | ADR-0045 |
| Later product capabilities | Deferred, unnumbered backlog | ADR-0040/0045 |

Phase 27 is not complete and no production-ready claim follows from deferring it.
By explicit owner decision, deferred Phase 27 remains incomplete. Phase 28 is
complete, Phase 29 is complete, and Phase 30 is the next implementation phase.
ADR-0045 numbers the selected lightweight product wave through Phase 36.

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

Logs and Spans do not have a pending Processor backlog. Their successful HTTP
response is issued only after every submitted terminal record is durable.
Repeating the same formed writer record is idempotent. Span natural identities also
survive SDK redelivery; a separately redelivered Log is at least once and may
duplicate because its ID contains the server receive time.

## Current physical storage names

The active schema generation is 11. The Error occurrence collection is
`error_events`; `events` is only a legacy generation-7 physical name. Native HTTP
routes may still contain `/events` because route names are product concepts, not
MongoDB collection selectors.

Current high-volume collections are:

```text
error_events
issues
issue_stats_hourly
logs
spans
span_stats_hourly
deploys
```

Future collection names in ADR-0040 are reserved design decisions, not evidence that
the corresponding product capability is enabled.

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
- `0045-lightweight-product-wave-phases-28-36.md`: current Phase 28 and the accepted
  Phase 29-36 sequence.
