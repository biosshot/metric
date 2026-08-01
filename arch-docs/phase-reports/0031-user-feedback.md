# Phase 31 report: User Feedback

> Historical test evidence, not an upgrade runbook. Its fresh-database language
> describes that phase's development environment only. The current binary requires
> generation 19; preserve data-bearing databases and follow
> [the current upgrade runbook](../../docs/upgrading.md).

- Date: 2026-07-27
- Result: complete
- Module contract: `module-contracts/0031-user-feedback-phase-31.md`

## Delivered

- The pinned `@sentry/browser` 10.66.0 `captureFeedback` row is accepted as its
  actual Envelope `feedback` item and routed to a low-volume Feedback path before
  Error normalization. Legacy `user_report` stays disabled.
- The domain retains only bounded message, name, contact, URL, exact telemetry
  identifiers, workflow status and committed attachment metadata after the existing
  project PII scrubber runs.
- Project policy has an explicit `feedback` capability. DSN credentials can submit
  but cannot read. Authenticated `ProjectRead` can list/detail/download, while
  `IssueWrite` is required for `open`, `resolved` and `spam` mutations.
- Feedback-specific field, attachment count/item/total bytes, PNG policy,
  per-project rate and limiter-capacity bounds run before the first durable side
  effect.
- Attachments reuse the existing event-owned Blob namespace and commit path.
  Metadata is inserted only after every accepted Blob commit. Failed metadata writes
  leave immutable orphans for the existing bounded reconciler.
- MongoDB schema generation advances from 12 to 13 with compact `feedback`
  documents, project/time and project/status/time indexes, and configurable
  `feedback_days` TTL. This is the accepted breaking generation; no migration
  subsystem was added.
- Duplicate SDK delivery is semantic and ignores retry-time timestamps while still
  rejecting a conflicting payload for the same Feedback ID.
- Project deletion has stable dataset code 16 for Feedback metadata. Blob reference
  reconciliation recognizes Feedback attachments; the existing project-owned Blob
  namespace deletion covers their bytes.
- Native API provides bounded cursor pagination, detail, status mutation and streamed
  attachment download. Optional Issue links are resolved only from an exact
  associated Event; no proximity correlation is inferred.
- Vue adds Feedback navigation, list/status filter, detail, telemetry links,
  attachments, status workflow and project enablement. SDK strings are rendered only
  through text interpolation, never trusted HTML.
- Feedback intentionally has no batch writer: it is a bounded low-volume
  append/workflow capability and does not consume Error, Log, Span or Session lanes.

## Exit gate

| Gate | Evidence |
| --- | --- |
| Blob commit before visibility | Ingest calls the existing attachment commit path before `FeedbackSink::persist_feedback`; the real Browser SDK and MongoDB tests verify committed attachment metadata and readable bytes. |
| Explicit anonymous/authenticated authorization | The module contract and native route matrix pin DSN submit-only, `ProjectRead` reads/downloads and `IssueWrite` status mutation. |
| No trusted HTML | Feedback list/detail use Vue text interpolation and `white-space: pre-wrap`; no `v-html` or equivalent sink is present. |
| Limits before side effects | Domain and ingest tests cover bounded values and attachment rejection; count/item/total/rate/capacity checks precede Blob and MongoDB calls. |
| Deletion and retention | Generation 13 adds strict Feedback TTL/indexes, deletion dataset code 16, Feedback-aware Blob references and the shared project-owned attachment namespace. |
| Real Browser SDK/widget row | Headless Chromium with pinned `@sentry/browser` 10.66.0 sent `captureFeedback` plus a real attachment through HTTP ingest; exactly one Feedback record and zero Error Events were produced. |
| Durable MongoDB workflow | Standalone local MongoDB E2E passed insert, retry duplicate, list, detail/status update and Blob-reference protection. |
| API/Web | Native permission matrix, known SPA routes, Rust server tests, 23 Vitest tests, ESLint and production Vue build passed. |
| Performance | The one retained release-mode run validated 100,000 Feedback records in 22 ms: **4,528,391 RPS**, above the 100,000 RPS local gate. |
| Process cleanup | Browser E2E closed Chromium and its HTTP listener; timed-out Cargo invocations and their `rustc` children were explicitly terminated; final inspection found no Cargo, Rust test, Node or Playwright process from this work. |

## Verification

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p metric-domain -p metric-ports -p metric-sentry-protocol \
  -p metric-application -p metric-mongo -p metric-blob \
  -p metric-symbolication -p metric-testkit
cargo test -p metric-server --lib
cargo test -p metric-server --test ingest_e2e \
  --test native_api_e2e --test web_e2e
cargo test -p metric-mongo --test feedback -- --ignored --nocapture
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_browser_sdk_sends_feedback_with_attachment -- --ignored --nocapture
cargo test -p metric-domain --release \
  feedback::tests::performance_feedback_validation_rps -- --ignored --nocapture

cd web
npm run lint
npm test
npm run build

cd sdk-tests/browser
npm run lint
npm run build
```

The performance artifact is
`performance/baselines/user-feedback/ryzen-5600h-windows-v1.json`. Exactly one
performance test was run.

## Known limits and next phase

- Screenshot bytes are accepted only when project Feedback, global attachments and
  the PNG policy are enabled; text/JSON attachments retain the existing scrub-safe
  handling.
- Replay IDs can be retained as exact future links, but Replay storage and UI remain
  deferred.
- Feedback is not a ticket system, form builder or chat subsystem.
- Schema generation 13 intentionally requires a fresh database under the owner's
  breaking-change rule.
- MCP, NATS, sharding, disk spool and online migrations remain deferred.
- At this report cutoff Phase 32 Unified Explore was next and Phase 27 was deferred.
  ADR-0047 later closed Phase 27 as obsolete without claiming its gates.
