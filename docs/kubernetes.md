# Kubernetes and Helm

The Metric chart deploys one application instance, an optional standalone MongoDB,
and persistent file storage. An existing MongoDB, S3-compatible storage and an
Ingress can be selected. The chart uses the same Min, Low, Medium and High
application profiles as Docker Compose; Min is the chart default.

The chart version, application version and exact Docker image tag are always the
same: chart **0.1.5** selects Metric **0.1.5**. There is no separate chart release
sequence. If Metric jumps to 2.0.0, its chart also becomes 2.0.0.

## Prerequisites

- A Kubernetes cluster (manifests require Kubernetes 1.32 or newer).
- Helm 3.19 or newer and `kubectl` configured for that cluster.
- A default StorageClass, an explicitly selected class, or existing persistent
  claims for MongoDB and local BlobStore.
- Access to the Metric and MongoDB container registries.

The deployment test uses Kind with Kubernetes 1.32.2. Kubernetes has its own CPU and
memory overhead: the Min Compose profile is not a claim that a complete Kubernetes
cluster fits in 1 GiB.

## Create installation credentials

For the bundled MongoDB, create one operator-owned Secret. This Bash example uses
OpenSSL to generate URL-safe MongoDB credentials and a 32-byte hexadecimal HMAC key:

```bash
kubectl create namespace metric
kubectl --namespace metric create secret generic metric-secrets \
  --from-literal=mongo-password="$(openssl rand -hex 24)" \
  --from-literal=scrub-hmac-key="$(openssl rand -hex 32)"
```

Keep this Secret with the database and blob backups. The chart references it but
does not create, rotate or delete it. Reusing retained data requires the same keys.
Do not generate a new Secret when upgrading or reinstalling existing data. The
`mongo-password` must contain URL-safe characters, without spaces or line endings,
because the chart uses it in the internal MongoDB connection URI.

## Install

After the matching release has published the public OCI package:

```bash
helm install metric oci://ghcr.io/biosshot/charts/metric \
  --version 0.1.5 --namespace metric \
  --set mongodb.enabled=true \
  --wait --timeout 10m
```

The chart is included in source before its first registry publication. To test a
checkout before that publication, replace the OCI URL with `./charts/metric` and
omit `--version`. Build and load the checkout's image into the test cluster, then
select it with `--set image.repository=metric-helm-test --set image.pullPolicy=Never`
(see chart development below). Unreleased source may differ from an already
published image carrying the current Cargo version. Do not infer package availability
from a merged source change; the first public chart ships with the next Metric release.

Open the UI through a local tunnel:

```bash
kubectl --namespace metric port-forward service/metric 4001:4001
```

Open `http://localhost:4001`. Retrieve the one-time bootstrap token from the Metric
container logs and follow [First setup](first-setup.md):

```bash
kubectl --namespace metric logs deployment/metric --container metric
helm test metric --namespace metric
```

Logs from a fresh installation contain the bootstrap token; keep them private.

## Configuration

Keep your overrides in a `my-values.yaml` file and use the same file for upgrades.
The chart validates unknown deployment values and incompatible settings before
installation. Application configuration is also validated by Metric at startup.

```yaml
profile: low
secrets:
  existingSecret: metric-secrets
persistence:
  size: 20Gi
mongodb:
  persistence:
    size: 30Gi
config:
  retention:
    events_days: 60
```

```bash
helm install metric oci://ghcr.io/biosshot/charts/metric \
  --version 0.1.5 --namespace metric -f my-values.yaml \
  --wait --timeout 10m
```

`config` merges application settings over the selected profile. Credentials use
existing `{ env = "NAME" }` references and `extraEnv` entries with `secretKeyRef`.
Do not put literal credentials in `config`, Helm values or `--set` arguments.
The listener, role, MongoDB URI, HMAC key and BlobStore/Symbolicator connections
are owned by the chart's deployment settings and cannot be overridden in `config`.

Changing application configuration changes the pod's configuration checksum and
restarts Metric. Updating an externally owned Secret does not automatically restart
the pod. Coordinate credential rotation separately; an ordinary chart upgrade does
not change MongoDB's existing users or passwords.

