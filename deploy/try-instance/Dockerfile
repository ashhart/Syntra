# Dockerfile for try.syntra.io — the shared public demo instance.
#
# Builds Syntra + Lycan from source (no dependency on a pre-published
# image). The build is a multi-stage clone of Syntra/docker/Dockerfile.demo
# with the try-instance-specific hardening layered on top:
#
#   * pins the four flagship capsules under /syntra/demo/capsules/ so the
#     entrypoint re-installs them on every fresh-volume boot
#     (predictive-autoscaling, anomaly-routing, seasonal-fraud-threshold,
#     hierarchical-region-routing),
#   * runs `syntra serve --dev-mode` so no admin token is required by
#     callers — this is a public demo, anonymous traffic is expected,
#   * enforces strict per-IP rate-limiting via Traefik middleware
#     (configured in docker-compose.yml — the syntra binary itself has
#     a single global limiter; per-IP enforcement lives at the proxy
#     layer),
#   * exposes only port 8787 (the demo dashboard runs on 8080 in the
#     base image but is not exposed in this hardened build — operators
#     access /admin on 8787),
#   * has a HEALTHCHECK that polls /health every 30s.
#
# Build context: the Lycan repo root (the directory containing Lang/
# and Syntra/), same convention as Syntra/docker/Dockerfile.demo.
# Build:
#   docker build -t syntra-try -f Syntra/deploy/try-instance/Dockerfile .
# Run via docker-compose.yml in this directory (do NOT `docker run`
# this image directly; Traefik in front terminates TLS and applies
# the per-IP rate limit).

# ── Stage 1: build the syntra + lycan binaries ───────────────────────────────
FROM rust:1.86-slim AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Syntra depends on Lang via a relative path, so both crates must be
# present before `cargo build`. The runtime also needs `lycan compile`
# at install-time, so we build that bin too.
COPY Lang/    Lang/
COPY Syntra/  Syntra/

RUN cd Syntra \
    && cargo build --release --bin syntra \
    && cd ../Lang \
    && cargo build --release --bin lycan \
    && cp /build/Syntra/target/release/syntra /usr/local/bin/syntra \
    && cp /build/Lang/target/release/lycan    /usr/local/bin/lycan

# ── Stage 2: hardened runtime image ──────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        python3 \
        python3-flask \
        python3-requests \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /syntra

COPY --from=builder /usr/local/bin/syntra /usr/local/bin/syntra
COPY --from=builder /usr/local/bin/lycan  /usr/local/bin/lycan

# Demo installer (compiles and POSTs each flagship capsule). The
# entrypoint is the try-instance-specific one in this directory (runs
# the binary in dev-mode, skips the demo dashboard + traffic generator
# that the regular demo image ships).
COPY Syntra/docker/demo/capsule/                 /syntra/demo/capsule/
COPY Syntra/deploy/try-instance/entrypoint.sh    /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# All five flagship capsule sources, pinned for daily-reset
# reproducibility. Each directory holds capsule.yaml, program.lycs
# (compiled in-container by lycan), and learning.json (PUT to the
# capsule's /learning endpoint after install). The install.py script
# expects the full set — omitting any one of them causes install.py
# to hard-fail and leaves later capsules uninstalled.
COPY Syntra/examples/predictive-autoscaling/         /syntra/demo/capsules/predictive-autoscaling/
COPY Syntra/examples/anomaly-routing/                /syntra/demo/capsules/anomaly-routing/
COPY Syntra/examples/seasonal-fraud-threshold/       /syntra/demo/capsules/seasonal-fraud-threshold/
COPY Syntra/examples/shared-state-action-embeddings/ /syntra/demo/capsules/shared-state-action-embeddings/
COPY Syntra/examples/hierarchical-region-routing/    /syntra/demo/capsules/hierarchical-region-routing/

# LYCAN_STORE_ROOT points at the persisted volume root that reset.sh
# wipes daily. SYNTRA_DEMO_CAPSULE selects which capsule appears as
# the dashboard's boot default; the docker-compose.yml in this
# directory overrides it via .env if desired.
ENV LYCAN_STORE_ROOT=/store \
    SYNTRA_DEMO_CAPSULE=predictive-autoscaling

VOLUME ["/store"]

EXPOSE 8787

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8787/health || exit 1

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
