# Deploying Syntra

A deployment is one `syntra serve` process and one persistent directory for
its store. There is no separate database, queue or control plane.
Decisions, rewards, model snapshots and the audit trail are in
`<store>/syntra.db` (SQLite), and the capsule specs and policies are files
beside it. The
process can be replaced at any time; the store has to survive it. Run one
server per store (SQLite has one writer) and put a TLS-terminating proxy in
front of it.

[operating.md](operating.md) covers what happens once it runs: the store,
durability, backups, metrics, tokens and troubleshooting.

## From source

```bash
cargo build --release          # builds target/release/syntra
export SYNTRA_ADMIN_KEY=$(openssl rand -hex 32)
./target/release/syntra serve --addr 0.0.0.0:8787 --store /var/lib/syntra
```

Keep the key somewhere safer than your shell history (a secrets manager,
an environment file readable only by the service user) and pass it through
`SYNTRA_ADMIN_KEY`, not `--admin-key`, so it does not show in the process
list. `serve` options:

| Option | Environment | Default |
|---|---|---|
| `--addr host:port` | | `127.0.0.1:8787` |
| `--store <dir>` | | `./syntra-store` |
| `--admin-key <key>` | `SYNTRA_ADMIN_KEY` (or `LYCAN_ADMIN_KEY`) | none: the server refuses to start |
| `--metrics-public` | `SYNTRA_METRICS_PUBLIC=1` | off: `/metrics` needs an admin credential |
| `--specs <dir>` | `SYNTRA_SPECS_DIR` | none: apply capsule spec files at startup ([operating.md](operating.md#specs-from-files)) |
| `--dev-mode` | | off: no authentication, loopback addresses only |
| `--dev-mode-allow-remote` | | off: allow `--dev-mode` on other addresses (an isolated container only) |
| | `SYNTRA_RATE_LIMIT_RPS`, `SYNTRA_RATE_LIMIT_BURST` | 50,000 requests/s and 100,000 burst per credential |
| | `RUST_LOG` | `info` |
| | `OTEL_EXPORTER_OTLP_ENDPOINT` and the other `OTEL_*` variables | off: OpenTelemetry tracing ([operating.md](operating.md#tracing)) |

`serve` refuses unknown options. Run the process under a supervisor that
restarts it and sends SIGTERM to stop it; on SIGTERM it drains requests
for up to 30 s, commits what is queued and snapshots the models. `syntra
status` and `syntra stop` (with `--addr` or `--port`) find the process
listening on a port, and `stop` only signals a syntra process.

## Docker

The `Dockerfile` at the repository root builds the release binary with
Rust 1.94 and copies it into `debian:bookworm-slim`, where it runs as the
unprivileged user `syntra` with the store at `/var/lib/syntra`:

```bash
docker build -t syntra .
export KEY=$(openssl rand -hex 32)
docker run -d --name syntra -p 8787:8787 -e SYNTRA_ADMIN_KEY=$KEY \
  -v syntra-data:/var/lib/syntra syntra
```

The image's entrypoint is `syntra` and its default command is
`serve --addr 0.0.0.0:8787 --store /var/lib/syntra`; any arguments you
give `docker run` replace that command, so repeat it in full when you add
an option (`serve --addr 0.0.0.0:8787 --store /var/lib/syntra --metrics-public`).
`docker-compose.yml` does the same with a named volume and reads the key
from `SYNTRA_ADMIN_KEY` in the environment or `.env`
(`cp templates/env.example .env`, then set the key). The image declares
`/var/lib/syntra` as a volume and has a health check (`syntra health`), so
`docker ps` shows whether the server answers.

The release workflow (`.github/workflows/release.yml`) builds this image
for linux/amd64 and linux/arm64 and pushes it as
`ghcr.io/<owner>/syntra:<tag>` on a `v*` tag, with Linux and macOS
binaries, Python wheels, an SBOM and checksums. Until a release has been
published, build the image yourself.

## Kubernetes

The Helm chart is in [`deploy/helm/syntra`](../deploy/helm/syntra/); its
[README](../deploy/helm/syntra/README.md) lists every value. It deploys one
replica of the server image (`ghcr.io/ashhart/syntra:v<appVersion>` unless
you set `image.repository` and `image.tag`) with:

- the command line `serve --addr 0.0.0.0:8787 --store /syntra/data`;
- the admin key from a Secret (generated, given with
  `syntra.adminToken`, or your own with `syntra.existingSecret`, key
  `adminToken`) in `SYNTRA_ADMIN_KEY`;
- a `ReadWriteOnce` PersistentVolumeClaim for the store and the `Recreate`
  strategy, so two pods never open the same store;
- `/health` as the liveness probe and `/ready` (the store is writable) as
  the readiness probe;
- optionally, capsule spec files from the `capsules` value, mounted from a
  ConfigMap and applied with `--specs` at startup;
- optionally, an Ingress and a Prometheus Operator ServiceMonitor that
  sends the admin key (or none, with `syntra.metricsPublic`).

With `router.yaml` holding a spec document (the format is in
[operating.md](operating.md#specs-from-files)):

```bash
helm install syntra deploy/helm/syntra --namespace syntra --create-namespace \
  --set image.repository=registry.example.com/syntra --set image.tag=v0.2.0 \
  --set-file 'capsules.router\.yaml=router.yaml'
kubectl -n syntra get secret syntra-admin -o jsonpath='{.data.adminToken}' | base64 -d
```

`replicaCount` is 1 (or 0), and the chart refuses more, or autoscaling:
one store has one server, and a second server on the same store refuses
to start. More replicas would need more than a shared volume, since
SQLite on a network file system with several writers is not safe.

## TLS and network placement

Syntra serves plain HTTP. Terminate TLS in front of it (Caddy, nginx,
Traefik, Envoy, your cloud load balancer or ingress controller) and keep
the server's port reachable only from that proxy and from the services
that call it. `scripts/demo-tls-gateway.py` runs the server behind a TLS
terminator and checks that a wrong CA, a wrong hostname and plain HTTP are
refused.

- Applications need `/v1/tenants/...` (and `/personalizer/v1.0/...` for
  Personalizer clients). Give each one a `read` token for its capsule.
- `/metrics` needs an admin credential unless `--metrics-public`; scrape it
  from inside the network.
- `/health` and `/ready` are open, for probes.
- `/admin` is a static page; everything it shows comes from API calls made
  with the key you type into it.

Do not expose Syntra to the public internet. It has had no external
security review ([SECURITY.md](../SECURITY.md)).

## Sizing

- **CPU.** A decide takes tens of microseconds on the server (the README
  has measurements with their hardware); evaluations are the heavy
  requests, and at most two run at once.
- **Memory.** Each loaded capsule keeps a dense model of about 12 bytes
  per hash slot, 3 MiB at the default `learner.bits` of 18. Capsules that
  SDKs decide on locally also keep their recently published models.
- **Disk.** In the quickstart, a decision with its reward took about 0.7 KB
  of `syntra.db` (two context fields, two actions); larger contexts and
  per-request action lists take more. Syntra deletes nothing on its own;
  `DELETE .../logs` erases a capsule's decisions and rewards.

## Upgrades

Take a backup (`syntra backup`), stop the server, replace the binary or
image, start it. `store.json` records the store format; a server
refuses a format it does not read, and refuses v1 stores, whose logs have
no propensities. SDK deciders keep deciding through the restart and pick
up the rebuilt model on their next sync.
