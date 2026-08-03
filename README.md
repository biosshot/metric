# Metric

<p>
  <img src="web/public/favicon.svg" width="72" alt="Metric logo" />
</p>

[User documentation](https://biosshot.github.io/metric/) ·
[Installation](https://biosshot.github.io/metric/getting-started) ·
[Configuration](https://biosshot.github.io/metric/configuration)

**Sentry-compatible observability, rewritten in Rust. One application, one
database, a working 1 GiB profile and a measured reference run of 4,983 durable
error events per second.**

Keep the official Sentry SDKs already used by your applications. Change the DSN
and Metric starts receiving errors, logs, traces, releases, monitors, application
metrics and feedback. It removes or pseudonymizes private data before storage,
groups repeated failures into issues and serves the investigation UI from the
same Rust binary.

No SaaS account. No repository clone for Docker installation. No 65-service
Compose stack. Your telemetry stays on infrastructure you control.

> Metric 0.1.3 is an early release. Read the
> [known limits](https://biosshot.github.io/metric/known-limits) before using it
> for important production data.

## Why Metric exists

Applications fail in production, usually before a user reports anything. Error
tracking should tell you what broke, where it broke and which release caused it.
Running the tracker itself should not require a second platform team.

Metric closes that loop:

1. **Connect an existing SDK.** Browser JavaScript, Node.js, Python, Java, .NET,
   Go and Rust are tested with real official Sentry SDK processes.
2. **Receive useful context.** Stack traces, breadcrumbs, request data, releases,
   environments, users and tags arrive through the normal Sentry protocol.
3. **Turn noise into issues.** Repeated events are grouped, while occurrence
   counts, first/last seen times and trends remain visible.
4. **Investigate in one web interface.** Errors, logs, traces, releases,
   monitors, metrics, dashboards and feedback are built into Metric.
5. **Keep the system bounded.** Every supplied profile defines memory ceilings,
   queues, upload limits and retention instead of assuming unlimited hardware.

Metric is not trying to become a smaller, weaker Sentry clone. The goal is to
match the workflow people need, then beat self-hosted Sentry on installation,
resource use, operational clarity and raw performance.

**Who it is for:** teams that want the Sentry workflow without the SaaS bill,
data-residency questions or the operational weight of self-hosted Sentry.

## Metric vs. self-hosted Sentry

| | **Metric 0.1.3** | **Self-hosted Sentry 26.7.2** |
| --- | --- | --- |
| Core stack | One Rust application serving the bundled Vue UI, plus MongoDB | Sentry, Relay, Snuba, Kafka, ClickHouse, PostgreSQL, Redis, workers and other services |
| Compose footprint | **2 services** in Min/Low; **4 services** in Medium/High with Symbolicator and its cleanup companion | **65 service definitions** in the official release Compose file¹ |
| Smallest documented machine | **1 vCPU, 1 GiB RAM, 15 GiB SSD** with the Min profile | **4 CPU, 16 GB RAM + 16 GB swap, 20 GB disk**; 32 GB RAM recommended² |
| Installation | One `curl` command; no clone and no local Rust/Node toolchain | Clone a release, run its installer and operate the generated stack |
| SDK migration | Keep supported official Sentry SDKs and replace the DSN | Native Sentry target |
| Published Metric result | **4,983 durable error events/s**, zero acknowledged loss in a short local reference run | No directly comparable per-instance result is published in the official self-hosted requirements |
| Intended deployment | Small VPS through a larger single-node installation | The official repository describes self-hosted Sentry as packaged for low-volume deployments and proofs of concept³ |

¹ The pinned
[Sentry 26.7.2 Compose file](https://github.com/getsentry/self-hosted/blob/26.7.2/docker-compose.yml)
declares 65 services. A service definition is not necessarily a distinct
always-running container: the file also contains optional profiles and setup or
maintenance jobs.

² See Sentry's
[official self-hosted requirements](https://develop.sentry.dev/self-hosted/).

³ See the
[official self-hosted repository](https://github.com/getsentry/self-hosted).

This comparison is about deployment shape, not a claim that the two products
already have identical feature coverage. Metric publishes its own reproducible
performance evidence and does not invent a Sentry throughput number.

## Measured performance

The main reference machine was an AMD Ryzen 5 5600H (6 cores, 12 threads) with
16 GiB RAM, a colocated load generator and MongoDB 8 on the same machine. Raw
artifacts are committed in [`performance/baselines/`](performance/baselines/).

| Workload | Result | Latency | Important qualification |
| --- | ---: | ---: | --- |
| Durable error ingest, 5,000/s target | **4,983 events/s** | p95 27.7 ms, p99 31.3 ms | 74,809 HTTP acknowledgements = 74,809 durable events; zero acknowledged loss; 190 generator-dropped iterations |
| Saturation probe, 20,000/s target | **7,307 durable events/s** | p95 296.8 ms | Zero acknowledged loss, but the arrival-rate and latency gates did not pass |
| 100M-events/day envelope smoke | **1,156 events/s** | p95 24.9 ms | Zero HTTP errors and zero dropped iterations in a short run |
| HTTP parse/auth/scrub path | **2,500 requests/s** | p99 0.62 ms | Uses a deterministic sink, not MongoDB durability |
| Normalizer | **77,840 events/s** weighted | — | In-process CPU boundary |
| MongoDB span writer | **32,639 spans/s** | — | Zero acknowledged loss |
| Processor backlog recovery | **1.56×** accepted steady rate | — | 1,000-event real-MongoDB fixture |
| Timeline query API | **511 queries/s** | p95 2.49 ms | 2,000-event fixture, page size 50 |

These are short regression references, not promises for every server. Event
size, enabled features, storage latency and workload mix all change capacity.
The 5,000/s and 20,000/s runs are documented honestly even where the load
generator missed its target. See
[capacity and sizing](https://biosshot.github.io/metric/capacity) before planning
a large installation.

Reproduce the headline workload on a suitable development machine:

```powershell
./performance/run-release-load.ps1 -Rps 5000 -Duration 15s
```

The design envelope targets 100 million events per day: 1,158/s average,
5,000/s steady headroom and a bounded 20,000/s burst. The model and remaining
release gates are documented in
[ADR-0037](arch-docs/0037-capacity-model-for-100-million-events-per-day.md) and
[`performance/README.md`](performance/README.md).

## Keep your Sentry SDK

Change the DSN. Keep the SDK.

```javascript
Sentry.init({ dsn: "http://<key>@your-metric-host:4001/<project_id>" });
```

Metric 0.1.3 is tested with real processes of:

- `@sentry/browser` 10.66.0 and `@sentry/node` 10.66.0;
- Python `sentry-sdk` 2.32.0 and Java `sentry-java` 8.50.1;
- .NET `Sentry` 6.7.0, Go `sentry-go` 0.48.0 and Rust `sentry` 0.48.5;
- `sentry-cli` 3.6.2 and 2.58.6 for debug files, source maps and artifact
  bundles.

Compatibility claims are fail-closed: Metric advertises a platform only after
its release test passes. Untested SDK families are listed explicitly in
[SDK compatibility](https://biosshot.github.io/metric/compatibility).

## What you get

- **Errors and issues** — durable ingest, grouping, search, event details,
  timelines and hourly trends.
- **Logs and traces** — structured logs, transactions, spans and trace views
  with bounded MongoDB writers.
- **Application metrics** — counters, gauges, distributions, dashboards and
  saved searches.
- **Releases** — deploy tracking, sessions and release health.
- **Monitoring** — cron check-ins and active HTTP uptime monitors.
- **Alerts** — email, Telegram and signed webhook delivery with retry history.
- **User evidence** — feedback, screenshots where attachments are enabled and
  Incident Capsule export.
- **Privacy and access** — mandatory pre-storage scrubbing or pseudonymization,
  Argon2id passwords, CSRF-protected sessions, scoped tokens, roles and audit
  records.
- **Files** — local or S3-compatible BlobStore, attachments, debug files,
  JavaScript source maps and optional cold archives.
- **Symbolication** — Symbolicator starts automatically in Medium and High;
  Min and Low keep raw frames without running it.
- **Session Replay** — implemented and enabled explicitly per project after a
  privacy decision.

The exact supported, optional and unavailable capabilities are kept in
[supported features](https://biosshot.github.io/metric/supported-capabilities).

## Install with one command

Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.3/deploy/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/biosshot/metric/v0.1.3/deploy/install.ps1 | iex
```

The installer creates a `metric` directory, generates private passwords, pulls
`ghcr.io/biosshot/metric:0.1.3` and starts the recommended Medium profile.
Running the installer again reuses the existing `.env` and database password.
You do not need to clone the repository.

Open `http://localhost:4001` or `http://SERVER_IP:4001`, then read the one-time
setup token:

```bash
cd metric
docker compose logs metric
```

Copy the value after `METRIC_BOOTSTRAP_TOKEN=` and follow the
[first setup guide](https://biosshot.github.io/metric/first-setup).

The supplied profiles deliberately allow sign-in over HTTP so a fresh server
works immediately. Put HTTPS in front of Metric before exposing it publicly or
storing important data.

### Choose a server profile

| Profile | Suggested server | BlobStore | Symbolicator | Error retention |
| --- | --- | ---: | --- | ---: |
| Min | 1 vCPU, 1 GiB RAM, 15 GiB SSD | 5 GiB | No | 30 days |
| Low | 2 vCPU, 2 GiB RAM, 30 GiB SSD | 10 GiB | No | 60 days |
| Medium | 4 vCPU, 8 GiB RAM, 100 GiB SSD | 33 GiB | Yes | 90 days |
| High | 8 vCPU, 16 GiB RAM, 250 GiB SSD | 83 GiB | Yes | 180 days |

Install Min on a small Linux server:

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.3/deploy/install.sh \
  | METRIC_PROFILE=min sh
```

Min and Low run Metric plus MongoDB. Medium and High additionally run
Symbolicator and its cleanup companion. Each profile changes memory ceilings,
ingest queues, batch sizes, upload limits and retention together; the full
reasoning is in [capacity and profiles](https://biosshot.github.io/metric/capacity).

For manual Compose installation, a local MongoDB, HTTPS, backup and Windows
details, use the complete
[installation guide](https://biosshot.github.io/metric/getting-started).

## Honest limits

Metric is not yet feature-complete against Sentry. Profiling, SSO, SCIM, MFA and
passkeys are not available. Apple/Cocoa, Flutter/Dart, Android/Kotlin, native
C++, PHP, React Native and Ruby SDKs are not claimed until their release tests
exist.

The supplied deployment is deliberately single-node:

- one Metric process and one MongoDB server;
- no sharding, split processing roles or built-in high availability;
- no built-in backup/restore command;
- no automatic migration between database schema generations;
- cold archives cannot yet be searched or restored through Metric.

The current binary requires MongoDB schema generation **19 exactly**. Never
delete the database, Docker volumes or the `schema_meta` record to fix a version
mismatch. Follow the [update guide](https://biosshot.github.io/metric/upgrading).

Minidumps and cold archive exist but remain disabled in the supplied profiles.
Session Replay is off for each new project. These features require an explicit
privacy or recovery decision, not merely a larger server.

The complete current list is maintained in
[known limits](https://biosshot.github.io/metric/known-limits).

## Documentation

- [Install Metric](https://biosshot.github.io/metric/getting-started)
- [First setup](https://biosshot.github.io/metric/first-setup)
- [Connect an SDK](https://biosshot.github.io/metric/sdk-setup)
- [Docker](https://biosshot.github.io/metric/docker)
- [Configuration](https://biosshot.github.io/metric/configuration)
- [Capacity and profiles](https://biosshot.github.io/metric/capacity)
- [Updates and rollback](https://biosshot.github.io/metric/upgrading)
- [Troubleshooting](https://biosshot.github.io/metric/troubleshooting)

## Support the project

Metric is developed in the open. If it saves you a Sentry invoice or a weekend
of operating a 65-service Compose file, you can support development:

| Network | USDT address |
| --- | --- |
| TON | `UQBo1KI8Llrp9D7xSF3o0RzMHsM3UesueXRnSTyLPDkOG7-Q` |
| Tron (TRC-20) | `TYiGUA56h1r19FEDzKRn33w511yTqZwy4V` |
| Ethereum (ERC-20) | `0x040835b43916307b4014322439a18cb33B26913F` |

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for local setup, checks and pull requests.
Architecture decisions and implementation reports live in
[`arch-docs/`](arch-docs/); benchmark commands and comparison rules live in
[`performance/README.md`](performance/README.md).

Metric is released under the [MIT License](LICENSE). Medium and High also pull
Symbolicator under its own license; see
[third-party notices](THIRD_PARTY_NOTICES.md).
