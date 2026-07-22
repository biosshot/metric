# Phase 5 contract: deterministic Error Event Normalizer

- Status: accepted for implementation
- Date: 2026-07-22
- Owners: `application::normalizer` (transformation and diagnostics), `domain::event`
  (stable normalized model)

## Responsibilities and exclusions

Normalizer converts one already-scrubbed `AcceptedEvent` JSON payload into a stable
adapter-independent `NormalizedEvent`. It normalizes occurrence timestamps,
exceptions, stack frames, tags, request/user/contexts, breadcrumbs, fingerprint,
platform, level/logger, release/dist/environment, and bounded compatible unknown
fields. Missing values use one canonical representation and object keys/tags are
ordered deterministically.

Normalizer performs no I/O and owns no MongoDB/BSON/body-codec, Processor retry,
symbolication, grouping, Issue, Finalizer, catalog, or pipeline-state behavior. It
does not read configuration or wall-clock state. Input is already scrubbed; it never
attempts to reconstruct removed data.

## Inputs, outputs, and errors

Input is an owned or borrowed `AcceptedEvent`. Output preserves its project/Event
identity, receive time, and scrub-policy revision alongside a `NormalizedEventBody`
and bounded ordered diagnostics. The domain model contains no wire DTO or adapter
type.

Invalid JSON, a non-object root, excessive structural complexity, and oversized
identity-bearing release/dist/environment values are stable terminal normalization
errors. Recoverable field-shape, timestamp, duplicate-tag, and collection-limit
problems omit or bound only the affected non-identity value and append a stable
diagnostic code and bounded field path. Diagnostics never contain payload values.

## Bounds and canonical behavior

`NormalizerLimits` bounds depth, visited JSON nodes, diagnostics, exceptions, frames,
tags, breadcrumbs, fingerprint entries, unknown top-level fields, object members,
array items, and string bytes. Defaults fit inside the accepted one-MiB Event limit.
Configured values are validated and cannot be zero or exceed hard ceilings.

Timestamp strings accept bounded RFC3339 UTC/offset forms; finite numeric timestamps
are interpreted as Unix seconds and canonicalized to milliseconds. Missing or invalid
occurrence time falls back to the durable server receive time with a diagnostic.
Release, dist, and environment preserve exact validated case-sensitive identity and
remain absent when missing; they are never lowercased, inferred, or truncated.
Default level is `error`, default platform is `other`, and absent/empty optional
collections have one representation.

Normalization is deterministic and idempotent under its canonical JSON projection:
normalizing an emitted canonical body produces the same body and no new diagnostics.

## Operability and verification

Fixed-label metrics may be recorded later by Processor from the returned outcome and
diagnostic codes; Normalizer itself has no global metrics/exporter dependency. Safe
logging fields are outcome, duration, input/output byte sizes, and bounded diagnostic
codes, never payload text or identifiers as labels.

Required verification is SDK-family golden input/output, pathological nested input,
determinism and canonical-projection idempotence properties, explicit collection and
complexity bounds, retained structured-field fuzz regressions, proof that the module
has no storage/network dependency, and recorded CPU/allocation-oriented throughput
across ADR-0037 size classes.
