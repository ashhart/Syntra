# Local Development

If you want to build Syntra from source, hack on the runtime, or run
the demo without Docker.

## Prerequisites

- **Rust 1.85 or newer** ([install via rustup](https://rustup.rs/)).
  Both crates use edition 2024, which requires 1.85.
- **Git**
- **Python 3.10+ with `flask` and `requests`** — the demo dashboard and
  the install script are Python. `pip install flask requests`.
- **Docker** *(optional)* — only needed if you want to build the demo
  image yourself instead of pulling
  `ghcr.io/ashhart/syntra:demo`.

macOS additionally needs the Xcode command-line tools for the Rust
linker: `xcode-select --install`.

## Clone and Build

```bash
git clone https://github.com/ashhart/Syntra.git
cd Syntra
cargo build --release            # builds the syntra binary
cd Lycan && cargo build --release # builds the lycan CLI (compiler + runtime)
```

The repo is self-contained — `Lycan/` ships as a subdirectory and the
Syntra crate's `Cargo.toml` references it via `path = "Lycan"`. No
sibling-repo checkout is required. The first `cargo build --release`
from the repo root builds the syntra binary and the lycan library
together; the second one (in `Lycan/`) builds the standalone `lycan`
CLI needed by the install script for `lycan compile program.lycs`.

**Build times:**

- *First build (clean):* ~3–5 minutes on a recent laptop. The two
  release `target/` directories combined are around 750 MB after a
  clean release build.
- *Typical incremental:* under 10 seconds for a recompile + relink
  when you've edited a single source file.

## Run Syntra with the Demo Capsules

The `Syntra/scripts/run-demo.sh` helper script does what the Docker
image's `entrypoint.sh` does, but against your locally-built release
binaries: starts `syntra serve --dev-mode`, installs the five flagship
demo capsules, drives traffic, and serves the dashboard.

```bash
cd Syntra
./scripts/run-demo.sh
```

What you'll see on stdout:

```text
[run-demo] store:     /tmp/syntra-demo-store.XXXXXX
[run-demo] api addr:  127.0.0.1:8787
[run-demo] dashboard: http://localhost:8080
[run-demo] installing demo capsules...
[install] predictive-autoscaling -> demo/autoscale/orders ...
[install] anomaly-routing        -> demo/routing/api ...
[install] seasonal-fraud-threshold -> demo/fraud/threshold ...
[install] shared-state-action-embeddings -> demo/embeddings/router ...
[install] hierarchical-region-routing -> demo/region/router ...
```

Then open <http://localhost:8080>. `Ctrl-C` to stop — the script
SIGTERMs the dashboard and syntra and removes its temporary store.

The script honors a few env vars:

| Variable | Default | Notes |
|---|---|---|
| `SYNTRA_ADDR` | `127.0.0.1:8787` | host:port for the API |
| `DASHBOARD_PORT` | `8080` | dashboard's local port |
| `LYCAN_ADMIN_KEY` | `dev-key-$$` (random) | bearer token |

## Build the Demo Container Locally

If you want to build the Docker image yourself (rather than pulling
the published `:demo` tag from GHCR):

```bash
# Build context is the Syntra repo root; Lycan/ is a subdirectory.
docker build -t syntra:local -f docker/Dockerfile.demo .

docker run -d --name syntra-demo \
  -p 8080:8080 \
  -p 8787:8787 \
  syntra:local
```

The first container build runs a full release compile inside the
builder stage, so it takes the same 3–5 minutes as a clean local
build.

## Running Tests

```bash
# Lycan lib tests
(cd Lycan && cargo test --release --lib)

# Lycan integration tests (serialised — some tests open sockets)
cd Lycan && cargo test --release --test integration -- --test-threads=1

# Syntra integration tests
cd Syntra && cargo test --release --test syntra_cli -- --test-threads=1
```

`--test-threads=1` for integration tests that open ports or write to
on-disk stores — running them in parallel produces flaky port-conflict
and store-collision failures.

## Iterating on Changes

For fast iteration during development, the debug profile recompiles
faster but runs slower at runtime — fine for tests and local servers,
not for benchmarks.

```bash
# Debug build is faster to compile but slower to run.
cd Lycan && cargo build
cd Syntra && cargo build

# Run the syntra server straight from the debug target.
cd Syntra
cargo run --bin syntra -- serve --dev-mode --addr 127.0.0.1:8787 --store ./dev-store
```

`--dev-mode` disables the admin-key requirement so you don't have to
manage a bearer token while iterating. Never run `--dev-mode` against
anything other than localhost.

## Inspecting and Stopping a Running Server

```bash
# Status check (default port 8787)
syntra status
syntra status --port 9090
syntra status --addr 127.0.0.1:9090

# SIGTERM the listener on the configured port
syntra stop
syntra stop --port 9090
```

Both emit JSON on stdout. `syntra stop` does not verify the process
is actually Syntra — it kills whatever holds the port — so if you
have something unrelated bound to 8787, run `lsof -i :8787` first.

## Troubleshooting

**First build is slow.** Expected — about 3–5 minutes for the
full Rust dependency graph. Subsequent incremental builds are
much faster (under 10 seconds for typical edits).

**`cargo build` fails with linker errors on macOS.** You need the
Xcode command-line tools: `xcode-select --install`.

**`error: cannot bind to 127.0.0.1:8787 — port already in use`.**
A previous `syntra serve` (or test run) is still alive. `syntra status`
to confirm; `syntra stop` to SIGTERM it.

**`run-demo.sh` exits with "release binaries not built".** Build both
crates first: `cargo build --release && (cd Lycan && cargo build --release)`
from the Syntra repo root.

**Tests intermittently fail with "address already in use".** A previous
test left a server running. `syntra stop` clears it. Use
`--test-threads=1` for integration suites.

**`/api/state` from the dashboard returns 502 errors.** The dashboard
is up but can't reach the API. Check that `SYNTRA_URL` matches the
server's bind address and that the admin key matches `LYCAN_ADMIN_KEY`.

## What to Read Next

- **[Concepts](../concepts/index.md)** — capsules, kernels, strategy
  nodes, drift, refusal
- **[Domain packs](../examples/index.md)** — worked examples of the
  full integration pattern
- **[API reference](../reference/api.md)** — every HTTP endpoint with
  request / response shape
- **[Cookbook](../reference/cookbook.md)** — common integration
  patterns
- **[Operations](../reference/operations.md)** — running Syntra in
  production
