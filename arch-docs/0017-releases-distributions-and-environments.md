# ADR-0017: Releases, distributions, and environments

- Status: Accepted
- Date: 2026-07-21

## Context

Sentry SDK Events already carry optional `release`, `dist`, and `environment` fields.
Their protocol meaning must be preserved without letting malformed or high-cardinality
SDK values create an unbounded metadata catalog or additional writes per Event.

Release identity also needs to remain compatible with Sentry release APIs and future
Sentry CLI support, where the same version used by multiple projects in one
organization represents one Release.

## Decision

### Protocol meaning and normalization

The initial Event model accepts the Sentry-compatible optional fields:

```javascript
{
  release: "backend@1.4.2",
  dist: "104",
  environment: "production"
}
```

- `release` identifies a logical application version;
- `dist` distinguishes a build or distribution variant of that Release;
- `environment` identifies a deployment environment such as production or staging.

Normalizer preserves the exact validated value. It does not lowercase, infer SemVer,
append the environment, or otherwise rewrite identity. `production` and `Production`
are distinct. Parsed `package@version`, semantic-version, or commit-like metadata may
be derived for display but never replaces the exact identity string.

ADR-0028 resolves an artifact-bundle upload's optional exact version to this
organization-scoped Release identity. A bundle may serve several authorized projects
in the organization. Its optional dist remains exact and does not create a second
Release identity.

Protocol validation and normalization are covered by Sentry compatibility vectors.
Safety bounds follow the supported protocol rather than silently truncating identity
fields into collisions.

### Missing environment representation

If an SDK omits `environment`, the canonical Event BSON document omits the field. It
does not store BSON null and does not materialize the string `"unknown"`.

The public API represents absence as null or an equivalent optional value, and Web may
render a localized `Unknown` label. A literal SDK value `"unknown"` remains a real,
distinct environment.

Omission saves per-Event bytes and permits a future sparse or partial index to exclude
events without an environment. The server does not invent `production`; SDK defaults,
when present, arrive as ordinary explicit values.

The same omission rule applies independently to missing `release` and `dist`.

### Organization-scoped Release identity

A Release is scoped to an organization and may be associated with several projects:

```text
release_id = BLAKE3(organization_id || canonical_length_prefix || exact_version)
```

The separator is an unambiguous canonical encoding, not raw string concatenation.
Two projects in one organization using the same exact version reference one Release.
The same version in another organization has another identity.

The initial logical Release document is:

```javascript
{
  _id,
  organization_id,
  version,
  status: "open" | "archived",
  project_ids,
  first_seen,
  last_seen,
  first_event_id,
  latest_event_id,
  created_at,
  released_at,
  ref,
  url,
  source: "event" | "api"
}
```

`project_ids` records projects observed or explicitly associated with the Release.
Release status affects catalog presentation and does not delete Events.

### Implicit Release materialization

An Event may reference a Release that was not created through an API or CLI. Processor
therefore supports implicit Release creation. FinalizeBatch groups unique
`(organization_id, release)` pairs and performs batched idempotent upserts rather than
one metadata request per Event.

The upsert creates identity and source metadata, adds the project association, and
updates first/last occurrence metadata. A bounded in-memory known-Release cache may
avoid redundant creation checks, but MongoDB remains the source of truth and every
operation remains idempotent after restart.

### Release catalog cardinality gate

Implicit catalog creation is configurable:

```toml
[releases]
max_implicit_per_project_per_day = 1000
```

The day is a UTC server-receipt day. Admission usage is kept in compact project
metadata and may have the same small crash-window drift accepted for non-financial
operational counters. Explicit authorized API/CLI creation has separate administrative
rate limits.

Exceeding the implicit limit does not reject or alter the Event. Its exact release
value remains stored, a bounded processing diagnostic and metric are emitted, and no
new Release catalog document is created automatically. Explicit creation can later
materialize the same deterministic identity.

### Project-scoped Environment catalog

Environment catalog identity is project-scoped:

```text
environment_id = BLAKE3(project_id || canonical_length_prefix || exact_name)
```

The logical document is:

```javascript
{
  _id,
  project_id,
  name,
  first_seen,
  last_seen,
  hidden,
  source: "event" | "api"
}
```

Implicit environment materialization is limited independently:

```toml
[environments]
max_implicit_per_project = 100
```

As with Release limits, exceeding it does not reject the Event or replace its value.
The value remains available on the Event, but it is not automatically added to the
catalog. An authorized explicit API operation can register it later.

### Distribution handling

`dist` remains an optional Event string and has no standalone collection initially.
It is meaningful as a build or deployment variant of a release and will later
participate in release-artifact and source-map lookup.

A `dist` received without `release` is preserved for protocol compatibility but does
not create a catalog relation.

### Event and Issue storage

The canonical Event stores each normalized value once. It does not redundantly store
both a long string and a deterministic ID merely for metadata lookup; IDs are derived
when needed.

Release, distribution, and environment do not participate in default GroupingKey
generation. An Issue can therefore contain occurrences from multiple builds and
environments unless the application explicitly supplies a distinguishing SDK
fingerprint.

Issue keeps only Release values paired with its first and last occurrence metadata:

```javascript
{
  first_seen,
  first_seen_release,
  last_seen,
  last_seen_release
}
```

The timestamp, Event ID, and Release value are updated as one logical pair, including
deterministic tie-breaking for equal timestamps. Missing Release remains absent from
the logical value. ADR-0024's compact Issue representation uses its optional `m: true`
marker only when an existing first release would otherwise make a missing latest
release indistinguishable from the default `last_seen_release == first_seen_release`.
Unbounded arrays of every observed Release, distribution, or environment are not
stored in the Issue document.

Release- or environment-filtered Issue lists require a separately bounded projection
or aggregate decision. They are not implemented through an unbounded Event scan or a
growing array on each Issue.

### Retention and deferred release features

Release and Environment catalog documents have no automatic TTL. A Release may be
archived and an Environment may be hidden. Project deletion removes its Environments
and project association; organization deletion removes its Releases.

Deploys, commits, repositories, release health, source maps, and release artifacts
are separate capabilities. Native debug-file identity from ADR-0013 remains based on
debug IDs and does not require a Release.

## Consequences

- Existing Sentry SDK release metadata retains its meaning and case-sensitive identity.
- Monorepo projects can share an organization Release without duplicate catalog rows.
- Missing environment costs no BSON value bytes and is not confused with the literal
  string `unknown`.
- FinalizeBatch amortizes catalog maintenance instead of adding a metadata write per
  Event.
- Accidental per-request Release or Environment values cannot create an unlimited
  metadata catalog.
- Release/environment Issue filtering needs another bounded projection decision.

## Deferred questions

- Deploy, commit, and repository models and Sentry CLI API coverage.
- Release- and environment-scoped Issue projections and statistics.
- Exact compatibility validation corpus and field bounds.
- Release catalog project-array limits for exceptionally large organizations.
- Release health after session ingestion is enabled.
