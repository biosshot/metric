# ADR-0039: Sequential module implementation plan

- Status: Accepted
- Date: 2026-07-21
- Completion: Phases 0-22 are complete; ADR-0040 owns completed Phases 23-26;
  ADR-0047 closes the former Phase 27 as obsolete; ADR-0045/0046 own completed
  Phases 28-38; ADR-0047 owns completed Phases 40/41 and selected Phase 42;
  ADR-0048 owns the completed unnumbered Unified Query v2 replacement

## Objective

Implement the accepted architecture as one modular Rust application, starting with
request ingestion and advancing only after each module passes its contract,
correctness, integration, performance, and cumulative E2E gate.

The plan produces useful vertical milestones without replacing ports with shortcuts
that would later need a rewrite. Optional features are added after the durable
Error-Event-to-Issue path is stable.

## Non-negotiable execution rules

1. Version one runs only `--role=all`.
2. Every phase begins by refining the owning module contract from ADR-0034.
3. Production modules communicate only through accepted domain values and ports.
4. A fake implements the same port contract as the real adapter; it cannot add a
   test-only capability that production lacks.
5. Tests are selected by risk; no global line-coverage target exists.
6. Hot or durable modules require load/fault tests and a recorded baseline.
7. Every completed module extends the cumulative E2E path when its real dependencies
   exist.
8. A failing correctness, bounded-resource, recovery, or performance gate blocks the
   next dependent phase.
9. Health, metrics, safe tracing, cancellation, and graceful shutdown are implemented
   inside each phase rather than appended at the end.
10. New persistent fields or cross-module dependencies require an architecture check.

Online MongoDB migrations, guaranteed backup/restore, MCP, split roles, NATS,
sharding, and disk spool are outside this plan. Initial empty-schema bootstrap remains
required.

## Common phase gate

Every phase closes with a short committed report containing:

```text
accepted module contract and public error codes
implemented resource limits and cancellation behavior
unit/property/golden/fuzz results selected for the module
real-adapter integration and failure results where applicable
load/soak baseline for hot or durable work
cumulative E2E scenarios added or intentionally not yet possible
metrics, health contribution and safe log fields
known limits and deliberately deferred cases
```

Performance baselines record the exact hardware, toolchain, MongoDB/backend version,
configuration and fixture corpus. A faster result obtained by dropping validation or
durability does not pass.

## Stage A: foundation and durable ingestion

### Phase 0 — Minimal workspace foundation

This is scaffolding, not a product module and not a substitute for starting with
Ingest.

Implement:

- the ADR-0034 Cargo workspace and dependency-graph CI check;
- `domain`, `ports`, `sentry-protocol`, `application`, adapter, `server`, and `testkit`
  crate skeletons;
- bounded ID, byte-size, duration, timestamp, error and cancellation primitives;
- ADR-0035 typed configuration, secret references and redacted effective config;
- structured tracing/metrics facade, request ID and graceful-shutdown root;
- fast, infrastructure, fuzz and performance test commands;
- local development orchestration for the exact MongoDB version used by tests.

Do not implement Event behavior, generic repositories, a dependency-injection
framework, migrations, or unused adapter abstractions.

Exit gate:

- dependency graph contains no cycle or forbidden adapter import;
- `--check-config`, `/live`, shutdown and redaction tests pass;
- workspace test commands run from a clean checkout;
- empty modules add negligible idle tasks and memory.

### Phase 1 — Sentry HTTP Ingest with fake ports

Implement the first functional module:

- Sentry ingest routes required by supported SDK Error Events;
- streaming body read and explicitly supported content decompression;
- DSN/auth parsing and URL/Envelope project consistency checks;
- Envelope header and Item framing with bounded counts and sizes;
- Error Event extraction, deterministic Event ID and `AcceptedEvent` construction;
- mixed enabled/disabled Item classification from ADR-0018;
- input limits, timeouts, request permits and overload responses;
- pre-storage PII policy application with a default/fake project snapshot;
- `ProjectResolver`, `EventSink`, `OutcomeSink`, clock and randomness ports;
- HTTP result mapping for durable success, duplicate, invalid, too large, limited and
  temporarily unavailable outcomes.

