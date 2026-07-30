FROM node:24-bookworm-slim AS web-builder
WORKDIR /source/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1.88-bookworm AS rust-builder
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY tools/ tools/
RUN cargo build --locked --release --bin metric-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system metric \
    && useradd --system --gid metric --home-dir /nonexistent --shell /usr/sbin/nologin metric \
    && mkdir -p /opt/metric/web /var/lib/metric/blobs /etc/metric \
    && chown -R metric:metric /var/lib/metric

COPY --from=rust-builder /source/target/release/metric-server /usr/local/bin/metric-server
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
