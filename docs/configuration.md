# Configuration

The Docker setup is ready to use without changing `metric.toml`. The installer
chooses one tested file from `deploy/profiles/` and saves it as `metric.toml`.
Most installations only need the values already created in `.env`:

- `METRIC_PROFILE`;
- `COMPOSE_PROFILES`;
- `METRIC_MONGO_PASSWORD`;
- `METRIC_SCRUB_HMAC_KEY`;
- `METRIC_HTTP_PORT`;
- `METRIC_IMAGE`;
- `METRIC_SYMBOLICATOR_IMAGE`;
- `METRIC_MONGO_CACHE_GB`;
- `METRIC_MONGO_MEMORY_LIMIT`;
- `METRIC_APP_MEMORY_LIMIT`;
- `METRIC_SYMBOLICATOR_MEMORY_LIMIT`;
- `METRIC_CLEANUP_MEMORY_LIMIT`;
- `METRIC_LOG_MAX_SIZE`;
- `METRIC_LOG_MAX_FILES`.

Advanced settings live in `metric.toml` beside `compose.yml`. Metric reads them
once at startup, so restart the container after a change.

## Supplied profiles

| Profile file | Main purpose |
| --- | --- |
| [`min.toml`](https://github.com/biosshot/metric/blob/v0.1.0/deploy/profiles/min.toml) | Minimum memory and disk use; no Symbolicator or attachments. |
| [`low.toml`](https://github.com/biosshot/metric/blob/v0.1.0/deploy/profiles/low.toml) | Small installation with attachments but no Symbolicator. |
| [`medium.toml`](https://github.com/biosshot/metric/blob/v0.1.0/deploy/profiles/medium.toml) | Recommended default with Symbolicator. |
| [`high.toml`](https://github.com/biosshot/metric/blob/v0.1.0/deploy/profiles/high.toml) | Larger queues, files, storage and retention. |

Profile settings are designed as a group. Copying only High queue sizes into Min
can make a 1 GiB server run out of memory. Copying only High retention into a
smaller profile can fill its disk.

Minidumps and cold archive stay disabled in all supplied profiles. Session Replay
still requires an explicit per-project choice.

## Check changes before starting

```bash
docker compose run --rm --no-deps metric \
  --config /etc/metric/metric.toml \
  --check-config
```

Unknown names and invalid values stop startup instead of being ignored.

The current Metric version requires MongoDB schema generation **19 exactly**. An
empty database is prepared automatically. A database created by another schema
generation is rejected; follow [Update Metric](upgrading.md) and do not delete data
to bypass this check.

## How values are selected

From highest to lowest priority:

```text
command line → APP__ environment variable → TOML file → built-in default
```

For example, the environment variable `APP__SERVER__REQUEST_TIMEOUT=45s`
overrides `server.request_timeout` from TOML.

Environment files are loaded only when passed with `--env-file`. Existing process
environment variables take priority over values in that file.

Docker Compose uses `.env` to fill values referenced by `compose.yml`. It does
not pass every value from `.env` into the Metric container. For Docker, the
simplest option is to edit `metric.toml`. If you use an `APP__...` override, also
add that variable under `services.metric.environment` in `compose.yml`.

## Secrets

Do not write passwords or keys directly in TOML. Point to an environment variable
or a file:

```toml
[mongodb]
uri = { env = "MONGODB_URI" }

[projects]
scrub_hmac_key = { file = "/run/secrets/scrub-hmac-key" }
```

`SCRUB_HMAC_KEY` must contain exactly 32 random bytes written as 64 lowercase
hexadecimal characters. Changing it changes IP-address pseudonyms, so keep the
same value while the installation contains data.

## Value formats

Durations accept values such as `250ms`, `30s`, `15m`, `24h` and `30d`. Sizes
accept values such as `64 KiB`, `20 MiB` and `1 GiB`.

The tables below list every setting and its standalone default. Docker profiles
override many of these values; use the selected `metric.toml` as the exact source
for that installation. The main profile differences are summarized in
[Capacity and profiles](capacity.md).

## Server and database

| Setting | Value | Meaning |
| --- | --- | --- |
| `role` | `all` | Runs the complete application. This is the only supported role. |
| `server.http_address` | `127.0.0.1:4001` | Address and port used by the binary. The container uses `0.0.0.0:4001`. |
| `server.shutdown_grace` | `10s` (Docker: `30s`) | Time allowed for work to finish during shutdown. |
| `server.trusted_proxies` | `[]` | Proxy IP addresses or networks allowed to supply forwarding headers. |
| `server.max_active_requests` | `512` | Maximum requests handled at the same time. Profiles use 16, 64, 256 or 1024. |
| `server.request_timeout` | `30s` | General HTTP request deadline. |
| `mongodb.uri` | `MONGODB_URI` | MongoDB connection string. Keep it secret. |
| `mongodb.database` | `metric` | MongoDB database name. |
| `mongodb.bootstrap_timeout` | `10s` (Docker: `30s`) | Time allowed for database startup checks. |
| `projects.scrub_hmac_key` | `SCRUB_HMAC_KEY` | Secret used to pseudonymize stored values such as IP addresses. |
| `projects.identity_collision_retries` | `16` | Attempts to create a unique project identifier. |
| `projects.max_keys_per_project` | `32` | Maximum DSN keys for one project. |

## Development safety

Do not enable these settings on an internet-facing installation.

| Setting | Value | Meaning |
| --- | --- | --- |
| `development.allow_literal_secrets` | `false` | Allows secrets to be written directly in TOML. |
| `development.allow_insecure_cookies` | `false` | Allows login cookies without HTTPS. |

## File and S3 storage

| Setting | Value | Meaning |
| --- | --- | --- |
| `blob.backend` | `local` | Storage type: `local` or `s3`. |
| `blob.root` | `./metric-data/blobs` (Docker: `/var/lib/metric/blobs`) | Local storage directory. |
| `blob.capacity` | `1 GiB` | Maximum space Metric may use in local storage. Profiles use 256 MiB, 2 GiB, 10 GiB or 50 GiB. |
| `blob.reserve` | `128 MiB` | Space kept free before new objects are rejected. |
| `blob.max_object_bytes` | `100 MiB` | Maximum size of one stored object. |
| `blob.s3.endpoint` | unset | Custom S3-compatible endpoint. Leave unset for AWS S3. |
| `blob.s3.region` | `us-east-1` | S3 region. |
| `blob.s3.bucket` | `metric` | S3 bucket name. |
| `blob.s3.access_key_id` | unset | S3 access-key reference. |
| `blob.s3.secret_access_key` | unset | S3 secret-key reference. |
| `blob.s3.session_token` | unset | Optional temporary S3 session-token reference. |
| `blob.s3.force_path_style` | `true` | Uses path-style bucket URLs for compatible storage services. |
| `blob.s3.part_bytes` | `8 MiB` | Size of each multipart-upload part. |

## Cold archive

Cold archive is disabled by default.

| Setting | Value | Meaning |
| --- | --- | --- |
| `archive.enabled` | `false` | Writes old errors, logs and spans to cold storage. |
| `archive.maximum_events` | `500` | Maximum records in one archive batch. |
| `archive.target_uncompressed_bytes` | `64 MiB` | Target batch size before compression. |
| `archive.write_chunk_bytes` | `256 KiB` | Streaming write chunk size. |
| `archive.poll_interval` | `30s` | How often Metric looks for archive work. |
| `archive.hot_copy_delay` | `0s` | Time data remains only in hot storage before archive work starts. |
| `archive.orphan_grace` | `24h` | Age required before an unused archive object can be deleted. |
| `archive.cleanup_max_pages` | `4` | Maximum cleanup pages handled in one run. |

## Native crashes and symbolication

| Setting | Value | Meaning |
| --- | --- | --- |
| `native_crash.minidump.enabled` | `false` | Accepts minidumps. They may contain raw process memory. |
| `native_crash.minidump.max_bytes` | `100 MiB` | Maximum minidump size. |
| `native_crash.minidump.chunk_bytes` | `64 KiB` | Streaming read chunk size. |
| `symbolicator.endpoint` | unset (Medium/High: `http://symbolicator:3021/symbolicate`) | Symbolicator API address. Min and Low leave it unset. |
| `symbolicator.callback_base_url` | `http://127.0.0.1:4001/` (Docker: `http://metric:4001/`) | Metric address that Symbolicator can call. Change it to a reachable address when Symbolicator runs outside the Compose network. |
| `symbolicator.request_timeout` | `20s` | Symbolicator request deadline. |
| `symbolicator.maximum_concurrency` | `8` | Maximum Symbolicator requests at the same time. |
| `symbolicator.circuit_failure_threshold` | `5` | Consecutive failures before requests pause. |
| `symbolicator.circuit_cooldown` | `30s` | Pause after the failure threshold is reached. |
| `symbolicator.maximum_response_bytes` | `4 MiB` | Maximum accepted Symbolicator response size. |

Medium and High also read `symbolicator.yml` beside `compose.yml`. This file
configures Symbolicator's internal server, cache and logging. Min and Low keep
the file for easy upgrades but do not start its container.

## Debug files and artifact bundles

These defaults are used when an `[artifacts]` section is not present.

| Setting | Value | Meaning |
| --- | --- | --- |
| `artifacts.maximum_bundle_bytes` | `64 MiB` | Maximum compressed bundle size. |
| `artifacts.maximum_logical_bytes` | `512 MiB` | Maximum total size after extraction. |
| `artifacts.maximum_entries` | `10000` | Maximum files in one bundle. |
| `artifacts.maximum_entry_bytes` | `16 MiB` | Maximum extracted size of one file. |
| `artifacts.maximum_concurrent_assemblies` | `2` | Maximum bundles assembled at the same time. |
| `artifacts.parse_timeout` | `30s` | Time allowed to inspect a bundle. |
| `artifacts.orphan_grace` | `24h` | Age required before unused upload data can be deleted. |
| `artifacts.claim_lease` | `5m` | Time one worker owns an assembly job. |
| `artifacts.blob_operation_timeout` | `30s` | Storage-operation deadline. |
| `artifacts.tombstone_retention` | `24h` | Time deletion markers are retained. |
| `artifacts.gc_interval` | `15m` | How often unused artifact data is cleaned. |
| `artifacts.gc_batch_size` | `100` | Objects inspected in one cleanup batch. |
| `artifacts.gc_max_concurrency` | `4` | Maximum cleanup operations at the same time. |
| `artifacts.maximum_bytes_per_organization` | `0 B` | Organization byte quota. Zero means unlimited. |
| `artifacts.maximum_bundles_per_organization` | `0` | Organization bundle quota. Zero means unlimited. |

## Incident Capsule export

| Setting | Value | Meaning |
| --- | --- | --- |
| `incident_capsule.max_events` | `10` | Maximum events included in one export. |
| `incident_capsule.max_activities` | `100` | Maximum issue activities included. |
| `incident_capsule.max_total_uncompressed_bytes` | `100 MiB` | Maximum total export size before compression. |
| `incident_capsule.max_entry_bytes` | `16 MiB` | Maximum size of one export entry. |
| `incident_capsule.generation_timeout` | `30s` | Time allowed to prepare an export. |
| `incident_capsule.max_concurrency` | `4` | Maximum exports prepared at the same time. |
| `incident_capsule.stream_chunk_bytes` | `64 KiB` | Download streaming chunk size. |
| `incident_capsule.stream_buffer_chunks` | `4` | Number of chunks buffered during download. |

## Incoming SDK data

| Setting | Value | Meaning |
| --- | --- | --- |
| `ingest.max_compressed_request_bytes` | `20 MiB` | Maximum compressed request size. |
| `ingest.max_decompressed_request_bytes` | `100 MiB` | Maximum size after decompression. |
| `ingest.max_event_bytes` | `1 MiB` | Maximum event body size. |
| `ingest.max_envelope_items` | `100` | Maximum items in one Sentry envelope. |
| `ingest.max_active_requests` | `512` | Maximum ingest requests handled at the same time. |
| `ingest.max_parsing_tasks` | `0` | Parsing-task limit. Zero selects it automatically. |
| `ingest.max_waiting_for_storage` | `512` | Maximum requests waiting for storage. |
| `ingest.request_timeout` | `10s` | Ingest request deadline. |
| `ingest.unsupported_backoff_seconds` | `3600` | Retry delay returned for unsupported data. |

### Attachments

| Setting | Value | Meaning |
| --- | --- | --- |
| `ingest.attachments.enabled` | `true` | Accepts safe attachment types. |
| `ingest.attachments.max_count` | `10` | Maximum attachments in one event. |
| `ingest.attachments.max_item_bytes` | `1 MiB` | Maximum size of one attachment. |
| `ingest.attachments.max_total_bytes` | `5 MiB` | Maximum combined attachment size. |
| `ingest.attachments.chunk_bytes` | `64 KiB` | Streaming chunk size. |
| `ingest.attachments.orphan_grace` | `24h` | Age required before unused attachment data can be deleted. |
| `ingest.attachments.cleanup_interval` | `15m` | How often unused attachments are cleaned. |
| `ingest.attachments.cleanup_batch_size` | `256` | Attachments inspected in one cleanup page. |
| `ingest.attachments.cleanup_max_pages` | `16` | Maximum cleanup pages in one run. |

### Session Replay

| Setting | Value | Meaning |
| --- | --- | --- |
| `ingest.replay.max_segment_bytes` | `5 MiB` | Maximum compressed replay segment size. |
| `ingest.replay.max_decompressed_segment_bytes` | `20 MiB` | Maximum segment size after decompression. |
| `ingest.replay.max_events_per_segment` | `100000` | Maximum replay records in one segment. |
| `ingest.replay.queue_capacity` | `32` | Maximum queued replay segments. |
| `ingest.replay.max_queued_bytes` | `32 MiB` | Maximum total replay data waiting in memory. |
| `ingest.replay.orphan_grace` | `1h` | Age required before unused replay data can be deleted. |
| `ingest.replay.cleanup_interval` | `5m` | How often unused replay data is cleaned. |
| `ingest.replay.cleanup_batch_size` | `100` | Replay objects inspected in one cleanup batch. |

### Cache, batching and backlog

| Setting | Value | Meaning |
| --- | --- | --- |
| `ingest.project_cache.capacity` | `100000` | Maximum cached project entries. |
| `ingest.project_cache.max_inflight` | `512` | Maximum project lookups at the same time. |
| `ingest.project_cache.positive_ttl` | `60s` | Cache time for a project that exists. |
| `ingest.project_cache.negative_ttl` | `5s` | Cache time for a project that was not found. |
| `ingest.batch.max_wait` | `20ms` | Maximum wait before a partial storage batch is written. |
| `ingest.batch.max_documents` | `250` | Maximum documents in one storage batch. |
| `ingest.batch.max_bytes` | `8 MiB` | Maximum estimated batch size. |
| `ingest.event_codec.compression_level` | `3` | Compression level for stored event bodies. |
| `ingest.event_codec.compression_min_savings` | `64` | Minimum saved bytes required to keep compression. |
| `ingest.backlog.max_pending_events` | `1000000` | Pending-event level where ingest protection activates. |
| `ingest.backlog.max_oldest_pending_age` | `1h` | Maximum acceptable age of the oldest pending event. |

## Background processing

| Setting | Value | Meaning |
| --- | --- | --- |
| `dispatcher.queue_capacity` | `4096` | In-memory processing queue size. Profiles use 128, 512, 2048 or 8192. |
| `dispatcher.worker_concurrency` | `32` | Dispatcher workers running at the same time. Profiles use 1, 4, 16 or 64. |
| `dispatcher.low_watermark` | `1024` | Queue level that triggers a refill. |
| `dispatcher.refill_target` | `3072` | Queue level targeted by a refill. |
| `dispatcher.refill_batch_size` | `512` | Maximum records fetched per refill. |
| `dispatcher.poll_interval` | `100ms` | Delay between empty-queue checks. |
| `dispatcher.metrics_interval` | `5s` | Interval for internal dispatcher measurements. |
| `dispatcher.source_timeout` | `5s` | Deadline for reading pending work. |
| `scheduler.poll_interval` | `1s` | How often scheduled work is checked. |
| `scheduler.maintenance_interval` | `1m` | General maintenance interval. |
| `scheduler.reconciliation_interval` | `5m` | Interval for checking unfinished state. |
| `scheduler.backlog_interval` | `5s` | Interval for checking processing backlog. |
| `scheduler.task_timeout` | `10s` | Deadline for one scheduler task. |
| `scheduler.retry_base` | `1s` | First scheduler retry delay. |
| `scheduler.retry_max` | `1m` | Maximum scheduler retry delay. |
| `scheduler.batch_size` | `500` | Maximum records handled in one scheduler batch. |
| `processor.max_concurrency` | `32` | Events processed at the same time. Profiles use 1, 4, 16 or 64. |
| `processor.max_attempts` | `5` | Maximum attempts before processing stops retrying. |
| `processor.retry_base` | `1s` | First processing retry delay. |
| `processor.retry_max` | `5m` | Maximum processing retry delay. |
| `processor.stage_timeout` | `15s` | Deadline for one processing stage. |
| `processor.total_timeout` | `1m` | Total deadline for one processing attempt. |
| `processor.state_timeout` | `5s` | Deadline for reading or updating processing state. |

## Data retention

| Setting | Value | Meaning |
| --- | --- | --- |
| `retention.events_days` | `30` | Days to keep error events. |
| `retention.feedback_days` | `90` | Days to keep user feedback. |
| `retention.issue_stats_hourly_days` | `400` | Days to keep hourly issue counts. |
| `retention.logs_days` | `30` | Days to keep structured logs. |
| `retention.spans_days` | `30` | Days to keep spans. |
| `retention.span_stats_hourly_days` | `90` | Days to keep hourly span statistics. |
| `retention.sessions_days` | `7` | Days to keep individual release sessions. |
| `retention.session_stats_hourly_days` | `400` | Days to keep hourly session statistics. |
| `retention.session_active_max_hours` | `24` | Maximum time a session may remain active. |
| `retention.monitor_runs_days` | `90` | Days to keep monitor runs. |
| `retention.metrics_days` | `90` | Days to keep application metric buckets. |
| `retention.metric_max_series_per_project` | `10000` | Maximum metric series per project. |
| `retention.metric_archive` | `false` | Archives application metrics when archive storage is enabled. |
| `retention.replays_days` | `30` | Days to keep Session Replays. |
| `retention.replay_archive` | `false` | Archives replay data when archive storage is enabled. |

## Project deletion

| Setting | Value | Meaning |
| --- | --- | --- |
| `project_deletion.grace_period` | `24h` | Delay during which project deletion can be cancelled. |
| `project_deletion.delete_batch_documents` | `5000` | Maximum records deleted in one batch. |
| `project_deletion.completed_job_retention` | `30d` | Time completed deletion records are kept. |
| `project_deletion.slug_reservation` | `30d` | Time a deleted project slug remains unavailable. |
| `project_deletion.poll_interval` | `1s` | How often deletion work is checked. |
| `project_deletion.operation_timeout` | `10s` | Deadline for one deletion operation. |
| `project_deletion.drain_timeout` | `10s` | Time allowed for active project work to stop. |
| `project_deletion.retry_base` | `1s` | First deletion retry delay. |
| `project_deletion.retry_max` | `1m` | Maximum deletion retry delay. |

## Authentication

| Setting | Value | Meaning |
| --- | --- | --- |
| `auth.identity_collision_retries` | `16` | Attempts to create a unique identity value. |
| `auth.store_timeout` | `5s` | Deadline for an authentication storage operation. |
| `auth.setup_token_timeout` | `24h` | Lifetime of setup and invitation tokens. |
| `auth.max_api_token_lifetime` | `365d` | Longest allowed personal API-token lifetime. |
| `auth.activity_touch_interval` | `5m` | Minimum interval between user activity updates. |
| `auth.secure_cookie` | `true` | Sends login cookies only over HTTPS. |
| `auth.session.idle_timeout` | `7d` | Signs out a session after this inactive period. |
| `auth.session.absolute_timeout` | `30d` | Maximum session lifetime, even when active. |
| `auth.password.memory_kib` | `19456` | Memory used when hashing one password. |
| `auth.password.iterations` | `2` | Password-hashing work passes. |
| `auth.password.parallelism` | `1` | Password-hashing parallel lanes. |
| `auth.password.max_concurrency` | `2` | Password hashes calculated at the same time. |
| `auth.login.max_attempts` | `5` | Failed sign-in attempts allowed in one window. |
| `auth.login.window` | `1m` | Sign-in rate-limit window. |
| `auth.login.capacity` | `10000` | Maximum sign-in rate-limit entries kept in memory. |

## Notifications

| Setting | Value | Meaning |
| --- | --- | --- |
| `notifications.transition_batch_size` | `100` | Alert changes expanded in one batch. |
| `notifications.due_scan_limit` | `100` | Due deliveries loaded in one scan. |
| `notifications.poll_interval` | `250ms` | How often queued notifications are checked. |
| `notifications.queue.capacity` | `1000` | In-memory notification queue size. |
| `notifications.queue.worker_concurrency` | `8` | Notification workers running at the same time. |
| `notifications.retry.max_attempts` | `8` | Maximum delivery attempts. |
| `notifications.retry.initial_delay` | `5s` | Delay before the first retry. |
| `notifications.retry.max_delay` | `1h` | Maximum retry delay. |
| `notifications.retry.timeout` | `10s` | Deadline for one delivery attempt. |
| `notifications.retry.attempt_lease` | `30s` | Time one worker owns a delivery attempt. |
| `notifications.retention.delivered_days` | `30` | Days to keep successful delivery records. |
| `notifications.retention.dead_days` | `90` | Days to keep permanently failed delivery records. |
| `notifications.webhook.maximum_response_bytes` | `64 KiB` | Maximum webhook response body read by Metric. |
| `notifications.webhook.maximum_retry_after` | `1h` | Maximum server-requested retry delay. |
| `notifications.webhook.allow_http` | `false` | Allows unencrypted HTTP webhook targets. |
| `notifications.webhook.allow_private_networks` | `false` | Allows webhook targets on private network addresses. |

## Print the effective configuration

For a bare binary:

```bash
metric-server \
  --config config/metric.example.toml \
  --env-file .env.local \
  --print-effective-config
```

Secrets are redacted. This command is useful when an environment override does
not behave as expected.
