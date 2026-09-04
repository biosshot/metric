# ADR-0049: Automatic crash-resumable schema migrations

- Status: Accepted
- Date: 2026-09-04
- Implementation: Complete (2026-09-04)
- Amends: ADR-0035 and ADR-0047
- Initial storage effect: none; schema generation remains 19

## Context

Metric currently bootstraps an empty MongoDB database directly at schema generation
19 and accepts an existing database only when its marker, collection set, validators
and indexes match generation 19 exactly. An older, newer, incomplete or unknown
schema fails startup. ADR-0035 deliberately deferred online migration until the
persistent model and executable application existed; that prerequisite is now met.

Requiring an operator to run a second command before starting a new container does
not create a meaningful safety decision: the new binary cannot serve against the old
schema, so applying the update already authorizes its required migration. Interactive
confirmation is also a poor fit for Docker. Metric therefore needs one automatic
startup path that remains observable, bounded in application memory and recoverable
after process or host interruption.

The supplied MongoDB deployment is a standalone server. It does not support
multi-document transactions, and MongoDB collection, validator and index operations
cannot be universally rolled back even on a replica set. The useful guarantee is
therefore resumable forward progress, not a misleading promise of general rollback.

## Decision

### Scope and ownership

The MongoDB adapter owns schema inspection, the migration registry, leases,
checkpoints, MongoDB operations and target-schema verification. The server composition
root owns startup lifecycle, maintenance HTTP behavior and readiness. Domain,
application and ordinary storage ports do not receive MongoDB migration types.

```text
crates/mongo   generation chain, migration execution and verification
crates/server  startup state, maintenance responses and runtime activation
```

This is intentionally a MongoDB implementation, not a universal migration language.
A future PostgreSQL adapter may publish the same small startup-progress model while
using its own DDL, transaction and locking semantics.

The first implementation adds no public `metric migrate`, `plan`, `apply`, approval
or downgrade command. Deploying and starting the new Metric image is the migration
authorization.

### Generation chain

The binary declares one target `SCHEMA_GENERATION` and an ordered immutable registry
of adjacent transitions:

```text
N -> N + 1 -> N + 2 -> ... -> SCHEMA_GENERATION
```

Startup validates the complete registry path before making a schema change. Duplicate
sources, non-adjacent transitions and a missing link are programming errors covered by
deterministic tests and fail closed at runtime. A database newer than the binary, a
non-empty database without Metric metadata and an unrecognized in-progress transition
remain incompatible.

An empty database is bootstrapped directly at the current target generation. It does
not replay historical migrations.

This framework release keeps `SCHEMA_GENERATION` at 19 and ships no production
transition. Its real adapter behavior is exercised with injected test migrations and
real MongoDB. The first production transition will be generation 19 to 20 when a
product feature, currently expected to be Telegram configuration, requires a real
schema change. No artificial `schema_migrations_v1` module or metadata-only generation
is introduced merely to exercise the framework.

### Durable state and forward-only recovery

The singleton `schema_meta` document remains authoritative. While complete,
`generation` is the last fully verified generation and `state` is `complete`. During
a transition it retains that generation and records bounded progress:

```javascript
{
  _id: "metric.schema",
  generation: 19,
  state: "migrating",
  migration: {
    from: 19,
    to: 20,
    name: "telegram_configuration",
    step: 2,
    total_steps: 4,
    cursor: <optional bounded BSON value>,
    processed: 12500,
    warning_counts: { malformed_optional_value: 3 },
    owner: "<random process identifier>",
    lease_until: <MongoDB datetime>
  }
}
```

The document never stores source documents, unbounded identifier samples, exception
strings or user data. Warning keys come from a closed migration-owned set.

Each step performs its idempotent side effect, verifies the acknowledged result and
only then advances the checkpoint. A crash before the checkpoint may repeat the last
operation; every step and batch must tolerate that repetition. After every complete
transition, target verification succeeds before one conditional update changes
`generation` to the target, restores `state: "complete"` and removes the temporary
progress object. A later transition then starts from that complete boundary.

There are no reverse migrations. An older binary refuses a newer complete generation
or an in-progress transition it does not understand. BlobStore backup and restore are
separate future work and do not gate this framework.

