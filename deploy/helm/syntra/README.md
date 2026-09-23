# Syntra Helm chart

Deploys one Syntra server with a PersistentVolumeClaim for its store. The
server is the image built from the repository's `Dockerfile`; the release
workflow publishes it as `ghcr.io/<owner>/syntra:v<version>` on a `v*` tag.
Until a release exists, build and push your own image and set
`image.repository` and `image.tag`.

[docs/deployment.md](../../../docs/deployment.md) covers deployment in
general and [docs/operating.md](../../../docs/operating.md) running it.

## Requirements

- Kubernetes 1.24 or later and Helm 3 or later.
- A StorageClass that provisions `ReadWriteOnce` volumes (the cluster
  default usually does).
- For `serviceMonitor.enabled`: the Prometheus Operator CRDs.

## Install

```bash
helm install syntra deploy/helm/syntra --namespace syntra --create-namespace \
  --set image.repository=registry.example.com/syntra --set image.tag=v0.2.0 \
  --set syntra.adminToken="$(openssl rand -hex 32)"

kubectl -n syntra rollout status deploy/syntra
kubectl -n syntra port-forward svc/syntra 8787:8787
curl -s http://localhost:8787/health
export KEY=$(kubectl -n syntra get secret syntra-admin -o jsonpath='{.data.adminToken}' | base64 -d)
curl -s http://localhost:8787/v1/auth/whoami -H "Authorization: Bearer $KEY"
```

Resource names start with the release's full name (`syntra` above): the
Deployment and Service `syntra`, the Secret `syntra-admin`, the PVC
`syntra-store`.

## What it runs

- A Deployment with one replica and the `Recreate` strategy, so two pods
  never open the same store. The container's command line is
  `serve --addr 0.0.0.0:<syntra.port> --store <syntra.storePath>`, plus
  `--metrics-public`, `--specs /etc/syntra/capsules`, `--dev-mode
  --dev-mode-allow-remote` and `syntra.extraArgs` when those are set.
- The admin key in the environment variable `SYNTRA_ADMIN_KEY`, from the
  key `adminToken` of the chart's Secret or of `syntra.existingSecret`.
- `/health` as the liveness probe and `/ready` as the readiness probe
  (`/ready` fails when the store is not writable).
- The pod runs as UID and GID 1000 (with `fsGroup` 1000 so the volume is
  writable), without privilege escalation and with every capability
  dropped.
- A ClusterIP Service on port 8787, a ServiceAccount, and optionally an
  Ingress, the capsule ConfigMap and a ServiceMonitor.

## The admin key

Three ways to supply it:

1. `syntra.existingSecret`: the name of a Secret you manage (External
   Secrets, Sealed Secrets, SOPS) with the key `adminToken`. The chart
   creates no Secret.
2. `--set syntra.adminToken=...` on install and on every upgrade.
3. Neither. The chart generates 32 random characters, and new ones on
   every render, so an upgrade that does not pass the key
   replaces it and restarts the pod with the new one. Use 1 or 2 for
   anything that lives past a first test.

