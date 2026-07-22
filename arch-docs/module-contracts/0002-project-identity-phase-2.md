# Phase 2 contract: project identity and DSN resolution

- Status: accepted for implementation
- Date: 2026-07-22
- Owners: `application::projects` (commands and cache), `mongo` (schema and
  persistence), `server` (configuration and composition)

## Responsibilities and exclusions

Phase 2 creates the initial `schema_meta`, `organizations`, `projects`, and
`project_keys` schema on an empty MongoDB database, validates that exact schema on
subsequent startup, and supplies the real `ProjectResolver` required by Ingest. The
application service owns cryptographic ID/key generation, collision retries, typed
project commands, authorization-cache policy, and immediate local invalidation.
MongoDB owns BSON translation, validators, indexes, uniqueness and authoritative
key-to-project lookup.

It does not implement users, Web/API administration, audit, project purge jobs,
Event persistence, MongoWriter, migrations, MCP, NATS, sharding, disk spool, or
distributed cache invalidation. Phase 3 remains responsible for durable Events.

## Inputs, outputs, ports, and stable errors

Commands accept bounded organization/project slugs and display names, immutable
acceptance policy, and a `Clock`/cryptographic `RandomSource`. Creation returns the
stable positive organization/project IDs and the initial 16-byte DSN key. State/key
commands expose only active, disabled and deletion-fenced acceptance states.

`ProjectStore` is the capability-specific persistence port. It creates identities,
loads one acceptance snapshot by DSN key, and performs bounded acceptance-state
updates. Stable application errors distinguish invalid commands, existing slugs,
exhausted random collisions, absent internal command targets, randomness failure and
temporary storage failure. Public ingest still maps every missing, disabled,
deletion-fenced, deleted or mismatched credential to the same unauthorized result.

## Side effects, idempotency, and persistent shape

Organization IDs are random positive 63-bit integers; project IDs are random
positive 31-bit integers; DSN keys are 16 random bytes. MongoDB uniqueness is the
final collision check and the service retries a bounded number of generated
candidates. Organization slug is globally unique and project slug is unique inside
its organization. `project_keys._id` is BSON generic binary containing the key.

The schema marker has one fixed ID, generation, module set and complete state.
Bootstrap is allowed only for an empty database and is idempotent after completion;
missing, incomplete, unknown or newer markers fail closed. This is initial schema
bootstrap, not a migration system.

## Resource, cancellation, and shutdown bounds

Slugs are canonical lowercase ASCII and at most 63 bytes; display names and key
labels are bounded. ID/key collision retries, keys returned by a state mutation,
cache entries, in-flight misses, positive/negative TTLs and MongoDB operation
deadlines are finite. Cache misses for one key share one in-flight future. Cache
unavailability is never cached. Cancellation may stop a lookup/command and cannot
turn an unauthorized or incomplete lookup into an active snapshot.

## Operability and safe fields

Safe metrics contain only operation/outcome/cache-result dimensions. Safe logs may
contain numeric project/organization IDs, bounded operation/error codes, counts and
latency. DSN keys, MongoDB URIs/database names, HMAC material, slugs, display names
and backend error strings are not logged or used as metric labels.

## Verification and performance acceptance

Required tests cover domain identifier/slug properties, deterministic collision
retries, real MongoDB schema/index/validator and uniqueness behavior, active versus
disabled/deletion-fenced/missing credentials, cross-project mismatch, positive and
negative cache TTL, miss coalescing, invalidation, bounded capacity/in-flight work,
and the cumulative `HTTP -> real ProjectResolver -> fake EventSink -> response`
path. A warm-cache resolver benchmark must exceed the ADR-0037 20,000 lookup/s burst
rate and record RPS and latency on declared hardware. Real MongoDB cold lookup
latency is recorded separately and is not misreported as a cached result.
