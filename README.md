# Metric

<p>
  <img src="web/public/favicon.svg" width="72" alt="Metric logo" />
</p>

**Sentry-compatible error tracking, rewritten in Rust. One binary, one database,
~5,000 durable events per second — measured on hardware matching Sentry's own
minimum requirements.**

Metric accepts events from the official Sentry SDKs you already use, scrubs PII
before storage, groups them into issues, and gives you a fast investigation UI —
without the 70+ containers a self-hosted Sentry stack drags along.

## What Metric does

Applications fail in production — usually silently. Users hit a blank screen or
a 500, say nothing, and leave. Error tracking exists so that you find out first,
with the evidence already collected.

Metric is a self-hosted error tracking and observability service that closes
that loop end to end:

1. **Integrate in one line.** Add the official Sentry SDK for your platform —
   browser, Node, Python, Java, .NET, Go, Rust — and point its DSN at Metric.
   Unhandled exceptions, captured errors, structured logs and traces start
   flowing automatically.
2. **Every event arrives with context.** Stack trace, release, environment,
   user, request data, tags and breadcrumbs — enough to reproduce the failure
   without asking the user what they clicked.
3. **Noise becomes signal.** PII is scrubbed *before* anything hits storage,
   and duplicate events are grouped into issues with occurrence counts,
   first/last seen and trends — one actionable entry instead of ten thousand
   identical stack traces.
4. **Investigate in the built-in web UI.** Issue list, timelines, search,
   event details, logs, traces and performance views ship in the same binary —
   there is no separate frontend to deploy.
5. **React and stay in budget.** Signed webhooks push new issues to your chat
   or ticket system; retention policies and the optional cold S3 archive keep
   storage bounded; Incident Capsule export hands a complete evidence bundle
   to whoever needs it.

Because Metric is self-hosted, your error data — which inevitably contains
sensitive user context — never leaves your infrastructure.

**Who it is for:** teams that want the Sentry workflow without the SaaS bill,
the data-residency questions, or the ~70-container ops burden.

---

## Metric vs. self-hosted Sentry


|                           | **Metric**                                                                                    | **Self-hosted Sentry**                                                                                                                                         |
| --------------------------- | ----------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Core technology           | Single **Rust** binary (Tokio, zero-copy parsing)                                            | Python/Django + Relay + Snuba + Kafka consumers                                                                                                                |
| Moving parts              | `metric-server` + MongoDB - **2 containers**, or a bare binary                               | **70+ containers** in the default install¹: Kafka, ClickHouse, PostgreSQL, Redis, Snuba, workers, cron, Relay, Symbolicator, vroom, plus init/migration jobs |
| Minimum hardware          | Benchmarked on hardware matching Sentry's official minimum class - 6-core CPU, 16 GiB RAM | [Official minimum](https://develop.sentry.dev/self-hosted/): 4 CPU, **16 GB RAM + 16 GB swap**, 20 GB disk; 32 GB RAM recommended                              |
| Time to first event       | One image + one TOML file + one bootstrap token                                               | `install.sh` pulls gigabytes of images for 70+ containers, runs migrations, typically 30–60 minutes                                                           |
| Durable ingest throughput | **4,983 events/s** measured, every event verified durable, zero acknowledged loss             | **~1–5 events/s** sustained (~300–400k/day) on the official minimum hardware, 10–20 events/s in short bursts²                                              |
| Hot-state footprint       | ~103 storage bytes + ~130 index bytes per event (measured, MongoDB WiredTiger)                | Postgres + ClickHouse + Kafka retention combined                                                                                                               |

¹ Counted from a default self-hosted deployment; not every container is a distinct
long-running service — many are one-shot setup, migration and bootstrap jobs.
² Sentry does not publish per-instance throughput; this is a field observation on
the official minimum hardware (4 CPU, 16 GB RAM) and varies with event size and
workload mix.

On the same class of hardware that self-hosted Sentry needs merely to idle,
Metric sustains roughly **three orders of magnitude more durable throughput**.
Metric publishes measured baselines instead of marketing numbers — every figure
below comes from a committed, reproducible benchmark artifact.