### Lease and cancellation

One supported Metric process normally owns the database, but a renewable MongoDB
lease prevents accidental concurrent migration and permits takeover after a crash.
Lease acquisition and checkpoint updates use conditional writes over the generation,
transition and owner. An active non-owned lease causes the process to wait and report
maintenance progress. An expired lease may be claimed by a new process.

The owner renews the lease independently of document batches. Before publishing a
checkpoint it verifies ownership. Because a paused process can outlive its lease, the
lease is not treated as a substitute for idempotency: steps must also tolerate rare
overlapping re-execution.

The server installs its operating-system shutdown handler before schema inspection.
A shutdown signal cancels the startup migration future, closes the maintenance
listener through the normal shutdown fence and leaves the last acknowledged
checkpoint intact. Metric does not advertise that an arbitrary MongoDB DDL operation
can be cancelled atomically. A stop during an operation is recovered by the same
idempotent replay rule after the lease expires.

Multiple active application replicas and mixed-version rolling upgrades remain
unsupported. During migration no application worker or ordinary request handler may
read or write the changing schema.

### Migration and record-error policy

Every transition declares:

1. adjacent source and target generations and a stable name;
2. a source-schema preflight;
3. a finite ordered set of named, idempotent steps;
4. bounded retry and record-error behavior for each step;
5. step verification and full target-schema verification;
6. progress units safe to expose without database names, values or identifiers.

Record anomalies are not all process-fatal. A step deliberately selects one of these
closed policies:

- `Strict`: every affected source record must satisfy the invariant; an ambiguous or
  unrepresentable record blocks the transition.
- `Tolerate`: named record errors may be skipped or retained only up to explicit
  migration-owned count/rate bounds and only when the target runtime can still read
  them safely.
- `Rebuildable`: derived data identified by its owning ADR may be discarded and
  rebuilt according to a migration-specific procedure.

There is no global percentage that silently permits damage to arbitrary data.
Identity, authorization, tenant scope, project keys, schema metadata and source data
default to `Strict`. Tolerated and rebuildable outcomes use bounded counters and emit
one aggregate safe log; they do not log every record.

Execution outcomes are classified as:

- `Warning`: an explicitly tolerated record anomaly; count it and continue;
- `Retryable`: a transient MongoDB, network or lease condition; retry inside the same
  process with exponential backoff capped at a fixed interval while maintenance mode
  remains available;
- `Blocked`: a missing chain, newer schema, failed required invariant, exhausted
  migration-owned tolerance or ambiguous transformation; return a redacted stable
  startup error and terminate with a nonzero status;
- `Fatal`: the process cannot keep its control surface trustworthy, for example
  invalid configuration, listener failure or an internal state-machine invariant;
  terminate with a nonzero status.

Only `Blocked` and `Fatal` terminate startup. Transient failures do not delegate their
retry cadence to Docker, and isolated tolerated anomalies do not cause a restart
loop. The existing Compose restart policy may repeatedly restart a genuinely blocked
binary; that condition is intentional and must retain a stable diagnostic rather
than mutating or deleting data on subsequent attempts.

### Bounded document transformation

A migration never collects an entire MongoDB cursor or BlobStore namespace into
application memory. It prefers MongoDB update operators and projections over decoding
and replacing complete documents. When Rust transformation is necessary it:

1. scans by a stable keyset cursor, normally ascending `_id`;
2. projects only fields required by the old and target encodings;
3. limits a batch by both document count and encoded BSON bytes;
4. transforms records incrementally and emits bounded write models;
5. verifies the acknowledged batch before persisting its cursor;
6. releases batch memory before requesting the next page.

The write predicate includes the expected old form where practical. A replayed batch
therefore either reapplies an idempotent `$set`/`$unset`/`$rename` or observes that the
record is already in its target form. Historical BSON is decoded by migration-local
code in the MongoDB adapter rather than current domain models that may no longer
accept it.

Validator changes use an expand-transform-contract sequence: temporarily accept old
and new forms, transform in bounded batches, verify the result and only then install
the target validator. Index creation is a separate named step executed by MongoDB;
its server-side memory and disk cost remains a release-specific migration concern.

