# Phase 29 module contract: Releases and Deploys

- Status: Implementation contract
- Architecture: ADR-0003, ADR-0008, ADR-0015, ADR-0017, ADR-0024, ADR-0034,
  ADR-0035, ADR-0036 and ADR-0045
- Scope: Phase 29 only

## Capability boundary

Phase 29 turns the existing implicit organization-scoped Release catalog into one
shared explicit and implicit workflow. It adds:

- bounded Release create, finalize, list and detail commands;
- the `sentry-cli` create/finalize/deploy subset;
- bounded Deploy start/finish records by environment;
- indexed new-Issue and latest-regression summaries;
- exact Release links to Errors, Logs, Spans and Artifact Bundles;
- Release and Deploy Web views.

Commit ingestion, Git diffs, changed files, suspect commits, CODEOWNERS, source
ownership and Release Health are excluded.

## Ownership and dependencies

The domain owns validated Release and Deploy values, deterministic identities and
DTOs. Ports own a narrow `ReleaseStore`; they expose no MongoDB documents or raw
filters. Application owns authorization, idempotency and command orchestration.
MongoDB owns physical codecs, validators and indexes. `/api/0` and `/api/v1` HTTP
adapters call the same application service. Web consumes only `/api/v1`.

The existing Finalizer remains the only owner of implicit Error observations. It
uses the same MongoDB Release upsert primitive as explicit commands. Log and Span
ingest do not gain Release catalog writes.

## Identity and bounds

Release identity remains the ADR-0017 deterministic identity:

```text
ReleaseId = BLAKE3(organization_id || exact validated version)
```

An implicit Event and an explicit command with the same organization and exact
version update one document. Release versions remain case-sensitive opaque strings
and are never compared as semantic versions.

Release metadata is bounded:

- version: existing 200-byte bound;
- URL: 2,048 UTF-8 bytes;
- reference or commit hash: 200 UTF-8 bytes;
- repository references: at most 16;
- repository identifier: 200 UTF-8 bytes;
- associated projects: at most 256.

An explicitly created Release may have no observed Error. Its first/last observation
and Event IDs are optional until Finalizer records an occurrence.

Deploy identity is deterministic from organization, Release and a 16-byte client
operation identity. Native callers supply `Idempotency-Key`. The compatible
`sentry-cli` adapter derives the operation identity from the canonical bounded
request when the client supplies no key. Reusing an identity with different content
returns a conflict.

Deploy bounds are:

- environment: 64 UTF-8 bytes;
- name: 200 UTF-8 bytes;
- URL: 2,048 UTF-8 bytes;
- associated projects: at most 256;
- finish must not precede start.

## Regression projection

Regression detection is unchanged:

```text
Issue is resolved && Event.received_at > Issue.resolved_at
```

The compact latest-regression object gains one optional exact Release string:

```text
d.r
```

It is copied from the Event that caused the regression. It does not participate in
detecting the transition and does not store Release history. It permits a bounded
indexed query for Issues whose latest regression occurred in one Release. New Issues
use the existing exact first Release `fr`.

Release detail returns at most the configured page size plus a continuation cursor
for new and latest-regressed Issues. It never scans an unbounded Error history and
does not expose an unbounded exact count.

## Storage and indexes

Phase 29 adds only:

```text
deploys
```

`releases` is extended for explicit metadata and optional observation fields. The
required timelines are:

```javascript
// Release list for an organization/project
{ organization_id: 1, project_ids: 1, activity_at: -1, _id: -1 }

// Deploys for a project Release
{ organization_id: 1, project_ids: 1, release_id: 1, started_at: -1, _id: -1 }

// New and latest-regressed Issues
{ p: 1, fr: 1, f: -1, _id: -1 }
{ p: 1, "d.r": 1, "d.t": -1, _id: -1 }
```

Release-to-signal relationships are queried, not copied into growing arrays:

- Errors use Search v1's exact Release token;
- Logs and Spans use their existing exact Release filters;
- Artifact Bundles use the existing organization-scoped `ReleaseId`;
- native debug files remain Debug-ID based.

Signals never store a Deploy ID. UI correlation uses exact Release, environment and
timestamps and never claims causation.

## API and authorization

Project readers may list and inspect Releases and Deploys. Release mutation requires
`release:write`; personal tokens may request this scope only when the actor's
organization role grants it.

Native routes use opaque Release/Deploy IDs and native DTOs. The accepted compatible
surface is limited to the endpoints exercised by pinned real `sentry-cli` create,
finalize and deploy commands. Compatible response DTOs do not leak native or compact
MongoDB representations.

Repeated create/finalize/deploy requests are idempotent. A finalize request without
an explicit time preserves the first server-selected `released_at`. Conflicting
metadata for the same operation identity returns a stable conflict.

## Verification

Phase 29 closes only when:

- implicit and explicit identities converge in a real MongoDB test;
- repeated create, finalize and deploy operations are idempotent;
- indexed bounded Release, Deploy, new-Issue and regression queries pass plan checks;
- missing repository metadata does not break Error or Artifact investigation;
- pinned real `sentry-cli` create/finalize/deploy E2E passes;
- Vue route refresh, list, detail, empty, loading and error states pass;
- one retained release-mode control-plane performance run reports RPS;
- formatting, dependency direction, lint and workspace tests pass.

Schema changes are breaking and increment the schema generation. No migration or
online compatibility path is part of Phase 29.
