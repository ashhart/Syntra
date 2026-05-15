# Syntra Deployment

## Docker (Recommended First Deployment)

Docker is the recommended way to run Syntra. It handles dependencies, isolation, and persistence out of the box.

### Quick Start

```bash
docker compose up
```

This builds the Syntra image from source and starts the server on port 8787. The final image is self-contained and does not need a local Lycan checkout at runtime. You can also run the image directly:

```bash
docker build -t syntra .
docker run -p 8787:8787 -v syntra-store:/var/lib/lycan -e LYCAN_ADMIN_KEY=your-real-key syntra
```

### Store Volume

Syntra persists all state -- weights, feedback, and audit logs -- to its store directory. In Docker, this is mounted at `/var/lib/lycan` via a named volume (`syntra-store`). Because the volume is separate from the container filesystem, data survives container restarts, image rebuilds, and upgrades.

To inspect the store contents:

```bash
docker volume inspect syntra-store
```

### Admin Key

The admin key is passed via the `LYCAN_ADMIN_KEY` environment variable. Syntra refuses to start without a key unless you explicitly use `--dev-mode`.

```yaml
environment:
  - LYCAN_ADMIN_KEY=your-real-key-here
```

Or pass it at runtime:

```bash
docker run -e LYCAN_ADMIN_KEY=your-real-key -p 8787:8787 -v syntra-store:/var/lib/lycan syntra
```

## Proxmox LXC

For bare-metal or LXC deployments (e.g., on a Proxmox host):

1. Install Rust and build from source, or copy a pre-built `syntra` binary into the container.
2. Create the store directory: `mkdir -p /var/lib/lycan`
3. Run the server:

```bash
syntra serve --addr 0.0.0.0:8787 --store /var/lib/lycan
```

4. Bind-mount the store directory from the host to ensure persistence across LXC rebuilds:

```
mp0: /mnt/data/lycan-store,mp=/var/lib/lycan
```

## Production Considerations

### What Is Production-Ready

- The core interpreter and graph executor are stable and well-tested.
- The capsule format and store persistence work reliably.
- Docker deployment is straightforward and reproducible.

### What Is Experimental

- The `serve` command and HTTP interface are new. Expect API surface changes.
- Authentication is a single shared admin key. There is no user/role model yet.
- There is no built-in rate limiting, request logging, or metrics endpoint.

### Security

- **TLS proxy**: Syntra serves plain HTTP. Run it behind a reverse proxy (nginx, Caddy, Traefik) that terminates TLS. Do not expose Syntra directly to the public internet.
- **Admin key**: Set a strong `LYCAN_ADMIN_KEY`. `/health` and the static `/admin` login shell are public. All admin data/API routes require Bearer authentication, and the browser console sends the key only after login.
- **Network egress**: Restrict outbound network access from the Lycan container. Capsules with `allow_network: false` in policy cannot make HTTP calls, but the runtime itself should be network-isolated.
- **Evolution**: Server evolution (`POST /evolve`) supports proposal mode only. The `--agent-command` subprocess mode is CLI-only and is not exposed over HTTP.
- **Backups**: Back up the `syntra-store` volume regularly. The store contains all persistent state — losing it means losing weights, feedback history, and audit logs.
- **Resource limits**: Set memory and CPU limits on the container in production.
