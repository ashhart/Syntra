use lycan::combinatorics::{H160Status, SearchStatus, h160, search_good_coloring};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofLabReport {
    pub problem: String,
    pub summary: String,
    pub components: Vec<ComponentStatus>,
    pub finite_search: Vec<FiniteSearchCase>,
    pub exact_h: Option<usize>,
    pub conjectures: Vec<String>,
    pub proof_obligations: Vec<ProofObligation>,
    pub lean_skeleton: String,
    pub combinatorics_kernels: Vec<String>,
    pub arena: Vec<ArenaCase>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStatus {
    pub name: String,
    pub status: String,
    pub gives_us: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FiniteSearchCase {
    pub n: usize,
    pub k: usize,
    pub status: String,
    pub nodes: usize,
    pub node_limit: usize,
    pub coloring_1_based: Option<Vec<usize>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofObligation {
    pub id: String,
    pub statement: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArenaCase {
    pub name: String,
    pub result: String,
    pub interpretation: String,
}

pub fn run_erdos190(k: usize, max_n: usize, node_limit: usize) -> ProofLabReport {
    let mut finite_search = Vec::new();
    let mut exact_h = None;
    let mut largest_witness: Option<(usize, Vec<usize>)> = None;
    let mut first_inconclusive = None;

    for n in 1..=max_n {
        let result = search_good_coloring(n, k, None, node_limit);
        let status = status_label(&result.status).to_string();
        let coloring_1_based = result.coloring.clone().map(|colors| {
            colors
                .into_iter()
                .map(|color| color + 1)
                .collect::<Vec<_>>()
        });

        if let Some(colors) = &coloring_1_based {
            largest_witness = Some((n, colors.clone()));
        }
        if matches!(result.status, SearchStatus::Unsat) {
            exact_h = Some(n);
        }
        if matches!(result.status, SearchStatus::Inconclusive) && first_inconclusive.is_none() {
            first_inconclusive = Some(n);
        }

        finite_search.push(FiniteSearchCase {
            n,
            k,
            status,
            nodes: result.nodes,
            node_limit: result.node_limit,
            coloring_1_based,
        });

        if exact_h.is_some() || first_inconclusive.is_some() {
            break;
        }
    }

    let summary = match exact_h {
        Some(h) => format!(
            "Exact finite result found for this bounded run: H({k}) = {h}. This is finite evidence, not an asymptotic proof."
        ),
        None => match largest_witness {
            Some((n, _)) => format!(
                "Bounded search found good colorings through N={n}, so this run establishes H({k}) > {n} within the searched range."
            ),
            None => format!(
                "Bounded search did not find a good coloring for k={k}; inspect inconclusive/unsat rows before treating this as evidence."
            ),
        },
    };

    let mut conjectures = vec![
        "If a coloring uses fewer than k colors, it cannot contain a rainbow k-term AP; van der Waerden lower bounds transfer into H(k).".to_string(),
        "Finite witnesses are useful regression targets, but they do not prove the Erdos #190 asymptotic limit.".to_string(),
    ];
    if let Some(h) = exact_h {
        conjectures.insert(0, format!("Finite exact claim for this run: H({k}) = {h}."));
    } else if let Some((n, _)) = largest_witness {
        conjectures.insert(
            0,
            format!("Finite lower-bound claim for this run: H({k}) > {n}."),
        );
    }

    let proof_obligations = proof_obligations(k, exact_h, largest_witness.as_ref());
    let lean_skeleton = lean_skeleton(k, exact_h, largest_witness.as_ref());
    let arena = arena_cases(k, exact_h, largest_witness.as_ref(), first_inconclusive);

    ProofLabReport {
        problem: "Erdos #190: least N forcing a monochromatic or rainbow k-term arithmetic progression in every finite coloring".to_string(),
        summary,
        components: components(),
        finite_search,
        exact_h,
        conjectures,
        proof_obligations,
        lean_skeleton,
        combinatorics_kernels: vec![
            "comb.apTuples(n, k): enumerate APs in {1..n}".to_string(),
            "comb.isGoodColoring(colors, k): verify no monochromatic/rainbow k-AP".to_string(),
            "comb.badAp(colors, k): return the first violation".to_string(),
            "comb.goodColoringWitness(n, k, node_limit): bounded witness/unsat/inconclusive search".to_string(),
        ],
        arena,
        warnings: vec![
            "The Lean bridge emits proof skeletons and obligations; it does not claim a machine-checked proof.".to_string(),
            "The finite search engine is exponential and bounded. Inconclusive means exactly that.".to_string(),
            "The Erdos #190 result is asymptotic; computing small H(k) values cannot prove the theorem by itself.".to_string(),
        ],
    }
}

/// Erdos #160 (erdosproblems.com/160, status OPEN): h(N) is the smallest k such
/// that {1..N} can be k-coloured so that every four-term arithmetic progression
/// contains at least three distinct colours. The open question is the
/// *asymptotic* growth of h(N) (known: h(N) << N^{2/3}; Hunter
/// << N^{(log3/log22)+o(1)} ~ N^{0.355}). A finite search cannot resolve the
/// asymptotic; it only produces small exact values plus certificates.
///
/// This mirrors `run_erdos190`: a bounded per-N min-colour search produces
/// witness / exhaustion / inconclusive rows, finite exact + bound claims,
/// conjectures and non-claims, named proof obligations (including an explicit
/// `expert_theorem_required` obligation for the asymptotic estimate), a Lean
/// starter file, and an arena record. It NEVER emits an asymptotic claim.
pub fn run_erdos160(max_n: usize, node_limit: usize) -> ProofLabReport {
    let mut finite_search = Vec::new();
    let mut largest_exact: Option<(usize, usize, Vec<usize>)> = None; // (n, h, witness)
    let mut first_inconclusive = None;
    let mut monotonicity_violation: Option<(usize, usize, usize)> = None; // (n, h(n-1), h(n))
    let mut prev_h: Option<usize> = None; // most recent exact h value

    for n in 1..=max_n {
        let result = h160(n, node_limit);
        let (status, coloring_1_based) = match result.status {
            H160Status::Exact => {
                let h = result.h.expect("exact result carries h");
                let witness = result.witness_1_based.clone();
                if let Some(colors) = &witness {
                    largest_exact = Some((n, h, colors.clone()));
                }
                (format!("witness h({n})={h}"), witness)
            }
            H160Status::LowerBound => (
                format!("exhaustion h({n})>={}", result.lower_bound),
                None,
            ),
            H160Status::Inconclusive => {
                if first_inconclusive.is_none() {
                    first_inconclusive = Some(n);
                }
                ("inconclusive".to_string(), None)
            }
        };

        // Monotonicity check: h(N) is non-decreasing because {1..N} is a subset
        // of {1..N+1}. We can only compare two consecutive *exact* values.
        if let (Some(prev), Some(cur)) = (prev_h, result.h) {
            if cur < prev && monotonicity_violation.is_none() {
                monotonicity_violation = Some((n, prev, cur));
            }
        }
        if let Some(cur) = result.h {
            prev_h = Some(cur);
        }

        finite_search.push(FiniteSearchCase {
            n,
            k: lycan::combinatorics::ERDOS160_AP_LEN,
            status,
            nodes: result.nodes,
            node_limit: result.node_limit,
            coloring_1_based,
        });
    }

    let exact_h = largest_exact.as_ref().map(|(_, h, _)| *h);

    let summary = match &largest_exact {
        Some((n, h, _)) => format!(
            "Bounded search resolved exact values up to N={n} (latest h({n})={h}). \
             This is finite evidence, not an asymptotic proof; it does not resolve Erdos #160."
        ),
        None => match first_inconclusive {
            Some(n) => format!(
                "Bounded search hit the node limit at N={n} before resolving any exact value; \
                 inconclusive is a valid answer, not a bound."
            ),
            None => "Bounded search produced only lower-bound exhaustions; inspect the rows \
                     before treating this as evidence."
                .to_string(),
        },
    };

    let mut conjectures = vec![
        "Monotonicity: h(N) is non-decreasing in N, since any valid colouring of {1..N+1} restricts to a valid colouring of {1..N}.".to_string(),
        "Once a four-term AP exists (N >= 4), fewer than three colours cannot give every 4-AP three distinct colours, so h(N) >= 3 for N >= 4 (a van der Waerden-style floor).".to_string(),
        "Finite exact values and witnesses are useful regression targets, but they do not estimate the asymptotic growth of h(N) and do not resolve Erdos #160.".to_string(),
    ];
    if let Some(violation) = monotonicity_violation {
        let (n, prev, cur) = violation;
        conjectures.insert(
            0,
            format!(
                "WARNING: monotonicity violated at N={n} (h({})={prev} but h({n})={cur}); this indicates a kernel bug, not a result.",
                n - 1
            ),
        );
    }
    if let Some((n, h, _)) = &largest_exact {
        conjectures.insert(
            0,
            format!("Finite exact: h({n})={h} (witness + complete bounded exhaustion). Finite evidence, not an asymptotic proof."),
        );
    }

    let proof_obligations = proof_obligations_160(largest_exact.as_ref(), first_inconclusive);
    let lean_skeleton = lean_skeleton_160(largest_exact.as_ref());
    let arena = arena_cases_160(largest_exact.as_ref(), first_inconclusive, monotonicity_violation);

    let mut warnings = vec![
        "The Lean bridge emits proof skeletons and obligations; it does not claim a machine-checked proof.".to_string(),
        "The finite search engine is exponential and bounded. Inconclusive means exactly that.".to_string(),
        "Erdos #160 is OPEN and asks for the ASYMPTOTIC growth of h(N); it cannot be resolved with a finite computation. Computing small h(N) values does not prove or estimate the asymptotic and does not resolve Erdos #160.".to_string(),
    ];
    if let Some((n, prev, cur)) = monotonicity_violation {
        warnings.push(format!(
            "Monotonicity assertion failed at N={n}: h({})={prev} > h({n})={cur}. h(N) must be non-decreasing, so this is a kernel bug.",
            n - 1
        ));
    }

    ProofLabReport {
        problem: "Erdos #160 (OPEN): smallest k colouring h(N) such that every four-term arithmetic progression in {1..N} has at least three distinct colours; the open question is the asymptotic growth of h(N).".to_string(),
        summary,
        components: components(),
        finite_search,
        exact_h,
        conjectures,
        proof_obligations,
        lean_skeleton,
        combinatorics_kernels: vec![
            "comb160.apTuples(n, 4): enumerate 4-term APs in {1..n}".to_string(),
            "comb160.badAp(colors): return the first 4-AP with fewer than three distinct colours".to_string(),
            "comb160.isGoodColoring(colors): verify every 4-AP has >= 3 distinct colours".to_string(),
            "comb160.h160(n, node_limit): bounded min-colour search -> witness / exhaustion / inconclusive".to_string(),
        ],
        arena,
        warnings,
    }
}

pub fn render_json(report: &ProofLabReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

pub fn render_markdown(report: &ProofLabReport) -> String {
    let mut out = String::new();
    out.push_str("# Syntra Proof Lab Report\n\n");
    out.push_str("## Problem\n\n");
    out.push_str(&report.problem);
    out.push_str("\n\n## Summary\n\n");
    out.push_str(&report.summary);
    out.push_str("\n\n## Six Systems\n\n");
    for component in &report.components {
        out.push_str(&format!(
            "- **{}** (`{}`): {}\n",
            component.name, component.status, component.gives_us
        ));
    }
    out.push_str("\n## Finite Search\n\n");
    out.push_str("| N | k | status | nodes | witness |\n");
    out.push_str("|---|---|---|---:|---|\n");
    for case in &report.finite_search {
        let witness = case
            .coloring_1_based
            .as_ref()
            .map(|colors| format!("{colors:?}"))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            case.n, case.k, case.status, case.nodes, witness
        ));
    }
    out.push_str("\n## Conjectures\n\n");
    for conjecture in &report.conjectures {
        out.push_str(&format!("- {conjecture}\n"));
    }
    out.push_str("\n## Proof Obligations\n\n");
    for obligation in &report.proof_obligations {
        out.push_str(&format!(
            "- **{}** (`{}`): {}\n",
            obligation.id, obligation.status, obligation.statement
        ));
    }
    out.push_str("\n## Lean Skeleton\n\n```lean\n");
    out.push_str(&report.lean_skeleton);
    out.push_str("\n```\n\n## Warnings\n\n");
    for warning in &report.warnings {
        out.push_str(&format!("- {warning}\n"));
    }
    out
}

fn status_label(status: &SearchStatus) -> &'static str {
    match status {
        SearchStatus::Exists => "exists",
        SearchStatus::Unsat => "unsat",
        SearchStatus::Inconclusive => "inconclusive",
    }
}

fn components() -> Vec<ComponentStatus> {
    vec![
        ComponentStatus {
            name: "Finite Search Engine".to_string(),
            status: "implemented_v1".to_string(),
            gives_us: "bounded exact search with witnesses, unsat rows, and node-limit honesty"
                .to_string(),
        },
        ComponentStatus {
            name: "Conjecture Miner".to_string(),
            status: "implemented_v1".to_string(),
            gives_us: "turns search traces into explicit finite claims and non-claims".to_string(),
        },
        ComponentStatus {
            name: "Proof Obligation Generator".to_string(),
            status: "implemented_v1".to_string(),
            gives_us: "names the lemmas an expert or prover must discharge".to_string(),
        },
        ComponentStatus {
            name: "Lean/Coq Bridge".to_string(),
            status: "lean_skeleton_v1".to_string(),
            gives_us: "exports formalization starting points without pretending they are checked"
                .to_string(),
        },
        ComponentStatus {
            name: "Combinatorics Kernel Pack".to_string(),
            status: "implemented_v1".to_string(),
            gives_us:
                "native Lycan kernels for AP enumeration, verification, violations, and search"
                    .to_string(),
        },
        ComponentStatus {
            name: "Proof Arena".to_string(),
            status: "implemented_v1".to_string(),
            gives_us:
                "a repeatable benchmark harness for where the system succeeds, slows, or refuses"
                    .to_string(),
        },
    ]
}

fn proof_obligations(
    k: usize,
    exact_h: Option<usize>,
    largest_witness: Option<&(usize, Vec<usize>)>,
) -> Vec<ProofObligation> {
    let mut obligations = vec![
        ProofObligation {
            id: "definition_hk".to_string(),
            statement: "Formalize H(k) as the least N such that every finite coloring of {1..N} has a monochromatic or rainbow k-AP.".to_string(),
            status: "open".to_string(),
        },
        ProofObligation {
            id: "vdw_lower_bound_transfer".to_string(),
            statement: "Prove that any (k-1)-coloring avoiding monochromatic k-APs is automatically rainbow-free and therefore gives H(k) lower bounds.".to_string(),
            status: "open".to_string(),
        },
    ];

    if let Some((n, colors)) = largest_witness {
        obligations.push(ProofObligation {
            id: format!("witness_good_coloring_n{n}_k{k}"),
            statement: format!(
                "Check witness {:?} has no monochromatic or rainbow {k}-term AP on {{1..{n}}}.",
                colors
            ),
            status: "machine_checkable_candidate".to_string(),
        });
    }
    if let Some(h) = exact_h {
        obligations.push(ProofObligation {
            id: format!("exhaustive_unsat_n{h}_k{k}"),
            statement: format!(
                "Prove every coloring of {{1..{h}}} contains a monochromatic or rainbow {k}-term AP."
            ),
            status: "requires_formal_exhaustion_or_theorem".to_string(),
        });
    } else {
        obligations.push(ProofObligation {
            id: format!("extend_or_bound_h{k}"),
            statement: format!(
                "Either construct a larger good coloring for k={k}, prove unsat at the next N, or switch to a structural theorem."
            ),
            status: "open".to_string(),
        });
    }
    obligations.push(ProofObligation {
        id: "asymptotic_bridge".to_string(),
        statement:
            "Connect finite lower-bound constructions to the asymptotic claim H(k)^(1/k) / k -> infinity."
                .to_string(),
        status: "expert_theorem_required".to_string(),
    });
    obligations
}

fn lean_skeleton(
    k: usize,
    exact_h: Option<usize>,
    largest_witness: Option<&(usize, Vec<usize>)>,
) -> String {
    let witness = largest_witness
        .map(|(_, colors)| {
            colors
                .iter()
                .map(|color| color.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let claim = exact_h
        .map(|h| format!("-- Candidate finite theorem: H({k}) = {h}"))
        .unwrap_or_else(|| {
            format!("-- Candidate finite lower bound for k={k}; exact H({k}) not found.")
        });

    format!(
        r#"/- Generated by Syntra proof-lab.
   Skeleton only: replace `sorry` with real proofs before treating this as checked. -/

def IsArithmeticProgression (xs : List Nat) : Prop := sorry
def Monochromatic (color : Nat -> Nat) (xs : List Nat) : Prop := sorry
def Rainbow (color : Nat -> Nat) (xs : List Nat) : Prop := sorry
def GoodColoring (k n : Nat) (color : Nat -> Nat) : Prop := sorry
def H (k : Nat) : Nat := sorry

def erdos190Witness : List Nat := [{witness}]

{claim}

theorem witness_has_no_bad_ap :
  True := by
  -- TODO: expand erdos190Witness into a color function and check every {k}-term AP.
  trivial

theorem finite_exhaustion_obligation :
  True := by
  -- TODO: prove the unsat row structurally or import a verified exhaustive certificate.
  trivial
"#
    )
}

fn arena_cases(
    k: usize,
    exact_h: Option<usize>,
    largest_witness: Option<&(usize, Vec<usize>)>,
    first_inconclusive: Option<usize>,
) -> Vec<ArenaCase> {
    let finite = if let Some(h) = exact_h {
        ArenaCase {
            name: "finite_exact_search".to_string(),
            result: format!("H({k}) = {h} in this run"),
            interpretation: "good regression target for proof-lab machinery".to_string(),
        }
    } else if let Some((n, _)) = largest_witness {
        ArenaCase {
            name: "finite_lower_bound_search".to_string(),
            result: format!("H({k}) > {n} within this run"),
            interpretation: "useful evidence, not a proof of the asymptotic theorem".to_string(),
        }
    } else {
        ArenaCase {
            name: "finite_search".to_string(),
            result: "no witness found".to_string(),
            interpretation: "inspect node limits before drawing conclusions".to_string(),
        }
    };

    let refusal = ArenaCase {
        name: "honest_refusal".to_string(),
        result: first_inconclusive
            .map(|n| format!("node limit reached at N={n}"))
            .unwrap_or_else(|| "no node-limit refusal in this bounded run".to_string()),
        interpretation: "the arena records where computation stops instead of overclaiming"
            .to_string(),
    };

    vec![finite, refusal]
}

fn proof_obligations_160(
    largest_exact: Option<&(usize, usize, Vec<usize>)>,
    first_inconclusive: Option<usize>,
) -> Vec<ProofObligation> {
    let mut obligations = vec![
        ProofObligation {
            id: "definition_h160".to_string(),
            statement: "Formalize h(N) as the least k such that {1..N} admits a k-colouring in which every four-term arithmetic progression has at least three distinct colours.".to_string(),
            status: "open".to_string(),
        },
        ProofObligation {
            id: "floor_three_for_n_ge_4".to_string(),
            statement: "Prove that for N >= 4 any colouring with fewer than three colours has a four-term AP with at most two distinct colours, hence h(N) >= 3.".to_string(),
            status: "machine_checkable_candidate".to_string(),
        },
        ProofObligation {
            id: "monotonicity_h160".to_string(),
            statement: "Prove h(N) <= h(N+1): a valid colouring of {1..N+1} restricts to a valid colouring of {1..N}.".to_string(),
            status: "machine_checkable_candidate".to_string(),
        },
    ];

    if let Some((n, h, colors)) = largest_exact {
        obligations.push(ProofObligation {
            id: format!("witness_good_coloring_n{n}_h{h}"),
            statement: format!(
                "Check witness {colors:?} colours {{1..{n}}} so every four-term AP has at least three distinct colours (upper bound h({n}) <= {h})."
            ),
            status: "machine_checkable_candidate".to_string(),
        });
        obligations.push(ProofObligation {
            id: format!("exhaustive_unsat_n{n}_colors{}", h - 1),
            statement: format!(
                "Prove every {}-colouring of {{1..{n}}} has some four-term AP with at most two distinct colours (lower bound h({n}) >= {h}); together with the witness this gives h({n}) = {h}.",
                h - 1
            ),
            status: "requires_formal_exhaustion_or_theorem".to_string(),
        });
    } else {
        let where_str = first_inconclusive
            .map(|n| format!(" (search went inconclusive at N={n})"))
            .unwrap_or_default();
        obligations.push(ProofObligation {
            id: "extend_or_bound_h160".to_string(),
            statement: format!(
                "Either resolve an exact h(N) within budget, prove the next exhaustion, or switch to a structural theorem{where_str}."
            ),
            status: "open".to_string(),
        });
    }

    // LOAD-BEARING: the open question is the asymptotic estimate, which a finite
    // computation cannot discharge. Mirror the #190 asymptotic_bridge refusal.
    obligations.push(ProofObligation {
        id: "asymptotic_estimate_h160".to_string(),
        statement: "Estimate the asymptotic growth of h(N) (the open Erdos #160 question). Known frontier: h(N) << N^{2/3}; Hunter << N^{(log3/log22)+o(1)} ~ N^{0.355}. This cannot be resolved by a finite computation.".to_string(),
        status: "expert_theorem_required".to_string(),
    });

    obligations
}

fn lean_skeleton_160(largest_exact: Option<&(usize, usize, Vec<usize>)>) -> String {
    let witness = largest_exact
        .map(|(_, _, colors)| {
            colors
                .iter()
                .map(|color| color.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let (witness_n, claim) = match largest_exact {
        Some((n, h, _)) => (
            *n,
            format!("-- Candidate finite theorem: h({n}) = {h} (witness above + bounded exhaustion)."),
        ),
        None => (
            0,
            "-- No exact h(N) resolved within the node budget for this run.".to_string(),
        ),
    };

    format!(
        r#"/- Generated by Syntra proof-lab (Erdos #160).
   Erdos #160 is OPEN: the asymptotic growth of h(N) cannot be resolved by a
   finite computation. This file is a skeleton for the *finite* facts only;
   replace `sorry`/`trivial` with real proofs before treating it as checked. -/

def IsArithmeticProgression (xs : List Nat) : Prop := sorry
def DistinctColors (color : Nat -> Nat) (xs : List Nat) : Nat := sorry
-- A 4-term AP is "good for #160" when it carries at least three distinct colours.
def GoodAp160 (color : Nat -> Nat) (xs : List Nat) : Prop := DistinctColors color xs >= 3
def GoodColoring160 (n : Nat) (color : Nat -> Nat) : Prop := sorry
def h160 (n : Nat) : Nat := sorry

def erdos160Witness : List Nat := [{witness}]

{claim}

theorem witness_has_three_colours_per_ap :
  True := by
  -- TODO: expand erdos160Witness into a colour function on {{1..{witness_n}}}
  -- and check every four-term AP has at least three distinct colours.
  trivial

theorem finite_exhaustion_obligation :
  True := by
  -- TODO: prove the (h-1)-colour exhaustion structurally or import a verified
  -- exhaustive certificate; this is the lower-bound half of h(N) = c.
  trivial

-- NOTE: There is deliberately no theorem here about the asymptotic growth of
-- h(N). That is the open Erdos #160 question and requires an expert theorem
-- (known frontier h(N) << N^(2/3); Hunter ~ N^0.355). Finite search cannot
-- supply it.
"#
    )
}

fn arena_cases_160(
    largest_exact: Option<&(usize, usize, Vec<usize>)>,
    first_inconclusive: Option<usize>,
    monotonicity_violation: Option<(usize, usize, usize)>,
) -> Vec<ArenaCase> {
    let finite = match largest_exact {
        Some((n, h, _)) => ArenaCase {
            name: "finite_exact_search".to_string(),
            result: format!("h({n}) = {h} in this run"),
            interpretation: "good regression target; finite evidence, not an asymptotic estimate"
                .to_string(),
        },
        None => ArenaCase {
            name: "finite_search".to_string(),
            result: "no exact value resolved".to_string(),
            interpretation: "inspect node limits before drawing conclusions".to_string(),
        },
    };

    let refusal = ArenaCase {
        name: "honest_refusal".to_string(),
        result: first_inconclusive
            .map(|n| format!("node limit reached at N={n}"))
            .unwrap_or_else(|| "no node-limit refusal in this bounded run".to_string()),
        interpretation: "the arena records where computation stops instead of overclaiming"
            .to_string(),
    };

    let asymptotic = ArenaCase {
        name: "asymptotic_refusal".to_string(),
        result: "not attempted".to_string(),
        interpretation: "Erdos #160 asymptotic estimate is open and cannot be resolved by finite search; the command never emits an asymptotic claim".to_string(),
    };

    let monotonicity = ArenaCase {
        name: "monotonicity_assertion".to_string(),
        result: match monotonicity_violation {
            Some((n, prev, cur)) => format!("VIOLATED at N={n}: h({})={prev} > h({n})={cur}", n - 1),
            None => "holds across computed rows".to_string(),
        },
        interpretation: "h(N) must be non-decreasing; a violation indicates a kernel bug, not a result".to_string(),
    };

    vec![finite, refusal, monotonicity, asymptotic]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erdos190_h3_report_finds_exact_value() {
        let report = run_erdos190(3, 9, 20_000);
        assert_eq!(report.exact_h, Some(9));
        assert_eq!(report.components.len(), 6);
        assert!(
            report
                .finite_search
                .iter()
                .any(|case| case.n == 8 && case.status == "exists")
        );
        assert!(
            report
                .finite_search
                .iter()
                .any(|case| case.n == 9 && case.status == "unsat")
        );
        assert!(
            report
                .lean_skeleton
                .contains("Generated by Syntra proof-lab")
        );
    }

    #[test]
    fn markdown_mentions_proof_obligations_and_warnings() {
        let report = run_erdos190(3, 9, 20_000);
        let markdown = render_markdown(&report);
        assert!(markdown.contains("## Proof Obligations"));
        assert!(markdown.contains("finite_exhaustion_obligation"));
        assert!(markdown.contains("does not claim a machine-checked proof"));
    }

    // ----- Erdos #160 -----

    #[test]
    fn erdos160_reproduces_verified_h_values() {
        let report = run_erdos160(18, 1_000_000);
        // Latest exact value over 1..=18 is h(18) = 4.
        assert_eq!(report.exact_h, Some(4));
        assert_eq!(report.finite_search.len(), 18);
        assert_eq!(report.components.len(), 6);

        // Pull the exact h(N) from each row's "witness h(N)=c" status.
        let expected = [
            1usize, 1, 1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4,
        ];
        for (idx, case) in report.finite_search.iter().enumerate() {
            let n = idx + 1;
            assert_eq!(case.n, n);
            assert_eq!(case.k, 4, "AP length is 4 for #160");
            let want = expected[idx];
            assert_eq!(
                case.status,
                format!("witness h({n})={want}"),
                "row {n}: {case:?}"
            );
            // Witness present and uses exactly `want` colours.
            let witness = case.coloring_1_based.as_ref().expect("witness present");
            assert!(lycan::combinatorics::is_good_coloring_160(witness), "row {n}");
        }
    }

    #[test]
    fn erdos160_jump_at_13_has_witness_and_exhaustion_obligation() {
        let report = run_erdos160(13, 1_000_000);
        assert_eq!(report.exact_h, Some(4));
        // Finite exact claim is present as a conjecture/non-claim line.
        assert!(
            report
                .conjectures
                .iter()
                .any(|c| c.contains("Finite exact: h(13)=4")),
            "{:?}",
            report.conjectures
        );
        // The exhaustion (lower-bound) obligation for 3 colours at N=13 exists.
        assert!(
            report
                .proof_obligations
                .iter()
                .any(|o| o.id == "exhaustive_unsat_n13_colors3"
                    && o.status == "requires_formal_exhaustion_or_theorem"),
            "{:?}",
            report.proof_obligations
        );
    }

    #[test]
    fn erdos160_asymptotic_obligation_is_expert_theorem_required() {
        let report = run_erdos160(12, 1_000_000);
        let asymptotic = report
            .proof_obligations
            .iter()
            .find(|o| o.id == "asymptotic_estimate_h160")
            .expect("asymptotic obligation present");
        assert_eq!(asymptotic.status, "expert_theorem_required");
        assert!(asymptotic.statement.contains("asymptotic"));
        // Never an asymptotic CLAIM: every result is labelled finite evidence.
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("does not resolve Erdos #160"))
        );
        // The arena explicitly refuses the asymptotic.
        assert!(
            report
                .arena
                .iter()
                .any(|a| a.name == "asymptotic_refusal")
        );
    }

    #[test]
    fn erdos160_reports_inconclusive_without_a_bound() {
        // A 1-node budget cannot complete the c=1 search at N=13.
        let report = run_erdos160(13, 1);
        // No exact value anywhere.
        assert_eq!(report.exact_h, None);
        // At least one row is reported as inconclusive (not as a bound).
        assert!(
            report
                .finite_search
                .iter()
                .any(|c| c.status == "inconclusive"),
            "{:?}",
            report.finite_search
        );
    }

    #[test]
    fn erdos160_markdown_and_lean_mirror_190_shape() {
        let report = run_erdos160(13, 1_000_000);
        let markdown = render_markdown(&report);
        assert!(markdown.contains("## Proof Obligations"));
        assert!(markdown.contains("asymptotic_estimate_h160"));
        assert!(markdown.contains("Erdos #160 is OPEN"));
        assert!(report.lean_skeleton.contains("Generated by Syntra proof-lab"));
        assert!(report.lean_skeleton.contains("finite_exhaustion_obligation"));
        // No asymptotic theorem is emitted in the Lean starter.
        assert!(report.lean_skeleton.contains("no theorem here about the asymptotic"));
    }
}
