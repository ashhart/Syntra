# Deploying Syntra

Syntra is a single self-contained binary plus a store directory. The binary
holds the HTTP server, the Lycan graph runtime, and the adaptive learning core;
the store directory holds everything the appliance learns. There are three
supported deployment shapes: a local Docker image for evaluation and
single-host production, a Helm chart for Kubernetes, and a bare-metal install
straight from `cargo build`. Pick the shape that matches your operational
posture; the running surface is identical across all three.

For the platform overview see [`../README.md`](../README.md); for what
shipped in each phase see [`../CHANGELOG.md`](../CHANGELOG.md). The API
endpoints referenced below are documented in [`api.md`](api.md), and the
runtime concerns once it's up are in [`operating.md`](operating.md).

## What you're standing up

Syntra serves a single HTTP listener on port 8787. There is no separate
control plane, no database, and no message broker. State lives in the store
directory under the layout below; the container or host is otherwise
disposable.

```
syntra-store/
  tenants/{tenant}/jobs/{job}/capsules/{capsule}/
    current.lyc       — installed graph binary
    policy.json       — runtime capability policy
    memory.json       — learned weights, meta-bandit, calibrators, OOD detectors
    learning.json     — algorithm config (contextSpec, refusal, …)
    warmup.json       — lifecycle state (Warmup / Active / Frozen)
    audit.jsonl       — mutation log
    decision.jsonl    — decision log (carries refused flag and confidence)
    feedback.jsonl    — feedback log
    snapshots/        — pre-mutation backups
```

Backing up the appliance means backing up the store directory. Restoring it
means restoring the store directory and starting the binary against it. There
is no schema migration step — the `memory.json` reader is backward-compatible
across schema versions 2 through 7, so older stores load cleanly into newer
binaries.

## Required configuration

The only mandatory setting is the admin key. Syntra refuses to start without
one unless you explicitly pass `--dev-mode`, in which case it binds to
`127.0.0.1` only and prints a warning. Set it via environment variable:

```
LYCAN_ADMIN_KEY=<a long random secret>
```

The store root defaults to the working directory but should be set explicitly
in any deployment that survives a restart:

```
LYCAN_STORE_ROOT=/var/lib/syntra
```

Generate a real key with `openssl rand -hex 32` and feed it through whatever
secret-management story your environment uses; Syntra has no opinion about
where it comes from beyond requiring its presence. Failed bearer authentication
returns `401` and is logged with the remote address. Comparison is constant-
time, so brute-force attempts do not leak via timing.

## Local Docker

The reference image is built from `Syntra/docker/Dockerfile.demo`. It is a
multi-stage build: a Rust toolchain image compiles `syntra` from source against
the Lycan sources in the same checkout, and the runtime stage is a slim Debian
image carrying just the binary, the demo capsule, and a small traffic
generator. Build from the repository root (the directory that contains
`Lycan/` and `Syntra/`):

```bash
docker build -t syntra:demo -f Syntra/docker/Dockerfile.demo .
docker run --rm \
  -p 8787:8787 -p 8080:8080 \
  -e LYCAN_ADMIN_KEY=$(openssl rand -hex 32) \
  -v syntra-store:/var/lib/syntra \
  syntra:demo
```

Port 8787 is the API listener; port 8080 is the live dashboard included in
the demo image. The named volume `syntra-store` persists across container
restarts, image rebuilds, and upgrades — losing it means losing every learned
weight and the entire audit history, so back it up the same way you back up
any production database volume.

For a non-demo deployment, build a minimal image that runs `syntra serve`
without the dashboard or traffic generator. The demo image is the production
shape with two convenience processes added; strip them out for any environment
where the dashboard does not need to be exposed by Syntra itself (most
production deployments will fronted by an existing observability stack).

## Kubernetes via Helm

A Helm chart lives at `Syntra/deploy/helm/syntra/` (see that directory if it
is present in your checkout). The chart deploys a single-replica Syntra
StatefulSet with a PersistentVolumeClaim for the store directory, a Service
exposing port 8787, and a Secret carrying `LYCAN_ADMIN_KEY`.

