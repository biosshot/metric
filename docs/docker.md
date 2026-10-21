# Docker

Metric uses one installed `compose.yml` for every resource profile. The installer
selects bundled MongoDB, bundled PostgreSQL or an existing database. The selected `.env`
controls container memory, MongoDB cache, log rotation and whether Symbolicator
is active. The selected `metric.toml` controls application limits and retention.
BlobStore receives about one third of the recommended disk for each profile; see
[Capacity and profiles](capacity.md).

## Containers by profile

| Container | Min | Low | Medium | High |
| --- | --- | --- | --- | --- |
| Metric | Yes | Yes | Yes | Yes |
| Bundled database (MongoDB or PostgreSQL) | If selected | If selected | If selected | If selected |
| Symbolicator | No | No | Yes | Yes |
| Symbolicator cache cleanup | No | No | Yes | Yes |

The web interface is included in the Metric image.

## Images

| Service | Image |
| --- | --- |
| Metric | `ghcr.io/biosshot/metric:0.1.9` |
| MongoDB | `mongo:8.0.12` |
| PostgreSQL | `postgres:18.6-bookworm`, pinned by digest in the generated template |
| Symbolicator | `ghcr.io/getsentry/symbolicator:26.6.0` |

Use exact image versions. Symbolicator is an independent third-party image under
FSL-1.1-MIT and is used only by Medium and High. See the
[third-party notice](https://github.com/biosshot/metric/blob/main/THIRD_PARTY_NOTICES.md).

## Start and stop

```bash
docker compose up -d --wait --wait-timeout 120
docker compose down
```

Run these commands from the directory containing `compose.yml`, `metric.toml`,
`symbolicator.yml` and `.env`. Do not pass a profile flag manually: the installer
has already stored the correct `COMPOSE_PROFILES` value in `.env`.

Keep generated `connection.env` beside these files. External deployments start
only Metric and the profile's optional Symbolicator; the installer does not operate
the external database server. Supplied certificates are copied under `certs/` and
mounted as individual read-only files into Metric. The host certificate directory
is private; the unprivileged app can read only the files explicitly mounted there.

The selected HTTP port is published on all host interfaces by default. Restrict
it in Compose if needed, for example `127.0.0.1:4001:4001`. This differs from the
app's `server.http_address`, which refers to its container network namespace.

Check the selected profile:

```bash
grep '^METRIC_PROFILE=' .env
```

On PowerShell:

```powershell
Select-String '^METRIC_PROFILE=' .env
```

## Resource limits

| Setting in `.env` | Meaning |
| --- | --- |
| `METRIC_PROFILE` | Name recorded for the operator. |
| `COMPOSE_PROFILES` | Starts Symbolicator for Medium and High. |
| `METRIC_MONGO_CACHE_GB` | MongoDB WiredTiger cache. |
| `METRIC_MONGO_MEMORY_LIMIT` | MongoDB container memory ceiling. |
| `METRIC_POSTGRES_MEMORY_LIMIT` | Bundled PostgreSQL container memory ceiling; a starting limit, not a benchmark guarantee. |
| `METRIC_APP_MEMORY_LIMIT` | Metric container memory ceiling. |
| `METRIC_SYMBOLICATOR_MEMORY_LIMIT` | Symbolicator memory ceiling when active. |
| `METRIC_LOG_MAX_SIZE` | Size of one rotated container log. |
| `METRIC_LOG_MAX_FILES` | Number of container log files retained. |

If a container reaches its memory ceiling, Docker may restart it. Do not increase
one limit beyond the server size without considering MongoDB, Metric, the
operating system and optional Symbolicator together.

## Data

Compose defines three volumes:

- `mongo-data` stores events, issues, users and settings;
- `blob-data` stores attachments, replays and other files;
- `symbolicator-cache` stores rebuildable caches for Medium and High.

Min and Low do not create or use the Symbolicator cache during a normal start.
Keep `mongo-data` and `blob-data` together. Do not run
`docker compose down -v` unless you intend to delete the complete installation.

The supplied Compose file does not publish the MongoDB or Symbolicator ports.
Do not reuse its MongoDB container for other applications.

## HTTPS

The supplied profiles work over HTTP, including sign-in from another machine.
HTTP does not encrypt passwords, session cookies or application data. For a
public or long-running installation, put an HTTPS proxy in front of port 4001.
Caddy, Nginx, Traefik and cloud load balancers can all do this.

The public DSN must use the same HTTPS hostname that your applications can reach.
You do not need another Compose file. If the proxy sends forwarding headers, set
`server.trusted_proxies` in `metric.toml` as described in
[Running Metric](operations.md#https-proxy).

## Health checks

```bash
curl http://localhost:4001/live
curl http://localhost:4001/ready
```

`/live` confirms that the process is running. `/ready` confirms that Metric and
its required workers can serve requests.

## Update the image

Read [Update Metric](upgrading.md) before changing versions. Change
`METRIC_IMAGE` in `.env` to the exact new version, then:

```bash
docker compose pull
docker compose up -d --wait --wait-timeout 120
```

Never delete the MongoDB volume to fix a schema-version error. Change
`METRIC_SYMBOLICATOR_IMAGE` only when Metric release notes name a compatible
version.
