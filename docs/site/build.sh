#!/usr/bin/env bash
# Build the Syntra static docs site.
#
# Activate a venv that has mkdocs + mkdocs-material installed, then run
# `mkdocs build`. The output goes to site/build/, as configured in
# mkdocs.yml.

set -euo pipefail

cd "$(dirname "$0")"

VENV="${SYNTRA_DOCS_VENV:-/tmp/syntra-site-venv}"

if [ ! -d "$VENV" ]; then
  echo "[build.sh] creating venv at $VENV"
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install --quiet --upgrade pip
  "$VENV/bin/pip" install --quiet mkdocs mkdocs-material
fi

# shellcheck source=/dev/null
source "$VENV/bin/activate"

echo "[build.sh] mkdocs --version: $(mkdocs --version)"
echo "[build.sh] building..."

mkdocs build --strict --clean

echo "[build.sh] done. Output: $(pwd)/site/build/"
