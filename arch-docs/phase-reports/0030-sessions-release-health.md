# Phase 30 report: Sessions and Release Health

- Date: 2026-07-27
- Result: complete
- Module contract: `module-contracts/0030-sessions-release-health-phase-30.md`

## Delivered

- Pinned individual Sentry `session` Envelope items are normalized into bounded
  project-scoped lifecycle updates. Release and Environment reuse their existing
  compact identities; the original JSON and arbitrary SDK attributes are not stored.
- `SessionWriter` owns a separate bounded queue, micro-batches updates, drains on
  shutdown and acknowledges only after the compact source Session is durable.
  Session load cannot consume Error, Log or Span queue reservations.
- MongoDB stores one compact document per Session and merges duplicate or
  out-of-order updates deterministically. Terminal precedence is
  `crashed > abnormal > exited > ok`.
- `session_stats_hourly` provides rebuildable Release/Environment health. Duplicate
  retries or an ambiguous derived write trigger an exact bounded rebuild of affected
  hours from durable Sessions.
- Approximate users use two fixed 128-byte mergeable sketches per bucket. The
  published standard error is approximately 3.25%, with a 7,098-user
  single-bucket saturation estimate.
- Detailed Session retention defaults to 7 days; hourly health defaults to 400
  days. Active Sessions are deterministically terminalized after the configurable
  24-hour maximum instead of receiving an unsafe ordinary TTL.
- Optional cold archival reuses the existing project/day, size-bounded
  Parquet/Zstandard coordinator, manifest/checksum commit order and BlobStore.
  There is no object-per-Session path.
- Native Release detail exposes hourly and summary crash-free Session/user health.
  Vue renders bounded summary cards, the environment/hour table and explicit
  approximation/error-bound disclosure.
- The official Node SDK fixture sends start, exit and crash lifecycle updates.
  Aggregate `sessions` items remain explicitly unsupported because the pinned
  fixture does not provide the stable contribution identity required to prevent
  silent overcounting.
- Schema generation advances from 11 to 12 as an explicitly breaking
  empty-database bootstrap generation; no migration subsystem was added.

## Exit gate

| Gate | Evidence |
| --- | --- |
| Duplicate/out-of-order convergence | Domain fixtures cover duplicate, lower-sequence and higher-precedence updates; real MongoDB E2E converges to one terminal Session. |
| Durable acknowledgement and retry repair | `SessionWriter` waits for `persist_sessions`; MongoDB source upsert precedes derived stats, while duplicate/failed derived writes rebuild the exact affected hours. |
| Active Sessions are bounded | Config validation pins `session_active_max_hours`; maintenance E2E terminalizes a stale active Session through the same durable persistence path. |
| Published user-sketch bounds | Domain tests pin 128 bytes, merge behavior, approximately 3.25% standard error and the 7,098-user saturation estimate. |
| BSON/index budget | Mongo codec tests pin the compact optional-field shape and assert representative BSON plus required index-key bytes remain within the 384-byte Phase 30 budget. |
| TTL and archive safety | Unit tests prove terminal TTL/archive fields are mutually safe and active state receives neither; real E2E commits archive metadata before delayed hot expiry. |
| Archive recovery/failure behavior | Shared Parquet tests read canonical Session rows back from Zstandard data; real archive E2E verifies complete manifests, checksummed objects and source commit, while retained crash-point tests preserve sources on failure. General database rehydration remains deferred by ADR-0035. |
| Lane isolation | Session traffic uses its own port, channel, writer task and maintenance task; writer tests exercise independent batching, capacity and shutdown drain. |
| Real SDK compatibility | Official `@sentry/node` 10.66.0 E2E sends lifecycle and crash state through the HTTP ingest path. No exact signal link is claimed. |
| Release Health API/Web | Authorized project/Release health query, merged release summaries, hourly environment rows, typed Vue client and production build passed. |
| Performance | One retained release-mode run completed 100,000 bounded Session merge operations in 22 ms: **4,483,219 RPS**, above the 100,000 RPS local gate. |
| Quality and dependency direction | Workspace format, dependency-check, strict all-feature/all-target Clippy, complete all-feature Rust tests, scoped Web format, ESLint, 23 Vitest tests and production Web build passed. |

## Verification

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
cargo test -p dependency-check
cargo test -p metric-mongo --test sessions \
  session_upsert_health_and_stale_terminalization_are_durable -- --ignored --exact
cargo test -p metric-server --test archive_e2e \
  cumulative_event_to_archive_object_manifest_then_hot_retention -- --ignored --exact
cargo test -p metric-server --test sdk_compatibility_e2e \
  real_node_sdk_sends_session_lifecycle_and_crash_state -- --ignored --exact
cargo test -p metric-domain --release \
  sessions::tests::performance_session_merge_rps -- --ignored --exact --nocapture
npx prettier --check src/api/client.ts src/api/types.ts src/style.css \
  src/views/ReleaseDetailView.vue
npm run lint
npm test
npm run build
```

The real MongoDB, archive and SDK rows passed against the user's standalone MongoDB
and local Node installation. The retained performance artifact is
`performance/baselines/sessions-release-health/ryzen-5600h-windows-v1.json`.
Exactly one performance run was captured and it started no server.

## Known limits and next phase

- Aggregate `sessions` payloads and speculative Session-to-signal correlation remain
  disabled until a pinned fixture provides the required exact identity.
- User counts are estimates and degrade near the published per-bucket saturation
  bound; the API and Web label them accordingly.
- Cold objects support verified export/recovery of canonical rows, not transparent
  Web search or a general database rehydration command.
- MCP, NATS, split roles, sharding, disk spool and online migrations remain deferred.
- At this report cutoff Phase 31 User Feedback was next and Phase 27 was deferred.
  ADR-0047 later closed Phase 27 as obsolete without claiming its gates.
