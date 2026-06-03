# Proof Lab

Syntra's proof lab is a bounded research workbench for finite combinatorics and
formal-proof handoff. It currently ships two targets:

- **`erdos190` (SOLVED).** A boundary test on a problem with a known answer:

  > H(k) is the least N such that every finite coloring of `{1..N}` contains a
  > monochromatic k-term arithmetic progression or a rainbow k-term arithmetic
  > progression.

- **`erdos160` (OPEN).** An honest target on a problem that *cannot* be resolved
  by finite computation:

  > h(N) is the smallest number of colours k such that `{1..N}` can be
  > k-coloured so that every four-term arithmetic progression contains at least
  > three distinct colours.

  The open question (erdosproblems.com/160) is the *asymptotic* growth of h(N)
  (known frontier: h(N) << N^(2/3); Hunter << N^(log3/log22 + o(1)) ~ N^0.355).
  The proof lab produces small exact values and certificates, and explicitly
  *refuses* the asymptotic estimate — it is filed as an
  `expert_theorem_required` proof obligation and the command never emits an
  asymptotic claim.

The point is not to claim that finite search solves an asymptotic theorem. The
point is to make Lycan/Syntra useful to an expert:

- search small finite cases exactly, with witnesses or unsat rows
- switch between DFS and SAT/CNF/DPLL backends for #160 finite rows
- emit replayable certificate records for witnesses, bounded-unsat rows, and
  node-limit gaps
- mine simple construction patterns from the best witness
- report finite lower/upper bounds and exactly where the run stopped
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

### Erdos #160 (OPEN, finite certificates only)

```bash
cargo run --release -- proof-lab erdos160 \
  --max-n 18 \
  --node-limit 200000 \
  --backend dfs
```

SAT-backed finite shadow:

```bash
cargo run --release -- proof-lab erdos160 \
  --max-n 12 \
  --max-colors 3 \
  --node-limit 100000 \
  --backend sat
```

Expected result: per-N rows with exact values
`h(N) = 1,1,1,3,3,3,3,3,3,3,3,3,4,4,4,4,4,4` for N = 1..18 (jumps at N=4 and
N=13), each backed by a witness colouring and a complete bounded exhaustion.
The AP length is fixed at 4, so there is no `--k` flag.

Every finite row is labelled finite evidence. The asymptotic growth of h(N) is
filed as an `expert_theorem_required` obligation (`asymptotic_estimate_h160`)
and the command never emits an asymptotic claim. The report also asserts
monotonicity (h(N) <= h(N+1)) across its rows and flags any violation. Use a
small `--node-limit` to see honest `inconclusive` rows instead of bounds.

The JSON report includes:

- `backend`: `dfs` or `sat`
- `certificates`: witness, bounded-unsat, and node-limit records
- `patterns`: histogram / periodicity / adjacent-run findings from the best
  witness
- `bounds`: the largest exact row plus finite lower/upper-bound wording

The SAT certificate is a replayable bounded DPLL exhaustion record. It is not
yet a DRAT/LRAT proof, Lean proof, or expert theorem.

## Native Kernels

The Lycan runtime now exposes these pure combinatorics capabilities:

| Capability | Purpose |
|---|---|
| `comb.apTuples(n, k)` | Enumerate k-term arithmetic progressions in `{1..n}`. |
| `comb.isGoodColoring(colors, k)` | Check that a coloring has no monochromatic or rainbow k-AP. |
| `comb.badAp(colors, k)` | Return the first monochromatic or rainbow violation. |
| `comb.goodColoringWitness(n, k, node_limit)` | Search for a witness coloring, unsat row, or inconclusive result. |

Erdos #160 uses a different predicate (a 4-term AP is bad when it has fewer than
three distinct colours), so the proof lab calls a dedicated combinatorics-kernel
surface in `Lycan/src/combinatorics.rs`:

| Kernel | Purpose |
|---|---|
| `bad_arithmetic_progression_160(colors)` | Return the first 4-term AP with fewer than three distinct colours. |
| `is_good_coloring_160(colors)` | Check that every 4-term AP has at least three distinct colours. |
| `search_coloring_160(n, max_colors, node_limit)` | Bounded exhaustive search for a valid colouring with at most `max_colors` colours. |
| `search_coloring_160_sat(n, max_colors, node_limit)` | Encode the finite #160 row as CNF and run a bounded DPLL SAT search. |
| `h160(n, node_limit)` | Minimum-colour search returning witness / exhaustion / inconclusive. |

The runtime capability catalog also exposes:

| Capability | Purpose |
|---|---|
| `comb.hasThreeDistinct4ApColoring(colors)` | Check that every 4-term AP has at least three distinct colours. |
| `comb.badThreeDistinct4Ap(colors)` | Return the first low-distinct 4-term AP violation. |
| `comb.threeDistinct4ApWitness(n, max_colors, node_limit)` | DFS witness / exhaustion / inconclusive search. |
| `comb.threeDistinct4ApSatWitness(n, max_colors, node_limit)` | CNF/DPLL witness / exhaustion / inconclusive search. |

The search is intentionally bounded. `inconclusive` is a valid answer, not a
failure. This is what keeps the tool honest when a problem leaves the tractable
finite slice — and for #160 the asymptotic question is permanently outside that
slice, so it is recorded as a refusal rather than computed.

## What This Gives Us

For hard mathematical work, this is the first serious step toward an expert
assistant loop:

```text
finite search -> witness/counterexample -> conjecture -> proof obligations
              -> certificate record -> pattern mining -> bounds report
              -> Lean skeleton -> expert/prover work -> regression arena
```

That is useful even when the final theorem still requires human-level proof
ideas, because it turns "try things" into checked artifacts that can be reused,
regressed, and formalized.
