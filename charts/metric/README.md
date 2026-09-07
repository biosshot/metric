# Metric Helm chart

Installs one Metric process, optional standalone MongoDB, persistent BlobStore and
optional Symbolicator. **Chart version = Metric version = Docker image tag.** The
first chart starts at the current Metric version and every Metric release includes
its matching chart.

Read the [Kubernetes guide](https://biosshot.github.io/metric/kubernetes) for the
credential setup, resource profiles, external MongoDB, S3, Ingress, updates and
retained-data reinstall. The source guide is `docs/kubernetes.md` in the repository.

Create the `metric` namespace and a `metric-secrets` Secret containing a URL-safe
`mongo-password` and a 64-character hexadecimal `scrub-hmac-key`, then install the
matching published chart:

```bash
helm install metric oci://ghcr.io/biosshot/charts/metric \
  --version 0.1.6 --namespace metric --wait --timeout 10m
kubectl --namespace metric port-forward service/metric 4001:4001
```

Before the initial OCI publication, build and load the checkout's image into a test
cluster, then use `helm install metric ./charts/metric --namespace metric
--set image.repository=metric-helm-test --set image.pullPolicy=Never --wait --timeout 10m`.
Do not assume an image or chart is available just because its source is merged;
the first public chart ships with Metric 0.1.6.

Default profile: Min. Application limits and resource ceilings come from the
existing Compose profiles. Kubernetes 1.32+ and Helm 3.19+ are required; a StorageClass
or pre-provisioned PVCs must be available. `values.yaml` documents deployment options
and `values.schema.json` rejects unsupported combinations.

One Metric replica and `Recreate` updates are mandatory. Readiness remains closed
during automatic migration. Do not automatically roll back images after a schema
migration. Normal Services exclude unready pods; use a direct pod port-forward or
logs to inspect migration progress.

Data PVCs and operator-owned Secrets survive `helm uninstall`. Preserve them together
and select `persistence.existingClaim` and `mongodb.persistence.existingClaim` when
reinstalling. Deleting a namespace can still delete its data. Helm is not a backup.

Checks: `python scripts/validate-helm-chart.py --image <locally-built-image>` and
`python scripts/test-helm-cluster.py --context <disposable-kind-context>
--image-repository metric-helm-test`. The cluster gate verifies first setup,
authentication, persistent data, upgrade and uninstall/reinstall.
