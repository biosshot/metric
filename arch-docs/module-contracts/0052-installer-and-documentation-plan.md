# Installer and Getting Started implementation plan

- Date: 2026-10-14
- Status: Implemented and locally verified; see [delivery evidence](../phase-reports/0052-installer-and-documentation.md)
- Related decision: [PostgreSQL backend](../0052-postgresql-storage-backend.md)
- Backend evidence: [Functional parity report](../phase-reports/0052-postgresql-functional-parity.md)
- Implementation branch: `feat/postgres`; preserve unrelated working tree changes
- Commits: Conventional Commits

Extend the existing shell and PowerShell installers with an explicitly selected
interactive wizard and PostgreSQL installation. Keep existing unattended commands
and MongoDB defaults working. Present the wizard first in Getting Started and keep
its questions limited to the database, connection, resource profile and Metric HTTP port.

This is a distribution and documentation follow-up to the completed backend work.
It does not change database adapters, schemas or installed migrations. The original
backend plan excluded public installer packaging; this document defines that next
scope. It does not publish a release or switch an existing installation's backend.

## Existing interfaces and compatibility

Preserve `curl .../install.sh | sh` and `irm .../install.ps1 | iex` without questions.
Preserve `METRIC_PROFILE`, `METRIC_VERSION`, `METRIC_INSTALL_DIR` and
`METRIC_DOWNLOAD_BASE`, their defaults and the existing directory detection.
Existing profile and deployment environment names remain valid.

Without new options, a fresh installation still uses Medium, bundled MongoDB and
HTTP port 4001. No terminal detection may silently enable the wizard. Both explicit
`--interactive` / `-Interactive` and `METRIC_INTERACTIVE=1` enable it; the environment
form also works with the existing download-and-execute commands.

An existing installation keeps its Compose, TOML, secrets, volumes and backend.
Omitted backend/mode options reuse the saved installation, rather than applying
fresh-installation defaults. Preserve the existing warning when a requested profile
is not applied. An explicitly conflicting backend or database mode exits with a
clear explanation; it does not reconnect to another database or rewrite files.
Keep the missing-secret guard for existing Mongo volumes and extend it to owned
PostgreSQL and BlobStore volumes. Never regenerate lost installation secrets over
data-bearing storage. The wizard is for initial setup, not an implicit upgrade or
reconfiguration tool.

## Public installer parameters

The table defines future interfaces. These options are not available in the current
published installer. `--help` and PowerShell `-Help` must list the same contract.

| Purpose | Shell option | PowerShell option | Environment | Fresh default |
| --- | --- | --- | --- | --- |
| Enable wizard | `--interactive` | `-Interactive` | `METRIC_INTERACTIVE=1` | Off |
| Resource profile | `--profile NAME` | `-Profile NAME` | `METRIC_PROFILE` | `medium` |
| Database backend | `--backend NAME` | `-Backend NAME` | `METRIC_DATABASE_BACKEND` | `mongodb` |
| Database placement | `--database MODE` | `-Database MODE` | `METRIC_DATABASE_MODE` | `bundled` |
| Installation directory | `--install-dir PATH` | `-InstallDir PATH` | `METRIC_INSTALL_DIR` | Existing directory detection, otherwise `metric` |
| Published HTTP port | `--http-port PORT` | `-HttpPort PORT` | `METRIC_HTTP_PORT` | `4001` |
| External database TLS | `--tls on\|off` | `-Tls on\|off` | `METRIC_DATABASE_TLS` | On |
| Custom trusted CA | `--tls-ca PATH` | `-TlsCa PATH` | `METRIC_DATABASE_TLS_CA` | Standard driver trust roots |
| Client certificate | `--tls-client-cert PATH` | `-TlsClientCert PATH` | `METRIC_DATABASE_TLS_CLIENT_CERT` | Unset |
| Separate client key | `--tls-client-key PATH` | `-TlsClientKey PATH` | `METRIC_DATABASE_TLS_CLIENT_KEY` | Unset |

Profiles are `min`, `low`, `medium`, `high`; backends are `mongodb`, `postgres`;
placement is `bundled`, `external`. External includes an existing database on the
same computer. URI credentials are accepted through environment only:

- PostgreSQL: `METRIC_POSTGRES_URI`, including the database name.
- MongoDB: `MONGODB_URI`; `METRIC_MONGO_DATABASE` selects its database, default `metric`.
- `METRIC_SCRUB_HMAC_KEY`: optional supplied permanent key; otherwise generate it.