Blob changes, when eventually required, stream one bounded object at a time and have
their own durable cursor. This ADR does not add a Blob migration.

### Maintenance HTTP lifecycle

After configuration and tracing are valid, the server binds its configured listener
before MongoDB migration and exposes a restricted startup gate. This explicitly
amends ADR-0035's earlier bind-after-composition order.

Before runtime activation:

- `GET /live` returns `200` while the process event loop is alive;
- `GET /ready` returns `503` with a stable startup/migration status;
- browser navigation returns a self-contained `503 Service Unavailable` maintenance
  page;
- API, SDK ingest and other non-probe requests return bounded JSON `503` with
  `Retry-After`;
- no ordinary application route reaches a partially constructed service or changing
  database.

The maintenance page has no dependency on Vue, MongoDB authentication state, remote
assets or BlobStore. It follows ADR-0041's dark neutral visual rules, uses explicit
text in addition to shape/color, supports English and Russian through
`Accept-Language`, and remains usable at narrow widths and with reduced motion.

Progress is honest and bounded. The page displays a determinate bar for known
migration steps, `step X of Y`, and the exact number of remaining steps. A long
document step may additionally expose processed and warning counts. Remaining time or
remaining documents are not invented, and Metric does not add an expensive full
collection count only to make the page look more precise. The page refreshes itself
through a minimal local mechanism and never exposes collection names, cursors or
error details.

After schema verification, BlobStore checks, service construction and required worker
startup succeed, the in-process gate publishes the complete application router and
readiness becomes true. Runtime activation is atomic from the request path's point of
view. During the migration window ingestion is unavailable and clients receive 503;
this release does not claim zero-downtime schema upgrades.

### Verification gate

The framework is not complete until it has:

- deterministic registry, chain, progress and error-policy tests;
- crash-window tests before and after side effects and checkpoints;
- real MongoDB tests for lease acquisition, expiry/takeover, bounded cursor resume,
  idempotent replay and final generation publication;
- server tests proving no application request crosses the startup gate;
- maintenance HTML/JSON, locale, progress, probe, security-header and narrow-layout
  checks;
- a Docker startup test against the supplied standalone MongoDB topology;
- workspace Rust checks and Web checks when bundled assets change.

Production migration `19 -> 20` is not part of this framework change and will require
its own schema amendment, fixtures and real-data tests with the Telegram feature.

## Implementation

The MongoDB adapter implements the registry, durable `schema_meta` transition state,
renewable lease, capped in-process retries, explicit record-error policies and
target verification in `crates/mongo/src/migrations.rs`. Its reusable keyset helper
retains at most 1,000 projected documents and 16 MiB of encoded BSON per page, and
also limits the driver's count-based cursor batch. The production registry in
`crates/mongo/src/lib.rs` is empty and `SCHEMA_GENERATION` remains 19.

The server binds the HTTP listener before MongoDB inspection, maps migration progress
into the startup gate and activates the ordinary router only after final schema and
service checks. The gate serves localized maintenance HTML and bounded JSON, keeps
`/live` open, keeps `/ready` closed and uses a stable telemetry route label before
application activation.

Deterministic tests cover invalid and incomplete chains, bounded checkpoints,
warning budgets, non-advancing cursors and request gating. A real MongoDB 8.0.12 test
interrupts an injected migration both after a document side effect and after a
durable checkpoint, expires and takes over the lease, replays safely, verifies the
target and publishes the final generation. The workspace test suite, strict Clippy
gate and two-start Docker smoke test pass on 2026-09-04.

## Consequences

- Updating a future Metric image can migrate an older supported generation without an
  interactive Docker command.
- A process or host interruption repeats at most a bounded idempotent unit and resumes
  from durable progress.
- Application memory remains bounded independently of collection size.
- Tolerated record anomalies are explicit and aggregated rather than turning every
  imperfect document into a process restart.
- Truly incompatible or ambiguous migrations still fail closed and exit nonzero.
- The Web explains planned unavailability and exposes truthful step progress while
  readiness remains closed.
- Downgrade, automatic backup/restore, mixed-version rolling upgrades and a generic
  cross-database migration DSL remain outside this decision.
