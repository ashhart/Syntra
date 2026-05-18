#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

echo
echo "============================================================"
echo "SHOWCASE 01: APPS LEARN DIFFERENTLY PER CONTEXT"
echo "============================================================"
echo "One capsule. Three context keys. Three separate learned winners."
echo

"$ROOT/examples/demo-context-memory.sh"
