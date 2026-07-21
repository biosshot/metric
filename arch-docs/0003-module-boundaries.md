# ADR-0003: Module boundaries in the single-process runtime

- Status: Accepted
- Date: 2026-07-20

## Context

The first version runs only in the `all` role, but deployment topology must not turn
the codebase into one monolithic component. Ingesting a Sentry envelope, normalizing
an event, resolving symbols, grouping an error, updating an issue, and running
maintenance tasks have different responsibilities and resource limits.

A role describes how code is deployed. A module describes what the code owns. All
modules in this decision run inside one process and are built from the bounded Cargo
workspace accepted by ADR-0034.

## Decision

### Application composition

The `all` runtime starts these logical components:

```text
HTTP Ingest
    -> MongoWriter
    -> Dispatcher
    -> Processor
        -> Normalizer
        -> Symbolicator
        -> Grouper
        -> IssueService
        -> Finalizer

NotificationDispatcher
NotificationDelivery
DebugFileUploadService
ArtifactUploadService
Scheduler
Web/API
MCP adapter
Storage
```

Application modules communicate through owned domain values, application services,
and bounded in-process channels. They are not independently deployed application
roles in the first version. A replaceable symbolication adapter may call the external
backend accepted by ADR-0013.

ADR-0034 defines the compile-time dependency direction, required module contracts,
risk-based tests, and the quality gate used during sequential implementation.
ADR-0039 defines the dependency-respecting implementation phases and cumulative E2E
ladder.

### Ingest

Ingest owns the external Sentry ingestion protocol. It:

- accepts HTTP requests;
- parses envelopes and supported item types;
- authenticates the DSN and resolves the project;
- applies acceptance-level validation and limits;
- classifies mixed Envelope Items and applies the capability contract in ADR-0018;
- accepts enabled standalone minidumps and creates the synthetic Event defined by
  ADR-0020;
- creates an `AcceptedEvent`;
- submits the event to the MongoDB micro-batch writer;
- maps the durable write result to the HTTP response.

Ingest does not group events, resolve symbols, update issues, send alerts, or perform
retention. It may aggregate bounded SDK `client_report` diagnostics without storing
their raw payloads.

### MongoWriter

MongoWriter owns durable micro-batch insertion. It:

- collects accepted events according to the configured wait and document limits;
- performs unordered MongoDB batch insertion;
- maps partial batch results to individual requests;
- treats an existing idempotency key as a successful retry;
- offers each newly persisted in-memory payload to Dispatcher without copying it.

MongoWriter does not interpret stack traces or issue semantics.

### Dispatcher

Dispatcher owns the bounded Processor queue and backlog refill. It:

- moves fresh in-memory payloads into the queue when capacity is available;
- tracks queued and running event keys locally;
- loads complete pending events from MongoDB on startup, at idle, and below the low
  watermark;
- respects retry scheduling;
- refills the queue toward a configured target.

Dispatcher schedules work but does not transform event contents. Distributed claims
and leases are deferred because only one process exists.

### Processor

Processor is the orchestrator for one event's post-acceptance lifecycle. It invokes
the processing stages in order, classifies temporary and permanent failures, manages
retry metadata, and chooses the final persistent state.

The intended flow is:

```text
AcceptedEvent
    -> Normalizer
    -> Symbolicator
    -> Grouper
    -> IssueService
    -> Finalizer
```

Processor owns orchestration and error policy. It does not absorb the algorithms and
storage implementation of its child services into one monolithic module.

### Normalizer

Normalizer converts accepted Sentry data into a stable internal event model. It owns
normalization of timestamps, exceptions, frames, tags, request/user context, release,
environment, platform-specific fields, and compatible unknown data.

Normalizer is deterministic where possible and does not own issue state.

### Symbolicator

The in-process SymbolicationService determines whether symbolication is required and
calls the selected backend through domain-owned request and response types. The first
backend is the license-gated external service defined by ADR-0013.

The adapter has independently configurable concurrency and classifies failures as
retryable or permanent. It does not create or update issues and does not expose
backend-specific protocol types to Processor.

For the accepted external backend, the adapter supplies the project-private source
and cache scope defined by ADR-0026. The corresponding internal index/download route
is owned by the symbols module, uses service authentication, and streams only
application-approved BlobStore objects.

