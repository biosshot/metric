# ADR-0044: Production readiness program before new product features

- Status: Accepted
- Date: 2026-07-25
- Supersedes: ADR-0040 product-phase ordering after Phase 26 until this program passes

## Context

Faultkeep already contains the product core needed for a useful Sentry-compatible
installation:

- Error ingest, processing, grouping, Issues and hourly statistics;
- Structured Logs;
- Transactions, Spans, virtual Traces and Performance Insights;
- Sentry SDK authentication and bounded Envelope parsing;
- native and JavaScript symbolication, debug files and Artifact Bundles;
- attachments, minidumps and Incident Capsules;
- retention, project deletion, optional S3-compatible cold archive;
- users, organization authorization, audit, Web and webhook notifications.

Phases 24 and 25 also own independent bounded Log and Span writers. They combine
terminal records into configurable time/document/byte-bounded unordered MongoDB
`insert_many` calls. A successful ingest response is returned only after every
record in the signal batch is durable. Repeating the same formed writer record is
idempotent; Span identities also survive SDK redelivery, while a separately
redelivered Log is explicitly at least once and may duplicate. Logs and Spans
deliberately have no pending Processor backlog because all accepted normalization and
PII work completes before their terminal write.

The remaining gap is not another user-facing dataset. It is evidence that the
existing system can be installed, secured, observed, loaded, failed, restored and
upgraded within an explicitly supported production envelope.

The previously numbered `Unified Explore` entry and all later product entries in
ADR-0040 are therefore paused and made unnumbered backlog. Phase 27 is this
production-readiness program. Deferred product scopes are not deleted; they receive
new numbers only when product work resumes.

## Decision

No new product capability is added until the production launch gate in this ADR
passes. Work is limited to correctness fixes, bounded performance improvements,
security, operational interfaces, deployment, verification and documentation for
already implemented capabilities.

The first supported production shape remains:

```text
one active faultkeep --role=all process
-> one declared MongoDB 8 topology
-> optional external Symbolicator
-> S3-compatible BlobStore for durable Blob features
   or a backed-up persistent local volume for smaller installations
-> TLS reverse proxy / ingress
-> external monitoring and backup system
```

Multiple active Faultkeep replicas, split roles, a broker, MongoDB sharding and
zero-downtime rolling upgrades are not implied by the production claim.

## Meaning of production-ready

Faultkeep is production-ready only when all of the following are true for a named
release and a declared deployment profile:

1. every acknowledged record is either durably recoverable or covered by an
   explicitly documented at-least-once/idempotency rule;
2. tenant authorization and PII boundaries pass adversarial tests;
3. memory, queues, concurrency, request sizes and query costs remain bounded;
4. expected dependency failures degrade visibly and recover without manual database
   editing;
5. capacity and latency are measured on declared hardware with production-shaped
   data;
6. operators can observe saturation, lag, failure and storage exhaustion before data
   safety is threatened;
7. a coordinated backup is restored into an isolated environment and verified;
8. installation, upgrade, rollback and incident runbooks are executable by someone
   other than the feature author;
9. the shipped image and configuration are reproducible, pinned and security
   reviewed;
10. a canary survives the accepted observation period without an unexplained
    correctness, security or resource incident.

Passing unit tests or building a container is necessary but cannot independently
satisfy this definition.

## Scope freeze

Allowed changes during Phase 27:

- correctness and crash-consistency fixes;
- removal of unbounded work or cardinality;
- performance work justified by profiles or query plans;
- tests, fixtures, fault injection and benchmark tooling;
- operational metrics, probes and low-cardinality diagnostics;
- secure configuration and deployment hardening;
- backup, restore and offline upgrade tooling/runbooks for schema generation 8;
- documentation corrections;
- narrowly required storage compatibility fixes before the release candidate freezes.

Not allowed:

- Unified Explore, saved queries or Dashboards;
- new product Alert/Monitor semantics;
- Issue collaboration, advanced grouping UI or release intelligence;
- Sessions, Feedback, Cron, Uptime, Metrics, Profiling or Replay;
- Teams, integrations, drains, MCP or AI;
- distributed roles, NATS, disk spool, sharding or a second database backend;
- speculative refactors without a failing gate, measured bottleneck or correctness
  defect.

## Known open blockers at program start

The documentation/code synchronization on 2026-07-25 identified these concrete
release blockers; they are not deferred feature requests:

1. Span release is stored in BSON `u`, while the current segment-list release filter
   targets `v` (Span status). The query field must be corrected and covered by real
   MongoDB/API/Web regression tests.
2. Log message contains-search uses a case-insensitive regex after project/time
   filtering and has no text/search index. Its production-shaped query cost must pass
   Gate 27.5 or the capability must be bounded/replaced.
3. Operational metric instruments exist, but the release deployment does not yet
   expose a standard metrics exporter suitable for external alerting.
4. The release compose runtime uses the MongoDB administrative bootstrap credential;
   Gate 27.2 requires a separate least-privilege application user.
5. Current operator documents outside `arch-docs` still mention schema generation 7
   and disabled Logs/Transactions/Spans; Gate 27.0 must make capability claims agree
   with generation 8.
6. A coordinated restore drill, controlled mixed-signal load/soak and production
   canary have not yet passed.

## Phase 27 execution plan

### Gate 27.0: truth baseline and release scope

Create one machine-readable release inventory containing:

- Git commit, Rust version, dependency lock hash and Web dependency lock hash;
- MongoDB version and supported topology;
- enabled collections and schema generation;
- BlobStore and Symbolicator modes;
- exact SDK/CLI compatibility rows;
- enabled Envelope items and known limits;
- default and tested non-default resource limits.

Correct all stale documentation before further claims. In particular:

- current operator/capability documentation must identify schema generation 8 rather
  than generation 7; historical phase reports retain the generation they tested;
- Logs, Transactions, Spans and Performance Insights must no longer be described as
  disabled;
- `/api/v1/capabilities`, compatibility documentation, configuration examples and
  known limits must agree;
- every ignored test must have an owner, prerequisite and classification:
  `release-blocking`, `scheduled`, or `documented unsupported`.

Exit gate:

- the release inventory is generated in CI;
- contradictory capability/schema documentation is rejected by an automated check;
- there is no unexplained ignored test.

### Gate 27.1: deterministic full-system verification

Make the release gate reproducible on a clean worker:

1. formatting, dependency direction and strict lint;
2. all unit, property and codec golden tests;
3. real MongoDB adapter suites;
4. Browser Web/authorization E2E;
5. real SDK Error, Log and Trace/Span flows for every claimed compatibility row;
6. `sentry-cli` debug-file and Artifact Bundle flows;
7. Local and S3 BlobStore suites;
8. external Symbolicator success, timeout, malformed response and outage cases;
9. container start, readiness, ingest, investigation and graceful shutdown;
10. installation from the produced release artifact, not from the source tree.

Flaky retry cannot make a red test green silently. A retry is reported and an
accepted flake budget is zero for correctness/security tests.

Exit gate:

- the complete gate passes twice from clean state;
- no background process or test database survives the run;
- test artifacts contain versions and sanitized failure diagnostics.

### Gate 27.2: security and isolation

Produce a concise threat model covering:

- public SDK ingest;
- authenticated native/Web API;
- organization/project isolation;
- decompression, JSON/BSON, minidump and artifact parsing;
- attachment/Blob download authorization;
- Symbolicator callback and private debug-file access;
- webhook SSRF, signing and secret storage;
- archive/object-store access;
- administrator bootstrap, sessions, tokens and CSRF.

Harden and verify:

- TLS termination and trusted-proxy behavior are explicit;
- production cookies are `Secure`, `HttpOnly` and use the accepted `SameSite`
  policy;
- security headers and CSP are tested for the shipped Web application;
- MongoDB is not exposed publicly and the application uses a least-privilege user,
  not the administrative bootstrap account;
- containers remain non-root, read-only where possible and without new privileges;
- secrets never appear in logs, metrics, readiness, error responses or support
  bundles;
- authentication, ingest, webhook and expensive query rate limits fail closed;
- parser fuzz corpora and compression/resource-exhaustion cases execute on schedule;
- dependencies and the container image receive vulnerability/license checks;
- the release publishes checksums and an SBOM.

Exit gate:

- tenant-isolation and authorization decision tests pass against real adapters;
- no unresolved critical/high vulnerability exists without a written, time-bounded
  exception;
- a manual security review closes every threat-model item.

### Gate 27.3: durability, backup, restore and offline upgrade

Advanced online migrations and mixed-version rolling deployment remain a deferred
operations backlog item. Production readiness nevertheless requires a safe
current-version recovery path.

Implement and document a coordinated offline procedure:

1. stop new admission;
2. drain bounded writers and graceful shutdown;
3. snapshot/export MongoDB and the selected BlobStore under one backup identifier;
4. record schema generation, object namespaces, checksums and configuration
   prerequisites;
5. restore into an empty isolated environment;
6. validate schema, indexes, counts, Blob references and sampled object hashes;
7. start the exact release and run read/ingest/investigation smoke tests.

Test at minimum:

- MongoDB-only installation with Blob features disabled;
- Local BlobStore on persistent storage;
- S3-compatible BlobStore;
- interrupted backup and incomplete restore;
- wrong schema generation and missing/corrupt Blob objects;
- rollback to the previous binary when no schema change occurred.

Every release declares its recovery point and recovery time objectives. They are
operator-visible measured results, not undocumented assumptions.

Exit gate:

- a fresh operator follows the runbook and restores the release successfully;
- acknowledged fixture IDs, Issue counts, Logs, Traces, artifacts and attachments
  match the backup manifest within their documented consistency semantics;
- destructive restore refuses a non-empty target unless explicitly authorized.

### Gate 27.4: dependency failure and crash recovery

Exercise the complete system under:

- MongoDB latency, connection loss, primary stepdown and temporary outage;
- MongoDB disk-full/write rejection;
- S3 latency, throttling, timeout, partial upload and outage;
- Local BlobStore disk reserve and disk-full conditions;
- Symbolicator timeout, malformed output and restart;
- webhook timeout, retry storm and permanently failing destination;
- process kill during Event insert, Log/Span batch, Event finalization, archive,
  project deletion, artifact GC and graceful shutdown;
- clock movement within the documented tolerance;
- corrupted configuration and missing secrets at startup.

Required properties:

- no HTTP success is reported for a record the accepted durability contract did not
  persist;
- retries inside each signal's documented idempotency scope do not create duplicate
  logical records; duplicates outside that scope are limited to explicit at-least-once
  contracts such as separate Log SDK redelivery and webhook delivery;
- queues, retry sets and error logs stay bounded;
- readiness becomes false when required durability is unavailable;
- optional dependencies degrade only their declared capability;
- recovery needs no direct MongoDB document editing;
- retention and deletion never remove pending or still-owned data.

Exit gate:

- a fault matrix records injection point, expected response, actual response,
  recovery time and invariant verification;
- every unexpected crash or invariant violation is fixed and receives a regression
  test.

### Gate 27.5: production-shaped capacity and soak

Retain ADR-0037's Error target:

```text
average target       1,158 accepted Error Events/s
steady headroom      5,000/s for 60 minutes
burst               20,000/s for 5 minutes
```

Run separate declared profiles for:

- Error-only ingest and Processor completion;
- Log-only ingest/search;
- Span/Transaction ingest, Trace assembly and aggregate updates;
- mixed Error/Log/Span traffic;
- symbolication cache hit/miss mixtures;
- concurrent Web/API search;
- retention/archive/GC concurrent with foreground ingest;
- backlog growth, process restart and recovery where the Event path is asynchronous.

For terminal Log/Span writers measure batch occupancy, bytes, wait time, in-flight
batches, MongoDB commands and ambiguous-response retries. Do not invent a durable
pending backlog for those terminal signals.

The Error acceptance SLO remains:

```text
acknowledged loss             0
duplicate durable records     0
unexpected 5xx                < 0.1%
durable acknowledgement p95  < 100 ms
durable acknowledgement p99  < 250 ms
bounded memory/queues         required
```

Logs and Spans publish their own measured SLOs after the mixed workload is profiled;
they cannot borrow Error admission capacity or invalidate the Error SLO.

Artifacts retain:

- hardware, OS, topology, configuration and commit;
- fixture distribution and actual BSON/Blob/index sizes;
- throughput and complete latency histograms;
- CPU, allocation and memory profiles;
- queue/batch occupancy and rejection classes;
- MongoDB CPU, cache, IOPS, disk latency, replication lag and query plans;
- retention/archive lag and storage growth.

Exit gate:

- all correctness invariants pass at target and during recovery;
- a minimum 24-hour production-shaped soak has no unbounded growth;
- the published capacity statement names the exact hardware/profile and never claims
  a hardware-independent 100-million/day guarantee;
- observed limits lead to configuration guidance, not premature role splitting or
  sharding.

### Gate 27.6: operability and supported deployment

Expose standard, externally consumable operational data:

- component-level liveness and readiness;
- Prometheus-compatible or equivalently standard metrics;
- HTTP rate/latency/result classes;
- writer queue documents/bytes, batch occupancy and saturation;
- Event pending count/oldest age and Processor completion latency;
- MongoDB/Blob/Symbolicator latency and errors;
- retention, archive, project deletion, upload and GC lag/failures;
- notification backlog/retries/dead deliveries;
- disk reserve, storage growth and capacity projection;
- build version, schema generation and capability state without secrets.

Metrics use a closed low-cardinality label set. Project IDs, URLs, releases, event
identifiers, filenames and user data never become labels.

Publish runbooks for:

- initial installation and secure reverse proxy;
- configuration validation and secret rotation;
- normal restart and graceful shutdown;
- MongoDB/S3/Symbolicator outage;
- disk pressure and retention lag;
- Event backlog and saturated Log/Span writer;
- backup, restore, offline upgrade and rollback;
- project deletion stuck/retry;
- evidence collection that excludes customer payloads.

The release deployment must:

- pin MongoDB and image versions/digests;
- use separate MongoDB bootstrap and least-privilege runtime credentials;
- persist every required data path explicitly;
- define resource requests/limits or documented host sizing;
- include health checks and a shutdown grace greater than the configured drain bound;
- avoid exposing MongoDB, Symbolicator private-source endpoints or operational
  metrics to the public network.

Exit gate:

- alert examples cover every condition that can threaten durability or sustained
  ingest;
- an operator drill resolves at least MongoDB outage, disk pressure and stuck
  backlog using only published interfaces/runbooks;
- ordinary operation does not require reading MongoDB documents manually.

### Gate 27.7: release candidate, canary and production declaration

Freeze one release-candidate commit after Gates 27.0-27.6. Only release-blocking
fixes and their regression tests may enter it.

Run:

1. the clean full-system gate;
2. the declared capacity suites;
3. the 24-hour soak;
4. backup and isolated restore;
5. a minimum seven-day canary on real bounded traffic;
6. one planned restart, one dependency-failure drill and one rollback drill;
7. final security and operations review.

The canary records accepted/rejected outcomes, p95/p99 latency, queue/batch occupancy,
Event completion lag, MongoDB/storage growth, query latency, readiness transitions
and operator interventions. Customer payloads are not copied into the report.

Exit gate:

- no acknowledged data loss, cross-tenant access or unexplained duplicate exists;
- no unbounded resource trend or unresolved high-severity incident remains;
- backup/restore and rollback evidence belongs to the exact candidate;
- `docs/supported-capabilities.md`, `docs/known-limits.md` and the compatibility matrix
  describe exactly what is being released;
- a signed go/no-go report names the supported topology, capacity envelope, known
  limits and deferred features.

Only after this gate may the release be described as production-ready.

## Mandatory release artifacts

Phase 27 produces:

```text
production-readiness/
  release-inventory.json
  test-summary.json
  fault-matrix.md
  security-review.md
  sbom.*
  capacity/
  soak/
  restore/
  canary/
  go-no-go.md
```

Large raw benchmark output may live outside Git, but the repository retains a
sanitized summary, checksum and immutable reference.

## Work ordering

The gates are executed in order because later evidence depends on earlier truth:

```text
truth/freeze
-> deterministic verification
-> security
-> durability and restore
-> fault recovery
-> capacity and soak
-> operability
-> canary and production declaration
```

Correctness, security and data-recovery failures always take precedence over
performance optimization. Performance changes require a before/after artifact.

## Deferred after production readiness

After Gate 27.7, product planning resumes from measured user value. The previous
ADR-0040 backlog remains candidate work, with these items deliberately later:

- Unified Explore, saved queries and Dashboards;
- advanced Alerts/Monitors and collaboration;
- teams and advanced authorization;
- MCP;
- online migrations and mixed-version rolling upgrades;
- distributed roles and horizontal scaling.

Production evidence may change their order. It does not silently authorize them.

## Consequences

- Faultkeep stops accumulating features until its existing value is operationally
  trustworthy.
- The initial production claim is intentionally narrower than broad Sentry parity.
- Single-process deployment remains simple and measurable.
- Basic coordinated offline recovery becomes mandatory; sophisticated online
  migration and zero-downtime operation remain deferred.
- The 100-million-Error/day objective becomes a hardware-specific verified result,
  never a Rust-language assumption.
- New feature phases start from a stable regression baseline rather than moving
  reliability targets.
