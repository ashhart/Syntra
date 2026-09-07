#!/usr/bin/env bash
# Example: export a capsule snapshot and optionally run offline evaluation.
#
# Prerequisites
# -------------
#   export SYNTRA_URL=http://localhost:8787
#   export SYNTRA_ADMIN_KEY=<your-admin-or-read-token>
#
# Basic export (stdout)
# ---------------------
python3 export.py \
    --syntra-url "${SYNTRA_URL:-http://localhost:8787}" \
    --admin-key  "${SYNTRA_ADMIN_KEY:?SYNTRA_ADMIN_KEY must be set}" \
    --tenant     myteam \
    --job        retry \
    --capsule    router

# Export to a file including the decision log
python3 export.py \
    --syntra-url "${SYNTRA_URL:-http://localhost:8787}" \
    --admin-key  "${SYNTRA_ADMIN_KEY}" \
    --tenant     myteam \
    --job        retry \
    --capsule    router \
    --include-decisions \
    --output     snapshot.json

# Use the snapshot as a static policy for offline evaluation.
# The policyByContext sub-object maps context_key -> bestOption which is
# exactly the format consumed by syntra-ope's EvalPolicy.from_json.
#
# Extract the policyByContext section first:
python3 - <<'EOF'
import json, sys

with open("snapshot.json") as f:
    snap = json.load(f)

policy = {ctx: entry["bestOption"] for ctx, entry in snap["policyByContext"].items()}
with open("policy_for_ope.json", "w") as f:
    json.dump(policy, f, indent=2)

print(f"Wrote {len(policy)} context(s) to policy_for_ope.json")
EOF

# Then run syntra-ope (from the offline-eval example directory):
# python3 ../offline-eval/evaluate.py logged_decisions.csv \
#     --mode static \
#     --policy-json policy_for_ope.json \
#     --format text
