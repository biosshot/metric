# Update Metric

Metric container versions follow the `MAJOR.MINOR.PATCH` format, for example
`0.1.5`.

## Before updating

1. Read the release notes for the new version.
2. Check which MongoDB schema generation it requires.
3. Back up MongoDB and file storage together.
4. Keep a copy of `.env`, `metric.toml` and the previous image version.
5. Note `METRIC_PROFILE` from `.env`.

The current Metric binary targets MongoDB schema generation **20**. It automatically
migrates a complete generation-19 database to generation 20 before ordinary startup.

::: danger Protect existing data
You must never drop or recreate a data-bearing MongoDB database to make another
Metric version start. Never edit the `schema_meta` generation manually.
Changing that number does not migrate the stored data.
:::

## Install the new image

If the release notes do not require new profile files, open `.env` and change
only `METRIC_IMAGE`:

```text
METRIC_IMAGE=ghcr.io/biosshot/metric:<new version>
```

Then run:

```bash
docker compose pull metric
docker compose up -d --wait --wait-timeout 120
docker compose ps
curl http://localhost:4001/ready
```

If Metric does not become ready, read its logs:

```bash
docker compose logs --tail=200 metric
```

When release notes publish changed Compose or profile settings, compare them
with your saved files before restarting. Do not replace `.env` wholesale: it
contains the installation secrets. Do not silently switch Min, Low, Medium or
High as part of an ordinary version update.

## Schema compatibility

| Database state | What Metric does | What you should do |
| --- | --- | --- |
| Empty | Creates schema generation 20 | Wait for `/ready` |
| Complete generation 20 | Starts normally | No schema action |
| Complete generation 19 | Migrates automatically to generation 20, then starts | Keep the browser or logs open and wait for `/ready` |
| Older generation with a complete transition chain in the new image | Migrates automatically, then starts | Keep the browser or logs open and wait for `/ready` |
| Older generation without a complete transition chain | Exits with a stable error | Stop and keep the data unchanged |
| Newer or different generation | Refuses to start | Use the matching Metric version |
| Non-empty database without Metric metadata | Refuses to start | Check the database name; do not erase it |

While a published migration runs, `/live` remains HTTP 200 and `/ready` remains
HTTP 503. Browser navigation shows a maintenance page with completed and remaining
steps; API and SDK requests receive HTTP 503 with `Retry-After`. Metric starts no
ordinary application workers until the target schema has been verified.

There is no separate migration command or interactive confirmation. Starting the
new image authorizes every required transition shipped in that image. Transient
database failures are retried inside the process. A missing transition, newer
schema, ambiguous transformation or failed required invariant stops startup with a
nonzero exit instead of guessing or deleting data.

Generation 19 to 20 is the only published transition in this binary. It adds the
default Telegram Bot API configuration in bounded, resumable batches and does not
delete or decrypt data. A generation older than 19 still fails closed because the
required earlier link is absent. An empty-database setup is not a migration.

Migrations are forward-only. Changing back to an older image is safe only when that
image supports the resulting schema generation. Do not assume that changing the
image tag is always a valid rollback.

## Backup rule

Treat MongoDB and the configured BlobStore as one operational unit. A MongoDB
copy and a file-storage copy made at different times may not match each other.
Keep and restore both together.

Until a tested migration is published for the exact old and new schema
generations, keep the old version and do not modify the existing data.
