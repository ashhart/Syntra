#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# Adaptive API Router Resilience Benchmark
#
# Tests whether Syntra can outperform static and conventional adaptive
# routing baselines under changing web-service conditions.
#
# Prerequisites:
#   - Syntra running on localhost:8787 (docker compose up)
#   - Lycan compiler available (cargo build --release in Lycan/)
#
# Usage:
#   ./run_benchmark.sh              # Full benchmark (30 seeds, 10k requests)
#   ./run_benchmark.sh --quick      # Quick check (3 seeds, 2k requests)
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
LYCAN="$ROOT/../Lycan/target/release/lycan"
CAPSULE="$SCRIPT_DIR/router_5provider.lyc"
SOURCE="$SCRIPT_DIR/router_5provider.lycs"
SYNTRA_URL="${SYNTRA_URL:-http://localhost:8787}"
ADMIN_KEY="${ADMIN_KEY:-dev-key}"

echo
echo "  Adaptive API Router Resilience Benchmark"
echo "  ========================================="
echo

# 1. Verify Lycan compiler
if [[ ! -x "$LYCAN" ]]; then
  echo "  Lycan compiler not found at $LYCAN"
  echo "  Attempting to build..."
  (cd "$ROOT/../Lycan" && cargo build --release --quiet 2>/dev/null) || {
    echo "  ERROR: Cannot build Lycan. Build it manually:"
    echo "    cd Lycan && cargo build --release"
    exit 1
  }
fi

# 2. Compile capsule (always recompile for freshness)
echo "  Compiling 5-provider router capsule..."
"$LYCAN" compile "$SOURCE" 2>&1 | sed 's/^/    /'
echo

# 3. Verify Syntra is running
echo "  Checking Syntra at $SYNTRA_URL..."
if ! curl -sf "$SYNTRA_URL/health" >/dev/null 2>&1; then
  echo "  ERROR: Syntra is not running at $SYNTRA_URL"
  echo "  Start it with: cd Syntra && docker compose up -d"
  exit 1
fi
echo "    ok"
echo

# 4. Run benchmark
echo "  Starting benchmark..."
echo
python3 "$SCRIPT_DIR/benchmark.py" \
  --syntra-url "$SYNTRA_URL" \
  --admin-key "$ADMIN_KEY" \
  "$@"
