# Install Metric

This guide starts Metric and MongoDB with Docker Compose.

## 1. Download the project

```bash
git clone https://github.com/biosshot/metric.git
cd metric
```

## 2. Create the environment file

```bash
cp deploy/release.env.example deploy/release.env
```

Generate two random values:

```bash
openssl rand -hex 24
openssl rand -hex 32
```

Open `deploy/release.env`:

- use the first value for `METRIC_MONGO_PASSWORD`;
- use the second value for `METRIC_SCRUB_HMAC_KEY`;
- leave `METRIC_HTTP_PORT=4001` unless that port is already used.

Keep this file private. Do not commit it to Git.

## 3. Start Metric

```bash
docker compose \
  --env-file deploy/release.env \
  -f deploy/compose.release.yml \
  up -d
```

Check that both containers are running:

```bash
docker compose \
  --env-file deploy/release.env \
  -f deploy/compose.release.yml \
  ps
```

Open `http://localhost:4001` in your browser.

## 4. Copy the first setup token

```bash
docker compose \
  --env-file deploy/release.env \
  -f deploy/compose.release.yml \
  logs metric
```

Find the line beginning with `METRIC_BOOTSTRAP_TOKEN=` and copy the value. Metric
shows this token only for the first setup.

Continue with [First setup](first-setup.md).

## Useful commands

View logs:

```bash
docker compose --env-file deploy/release.env -f deploy/compose.release.yml logs -f metric
```

Stop Metric without deleting data:

```bash
docker compose --env-file deploy/release.env -f deploy/compose.release.yml down
```

::: danger Keep your data
Do not add `-v` to `docker compose down`. That option deletes the MongoDB and file
storage volumes.
:::
