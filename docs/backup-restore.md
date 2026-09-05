# Manual backup and restore

You do not need a backup feature inside Metric to save and restore an installation.
Use MongoDB Database Tools for **the Metric database only**, plus a copy of its
BlobStore and installation secrets. This guide does not dump other databases on
the MongoDB server and does not lock the server or disable its TTL monitor.

The simplest procedure pauses **Metric**, not MongoDB. Other applications using
different databases can continue working. Backups still consume database, disk and
network resources; no global lock does not mean zero performance impact.

## Scope and consistency

::: warning Not a point-in-time snapshot
Stop every Metric process using this installation before copying data, and keep
them stopped until **both** the database and BlobStore copies finish. Finish any
schema migration first. Do not change the Metric database manually during backup.

MongoDB's own TTL cleanup continues while Metric is stopped. Documents whose
retention has expired may disappear during the dump or after restore. Collections
are read at different times, so this procedure does **not** guarantee a single
point-in-time image or exact agreement between all historical counters and raw
events. It is a practical small-installation backup with an explicit TTL caveat,
not a zero-loss live-backup promise.

If an exact point-in-time copy is required, use an operator-managed coordinated
database/storage snapshot or another documented consistent backup strategy.
Do not silently apply a server-wide lock to a shared MongoDB instance.
:::

