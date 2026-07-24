# Operations

## Bare-metal Windows development

Use a locally installed MongoDB and an explicit env file:

```text
MONGODB_URI=mongodb://127.0.0.1:27017/?retryWrites=false
SCRUB_HMAC_KEY=<64 lowercase hexadecimal characters>
```

Validate and start:

```powershell
cargo run -p faultkeep-server -- --config config/faultkeep.example.toml --env-file .env.local --check-config
cargo run -p faultkeep-server -- --config config/faultkeep.example.toml --env-file .env.local
```

Open `http://127.0.0.1:4001/`. `/live` reports process liveness and `/ready` reports
required dependency/worker readiness without exposing configuration or backend
diagnostics.

## Container deployment

The release image runs one non-root `--role all` process with built Vue assets and
local BlobStore data on a dedicated volume. MongoDB remains a separate pinned
service.

```powershell
Copy-Item deploy/release.env.example deploy/release.env
# Replace both placeholder secrets in deploy/release.env.
docker compose --env-file deploy/release.env -f deploy/compose.release.yml config
docker compose --env-file deploy/release.env -f deploy/compose.release.yml up --build -d
```

Terminate with:

```powershell
docker compose --env-file deploy/release.env -f deploy/compose.release.yml down
```

Do not add `-v` unless permanent MongoDB and BlobStore data should be deleted.

## Optional Symbolicator

No Symbolicator image is bundled. Configure a separately operated compatible service
with `APP__SYMBOLICATOR__ENDPOINT` and set an externally reachable
`APP__SYMBOLICATOR__CALLBACK_BASE_URL`. Faultkeep remains ready for ordinary Error
Event ingest when this optional component is degraded; symbolication-specific
capabilities report the failure.

## Monitoring and alerts

Alert on sustained readiness failure, MongoDB/BlobStore errors, local disk reserve,
Dispatcher queue saturation, oldest pending age, Processor lag, retention/archive
lag, upload/GC failures and notification backlog. Metric labels are closed,
low-cardinality values; never attach project, Event, Issue, URL, release, filename or
user values.

Graceful shutdown stops admission, drains bounded writer/finalizer work through the
configured grace period, and leaves durable pending/claimed work retryable after
restart.

## Data safety

Faultkeep does not claim an application-consistent backup/restore command. Backend
snapshots taken independently are not guaranteed to form one consistent restore.
Use backend-native tooling, retain MongoDB and BlobStore together, test restoration
in isolation, and do not describe that procedure as a Faultkeep transactional
backup.

Schema generation 7 supports empty-database bootstrap only. An older generation is
rejected; online migrations and rolling mixed-version upgrades are not implemented.
