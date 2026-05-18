#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

echo
echo "============================================================"
echo "SHOWCASE 05: RUNTIME APPLIANCE MEMORY"
echo "============================================================"
echo "Install capsule, decide, feedback, restart, prove memory survived."
echo

"$ROOT/examples/demo-docker-quickstart.sh"
