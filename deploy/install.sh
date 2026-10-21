#!/bin/sh
set -eu

# Parse the complete downloaded script before any child process can read stdin.
# This also protects curl | sh when native Windows tools are reached through WSL.
main() {
case "$-" in *x*) set +x ;; esac
fail() { printf '%s\n' "$*" >&2; exit 1; }
help() {
  cat <<'HELP'
Metric installer (0.1.9 source; new options require this updated script)
Without options: unattended Medium profile with bundled MongoDB.
  --interactive                    METRIC_INTERACTIVE=1
  --profile min|low|medium|high     METRIC_PROFILE
  --backend mongodb|postgres       METRIC_DATABASE_BACKEND
  --database bundled|external      METRIC_DATABASE_MODE
  --install-dir PATH               METRIC_INSTALL_DIR
  --http-port PORT                 METRIC_HTTP_PORT
  --tls on|off                     METRIC_DATABASE_TLS (external only)
  --tls-ca PATH                    METRIC_DATABASE_TLS_CA
  --tls-client-cert PATH           METRIC_DATABASE_TLS_CLIENT_CERT
  --tls-client-key PATH            METRIC_DATABASE_TLS_CLIENT_KEY
  --help
Also: METRIC_VERSION, METRIC_DOWNLOAD_BASE, METRIC_IMAGE.
External credentials: METRIC_POSTGRES_URI or MONGODB_URI (environment only).
Mongo database: METRIC_MONGO_DATABASE, otherwise URI database or metric.
URI input is visible. Existing configuration and secrets are retained.
HELP
}
profile=${METRIC_PROFILE:-medium}; backend=${METRIC_DATABASE_BACKEND:-}
mode=${METRIC_DATABASE_MODE:-}; interactive=${METRIC_INTERACTIVE:-0}
http_port=${METRIC_HTTP_PORT:-4001}; tls=${METRIC_DATABASE_TLS:-}
ca=${METRIC_DATABASE_TLS_CA:-}; cert=${METRIC_DATABASE_TLS_CLIENT_CERT:-}
key=${METRIC_DATABASE_TLS_CLIENT_KEY:-}; install_dir=${METRIC_INSTALL_DIR:-}
version=${METRIC_VERSION:-0.1.9}
download_base=${METRIC_DOWNLOAD_BASE:-https://raw.githubusercontent.com/biosshot/metric/v${version}/deploy}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --help|-h) help; exit 0 ;;
    --interactive) interactive=1; shift; continue ;;
    --profile|--backend|--database|--install-dir|--http-port|--tls|--tls-ca|--tls-client-cert|--tls-client-key)
      [ "$#" -ge 2 ] && [ -n "$2" ] || fail "Missing value for $1. Use --help."
      case "$1" in
        --profile) profile=$2 ;; --backend) backend=$2 ;; --database) mode=$2 ;;
        --install-dir) install_dir=$2 ;; --http-port) http_port=$2 ;; --tls) tls=$2 ;;
        --tls-ca) ca=$2 ;; --tls-client-cert) cert=$2 ;; --tls-client-key) key=$2 ;;
      esac
      shift; shift ;;
    *) fail 'Unknown installer option. Use --help.' ;;
  esac
done
if [ -z "$install_dir" ]; then
  if [ -f ./compose.yml ] && { [ -f ./metric.toml ] || [ -f ./.env ]; }; then
    install_dir="."
  else install_dir=metric; fi
fi
env_value() {
  awk -v name="$2" 'index($0,name "=")==1 {v=substr($0,length(name)+2); if(v ~ /^\047.*\047$/)v=substr(v,2,length(v)-2); print v; exit}' "$1"
}
existing=0
if [ -f "$install_dir/.env" ]; then
  existing=1
  saved_backend=$(env_value "$install_dir/.env" METRIC_DATABASE_BACKEND)
  saved_mode=$(env_value "$install_dir/.env" METRIC_DATABASE_MODE)
  saved_backend=${saved_backend:-mongodb}; saved_mode=${saved_mode:-bundled}
  [ -z "$backend" ] || [ "$backend" = "$saved_backend" ] || fail 'Existing installation keeps its backend. Switching databases is not a migration.'
  [ -z "$mode" ] || [ "$mode" = "$saved_mode" ] || fail 'Existing installation keeps its database placement.'
  backend=$saved_backend; mode=$saved_mode
