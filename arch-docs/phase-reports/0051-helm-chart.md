# Helm chart implementation evidence

- Date: 2026-09-05
- Branch: `feat/helm-chart`, recreated from `main` at `9ad9033`
- Issue: https://github.com/biosshot/metric/issues/4
- Decision: [ADR-0051](../0051-helm-chart-and-release-versioning.md)
- Metric / image tag / chart version / appVersion: `0.1.5`

## Scope

Added chart manifests, generated deployment profiles, operator documentation,
configuration validation, disposable-cluster tests and OCI release integration.
No Rust source, application behavior, database schema or migration was changed.

## Local verification

- Built `metric-helm-test:0.1.5` from the current Dockerfile, including the Web UI.
- Helm 3.19.0 and Helm 4.2.4: strict lint and 11 rendered configurations accepted by
  the actual Metric binary using `--check-config`; 22 invalid values rejected.
- Kind 0.27.0 / Kubernetes 1.32.2: API server dry-run validation of all profiles,
  external MongoDB, S3 and Ingress manifests.
- Real chart installation: readiness, in-cluster Service access, bundled Web page,
  first-account bootstrap, login and project creation.
- Configuration upgrade: a new Metric pod retained the account, project, BlobStore
  volume sentinel and operator-owned Secret.
- Uninstall/reinstall: both PVC identities and Secret identity were retained;
  selecting the existing claims restored access to the account, project and sentinel.
- The lifecycle was exercised with both Helm 3 and Helm 4. Local MongoDB used the
  pinned official `mongo:8.0.12` image; no user database was used.
- The final Helm 3 lifecycle run also enabled the pinned Symbolicator `26.6.0`:
  its healthcheck and cleanup sidecar passed alongside install, upgrade and retained
  reinstall (`scripts/test-helm-cluster.py --with-symbolicator`).
- OCI round-trip through an isolated loopback `registry:2.8.3`: first publication,
  anonymous pull and identical retry without overwrite. Helm 4 also accepted the
  chart packaged by Helm 3 without a content change.
- Eight isolated publication tests cover immutable content, permission/network
  failures, missing blobs, private packages and anonymous content mismatch.
- Deployment-profile and documentation validators passed; the VitePress site built.
- `actionlint` 1.7.7 accepted both changed workflows. ShellCheck/Pyflakes were not
  included in that invocation. `git diff --check` passed.

## Remaining verification boundary

The initial Symbolicator download failure was traced to saved GHCR credentials.
An empty Docker configuration still auto-detected Windows `wincred`, so the initial
attempt described as anonymous was not actually isolated. An explicit empty
`auths["ghcr.io"]` entry disabled native-store detection; the same official `26.6.0`
image downloaded successfully and reported version `26.6.0`, commit `3cf2c504`.
No user credentials or global Docker settings were changed. The chart publication
check likewise disables native-store detection in both its isolated Helm and Docker
configurations before checking anonymous access.

External MongoDB/S3/Ingress alternatives received configuration and manifest
validation, not independent end-to-end infrastructure tests. The chart's database
test used bundled MongoDB; it does not replace existing migration regression tests.

No chart/image was published to GHCR, no GitHub Release was created, and no issue
was closed. Public installation still requires the release workflow and the
one-time GHCR package visibility/access setup documented in `docs/kubernetes.md`.
The temporary cluster, registry, fixture data and image-transfer archive were removed
after verification. The locally built Docker images remain cached for further tests.
