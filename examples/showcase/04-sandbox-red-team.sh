#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

echo
echo "============================================================"
echo "SHOWCASE 04: SANDBOX RED TEAM"
echo "============================================================"
echo "File escape and SSRF attempts blocked by runtime policy."
echo

"$ROOT/examples/demo-sandbox.sh"