fi
backend=${backend:-mongodb}; mode=${mode:-bundled}
case "$interactive" in 0|1) ;; *) fail 'METRIC_INTERACTIVE must be 0 or 1.' ;; esac
prompt() {
  printf '%s [%s]: ' "$1" "$2" >&3
  IFS= read -r answer <&3 || fail 'Setup cancelled before installation.'
  answer=${answer:-$2}
}
uri=
if [ "$backend" = postgres ]; then uri=${METRIC_POSTGRES_URI:-}; else uri=${MONGODB_URI:-}; fi
if [ "$interactive" = 1 ] && [ "$existing" = 0 ]; then
  { exec 3<>/dev/tty; } 2>/dev/null || fail 'Interactive setup needs a terminal. Supply environment values instead.'
  printf 'Metric setup\n' >&3
  prompt 'Database (mongodb/postgres)' "$backend"; backend=$answer
  prompt 'Database placement (bundled/external)' "$mode"; mode=$answer
  if [ "$backend" = postgres ]; then uri=${METRIC_POSTGRES_URI:-}; else uri=${MONGODB_URI:-}; fi
  if [ "$mode" = external ]; then
    if [ -z "$uri" ]; then prompt 'Connection URI (input is visible)' ''; uri=$answer
    else printf 'Using connection URI from environment (credentials omitted).\n' >&3; fi
  fi
  prompt 'Resource profile (min/low/medium/high)' "$profile"; profile=$answer
  prompt 'Metric HTTP port (published on all host interfaces)' "$http_port"; http_port=$answer
fi
case "$profile" in min|low|medium|high) ;; *) fail 'METRIC_PROFILE must be min, low, medium or high.' ;; esac
case "$backend" in mongodb|postgres) ;; *) fail 'Database backend must be mongodb or postgres.' ;; esac
case "$mode" in bundled|external) ;; *) fail 'Database placement must be bundled or external.' ;; esac
if [ "$existing" = 0 ] && [ "$mode" = bundled ] && [ -n "$tls$ca$cert$key" ]; then fail 'TLS options configure an existing database only. The bundled database uses internal Docker TCP.'; fi
case "$http_port" in ''|*[!0-9]*) fail 'HTTP port must be an integer from 1 to 65535.' ;; esac
[ "$http_port" -ge 1 ] && [ "$http_port" -le 65535 ] || fail 'HTTP port must be from 1 to 65535.'
query_value() {
  printf '%s\n' "$uri" | awk -v name="$1" '{n=split($0,q,"?"); if(n>1){n=split(q[2],a,"[&;]");for(i=1;i<=n;i++){p=index(a[i],"=");if(tolower(substr(a[i],1,p-1))==tolower(name)){print substr(a[i],p+1);exit}}}}'
}
decode() {
  printf '%s\n' "$1" | LC_ALL=C awk '{for(i=1;i<=length($0);i++){c=substr($0,i,1);if(c=="%" && substr($0,i+1,2)~/^[0-9a-fA-F][0-9a-fA-F]$/){h=tolower(substr($0,i+1,2));v=(index("0123456789abcdef",substr(h,1,1))-1)*16+index("0123456789abcdef",substr(h,2,1))-1;if(v==0 || v==10 || v==13)exit 1;printf "%c",v;i+=2}else printf "%s",c}}'
}
mongo_database=${METRIC_MONGO_DATABASE:-}; pg_ssl=verify-full
if [ "$mode" = external ] && [ "$existing" = 0 ]; then
  [ -n "$uri" ] || fail 'External database requires METRIC_POSTGRES_URI or MONGODB_URI.'
  case "$uri" in *'