MongoDB documents the [consistency limits of a dump without oplog
replay](https://www.mongodb.com/docs/database-tools/mongodump/#std-option-mongodump.--oplog).
`--oplog` is not a switch to add to this example: it requires a replica-set member
and cannot be combined with `--db`. A full-server dump is not required here.

**Database and collection are different:** `--db metric` saves all collections in
that database, including users, projects, settings and `schema_meta`. Do not add
`--collection error_events`: events alone cannot restore a Metric installation.
Use the database name from `mongodb.database` if it is not `metric`.

## What to keep together

| Component | What to save |
| --- | --- |
| MongoDB | A database-scoped dump, including collection options and index definitions |
| Local BlobStore | The complete configured `blob.root`, preserving paths and ownership |
| S3 BlobStore | Current objects with their original keys **and user metadata** |
| Configuration | `compose.yml`, `.env`, `metric.toml`, `symbolicator.yml`, any overrides and external secret files |
| Versions | Exact Metric image/build, MongoDB version and Database Tools version |

Keep the original `SCRUB_HMAC_KEY` (`METRIC_SCRUB_HMAC_KEY` in Compose's `.env`).
It is also used to derive encryption keys for notification credentials and uptime
headers. Generating a replacement is not a valid way to restore those secrets.
An external secret manager, KMS key or S3 encryption key needs its own recovery plan;
a copy of a configuration file containing a reference does not save the secret.

The Symbolicator cache is rebuildable and can be omitted. Include any separately
configured cold-archive storage too. An archive is not itself an installation
backup, and this guide does not add cold-archive search or import to Metric.

Treat the entire backup as secret data. Restrict access, encrypt it with your backup
tool, and retain a copy outside the original host/storage failure domain. A backup
bucket on the same MinIO disk protects neither against loss of that disk nor loss
of the server. Use a new destination for each backup; do not maintain just one
mirror that overwrites the last good copy.

## Back up a Docker Compose installation

These shell examples use Bash on Linux, macOS or WSL and start in the directory
created by the [installer](getting-started.md). Docker must be able to mount the
chosen backup directory. On PowerShell, use the same Docker arguments on one line;
the host-directory setup is shown separately below. Never pipe binary archives
through a shell's text-processing commands.

### 1. Prepare and pause Metric

Create a new backup directory, then copy the installation files into it:

```bash
umask 077
BACKUP_DIR="$PWD/backups/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$BACKUP_DIR/config"
cp .env metric.toml compose.yml symbolicator.yml "$BACKUP_DIR/config/"
```

PowerShell equivalent for this preparation:

```powershell
$backupDir = Join-Path (Get-Location) ('backups/' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ'))
New-Item -ItemType Directory -Path (Join-Path $backupDir 'config') -ErrorAction Stop
Copy-Item -LiteralPath '.env','metric.toml','compose.yml','symbolicator.yml' -Destination (Join-Path $backupDir 'config') -ErrorAction Stop
```

On Windows, restrict the directory's NTFS permissions to the backup operator.
Also copy any custom Compose override files and externally referenced secrets.
Record the actual deployed image/build, not merely the version shown by a newer
checkout. Record these command outputs privately with the backup:

```bash
docker compose images
docker compose exec -T mongodb mongod --version
docker compose exec -T mongodb mongodump --version
docker compose exec -T mongodb mongorestore --version
docker compose stop metric
docker compose ps -a
```

Verify that the Metric container is stopped. Do not use `docker compose down -v`:
that deletes data volumes. Do not stop the MongoDB service for the logical dump.

### 2. Dump only the Metric database

The supplied `mongo` image already contains `mongodump` and `mongorestore`; the
Metric image does not need them. This command asks for the MongoDB password
interactively, rather than putting the password in shell history:

```bash
docker compose exec mongodb sh -c 'umask 077; exec mongodump "$@"' sh \
  --username metric --authenticationDatabase admin \
  --db metric --archive=/tmp/metric-manual.archive.gz --gzip \
  --numParallelCollections=1
```

Use `METRIC_MONGO_PASSWORD` from the original `.env` when prompted. Wait for a
successful exit before copying the archive. No other backup should use this
temporary filename concurrently. A failed dump can leave a partial file: do not
copy an old or partial archive and call the run successful.

Copy the binary file out without stdout redirection:

```bash
docker compose cp mongodb:/tmp/metric-manual.archive.gz "$BACKUP_DIR/mongodb.archive.gz"
```

On PowerShell, substitute `$backupDir` for `$BACKUP_DIR`; `docker compose cp` avoids
binary-redirection differences between PowerShell versions. After confirming the
copy, remove only the temporary dump inside the container:

```bash
docker compose exec -T mongodb rm -- /tmp/metric-manual.archive.gz
```

`--numParallelCollections=1` reduces concurrent collection reads. It is not a RAM
limit or a consistency guarantee. The tool streams documents instead of requiring
an application-level in-memory copy of the database, but it still adds I/O and
cache pressure. Allow enough disk space for both the temporary dump and its copy.

### 3A. Copy local BlobStore

For the supplied Compose project, the volume is `metric_blob-data`. Verify the
actual name using `docker volume inspect metric_blob-data`; if you changed the
Compose project name or use an external volume, use that volume instead.

Using the same MongoDB image version as your deployment as a temporary tar helper:

```bash
docker run --rm --network none --user 0:0 --entrypoint tar \
  --mount type=volume,source=metric_blob-data,target=/blob-volume,readonly \
  --mount "type=bind,source=$BACKUP_DIR,target=/backup" \
  mongo:8.0.12 czpf /backup/blobs.tar.gz -C /blob-volume .
```

This helper does not run MongoDB or modify the source volume. Check its exit status.
For a native filesystem installation, archive the whole configured `blob.root`
with `tar` or your backup tool while Metric is stopped. Preserve ownership, file
permissions and paths; do not copy only selected attachment folders.

### 3B. Copy S3/MinIO BlobStore instead

Do **not** substitute a download-to-files followed by upload: it loses S3 user
metadata. Metric objects carry `metric-blake3`, `metric-kind` and
`metric-created-ms`; missing metadata can break storage cleanup and verification
even when an individual object can still be downloaded.

For two buckets reachable through the **same S3 endpoint**, AWS CLI v2 can copy
objects with metadata. Configure credentials using your normal AWS profile or
credential provider; do not put secret keys in the command text. Create a private
backup bucket and choose a fresh prefix for this run:

```bash
aws --endpoint-url https://YOUR-S3-ENDPOINT s3 sync \
  s3://METRIC-BUCKET/ s3://BACKUP-BUCKET/UNIQUE-BACKUP-ID/ \
  --copy-props default
```

For AWS S3, omit `--endpoint-url`; select the appropriate profile/region. Do not
use `--delete`, `--copy-props none` or disable certificate verification. The
credentials need source read/list access and destination write access; copying
tags/properties may require additional permissions. See
[AWS CLI property copying](https://docs.aws.amazon.com/cli/latest/reference/s3/sync.html).

Preserve the bucket configuration separately when relevant (encryption and its
keys, lifecycle, access policy, versioning). This copies current objects, not old
versions or bucket configuration. Ensure lifecycle rules and other writers do not
delete or change source objects during the copy, or the snapshot remains incomplete.

This command has one endpoint; it is **not** a cross-endpoint MinIO-to-AWS example.
For another endpoint or an offline filesystem backup, use a tool that explicitly
preserves per-object metadata as well as bytes and keys, and test that restore.
Do not assume a generic mirror/export does so. A live copy of a running MinIO
server's underlying data directory is not the equivalent of an S3 object backup.

### 4. Finish the backup

Only after the database, configuration and selected BlobStore copy have all
succeeded, resume the original installation:

```bash
docker compose start metric
docker compose ps
curl --fail http://localhost:4001/ready
```

If copying failed, record the set as incomplete. You can resume service to end the
outage, but make a **new complete set** next time; do not finish an old partial set
after allowing Metric to change the data again.

Record checksums with your backup tool (`sha256sum` or PowerShell `Get-FileHash`)
and store them with the set. Checksums detect transfer damage; only a restore drill
demonstrates that the installation can be recovered.

## Restore into an empty installation

::: danger Keep the original installation intact
Use a separate directory/Compose project, empty MongoDB database and fresh local
volume or S3 bucket. Never restore over the only live copy. There is no automatic
rollback if `mongorestore` fails partway through. Do not use `--drop` as a shortcut.

Keep Metric stopped until **all** components are restored. Do not run the ordinary
installer against the empty destination first: starting Metric initializes a new
schema and account setup, which is not the target for this procedure.
:::

### 1. Prepare the destination

Restore the saved deployment/configuration files and required secrets. Start with
the **same Metric build**, MongoDB major/feature-compatibility version and matching
`mongodump`/`mongorestore` versions. A restore is not a MongoDB upgrade or a schema
downgrade. See [MongoDB restore compatibility](https://www.mongodb.com/docs/database-tools/mongorestore/mongorestore-behavior-access-usage/).

For an isolated Compose restore, consistently use another project name on every
command, for example `docker compose -p metric-restore ...`; this overrides the
`name: metric` in the saved file. Check that overrides do not refer to the original
external volumes or database. Choose an unused HTTP port in the destination `.env`.
Keep the original scrub key; destination database/S3 credentials must authenticate
to the **destination**, not send restored Metric back to production storage.

Start MongoDB only:

```bash
docker compose -p metric-restore up -d --wait mongodb
docker compose -p metric-restore exec mongodb mongosh \
  --username metric --authenticationDatabase admin \
  --eval 'db.getSiblingDB("metric").getCollectionNames()'
```

The target database must have no collections (`[]`). MongoDB's own `admin`,
`config` and `local` databases are normal and must not be erased. The saved dump
does not replace MongoDB server users: the new server initializes its own account
from the destination configuration.

### 2. Restore MongoDB

Set `BACKUP_DIR` to the absolute directory containing the selected backup set,
then run:

```bash
docker compose -p metric-restore cp "$BACKUP_DIR/mongodb.archive.gz" mongodb:/tmp/metric-restore.archive.gz
docker compose -p metric-restore exec mongodb mongorestore \
  --username metric --authenticationDatabase admin \
  --archive=/tmp/metric-restore.archive.gz --gzip --nsInclude='metric.*' \
  --stopOnError --numParallelCollections=1 --numInsertionWorkersPerCollection=1
```

Check the exit status and the final restored/failed document counts. The archive
contains index definitions and collection validators; do not use `--noIndexRestore`
or `--noOptionsRestore`. Do not start Metric if any part of restore failed. Keep
the failed destination for diagnosis or replace that disposable destination and
start again from the intact backup; rerunning against a partial database is not
the empty-target procedure.

If the database name must change, add `--nsFrom='metric.*' --nsTo='NEW-DATABASE.*'`,
retain `--nsInclude='metric.*'`, and set `mongodb.database` to `NEW-DATABASE` in the
destination configuration. This renames namespaces, not the contents of documents.
After success, remove the temporary `/tmp/metric-restore.archive.gz` inside that
destination container.

### 3A. Restore local BlobStore

With the unchanged Compose file and `-p metric-restore`, the new volume name is
`metric-restore_blob-data`. Confirm this before using it; external/custom volume
names do not necessarily follow that convention. Create the volume and confirm
that it is empty before extracting:

```bash
docker volume create metric-restore_blob-data
docker run --rm --network none --entrypoint sh \
  --mount type=volume,source=metric-restore_blob-data,target=/blob-volume,readonly \
  mongo:8.0.12 -c 'test -z "$(ls -A /blob-volume)"'
docker run --rm --network none --user 0:0 --entrypoint tar \
  --mount type=volume,source=metric-restore_blob-data,target=/blob-volume \
  --mount "type=bind,source=$BACKUP_DIR,target=/backup,readonly" \
  mongo:8.0.12 xzpf /backup/blobs.tar.gz -C /blob-volume
```

Do not continue if the emptiness check fails. Restore only archives you trust;
extraction runs as root in this helper. For a native installation, restore to an
empty directory and ensure the Metric service user can read and write it.

### 3B. Restore S3 BlobStore instead

Create a private **empty destination bucket**, with the required encryption and
access configuration. Copy the matching backup prefix back to the original key
layout, keeping metadata:

```bash
aws --endpoint-url https://YOUR-S3-ENDPOINT s3 sync \
  s3://BACKUP-BUCKET/UNIQUE-BACKUP-ID/ s3://EMPTY-RESTORE-BUCKET/ \
  --copy-props default
```

Point the restored Metric configuration at that bucket. Check object counts and
sizes and use `aws ... s3api head-object --bucket EMPTY-RESTORE-BUCKET --key OBJECT-KEY`
to compare all three `metric-*` metadata fields with the source backup. Do not
reuse production bucket credentials/endpoints without verifying the target.

### 4. Start and verify

For a restore drill, isolate outbound network access before starting Metric:
restored notification queues and monitors may otherwise contact real recipients
or monitored systems. Then start the destination and check it:

```bash
docker compose -p metric-restore up -d --wait --wait-timeout 120
docker compose -p metric-restore logs --tail=100 metric
```

Verify all of the following before declaring the backup usable:

- `/ready` succeeds and the logs contain no schema/startup failures.
- You can sign in with the original account and see the expected projects.
- Representative non-expired events are readable, with their issue information.
- Attachments, Replay segments or debug files you actually use are downloadable;
  compare their bytes/checksums, not just their metadata rows.
- A previously issued DSN, pointed at the isolated destination, accepts a new event.
- Configured integrations still have their original secrets; use isolated test
  endpoints before attempting any real notification delivery.

TTL deadlines are not reset by restoring. Data already past its retention deadline
can disappear as MongoDB rebuilds TTL indexes or runs cleanup. This is not evidence
that `mongorestore` silently dropped unexpired data. `/ready` alone is also not a
backup-integrity check: a missing attachment can return HTTP 503 while Metric stays
ready and the associated event remains readable.

## External MongoDB and other deployment layouts

Use the same `mongodump --db YOUR-METRIC-DATABASE --archive=... --gzip` and
`mongorestore --nsInclude='YOUR-METRIC-DATABASE.*'` approach with Database Tools
installed on the operator's machine or in a temporary tools container. Supply the
actual host, TLS settings, authentication database and credentials for that server.
The source user needs read/list access to the selected database; it does not need
permission to lock the server. Destination permissions must allow the required
collection, document and index creation. Prefer a protected Database Tools
configuration file or interactive password prompt over credentials in argv.

For Helm, stop the Metric workload and use separate restore database/PVC/Secret
targets; do not blindly reuse Compose volume names. Operator-managed storage,
bucket lifecycle, KMS and database snapshots require their provider's instructions.

## Verification evidence

The database-scoped archive, local-volume tar and S3-to-S3 metadata-preserving
copy were exercised with an unchanged Metric binary in Docker. The scoped dump
preserved all 38 Metric collections, their counts, indexes and validators. Restore
tests exercised real accounts, events, attachments and existing DSN ingestion.
The report also records TTL, missing-file and damaged-backup limitations:
[manual backup verification](https://github.com/biosshot/metric/blob/main/arch-docs/verification/2026-09-05-manual-backup-restore.md).
