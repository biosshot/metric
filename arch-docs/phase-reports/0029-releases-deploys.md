# Phase 29 report: Releases and Deploys

- Date: 2026-07-27
- Result: complete
- Module contract: `module-contracts/0029-releases-deploys-phase-29.md`

## Delivered

- The existing organization-scoped deterministic Release identity now serves both
  implicit Error observations and explicit Release commands. Explicit metadata is
  bounded and optional; a Release remains useful without repository metadata.
- `ReleaseStore` and `ReleaseService` provide create, finalize, detail, list, deploy
  and indexed new/regressed Issue operations without leaking MongoDB documents into
  domain or application modules.
- `releases` gained bounded explicit metadata and optional Error-observation fields.
  The only new collection is `deploys`, with a strict validator and one bounded
  project/Release timeline index. Schema generation advances from 10 to 11.
- Repeated Release creation, finalization and Deploy recording are idempotent.
  Compatible Deploy requests derive a stable operation identity; native requests
  require an explicit `Idempotency-Key`.
- The Issue latest-regression projection stores the exact optional Release from the
  Error that reopened a resolved Issue. It is not used to detect regression and is
  not an unbounded Release history.
- Personal tokens support `release:read` and `release:write`. Readers can inspect;
  only roles that already grant the write capability can mint a write-scoped token.
- Native `/api/v1` routes and the accepted `/api/0` `sentry-cli` subset call the
  same application service.
- Vue gained Release list/detail routes, explicit create/finalize, new and latest
  regressed Issue summaries, exact Error/Log/Span links, Deploy timeline/creation
  and copyable `sentry-cli` instructions. Direct refresh for both Release routes is
  served by the Rust SPA adapter.
- Project deletion treats Deploys like Releases: it removes only the deleted project
  association and preserves records still shared by another project.

## Exit gate

| Gate | Evidence |
| --- | --- |
| Implicit and explicit identity convergence | Real MongoDB Finalizer test materialized `backend@1.0`, then explicit creation returned the same `ReleaseId` and exactly one document. |
| Idempotent create/finalize/deploy | Domain/store tests and real `sentry-cli` E2E repeated the same Deploy; MongoDB retained one identical record. Finalize preserves the first selected release time. |
| Bounded summaries without Error scans | Release list uses the bounded Release catalog. New Issues use `fr`; latest regressions use compact `d.r`. Both queries are limited and index-backed. |
| Repository metadata remains optional | Validators, codecs and implicit-to-explicit integration cover Releases with no repositories; Error and Artifact lookup ownership is unchanged. |
| Real CLI compatibility | Globally installed `sentry-cli 3.6.2` passed create, finalize and two identical deploy commands against a real local HTTP listener and MongoDB. |
| Indexed Release/Deploy queries | Real MongoDB explain assertions selected `release_organization_timeline`, `deploy_project_release_timeline`, `issue_release_new_timeline` and `issue_release_regression_timeline`. |
| Web behavior | ESLint, 23 Vitest tests, TypeScript checking and production Vite build passed; the Rust static adapter test includes `/releases` and `/releases/:id` refresh. |
| Performance | The retained release-mode CPU baseline completed 100,000 bounded Release validation/identity operations in 31 ms: **3,179,691 RPS**, above the 100,000 RPS local gate. |
| Quality and dependency direction | Workspace formatting, dependency-check, strict all-feature/all-target Clippy and the complete all-feature workspace test suite passed. |

## Verification

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
cargo test -p dependency-check
cargo test -p metric-mongo --test finalizer_store \
  infrastructure_finalizer_event_issue_bucket_catalog_limits_and_explains -- --ignored
cargo test -p metric-mongo --test issue_store \
  infrastructure_issue_atomic_lifecycle_contention_and_query_plans -- --ignored
cargo test -p metric-mongo --test project_deletion \
  infrastructure_project_deletion_cancel_restart_rescan_and_tombstone -- --ignored
cargo test -p metric-server --test releases_e2e \
  real_sentry_cli_release_finalize_and_idempotent_deploy -- --ignored
cargo test -p metric-domain --release performance_release_identity_rps \
  -- --ignored --nocapture
npm run lint
npm test -- --run
npm run build
```

The real MongoDB and CLI rows passed against the user's standalone MongoDB on
`127.0.0.1:27017`; no Docker MongoDB was started. The performance artifact is
`performance/baselines/releases-deploys/ryzen-5600h-windows-v1.json`.

## Known limits and next phase

- Release versions are exact opaque strings; Metric does not guess semantic version
  ordering.
- The regression projection represents only the latest resolved-to-open transition.
- Commit ingestion, diffs, suspect commits, ownership and source integrations remain
  deferred.
- Sessions and Release Health are not implemented by Phase 29. Phase 30 is next.
- Schema generation 11 is a breaking empty-schema bootstrap generation. Online
  migrations remain outside the accepted architecture.
