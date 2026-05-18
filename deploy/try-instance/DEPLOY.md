# Deploy try.syntra.io — exact commands

This is the copy-paste path for a first deploy on a Hetzner CPX21 (or
any 4 GB Debian-12-class VPS). Read [`README.md`](./README.md) first
for context and decisions.

Assumes:

- You've already created the VPS, can `ssh root@<VPS_IP>`, and have
  the public IPv4 address.
- You own a DNS zone (`syntra.io` or equivalent) and can edit records.
- You have a Slack/Discord webhook URL ready (or an email-gateway URL
  that accepts POST + JSON).

Everything below runs on the VPS as root unless noted otherwise.
Substitute your values for the **bold** placeholders.

---

## Step 1 — DNS

In your DNS provider:

```
A    try.syntra.io.   IN   <VPS_PUBLIC_IPV4>   TTL 300
```

If you're Cloudflare-fronting, set the proxy status to **proxied
(orange cloud)** later — but for the initial Let's Encrypt issuance,
**leave it DNS-only (grey cloud)** for the first 10 minutes so the
HTTP-01 challenge reaches the VPS directly. Switch to orange once the
cert is issued.

Verify:

```bash
dig +short try.syntra.io
# → should print your VPS IPv4
```

Wait up to a TTL before continuing.

---

## Step 2 — Install Docker on the VPS

```bash
ssh root@<VPS_IP>

# Pull Docker's official install script (or follow docs.docker.com).
curl -fsSL https://get.docker.com -o get-docker.sh
sh get-docker.sh
rm get-docker.sh

# Verify.
docker --version
docker compose version
```

---

## Step 3 — Get the Lycan repo onto the VPS

The Dockerfile's build context is the Lycan repo root. Sync the whole
tree:

```bash
# Option A: git clone (if the repo is public or you've set up an
# access token).
mkdir -p /opt
cd /opt
git clone https://github.com/ashhart/Syntra.git syntra-try
# If the repo's name on GitHub differs, adjust above.

# Option B: rsync from your laptop (recommended if private).
# From your laptop:
#   rsync -avz --exclude target --exclude .git \
#     /path/to/Lycan/ root@<VPS_IP>:/opt/syntra-try/
```

Confirm:

```bash
ls /opt/syntra-try/
# → Lycan  Syntra  ...
```

---

## Step 4 — Fill in `.env`

```bash
cd /opt/syntra-try/Syntra/deploy/try-instance
cp .env.example .env
nano .env
```

Set at minimum:

```bash
LETSENCRYPT_EMAIL=ops@<your-org>.com
RESET_WEBHOOK_URL=https://hooks.slack.com/services/T.../B.../...
MONITOR_WEBHOOK_URL=https://hooks.slack.com/services/T.../B.../...
SYNTRA_RATE_LIMIT_RPM=120
SYNTRA_RATE_LIMIT_BURST=30
SYNTRA_DEMO_CAPSULE=predictive-autoscaling
```

---

## Step 5 — First build + boot

```bash
cd /opt/syntra-try/Syntra/deploy/try-instance

# Build the syntra-try image from source (~3–5 minutes).
docker compose build

# Start the stack.
docker compose up -d

# Watch first-boot.
docker compose logs -f
```

What you should see, in order:

1. Traefik starts and starts requesting a Let's Encrypt cert for
   `try.syntra.io`. ~10 s.
2. The syntra container starts in dev-mode (`WARNING: running in dev
   mode — all routes unauthenticated`). That's expected.
3. `install.py` compiles + installs all five flagship capsules:
   `predictive-autoscaling`, `anomaly-routing`,
   `seasonal-fraud-threshold`, `shared-state-action-embeddings`,
   `hierarchical-region-routing`. ~10 s.
4. `[try] ready — Syntra serving on 8787`.

Hit `Ctrl-C` to exit the log tail (services keep running).

Verify:

```bash
curl -fsS https://try.syntra.io/health
# → {"ok":true,"service":"Syntra"}

curl -fsS https://try.syntra.io/admin/capsules | jq '.capsules | length'
# → 5
```

Open `https://try.syntra.io/admin` in a browser — the dashboard
should load with no auth prompt.

---

## Step 6 — Switch Cloudflare to proxied

Now that LE issued the cert directly against the VPS, you can turn
on the Cloudflare proxy:

1. In Cloudflare DNS, click the cloud icon next to the `try` A
   record → orange (proxied).
2. SSL/TLS mode → **Full**.
3. Enable **Always Use HTTPS**.
4. Optional: Bot Fight Mode.

Verify the cert chain is still trusted from the public internet:

```bash
curl -fsS https://try.syntra.io/health
```

---

## Step 7 — Install cron

```bash
sudo crontab -e
```

Append:

```cron
# Daily wipe at 00:00 UTC.
0 0 * * * /opt/syntra-try/Syntra/deploy/try-instance/reset.sh

# Health probe every five minutes.
*/5 * * * * /opt/syntra-try/Syntra/deploy/try-instance/monitor.sh
```

Test both immediately:

```bash
/opt/syntra-try/Syntra/deploy/try-instance/monitor.sh
# → no output on success; check $MONITOR_WEBHOOK_URL got nothing.

# Don't test reset.sh now — it wipes the store. Wait until midnight
# UTC, or accept the wipe and:
/opt/syntra-try/Syntra/deploy/try-instance/reset.sh
# → ~30 s of downtime, capsules re-installed at the end.
```

---

## Step 8 — Verify the alert path

Briefly stop the syntra container so `monitor.sh` fires:

```bash
docker compose stop syntra
sleep 30
# Wait for the next 5-minute cron tick, or run monitor.sh manually:
/opt/syntra-try/Syntra/deploy/try-instance/monitor.sh
# → should POST to MONITOR_WEBHOOK_URL.

# Bring it back.
docker compose start syntra
```

Confirm the message arrived in Slack/Discord/email.

---

## Step 9 — Smoke test the API

```bash
# Get a decision from the predictive-autoscaling capsule.
curl -sX POST https://try.syntra.io/tenants/demo/jobs/autoscale/capsules/orders/decide \
    -H "Content-Type: application/json" \
    -d '{
      "load_history":[80,90,110,140,180,220],
      "current_instances":3,
      "target_per_instance":100,
      "min_instances":1,
      "max_instances":20,
      "features":{"hour":14.0,"current_instances":3,"load_trend":0.6}
    }' | jq '.'
# → {"ok":true,"decisions":[{"chosen_option":..,"published":{"recommended_instances":..}}], ...}
```

Done. The instance is live.

---

## Rollback / take-down

```bash
cd /opt/syntra-try/Syntra/deploy/try-instance

# Stop everything, keep volumes (data preserved for next start).
docker compose down

# OR: stop everything AND wipe state.
docker compose down -v

# OR: full nuke.
docker compose down -v
sudo rm -rf /opt/syntra-try
# ...then destroy the VPS and delete the DNS A record.
```
