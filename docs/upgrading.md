# Update Metric

Metric container versions follow the `MAJOR.MINOR.PATCH` format, for example
`0.1.4`.

## Before updating

1. Read the release notes for the new version.
2. Check which MongoDB schema generation it requires.
3. Back up MongoDB and file storage together.
4. Keep a copy of `.env`, `metric.toml` and the previous image version.
5. Note `METRIC_PROFILE` from `.env`.

The current Metric binary requires MongoDB schema generation **19 exactly**.

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
| Empty | Creates schema generation 19 | Wait for `/ready` |
| Complete generation 19 | Starts normally | No schema action |
| Older generation | Refuses to start | Stop and keep the data unchanged |
| Newer or different generation | Refuses to start | Use the matching Metric version |
| Non-empty database without Metric metadata | Refuses to start | Check the database name; do not erase it |

Metric 0.1.4 cannot automatically migrate an older database to generation 19.
An empty-database setup is not a migration.

Changing back to an older image is safe only when that image supports the same
schema generation. Do not assume that changing the image tag is always a valid
rollback.

## Backup rule

Treat MongoDB and the configured BlobStore as one operational unit. A MongoDB
copy and a file-storage copy made at different times may not match each other.
Keep and restore both together.

Until a tested migration is published for the exact old and new schema
generations, keep the old version and do not modify the existing data.
