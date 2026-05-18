#!/usr/bin/env bash
# example_run.sh -- convenience runner for the Syntra A/B harness example.
#
# Prerequisites:
#   - Syntra server running at http://localhost:8787 (or override SYNTRA_URL)
#   - `syntra` binary on PATH (for capsule authoring)
#   - Python 3.8+
#
# Optional: PyYAML for YAML traffic specs (pip install pyyaml)
#   Without it, convert example_traffic.yaml to example_traffic.json first.
#
# Usage:
#   cd /path/to/ab-harness
#   ./example_run.sh
#   ./example_run.sh --rounds 500 --seeds 20

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

SYNTRA_URL="${SYNTRA_URL:-http://localhost:8787}"
SYNTRA_ADMIN_KEY="${SYNTRA_ADMIN_KEY:-dev-key}"

CAPSULE_A="${SCRIPT_DIR}/example_capsule_a.yaml"
CAPSULE_B="${SCRIPT_DIR}/example_capsule_b.yaml"
TRAFFIC_SPEC="${SCRIPT_DIR}/example_traffic.yaml"

ROUNDS="${ROUNDS:-1000}"
SEEDS="${SEEDS:-10}"
SEED_OFFSET="${SEED_OFFSET:-1000}"

OUTPUT_DIR="${SCRIPT_DIR}/results/example_$(date +%Y%m%d_%H%M%S)"

echo "========================================================"
echo "  Syntra A/B Harness -- Example Run"
echo "========================================================"
echo "  Capsule A:    ${CAPSULE_A}"
echo "  Capsule B:    ${CAPSULE_B}"
echo "  Traffic spec: ${TRAFFIC_SPEC}"
echo "  Rounds:       ${ROUNDS}"
echo "  Seeds:        ${SEEDS}"
echo "  Syntra URL:   ${SYNTRA_URL}"
echo "  Output dir:   ${OUTPUT_DIR}"
echo "========================================================"

python3 "${SCRIPT_DIR}/ab_harness.py" \
    "${CAPSULE_A}" \
    "${CAPSULE_B}" \
    "${TRAFFIC_SPEC}" \
    --rounds "${ROUNDS}" \
    --seeds "${SEEDS}" \
    --seed-offset "${SEED_OFFSET}" \
    --syntra-url "${SYNTRA_URL}" \
    --admin-key "${SYNTRA_ADMIN_KEY}" \
    --output-dir "${OUTPUT_DIR}" \
    "$@"

echo ""
echo "Done. Results written to: ${OUTPUT_DIR}"