'*) fail 'Connection URI must be one line.' ;; esac
  carriage_return=$(printf '\r')
  case "$uri" in *"$carriage_return"*) fail 'Connection URI must be one line.' ;; esac
  case "$backend:$uri" in postgres:postgres://*|postgres:postgresql://*|mongodb:mongodb://*|mongodb:mongodb+srv://*) ;; *) fail 'Connection URI scheme does not match the selected backend.' ;; esac
  if [ "$backend" = postgres ]; then
    uri_tls=$(query_value sslmode)
    case "$uri_tls" in '') ;; disable) pg_ssl=disable ;; require|verify-ca|verify-full) pg_ssl=$uri_tls ;; *) fail 'Unsupported PostgreSQL sslmode.' ;; esac
    if [ -n "$uri_tls" ]; then
      if [ "$uri_tls" = disable ]; then inferred=off; else inferred=on; fi
    else inferred=; fi
  else
    if [ -z "$mongo_database" ]; then
      path=$(printf '%s\n' "$uri" | sed 's|^[^:]*://[^/]*/*||; s/[?].*$//')
      if [ -n "$path" ]; then mongo_database=$(decode "$path")
      elif [ "$interactive" = 1 ]; then prompt 'Mongo database for Metric data' metric; mongo_database=$answer
      else mongo_database=metric; fi
    fi
    uri_tls=$(query_value tls); [ -n "$uri_tls" ] || uri_tls=$(query_value ssl)
    case "$uri_tls" in true) inferred=on ;; false) inferred=off ;; '') case "$uri" in mongodb+srv://*) inferred=on ;; *) inferred= ;; esac ;; *) fail 'Mongo TLS must be true or false.' ;; esac
    [ -n "$ca" ] || ca=$(decode "$(query_value tlsCAFile)")
    [ -n "$cert" ] || cert=$(decode "$(query_value tlsCertificateKeyFile)")
  fi
  [ -z "$tls" ] || [ -z "$inferred" ] || [ "$tls" = "$inferred" ] || fail 'URI TLS setting conflicts with the selected TLS mode.'
  tls=${tls:-$inferred}
  if [ -z "$tls" ] && [ "$interactive" = 1 ]; then prompt 'Database TLS (on=verified TLS, off=plain TCP)' on; tls=$answer; fi
  tls=${tls:-on}
  case "$tls" in on|off) ;; *) fail 'TLS must be on or off.' ;; esac
  if [ "$tls" = on ] && [ "$interactive" = 1 ]; then
    if [ -z "$ca" ]; then prompt 'Custom CA file (Enter for standard trust)' ''; ca=$answer; fi
    if [ -z "$cert" ]; then prompt 'Client certificate file (Enter if not required)' ''; cert=$answer; fi
    if [ -n "$cert" ] && [ -z "$key" ]; then prompt 'Separate client key file (Mongo combined PEM: Enter)' ''; key=$answer; fi
  fi
  if [ "$tls" = off ]; then
    [ -z "$ca$cert$key" ] || fail 'Certificate files cannot be used with TLS off.'
    pg_ssl=disable
  fi
fi
for program in docker curl awk sed; do command -v "$program" >/dev/null 2>&1 || fail "$program is required."; done
docker compose version </dev/null >/dev/null 2>&1 || fail 'Docker Compose is required.'
random_hex() {
  if command -v openssl >/dev/null 2>&1; then openssl rand -hex "$1"
  elif command -v od >/dev/null 2>&1; then od -An -N "$1" -tx1 /dev/urandom | tr -d ' \n'
  else fail 'OpenSSL or od is required to generate secrets.'; fi
}
mkdir -p "$install_dir"
if [ "$existing" = 0 ]; then
  volumes=$(docker volume ls --format '{{.Name}}' </dev/null) || fail 'Cannot list Docker volumes. Check Docker daemon availability.'
  has_volume() { printf '%s\n' "$volumes" | grep -Fqx "$1"; }
  if has_volume metric_mongo-data; then
    fail 'MongoDB data already exists, but .env is missing. Restore the original .env. The installer will not generate a different password for existing data.'
  fi
  project=${COMPOSE_PROJECT_NAME:-metric}
  for volume in mongo-data postgres-data blob-data; do
    if has_volume "${project}_${volume}"; then fail 'Installation data exists, but .env is missing. Restore the original secrets.'; fi
  done
