# Docker

The supplied Compose file runs Metric and MongoDB. The web interface is already
inside the Metric image.

## Image

The release image is:

```text
ghcr.io/biosshot/metric:0.1.0
```

Use an exact version in production. Avoid changing to an untested image tag.

## Start and stop

```bash
docker compose up -d --wait --wait-timeout 120
docker compose down
```

Run these commands from the directory containing `compose.yml`, `metric.toml` and
`.env`. Stopping the containers does not delete data.

## Data

Compose creates two volumes:

- `mongo-data` stores events, issues, users and settings;
- `blob-data` stores attachments, replays and other files.

Keep both volumes. Do not run `docker compose down -v` unless you intend to delete
the complete installation.

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
