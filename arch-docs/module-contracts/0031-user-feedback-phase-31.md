# Phase 31 module contract: User Feedback

## Ownership

- `metric-domain::feedback` owns bounded Feedback values, workflow status, links,
  attachment metadata and deterministic identity.
- `metric-application::ingest` recognizes the pinned current Browser SDK Feedback
  event before Error normalization, applies project policy, PII scrubbing and
  Feedback-specific abuse limits, then reuses the existing attachment commit path.
- `metric-ports::FeedbackSink` is the low-volume durable submission boundary.
  `FeedbackStore` owns project-scoped list, detail and status operations.
- `metric-mongo::feedback` owns the compact `feedback` codec, validators, retention
  and bounded indexes.
- Native API/Web expose authenticated investigation and workflow; SDK submission
  remains DSN-authenticated and write-only.

## Accepted compatibility row

Phase 31 accepts `@sentry/browser` 10.66.0 `captureFeedback`. Its pinned payload is
an Envelope `feedback` item whose JSON `type` is also `feedback` and whose bounded
user fields are under `contexts.feedback`. The protocol adapter assigns that current
item the existing primary-record role before Error normalization. Legacy
`user_report` remains disabled because this compatibility row does not require it.

The SDK Event ID is the Feedback ID. Optional exact links are retained only when
the payload supplies valid `associated_event_id`, Trace ID or Replay ID. No
time/release proximity relationship is inferred.

## Privacy, authorization and abuse boundary

The full bounded Feedback JSON passes through the mandatory project scrubber before
any field is retained. Only message, optional name/contact/url, exact identifiers
and committed attachment metadata survive normalization. Raw JSON, tags and unknown
contexts are not stored.

Authorization is explicit:

- active DSN + project `feedback` capability: submit only;
- authenticated `ProjectRead`: list/detail and attachment read;
- authenticated `IssueWrite`: status mutation;
- no DSN credential can read or mutate Feedback.

Feedback-specific message/contact/url, attachment count/bytes and per-project
fixed-window limits are validated before the first Blob or MongoDB write. The
limiter has a configured bounded project capacity.

## Durability and storage

For submissions with attachments:

```text
bounded parse/auth/scrub/abuse checks
-> commit every accepted Blob
-> insert compact Feedback metadata with owned Blob references
-> acknowledge
```

Feedback is never query-visible before all referenced Blobs are committed. A crash
or metadata failure after Blob publication leaves only an immutable orphan; the
existing bounded Blob reconciler removes it after the configured grace period.

`feedback` uses `_id`, one project/time list index, one project/status/time workflow
index and TTL on absolute expiry. Blob bytes never enter BSON. Project deletion
purges Feedback metadata and the already shared project-owned attachment namespace.

## Workflow and rendering

The initial closed status set is `open`, `resolved`, `spam`. Repeating a status
mutation is idempotent. Web renders message/name/contact as Vue text interpolation;
it never uses trusted HTML for SDK content.

## Explicit exclusions

Phase 31 does not add a batch writer, ticket/chat/form-builder features, arbitrary
custom fields, legacy feedback endpoints, inferred links, Replay storage, MCP,
NATS, migrations, sharding or disk spool. Error, Log, Span and Session writers and
codecs remain unchanged.