fi
fetch() {
  curl --fail --location --silent --show-error "$download_base/$1" --output "$2" </dev/null || fail 'Failed to download an installer asset. Existing files were retained.'
}
env_line() {
  escaped=$(printf '%s' "$2" | sed "s/'/\\\\'/g")
  printf "%s='%s'\n" "$1" "$escaped"
}
if [ "$existing" = 0 ]; then
  umask 077
  stage=$(mktemp -d "$install_dir/.install.XXXXXX")
  cleanup() { rm -f "$stage/compose.yml" "$stage/metric.toml" "$stage/template.env" "$stage/.env" "$stage/connection.env" "$stage/symbolicator.yml" "$stage/config.tmp"; rmdir "$stage" 2>/dev/null || :; }
  trap cleanup EXIT
  compose_asset=compose.yml
  [ "$backend:$mode" = mongodb:bundled ] || compose_asset="compose.$backend.$mode.yml"
  fetch "$compose_asset" "$stage/compose.yml"
  config_asset="profiles/$profile.toml"
  [ "$backend" = mongodb ] || config_asset="profiles/postgres/$profile.toml"
  fetch "$config_asset" "$stage/metric.toml"
  fetch symbolicator.yml "$stage/symbolicator.yml"
  fetch "profiles/$profile.env.example" "$stage/template.env"
  scrub_hmac_key=${METRIC_SCRUB_HMAC_KEY:-$(random_hex 32)}
  [ "${#scrub_hmac_key}" = 64 ] || fail 'HMAC key must contain 64 lowercase hexadecimal characters.'
  case "$scrub_hmac_key" in *[!0-9a-f]*) fail 'HMAC key must be lowercase hexadecimal.' ;; esac
  password=$(random_hex 24)
  sed -e "s/replace-with-a-long-url-safe-random-password/$password/" -e "s/replace-with-64-lowercase-hex-characters/$scrub_hmac_key/" "$stage/template.env" | awk '$0 !~ /^METRIC_HTTP_PORT=/ && $0 !~ /^METRIC_IMAGE=/' > "$stage/.env"
  env_line METRIC_HTTP_PORT "$http_port" >> "$stage/.env"
  env_line METRIC_IMAGE "${METRIC_IMAGE:-ghcr.io/biosshot/metric:$version}" >> "$stage/.env"
  env_line METRIC_DATABASE_BACKEND "$backend" >> "$stage/.env"
  env_line METRIC_DATABASE_MODE "$mode" >> "$stage/.env"
  : > "$stage/connection.env"
  if [ "$backend" = postgres ]; then
    env_line METRIC_POSTGRES_PASSWORD "$password" >> "$stage/.env"
    env_line METRIC_POSTGRES_MEMORY_LIMIT "$(env_value "$stage/template.env" METRIC_MONGO_MEMORY_LIMIT)" >> "$stage/.env"
    if [ "$mode" = bundled ]; then pg_ssl=disable; fi
    env_line APP__POSTGRES__SSL_MODE "$pg_ssl" >> "$stage/connection.env"
    if [ "$pg_ssl" != verify-full ]; then env_line APP__POSTGRES__ALLOW_INSECURE_CONNECTION true >> "$stage/connection.env"; fi
  else
    mongo_database=${mongo_database:-metric}
    if [ "$mode" = external ]; then
      escaped_db=$(printf '%s' "$mongo_database" | sed 's/\\/\\\\/g; s/"/\\"/g')
      awk -v db="$escaped_db" '/^database = / {$0="database = \"" db "\""} {print}' "$stage/metric.toml" > "$stage/config.tmp"
      mv "$stage/config.tmp" "$stage/metric.toml"
      if [ -z "$(query_value tls)$(query_value ssl)" ]; then
        separator='?'; case "$uri" in *'?'*) separator='&' ;; esac
        if [ "$tls" = on ]; then uri="$uri${separator}tls=true"; else uri="$uri${separator}tls=false"; fi
      fi
    fi
  fi
  mkdir -p "$install_dir/certs"; chmod 700 "$install_dir/certs"
  mount_cert() {
    [ -f "$1" ] && [ -r "$1" ] || fail 'Certificate/key file does not exist or cannot be read.'
    cp "$1" "$install_dir/certs/$2"; chmod 644 "$install_dir/certs/$2"
    awk -v file="$2" '{print} $0=="      - ./metric.toml:/etc/metric/metric.toml:ro" {print "      - ./certs/" file ":/run/metric-certs/" file ":ro"}' "$stage/compose.yml" > "$stage/config.tmp"
    mv "$stage/config.tmp" "$stage/compose.yml"
  }
  set_mongo_file() {
    name=$1; value=$2; old=$(query_value "$name")
    if [ -n "$old" ]; then
      uri=$(printf '%s\n' "$uri" | awk -v name="$name" -v value="$value" '{p=index($0,"?");base=substr($0,1,p);n=split(substr($0,p+1),a,"[&;]");printf "%s",base;for(i=1;i<=n;i++){if(i>1)printf "&";k=index(a[i],"=");if(tolower(substr(a[i],1,k-1))==tolower(name))printf "%s=%s",name,value;else printf "%s",a[i]}}')
    else uri="$uri&$name=$value"; fi
  }
  if [ -n "$ca" ]; then
    mount_cert "$ca" ca.pem
    if [ "$backend" = postgres ]; then env_line APP__POSTGRES__SSL_ROOT_CERT /run/metric-certs/ca.pem >> "$stage/connection.env"
    else set_mongo_file tlsCAFile /run/metric-certs/ca.pem; fi
  fi
  if [ -n "$cert" ]; then
    mount_cert "$cert" client.pem
    if [ "$backend" = postgres ]; then
      [ -n "$key" ] || fail 'PostgreSQL client certificate requires a separate key file.'
      mount_cert "$key" client.key
      env_line PGSSLCERT /run/metric-certs/client.pem >> "$stage/connection.env"
      env_line PGSSLKEY /run/metric-certs/client.key >> "$stage/connection.env"
    else
      if [ -n "$key" ]; then [ -r "$key" ] || fail 'Client key is not readable.'; printf '\n' >> "$install_dir/certs/client.pem"; cat "$key" >> "$install_dir/certs/client.pem"; fi
      set_mongo_file tlsCertificateKeyFile /run/metric-certs/client.pem
    fi
  elif [ -n "$key" ]; then fail 'A client key requires a client certificate.'; fi
  if [ "$mode" = external ]; then
    uri=$(printf '%s\n' "$uri" | sed -E 's#([@/])localhost([:/?]|$)#\1host.docker.internal\2#g; s#([@/])127\.0\.0\.1([:/?]|$)#\1host.docker.internal\2#g')
    if [ "$backend" = postgres ]; then env_line METRIC_POSTGRES_URI "$uri" >> "$stage/.env"; else env_line MONGODB_URI "$uri" >> "$stage/.env"; fi
    case "$uri" in *host.docker.internal*)
      if ! docker info --format '{{.OperatingSystem}}' </dev/null | grep -qi 'docker desktop'; then
        awk '{print} $0=="  metric:" {print "    extra_hosts: [\"host.docker.internal:host-gateway\"]"}' "$stage/compose.yml" > "$stage/config.tmp"
        mv "$stage/config.tmp" "$stage/compose.yml"
      fi ;;
    esac
  fi
  chmod 644 "$stage/metric.toml" "$stage/compose.yml" "$stage/symbolicator.yml"
  for file in compose.yml metric.toml symbolicator.yml; do
    if [ ! -f "$install_dir/$file" ]; then mv "$stage/$file" "$install_dir/$file"; fi
  done
  mv "$stage/connection.env" "$install_dir/connection.env"
  mv "$stage/.env" "$install_dir/.env"
