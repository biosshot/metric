# ADR-0006: Ingest and Processor responsibility boundary

- Status: Accepted
- Date: 2026-07-20

## Context

Ingest must acknowledge SDK traffic quickly, but durable storage must not receive
unbounded, structurally unusable, or unsanitized input. Conversely, grouping,
symbolication, and complete semantic normalization are too expensive and failure-prone
to run before every HTTP response.

The boundary must preserve Sentry SDK data that the current server does not yet fully
interpret while ensuring that secrets and configured personal data are not written to
MongoDB first and scrubbed later.

## Decision

### Ingest responsibilities

Before durable MongoDB insertion, Ingest performs only acceptance-critical work:

- parse the HTTP request and supported content encodings;
- authenticate the DSN and resolve its project;
- parse the envelope framing and item headers;
- validate required identifiers and structural invariants;
- extract or validate the event identifier;
- reject input that is clearly corrupt or cannot be represented safely;
- apply server-side PII scrubbing;
- preserve acceptable unknown SDK fields for compatibility;
- create the `AcceptedEvent` passed to MongoWriter.

Ingest does not perform final grouping, issue lookup, symbolication, source-map
resolution, title/culprit derivation, alert evaluation, or complete platform-specific
normalization.

### Durable acceptance and response

The sanitized accepted payload is encoded in the versioned body `b` and inserted with
compact pending state `q` from ADR-0022 through the configured micro-batch writer. A
successful SDK response is returned only after MongoDB confirms the durable write for
that event.

The original unsanitized payload is not stored durably. A future disk spool must obey
the same rule unless a separate encrypted pre-scrub design is explicitly accepted.

### PII scrubbing

PII scrubbing runs before MongoDB insertion. It covers the accepted fields and
preserved unknown structures according to the active server and project policy.

Scrubbing policy is part of the acceptance configuration resolved for a DSN. A
scrubbing failure that prevents safe persistence is an ingestion failure; it is not
deferred to Processor after returning success.

The default security floor, project rules, HMAC behavior, failure contract, audit
metadata, and attachment policy are defined by ADR-0011.

### Project configuration cache

Ingest does not query MongoDB for the project and scrub policy on every event. It uses
the bounded positive/negative `DsnKey` cache defined by ADR-0019.

The cache contains only acceptance-path configuration, including:

- active/disabled project state;
- valid project keys;
- accepted origin or transport policy where applicable;
- PII scrubbing policy;
- ingestion and rate-limit policy;
- compatibility flags needed by envelope parsing.

Concurrent misses are coalesced. Application commands invalidate local entries and a
bounded TTL provides a backstop. No distributed invalidation is required while only
the `all` process exists.

### Processor responsibilities

After durable acceptance, Processor owns semantic transformation and enrichment:

- complete normalization into the stable internal event model;
- timestamp correction and classification beyond acceptance checks;
- platform-specific exception and frame processing;
- native symbolication, source maps, and demangling;
- grouping fingerprint and explanation;
- issue creation and update;
- title, culprit, and other derived investigation fields;
- final body replacement, pending-marker removal, and retry classification.

Processor operates only on the sanitized accepted payload. Reprocessing therefore
cannot restore information removed by the accepted PII policy.

### Unknown SDK fields

Unknown fields inside an otherwise accepted event are preserved when they can be
bounded, represented safely, and scrubbed recursively. Unknown data is not interpreted
as trusted internal state and cannot override server-owned fields such as project
identity, pipeline state, issue identity, or processing metadata.

Unsupported envelope item types require a separate compatibility policy. Preserving
unknown fields inside a supported item does not imply accepting arbitrary unknown item
types or unbounded binary content.

## Consequences

- Secrets and configured personal data are not intentionally persisted first and
  removed asynchronously later.
- The HTTP acceptance path remains bounded and avoids symbolication and grouping
  latency.
- SDK payload evolution can retain compatible unknown fields without immediately
  teaching Processor how to interpret all of them.
- Project configuration reads are removed from the per-event MongoDB hot path.
- Incorrect scrub configuration cannot be repaired from an unsanitized durable copy;
  users must make deliberate policy choices before ingestion.

## Deferred questions

- Encrypted pre-scrub handling for a future disk spool, if ever required.