Production composition must not acknowledge fake durability; the fake EventSink is
available only to tests and benchmark binaries.

Tests/gate:

- real and captured Sentry SDK Error Event fixtures;
- golden Envelope/auth/response vectors;
- malformed, truncated, conflicting-project and decompression-bomb cases;
- mixed disabled Items and client reports;
- parser/property tests plus retained fuzz regressions;
- slow body, cancellation, timeout and permit exhaustion;
- bounded-allocation/RAM assertions;
- HTTP load against deterministic fake ports, demonstrating that parsing is not the
  expected 20,000 Event/s burst bottleneck on recorded reference hardware;
- first black-box E2E: `HTTP -> Ingest -> fake durable outcome -> response`.

### Phase 2 — Project identity, DSN resolution, and minimal control storage

Implement enough real control plane for authenticated ingestion without yet building
the full Web identity system:

- empty MongoDB schema marker/bootstrap for organizations, projects and project keys;
- random IDs, organization/project slugs and DSN-key creation;
- typed internal ProjectService commands usable from testkit/bootstrap tooling;
- project acceptance snapshot and capability/PII/limit configuration;
- `project_keys._id` lookup and bounded positive/negative cache;
- active/disabled/deletion-fenced state and generic unauthorized response;
- cache invalidation in the single process.

Tests/gate:

- real MongoDB identity/uniqueness and authorization integration;
- cross-project DSN mismatch, disabled/deleted key and collision paths;
- cache coalescing, TTL, invalidation and bounded-capacity tests;
- lookup latency/load at and above the ADR-0037 burst rate;
- E2E: real ProjectResolver plus fake EventSink.

### Phase 3 — Event BSON codec, Mongo EventStore, and MongoWriter

Implement durable acceptance:

- ADR-0022 compact pending Event BSON codec and deterministic composite `_id`;
- collection validator and initial Event indexes;
- domain-oriented EventStore port and MongoDB adapter;
- MongoWriter micro-batches using wait/documents/bytes thresholds;
- unordered inserts, duplicate idempotency and per-request partial-result mapping;
- ambiguous acknowledgement handling and safe SDK retry behavior;
- newly stored payload handoff port without an extra RAM copy;
- writer shutdown drain and explicit rejection after shutdown fence;
- accepted/duplicate/storage outcomes and batch metrics.

Tests/gate:

- codec round trip, malformed BSON and golden byte-size corpus;
- real MongoDB unordered/partial/duplicate/ambiguous failure tests;
- batch threshold, timer, byte cap, cancellation and shutdown cases;
- crash/retry verification that one Event ID yields one durable document;
- module throughput, occupancy and p95/p99 latency baseline;
- E2E: `official SDK -> HTTP -> Ingest -> MongoWriter -> MongoDB -> response`;
- ADR-0037 sustained and bounded burst tests on declared hardware with zero
  acknowledged Event loss.

Milestone A: official SDK Error Events are durably and idempotently accepted.

## Stage B: processing and Issue creation

### Phase 4 — Dispatcher and bounded durable-backlog refill

Implement:

- bounded Processor RAM queue and fresh-payload offer;
- queued/running Event-key deduplication;
- low-watermark, idle and startup MongoDB pending refill;
- retry time ordering and project deletion fence;
- no payload duplication between accepted handoff and queue;
- deterministic shutdown and restart recovery;
- queue/backlog/oldest-pending metrics.

Use a controlled fake WorkHandler until Processor exists.

Tests/gate:

- deterministic queue/refill simulations;
- full queue with continued durable ingest;
- large preloaded backlog, duplicate offers and retry scheduling;
- restart/soak/fault tests with bounded memory;
- refill throughput above the later Processor consumption target;
- E2E: durable Event reaches fake WorkHandler exactly once concurrently, while
  retry after crash may safely reschedule it.

### Phase 5 — Normalizer

Implement the pure deterministic normalization contract:

- timestamps, exception chains, frames, tags, request/user/context and breadcrumbs;
- platform, level/logger, release/dist/environment and compatible unknown fields;
- canonical absent/default representation;
- diagnostics for bounded recoverable normalization problems;
- stable normalized domain Event independent of BSON and wire DTOs.

Tests/gate:

- SDK-family golden vectors and pathological nested inputs;
- property tests for idempotent normalization and bounds;
- no database/network access and deterministic output;
- allocation/CPU baseline across ADR-0037 fixture sizes;
- retained fuzz regressions for structured Event fields.

### Phase 6 — Symbolication application boundary with baseline behavior

Implement only the application-owned stage contract initially:

- determine not-required versus native/JavaScript work;
- raw-frame preservation and domain result/error types;
- bounded retry/permanent classification hooks;
- a production baseline behavior that finalizes ordinary non-symbolicated Error
  Events and records missing/unavailable symbolication without blocking forever;
- fake adapter conformance suite used by Processor tests.

Do not yet implement debug upload, Symbolicator HTTP, source maps or external caches.

Tests/gate:

- not-required, complete, partial, missing, malformed and timeout vectors;
- guarantee that raw frames survive every outcome;
- concurrency/cancellation contract tests with fake adapter;
- backend wire types cannot appear in application/domain crates.

### Phase 7 — Grouper

Implement ADR-0014 as a pure domain module:

- canonical grouping component encoding and version registry;
- SDK fingerprint behavior;
- platform/native fallback strategies;
- deterministic Issue ID derivation;
- complete grouping explanation and selected components.

Tests/gate:

- golden grouping corpus across SDKs/platforms;
- property tests for determinism and canonical encodings;
- collision/corruption fixtures and version pinning;
- semantic regression suite showing expected merge/separation;
- CPU/allocation baseline with no storage dependency.

### Phase 8 — IssueService and Issue persistence

Implement:

- ADR-0024 compact Issue codec and indexes;
- deterministic creation/upsert by grouping identity;
- first/last Event and release pairs;
- occurrence count's accepted approximate semantics;
- open/resolved/ignored, regression and activity transitions;
- assignment representation without deferred team semantics;
- idempotent retry behavior and bounded title search projection.

Tests/gate:

- codec/property/byte-size golden tests;
- real MongoDB concurrent create/update and lifecycle integration;
- repeated Event processing and crash-window count drift cases;
- high-contention same-Issue and distributed-many-Issue load profiles;
- query `explain` baselines for initial Issue list/title search.

### Phase 9 — Finalizer, hourly buckets, Releases and Environments

Implement one bounded FinalizeBatch owner:

- replace pending Event body with normalized compact canonical body;
- persist Issue association, derived frames/diagnostics and terminal state;
- batched Issue and `issue_stats_hourly` updates;
- Release/Environment cardinality gates and batched materialization;
- Search v1 exact-token projection;
- notification intent hook without external delivery;
- retry-safe ordering and accepted approximate counter gaps.

Tests/gate:

- real MongoDB Event/Issue/bucket/catalog integration;
- crash injection before/after every acknowledged step;
- retry does not create a second Event or Issue identity;
- hourly/release/environment bounds and retention timestamps;
- FinalizeBatch contention and throughput baseline;
- production-shaped query/index explains.

### Phase 10 — Processor orchestration

Implement:

- ordered Normalizer -> Symbolication -> Grouper -> IssueService -> Finalizer stages;
- stage deadlines/cancellation and stable temporary/permanent error mapping;
- retry attempt/backoff update and terminal failure behavior;
- bounded processing concurrency and graceful drain;
- project deletion/disabled capability fences;
- processing latency/outcome metrics.

Tests/gate:

- complete stage state matrix with fake ports;
- real MongoDB restart and retry recovery;
- slower-than-ingest backlog growth, guard activation and recovery;
- zero permanently pending Events after terminal classification;
- end-to-end processing rate can recover at the ADR-0037 target ratio;
- cumulative E2E: `official SDK -> durable Event -> Processor -> Issue/hourly stats`.

