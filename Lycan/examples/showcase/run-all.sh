#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"

echo
echo "============================================================"
echo "LYCAN SHOWCASE SUITE"
echo "============================================================"
echo "Five demos. No sprawl. Each one proves a core claim."
echo

for demo in \
  "$DIR/01-apps-learn-contexts.sh" \
  "$DIR/02-live-mars-mission.sh" \
  "$DIR/03-autonomous-evolution.sh" \
  "$DIR/04-sandbox-red-team.sh" \
  "$DIR/05-runtime-appliance-memory.sh"
do
  "$demo"
done

echo
echo "============================================================"
echo "SHOWCASE COMPLETE"
echo "============================================================"
echo "1. Context memory"
echo "2. Live Mars science"
echo "3. Autonomous evolution"
echo "4. Runtime sandbox"
echo "5. Appliance persistence"
echo
