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
RUN cargo build --locked --release --bin faultkeep-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system faultkeep \
    && useradd --system --gid faultkeep --home-dir /nonexistent --shell /usr/sbin/nologin faultkeep \
    && mkdir -p /opt/faultkeep/web /var/lib/faultkeep/blobs /etc/faultkeep \
    && chown -R faultkeep:faultkeep /var/lib/faultkeep

COPY --from=rust-builder /source/target/release/faultkeep-server /usr/local/bin/faultkeep-server
COPY --from=web-builder /source/web/dist/ /opt/faultkeep/web/
COPY deploy/faultkeep.container.toml /etc/faultkeep/faultkeep.toml

ENV FAULTKEEP_WEB_DIR=/opt/faultkeep/web
USER faultkeep:faultkeep
WORKDIR /var/lib/faultkeep
EXPOSE 4001
HEALTHCHECK --interval=10s --timeout=2s --start-period=20s --retries=6 \
  CMD ["curl", "--fail", "--silent", "http://127.0.0.1:4001/live"]
ENTRYPOINT ["faultkeep-server"]
CMD ["--config", "/etc/faultkeep/faultkeep.toml"]