ADR-0028 extends the same adapter with JavaScript/Node.js `/symbolicate-js`, an
independent project-private artifact source, and raw/derived frame mapping. Backend
wire types remain inside the adapter.

### Grouper

Grouper is a pure, deterministic domain algorithm. It consumes the normalized and,
where applicable, symbolicated event and returns:

- a grouping fingerprint;
- contributing grouping components;
- an explanation of why the event belongs to the group;
- the grouping algorithm version.

It honors an explicit SDK fingerprint when applicable. Grouper performs no MongoDB
operations and does not mutate issue state.

### IssueService

IssueService applies a grouping result to issue state. It owns:

- deterministic issue identity or lookup;
- issue creation;
- event-to-issue association;
- first-seen and last-seen behavior;
- resolved, ignored, and regression transitions;
- issue counters and future statistical buckets;
- idempotency of repeated event processing.

The exact MongoDB schema and atomicity strategy are decided separately.

### Finalizer

Finalizer replaces the accepted body with the compact canonical body, persists the
Issue association and derived symbolication result, and removes the pending marker as
defined by ADR-0022. Grouping key, revision, and strategy are persisted on the Issue,
not repeated in the Event. Finalizer also emits or records downstream action intents
required by alerts and other background work.

The boundary between IssueService and Finalizer must preserve idempotency when the
process stops between issue mutation and event finalization.

### Scheduler

Scheduler owns periodic maintenance rather than per-event orchestration. Its tasks
include:

- waking due retries and recovering pending work;
- retention and optional archival;
- blob and symbol-cache cleanup;
- recovery of pending debug-file assembly jobs and cleanup of expired upload chunks;
- periodic statistics work;
- detection of stale maintenance operations.

Dispatcher remains responsible for continuous queue refill.

### NotificationDispatcher and delivery

NotificationDispatcher expands durable Issue transition intents into idempotent
delivery documents. NotificationDelivery owns the bounded delivery queue, retry
classification, webhook security, and the selected backend adapter. Neither performs
external work in Processor, and their durable handoff is defined by ADR-0016.

### Storage and BlobStorage

Storage implements domain-oriented MongoDB operations required by the application
services. It does not own grouping, regression, or HTTP behavior.

BlobStorage contains debug symbols, source maps, minidumps, attachments, and optional
archive objects. It uses the local filesystem by default and may use S3-compatible
storage when configured.

DebugFileUploadService owns the Sentry CLI-compatible routes, streaming temporary
chunk validation, missing-chunk discovery, and the bounded whole-file assembly queue
defined by ADR-0025. It creates no MongoDB document per chunk. Only complete-file
assembly jobs are durable and recoverable.

ArtifactUploadService reuses the organization chunk service and owns artifact-bundle
assemble, validation, compact lookup projection, immutable publication, and the
recoverable `artifact_uploads` queue defined by ADR-0028. It stores whole bundles and
does not create one persistent document per source file.

### Web/API and MCP

Web/API and the future MCP adapter call shared application query and command services,
such as issue queries, issue commands, project services, release services, and event
queries.

Neither the UI/API layer nor MCP may bypass these services and issue arbitrary direct
MongoDB operations. This preserves authorization, project isolation, validation, and
audit behavior across all interfaces.

ADR-0021 defines the shared `AuthContext`, role and permission model, Web sessions,
and scoped personal API tokens. MCP uses those API tokens and never a DSN key or a
separate privileged authentication path.

### Source layout

ADR-0034 replaces the earlier single-package sketch with a small Cargo workspace:
`domain`, `ports`, `sentry-protocol`, `application`, concrete adapter crates, the
`server` composition root, and `testkit`. Product services remain logical modules
inside `application`; the workspace does not create one crate per service.

## Consequences

- The first deployment remains one process without giving every concern access to
  every other concern.
- Processing algorithms can be unit-tested without MongoDB or HTTP.
- Symbolication can enforce its own CPU and memory concurrency independently from
  ordinary processing.
- Explainable grouping is a first-class result of a pure domain module.
- A future role split can reuse module boundaries, but no network protocol or
  distributed coordination is implemented prematurely.
- Application services form the stable authorization boundary for Web/API and MCP.

## Deferred questions

- Symbolication regrouping and later format-specific stages.
