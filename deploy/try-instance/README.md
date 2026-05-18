# try.syntra.io — public demo deployment

This directory is the complete deployment artifact for the shared
public demo instance at `try.syntra.io`. One VM, one Docker stack,
two cron jobs. Drop it on a $20/mo VPS, point DNS at the box,
`docker compose up -d`, done.

The artifact builds Syntra + Lycan **from source** inside a
multi-stage Docker build — there is no dependency on a pre-published
container image. The build context is the Lycan repo root (the
directory containing `Lycan/` and `Syntra/`).

The artifact assumes:

- you have a `try.syntra.io` (or equivalent) DNS zone you control;
- you have a place to receive webhook alerts (Slack incoming webhook,
  Discord webhook, or any HTTP gateway);
- you accept that this is an **untrusted, anonymous demo** — every
  capsule's state is wiped at 00:00 UTC daily and the API runs in
  dev-mode (no auth on any route). Don't post anything here you
  wouldn't put on a billboard.

---

## File tree

```
try-instance/
├── Dockerfile             # multi-stage: builds syntra+lycan from source
├── docker-compose.yml     # traefik + syntra services
├── entrypoint.sh          # runs `syntra serve --dev-mode` + install.py
├── .env.example           # copy to .env, fill in
├── landing.html           # static landing page served at /
├── reset.sh               # daily wipe (run via cron)
├── monitor.sh             # five-minute health probe (run via cron)
├── DEPLOY.md              # exact copy-paste commands for first deploy
└── README.md              # this file
```

The five flagship capsule sources are baked in from `Syntra/examples/`
at build time — there is no local mirror in this directory.

---

## Operator decision checklist

Before running `docker compose up -d`, decide:

| Decision | Recommendation | Where it goes |
|----------|----------------|---------------|
| **DNS hostname** | `try.syntra.io` (or any subdomain you own) | A record at registrar; `Host()` rule in `docker-compose.yml` |
| **VPS provider** | Hetzner CPX21 (~$8/mo, EU) for cost; DigitalOcean Basic 4 GB ($24/mo) for global | See section 1 below |
| **Webhook endpoint** | Slack incoming webhook URL | `MONITOR_WEBHOOK_URL` in `.env`, also `RESET_WEBHOOK_URL` |
| **Let's Encrypt email** | An email you read; LE notifies expiry from here | `LETSENCRYPT_EMAIL` in `.env` |
| **Cloudflare proxy** | Yes — free DDoS shield; orange-cloud the A record | Cloudflare dashboard |
| **Build-from-source vs. prebuilt image** | **Build from source** (this is the default; no prebuilt image is published yet) | Dockerfile is already set up for this — no action needed |
| **Per-IP rate limit** | Default 120 req/min average, 30 burst | `SYNTRA_RATE_LIMIT_RPM` / `SYNTRA_RATE_LIMIT_BURST` in `.env` |

If you want to host a prebuilt image yourself (faster boot, no Rust
toolchain on the VPS), see "Alternative: prebuilt image" at the
bottom.

---

## 1. Pick a VPS ($20/month class)

Any of these works. All three give you 2 vCPU / 4 GB RAM / ~80 GB SSD
which is comfortable for the demo image (syntra binary + debian base
≈ 600 MB; runtime RSS is well under 1 GB).

| Provider     | Plan                                 | $/mo | Notes                                     |
| ------------ | ------------------------------------ | ---- | ----------------------------------------- |
| Hetzner      | CPX21 (3 vCPU AMD, 4 GB)             | ~$8  | Best price/perf. EU only.                 |
| DigitalOcean | Basic Droplet, Regular SSD, 4 GB     | $24  | Easy to provision, global regions.        |
| Vultr        | High Frequency 4 GB                  | $24  | Decent middle ground.                     |

Recommended OS: Debian 12 or Ubuntu 24.04 LTS. Install Docker Engine +
the compose plugin from the official Docker apt repo (not the distro
package — distro Docker is usually too old for compose v2).

**First build takes ~3–5 minutes** because we compile Syntra + Lycan
from source on the VPS. Subsequent rebuilds reuse the Cargo layer
cache and take ~30 seconds. If the VPS has fewer than 2 GB RAM, the
Rust linker will OOM — bump to 4 GB or build elsewhere and ship the
image.

---

## 2. DNS

Add an **A record** in your zone:

```
try.syntra.io.   IN   A   <VPS_PUBLIC_IPV4>
```

If you want IPv6 too, add an `AAAA` record. The Let's Encrypt HTTP-01
challenge only needs the A record though — Traefik will handle the
issuance the first time port 80 is hit.

---

## 3. Cloudflare proxy (DDoS protection)

1. Add `syntra.io` (or just the `try` subdomain via a CNAME flattening
   strategy if you only own a subzone) to Cloudflare.
2. **Orange-cloud** the `try` A record so traffic flows through
   Cloudflare's network. This gives you free WAF, bot fight mode, and
   DDoS absorption.
3. Set SSL/TLS mode to **Full** (not Flexible, not Full Strict — the
   Let's Encrypt cert Traefik issues is publicly trusted, but Full
   Strict requires the origin cert to also be Cloudflare-issued).
4. Enable **Always Use HTTPS** and **Automatic HTTPS Rewrites**.
5. Optional: turn on **Bot Fight Mode** to swat obvious scrapers.

If you skip Cloudflare entirely the deploy still works — Traefik
terminates Let's Encrypt directly on the VPS. You just lose the
upstream DDoS shield.

---

## 4. Deploy

See [`DEPLOY.md`](./DEPLOY.md) for the exact copy-paste commands. The
short version:

```bash
# On the VPS (Docker + compose plugin already installed).
sudo mkdir -p /opt/syntra-try && sudo chown "$USER" /opt/syntra-try
cd /opt/syntra-try

# Sync the LYCAN REPO ROOT here (this artifact's Dockerfile expects
# Lycan/ and Syntra/ to be sibling directories at the build context).
# rsync, git clone, scp — whatever you prefer.

cd Syntra/deploy/try-instance
cp .env.example .env
$EDITOR .env                # fill in LETSENCRYPT_EMAIL etc.

docker compose build        # ~3–5 min first time (Rust build)
docker compose up -d
docker compose logs -f      # watch first-boot install
```

First boot: Traefik does the Let's Encrypt HTTP-01 exchange (~10 s
once port 80 is reachable), the syntra container starts in dev-mode,
and `install.py` compiles + installs all five flagship capsules
(~10 s). Visit `https://try.syntra.io/` → landing page; click "Enter
the dashboard" → `/admin` opens with no auth prompt.

---

## 5. Cron

Install both cron jobs as root (the user that owns the docker
socket):

```cron
# Daily wipe at 00:00 UTC.
0 0 * * * /opt/syntra-try/Syntra/deploy/try-instance/reset.sh

# Health probe every five minutes.
*/5 * * * * /opt/syntra-try/Syntra/deploy/try-instance/monitor.sh
```

`reset.sh` logs to `/var/log/syntra-reset.log`. `monitor.sh` only
writes to its log on failure; alerts go to `MONITOR_WEBHOOK_URL`.

---

## 6. Expected operating cost

| Line item          | $/mo    | Notes                                       |
| ------------------ | ------- | ------------------------------------------- |
| VPS (Hetzner CPX21) | $8     | Cheapest workable option, EU only.          |
| VPS (DigitalOcean 4 GB) | $24 | Globally available alternative.            |
| DNS (Cloudflare)   | $0      | Free tier covers proxy + DNS + WAF basics.  |
| Let's Encrypt      | $0      | Automated by Traefik.                       |
| Webhook gateway    | $0      | Slack/Discord webhooks are free.            |
| **Total**          | **$8–$24** | Depending on VPS choice.                 |

Budget assumption from the brief: **$20/month class**. Hetzner sits
well under, DigitalOcean/Vultr sit just over. All three are fine.

---

## 7. Taking the demo down

When you decide to retire it:

```bash
cd /opt/syntra-try/Syntra/deploy/try-instance
docker compose down -v        # -v drops named volumes (syntra-store + traefik-letsencrypt)
```

Then destroy the VPS in your provider's console and **delete the
`try.syntra.io` A record** in DNS. If you Cloudflare-proxied it,
also delete the record there.

If you want to keep the data but stop the demo cheaply, replace
`docker compose down -v` with `docker compose down` and snapshot the
VPS — restart takes ~30 s next time you boot the snapshot.

---

## Operational notes / caveats

- **Dev mode is unauthenticated.** The custom `entrypoint.sh` in this
  directory runs `syntra serve --dev-mode`, so every API route is
  open to anyone who can reach port 8787. That's intentional for a
  public demo — but it also means the **Traefik router rule in
  `docker-compose.yml` is the trust boundary**: it forwards only
  specific path prefixes (`/admin`, `/health`, `/decide`, `/feedback`,
  `/memory`, `/decisions`, `/api`, `/console`). Don't widen that
  rule without thinking about what's exposed.

- **No per-IP rate limit inside the syntra binary.** The Syntra
  runtime ships a single global limiter. The per-IP enforcement on
  this deploy lives in the **Traefik middleware** (`try-ratelimit` in
  `docker-compose.yml`), tuned via `SYNTRA_RATE_LIMIT_RPM` in `.env`.
  Cloudflare's WAF + rate-limit rules sit upstream of that.

- **All five flagship capsules are installed**:
  predictive-autoscaling, anomaly-routing, seasonal-fraud-threshold,
  shared-state-action-embeddings, and hierarchical-region-routing.
  `install.py` is hardcoded to expect the full set; omitting any
  capsule causes the installer to hard-fail and leaves later
  capsules uninstalled. If you want a subset, fork `install.py`
  rather than dropping `COPY` lines from the Dockerfile.

- **Reset window is ~30 s of API downtime** at 00:00 UTC (stop
  container, wipe volume, restart, re-install all five capsules).
  The landing page stays up because Traefik isn't restarted.

- **Syntra prints a dev-mode warning on boot** (`dev mode on
  non-loopback address 0.0.0.0:8787 — this is unsafe`). That's
  expected: binding 0.0.0.0 is required so Traefik in the same
  Docker network can reach the syntra container. The warning is the
  binary doing its job — flag it back to operators rather than
  silently allowing the unsafe pattern.

---

## Alternative: prebuilt image

If you'd rather not compile on the VPS, you can publish the
`syntra-try` image to a registry once and `docker compose pull` it
on the VPS:

```bash
# On a build host with the Lycan repo checked out:
cd /path/to/Lycan
docker build -t ghcr.io/<your-org>/syntra-try:latest \
    -f Syntra/deploy/try-instance/Dockerfile .
docker push ghcr.io/<your-org>/syntra-try:latest
```

Then edit `docker-compose.yml`: replace the `build:` block on the
`syntra` service with `image: ghcr.io/<your-org>/syntra-try:latest`
and the VPS skips the Rust toolchain entirely. First boot drops from
~3–5 min to ~30 s.

The default config builds from source because no prebuilt image is
published yet. Pick whichever fits your operational preference.