## Measured performance

All numbers were recorded on hardware corresponding to **Sentry's official
minimum requirements** — an AMD Ryzen 5 5600H (6C/12T) with 16 GiB RAM, running
a colocated load generator and a standalone MongoDB 8. Raw artifacts live in
[`performance/baselines/`](performance/baselines/).


| Workload                               | Result                       | Latency                  | Loss                               |
| ---------------------------------------- | ------------------------------ | -------------------------- | ------------------------------------ |
| Durable error ingest, 5,000 rps target | **4,983 events/s**           | p95 27.7 ms, p99 31.3 ms | 0 errors, 100% durability verified |
| Saturation run, 20,000 rps target      | **7,307 durable events/s**   | p95 296.8 ms             | 0 acknowledged loss                |
| Capacity envelope (100M events/day)    | **1,158 events/s** sustained | p95 24.9 ms              | 0 errors                           |
| HTTP ingest path (parsing/auth/limits) | 2,500 req/s                  | **p99 0.62 ms**          | 0 errors                           |
| Normalizer (scrub + canonicalize)      | **77,840 events/s** weighted | —                       | —                                 |
| Span batch writer                      | **32,639 spans/s**           | —                       | 0 acknowledged loss                |
| Backlog recovery                       | **1.56×** arrival rate      | —                       | —                                 |
| Timeline query API                     | 511 qps                      | **p95 2.49 ms**          | —                                 |

Reproduce the headline number yourself:

```powershell
./performance/run-release-load.ps1 -Rps 5000 -Duration 15s
```

The design envelope ([ADR-0037](arch-docs/0037-capacity-model-for-100-million-events-per-day.md),
[`docs/capacity.md`](docs/capacity.md))
targets 100 million events/day: 1,158 events/s average, 5,000/s steady headroom,
20,000/s bounded burst.

## Drop-in Sentry compatibility

Keep your SDK. Change the DSN. Done.

```javascript
Sentry.init({ dsn: "http://<key>@your-metric-host:4001/<project_id>" });
```

Verified against real processes of the official SDKs — no mocks:

- `@sentry/browser` 10.66.0 (Chromium 149), `@sentry/node` 10.66.0
- Python `sentry-sdk` 2.32.0, Java `sentry-java` 8.50.1, .NET `Sentry` 6.7.0
- Go `sentry-go` 0.48.0, Rust `sentry` 0.48.5
- `sentry-cli` 3.6.2 / 2.58.6 (debug files, source maps, artifact bundles)

Errors, structured logs (`Sentry.logger`), transactions and spans
(`Sentry.startSpan`), envelopes, attachments and minidumps all flow through the
standard Sentry endpoints. Compatibility claims are **fail-closed**: only SDK rows
marked `pass` in [`compatibility/sentry-sdk-matrix.toml`](compatibility/sentry-sdk-matrix.toml)
are advertised; untested families are explicitly not claimed. See
[`docs/compatibility.md`](docs/compatibility.md).

## What you get

