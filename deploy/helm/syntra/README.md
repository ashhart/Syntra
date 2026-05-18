# Syntra Helm chart

Deploys a single-instance Syntra HTTP appliance into a Kubernetes
cluster, backed by a `PersistentVolumeClaim` for its on-disk store.

## Table of contents

- [Prerequisites](#prerequisites)
- [Installation](#installation)
  - [With Helm CLI](#with-helm-cli)
  - [With a Helm operator (Argo CD / Flux)](#with-a-helm-operator-argo-cd--flux)
- [Auth tokens](#auth-tokens)
- [Prometheus](#prometheus)
- [Custom capsules at startup](#custom-capsules-at-startup)
- [Configuration reference](#configuration-reference)
- [Upgrade](#upgrade)
- [Uninstall](#uninstall)
- [Known limitations](#known-limitations)

## Prerequisites

- Kubernetes 1.24+
- Helm 3.8+
- A `StorageClass` capable of provisioning `ReadWriteOnce` volumes
  (the cluster default is almost always fine)
- For `serviceMonitor.enabled=true`: the Prometheus Operator
  `monitoring.coreos.com/v1` CRD installed

## Installation

### With Helm CLI

The admin bearer token is required; the chart refuses to template
without one unless `syntra.devMode=true` or you pass an existing
secret.

```bash
helm install syntra ./deploy/helm/syntra \
  --namespace syntra \
  --create-namespace \
  --set syntra.adminToken="$(openssl rand -hex 32)"
```

Verify the install:

```bash
kubectl -n syntra rollout status deploy/syntra
kubectl -n syntra port-forward svc/syntra 8787:8787
curl -s http://localhost:8787/health
```

Pin a specific Syntra image version (recommended for production):

```bash
helm install syntra ./deploy/helm/syntra \
  --namespace syntra \
  --create-namespace \
  --set image.tag=0.2.3 \
  --set syntra.adminToken="$(openssl rand -hex 32)"
```

### With a Helm operator (Argo CD / Flux)

This chart has no custom CRD or webhook dependencies, so it works
unchanged under either operator. The only operator-specific concern
is keeping the admin token out of git.

**Argo CD** — point an `Application` at the chart path and mount the
admin token through `ExternalSecrets`/`SealedSecrets`. Then set
`syntra.existingSecret` to the resulting `Secret` name. The chart
skips its own Secret template when that field is non-empty.

```yaml
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: syntra
spec:
  destination:
    namespace: syntra
    server: https://kubernetes.default.svc
  project: default
  source:
    repoURL: https://github.com/ashhart/Syntra
    path: deploy/helm/syntra
    targetRevision: main
    helm:
      values: |
        image:
          tag: "0.2.3"
        syntra:
          existingSecret: syntra-admin-external
  syncPolicy:
    automated:
      prune: true
      selfHeal: true
```

**Flux** — a `HelmRelease` referencing a `GitRepository` that points
at the chart path. Same pattern: `syntra.existingSecret` to a Secret
managed by `external-secrets` or `sops`.

```yaml
apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: syntra
  namespace: syntra
spec:
  interval: 5m
  chart:
    spec:
      chart: ./deploy/helm/syntra
      sourceRef:
        kind: GitRepository
        name: syntra
        namespace: flux-system
  values:
    image:
      tag: "0.2.3"
    syntra:
      existingSecret: syntra-admin-external
```

## Auth tokens

Syntra requires a bearer token on every route except `/health`. The
token is read from the env var `LYCAN_ADMIN_KEY` inside the
container — this name is fixed by the binary (`Syntra/src/lib.rs`).
The chart's value is named `syntra.adminToken` and the rendered Secret
key is `adminToken`; the Deployment template maps both through to the
correct env var.

There are three supported ways to supply the token, in order of
preference for production:

1. **External secret manager** — set `syntra.existingSecret` to the
   name of a pre-existing `Secret` containing key `adminToken`. The
   chart will not create its own Secret. Use this with
   `external-secrets`, `sops`, `SealedSecrets`, `vault-secret-injector`,
   etc.

2. **`--set` at install time** — pass `--set syntra.adminToken=…` on
   the `helm install` / `helm upgrade` command line. The token lives
   in Helm's release storage (also a Secret, encrypted by default in
   Helm 3), not in plain values.yaml on disk.

3. **`-f values.yaml`** — only for ephemeral dev clusters. Do not
   commit a values file containing a real admin token to git.

Rotate by running `helm upgrade` with a new value. The Deployment
template carries a `checksum/secret` annotation, so the pod rolls
automatically when the Secret content changes.

## Prometheus

Syntra exposes Prometheus text on `/metrics` unauthenticated on the
same port as the HTTP API (`8787` by default).

If you run the Prometheus Operator (kube-prometheus-stack or
equivalent), enable the bundled `ServiceMonitor`:

```bash
helm upgrade syntra ./deploy/helm/syntra \
  --reuse-values \
  --set serviceMonitor.enabled=true \
  --set serviceMonitor.labels.release=kube-prometheus-stack
```

The `release: kube-prometheus-stack` label is the default
`serviceMonitorSelector` Prometheus uses; check your install's
`Prometheus` resource for the exact selector if scraping does not
appear.

For a vanilla Prometheus (no operator), add a static scrape
config pointing at the Service DNS name:

```yaml
scrape_configs:
  - job_name: syntra
    static_configs:
      - targets: ['syntra.syntra.svc.cluster.local:8787']
```

## Custom capsules at startup

The chart accepts capsule definitions through `values.capsules`. Each
key becomes a file inside a `ConfigMap`, mounted at
`/etc/syntra/capsules` (read-only) inside the container. The mount
is created if and only if `capsules` is non-empty.

```yaml
# my-values.yaml
capsules:
  router.yaml: |
    name: router
    options:
      - cheap_fast
      - balanced
      - expensive_accurate
    reward:
      type: continuous
      range: [-1.0, 1.0]
  router.learning.json: |
    {
      "contextSpec": {"type": "discrete"},
      "refusal": {"enabled": false}
    }
```

Then either:

- Build a custom image whose entrypoint reads `/etc/syntra/capsules/*`
  after `syntra serve` becomes ready (the demo image's
  `entrypoint.sh` shows the install-via-curl pattern), or
- Run `syntra author` and `curl … /install` from a one-shot Job that
  references the same `ConfigMap`.

The chart only provides the delivery mechanism; the install timing is
left to your image so a stock release image stays unopinionated. See
`Syntra/docker/demo/capsule/install.py` for a working installer.

## Configuration reference

See [`values.yaml`](./values.yaml) — every value is documented inline.
The high-traffic knobs:

| Value | Default | Purpose |
| --- | --- | --- |
| `image.repository` | `ghcr.io/ashhart/syntra` | Image repo. |
| `image.tag` | `demo` | Image version. Pin to a real release for prod. |
| `syntra.adminToken` | `""` | Required unless `existingSecret` set. |
| `syntra.port` | `8787` | Container HTTP port. |
| `syntra.storePath` | `/syntra/data` | On-disk store mount path. |
| `persistence.size` | `10Gi` | PVC size. |
| `persistence.accessMode` | `ReadWriteOnce` | RWO; see HPA notes. |
| `service.type` | `ClusterIP` | Service exposure. |
| `ingress.enabled` | `false` | Render the Ingress resource. |
| `serviceMonitor.enabled` | `false` | Render the ServiceMonitor CRD. |
| `autoscaling.enabled` | `false` | Render the HPA. **See limitations.** |

## Upgrade

```bash
helm upgrade syntra ./deploy/helm/syntra \
  --namespace syntra \
  --reuse-values \
  --set image.tag=0.2.4
```

The Deployment's update strategy is `Recreate`, not `RollingUpdate`,
because the RWO PVC cannot be bound to two pods simultaneously. There
will be a few seconds of API downtime during upgrade. Schedule it
accordingly.

## Uninstall

```bash
helm uninstall syntra --namespace syntra
```

The PVC survives by default (Helm policy). Delete it explicitly if
you want a clean slate:

```bash
kubectl -n syntra delete pvc syntra-store
```

## Known limitations

- **Single-instance store.** Syntra's on-disk store is single-writer.
  The default PVC is `ReadWriteOnce` and the Deployment strategy is
  `Recreate` for that reason. Running >1 replica against the same RWO
  PVC will not schedule; against a shared RWX PVC it works at the
  filesystem level but has no inter-replica coordination.

- **No clustering.** The binary has no leader election, no
  log-segment ownership, no consensus on the meta-bandit reward
  distributions. Running >1 replica against a shared RWX volume
  works (atomic appends prevent line corruption) but produces a
  slightly different learning trajectory than the single-replica
  case. For audit-grade traces, stay at one replica.

- **HPA off by default.** Horizontal autoscaling only makes sense
  with RWX storage AND tolerance for the learning-trajectory caveat
  above. The HPA template explains the trade in detail.

- **No built-in TLS.** Run Syntra behind an Ingress / proxy with
  TLS termination. The `/health` endpoint is the only unauthenticated
  route; everything else requires the bearer token.

- **Backup is your responsibility.** Snapshot the PVC on the
  cadence your `decision.jsonl` / `audit.jsonl` retention demands.
  Syntra writes pre-mutation snapshots into the store itself
  (`snapshots/` directory), but a lost PVC loses both the live state
  and those internal backups.

## License

Apache-2.0.
