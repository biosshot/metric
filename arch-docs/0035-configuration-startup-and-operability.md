# ADR-0035: Configuration, startup checks, and standard extensible operability

- Status: Accepted
- Date: 2026-07-21

Current amendment (2026-08-01): ADR-0047 closes the broad Phase 27 operability
program and removes a Prometheus endpoint and built-in backup/restore commands from
the selected roadmap. The instrumentation facade and low-cardinality rules below
remain valid, but no Prometheus exporter is claimed by the current runtime.

## Context

The first release needs predictable startup, safe secrets, and ordinary health and
metrics integration without building a custom monitoring platform. Online MongoDB
schema migration, coordinated backup/restore, and a universal cross-store repair tool
are useful but do not block implementing modules against an empty version-one schema.

Configuration and operability are cross-cutting module contracts: a module cannot be
self-contained if it reads arbitrary environment variables, initializes its own
logger, or invents unrelated health and metric formats.

## Decision

### Typed configuration and precedence

The composition root loads one immutable typed configuration with this precedence:

```text
explicit CLI override
-> APP__SECTION__FIELD environment override
-> TOML configuration file
-> documented default
```

The neutral `APP__` prefix avoids coupling deploy-time variable names to a product
name that may change. Nested fields use double underscores. Only a small bootstrap
CLI surface exists initially:

```text
--config <path>
--role all
--check-config
--print-effective-config   # secrets redacted
```

Unknown TOML fields and environment paths are errors rather than silently ignored
typos. Human sizes and durations accept documented units and are converted to bounded
strong types. Zero, absence, and `none` retain the explicit meanings in their owning
ADRs; generic configuration code does not reinterpret them.

Every module receives its validated configuration subtree from composition. Modules
do not call environment or file APIs directly.

### Secret references

Passwords, token-signing material, internal HMAC keys, and external credentials use a
typed secret reference:

```toml
mongodb_uri = { env = "MONGODB_URI" }
hmac_key = { file = "/run/secrets/internal_hmac" }
```

Literal secrets in TOML are allowed only in an explicit local-development mode and
produce a warning. Secret files are read with bounded size, surrounding line endings
removed only according to the secret type, and never included in effective-config,
panic, metric, or debug output.

The first version does not implement a vault client or automatic secret rotation.
Adapters receive opaque secret values and expose explicit rotation hooks only where
an accepted ADR already defines current/previous credentials.

### Static runtime configuration

Version one applies process configuration at startup and requires restart for a
change. It does not implement partial hot reload, which could leave modules observing
different limits. Project-owned runtime policy stored in MongoDB continues to change
through typed application commands and local cache invalidation.

A future reload design must publish one validated immutable snapshot atomically and
declare which fields are reloadable; it cannot call module-specific ad hoc reload
methods.

### Startup sequence

The `all` composition root starts in this order:

1. parse CLI and load/redact/validate configuration;
2. initialize structured tracing and process metrics;
3. construct clock, randomness, cancellation, and bounded executors;
4. connect to MongoDB and validate the initial schema marker/index contract;
5. initialize BlobStore and perform a bounded capability/capacity check;
6. initialize optional Symbolicator adapter and other noncritical dependencies;
7. construct application services and bounded channels;
8. start Scheduler and background workers;
9. bind HTTP listeners;
10. mark readiness only after required dependencies and workers are ready.

Failure before readiness exits with a stable redacted diagnostic and nonzero status.
Optional component failure can mark that component degraded without disabling Event
ingest when its capability is not required.

### Initial schema bootstrap; migrations deferred

An empty configured MongoDB database may be bootstrapped idempotently with the
version-one collections, validators, and indexes accepted by the ADR set. One
`schema_meta` record stores the exact schema generation and bootstrap state.

Version one does not implement online upgrades, data rewrites, downgrade, rolling
mixed-version compatibility, or background index migrations. Startup refuses an
unknown, incomplete, or newer schema rather than guessing. Changing an accepted
persistent codec before a migration framework exists requires recreating development
data or an explicit one-off development tool.