```bash
helm install syntra ./Syntra/deploy/helm/syntra/ \
  --set adminKey=$(openssl rand -hex 32) \
  --set persistence.size=20Gi \
  --set image.tag=latest
```

The single-replica posture is deliberate: the store is a local filesystem and
the learner does not currently support multi-writer state, so scaling is
vertical until a clustering mode lands. For HA today, run an active capsule
with shadow-mode peers and promote on failure rather than running concurrent
writers.

If the chart directory does not exist in your checkout, fall back to the
bare-metal recipe below and wrap it in a manifest of your choosing.

## Bare-metal

For Proxmox LXC, a systemd-managed VM, or any host where Rust is acceptable:

```bash
cd Lycan/..   # the directory containing Lycan/ and Syntra/
cargo build --release --manifest-path Syntra/Cargo.toml --bin syntra
install -m 0755 Syntra/target/release/syntra /usr/local/bin/syntra

mkdir -p /var/lib/syntra
LYCAN_ADMIN_KEY=$(openssl rand -hex 32) \
  syntra serve \
    --addr 0.0.0.0:8787 \
    --store /var/lib/syntra
```

Wrap this in a systemd unit, an LXC entrypoint, or whatever supervises
long-running processes in your environment. For Proxmox LXC specifically,
bind-mount the store from the host so the directory survives container
rebuilds:

```
mp0: /mnt/data/syntra-store,mp=/var/lib/syntra
```

For a pre-built binary release (no Rust on the target host), build on a
build host, copy `syntra` and the appropriate `libc`-compatible glibc, and run
the same `syntra serve` command. The binary is self-contained at runtime; it
does not need a Lycan checkout once compiled.

## TLS, proxies, and exposure posture

Syntra serves plain HTTP. Do not expose port 8787 to the public internet.
Run it behind a TLS-terminating reverse proxy — nginx, Caddy, Traefik, or
your cloud's load balancer — and lock the proxy down to your service network.
The threat model and the path to direct-exposure hardening live in
[`../SECURITY.md`](../SECURITY.md) and are tracked in the Syntra issue
tracker; for now, treat the appliance as you would an internal datastore.

The proxy should forward `Authorization` headers untouched, preserve the
request body up to 4 MB, and keep the connection open long enough for the
slowest capsule on your installation to return a decision (default budget is
generous; capsules that call out via the HTTP capability are the slow path
to watch).

Network egress from the Syntra host should be restricted to whatever your
capsules explicitly need. Capsules with `allow_network: false` in policy
cannot reach out at all; capsules with `allow_network: true` are restricted
to their `allowed_hosts` list with SSRF protection against private ranges by
default.

## Resource sizing

A single Syntra instance handles thousands of decisions per second on a
modest VM (2 vCPU, 2 GB RAM). The dominant memory cost is the in-memory
mirror of `memory.json` for active capsules; the dominant CPU cost is the
graph executor under high `/decide` rates. For most workloads the bottleneck
is `feedback.jsonl` fsync throughput, which is the limiting factor when you
run with `snapshotOnFeedback: true` and `journalOnFeedback: true`. Switch the
capsule's `learning.json` to `"mode": "highThroughput"` to disable those at
a small durability cost, or leave them on and put the store on faster local
storage.

Set CPU and memory limits on the container or unit. Syntra does not currently
enforce its own resource ceilings; the supervisor is expected to.

## Backups

The store is the entire backup target. `cp -r` of the store root, a volume
snapshot, or a `restic backup` against the store directory all produce a
restorable backup. Stop the appliance for a fully consistent snapshot, or
take a volume-level snapshot (LVM, ZFS, EBS) for a live backup with point-in-
time consistency.

Restore is symmetric: stop the appliance, replace the store, start the
appliance against the restored path. There is no schema migration step.

A first-class HTTP backup endpoint is planned for Phase 1E; see
[`api.md`](api.md) for the current state.

## Upgrades

Upgrade by replacing the binary or container image. The `memory.json` schema
reader is backward-compatible from version 2 through 7, so a newer Syntra
binary will read an older store without intervention. Roll forward by
shutting down the appliance, swapping the binary, and starting against the
same store. Take a backup first.

There is no documented downgrade path. If you must roll back, restore from a
backup taken before the upgrade.
