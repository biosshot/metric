# ADR-0032: Debug-symbol and Artifact Bundle retention and quotas

- Status: Accepted
- Date: 2026-07-21
- Amended: 2026-10-16

## Context

Raw Events and hourly statistics already have configurable retention, while original
debug files and JavaScript Source Bundles are deliberately persistent. Automatically
removing an old symbol object can make a retained historical Event harder to
understand, but unlimited uploads can fill a local disk or create unbounded object
storage cost.

Artifact Bundles are content-deduplicated across projects in one organization;
project-private debug files are not. Temporary organization chunks have neither
MongoDB documents nor exact reference counts. The quota model must reflect those
physical ownership rules without adding one accounting document per blob or a scan
to every upload.

## Decision

### Persistent by default

Ready debug files and Artifact Bundles have no automatic age expiration by default:

```toml
[symbol_storage.debug_files]
max_age = "none"

[symbol_storage.artifact_bundles]
max_age = "none"
```

An operator may explicitly configure a bounded age. Age is measured from the
server-controlled ready `uploaded_at`, not Event occurrence time or uploader
metadata. The system does not store or update exact `last_used_at`; avoiding that
field and symbolication write amplification is more valuable than automatic LRU
eviction.

Enabling age retention is an explicit acknowledgement that older retained Events may
lose original symbol/source context. Policy changes are audited and the UI/API show
this consequence rather than describing the operation as harmless cache cleanup.

### Reject new physical storage instead of evicting old context

Crossing a configured logical quota rejects the new physical publication. Version
one never silently evicts the oldest debug file or Artifact Bundle to make room.

An idempotent upload that already has ready physical content is not charged again.
Adding another binding to an existing Artifact Bundle consumes only bounded MongoDB
metadata and does not consume the organization physical-byte quota again.

Upload APIs return a stable `quota_exceeded` reason through their compatible error
shape. Temporary chunks already received remain governed by their short retention;
they do not force publication beyond quota.

### Physical ownership and charging

Physical ready storage is charged as follows:

```text
debug_files       size and count -> owning project
artifact_bundles  size and count -> organization, once per content object
bindings          no duplicated physical-byte charge
temporary chunks  approximate bytes -> organization temporary budget
```

Version one does not create a per-project Artifact Bundle byte quota. A shared bundle
cannot be assigned fairly without double charging or an arbitrary owner. Organization
physical quota is exact enough for the actual deduplication boundary. Project-level
upload rate and individual bundle-size limits remain available for abuse control.

The initial configuration surface is:

```toml
[symbol_storage.debug_files]
max_age = "none"
max_bytes_per_project = 0 # zero means unlimited
max_count_per_project = 0

[symbol_storage.artifact_bundles]
max_age = "none"
max_bytes_per_organization = 0
max_count_per_organization = 0

[symbol_storage.temporary_chunks]
max_bytes_per_organization = 0
retention = "24h"
```

Values are validated as nonnegative bounded quantities. An unlimited S3-compatible
deployment emits an operator warning because the application cannot discover a
meaningful remote free-space boundary.

### Compact approximate counters

Request-time quota admission uses compact counters rather than an aggregate scan:

```text
projects.db       ready/project debug-file bytes
projects.dc       ready/project debug-file count
organizations.ab physical Artifact Bundle bytes
organizations.ac physical Artifact Bundle count
organizations.tb approximate temporary-chunk bytes
```

Fields are absent for zero and are stored as nonnegative BSON `int64`. They are
capacity-control projections, not billing records. Ready metadata and physical
BlobStore objects remain the source of truth.

Before publishing new physical content, the durable upload worker performs a
conditional atomic counter increment that fails if the configured byte or count
limit would be exceeded. The upload job records that it passed reservation so normal
retries do not intentionally charge twice. A deduplicated ready hit bypasses physical
reservation.

MongoDB transactions remain disabled. A crash between a job-state update, counter
update, metadata publication, or deletion can cause small counter drift. The ordering
prefers conservative overcount where possible: temporary rejection is safer than
unbounded physical overshoot. Concurrency bounds cap the remaining undercount window.

Scheduler periodically reconciles:

- project debug counters from ready `debug_files` metadata;
- organization artifact counters from physical states that still retain `z`;
- temporary usage from bounded BlobStore namespace scans and object metadata.

Reconciliation writes only a changed counter and emits drift metrics. Counters are
never used for financial billing, exact customer invoices, or proof of deletion.

### Local filesystem capacity guard

Logical quota does not protect an installation whose filesystem is consumed by
MongoDB, logs, another process, or unlimited projects. The local BlobStore therefore
has a hard admission guard:

```toml
[blob_store.capacity]
min_free_bytes = "1GiB"
min_free_percent = 5
```

Both thresholds are checked against the filesystem containing the BlobStore root;
the larger required reserve wins. Bounds are configurable, but production mode
rejects disabling both without an explicit unsafe override.

The guard is checked before accepting a new temporary or final object and during a
streaming upload where the backend exposes updated capacity. Crossing it produces a
temporary storage-unavailable response and an operational alert. Event-only
ingestion may continue when it needs no BlobStore write; an Envelope requiring an
enabled attachment follows ADR-0012 and is not acknowledged without its blob.

S3-compatible BlobStore implementations use logical quotas and backend errors because
portable remaining-capacity discovery is unavailable.

### Temporary chunk budget