Milestone B: accepted Error Events become normalized, searchable Issues.

## Stage C: usable investigation product

### Phase 11 — Users, sessions, tokens, authorization, and audit

Complete ADR-0021:

- bootstrap first owner and organization;
- users, memberships and role/permission expansion;
- Argon2id password setup/login;
- opaque Web sessions, CSRF and revocation;
- personal API tokens and scope intersection;
- bounded audit log for administrative mutations;
- shared AuthContext and command authorization middleware.

Tests/gate:

- final-owner, demotion, disabled-user and token-revocation cases;
- session expiry/rotation, CSRF and generic login failure;
- cross-organization/project authorization matrix;
- secret redaction and audit allowlist tests;
- real MongoDB security integration and login rate-limit load.

### Phase 12 — Native query/command API and MongoDB Search v1

Implement the minimal `/api/v1` contract from ADR-0036:

- project/key/policy commands;
- Issue list/detail/statistics/lifecycle/activity;
- Event list/detail and bounded Search v1;
- Release/Environment lists;
- capabilities and authenticated component status;
- descriptive DTO mapping, stable errors and opaque keyset cursors;
- no raw MongoDB filter/projection surface.

Tests/gate:

- DTO/error/cursor golden contracts;
- permission matrix for every route;
- pagination stability under concurrent inserts;
- search grammar/cardinality/limit security tests;
- real dataset query explains and p95/p99 latency baseline;
- E2E: create project -> SDK Event -> Issue query -> lifecycle mutation.

### Phase 13 — Minimal Web

Implement the ADR-0036 screens as a thin `/api/v1` client:

- login/bootstrap and organization/project navigation;
- DSN setup instructions;
- Issue list/detail/statistics/activity;
- Event raw/derived detail;
- retention/PII/key settings;
- capability and degraded-component presentation.

Tests/gate:

- browser login/session/CSRF E2E;
- project isolation and permission-dependent controls;
- deterministic fixtures for empty/error/loading/large stack states;
- key investigation flow on supported browsers;
- accessibility smoke checks and bounded large-Issue rendering;
- no hidden direct server/database path outside `/api/v1`.

Milestone C: a user can configure an SDK and investigate/manage an Issue in Web.

## Stage D: core operational completion

### Phase 14 — Scheduler, retention, counters, and narrow reconciliation

Implement:

- due retry/backlog maintenance not owned by continuous Dispatcher refill;
- Event/hourly retention and gradual policy reductions;
- upload/chunk expiry hooks even while optional upload modules are disabled;
- approximate count/quota reconciliation owned by existing modules;
- typed Blob orphan task registration for later Blob modules;
- standard task leases/exclusion appropriate to one process;
- task lag/failure metrics and bounded retry.

Tests/gate:

- fake-clock deterministic schedules;
- retention never deletes pending Events;
- task failure isolation and process restart;
- foreground ingest load while retention/maintenance runs;
- bounded scans and no unindexed unbounded work.

### Phase 15 — Project deletion and core capacity protection

Implement ADR-0030 for datasets available so far:

- pending-delete grace/cancel and DSN fence;
- local work drain and project deletion job;
- versioned deletion-plan registry requiring every dataset classification;
- bounded MongoDB purge and permanent project tombstone;
- Event/Issue/statistics/search/control-plane cleanup;
- ingest backlog and local filesystem capacity guards;
- operational status and audit.

Tests/gate:

- delete/cancel authorization and key restoration rules;
- crash/restart at every phase and idempotent batch repetition;
- in-flight ingest/Processor fence and final rescan;
- large-project deletion concurrent with another active project;
- schema test fails for an unclassified collection/namespace.

Milestone D: the core Error tracking product has bounded retention, maintenance,
deletion, overload behavior and standard operability.

## Stage E: Blob and native-debugging modules

### Phase 16 — Local BlobStore, attachments, and standalone minidumps

Implement:

- typed local filesystem BlobStore with temporary/final atomic publication;
- path containment, checksums, streaming limits and disk reserve;
- event-owned attachment blob-first commit protocol;
- enabled safe attachment policies and metadata;
- standalone minidump ingestion and synthetic Event relation;
- orphan cleanup and parent Event retention behavior.

S3 remains a later adapter.

Tests/gate:

- BlobStore conformance, crash publication and path traversal tests;
- streaming memory/size/decompression limits;
- Mongo/blob failure matrix proving no accepted missing attachment;
- minidump multipart compatibility corpus;
- bytes/s, concurrency, disk-full and slow-filesystem load;
- E2E SDK attachment/minidump -> Event -> authorized metadata/download behavior.

### Phase 17 — Debug-file upload and external Symbolicator adapter

Implement:

- Sentry CLI chunk discovery/upload and whole-file assembly;
- compact `debug_uploads`/`debug_files` codecs and quota counters;
- bounded parsing and immutable Blob publication;
- private project Symbolicator index/download callback;
- external Symbolicator domain adapter with timeout/circuit/concurrency controls;
- native processing results and cache revision;
- ADR-0033 exact-ID delete and orphan cleanup.

Tests/gate:

- pinned real `sentry-cli` DIF upload contract;
- DebugId/CodeId codecs and malformed debug corpus;
- chunk retry/expiry/assembly crash recovery;
- private authorization and cross-project isolation;
- fake and pinned external Symbolicator contract tests;
- cache-hit/miss and backend-failure load profiles;
- E2E native crash -> upload symbols -> newly processed symbolicated frames.

### Phase 18 — JavaScript Artifact Bundles and source maps

Implement:

- Sentry CLI Artifact Bundle assemble contract;
- safe bounded Source Bundle validation;
- compact ready/upload BSON, bindings and Debug ID tokens;
- private Symbolicator JavaScript lookup/download callback;
- modern Debug ID and legacy release/dist resolution;
- artifact revision, quotas, association removal and ADR-0031 GC.

Tests/gate:

- pinned real `sentry-cli sourcemaps` contract;
- malicious ZIP/manifest/path/compression corpus;
- same-array `$elemMatch` lookup/query explains;
- shared binding, rescue, GC and crash/republication state tests;
- Symbolicator JS integration with generated/raw frames preserved;
- E2E minified JavaScript Event -> uploaded bundle -> readable mapped frames.

Milestone E: native and JavaScript Error Events support private uploaded debugging
artifacts through a replaceable symbolication boundary.

## Stage F: investigation and background extensions

### Phase 19 — Incident Capsule

Implement ADR-0038:

- bounded IncidentCapsuleService and `/api/v1` streaming command;
- stable Issue/Event/statistics/activity export DTOs;
- ZIP64 writer, entry BLAKE3 manifest and omissions;
- authorization/audit and fixed path allowlist;
- no server persistence, attachment bytes or full debug/source artifacts.

Tests/gate:

- independent reader and golden capsule corpus;
- corruption, duplicate path, traversal and archive-limit tests;
- cancellation/backpressure and maximum-size memory tests;
- project/Issue/Event authorization matrix;
- E2E Issue -> capsule -> independent validation.

### Phase 20 — Notification outbox and webhook delivery

Implement ADR-0016:

- Issue transition intent expansion;
- idempotent notification delivery documents;
- bounded due queue, retries, expiry and destination fairness;
- webhook signing, SSRF controls, redirect/time/size limits;
- secret storage/redaction and activity/audit boundary.

Tests/gate:

- crash windows and duplicate delivery prevention contract;
- controlled webhook server integration and signature vectors;
- SSRF/DNS/redirect adversarial corpus;
- retry/backoff/expiry fake-clock tests;
- noisy destination/project load and E2E Issue transition -> webhook.

### Phase 21 — S3-compatible BlobStore and optional cold archive

Implement only after local Blob contracts are stable:

- S3-compatible BlobStore conformance adapter;
- multipart/atomic-publication semantics and checksum verification;
- Parquet/Zstandard Event archive writer and manifests;
- archive-before-hot-delete ordering;
- archive failure that preserves MongoDB Events;
- project deletion/retention support for new namespaces.