Do not add password arguments. Keep version and download-base environment options;
they do not need new wizard questions. Do not add generic `--set`, a second complete
configuration language, mandatory `--non-interactive`, or an advanced-settings menu.
Unattended mode is already the default. Ordinary application settings remain TOML.

For a fresh install, precedence is command-line option, environment, then default.
The wizard displays resolved settings as initial answers and uses accepted answers
as the final choices. URI entry is deliberately visible; generated output and errors
still redact credentials instead of printing the full URI again.
Only explicitly selected backend secrets are required and passed to the application.
Invalid enums, ports, certificate paths or missing external URI fail unattended mode
with a nonzero exit status and an actionable error; never prompt there.

The common TLS names replace the PostgreSQL-only installer flags discussed earlier.
They do not rename runtime TOML fields. Advanced PostgreSQL `ssl_mode` values stay
in the configuration reference, rather than becoming extra wizard choices.

## Minimal interactive wizard

The ordinary path has four choices, each with a visible default. External database
connection uses one visibly entered URI, as agreed on 2026-10-14. Metric's HTTP port
is separate from the database port embedded in that URI.

```text
Metric setup

Database:          MongoDB [default] / PostgreSQL
Database location: Start a database with Metric [default] / Use an existing database
Resource profile:  Min / Low / Medium [default] / High
Metric HTTP port:  4001 [default]
```

Ask connection questions only when the existing-database choice is selected:

1. Connection URI, entered visibly so the operator can inspect and edit it.
2. Mongo database name only if the URI does not contain one; default `metric`.
3. TLS only if not determined by the URI; default verified TLS, with an explicit
   plain TCP choice. Mongo SRV already implies TLS unless explicitly disabled.
4. With TLS enabled, optional CA and client certificate files when needed. Ask for
   a separate private key only when needed by the chosen backend/file format.

An environment-supplied complete URI is reused without reconstructing host/password
fields. Preserve driver options such as Mongo replica-set/SRV/authentication options.
Show a redacted generated summary; actual URI input remains visible.
Do not silently erase URI TLS choices; reject an explicit contradiction between URI
and wizard/CLI policy and explain which setting to change.

Do not ask about retention, queues, pool sizes, migration mode, archive, disk quotas,
Symbolicator internals, bind addresses or telemetry features. Ask the Metric HTTP
port alongside the profile. The selected profile
supplies those settings. After successful setup, show the actual interface URL,
selected backend/profile, installation directory, TOML location and setup-token
retrieval command. Show no password, private key or full credential-bearing URI.

On shell download pipelines, wizard input must come from the controlling terminal,
not from the stream containing the script. URI input uses ordinary visible terminal
reading; do not disable echo. PowerShell must support
both direct script invocation and the existing `irm | iex` environment route. If
interactive mode is explicitly requested without an interactive terminal, fail
clearly and suggest supplying environment values instead.

## TLS and connection responsibility

TLS here protects the native database protocol, not HTTP. The operator configures
their existing MongoDB/PostgreSQL server and supplies its connection information.
The installer does not create a certificate authority, issue/renew certificates,
enable server TLS or require a paid certificate/provider.

For a bundled database, retain the simple internal Docker connection without TLS,
explicitly configure PostgreSQL's insecure-connection acknowledgement and do not
publish the database port. Only Metric's configured HTTP port is published. Existing
Mongo deployment defaults stay unchanged.

For an external database, default to verified TLS. An explicit `--tls off` or the
wizard's TCP choice acknowledges plaintext; no extra confirmation prompt is needed.
Map this to PostgreSQL `ssl_mode = "disable"` and
`allow_insecure_connection = true`, or Mongo `tls=false`. Verified mode maps to
PostgreSQL `verify-full` or Mongo `tls=true`, with normal certificate/hostname checks.
Do not turn on `tlsAllowInvalidCertificates` or `tlsAllowInvalidHostnames`.

Mount selected certificate/key files read-only into Metric and store container
paths, not host paths, in driver configuration. Mongo's `tlsCertificateKeyFile`
needs a PEM containing the client certificate and key; if the wizard receives
separate files, prepare an installation-owned private PEM with restricted access.
PostgreSQL can use its existing operator-controlled `PGSSLCERT` / `PGSSLKEY` driver
inputs with container paths. Certificate authentication also needs the appropriate
URI authentication options; support supplying a complete operator URI and document
this advanced case without expanding the ordinary wizard. Reject incompatible
certificate inputs instead of assuming password and certificate authentication
are interchangeable.

