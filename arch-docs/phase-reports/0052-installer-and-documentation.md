# Installer and documentation delivery

- Date: 2026-10-14
- Branch: `feat/postgres`
- Target version: `0.1.8`, explicitly authorized without publication
- Status: Completed; all required local installer gates passed
- Contract: [Installer plan](../module-contracts/0052-installer-and-documentation-plan.md)

## Delivered scope

The shell and PowerShell installers retain their unattended download pipelines and
MongoDB/Medium defaults. An explicitly enabled wizard asks backend, bundled/external
placement, a visibly entered URI for an existing database, resource profile and
Metric HTTP port. Mongo database/TLS/certificate inputs appear only when necessary.
The HTTP port is published on all interfaces by default. Host exposure is a Compose
setting and can be restricted there after installation.

Installed files and permanent secrets survive reruns. A conflicting backend fails
without switching data sources. Lost `.env` over owned populated volumes stops
instead of generating another password/HMAC key. The application and both adapter
implementations are unchanged; Cargo/Web/Helm changes only synchronize version
metadata for 0.1.8. No database migration was added or modified.

PostgreSQL profile/Compose assets are generated from shared application profiles
and the existing Compose service definitions. They do not establish new performance
claims. Verified external TLS supports custom CA and client certificates through
read-only mounts. Bundled databases remain internal TCP services without published
database ports. Configuring server TLS stays the operator's responsibility.

Getting Started now places interactive installation first, keeps unattended commands,
explains existing-database setup, links exact-version templates and identifies the
installed TOML to edit. Configuration, Docker, backups, updates, troubleshooting,
README/home, navigation and release notes describe both backends. New download
commands are marked as release preparation until the 0.1.8 tag/image are published.

## Release image

The existing Dockerfile stages were built using Rust 1.94 and serialized Cargo.
Release compilation completed in 9m52s. Only the ignored test Dockerfile adds `-j 1`;
the shipped Dockerfile remains unchanged. The frozen build context takes the committed
CSS rather than the unrelated user's working CSS edits. The user's file remains
unchanged and is not part of this delivery.

Image: `metric-installer:0.1.8`; actual binary reports `metric 0.1.8`.
Manifest list: `sha256:4980839ab0d908358173e6198ccf2c3e3d560a3760416ac83de1e32b277fd4cf`.
Build log: ignored `target/installer-image-build.log`.

## Functional evidence

The conformance coordinator downloads the actual frozen installer scripts and assets
from an owned local HTTP server, then runs real Docker database/application images.
The test server sets image pull policy to reuse the locally built candidate and
cached pinned dependency images; it does not substitute application behavior.
All test databases, containers, networks and volumes are owned UUID fixtures.
The user's existing `myapp`, developer instances and persistent volumes are not used.

| Gate | Result | Evidence |
| --- | --- | --- |
| Actual binary config check, all four profiles for both backends | Eight passed | Coordinator `--profiles-only` output |
| Invalid arguments, missing URI, no-terminal wizard, TLS conflict and help | 16 parsing/help checks passed across shell/PowerShell | Coordinator `--negative-only` output |
| Unattended installation matrix | Eight passed on 0.1.8 | `target/installer-fd9d36e9d619421eb7be5bb7972f0267/results.json` |
| Native Linux terminal wizard, including `curl | sh` | Four passed | `target/installer-30ef32cb1d924891bf52639a622e3e03/results.json` |
| Native Windows PowerShell terminal wizard, including `irm | iex` | Four passed | `target/installer-3a611b9127764e2ba2c4481e2b9301bb/results.json` |
| External verified TLS requiring client certificates | Four passed; wrong CA, client key and hostname rejected in all four | `target/installer-0089eac773d74a00821ef759f3781479/results.json` |
| Documentation/site build | Passed | `npm run docs:build` |
| Documentation/profile/generated-assets validators | Passed | Corresponding scripts |
| Cargo metadata and Helm metadata/configuration | Passed; 11 valid and 22 rejected Helm cases | Locked metadata and Helm validator |

The eight unattended cases cover shell/PowerShell times bundled Mongo, bundled
PostgreSQL, external Mongo and external PostgreSQL. Old `curl | sh` and `irm | iex`
forms are executed, along with new CLI flags. Every case exercises the shared HTTP
functional coordinator, file/secret hashes across reruns, backend-conflict rejection,
the missing-secret data guard and successful restart. Special-character database
passwords are encoded in the URI. CLI profile/port precedence is exercised.

Native shell terminal checks use Ubuntu under WSL. Because Docker integration is
disabled in that distro, a fixture-local bridge invokes the existing Windows Docker
and curl executables without changing WSL/Docker settings. The shell interpreter,
POSIX utilities, controlling terminal and wizard input run in Linux. Noninteractive
shell cases also passed through Git's shell on Windows. PowerShell terminal checks
use ConPTY. Each wizard case exercises HTTP behavior, reruns, data guards and restart.
macOS is not available for a native run; no macOS execution claim is made. Certificate
and host-mapping insertion uses portable awk instead of GNU-only sed append syntax.

Mutual TLS fixtures require a client certificate on the database server. Negative
tests run one-off application processes against the generated installation config
and individually corrupt the CA, client key and hostname. All 12 reject the protected
connection without fallback; original fixture files are restored after each probe.
Fixture cleanup verifies UUID ownership before deleting containers/storage/network.
The unrelated working CSS retains SHA256
`EECCC7A218A28B124CFA5BE9A5BE01507BD6AC3BF1272ECFDD442177F3C4FCC8`.

## Explicit boundaries

No Git tag, remote push, registry publication, GitHub release or deployment is
performed. GitHub Actions definitions are updated, but hosted Actions are not claimed
to have run locally. Load/soak testing remains deferred. Existing backend functional
evidence is separate from installer delivery and is not counted as new SDK testing.
