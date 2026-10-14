# Install Metric

You need Docker with Docker Compose. You do not need Git, Rust, Node.js or a copy
of the source repository.

::: info Release preparation
The 0.1.8 installer is being prepared on `feat/postgres`. The versioned download
commands below become available when 0.1.8 is published; they do not install an
already published 0.1.7 image with the new options.
:::

## Interactive installation

Use the setup wizard when installing for the first time. It asks which database
to use, whether to start a database with Metric or connect an existing one, the
resource profile and Metric's HTTP port. Existing-database setup accepts one
**visibly entered connection URI**; TLS and certificate questions appear only
when needed. It does not ask about queues, retention or other internal settings.

### Linux or macOS

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/install.sh \
  | METRIC_INTERACTIVE=1 sh
```

### Windows PowerShell

```powershell
$env:METRIC_INTERACTIVE = '1'
try {
  irm https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/install.ps1 | iex
} finally {
  Remove-Item Env:METRIC_INTERACTIVE -ErrorAction SilentlyContinue
}
```

You can also download and review the script, then run `sh install.sh --interactive`
or `.\install.ps1 -Interactive`. A terminal is required for the wizard. Without
interactive mode, installation never asks questions and keeps the existing defaults.

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

## Installation without questions

Medium is used when no profile is specified.

### Linux or macOS

Medium:

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/install.sh | sh
```

Another profile:

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/install.sh \
  | METRIC_PROFILE=min sh
```

Replace `min` with `low` or `high` when needed.

### Windows PowerShell

Medium:

```powershell
irm https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/install.ps1 | iex
```

Another profile:

```powershell
$env:METRIC_PROFILE = 'min'
irm https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/install.ps1 | iex
Remove-Item Env:METRIC_PROFILE
```

The installer:

1. creates a `metric` directory;
2. downloads one Compose file and the selected Metric profile;
3. generates random passwords and stores them in `metric/.env`;
4. pulls only the images used by that profile;
5. starts the containers and waits until they are healthy.

Without database options these commands retain bundled MongoDB. For a bundled
PostgreSQL instead, set `METRIC_DATABASE_BACKEND=postgres` before invoking the
same installer. It generates the database password; no database URI is needed.
Bundled database ports are not published. Only Metric's selected HTTP port is
published, on all host interfaces by default.

You can review the
[Linux installer](https://github.com/biosshot/metric/blob/v0.1.8/deploy/install.sh)
or [Windows installer](https://github.com/biosshot/metric/blob/v0.1.8/deploy/install.ps1)
before running it.

## Connect an existing database

Create the PostgreSQL database and database user first. The user needs permission
to create/update Metric's `metric` schema. MongoDB needs a user allowed to manage
Metric's database and its indexes. Metric applies its own migrations automatically;
the installer does not configure TLS on your database server or provision an
external PostgreSQL database.

The following unattended examples use verified TLS. Replace the example URI with
your connection details; percent-encode reserved characters in its password.

```bash
export METRIC_POSTGRES_URI='postgres://metric:replace-me@db.example:5432/metric'
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/install.sh \
  | METRIC_DATABASE_BACKEND=postgres METRIC_DATABASE_MODE=external METRIC_PROFILE=low sh
unset METRIC_POSTGRES_URI
```

```powershell
$env:METRIC_POSTGRES_URI = 'postgres://metric:replace-me@db.example:5432/metric'
$env:METRIC_DATABASE_BACKEND = 'postgres'
$env:METRIC_DATABASE_MODE = 'external'
$env:METRIC_PROFILE = 'low'
try {
  irm https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/install.ps1 | iex
} finally {
  'METRIC_POSTGRES_URI','METRIC_DATABASE_BACKEND','METRIC_DATABASE_MODE','METRIC_PROFILE' |
    ForEach-Object { Remove-Item "Env:$_" -ErrorAction SilentlyContinue }
}
```

For MongoDB, use `MONGODB_URI`, select `mongodb`, and supply a URI such as
`mongodb://metric:replace-me@db.example:27017/metric?authSource=admin&tls=true`.
`/metric` selects the data database; `authSource=admin` selects the authentication
database. Without a URI data database, set `METRIC_MONGO_DATABASE` or accept `metric`.

