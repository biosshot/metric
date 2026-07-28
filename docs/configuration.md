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

Configuration is static until restart. Project-owned PII/retention/key policy changes
through authorized API commands and local cache invalidation; there is no partial
config hot reload.

Use `--check-config` before startup and `--print-effective-config` for a redacted
diagnostic. MongoDB schema generation 18 permits idempotent empty-database bootstrap
only; it is not a migration mechanism.
