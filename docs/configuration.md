# Configuration

Metric loads one immutable typed configuration at startup:

```text
CLI override -> APP__SECTION__FIELD -> TOML -> documented default
```

An env file is loaded only when explicitly named:

```powershell
metric-server --config config/metric.example.toml --env-file .env.local
```

Existing process environment variables override values from that file. Unknown TOML
fields and unknown `APP__` paths fail startup.

MongoDB URI, scrub HMAC material and S3 credentials use secret references:

```toml
[mongodb]
uri = { env = "MONGODB_URI" }

[projects]
scrub_hmac_key = { file = "C:/metric/secrets/scrub-hmac.txt" }
```

Literal secrets require explicit development mode and produce a warning. Effective
configuration, Debug output, probes and errors render only the secret origin.

Native API admission is bounded independently from ingest:

```toml
[server]
max_active_requests = 512
request_timeout = "30s"
trusted_proxies = ["127.0.0.1/32", "::1/128"]
```

`trusted_proxies` accepts at most 64 IPv4/IPv6 addresses or CIDR ranges. Metric uses
`X-Forwarded-For` for the login network limiter only when the direct TCP peer
matches one of these entries. Leave the list empty when Metric receives client
connections directly.

Configuration is static until restart. Project-owned PII/retention/key policy changes
through authorized API commands and local cache invalidation; there is no partial
config hot reload.

Use `--check-config` before startup and `--print-effective-config` for a redacted
diagnostic. The current binary requires MongoDB schema generation **19 exactly**.
It permits idempotent empty-database bootstrap only; it is not a migration
mechanism. Older data-bearing generations are rejected. Do not edit the schema
marker or recreate the database; see [Schema compatibility and
upgrades](upgrading.md).