Tests/gate:

- conformance suite shared with local BlobStore;
- emulator plus selected real-compatible service matrix;
- multipart interruption, retry, missing object and permission failures;
- archive manifest crash points and checksum verification;
- foreground load with archive work and bounded memory;
- E2E archive completion before hot Event expiry.

Milestone F: optional export, notification and cold-storage features are modularly
available without changing the core ingest/processing contracts.

## Stage G: release hardening

### Phase 22 — Full-system verification and packaging

Run and publish:

- the complete ADR-0036 SDK/CLI compatibility matrix for enabled capabilities;
- all crate contract/conformance suites from a clean environment;
- security/adversarial corpora and extended fuzz regression runs;
- ADR-0037 5,000/s steady, 20,000/s burst, backlog recovery and restart suites;
- long soak with retention, Scheduler, Web queries and enabled add-ons;
- capacity report with actual BSON/index/storage measurements;
- graceful shutdown/restart and dependency degradation matrix;
- container image and simple all-in-one deployment using MongoDB and local BlobStore;
- optional external Symbolicator configuration without bundling its image by default;
- configuration, operations, supported-capability and known-limit documentation.

Release gate:

- zero lost acknowledged Events and zero duplicate durable Event identities;
- no unbounded queue/task/cardinality discovered in soak;
- passing security/tenant-isolation suite;
- passing compatibility rows match published claims;
- performance results identify hardware and do not hide overload responses;
- every enabled collection/Blob namespace is registered for deletion/retention;
- no unresolved critical/high defect in an enabled module.

Closure scope amendment (2026-07-24):

- the version-one required SDK subset is the versioned
  `release_required_families` set in the compatibility manifest: Python, Java and
  .NET;
- retained bounded-resource/restart tests and the recorded short Windows
  correctness/saturation profiles are accepted for this Phase 22 development
  release instead of a new long soak or controlled-duration load run;
- ADR-0037's 60-minute steady, five-minute burst and long production-shaped soak
  remain future production-capacity evidence and are not claimed by this closure.

Milestone G: version-one release candidate.

## Cumulative E2E ladder

The mandatory E2E chain grows in this exact order:

```text
1. HTTP -> fake durable outcome
2. SDK -> Ingest -> MongoDB
3. SDK -> MongoDB -> Dispatcher -> fake WorkHandler
4. SDK -> Processor -> finalized Event -> Issue/buckets
5. authenticated API -> Issue/Event query and commands
6. browser -> project setup -> SDK -> Issue investigation
7. attachment/minidump -> BlobStore -> Event
8. native symbols -> Symbolicator -> derived frames
9. source maps -> Artifact Bundle -> mapped JS frames
10. Issue -> Incident Capsule
11. Issue transition -> webhook
12. Event -> archive manifest/object -> hot retention
```

Earlier rungs remain in CI after later rungs exist. A high-level E2E test never
replaces the lower module contract and failure suites.

## Deliberately outside this implementation plan

- MCP runtime and tools;
- transaction, span, profile, session, replay, check-in, metrics/log and feedback
  processing;
- split application roles, inter-role wire protocol, NATS and distributed claims;
- disk spool;
- MongoDB sharding/partition implementation;
- second Storage or Search backend;
- online migrations and rolling mixed-version deployment;
- guaranteed application-consistent backup/restore and universal reconciliation;
- teams, SSO/SCIM/MFA/passkeys and advanced permission models;
- ProGuard/IL2CPP/BCSymbolMap/Hermes-specific extended pipelines.

Each item requires its own accepted boundary and tests before being enabled; none may
silently enter an earlier phase.

## Change policy

This plan is sequential but not calendar-based. Estimates are created only when a
phase is about to start and its contract/fixtures are available. If a module reveals
an architectural contradiction, implementation stops, the relevant ADR is amended,
and the phase restarts from its contract gate.

A phase may be split into smaller internal work items, but its public exit gate cannot
be weakened to claim completion. Reordering phases requires documenting the new
dependency path and preserving every cumulative E2E rung.
