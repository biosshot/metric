# Phase 12 contract: native query/command API and MongoDB Search v1

Status: implemented and verified by the Phase 12 exit gate.
Owning ADRs: ADR-0003, ADR-0008, ADR-0009, ADR-0015, ADR-0017, ADR-0019,
ADR-0021, ADR-0022, ADR-0023, ADR-0024, ADR-0034, ADR-0035, ADR-0036, ADR-0039

## Responsibility

The native API owns the versioned `/api/v1` HTTP contract and maps descriptive JSON
DTOs to typed application query and command services. The application layer owns
authorization, project isolation, cursor binding, Search v1 parsing, candidate
verification, command idempotency, and stable application errors. The MongoDB
adapter owns compact-field queries, projections, index hints, and BSON codecs.

Phase 12 includes:

- Web-session and personal-token authentication transport;
- organization-scoped project creation/list/detail, DSN key creation/list/disable,
  and project ingest-policy reads/updates;
- Issue list/detail/hourly statistics/lifecycle/activity;
- Event project/Issue timelines, exact detail, and bounded Search v1;
- project-scoped Release and Environment lists;
- capabilities and authenticated component status.

It does not implement Web screens, MCP, migrations, NATS, sharding, disk spool,
retention scheduling, project deletion, or any upload/symbolication management API.

## Typed contract and stable errors

The public API uses descriptive snake_case JSON, string identifiers, RFC 3339 UTC
timestamps, and opaque versioned cursors. Errors have this bounded envelope:

```json
{"error":{"code":"invalid_request","message":"request is invalid","request_id":"..."}}
```

The stable codes are:

```text
invalid_request
invalid_cursor
invalid_credentials
csrf_failed
forbidden
not_found
conflict
rate_limited
search_syntax_invalid
search_field_not_indexed
search_requires_positive_anchor
search_limit_exceeded
temporarily_unavailable
```

Messages never include secrets, raw queries, payloads, MongoDB errors, collection
names, or compact physical field names.

## Ports and effects

`InvestigationStore` is the only Phase 12 read/query storage capability. Its methods
are explicitly project-scoped and accept typed filters and keyset anchors. It returns
domain projections and cannot receive BSON, JSON MongoDB operators, raw projections,
or collection names.

Existing `ProjectStore`, `IssueStore`, and `AuthStore` capabilities remain the write
boundaries. Phase 12 may add narrowly typed list/policy/token operations to their
owning capabilities. Administrative project/key/policy mutations append allowlisted
audit records and invalidate affected DSN cache entries.

Search compiles into typed positive exact-token/time/identity constraints. MongoDB
returns no more than the configured candidate cap; application code decodes and
post-verifies every candidate before returning it.

## Idempotency, ordering, and pagination

Issue lifecycle writes require a caller idempotency key and reuse the Phase 9
single-document command fence. Project/key commands use unique domain identities and
stable conflict errors.

Timeline order is newest first:

```text
Issue: (last_seen DESC, issue_id DESC)
Event: (occurred_at DESC, event_key DESC)
Activity: (timestamp DESC, activity_id DESC)
Release/Environment: (last_seen DESC, id DESC)
```

Cursors contain a format version, cursor kind, last ordering tuple, and a BLAKE3
digest bound to project, normalized filter/query, and ordering. Invalid,
cross-project, cross-query, and cross-route cursors fail closed.

## Bounds

- JSON request body: 64 KiB;
- default page size: 50; maximum: 100;
- query text: 4,096 UTF-8 bytes;
- predicates: 16; OR branches: 8; nesting: 4;
- default time range: 24 hours; maximum: 30 days;
- MongoDB Search candidates: 10,000;
- Search deadline: 2 seconds;
- custom indexed tags: disabled until a project allowlist is implemented;
- activity/statistic query ranges and returned buckets are bounded by the same
  30-day and 100-item limits.

No endpoint buffers an unbounded database cursor or returns an unbounded Event body.
Cancellation drops in-flight futures; there is no Phase 12 background worker.

## Authorization

Every project route calls the authoritative Phase 11 project authorization boundary.
Read routes require their matching `project:read`, `issue:read`, or `event:read`
permission. Project/key/policy writes require `project:admin`. Issue lifecycle writes
require `issue:write`. Organization project creation/list requires
`organization:admin`/`project:read` respectively.

Session state-changing requests require the session cookie and CSRF header. Personal
tokens use `Authorization: Bearer` and are restricted to their stored scopes.

## Operability

Metrics use only operation, outcome, response class, and bounded error code. Search
records candidate count and latency without project, query, Event, Issue, tag, user,
or release labels. Logs contain request correlation IDs and stable operation/error
codes only.

## Gate

- DTO, error envelope, and cursor golden tests;
- route permission matrix;
- pagination stability during concurrent inserts;
- Search grammar, branch/predicate/cardinality, time-range, field allowlist,
  positive-anchor, cursor-binding, and post-verification security tests;
- real MongoDB integration with accepted index explains;
- one retained local p95/p99 and RPS baseline over a representative real dataset;
- cumulative E2E: authenticated project creation -> SDK Event -> processed Issue
  query -> idempotent lifecycle mutation;
- workspace format, lint, tests, and explicit cleanup of benchmark/server processes.