- **Errors** — durable ingest, mandatory pre-storage PII scrubbing (HMAC'd),
  bounded normalization, issue grouping, full issue timeline and search.
- **Logs, traces, performance** — structured logs and span pipelines with
  bounded batch writers and retention policies.
- **Web UI included** — the Vue 3 investigation UI is built into the same image
  and served by the same binary. Nothing extra to deploy.
- **Security boundary** — Argon2id passwords, opaque sessions with CSRF, scoped
  personal API tokens, organization roles, bounded audit records.
- **Lifecycle** — retention with gradual policy reduction, durable project
  deletion with grace period, signed webhook notifications, Incident Capsule
  export.
- **Storage flexibility** — local BlobStore or S3-compatible object storage;
  optional Parquet/Zstd cold event archive.
- **Symbolication** — debug files, JS artifact bundles/source maps, optional
  external Symbolicator.

## Quick start

**Option A — Docker Compose (2 containers):**

```bash
cp deploy/release.env.example deploy/release.env   # set METRIC_MONGO_PASSWORD and METRIC_SCRUB_HMAC_KEY
docker compose --env-file deploy/release.env -f deploy/compose.release.yml up -d
```

This pulls the published `ghcr.io/biosshot/metric:0.1.0` image. Add `--build`
when testing local source changes instead.

**Option B — bare binary, no container at all:**

```bash
cargo build --release --bin metric-server
export MONGODB_URI="mongodb://127.0.0.1:27017/metric"
./target/release/metric-server --config config/metric.example.toml
```

Point any Sentry SDK DSN at `http://<host>:4001` and watch the first event land.
On first startup the server prints a one-time `METRIC_BOOTSTRAP_TOKEN` for the
Web setup form. Full reference: [`docs/configuration.md`](docs/configuration.md),
[`docs/operations.md`](docs/operations.md).

The current binary requires MongoDB schema generation **19 exactly**. It may
bootstrap an empty database, but it cannot migrate an older data-bearing database.
Do not edit `schema_meta`, drop the database or recreate it to resolve a generation
mismatch; follow the data-safety guidance in
[`docs/upgrading.md`](docs/upgrading.md).

## Honest limits — today

Metric is **not yet** a feature-complete Sentry clone — but it is built to become
one, and then some. The stated goal of the project is to match Sentry feature for
feature and surpass it in capability quality, operational reliability and raw
performance. The performance headroom above shows the foundation is already
there; the remaining gaps are a roadmap, not a ceiling.

What is still missing today: Profiling remains deliberately deferred. Application
Metrics and Session Replay are implemented; Replay is explicitly enabled per
project and is off by default. The runtime is a single `--role all` process: no
split roles, sharding or online schema migrations, and the supplied compose file is
a simple single-MongoDB deployment, not HA. Each boundary is tracked in
[`arch-docs/`](arch-docs/). The full current list:
[`docs/known-limits.md`](docs/known-limits.md).

## Support the project

Metric is developed in the open. If it saves you a Sentry invoice — or a weekend
of babysitting 70+ containers — you can support development with a USDT
donation:


| Network           | USDT address                                       |
| ------------------- | ---------------------------------------------------- |
| TON               | `UQBo1KI8Llrp9D7xSF3o0RzMHsM3UesueXRnSTyLPDkOG7-Q` |
| Tron (TRC-20)     | `TYiGUA56h1r19FEDzKRn33w511yTqZwy4V`               |
| Ethereum (ERC-20) | `0x040835b43916307b4014322439a18cb33B26913F`       |

Every donation goes directly toward closing the roadmap above: more supported
signal types, more SDK families, more performance headroom.

## Development

Local checks:

```text
cargo fmt --all -- --check
cargo dep-graph --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test-fast --locked
```

Additional risk tiers: `cargo test-infrastructure`, `cargo test-fuzz`, and
selected ignored performance tests. Infrastructure tests use the MongoDB image
from `deploy/compose.dev.yml` on port `27018`. k6 workloads, baselines and
regression comparators are documented in [`performance/README.md`](performance/README.md).

Run the server locally:

```text
cargo run -p metric-server --bin metric-server -- --env-file .env.local --config config/metric.example.toml
```

Copy `.env.local.example` to `.env.local` first and replace the placeholder
secret. Metric never searches for dotenv files implicitly; existing process
environment variables take precedence. Validate configuration without starting
via `--check-config`, or print it redacted with `--print-effective-config`.

**Web development:** the Vue 3 client lives in `web/` and consumes only
`/api/v1`. Run `npm install` once, then `npm run dev` while Metric listens on
`127.0.0.1:4001`. Web checks: `npm run format:check`, `npm run lint`,
`npm test`, `npm run build`, `npm run test:e2e`.

**SDK compatibility harnesses** live in `sdk-tests/` with isolated dependency
graphs. The initial Node gate:

```text
cd sdk-tests/node
npm ci
cd ../..
cargo test -p metric-server --test sdk_compatibility_e2e real_node_sdk_sends_an_error_event -- --ignored --nocapture
```

Architecture decisions are recorded in [`arch-docs/`](arch-docs/).
