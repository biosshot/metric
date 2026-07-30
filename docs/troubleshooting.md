# Troubleshooting

Run the Docker commands below from the directory containing `compose.yml`,
`metric.toml`, `symbolicator.yml` and `.env`.

## Metric does not start

Show the logs:

```bash
docker compose logs metric
```

Common causes:

- a placeholder remains in `.env`;
- `METRIC_PROFILE` in `.env` does not match the installed `metric.toml`;
- `metric.toml` is not beside `compose.yml`;
- on Medium or High, `symbolicator.yml` is not beside `compose.yml`;
- `METRIC_SCRUB_HMAC_KEY` is not 64 hexadecimal characters;
- port 4001 is already used;
- MongoDB is still starting;
- the database schema belongs to another Metric version.

## Check the configuration

```bash
docker compose run --rm --no-deps metric \
  --config /etc/metric/metric.toml \
  --check-config
```

Metric reports unknown settings and invalid values before starting.

## MongoDB reports `Authentication failed` after restart

MongoDB creates its `metric` user only when the data volume is empty. Its password
must continue to match `METRIC_MONGO_PASSWORD` in the original `.env`.

Do not delete `metric_mongo-data`. Restore the original `.env`, remove any shell
override with `unset METRIC_MONGO_PASSWORD`, and recreate the containers:

```bash
cd /root/metric
unset METRIC_MONGO_PASSWORD
docker compose up -d --force-recreate
```

The installer preserves an existing `.env`. If the volume exists but that file
is missing, it stops instead of generating a new password.

## The browser returns to the sign-in page

The supplied Docker profiles support sign-in over HTTP. Check that `metric.toml`
contains both settings:

```toml
[development]
allow_insecure_cookies = true

[auth]
secure_cookie = false
```

Then restart Metric with `docker compose up -d`. If you use a custom
configuration with `auth.secure_cookie = true`, open Metric through HTTPS.

## The SDK sends no events

Check:

1. the DSN belongs to the selected project;
2. the DSN key is active;
3. the application can reach the Metric hostname;
4. the project accepts Error Events;
5. the Metric `/ready` endpoint returns HTTP 200.

Then inspect the SDK debug log and the Metric container log.

## Readiness fails

```bash
curl -i http://localhost:4001/ready
docker compose ps
```

Check the MongoDB container first. If it is unhealthy, inspect its logs:

```bash
docker compose logs mongodb
```

On Medium or High, inspect Symbolicator when it is unhealthy:

```bash
docker compose logs symbolicator
```

Its cache is rebuildable. If the logs report corrupted cache data, stop the
installation and remove only the `metric_symbolicator-cache` volume. Never remove
`metric_mongo-data` or `metric_blob-data` while troubleshooting Symbolicator.

Min and Low do not show a Symbolicator container in `docker compose ps`. That is
expected.

## Container restarts or out-of-memory errors

Check current use and the selected profile:

```bash
docker stats
grep '^METRIC_PROFILE=' .env
```

On Min, configure 1–2 GiB of host swap and stop unrelated services. Do not raise
the Metric limit without leaving memory for MongoDB and Linux. Repeated restarts
under normal traffic mean the installation should move to the next profile.

If the server stays up but returns `429` or `503` during a brief spike, its
bounded queues are protecting it from running out of memory.

## Disk is filling

Check Docker and volume use:

```bash
docker system df
docker compose logs --tail=50 metric mongodb
```

Do not delete files directly from `mongo-data` or `blob-data`. Reduce SDK log,
trace or Replay volume, shorten retention in `metric.toml`, or move to a larger
disk. Container logs already rotate according to the selected profile.

## Schema mismatch

Stop the update and keep the data unchanged. Do not edit `schema_meta`, drop the
database or delete Docker volumes. Follow [Update Metric](upgrading.md).

## Ask for help

When opening a GitHub issue, include:

- Metric version;
- Docker or binary installation;
- operating system;
- the exact error;
- relevant logs with passwords, tokens, DSNs and private event data removed.