ADR-0025 intentionally stores no MongoDB document per chunk. Temporary quota is
therefore conservative and approximate: upload admission combines the coalesced
organization byte counter, object-existence checks for checksum retries, bounded
request/concurrency limits, and periodic namespace reconciliation.

Temporary quota is independent of ready debug/artifact quotas. A whole-file assembly
may fail ready quota even though all chunks were accepted. The compatible response
reports the permanent quota reason, and chunks age out normally.

### Optional age-retention worker

When `max_age` is configured, Scheduler incrementally scans ready metadata and
processes bounded batches. It never issues an unbounded deletion or relies on MongoDB
TTL to remove BlobStore data.

- Project-private debug files use ADR-0033's single-process exact-ID deletion and
  orphan-reconciliation protocol.
- Artifact Bundle policy removes its bindings, increments affected project artifact
  revisions, and uses ADR-0031 if the bundle becomes unreferenced.
- Physical counters are released only after the corresponding physical generation
  has been deleted or reconciled absent.
- A temporary backend failure leaves the object and its charge intact and retries
  with backoff.

The existing project/uploaded-time debug-file index supports debug retention.
Artifact Bundle retention reads each project's bundles, newest upload first,
through the existing project/upload-time index, so it adds no index.

### Artifact Bundle retention policy

Amended 2026-10-16. Age alone is the wrong rule for source maps: a CI pipeline that
uploads a bundle on every build would lose the maps of a release that still raises
Events, and a project that has not built for a month would lose all of them. The
implemented policy keeps a project's binding while any of these holds:

- the bundle is among the project's newest `retention_keep_latest` uploads;
- it was uploaded within `retention_max_age`;
- the binding's release has `last_seen` within `retention_max_age`.

```toml
[artifacts]
retention_mode = "off"   # "report" only logs, "enforce" removes
retention_max_age = "30d"
retention_keep_latest = 20
retention_interval = "1h"
```

The release rule protects only bindings that carry a release; bundles uploaded
without one rely on age and count. A release is organization-wide, so an Event of
the release in any project keeps it active. Event finalization already maintains
release `last_seen`, so the rule needs no `last_used_at` field, no write on
symbolication and no schema migration.

Each pass visits every active or disabled project that binds a ready bundle; a
disabled project's bundles still hold storage and quota. Projects in deletion are
left to ADR-0030, and a project that enters deletion during the pass is skipped:
binding removal refuses it, so the deletion job still finds every bundle whose
garbage collection it waits for. PostgreSQL checks the state under the project row
lock. MongoDB has no such lock, so a deletion request can land between its check and
its write. There the write that takes a bundle's last binding also records the
releasing project in `j`, as four bytes that never equal a sixteen-byte deletion
operation, and the deletion job finishes only when no bundle carries its operation
or its project's mark. A bundle the project released shortly before its deletion
therefore delays that deletion until garbage collection, at most by `orphan_grace`.
Pages follow the organization upload timeline, as lookups do. Releases are looked
up once per page and only their `last_seen` and version are kept, so a page bounds
the memory of a pass. `report` logs every binding past retention and a summary of the
pass, and changes nothing, so an operator sees what enforcement would remove before
enabling it. `enforce` removes those bindings through the exact-binding removal
above, conditioned on the upload time the pass read. A new upload of content that is
already stored refreshes that time in both backends, never moving it back; MongoDB
conditions its replacement on the time it read, so a slower concurrent publication
cannot move it back either. Lookups return the newest uploads first, so a refresh
that moves the time on advances the ADR-0028 artifact revision of every project bound
to the bundle, not only of new bindings, and cached lookups are not served stale.
PostgreSQL locks those projects with the ones being bound, in one id-ordered pass
before the organization, as every transaction that locks both does; a publication
that finds a project bound after it read them starts over. Age therefore counts from
the latest upload, and a binding published again after the pass read the page is
kept. A bundle left without bindings becomes an ADR-0031 orphan and is deleted, with
its quota charge released,
once `orphan_grace` has passed since its removal. A storage error ends the pass; the
next pass starts over.

Passes run in their own task: the first right after startup, then every
`retention_interval`. Garbage collection never waits for a pass, and shutdown drops
a pass in progress between storage calls, which leaves the state a crash there would.

### Project deletion

ADR-0030 bypasses ordinary age retention and quota decisions. It physically deletes
project-private debug files, removes Artifact Bundle bindings, waits for required
ADR-0031 garbage collection, and finally reconciles or removes the project's compact
counters. Shared Artifact Bundle bytes remain charged to the organization when
another project retains a binding.

## Consequences

- Symbols and source maps remain available indefinitely unless the operator explicitly
  chooses otherwise.
- Quota exhaustion fails a new upload instead of silently degrading historical
  Events.
- Physical deduplication and quota charging use the same organization boundary.
- Hot upload admission reads compact counters rather than aggregating all metadata.
- Counter drift is observable and repairable without pretending to be financial
  accounting.
- A local BlobStore cannot consume the filesystem's configured emergency reserve.
- Project-private debug deletion remains compact in the single-process runtime.

## Deferred questions

- Per-project logical Artifact Bundle quotas if organization-level controls prove
  insufficient.
- Measured reconciliation interval and batch defaults for very large symbol stores.
- Quotas for event attachments and cold archives beyond their retention policies.
- Billing-grade allocation accounting, which is explicitly outside version one.
