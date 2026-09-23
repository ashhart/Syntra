# Deploying Syntra

This directory packages Syntra for deployment: a Helm chart for
Kubernetes, a Grafana dashboard, and a single-container "try" instance.

```
deploy/
  helm/syntra/        Helm chart. Source of truth for the workload spec.
  grafana/            Dashboard for the /metrics series.
  try-instance/       Public demo instance behind Traefik.
```

## Which should I use?

| You have… | Use |
| --- | --- |
| An existing Kubernetes cluster | `helm/syntra/` directly. |
| Just trying it | `docker run syntra:demo` (see top-level README) — don't bother with K8s. |

## Image: build your own

The `ghcr.io/ashhart/syntra:*` tags referenced in older docs do not
exist yet. The chart defaults to `image.repository=syntra`,
`image.tag=demo` — the tag from `docker build -f docker/Dockerfile.demo`.

For anything beyond a single-node cluster that can load the image
directly, build and push your own:

```bash
# From the repo root:
docker build -t my-registry.example.com/syntra:0.1.0 -f docker/Dockerfile.demo .
docker push my-registry.example.com/syntra:0.1.0
```

Then pass `--set image.repository=...` and `--set image.tag=...` to Helm.

## Admin key

Syntra refuses to start without `LYCAN_ADMIN_KEY`. The chart handles this
three ways (see `helm/syntra/values.yaml`):

- `adminKey.existingSecret` — point at a Secret you manage out of band
  (the recommended approach for production).
- `adminKey.value` — inline string, written into a chart-managed Secret.
- `adminKey.generate` — chart auto-generates 32 alphanumerics on first
  install. This is the default.

To retrieve the auto-generated key:

```bash
kubectl -n <namespace> get secret <release-name> -o jsonpath='{.data.adminKey}' | base64 -d
```

## Persistence

All deployments back `/syntra/data` with a `ReadWriteOnce` PVC at 10 GiB
by default. The AWS module exposes a `enable_efs` toggle to swap to
`ReadWriteMany` (EFS) for cross-AZ durability. GCP and Azure document
the equivalent NFS-backed options (Filestore, Azure Files) — apply them
out of band and pass `persistence.existingClaim` if you need them.

## Caveats

- Syntra is single-writer. `replicaCount = 1` is the only supported shape.
  A leader-elected multi-replica mode is on the roadmap but does not
  exist today.
- Syntra is not yet hardened for direct public-internet exposure. Run it
  behind a TLS-terminating proxy or the cluster's L7 LB.
- The Helm chart uses `/health` for both liveness
  and readiness. A dedicated `/ready` endpoint is being added separately;
  switch `readinessProbe` once it ships.

## License

Apache-2.0.