Wrong certificates, unsupported protected connections or wrong credentials must
never trigger plaintext fallback. A local CA is supported without an external
HTTP certificate download. The database must already exist for `external`; Metric
creates its schema/collections using existing startup migrations. Explain required
permissions and keep database provisioning outside this connection path.

Use Docker Desktop's host address on Windows/macOS for a database on this computer.
On supported Linux Docker Engine versions, generate the host-gateway mapping and
check actual reachability; do not promise that a loopback-only host listener is
accessible from a Linux container. Give a concrete diagnostic without changing
the database's bind address or firewall. Inside a container, `localhost` denotes
that container. Preserve named Docker-service addresses when explicitly supplied.
Publish Metric's port on all host interfaces by default, preserving existing
behavior. Do not ask a wizard bind-address question. Host publication is a Compose
setting; changing the app's TOML bind address inside its container does not restrict
the host port. Document optional Compose edits for loopback-only publication.

## Configuration generation and startup

Use the existing four profiles as the shared source of application limits. Backend
selection changes the storage/connection section, database service and relevant
database resource settings. Do not maintain eight independent copies of application
settings. If release assets include PostgreSQL profile variants, generate them
deterministically from shared profile data and validate their common fields.
PostgreSQL resource settings are conservative starting values; existing Mongo-only
measurements do not establish PostgreSQL throughput or minimum-machine guarantees.

Produce one installed `compose.yml`, one `metric.toml` and a private `.env`, retaining
optional Symbolicator files and existing ordinary `docker compose` commands. Keep
legacy bundled Mongo composition compatible. An external configuration has no
bundled database service/dependency; bundled PostgreSQL has no Mongo service.
Reuse common Metric/BlobStore/Symbolicator definitions or checked generated assets
to prevent independent Compose copies from drifting. Preserve logging, resource
limits, restart policy, read-only mounts and other current deployment protections.

Secrets must be passed explicitly into the container, since Compose interpolation
does not pass arbitrary `.env` values automatically. Use correct dotenv/TOML/URI
escaping for special characters; never interpolate secrets into shell code or use
unescaped search-and-replace. Preserve passwords and the HMAC key across restarts
and installer reruns. Use restrictive file permissions on Unix and protect private
files with appropriate Windows ACLs. Do not print unredacted Compose configuration
or connection commands in normal output/error logs.

Check generated config before application startup, then use the existing readiness
wait and startup migration path. Report connection/configuration failures in plain
language. Do not perform test inserts/deletes against an external user database,
drop schemas, edit schema generations or add a second migration implementation.
Any retry must follow existing runtime policy; protected TLS failures remain fatal.

## Getting Started page structure

Update [Install Metric](../../docs/getting-started.md) in this order:

1. **Interactive installation**, first immediately after prerequisites. Show Linux/
   macOS and Windows one-command examples for the actual release version. Explain
   that the wizard selects a database and profile and prepares the files.
2. **Installation without questions**, retaining the exact old commands and profile
   environment examples. State MongoDB/Medium defaults. Link the CLI parameter table.
3. **Existing PostgreSQL or MongoDB**, with ready-to-copy environment-based examples
   for both shell and PowerShell, verified TLS and explicit TCP variants, database
   provisioning requirements and host/container address explanations.
4. **Open Metric**, interface URL, readiness and setup-token instructions.
5. **Where settings are stored**, the installed directory tree and a direct
   explanation that the installed `metric/metric.toml` is the file to edit.
6. **Edit settings after installation**, one complete retention-setting example,
   config validation and the correct restart/recreate commands.
7. **Manual installation and useful commands**, exact-version profile/template
   download links and retained data-preservation guidance.

The page must link the actual profile TOMLs, `.env` examples and configuration
reference. Describe these as source templates, distinct from the installed edited
file. Show `notepad .\metric.toml` on Windows and an ordinary editor on Linux/macOS,
from the installation directory. Do not rely on the reader knowing the repository
layout or needing to clone it. Do not place the older unattended example above the
wizard. The installer help and docs must agree on spellings, values and defaults.

Future release command forms:

```bash
# Replace <release-tag> with the published version when preparing release docs.
curl -fsSL https://raw.githubusercontent.com/biosshot/metric/<release-tag>/deploy/install.sh \
  | METRIC_INTERACTIVE=1 sh

# A downloaded installer also accepts:
sh install.sh --interactive
```

