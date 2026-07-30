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
- `metric.toml` is not beside `compose.yml`;
- `symbolicator.yml` is not beside `compose.yml`;
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

## The browser returns to the sign-in page

When opening Metric through a remote hostname, use HTTPS. Secure login cookies are
not sent over ordinary remote HTTP connections.

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

If Symbolicator is unhealthy, inspect its logs:

```bash
docker compose logs symbolicator
```

Its cache is rebuildable. If the logs report corrupted cache data, stop the
installation and remove only the `metric_symbolicator-cache` volume. Never remove
`metric_mongo-data` or `metric_blob-data` while troubleshooting Symbolicator.

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
