# Phase 9 contract: bounded FinalizeBatch and derived catalogs

- Status: accepted for implementation
- Date: 2026-07-22
- Owners: `application::finalizer`, `domain::finalization`, `ports::FinalizationStore`, `mongo::finalizer`

## Responsibilities and exclusions

Finalizer is the single owner of durable successful-processing order for a bounded
FinalizeBatch. IssueService still owns deterministic Issue identity and occurrence
semantics, but exposes a preparation operation: the Phase 10 pipeline passes the
prepared occurrence to Finalizer and does not persist it separately. Finalizer groups
those occurrences by Issue and hour, then asks one capability-specific storage port
to apply the batch.

Phase 9 replaces each still-pending Event body with one canonical normalized and
derived body, stores the Issue association and Search v1 tokens, removes processing
state, and installs absolute Event retention. It also performs one Issue mutation per
Issue, one increment per Issue/hour, bounded implicit Release/Environment admission,
and the Issue-owned new-Issue/regression notification-intent hook.

Phase 9 does not orchestrate Normalizer/Symbolicator/Grouper deadlines or retries,
deliver notifications, expose Search/API queries, create daily rollups, materialize
distributions, implement release health/deploys/commits, add migrations, or add MCP,
NATS, sharding, disk spool, external search, or cold archival.

## Typed batch and bounds

Each item contains the accepted Event identity/timestamps, its prepared Issue
occurrence, canonical processed JSON bytes, normalized level/platform, and at most 16
domain-separated signed 64-bit Search v1 tokens. The body contains normalization
diagnostics and backend-independent symbolication status/derived frames; raw and
derived frames remain distinguishable.

The application configuration bounds events per batch, encoded/decoded body size,
Event and hourly-stat retention durations, implicit Releases per project/UTC receipt
day, and implicit Environments per project. Zero values, duration overflow, more than
16 tokens, duplicate Event keys, or inconsistent Event/Issue project/identity fail
before storage mutation.

Release IDs are BLAKE3-128 over an unambiguous organization/exact-version encoding.
Environment IDs are BLAKE3-128 over project/exact-name. Hour bucket IDs are BLAKE3-128
over project, Issue, and UTC hour. Exact catalog strings remain case-sensitive and
are never replaced by their IDs in the Event body.

## Retry and acknowledged-step order

The adapter first filters the requested keys to Events still satisfying `q.s == 0`.
If none remain, the batch is an idempotent no-op. For the remaining set it performs:

1. grouped Issue updates, including atomic new-Issue/regression outbox transitions;
2. grouped `issue_stats_hourly` increments and absolute bucket expiry;
3. bounded Release and Environment admission plus idempotent catalog upserts;
4. per-Event conditional finalization filtered again by `_id`, project, and `q.s == 0`.

Issue and bucket increments before step 4 may positively drift after a crash and
retry, exactly as accepted by ADR-0005. First/last pairs use min/max plus Event-ID
tie-breaking and cannot drift. Deterministic identities prevent another Event, Issue,
bucket, Release, Environment, activity, or notification transition. Catalog admission
usage may drift conservatively but never exceeds its configured creation ceiling.

The Event update is the strict terminal fence. It sets `u`, canonical `b`, optional
`k`, normalized compact metadata and `x`, then removes `q`. A completed Event is never
rewritten by a retry or associated with another Issue.

## Physical projections and observability

`issue_stats_hourly` uses deterministic binary `_id`, descriptive project/Issue/hour
fields, int64 count and an absolute TTL date. Releases retain organization scope,
exact version, bounded project associations, first/latest composite Event keys and no
TTL. Environments retain project scope, exact name, first/last seen, hidden/source and
no TTL. Projects store compact admission usage only after first use.

The initial stats and catalog indexes are exactly those accepted by ADR-0008. Safe
metrics contain operation, acknowledged step and bounded outcome only; project,
Event/Issue/catalog IDs, release/environment values, titles, tokens and body contents
are forbidden labels.

## Verification gate

Required verification includes canonical processed-body and Search-token goldens;
real MongoDB Event/Issue/bucket/catalog integration; failure/retry at every
acknowledged step; duplicate and partially completed batches; Issue/hour aggregation,
catalog limits and retention timestamps; notification-intent idempotency; production
query explains; one retained FinalizeBatch RPS baseline; full format/lint/workspace
tests; and an explicit post-benchmark process cleanup check.