A change to the Secret or to the capsule ConfigMap rolls the pod (the
Deployment carries checksums of both). The key gives full access, like the
admin key of any Syntra server. Issue scoped tokens to applications
(`POST /v1/admin/tokens`, see [docs/operating.md](../../../docs/operating.md#access)).

## Capsule specs at startup

Each entry of `capsules` becomes a file in the ConfigMap
`<fullname>-capsules`, mounted read-only at `/etc/syntra/capsules` and
passed as `syntra serve --specs`:

```yaml
capsules:
  router.yaml: |
    tenant: acme
    job: prod
    capsule: router
    spec:
      actions:
        - {id: small, features: {cost: 0.1}}
        - {id: large, features: {cost: 1.0}}
      reward: {default: 0, waitSeconds: 600}
```

At startup each spec replaces the capsule's stored spec when it differs
(fields it leaves out take their defaults), and the audit trail records
the file name; an unchanged spec changes nothing. An invalid file stops the server,
so the pod does not become ready and its log names the file and the field.
The file wins over the API. A change made through `PUT .../spec` or
`promote` to a capsule listed here goes away at the next restart.

## Metrics

`/metrics` needs an admin credential unless `syntra.metricsPublic` is
true. With `serviceMonitor.enabled`, the ServiceMonitor scrapes `/metrics`
on the `http` port and sends the admin key from the Secret as a Bearer
credential; with `syntra.metricsPublic` (or `syntra.devMode`) it sends
none. Add the label your Prometheus selects monitors by, for example
`--set serviceMonitor.labels.release=kube-prometheus-stack`.

## Dev mode

`syntra.devMode=true` creates no Secret, sets no key and adds
`--dev-mode --dev-mode-allow-remote`. Every route is open to anyone who
can reach the pod. Use it only on a throwaway cluster.

## Values

Every value is commented in [values.yaml](values.yaml). The ones you are
most likely to set:

| Value | Default | Purpose |
|---|---|---|
| `image.repository` | `ghcr.io/ashhart/syntra` | Server image. |
| `image.tag` | `""` | Empty means `v` + the chart's `appVersion` (`v0.2.0`). |
| `imagePullSecrets` | `[]` | For a private registry. |
| `syntra.adminToken` | `""` | The admin key; see above. |
| `syntra.existingSecret` | `""` | A Secret with the key `adminToken`. |
| `syntra.metricsPublic` | `false` | Serve `/metrics` without a credential. |
| `syntra.devMode` | `false` | No authentication at all. |
| `syntra.port` | `8787` | Container port. |
| `syntra.storePath` | `/syntra/data` | Where the volume is mounted and the store lives. |
| `syntra.extraEnv` | `[]` | Extra environment, e.g. `RUST_LOG`, `SYNTRA_RATE_LIMIT_RPS`, or `OTEL_EXPORTER_OTLP_ENDPOINT` to send traces to a collector. |
| `syntra.extraArgs` | `[]` | Extra `serve` options. |
| `capsules` | `{}` | Spec files applied at startup. |
| `persistence.enabled` | `true` | `false` uses an `emptyDir`: the store dies with the pod. |
| `persistence.size` | `10Gi` | PVC size. |
| `persistence.storageClass` | `""` | Cluster default when empty. |
| `persistence.existingClaim` | `""` | Use a PVC you manage instead of creating one. |
| `persistence.annotations` | `{}` | Annotations on the chart's PVC. |
| `ingress.enabled` | `false` | Render an Ingress (set `hosts` and `tls`). |
| `serviceMonitor.enabled` | `false` | Render a ServiceMonitor. |
| `resources` | 100m/256Mi requested, 1 CPU/1Gi limit | Each loaded capsule's model is about 3 MiB at the default 18 hash bits. |

Leave `replicaCount` at 1 and `autoscaling.enabled` false. The store is
SQLite with one writer. More replicas on a shared (`ReadWriteMany`)
volume would put several writers on one database over a network file
system, which is not safe.

## Upgrade

```bash
helm upgrade syntra deploy/helm/syntra --namespace syntra --reuse-values \
  --set image.tag=v0.2.1 --set syntra.adminToken="$KEY"
```

`Recreate` stops the old pod before the new one starts, so the API is
down for a few seconds; on SIGTERM the server finishes requests in
flight, commits its queue and snapshots its models. SDK deciders keep
deciding on their last model meanwhile. Take a backup first
(`syntra backup` in the pod with `kubectl exec`, copied out with
`kubectl cp`).

## Uninstall

```bash
helm uninstall syntra --namespace syntra
```

This deletes the PVC the chart created, and with most StorageClasses the
volume and the store with it. To keep the store, install with
`persistence.existingClaim` pointing at a PVC you manage, or with
`--set-string 'persistence.annotations.helm\.sh/resource-policy=keep'`,
which makes Helm leave the PVC behind.
