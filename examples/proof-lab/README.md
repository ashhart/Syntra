# Proof Lab

Syntra's proof lab is a bounded research workbench for finite combinatorics and
formal-proof handoff.

It was added after using Erdos Problem #190 as a boundary test:

> H(k) is the least N such that every finite coloring of `{1..N}` contains a
> monochromatic k-term arithmetic progression or a rainbow k-term arithmetic
> progression.

The point is not to claim that finite search solves an asymptotic theorem. The
point is to make Lycan/Syntra useful to an expert:

- search small finite cases exactly, with witnesses or unsat rows
- mine finite claims and non-claims from the trace
- generate named proof obligations
- export Lean skeletons for formalization
- expose arithmetic-progression/coloring kernels to Lycan capsules
- keep an arena record of where computation succeeds, slows, or refuses

## Run It

```bash
cargo run --release -- proof-lab erdos190 \
  --k 3 \
  --max-n 9 \
  --node-limit 20000
```

Expected result: the finite search reports `H(3) = 9`.

Write a Markdown report and a Lean skeleton:

```bash
cargo run --release -- proof-lab erdos190 \
  --k 3 \
  --max-n 9 \
  --node-limit 20000 \
  --format markdown \
  --out erdos190-proof-lab.md \
  --lean-out erdos190.lean
```

## Native Kernels

The Lycan runtime now exposes these pure combinatorics capabilities:

| Capability | Purpose |
|---|---|
| `comb.apTuples(n, k)` | Enumerate k-term arithmetic progressions in `{1..n}`. |
| `comb.isGoodColoring(colors, k)` | Check that a coloring has no monochromatic or rainbow k-AP. |
| `comb.badAp(colors, k)` | Return the first monochromatic or rainbow violation. |
| `comb.goodColoringWitness(n, k, node_limit)` | Search for a witness coloring, unsat row, or inconclusive result. |

The search is intentionally bounded. `inconclusive` is a valid answer, not a
failure. This is what keeps the tool honest when a problem leaves the tractable
finite slice.

## What This Gives Us

For hard mathematical work, this is the first serious step toward an expert
assistant loop:

```text
finite search -> witness/counterexample -> conjecture -> proof obligations
              -> Lean skeleton -> expert/prover work -> regression arena
```

That is useful even when the final theorem still requires human-level proof
ideas, because it turns "try things" into checked artifacts that can be reused,
regressed, and formalized.
