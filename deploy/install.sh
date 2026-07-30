#!/bin/sh
set -eu

version="${METRIC_VERSION:-0.1.0}"
install_dir="${METRIC_INSTALL_DIR:-metric}"
download_base="${METRIC_DOWNLOAD_BASE:-https://raw.githubusercontent.com/biosshot/metric/v${version}/deploy}"
profile="${METRIC_PROFILE:-medium}"

case "${profile}" in
  min|low|medium|high) ;;
  *)
    echo "METRIC_PROFILE must be min, low, medium or high." >&2
    exit 1
    ;;
esac

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

for file in compose.yml symbolicator.yml; do
  if [ ! -f "${install_dir}/${file}" ]; then
    temporary_file="${install_dir}/${file}.tmp"
    rm -f "${temporary_file}"
    curl --fail --location --silent --show-error \
      "${download_base}/${file}" \
      --output "${temporary_file}"
    mv "${temporary_file}" "${install_dir}/${file}"
  fi
done

if [ ! -f "${install_dir}/metric.toml" ]; then
  temporary_file="${install_dir}/metric.toml.tmp"
  rm -f "${temporary_file}"
  curl --fail --location --silent --show-error \
    "${download_base}/profiles/${profile}.toml" \
    --output "${temporary_file}"
  mv "${temporary_file}" "${install_dir}/metric.toml"
fi

if [ ! -f "${install_dir}/.env" ]; then
  umask 077
  mongo_password="$(random_hex 24)"
  scrub_hmac_key="$(random_hex 32)"
  template_file="${install_dir}/.env.template.tmp"
  generated_file="${install_dir}/.env.tmp"
  rm -f "${template_file}" "${generated_file}"
  curl --fail --location --silent --show-error \
    "${download_base}/profiles/${profile}.env.example" \
    --output "${template_file}"
  sed \
    -e "s/replace-with-a-long-url-safe-random-password/${mongo_password}/" \
    -e "s/replace-with-64-lowercase-hex-characters/${scrub_hmac_key}/" \
    -e "s|ghcr.io/biosshot/metric:0.1.0|ghcr.io/biosshot/metric:${version}|" \
    "${template_file}" >"${generated_file}"
  mv "${generated_file}" "${install_dir}/.env"
  rm -f "${template_file}"
fi

active_profile="$(
  sed -n 's/^METRIC_PROFILE=//p' "${install_dir}/.env" | head -n 1
)"
if [ -z "${active_profile}" ]; then
  active_profile="unknown"
elif [ "${active_profile}" != "${profile}" ]; then
  echo "Existing installation keeps profile ${active_profile}; requested ${profile} was not applied." >&2
fi

(
  cd "${install_dir}"
  docker compose pull
  docker compose up -d --wait --wait-timeout 120
  docker compose ps
)

echo
echo "Profile: ${active_profile}"
echo "Metric is ready at http://localhost:4001"
echo "First setup token: cd ${install_dir} && docker compose logs metric"