`resources` and `mongodb.resources` merge with the profile's resource settings.
WiredTiger cache follows the same profile unless `mongodb.cacheSizeGB` is set.
Limits are ceilings; requests are reservations used by the Kubernetes scheduler.

### Local storage

Blob PVC capacity defaults to the profile's configured 5/10/33/83 GiB. A larger
PVC does not itself increase Metric's storage quota: use `config.blob.capacity`
and `config.blob.reserve` when changing that quota. A new PVC smaller than the
configured quota is rejected. Existing claims must also provide sufficient space.

`persistence.storageClass` and `mongodb.persistence.storageClass` have three meanings:
omitted/null selects the default class, an empty string requests a pre-provisioned
volume without a class, and a nonempty name selects that class. Volume expansion
depends on the storage driver; never shrink an existing claim or switch a live
installation to a different data volume as an ordinary upgrade.

### External MongoDB

Pre-create the selected Secret with `mongodb-uri` and `scrub-hmac-key`. The URI may
point to a standalone server or an operator-managed replica set. Then set:

```yaml
mongodb:
  enabled: false
  database: metric
secrets:
  existingSecret: metric-external-secrets
```

The chart then creates no MongoDB workload or database PVC. `mongodb.database`
selects the application database independently of the URI path.

### S3-compatible BlobStore

Create the bucket first and provide `s3-access-key-id` and `s3-secret-access-key`
in an existing Secret. Select S3 explicitly:

```yaml
blob:
  backend: s3
  s3:
    endpoint: https://minio.example.com
    region: us-east-1
    bucket: metric
    forcePathStyle: true
    existingSecret: metric-s3-secrets
```

Omit `endpoint` for AWS S3. Optional temporary credentials use
`blob.s3.sessionToken: true` and the `s3-session-token` Secret key. The chart creates
no local BlobStore PVC in this mode and does not install MinIO. Selecting S3 on an
existing local installation does not copy its objects; move data separately with
an agreed migration procedure before changing backends.

### Ingress and HTTPS

The cluster must already have an Ingress controller and, when used, a TLS Secret.
The chart does not install a controller or certificate manager.

```yaml
ingress:
  enabled: true
  className: nginx
  host: metric.example.com
  tls:
    - hosts: [metric.example.com]
      secretName: metric-tls
http:
  secureCookies: true
  # Use the actual address range of your trusted proxy, not every client network.
  trustedProxies: [10.42.0.0/16]
```

Set any controller-specific upload-size and timeout annotations in
`ingress.annotations` to accommodate the selected Metric ingest limits. Configure
the proxy to replace incoming forwarding headers. Use the public HTTPS hostname
when entering SDK DSNs.

### Symbolicator

