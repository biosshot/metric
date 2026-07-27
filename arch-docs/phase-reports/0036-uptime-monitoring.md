# Phase 36 report: Uptime Monitoring

- Date: 2026-07-27
- Result: complete
- Governing decision: ADR-0045
- Module contract: `module-contracts/0036-uptime-monitoring-phase-36.md`

## Delivered

- GET/HEAD Uptime definitions and compact latency/failure history.
- Durable due scheduling with lease recovery and deterministic run identities.
- Manual redirect handling with per-hop DNS/IP validation and DNS pinning.
- Sealed write-only custom headers with redirect stripping.
- Global/per-host concurrency bounds and host-fair claim ordering.
- Native API and Vue lifecycle/history UI.
- Existing monitor notifications now emit Uptime firing and resolved payloads.

## Deferred by contract

Response bodies, private networks, browser JavaScript, screenshots, multi-step
checks and restore of expired run history remain outside Phase 36.

## Verification

| Exit gate | Evidence |
| --- | --- |
| SSRF, DNS rebinding, redirect, IPv4/IPv6 and metadata corpus | Every redirect is reparsed, all fresh DNS answers are rejected if any address is forbidden, and the selected address is pinned. `rejects_private_metadata_and_mapped_ipv6_corpus` covers the closed address corpus. |
| Header sealing and redirect stripping | `redirect_header_policy_strips_secrets_and_cross_origin_values` proves ciphertext-at-rest, first-hop-only sensitive headers, same-origin-only ordinary headers and cross-origin stripping. Domain tests pin forbidden framing/proxy/forwarding names. |
| Hostile hosts remain bounded | The executor owns one timeout budget, at most three redirects, 64 KiB of discarded response bytes, GET/HEAD only, 16 headers, global concurrency 16 and per-host concurrency 2. No response body enters MongoDB or BlobStore. |
| Global/per-host fairness | Claimed monitors are round-robin ordered by normalized host before bounded execution; `host_fairness_is_round_robin_and_restart_id_is_deterministic` pins ordering. |
| Restart scheduling is deterministic | Durable due/lease state is compare-and-set claimed. Run identity derives from monitor plus scheduled time; lease expiry therefore retries the same identity. |
| Uptime cannot make ingest/readiness/alerts unavailable | Uptime has a separate task, executor and semaphores, is excluded from readiness, and only appends to the existing durable notification outbox after a terminal run. |
| Browser lifecycle/history/firing/resolved | Chromium E2E creates an Uptime monitor with a write-only header, renders 503/failure and 200/recovery history, then creates a monitor rule with recovery notifications. |

Verification commands:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd web
npm run format:check
npm run lint
npm test
npm run build
npx playwright test tests/e2e/application.spec.ts --project=chromium --grep "uptime monitor lifecycle"
```

Exactly one performance test was executed. It created and dropped a unique local
MongoDB database and did not start an HTTP server:

```text
runs=2000
batch=200
elapsed_ms=371
durable_rps=5382.78
```

The baseline and comparator are retained under
`performance/baselines/uptime-monitoring/` and `performance/compare-uptime-monitoring.mjs`.
Playwright's temporary Vite listener was verified closed after E2E.