To explicitly use plain TCP, use PostgreSQL `?sslmode=disable` or MongoDB
`&tls=false` in the URI. TLS failure never falls back to TCP. Custom CA and client
certificate files are described in [Installer options](installer-options.md).
Certificate files are copied into the installation and mounted read-only into Metric.

For a database on this computer, a `localhost` URI is translated to the Docker
host address. Inside a container, `localhost` means the container itself. Linux
host-gateway reachability depends on the database listener and firewall; the
installer does not change either. Named service addresses and remote hostnames
are retained. Use a hostname matching the server certificate for verified TLS.

## Open Metric

Open `http://localhost:4001` in your browser. From another machine, open
`http://SERVER_IP:4001`. Replace `4001` with the HTTP port selected in the wizard
or installer options; the installer prints the actual URL. The supplied profiles allow sign-in over HTTP so the
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
|-- connection.env
|-- certs/             # Only when certificate files are supplied
`-- .env
```

Keep the generated files in the same directory:

- [`compose.yml`](https://github.com/biosshot/metric/blob/v0.1.8/deploy/compose.yml)
  describes all available containers;
- `metric.toml` is the selected Min, Low, Medium or High configuration;
- [`symbolicator.yml`](https://github.com/biosshot/metric/blob/v0.1.8/deploy/symbolicator.yml)
  is used only by Medium and High;
- `.env` contains the selected profile, passwords, image versions and Docker
  resource limits.
- `connection.env` supplies backend TLS settings to the container when needed.
  Leave it beside Compose; it is generated by the installer.
- `certs/` contains installation copies of supplied certificates/keys. Changing
  the original file elsewhere does not replace this copy.

**Edit the installed `metric/metric.toml`, not a profile in the source repository.**
The installer copies the selected source profile into this file. The links below
are templates for reference; [Configuration](configuration.md) explains every field.

Keep `.env` private and retain its secret values during updates.

## Edit settings after installation

Open a terminal in the installed `metric` directory. On Windows:

```powershell
notepad .\metric.toml
```

On Linux/macOS, open `metric.toml` in your preferred editor. For example, change
`events_days` in the existing `[retention]` section to `14`. Do not add a duplicate
section. Save the file, validate it, then restart the app:

```bash
docker compose run --rm --no-deps metric --config /etc/metric/metric.toml --check-config
docker compose restart metric
```

The config check validates fields and secret references; it does not contact the
database. If validation fails, fix the file before restarting. For changes to
`.env`, `connection.env`, certificate mount paths or Compose settings, recreate
the app instead:

```bash
docker compose up -d --force-recreate metric
```

To restrict host access, change the Compose port mapping to
`127.0.0.1:${METRIC_HTTP_PORT:-4001}:4001`. This is a Docker setting: the TOML app
listen address does not restrict the published host port. Changing a bundled
database's initial password variable does not change an existing database user's
password. Changing `storage.backend` does not move data between databases.

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

- [compose.yml](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/compose.yml)
- [symbolicator.yml](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/symbolicator.yml)

Then choose one pair and save it under the name shown:

| Profile | Save as `metric.toml` | Save as `.env` |
| --- | --- | --- |
| Min | [min.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/profiles/min.toml) | [min.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/profiles/min.env.example) |
| Low | [low.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/profiles/low.toml) | [low.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/profiles/low.env.example) |
| Medium | [medium.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/profiles/medium.toml) | [medium.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/profiles/medium.env.example) |
| High | [high.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/profiles/high.toml) | [high.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.8/deploy/profiles/high.env.example) |

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
