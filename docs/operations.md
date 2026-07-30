# Running Metric

The [installer](getting-started.md) creates a directory containing
`compose.yml`, `metric.toml` and `.env`. Run all commands on this page from that
directory.

## Start and stop

Start Metric and wait until it is healthy:

```bash
docker compose up -d --wait --wait-timeout 120
```

Stop Metric without deleting data:

```bash
docker compose down
```

Do not add `-v`. It deletes the MongoDB and file-storage volumes.

## Logs and status

```bash
docker compose ps
docker compose logs -f metric
```

Metric writes JSON logs. Passwords, tokens, DSNs and private event data should
still be removed before sharing logs.

Health endpoints:

```bash
curl http://localhost:4001/live
curl http://localhost:4001/ready
```

`/live` confirms that the process is running. `/ready` confirms that Metric and
its required workers are ready.

Metric does not yet provide a Prometheus endpoint. The `/metrics` browser path is
a page in the web interface, not a monitoring endpoint.

For a long-running installation, monitor `/ready`, repeated container restarts
and free disk space in both Docker volumes.

## HTTPS proxy

Use an HTTPS proxy when Metric is opened from another machine. You do not need a
second Compose file.

If the proxy sends `X-Forwarded-For`, add the proxy address or network to
`server.trusted_proxies` in `metric.toml`. Configure the proxy to replace any
`X-Forwarded-For` value received from the internet.

Restart Metric after changing the configuration:

```bash
docker compose up -d --wait --wait-timeout 120
```

## Backups

Metric does not yet include its own backup command. MongoDB and file storage
contain different parts of the same data, so back them up together and test the
restore on a separate installation.

Session Replay especially depends on both: MongoDB stores the replay description
and file storage contains the recording segments.

Stop both containers while taking volume snapshots so that no new application
data is written:

```bash
docker compose stop
# Copy or snapshot both Docker volumes with your host or backup tool.
docker compose up -d --wait --wait-timeout 120
```

The volumes are named `metric_mongo-data` and `metric_blob-data` in the supplied
Compose setup. The exact copy and restore command depends on your Docker host or
storage provider.

## Optional Symbolicator

Metric does not include a Symbolicator container. If you operate a compatible
Symbolicator separately, set its addresses in `metric.toml`:

```toml
[symbolicator]
endpoint = "http://symbolicator.example:3021/symbolicate"
callback_base_url = "https://metric.example.com/"
```

Ordinary error events continue to work when this optional service is unavailable.

## Updates

The current binary requires schema generation **19 exactly**. It rejects a
database created by an incompatible Metric version instead of changing or
deleting it.

Do not edit `schema_meta`, delete MongoDB collections or remove Docker volumes
after a schema error. Follow [Update Metric](upgrading.md).
