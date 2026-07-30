#!/bin/sh
set -eu

version="${METRIC_VERSION:-0.1.0}"
install_dir="${METRIC_INSTALL_DIR:-metric}"
download_base="${METRIC_DOWNLOAD_BASE:-https://raw.githubusercontent.com/biosshot/metric/v${version}/deploy}"

if ! command -v docker >/dev/null 2>&1; then
  echo "Docker is required: https://docs.docker.com/get-docker/" >&2
  exit 1
fi

if ! docker compose version >/dev/null 2>&1; then
  echo "Docker Compose is required." >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required." >&2
  exit 1
fi

random_hex() {
  byte_count="$1"
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex "${byte_count}"
  elif command -v od >/dev/null 2>&1; then
    od -An -N "${byte_count}" -tx1 /dev/urandom | tr -d ' \n'
  else
    echo "OpenSSL or od is required to generate secrets." >&2
    exit 1
  fi
}

mkdir -p "${install_dir}"

for file in compose.yml metric.toml symbolicator.yml; do
  if [ ! -f "${install_dir}/${file}" ]; then
    temporary_file="${install_dir}/${file}.tmp"
    rm -f "${temporary_file}"
    curl --fail --location --silent --show-error \
      "${download_base}/${file}" \
      --output "${temporary_file}"
    mv "${temporary_file}" "${install_dir}/${file}"
  fi
done

if [ ! -f "${install_dir}/.env" ]; then
  umask 077
  mongo_password="$(random_hex 24)"
  scrub_hmac_key="$(random_hex 32)"
  {
    echo "METRIC_MONGO_PASSWORD=${mongo_password}"
    echo "METRIC_SCRUB_HMAC_KEY=${scrub_hmac_key}"
    echo "METRIC_HTTP_PORT=4001"
    echo "METRIC_IMAGE=ghcr.io/biosshot/metric:${version}"
    echo "METRIC_SYMBOLICATOR_IMAGE=ghcr.io/getsentry/symbolicator:26.6.0"
  } >"${install_dir}/.env"
fi

(
  cd "${install_dir}"
  docker compose pull
  docker compose up -d --wait --wait-timeout 120
  docker compose ps
)

echo
echo "Metric is ready at http://localhost:4001"
echo "First setup token: cd ${install_dir} && docker compose logs metric"
