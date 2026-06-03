use lycan::combinatorics::{SearchStatus, search_good_coloring};
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
}
