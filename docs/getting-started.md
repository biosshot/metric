# Install Metric

You need Docker with Docker Compose. You do not need Git, Rust, Node.js or a copy
of the source repository.

## Choose a profile

| Profile | Suggested server | What it is for |
| --- | --- | --- |
| **Min** | 1 vCPU, 1 GiB RAM, 15 GiB SSD | A few small projects and rare errors. |
| **Low** | 2 vCPU, 2 GiB RAM, 30 GiB SSD | A small everyday installation. |
| **Medium** | 4 vCPU, 8 GiB RAM, 100 GiB SSD | The recommended full-featured default. |
| **High** | 8 vCPU, 16 GiB RAM, 250 GiB SSD | Higher traffic, longer retention and larger files. |

Min and Low do not start Symbolicator. They still receive, group and display
ordinary errors, but they do not process uploaded debug files or source maps.
Medium and High start Symbolicator and its cache cleanup automatically.

These are starting points, not event-rate guarantees. See
[Capacity and profiles](capacity.md) for the complete feature and retention
comparison.

## Install with one command

Medium is used when no profile is specified.

### Linux or macOS

Medium:

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/install.sh | sh
```

Another profile:

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/install.sh \
  | METRIC_PROFILE=min sh
```

Replace `min` with `low` or `high` when needed.

### Windows PowerShell

Medium:

```powershell
irm https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/install.ps1 | iex
```

Another profile:

```powershell
$env:METRIC_PROFILE = 'min'
irm https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/install.ps1 | iex
Remove-Item Env:METRIC_PROFILE
```

The installer:

1. creates a `metric` directory;
2. downloads one Compose file and the selected Metric profile;
3. generates random passwords and stores them in `metric/.env`;
4. pulls only the images used by that profile;
5. starts the containers and waits until they are healthy.

You can review the
[Linux installer](https://github.com/biosshot/metric/blob/v0.1.1/deploy/install.sh)
or [Windows installer](https://github.com/biosshot/metric/blob/v0.1.1/deploy/install.ps1)
before running it.

## Open Metric

Open `http://localhost:4001` in your browser. From another machine, open
`http://SERVER_IP:4001`. The supplied profiles allow sign-in over HTTP so the
first start works without a proxy.

HTTP does not encrypt passwords, session cookies or application data. Use HTTPS
before exposing Metric to the internet or storing important data. See
[Docker](docker.md#https).

Show the first setup token:

```bash
cd metric
docker compose logs metric
```

Find `METRIC_BOOTSTRAP_TOKEN=` and copy its value. Continue with
[First setup](first-setup.md).

## Files created by the installer

```text
metric/
|-- compose.yml
|-- metric.toml
|-- symbolicator.yml
`-- .env
```

All four files must stay in the same directory:

- [`compose.yml`](https://github.com/biosshot/metric/blob/v0.1.1/deploy/compose.yml)
  describes all available containers;
- `metric.toml` is the selected Min, Low, Medium or High configuration;
- [`symbolicator.yml`](https://github.com/biosshot/metric/blob/v0.1.1/deploy/symbolicator.yml)
  is used only by Medium and High;
- `.env` contains the selected profile, passwords, image versions and Docker
  resource limits.

Keep `.env` private and retain its secret values during updates.

::: warning Existing installation
The installer does not overwrite an existing `.env` or `metric.toml`. You can
rerun it from the directory that contains `metric/` or from inside `metric/`;
both reuse the same files and passwords. Setting `METRIC_PROFILE` and running it
again does not silently change a live installation.

If the MongoDB volume exists but `.env` is missing, the installer stops and asks
you to restore the original file. It never generates a new password for existing
data.
:::

## Manual installation

Create an empty directory and download:

- [compose.yml](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/compose.yml)
- [symbolicator.yml](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/symbolicator.yml)

Then choose one pair and save it under the name shown:

| Profile | Save as `metric.toml` | Save as `.env` |
| --- | --- | --- |
| Min | [min.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/profiles/min.toml) | [min.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/profiles/min.env.example) |
| Low | [low.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/profiles/low.toml) | [low.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/profiles/low.env.example) |
| Medium | [medium.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/profiles/medium.toml) | [medium.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/profiles/medium.env.example) |
| High | [high.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/profiles/high.toml) | [high.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.1/deploy/profiles/high.env.example) |

Replace the two placeholder secrets in `.env`:

Linux or macOS:

```bash
openssl rand -hex 24
openssl rand -hex 32
```

Windows PowerShell:

```powershell
$bytes = New-Object byte[] 24
[Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
[BitConverter]::ToString($bytes).Replace('-', '').ToLower()

$bytes = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
[BitConverter]::ToString($bytes).Replace('-', '').ToLower()
```

Use the first value for `METRIC_MONGO_PASSWORD` and the second for
`METRIC_SCRUB_HMAC_KEY`. Start the selected profile:

```bash
docker compose up -d --wait --wait-timeout 120
```

Check the result:

```bash
docker compose ps
curl http://localhost:4001/ready
```

## Useful commands

Run these commands inside the `metric` directory.

```bash
docker compose logs -f metric
docker compose down
docker compose up -d --wait --wait-timeout 120
```

::: danger Keep your data
Do not add `-v` to `docker compose down`. That option deletes the MongoDB and file
storage volumes.
:::

::: warning Optional Symbolicator license
Medium and High pull Sentry Symbolicator 26.6.0 as an independent third-party
container. It is not covered by Metric's MIT License. Review the
[third-party notice](https://github.com/biosshot/metric/blob/main/THIRD_PARTY_NOTICES.md).
:::
