# syntax=docker/dockerfile:1

FROM node:24-bookworm-slim AS web-builder
WORKDIR /source/web
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm \
    set -eu; \
    attempt=1; \
    until npm ci \
        --no-audit \
        --no-fund \
        --prefer-offline \
        --fetch-retries=5 \
        --fetch-retry-mintimeout=20000 \
        --fetch-retry-maxtimeout=120000 \
        --fetch-timeout=600000; do \
      if [ "${attempt}" -ge 3 ]; then \
        echo "npm ci failed after ${attempt} attempts." >&2; \
        exit 1; \
      fi; \
      attempt=$((attempt + 1)); \
      echo "npm registry request failed; retrying (${attempt}/3)." >&2; \
      sleep 10; \
    done
COPY web/ ./
RUN npm run build

FROM rust:1.88-bookworm AS rust-builder
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY tools/ tools/
RUN --mount=type=cache,target=/var/cache/cargo \
    --mount=type=cache,target=/source/target \
    CARGO_HOME=/var/cache/cargo \
    cargo build --locked --release --bin metric-server \
    && cp /source/target/release/metric-server /tmp/metric-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system metric \
    && useradd --system --gid metric --home-dir /nonexistent --shell /usr/sbin/nologin metric \
    && mkdir -p /opt/metric/web /var/lib/metric/blobs /etc/metric \
    && chown -R metric:metric /var/lib/metric

COPY --from=rust-builder /tmp/metric-server /usr/local/bin/metric-server
COPY --from=web-builder /source/web/dist/ /opt/metric/web/
COPY deploy/metric.toml /etc/metric/metric.toml

ENV METRIC_WEB_DIR=/opt/metric/web
USER metric:metric
WORKDIR /var/lib/metric
EXPOSE 4001
HEALTHCHECK --interval=10s --timeout=2s --start-period=20s --retries=6 \
  CMD ["curl", "--fail", "--silent", "http://127.0.0.1:4001/live"]
ENTRYPOINT ["metric-server"]
CMD ["--config", "/etc/metric/metric.toml"]