This bootstrap is not presented as a general migration system. The migration
framework is deliberately scheduled after the first stable schema and executable
module chain exist.

### Standard health endpoints

The server exposes conventional minimal probes:

```text
GET /live   process event loop is alive and not terminally shutting down
GET /ready  required dependencies and worker lifecycle permit traffic
```

Probe responses contain a status and optional stable component codes, never URIs,
credentials, database names, filesystem paths, user data, or internal errors.

Readiness requires MongoDB acceptance, required BlobStore capability when configured,
completed schema bootstrap/check, active writer/dispatcher lifecycle, and no shutdown
fence. Symbolicator unavailability is reported as degraded and does not fail generic
Event-ingest readiness. Capability-specific APIs still return their typed temporary
failure.

Deep administrative diagnostics use authenticated API commands rather than making
public probes expensive.

### Metrics and tracing

Metric instruments are registered through one application facade so modules do not
depend on an exporter. The current runtime does not expose a Prometheus endpoint and
ADR-0047 leaves exporter work outside the selected roadmap. A future exporter remains
a removable adapter and requires a new focused decision rather than changing module
code.

Stable low-cardinality dimensions include module, operation, outcome, item category,
and bounded error code. Project, organization, Event, Issue, release, URL, filename,
exception message, tag value, and user identity are forbidden metric labels.

The initial standard metrics cover:

```text
HTTP requests, active requests, latency and response class
accepted/rejected Envelope items and bytes
Mongo batch size/bytes/wait/latency/partial failures
RAM queue depth, low-water refill and dropped duplicate schedules
pending count estimate, oldest pending age and Processor throughput/latency
normalization/symbolication/grouping/finalization outcomes
MongoDB and BlobStore operation latency/errors
retention/archive/upload/GC/reconciliation work and lag
notification backlog/delivery outcomes when enabled
runtime CPU, memory, threads/tasks and shutdown duration
```

Structured logs and traces use a generated request correlation ID and stable operation
codes. SDK payloads, DSN keys, API tokens, secrets, source code, attachment data, and
unbounded error strings are never emitted. Sampling is configurable; errors and
administrative mutations retain bounded diagnostic events.

### Extension rule

Each module contributes a static health/metric descriptor at composition. Adding an
exporter or another module does not change probe JSON arbitrarily or give the module
direct access to an exporter client. Cold-path health dispatch may use ordinary trait
objects; it is outside the Event hot path.

The project does not implement its own pager, time-series database, or dashboard
engine. It ships documented alert recommendations for sustained readiness failure,
Mongo/Blob errors, disk reserve, queue saturation, oldest pending age, Processor lag,
TTL/archive lag, GC failures, and notification backlog. Operators connect standard
monitoring tools.

### Backup, restore, and general reconciliation deferred

Version one does not claim an application-consistent backup/restore command. Operators
may snapshot MongoDB and BlobStore using backend-native tooling, but documentation
must not describe independently timed snapshots as a guaranteed consistent restore.

A future protocol will define snapshot ordering, manifests, checksums, restore
validation, and point-in-time behavior. Likewise, a universal scanner comparing every
MongoDB record with every BlobStore namespace is deferred.

Correctness mechanisms already required by accepted write protocols are not removed:
attachment/debug orphan cleanup, quota-counter repair, artifact GC, Processor backlog
recovery, and project deletion continue to run. They remain narrow owners of their
own invariants rather than a premature universal repair subsystem.

## Consequences

- Configuration is deterministic, typed, brand-neutral, and redacted.
- Startup fails safely on an incompatible database instead of mutating it implicitly.
- Standard liveness/readiness probes and structured diagnostics are available
  without claiming a Prometheus exporter.
- New modules extend one low-cardinality operability contract.
- Online migrations and guaranteed backup/restore are explicitly postponed rather
  than accidentally half-implemented.

## Deferred questions

- Online/rolling schema migrations and downgrade policy.
- Application-consistent backup, restore, and disaster-recovery verification.
- Atomic validated hot reload for a deliberately selected field subset.
- Vault/KMS integrations and automated secret rotation.
