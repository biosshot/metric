# Docker

The supplied Compose file starts:

- Metric;
- MongoDB;
- Sentry Symbolicator;
- a small Symbolicator cache-cleanup process.

The web interface is already inside the Metric image.

## Images

| Service | Image |
| --- | --- |
| Metric | `ghcr.io/biosshot/metric:0.1.0` |
| MongoDB | `mongo:8.0.12` |
| Symbolicator | `ghcr.io/getsentry/symbolicator:26.6.0` |

Use an exact version in production. Avoid changing to an untested image tag.

Symbolicator is an independent third-party image under FSL-1.1-MIT. It is not
covered by Metric's MIT License. See the
[third-party notice](https://github.com/biosshot/metric/blob/main/THIRD_PARTY_NOTICES.md).

## Start and stop

```bash
docker compose up -d --wait --wait-timeout 120
docker compose down
```

Run these commands from the directory containing `compose.yml`, `metric.toml`,
`symbolicator.yml` and `.env`. Stopping the containers does not delete data.

## Data

Compose creates three volumes:

- `mongo-data` stores events, issues, users and settings;
- `blob-data` stores attachments, replays and other files;
- `symbolicator-cache` stores downloaded symbols and generated caches.

Keep `mongo-data` and `blob-data` together. The Symbolicator cache can be rebuilt
and does not need to be backed up. Do not run `docker compose down -v` unless you
intend to delete the complete installation.

The supplied Compose file does not publish the MongoDB port. Do not reuse this
MongoDB container for other applications.

## HTTPS

When Metric is available over the internet, put an HTTPS proxy in front of port
4001. Caddy, Nginx, Traefik and cloud load balancers can all do this.

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

Read [Update Metric](upgrading.md) before changing versions. Change `METRIC_IMAGE`
in `.env` to the exact new version, then:

```bash
docker compose pull metric
docker compose up -d --wait --wait-timeout 120
```

Never delete the MongoDB volume to fix a schema-version error.

Change `METRIC_SYMBOLICATOR_IMAGE` only when the Metric release notes name a
compatible Symbolicator version.
