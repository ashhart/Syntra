# syntra-export

Export a Syntra capsule's current learned state to a portable JSON snapshot.

Operators use the snapshot to:

- **Archive** a converged policy before a model rollout.
- **Migrate** learned weights between Syntra instances (dev -> staging -> prod).
- **Evaluate offline** what Syntra would have done on historical traffic using
  `syntra-ope evaluate --mode static`.

## Installation

```bash
pip install -e .
```

Or run directly without installing:

```bash
python3 export.py --syntra-url http://localhost:8787 ...
```

## Usage

```
syntra-export \
    --syntra-url http://localhost:8787 \
    --admin-key  $SYNTRA_ADMIN_KEY \
    --tenant     myteam \
    --job        retry \
    --capsule    router \
    [--include-decisions] \
    [--include-audits] \
    [--include-snapshots] \
    [--output snapshot.json]
```

When `--output` is absent the snapshot JSON is written to stdout.

### Arguments

| Flag | Required | Description |
|------|----------|-------------|
| `--syntra-url` | yes | Base URL of the Syntra server |
| `--admin-key` | yes | Bearer token (admin key or scoped Read token) |
| `--tenant` | yes | Tenant identifier |
| `--job` | yes | Job identifier |
| `--capsule` | yes | Capsule identifier |
| `--include-decisions` | no | Append raw decision log (NDJSON) |
| `--include-audits` | no | Append audit log (NDJSON) |
| `--include-snapshots` | no | Append snapshot metadata list (no bodies) |
| `--output` | no | Write to file; default is stdout |

## Output schema (v1)

```json
{
  "v": 1,
  "exportedAt": 1747500000,
  "syntraVersion": "0.2.0",
  "tenant": "t",
  "job": "j",
  "capsule": "c",
  "capsuleHash": "sha256...",
  "learningConfig": { "...": "full LearningConfig from GET /learning" },
  "report":         { "...": "full GET /report payload" },
  "memory":         { "...": "full GET /memory payload" },
  "warmupState":    "active|warmup|frozen",
  "metaBanditLeader": "Ucb",
  "policyByContext": {
    "context_key_1": { "bestOption": 1, "weights": [0.05, 0.90, 0.05] },
    "context_key_2": { "bestOption": 0, "weights": [0.80, 0.10, 0.10] }
  },
  "decisions": ["..."],
  "audits":    ["..."],
  "snapshots": ["..."]
}
```

`decisions`, `audits`, and `snapshots` are only present when the corresponding
`--include-*` flag is passed.  Snapshot bodies are never included — only the
metadata list from `GET /snapshots`.

## policyByContext derivation

For each strategy node in the memory sidecar the export tool:

1. Reads the meta-bandit leader from `memory.strategies[N].metaBandit.leader`.
2. If a leader is known, collects all `candidateContexts` entries prefixed
   `"<leader>|"` — these are the weights the winning algorithm accumulated.
3. Falls back to the legacy `contexts` bucket when no leader has been elected
   (capsule still in warmup).
4. Emits `{ bestOption: argmax(weights), weights: [...] }` per context key.

This gives a static lookup table suitable for `EvalPolicy.from_json` in
`syntra-ope evaluate --mode static`.

## Offline evaluation

```bash
# Step 1: export
syntra-export --syntra-url http://localhost:8787 --admin-key $KEY \
    --tenant myteam --job retry --capsule router \
    --output snapshot.json

# Step 2: extract the policy table for syntra-ope
python3 -c "
import json
snap = json.load(open('snapshot.json'))
policy = {ctx: entry['bestOption'] for ctx, entry in snap['policyByContext'].items()}
json.dump(policy, open('policy.json','w'), indent=2)
"

# Step 3: evaluate
syntra-ope evaluate logged_decisions.csv \
    --mode static --policy-json policy.json --format text
```

## Authentication

A scoped read token works as well as an admin key.  The token must have
`scope::Read { tenant, job, capsule }` for the target capsule.  No writes
are performed.

## Running tests

```bash
cd /path/to/export-tool
PYTHONPATH=. python3 -m pytest tests/ -v
```

Tests run entirely offline: no Syntra server is required.

## License

Apache-2.0
