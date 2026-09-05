{{- define "metric.name" -}}
{{- default .Release.Name .Values.fullnameOverride | trunc 50 | trimSuffix "-" -}}
{{- end -}}

{{- define "metric.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | quote }}
app.kubernetes.io/name: metric
app.kubernetes.io/instance: {{ .Release.Name | quote }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service | quote }}
{{- end -}}

{{- define "metric.selector" -}}
app.kubernetes.io/name: metric
app.kubernetes.io/instance: {{ .Release.Name | quote }}
app.kubernetes.io/component: {{ .component | quote }}
{{- end -}}

{{- define "metric.profile" -}}
{{- index (.Files.Get "files/profiles.json" | fromYaml) .Values.profile | toYaml -}}
{{- end -}}

{{- define "metric.image" -}}
{{- if ne .Chart.Version .Chart.AppVersion -}}
{{- fail "Chart.version and Chart.appVersion must match" -}}
{{- end -}}
{{- if and .Values.image.tag (ne .Values.image.tag .Chart.Version) -}}
{{- fail "Metric image.tag must equal Chart.version; update the chart and image together" -}}
{{- end -}}
{{- printf "%s:%s" .Values.image.repository .Chart.Version -}}
{{- end -}}

{{- define "metric.symbolicatorEnabled" -}}
{{- if kindIs "bool" .Values.symbolicator.enabled -}}
{{- .Values.symbolicator.enabled -}}
{{- else -}}
{{- (include "metric.profile" . | fromYaml).symbolicatorEnabled -}}
{{- end -}}
{{- end -}}

{{- define "metric.blobClaim" -}}
{{- default (printf "%s-blobs" (include "metric.name" .)) .Values.persistence.existingClaim -}}
{{- end -}}
{{- define "metric.mongoClaim" -}}
{{- default (printf "%s-mongodb" (include "metric.name" .)) .Values.mongodb.persistence.existingClaim -}}
{{- end -}}

{{- define "metric.quantityBytes" -}}
{{- $q := . | toString | replace " " "" -}}
{{- if not (regexMatch "^[1-9][0-9]*(B|Ki|KiB|Mi|MiB|Gi|GiB|Ti|TiB)$" $q) -}}
{{- fail "Storage sizes must be positive integers in B, KiB, MiB, GiB or TiB (Kubernetes suffixes Ki/Mi/Gi/Ti also work)" -}}
{{- end -}}
{{- $factor := 1 -}}
{{- if contains "Ki" $q -}}{{- $factor = 1024 -}}{{- end -}}
{{- if contains "Mi" $q -}}{{- $factor = 1048576 -}}{{- end -}}
{{- if contains "Gi" $q -}}{{- $factor = 1073741824 -}}{{- end -}}
{{- if contains "Ti" $q -}}{{- $factor = 1099511627776 -}}{{- end -}}
{{- mul (regexFind "^[0-9]+" $q | int64) $factor -}}
{{- end -}}

{{- define "metric.config" -}}
{{- $cfg := mergeOverwrite (deepCopy (include "metric.profile" . | fromYaml).config) (deepCopy .Values.config) -}}
{{- $reserved := dict "role" (list "*") "server" (list "http_address" "shutdown_grace" "trusted_proxies") "mongodb" (list "uri" "database") "projects" (list "scrub_hmac_key") "blob" (list "backend" "root" "s3") "symbolicator" (list "endpoint" "callback_base_url") "auth" (list "secure_cookie") "development" (list "allow_literal_secrets" "allow_insecure_cookies") -}}
{{- range $section, $fields := $reserved -}}
{{- if hasKey $.Values.config $section -}}
{{- range $field := $fields -}}
{{- if or (eq $field "*") (hasKey (index $.Values.config $section) $field) -}}
{{- fail (printf "config.%s.%s is chart-owned; use the documented deployment values" $section $field) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $_ := set $cfg.server "http_address" "0.0.0.0:4001" -}}
{{- $_ := set $cfg.server "shutdown_grace" "30s" -}}
{{- $_ := set $cfg.server "trusted_proxies" .Values.http.trustedProxies -}}
{{- $_ := set $cfg.mongodb "database" .Values.mongodb.database -}}
{{- $_ := set $cfg.mongodb "uri" (dict "env" "MONGODB_URI") -}}
{{- $_ := set $cfg.projects "scrub_hmac_key" (dict "env" "SCRUB_HMAC_KEY") -}}
{{- $_ := set $cfg.auth "secure_cookie" .Values.http.secureCookies -}}
{{- $_ := set $cfg.development "allow_insecure_cookies" (not .Values.http.secureCookies) -}}
{{- $_ := set $cfg.blob "backend" .Values.blob.backend -}}
{{- $_ := set $cfg.blob "root" "/var/lib/metric/blobs" -}}
{{- if eq .Values.blob.backend "s3" -}}
{{- $s3 := dict "region" .Values.blob.s3.region "bucket" .Values.blob.s3.bucket "force_path_style" .Values.blob.s3.forcePathStyle "access_key_id" (dict "env" "S3_ACCESS_KEY_ID") "secret_access_key" (dict "env" "S3_SECRET_ACCESS_KEY") -}}
{{- if .Values.blob.s3.endpoint -}}{{- $_ := set $s3 "endpoint" .Values.blob.s3.endpoint -}}{{- end -}}
{{- if .Values.blob.s3.sessionToken -}}{{- $_ := set $s3 "session_token" (dict "env" "S3_SESSION_TOKEN") -}}{{- end -}}
{{- $_ := set $cfg.blob "s3" $s3 -}}
{{- end -}}
{{- if eq (include "metric.symbolicatorEnabled" .) "true" -}}
{{- $_ := set $cfg.symbolicator "endpoint" (default (printf "http://%s-symbolicator:3021/symbolicate" (include "metric.name" .)) .Values.symbolicator.externalEndpoint) -}}
{{- else -}}
{{- $_ := unset $cfg.symbolicator "endpoint" -}}
{{- end -}}
{{- $_ := set $cfg.symbolicator "callback_base_url" (printf "http://%s:%v/" (include "metric.name" .) .Values.service.port) -}}
{{- include "metric.configIntegers" $cfg -}}
{{- $cfg | toToml | required "Application config could not be serialized as TOML" -}}
{{- end -}}

{{/* Helm's YAML decoder produces float64 numbers; the Rust config requires integers. */}}
{{- define "metric.configIntegers" -}}
{{- range $key, $value := . -}}
{{- if kindIs "map" $value -}}
{{- include "metric.configIntegers" $value -}}
{{- else if kindIs "float64" $value -}}
{{- if or (ne (floor $value) $value) (gt $value 9007199254740991.0) (lt $value -9007199254740991.0) -}}
{{- fail "Application numeric settings must be exactly representable whole numbers; use strings for sizes and durations" -}}
{{- end -}}
{{- $_ := set $ $key (int64 $value) -}}
{{- else if kindIs "slice" $value -}}
{{- $items := list -}}
{{- range $item := $value -}}
{{- $holder := dict "item" $item -}}
{{- include "metric.configIntegers" $holder -}}
{{- $items = append $items $holder.item -}}
{{- end -}}
{{- $_ := set $ $key $items -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "metric.blobSize" -}}
{{- $cfg := mergeOverwrite (deepCopy (include "metric.profile" . | fromYaml).config) (deepCopy .Values.config) -}}
{{- $capacity := include "metric.quantityBytes" $cfg.blob.capacity | int64 -}}
{{- if .Values.persistence.size -}}
{{- if lt (include "metric.quantityBytes" .Values.persistence.size | int64) $capacity -}}
{{- fail "persistence.size must cover config.blob.capacity (or the selected profile's capacity)" -}}
{{- end -}}
{{- .Values.persistence.size -}}
{{- else -}}
{{- $capacity -}}
{{- end -}}
{{- end -}}

{{- define "metric.validateEnv" -}}
{{- $seen := dict -}}
{{- range .Values.extraEnv -}}
{{- if or (hasKey $seen .name) (has .name (list "MONGODB_URI" "METRIC_MONGO_PASSWORD" "SCRUB_HMAC_KEY" "METRIC_WEB_DIR" "S3_ACCESS_KEY_ID" "S3_SECRET_ACCESS_KEY" "S3_SESSION_TOKEN")) (hasPrefix "APP__" .name) -}}
{{- fail (printf "extraEnv variable %s duplicates or overrides chart wiring; use config for application overrides" .name) -}}
{{- end -}}
{{- $_ := set $seen .name true -}}
{{- end -}}
{{- end -}}

{{- define "metric.symbolicatorConfig" -}}
cache_dir: /data
bind: 0.0.0.0:3021
logging:
  level: warn
  format: simplified
  enable_backtraces: false
metrics:
  statsd: null
sentry_dsn: null
# Metric serves signed debug-file URLs over the cluster's private network.
connect_to_reserved_ips: true
symstore_proxy: false
max_concurrent_requests: 8
{{- end -}}
