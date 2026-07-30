# Install Metric

You need Docker with Docker Compose. You do not need Git, Rust, Node.js or a copy
of the source repository.

## Install with one command

### Linux or macOS

```bash
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/v0.1.0/deploy/install.sh | sh
```

### Windows PowerShell

```powershell
irm https://raw.githubusercontent.com/biosshot/metric/v0.1.0/deploy/install.ps1 | iex
```

The installer:

1. creates a `metric` directory;
2. downloads only the Compose and Metric configuration files;
3. generates random passwords and stores them in `metric/.env`;
4. pulls the published Metric and MongoDB images;
5. starts both containers.

You can review the
[Linux installer](https://github.com/biosshot/metric/blob/v0.1.0/deploy/install.sh)
or [Windows installer](https://github.com/biosshot/metric/blob/v0.1.0/deploy/install.ps1)
before running it.

## Open Metric

Open `http://localhost:4001` in your browser.

If you open Metric from another machine, place an HTTPS proxy in front of port
4001. Remote sign-in over plain HTTP is not supported. See [Docker](docker.md).

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
`-- .env
```

All three files must stay in the same directory:

- [`compose.yml`](https://github.com/biosshot/metric/blob/v0.1.0/deploy/compose.yml)
  describes the Metric and MongoDB containers;
- [`metric.toml`](https://github.com/biosshot/metric/blob/v0.1.0/deploy/metric.toml)
  contains the container settings;
- `.env` contains passwords, the image version and the public port.

Keep `.env` private and retain its values during updates.

## Manual installation

If you do not want to run an installer, create an empty directory and download
the three example files:

- [compose.yml](https://raw.githubusercontent.com/biosshot/metric/v0.1.0/deploy/compose.yml)
- [metric.toml](https://raw.githubusercontent.com/biosshot/metric/v0.1.0/deploy/metric.toml)
- [.env.example](https://raw.githubusercontent.com/biosshot/metric/v0.1.0/deploy/.env.example)

Save `.env.example` as `.env`. Replace the two placeholder secrets:

```bash
openssl rand -hex 24
openssl rand -hex 32
```

Use the first value for `METRIC_MONGO_PASSWORD` and the second for
`METRIC_SCRUB_HMAC_KEY`. Then run this command from that directory:

```bash
docker compose up -d
```

Check the result:

```bash
docker compose ps
curl http://localhost:4001/ready
```

## Configuration examples

- [Container configuration](https://github.com/biosshot/metric/blob/v0.1.0/deploy/metric.toml)
  is ready for Docker and must be saved beside `compose.yml`.
- [Complete configuration example](https://github.com/biosshot/metric/blob/v0.1.0/config/metric.example.toml)
  shows advanced settings for the standalone binary. Its local paths and listen
  address are not intended to replace the container configuration unchanged.
- [Configuration reference](configuration.md) explains every setting.

## Useful commands

Run these commands inside the `metric` directory.

View logs:

```bash
docker compose logs -f metric
```

Stop without deleting data:

```bash
docker compose down
```

Start again:

```bash
docker compose up -d
```

::: danger Keep your data
Do not add `-v` to `docker compose down`. That option deletes the MongoDB and file
storage volumes.
:::