Medium/High enable the bundled Symbolicator and a cache-cleanup container in one
pod. Their rebuildable cache uses a size-bounded `emptyDir` and disappears when that
pod is replaced. The pinned image is `ghcr.io/getsentry/symbolicator:26.6.0`, the same
release named by the Compose contract. Symbolicator uses its separate third-party
license; see [the third-party notice](https://github.com/biosshot/metric/blob/main/THIRD_PARTY_NOTICES.md).

The internal Symbolicator Service is not exposed by the Metric Ingress. Its config
permits private-network callbacks so it can retrieve signed debug-file URLs from
Metric. Do not expose Symbolicator directly to untrusted callers.

`symbolicator.enabled` explicitly overrides profile selection. An existing service
can be used with `symbolicator.enabled: true` and
`symbolicator.externalEndpoint: http://symbolicator.example.com:3021/symbolicate`.
That service must be able to resolve and reach Metric's cluster Service for debug
file callbacks. Otherwise use the bundled instance or arrange that connectivity.

## Upgrade and migration progress

Choose the new Metric/chart version together and read [Update Metric](upgrading.md).
Keep MongoDB, BlobStore and installation Secrets together in your backup procedure.

```bash
helm upgrade metric oci://ghcr.io/biosshot/charts/metric \
  --version <new-metric-version> --namespace metric \
  -f my-values.yaml --wait --timeout 20m
```

An `image.tag` override is allowed only when it equals the chart version. Prefer
leaving it empty so the chart selects its own version. `image.repository` can point
to a mirror with that same tag.

Updates use `Recreate`: the old pod terminates before its replacement starts.
Exactly one Metric replica is supported; there is no autoscaling or rolling upgrade.
Do not force-delete a pod on an unreachable node and start another writer against
the same database: Recreate is an update strategy, not storage fencing or HA.

Automatic migrations run inside Metric. `/live` remains HTTP 200, `/ready` stays
HTTP 503, and the pod becomes ready after schema verification and worker startup.
The normal Service excludes unready pods, so an Ingress may display its own error
page while there is no ready backend. To inspect the built-in progress page:

```bash
kubectl --namespace metric get pods -l app.kubernetes.io/component=server
kubectl --namespace metric logs <metric-pod-name> --container metric --follow
kubectl --namespace metric port-forward pod/<metric-pod-name> 4001:4001
```

Open `http://localhost:4001` through that direct pod tunnel. A Helm wait timeout
does not undo the migration or delete data: inspect progress and keep the matching
image. Do not use Helm 3 `--atomic`, Helm 4 automatic rollback-on-failure, or
`helm rollback` across schema generations. Helm can restore manifests and images;
it cannot undo database migrations.

## Uninstall and reinstall

```bash
helm uninstall metric --namespace metric --wait
```

The chart retains its MongoDB and BlobStore PVCs with
`helm.sh/resource-policy: keep`. Operator-owned Secrets and existing claims are also
left in place. Helm release history is not a data backup.

To reinstall, keep the namespace and Secret and explicitly select the retained
claims in `my-values.yaml` (names shown for a release called `metric`):

```yaml
secrets:
  existingSecret: metric-secrets
persistence:
  existingClaim: metric-blobs
mongodb:
  enabled: true
  persistence:
    existingClaim: metric-mongodb
```

Then install the version compatible with the stored schema. Do not delete the
namespace: Kubernetes can delete its PVCs and Secrets regardless of Helm's retention
annotation. PVC retention also does not protect against storage-provider loss.

## Chart development and publication

Source lives in `charts/metric`. To refresh the packaged profiles after editing
Compose profiles, run `python scripts/sync-helm-profiles.py`. CI checks that those
generated files remain identical to their source configurations and resource limits.

```bash
python -m pip install -r scripts/helm-requirements.txt
python scripts/validate-helm-chart.py
python scripts/test-helm-publish.py
docker build -t metric-helm-test:0.1.5 .
python scripts/validate-helm-chart.py --image metric-helm-test:0.1.5
```

`scripts/test-helm-cluster.py` requires an explicitly selected disposable `kind-`
context and a locally loaded image. It creates a unique test namespace, exercises
install, authenticated API data, upgrade and retained reinstall, and deletes its
fixture namespace afterward. It never selects your current cluster implicitly.
The script also performs Kubernetes API dry-run validation for the other profiles,
external MongoDB, S3 and Ingress. CI passes `--with-symbolicator` to exercise the
pinned Symbolicator and cleanup containers as well.

The release workflow checks the Git tag, Cargo version, `Chart.version` and
`Chart.appVersion`. It publishes the image first, then packages and pushes the chart
to `oci://ghcr.io/biosshot/charts/metric`, and finally creates the GitHub Release.
Chart publication runs for every release, including releases with no chart changes.
An existing version with different chart contents is rejected.

GHCR setup is performed once by the package owner: connect the chart package to this
repository, grant its Actions workflow access, and make the package public. The
publication job verifies an anonymous pull. If the first push creates a private
package, change its visibility and rerun the job; an identical package is reused.
The release is not reported successful while anonymous installation is unavailable.

Some registries return `denied` instead of `not found` for a package that has never
existed. The job stops on that ambiguity; it does not assume a permission failure
means it is safe to overwrite a release. For first publication only, the package
owner can confirm that `charts/metric` does not exist, validate the exact release
checkout, and bootstrap the package using an authenticated Helm client:

```bash
python scripts/validate-helm-chart.py --release-tag v0.1.5
helm package charts/metric --destination target/helm-package
helm push target/helm-package/metric-0.1.5.tgz oci://ghcr.io/biosshot/charts
```

Then link the package, make it public and rerun the release job. Never use this
bootstrap push to replace an existing version. Registry behavior and initial
visibility are described in [GitHub's container registry documentation](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry).