```powershell
$env:METRIC_INTERACTIVE = '1'
try {
    irm https://raw.githubusercontent.com/biosshot/metric/<release-tag>/deploy/install.ps1 | iex
} finally {
    Remove-Item Env:METRIC_INTERACTIVE -ErrorAction SilentlyContinue
}

# A downloaded installer also accepts:
.\install.ps1 -Interactive
```

Do not publish these new options under a release tag whose installer lacks them.
Test the exact download pipeline, not just a local-script substitute.

The editing instructions use commands already understood by the application:

```bash
cd metric
# Edit metric.toml, then validate without connecting to the database:
docker compose run --rm --no-deps metric --config /etc/metric/metric.toml --check-config
docker compose restart metric
```

`--check-config` validates settings and secret resolution, not database connectivity.
For changed `.env`, certificates mounted at different paths or Compose settings,
use `docker compose up -d --force-recreate metric`. Changes to a bundled database
service may require recreating that service too; never imply that changing an init
password variable updates credentials in an existing database volume. Document that
switching `storage.backend` does not migrate data and is outside ordinary tuning.

## Supporting documentation changes

| File | Required update |
| --- | --- |
| [Configuration](../../docs/configuration.md) | Storage selection; all implemented PostgreSQL fields/defaults; native TCP/TLS; Mongo TLS URI options and certificate mounts; environment versus Compose behavior; migration auto/check semantics |
| [Docker](../../docs/docker.md) | Bundled/external topology, certificate mounts, same-computer addressing and persistent volumes |
| [Backup and restore](../../docs/backup-restore.md) | Separate PostgreSQL procedure keeping database, BlobStore and HMAC/secrets together; scoped shared-server backup; no claim of a complete installation backup from events alone |
| [Update Metric](../../docs/upgrading.md) | Backend-specific schema requirements, PostgreSQL migration ledger/checksums, forward recovery and preservation of existing installer configuration |
| [Troubleshooting](../../docs/troubleshooting.md) | Missing database, bad credentials, permissions, CA/hostname failures, host addressing and readiness; distinguish config check from connection check |
| [README](../../README.md) and [docs home](../../docs/index.md) | Wizard-first installation link and actual backend availability; retain Mongo-only performance attribution |
| [Example config](../../config/metric.example.toml) | Preserve Mongo default; add discoverable commented PostgreSQL selection/connection example or link a dedicated shipped example |
| Release notes and docs navigation | Link PostgreSQL setup and installer changes under their actual release; keep version/image/template references synchronized |

Update the architecture index's current deployment claims when packaging is
implemented; distinguish the already verified backend from installer delivery.
Detailed existing migration evidence stays in the functional report.

## Implementation stages and acceptance

1. Add argument parsing and normalized installer settings to both scripts while
   preserving the current unattended path. Test `--help`, invalid/missing arguments,
   environment precedence and PowerShell download-and-execute binding.
2. Add the minimal terminal wizard with visible URI input and cancellation handling.
   Feed its answers into the same settings/config generation as CLI mode.
3. Add backend/placement assets and checked generation, certificate handling and
   secret persistence. Validate resulting TOML through the actual Metric image.
4. Write Getting Started with the wizard first, update the supporting references
   and provide exact-version executable examples.
5. Run targeted installer and real Docker functional gates; record results before
   claiming release readiness. Keep production adapter code unchanged.

Required functional matrix covers Mongo/PostgreSQL times bundled/external on shell
and PowerShell. Cover all four profiles through config/topology validation and
representative actual startup without parallel resource-heavy stacks. Check the
existing published-style commands, local flag commands, environment-only commands,
interactive pipelines, non-terminal rejection and identical shell/PowerShell
configuration outcomes. Test Linux/macOS/Windows addressing where supported; mark
platforms not actually exercised rather than claiming them verified.

Real Docker cases must include both verified TLS and explicit TCP against owned
external fixtures, custom CA and client-certificate cases for each backend, wrong
CA/hostname/key, missing URI/database, special-character credentials, readiness,
restart persistence and clean shutdown. Certificate authentication cases must use
fixtures configured to require it. Verify selected services only, no published
bundled DB port, preserved security limits and no secret output.

Rerun each installer on populated owned fixtures, compare config/secret checksums,
test missing-secret protection and explicit backend conflicts, and prove that no
unrelated volumes or databases are changed. Run doc/link/profile/version validators
and the docs build. Never test destructive behavior on a user's existing database.
Load/soak testing remains deferred; no measured PostgreSQL capacity claim is added.

Installer implementation and documentation are delivered for 0.1.8 as
explicitly authorized. Publication and deployment remain separate subsequent work.
