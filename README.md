# Metric

<p>
  <img src="web/public/favicon.svg" width="72" alt="Metric logo" />
</p>

[Documentation](https://biosshot.github.io/metric/) ·
[Installation](https://biosshot.github.io/metric/getting-started) ·
[Configuration](https://biosshot.github.io/metric/configuration)

Metric is a self-hosted error tracking service that works with official Sentry
SDKs. It receives errors and other application data, groups repeated errors into
issues, and shows everything in a built-in web interface.

The supplied Docker setup has four resource profiles. The smallest runs Metric and
MongoDB on a 1 GiB server; Medium and High also start Symbolicator. You do not need
to clone this repository or install Rust and Node.js. Your application data stays
in storage that you control.

> Metric 0.1.0 is an early release. Read the
> [known limits](https://biosshot.github.io/metric/known-limits) before using it
> for important production data.

## Install

Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.0/deploy/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/biosshot/metric/v0.1.0/deploy/install.ps1 | iex
```

The installer creates a `metric` directory, generates private passwords, pulls
the container images and starts the recommended Medium profile.

Available profiles:

| Profile | Suggested server | Symbolicator | Raw error retention |
| --- | --- | --- | --- |
| Min | 1 vCPU, 1 GiB RAM, 15 GiB SSD | No | 7 days |
| Low | 2 vCPU, 2 GiB RAM, 30 GiB SSD | No | 14 days |
| Medium | 4 vCPU, 8 GiB RAM, 100 GiB SSD | Yes | 30 days |
| High | 8 vCPU, 16 GiB RAM, 250 GiB SSD | Yes | 90 days |

For example, install Min on Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.0/deploy/install.sh \
  | METRIC_PROFILE=min sh
```

Open `http://localhost:4001`, then get the one-time setup token:

```bash
cd metric
docker compose logs metric
```

Copy the value after `METRIC_BOOTSTRAP_TOKEN=` and follow the
[first setup guide](https://biosshot.github.io/metric/first-setup).

For manual installation, Windows instructions and HTTPS, see the complete
[installation guide](https://biosshot.github.io/metric/getting-started).

## What Metric includes

- errors, messages, logs, transactions and spans;
- private-data removal or pseudonymization before storage;
- issue grouping, search and event details;
- releases, deploys and release health;
- cron and HTTP monitors;
- application metrics and dashboards;
- email, Telegram and signed webhook notifications;
- user feedback and Incident Capsule export;
- attachments, plus debug-file and source-map processing in Medium and High;
- optional Session Replay, minidumps, S3 storage and cold archives;
- users, projects, roles, API tokens, retention and ingest limits.

See [supported features](https://biosshot.github.io/metric/supported-capabilities)
for the complete list.

## Sentry SDK compatibility

Keep the official Sentry SDK used by your application and replace its DSN with
the DSN created by Metric.

Metric 0.1.0 is tested with browser JavaScript, Node.js, Python, Java, .NET, Go
and Rust SDKs. Exact tested versions and unsupported platforms are listed in
[SDK compatibility](https://biosshot.github.io/metric/compatibility).

## Performance

A committed reference test reached 4,983 durable error events per second with
no acknowledged loss on a 6-core AMD Ryzen 5 5600H with 16 GiB RAM. This is a
reference result, not a guarantee for every installation. Event size, enabled
features, storage and hardware all affect capacity.

Raw results are stored in [`performance/baselines/`](performance/baselines/).
See [capacity and sizing](https://biosshot.github.io/metric/capacity) before
planning a large installation.

## Important limits

- every supplied profile uses one Metric process and one MongoDB server;
- Medium and High also use one Symbolicator process;
- high availability, sharding and multiple Metric processing nodes are not
  included;
- profiling is not supported;
- Metric does not yet include its own backup and restore command;
- automatic migration between database schema generations is not available.

The current binary requires MongoDB schema generation **19 exactly**. Never
delete the database, Docker volumes or the `schema_meta` record to fix a version
mismatch. Follow the [update guide](https://biosshot.github.io/metric/upgrading).

## Documentation

- [Install Metric](https://biosshot.github.io/metric/getting-started)
- [First setup](https://biosshot.github.io/metric/first-setup)
- [Connect an SDK](https://biosshot.github.io/metric/sdk-setup)
- [Docker](https://biosshot.github.io/metric/docker)
- [Configuration](https://biosshot.github.io/metric/configuration)
- [Troubleshooting](https://biosshot.github.io/metric/troubleshooting)

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for local setup, checks and pull requests.
Architecture decisions and implementation notes are kept in
[`arch-docs/`](arch-docs/).

## Support the project

| Network | USDT address |
| --- | --- |
| TON | `UQBo1KI8Llrp9D7xSF3o0RzMHsM3UesueXRnSTyLPDkOG7-Q` |
| Tron (TRC-20) | `TYiGUA56h1r19FEDzKRn33w511yTqZwy4V` |
| Ethereum (ERC-20) | `0x040835b43916307b4014322439a18cb33B26913F` |

Metric is released under the [MIT License](LICENSE). Medium and High also pull
Symbolicator under its own license; see
[third-party notices](THIRD_PARTY_NOTICES.md).
