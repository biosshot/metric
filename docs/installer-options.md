# Installer options

The shell and PowerShell installers share the same choices. Without an interactive
flag they never ask questions. These options belong to the upcoming 0.1.8 installer;
use the matching script and assets when the release is published.

| Purpose | Shell | PowerShell | Environment |
| --- | --- | --- | --- |
| Wizard | `--interactive` | `-Interactive` | `METRIC_INTERACTIVE=1` |
| Profile | `--profile low` | `-Profile low` | `METRIC_PROFILE` |
| Backend | `--backend postgres` | `-Backend postgres` | `METRIC_DATABASE_BACKEND` |
| Placement | `--database external` | `-Database external` | `METRIC_DATABASE_MODE` |
| Installation folder | `--install-dir PATH` | `-InstallDir PATH` | `METRIC_INSTALL_DIR` |
| Metric HTTP port | `--http-port 4001` | `-HttpPort 4001` | `METRIC_HTTP_PORT` |
| External DB TLS | `--tls on` | `-Tls on` | `METRIC_DATABASE_TLS` |
| Trusted CA file | `--tls-ca PATH` | `-TlsCa PATH` | `METRIC_DATABASE_TLS_CA` |
| Client certificate | `--tls-client-cert PATH` | `-TlsClientCert PATH` | `METRIC_DATABASE_TLS_CLIENT_CERT` |
| Separate client key | `--tls-client-key PATH` | `-TlsClientKey PATH` | `METRIC_DATABASE_TLS_CLIENT_KEY` |
| Help | `--help` | `-Help` | — |

Profiles: `min`, `low`, `medium`, `high`. Backends: `mongodb`, `postgres`.
Placement: `bundled` starts a database container; `external` uses an existing
database, including one on this computer. External TLS is `on` by default;
explicit `off` acknowledges an unencrypted connection.

For fresh installs, flags override environment values, then defaults are used.
The wizard uses these initial choices. Existing installations retain their files
and secrets; rerunning the installer does not silently change a backend, profile
or connection. Conflicting backend/placement requests stop with an explanation.

## Connection credentials

Supply PostgreSQL through `METRIC_POSTGRES_URI`, MongoDB through `MONGODB_URI`.
Do not pass passwords as CLI arguments. The URI is entered **visibly** in the
wizard and is not echoed in generated summaries or errors. Mongo's data database
comes from the URI path or `METRIC_MONGO_DATABASE`; `authSource` remains separate.
Do not confuse a Mongo auth database with the Metric data database.

Retained environment options include `METRIC_VERSION`, `METRIC_DOWNLOAD_BASE`,
`METRIC_INSTALL_DIR` and `METRIC_PROFILE`. `METRIC_IMAGE` overrides the application
image for a reviewed development build. `METRIC_SCRUB_HMAC_KEY` can supply an
existing permanent key; otherwise a random key is generated and retained.

## Certificates

For an existing verified TLS connection, supply a custom CA only if the normal
driver trust roots do not trust the database server:

```bash
sh install.sh --backend postgres --database external --tls-ca ./ca.pem
```

```powershell
.\install.ps1 -Backend postgres -Database external -TlsCa .\ca.pem
```

The installer maps host files to read-only container files. PostgreSQL client TLS
uses a certificate and separate private key. Mongo accepts a combined client PEM
or combines supplied certificate/key files. Certificate authentication also needs
the correct URI authentication settings. The installer configures the client;
the operator configures the database server's TLS and authentication policy.

URI TLS choices are retained and an explicit contradictory CLI choice is rejected.
Mongo SRV implies TLS unless disabled. PostgreSQL accepts `sslmode` in its URI;
other PostgreSQL URI query parameters are not supported by Metric's current URI
policy. Use TOML and the documented driver environment settings for those options.
No failed protected connection is automatically retried as plaintext.
