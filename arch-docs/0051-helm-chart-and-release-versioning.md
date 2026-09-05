# ADR-0051: Helm chart and unified release versioning

- Status: Accepted
- Date: 2026-09-05
- Issue: https://github.com/biosshot/metric/issues/4
- Extends: ADR-0001, ADR-0035 and ADR-0049 deployment contracts

## Decision

The repository ships `charts/metric` as a versioned Kubernetes installation of the
existing `role = "all"` binary. It owns manifests, configuration, documentation and
deployment verification. It adds no runtime adapter, database field or migration.

The Cargo package version, exact Metric image tag, `Chart.version` and
`Chart.appVersion` are identical. The first chart starts at the current Metric
version, without an independent chart history. Every subsequent Metric release
publishes that version of the chart even when its templates have not changed.
An explicitly supplied Metric image tag must also match the chart version; a
repository override supports mirrors and local testing without changing this rule.

Chart source stays in this repository. GitHub Actions validates version agreement,
packages the chart and publishes it to `oci://ghcr.io/biosshot/charts/metric` after
the matching application image succeeds. The GitHub release waits for both artifacts.
Chart publication has no path filter. Published versions are immutable; fixes use
the next Metric version. A public package and a verified anonymous OCI pull are
required before claiming internet installation works.

## Runtime and state

- Exactly one Metric replica uses a Deployment with `Recreate`. Multiple active
  Metric processes and mixed-version rolling updates remain unsupported.
- Startup and liveness probes use `/live`; readiness uses `/ready`. Migration does
  not fail liveness and has no overall probe deadline based on readiness.
- Services exclude unready pods. During migration the ordinary Ingress may show its
  own unavailable response; logs and a direct pod port-forward expose progress.
- Shutdown allows the application its configured grace period. Updating waits for
  the old pod to terminate before starting the new one. Recreate is an upgrade
  strategy, not fencing against a partitioned node or a forcibly deleted pod.
- Default storage is local BlobStore on a retained PVC. An existing PVC or external
  S3-compatible bucket can be selected. Uninstall does not delete data PVCs.
- MongoDB is either one optional bundled standalone StatefulSet or an external
  database supplied through a Secret. Bundled MongoDB uses the same pinned official
  image and WiredTiger resource limits as Compose. It does not provide HA.
- Existing Kubernetes Secrets supply installation credentials. Chart rendering never
  generates or rotates passwords or the HMAC key. Secrets survive Helm uninstall
  because the operator owns them. Reusing retained data requires the same secrets.
- Min/Low/Medium/High configuration and resource ceilings are generated from the
  existing deployment profiles, with a check against drift. Medium/High include
  optional Symbolicator and bounded cache cleanup; external Symbolicator is allowed.
- Service and optional Ingress expose HTTP. TLS termination, certificates, storage
  provisioning, database administration and backup/restore remain operator-owned.

The chart accepts bounded deployment settings and application configuration overrides.
It reserves wiring fields such as listener, role, database credentials and BlobStore
backend so that configuration cannot silently disconnect probes or persistence.
Application secrets are referenced by environment name, not stored in ConfigMaps.
Metric uses a non-root numeric security context, read-only root filesystem, dropped
capabilities, a writable bounded temporary volume and no Kubernetes API token.

Helm rollback changes manifests/images, not database contents. Automatic rollback
after migration and downgrade are not supported. Helm wait timeouts do not undo a
migration; operators inspect progress and retain the matching image and chart.

## Verification

The deployment gate checks version equality, generated profile agreement, rendered
configuration with the actual binary, and Kubernetes manifests for every profile,
external MongoDB, S3, Ingress, existing claims and secrets. Invalid replica/image
settings fail before installation. A disposable Kind cluster exercises first install,
readiness, HTTP/API access, persistent data, configuration upgrade, pod replacement,
uninstall and reinstall with retained disks and secrets. CI also validates manifests
against the Kubernetes API and exercises the pinned Symbolicator and cleanup sidecar.
The release job independently checks versions, rendering and immutable publication
gates. Existing runtime regression tests remain authoritative for storage
migration internals; Helm tests verify the surrounding startup/update lifecycle.

No hot-path benchmark is required: the chart runs the unchanged binary with the
existing bounded profiles. Kubernetes itself has additional resource requirements;
the Compose 1 GiB host profile is not a 1 GiB Kubernetes cluster claim.