fi
active_profile=$(env_value "$install_dir/.env" METRIC_PROFILE)
if [ "$active_profile" != "$profile" ]; then printf 'Existing installation keeps profile %s; requested %s was not applied.\n' "$active_profile" "$profile" >&2; fi
(
  cd "$install_dir"
  # Compose prefers process variables over .env. Use the persisted connection,
  # including its container host/certificate paths, rather than the original URI.
  unset MONGODB_URI METRIC_POSTGRES_URI METRIC_HTTP_PORT METRIC_IMAGE METRIC_SCRUB_HMAC_KEY METRIC_MONGO_PASSWORD METRIC_POSTGRES_PASSWORD
  docker compose config --quiet || fail 'Generated Compose configuration is invalid.'
  docker compose pull
  docker compose run -T --rm --no-deps metric --config /etc/metric/metric.toml --check-config
  docker compose up -d --wait --wait-timeout 120
  docker compose ps
) </dev/null # Docker must not consume the remaining installer in curl | sh.
http_port=$(env_value "$install_dir/.env" METRIC_HTTP_PORT)
printf '\nProfile: %s\nDatabase: %s (%s)\nMetric is ready at http://localhost:%s\nConfiguration: %s/metric.toml\n' "$active_profile" "$backend" "$mode" "$http_port" "$install_dir"
printf 'First setup token: cd %s && docker compose logs metric\n' "$install_dir"
}

main "$@"
