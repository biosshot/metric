# ADR-0019: Sentry DSN and rotatable project keys

- Status: Accepted
- Date: 2026-07-21

## Context

Official Sentry SDKs identify an ingest destination through a DSN containing a
numeric project ID and a public project key. Ingest must resolve this without a
MongoDB query on every request, prevent a URL or payload from redirecting an Event to
another project, and support key rotation without changing all persisted references.

An earlier design considered separate internal and Sentry-compatible project IDs.
That distinction adds no value in the first storage model and is explicitly rejected.

## Decision

### Two identifiers only

The model has exactly two relevant values:

```text
project_id   stable project identity
dsn_key      rotatable ingest credential
```

There is no separate local, internal, MongoDB, or external Sentry project ID.

`project_id` is one cryptographically random positive 31-bit integer in
`1..=i32::MAX`. It is simultaneously:

- `projects._id` in MongoDB;
- the numeric project component of the Sentry DSN;
- the project path parameter of Sentry-compatible ingest endpoints;
- the value referenced by Events, Issues, Environments, project keys, and all other
  project-owned documents.

Project creation generates a random value and retries the insert if the unique
MongoDB `_id` happens to collide. The positive signed range preserves BSON `int32`,
supports more than two billion projects per installation, and reduces every Event
and project-prefixed index. ADR-0022 defines the compact Event representation.

### DSN form

The generated DSN is logically:

```text
https://{dsn_key}@{configured_public_host}/{project_id}
```

The application `public_url` configuration is the source of scheme, host, port, and
optional base path. Client-supplied DSN host information is parsed for protocol
compatibility but never controls internal routing, outbound requests, BlobStore URLs,
or generated links.

### DSN key representation

A new key contains 16 cryptographically random bytes and is rendered as 32 lowercase
hexadecimal characters in Sentry-compatible transports. MongoDB stores those bytes as
the `project_keys` document `_id`:

```javascript
{
  _id: BinData(16),
  project_id: NumberInt(...),
  status: "active" | "disabled" | "suspended_by_deletion",
  label,
  created_at,
  disabled_at,
  last_used_at
}
```

The automatic `_id` index is therefore the ingest lookup index; no duplicate public
key index or lookup by project ID is needed on the hot path.

The name `DsnKey` is used internally to avoid implying an asymmetric cryptographic
public key. Protocol fields continue to use Sentry names such as `sentry_key`.

### Public write-only credential

A DSN key is expected to be embedded in browsers, mobile applications, desktop
binaries, and logs. It is not a confidential credential. It authorizes only ingest
submission and never grants Event reads, Issue commands, project administration,
Web access, or MCP access.

Abuse is controlled by project and category rate limits, bounded requests, optional
browser Origin rules, cardinality gates, project state, and key disable/rotation.
Mandatory client HMAC authentication is not added because official public-client SDKs
cannot protect such a secret.

### Multiple keys and rotation

A project may have several active keys. Rotation is:

1. create another active key;
2. deploy the new DSN to clients;
3. observe coalesced `last_used_at` metadata;
4. change the old key from `active` to `disabled`.

Disabled key documents remain as tombstones and return a generic unauthorized project
credential response. They are not immediately deleted or regenerated. Disabling a key
does not change `project_id` or any persisted Event/Issue relation.

`suspended_by_deletion` is the reversible project-level fence from ADR-0030. Only
keys that were active enter it, only cancellation may restore them to active, and
irreversible purge removes all keys for the project.

`last_used_at` is coalesced in memory and flushed at a bounded interval rather than
updated for every Envelope.

### Accepted authentication transports

Ingest accepts the Sentry-compatible forms required by supported endpoints:

- `X-Sentry-Auth` with `sentry_key`, protocol version, and client metadata;
- `sentry_key` and related query parameters;
- DSN in the Envelope header;
- a key in an endpoint path only where that endpoint defines it.

Identical duplicated information may be accepted. Conflicting keys are a malformed
authentication request; a key whose resolved project differs from a stated URL or DSN
project ID is rejected as a project mismatch.

Legacy DSN password or `sentry_secret` syntax is accepted but ignored. The value is
never generated, stored, logged, compared, or treated as confidential security.

### Authoritative project resolution

The only authoritative resolution flow is:

```text
dsn_key
    -> project_keys._id
    -> active key document
    -> project_id
    -> projects._id
```

After resolution, every numeric project ID stated in the endpoint or Envelope DSN
must equal the resolved `project_id`. A project-like field in an SDK payload is
ignored or overwritten by server-owned identity and can never choose a tenant.

Authentication errors use a generic response that does not distinguish missing,
disabled, mismatched, or deleted projects to unauthenticated clients.

### Project authorization cache

Ingest uses a bounded cache keyed only by parsed `DsnKey`. A positive entry contains a
project acceptance snapshot, including project/key state, PII and attachment policy,
ingest limits, grouping revision, and item capabilities.

```toml
[ingest.project_cache]
capacity = 100000
positive_ttl = "60s"
negative_ttl = "5s"
```

Concurrent misses for one key are coalesced into one MongoDB lookup. Approximate LRU
eviction keeps the cache bounded. Project/key commands through the application service
invalidate the local entry immediately; TTL is a correctness backstop.

The first version has one process, so no distributed invalidation protocol is needed.
Direct unsupported edits in MongoDB can remain visible for at most the positive TTL.
MongoDB unavailability still returns `503` for durable Event acceptance even when
project authorization is cached.

### Origin policy

An empty project Origin allowlist accepts browser origins for SDK compatibility. If
configured, exact and explicitly permitted wildcard origins are checked only for
browser requests that contain `Origin`. Server SDK requests without `Origin` are not
rejected merely for its absence.

Origin is an additional abuse filter, not authentication and not a source of project
identity.

### Event storage

Every Event stores the stable numeric `project_id`. It does not duplicate the DSN key
or key slot initially. Key usage is visible through coalesced project-key metadata and
aggregate ingest telemetry. Exact per-Event key attribution can be introduced later
only with a demonstrated security/audit need and an explicit storage-cost decision.

## Consequences

- One compact 31-bit numeric ID serves MongoDB, DSN, endpoint, and
  document-reference needs.
- Key rotation cannot change Event or Issue ownership.
- Most requests authenticate without a MongoDB read.
- Guessing a project ID alone is insufficient to submit an Event.
- A public DSN remains intentionally incapable of reading project data.
- Per-Event storage does not pay for repeated key attribution.

## Deferred questions

- Exact compatibility vectors for legacy auth header/query variants.
- Distributed project-cache invalidation if ingest roles are split later.
- Optional exact per-Event DSN key auditing.
