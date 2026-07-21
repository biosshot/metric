# Phase 1 contract: Sentry HTTP Ingest with fake ports

- Status: accepted for implementation
- Date: 2026-07-21
- Owners: `sentry-protocol` (wire framing), `application::ingest` (policy and durable
  outcome orchestration), `server` (HTTP and composition)

## Responsibilities and exclusions

Ingest bounds and decodes HTTP bodies, parses Sentry Envelope/store transports,
resolves one DSN key, enforces project/path consistency, classifies Items, validates
one Error Event, recursively applies the mandatory scrub floor, submits one
`AcceptedEvent` to `EventSink`, records lossy outcomes, and maps the result to HTTP.

It does not implement MongoDB, Project storage/cache, grouping, normalization,
symbolication, Issue behavior, attachments, migrations, disk spool, MCP, NATS, or
distributed rate limiting. Production composition cannot acknowledge fake durability.

## Inputs, outputs, and stable errors

Inputs are a bounded decoded Envelope or store body, bounded authentication sources,
the URL project ID, and the request cancellation/deadline. Output is handled-without-
Event, durably accepted Event, or duplicate. Stable error codes are
`invalid_request`, `unauthorized`, `too_large`, `rate_limited`, `unavailable`,
`timeout`, `shutting_down`, and `scrub_failed`.

Required ports are `ProjectResolver`, `EventSink`, lossy `OutcomeSink`, `Clock`, and
`RandomSource`. Fake implementations expose exactly these production contracts.

## Idempotency and side effects

Event identity is the validated 16-byte Sentry Event ID. `EventSink` reports accepted
or duplicate; both map to HTTP 200, and duplicate payloads are not treated as new
durable work. The only Phase 1 side effect is the configured sink call plus lossy
outcome aggregation. No raw or unsanitized payload is persisted or logged.

## Resource, cancellation, and shutdown bounds

Defaults follow ADR-0010: 20 MiB compressed, 100 MiB decompressed, 1 MiB Event, 100
Items, 512 active requests, CPU-derived parsing permits, 512 storage waiters, and a
10-second request deadline. Compressed and decoded bodies are not retained together.
Declared Item length is authoritative. Permit exhaustion, deadline expiry, client
cancellation, and the process shutdown fence cannot produce durable success.

## Operability and safe fields

Metrics use fixed route/operation/outcome/item/error labels. Safe tracing fields are
request ID, byte counts, item type, project ID after authentication, scrub-policy
revision, and stable error code. Raw headers, query strings, DSNs, keys, bodies,
payload values, URLs, and arbitrary parser/backend errors are forbidden.

## Verification and performance acceptance

Required tests cover captured/golden Envelopes, header/query/envelope auth, gzip and
deflate, mixed/disabled/unknown Items, malformed/truncated/conflicting framing,
recursive credential scrubbing, compressed/decompressed/Event/Item-count bounds,
permit exhaustion, slow sink, deadline, shutdown, and production fake fencing.

The black-box E2E is HTTP -> auth/parser/admission -> deterministic fake durable
outcome -> response. k6 records requests/s, bytes/s, p50/p95/p99, errors, and achieved
RPS as JSON. Results retain commit, toolchain, k6 version, hardware, configuration,
fixture revision, and command. A baseline regression is reviewed rather than hidden;
5,000/s steady and 20,000/s burst remain controlled-hardware targets, not claims from
an arbitrary development machine.
