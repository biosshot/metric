# Operations

## Bare-metal Windows development

Use a locally installed MongoDB and an explicit env file:

```text
MONGODB_URI=mongodb://127.0.0.1:27017/?retryWrites=false
SCRUB_HMAC_KEY=<64 lowercase hexadecimal characters>
```

Validate and start:

```powershell
cargo run -p metric-server -- --config config/metric.example.toml --env-file .env.local --check-config
cargo run -p metric-server -- --config config/metric.example.toml --env-file .env.local
```

Open `http://127.0.0.1:4001/`. `/live` reports process liveness and `/ready` reports
required dependency/worker readiness without exposing configuration or backend
diagnostics.

## Container deployment

The [installation guide](getting-started.md) creates a directory containing
`compose.yml`, `metric.toml` and `.env`. Run Compose commands from that directory.
The release image runs one non-root process with the web interface included.
MongoDB runs as a separate pinned container.

If you put an HTTPS proxy in front of Metric, add the proxy address or network to
`server.trusted_proxies`. Configure the proxy to replace `X-Forwarded-For`, not add
to a value received from the internet. Metric ignores these headers from addresses
that are not trusted.

```powershell
docker compose config
docker compose up -d
```

Terminate with:

```powershell
docker compose down
```

Do not add `-v` unless permanent MongoDB and BlobStore data should be deleted.

## Optional Symbolicator

No Symbolicator image is bundled. Configure a separately operated compatible service
with `APP__SYMBOLICATOR__ENDPOINT` and set an externally reachable
`APP__SYMBOLICATOR__CALLBACK_BASE_URL`. Metric remains ready for ordinary Error
Event ingest when this optional component is degraded; symbolication-specific
capabilities report the failure.

## Monitoring and alerts

Metric currently provides JSON logs plus `/live` and `/ready`. Alert when `/ready`
continues to fail and when the container restarts repeatedly. Monitor free disk
space for both Docker volumes.

Metric does not yet expose a Prometheus endpoint. The `/metrics` browser path is an
application page, not a Prometheus scrape target.

During a normal shutdown, Metric finishes active work until the configured timeout.
Work that is not finished remains in the database and is retried after restart.

## Data safety

Metric does not yet include its own backup command. Back up MongoDB and BlobStore
together with their native tools, and test the restore separately. Copies made at
different times may not match each other.

Session Replay makes this pairing mandatory when Replay is enabled: MongoDB owns
the compact manifest while BlobStore owns the immutable recording segments.

## Schema compatibility and upgrades

The current binary requires schema generation **19 exactly**. An empty database may
be bootstrapped at generation 19 and a complete generation-19 database is validated
before startup. Older, newer, incomplete and unmanaged non-empty databases are
rejected.

There is no online or automatic migration, no mixed-generation rolling upgrade, and
no supported data-preserving conversion from generation 18 to 19. A startup
rejection means "stop and preserve the data", not "recreate the database":

- do not change the `schema_meta` marker manually;
- do not drop MongoDB collections or volumes;
- retain the old binary and configuration;
- take a backend-native MongoDB and BlobStore backup together;
- proceed only with an explicit tested procedure for the exact generation
  transition.

The complete decision table and pre-upgrade checklist are in [Schema compatibility
and upgrades](upgrading.md).
