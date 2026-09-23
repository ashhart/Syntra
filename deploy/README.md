# Deploying Syntra

- [`helm/syntra/`](helm/syntra/): the Helm chart. One replica of the server
  image, the store on a PersistentVolumeClaim, the admin key from a
  Secret, optional capsule spec files applied at startup, an optional
  Ingress and ServiceMonitor. Its [README](helm/syntra/README.md) covers
  installing, the admin key, spec files, metrics and upgrades.

The server image is built from the [`Dockerfile`](../Dockerfile) at the
repository root and published as `ghcr.io/<owner>/syntra:v<version>` by
the release workflow. Running from source, with Docker, and behind TLS is
in [docs/deployment.md](../docs/deployment.md).
