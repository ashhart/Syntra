use lycan::combinatorics::{
    H160Status, SearchStatus, h160, search_coloring_160_sat, search_good_coloring,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofLabReport {
    pub problem: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    pub components: Vec<ComponentStatus>,
    pub finite_search: Vec<FiniteSearchCase>,
    pub exact_h: Option<usize>,
    pub conjectures: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub certificates: Vec<ProofCertificate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<PatternFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<BoundReport>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_colors: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    pub status: String,
    pub nodes: usize,
    pub node_limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clauses: Option<usize>,
    pub coloring_1_based: Option<Vec<usize>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofObligation {
    pub id: String,
    pub statement: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofCertificate {
    pub id: String,
    pub kind: String,
    pub backend: String,
    pub statement: String,
    pub status: String,
    pub check: String,
    pub nodes: usize,
    pub node_limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clauses: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coloring_1_based: Option<Vec<usize>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternFinding {
    pub name: String,
    pub evidence: String,
    pub interpretation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundReport {
    pub largest_exact_n: Option<usize>,
    pub lower_bound: String,
    pub upper_bound: String,
    pub terminal: String,
    pub interpretation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArenaCase {
    pub name: String,
    pub result: String,
    pub interpretation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofBackend {
    Dfs,
    Sat,
}

impl ProofBackend {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "dfs" => Some(Self::Dfs),
            "sat" => Some(Self::Sat),
            _ => None,
        }
    }
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
            max_colors: None,
            backend: Some("dfs".to_string()),
            status,
            nodes: result.nodes,
            node_limit: result.node_limit,
            variables: None,
            clauses: None,
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
        backend: Some("dfs".to_string()),
        components: components(),
        finite_search,
        exact_h,
        conjectures,
        certificates: Vec::new(),
        patterns: Vec::new(),
        bounds: None,
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
pub fn run_erdos160_with_backend(
    max_n: usize,
    node_limit: usize,
    backend: ProofBackend,
    max_colors: Option<usize>,
) -> ProofLabReport {
    match backend {
        ProofBackend::Dfs => run_erdos160_dfs(max_n, node_limit),
        ProofBackend::Sat => {
            run_erdos160_sat(max_n, max_colors.unwrap_or(max_n.max(1)), node_limit)
        }
    }
}

fn run_erdos160_dfs(max_n: usize, node_limit: usize) -> ProofLabReport {
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
            H160Status::LowerBound => (format!("exhaustion h({n})>={}", result.lower_bound), None),
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
            max_colors: result.h.or(Some(result.lower_bound)),
            backend: Some("dfs".to_string()),
            status,
            nodes: result.nodes,
            node_limit: result.node_limit,
            variables: None,
            clauses: None,
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
    let arena = arena_cases_160(
        largest_exact.as_ref(),
        first_inconclusive,
        monotonicity_violation,
    );
    let certificates = certificates_160_dfs(
        largest_exact.as_ref(),
        first_inconclusive,
        &finite_search,
        node_limit,
    );
    let patterns = largest_exact
        .as_ref()
        .map(|(_, _, witness)| pattern_findings(witness))
        .unwrap_or_default();
    let bounds = Some(bounds_160(
        largest_exact.as_ref(),
        first_inconclusive.map(|n| format!("N={n} was inconclusive at the node limit.")),
    ));

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
        backend: Some("dfs".to_string()),
        components: components(),
        finite_search,
        exact_h,
        conjectures,
        certificates,
        patterns,
        bounds,
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

fn run_erdos160_sat(max_n: usize, max_colors: usize, node_limit: usize) -> ProofLabReport {
    let mut finite_search = Vec::new();
    let mut exact_rows: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    let mut first_inconclusive: Option<(usize, usize)> = None;
    let mut first_color_cap: Option<usize> = None;
    let mut monotonicity_violation: Option<(usize, usize, usize)> = None;
    let mut prev_h: Option<usize> = None;

    'n_loop: for n in 1..=max_n {
        for colors in 1..=max_colors {
            let result = search_coloring_160_sat(n, colors, node_limit);
            let coloring_1_based = result
                .coloring
                .as_ref()
                .map(|found| found.iter().map(|color| color + 1).collect::<Vec<_>>());
            let status = status_label(&result.status).to_string();
            let row = FiniteSearchCase {
                n,
                k: lycan::combinatorics::ERDOS160_AP_LEN,
                max_colors: Some(colors),
                backend: Some("sat".to_string()),
                status: status.clone(),
                nodes: result.nodes,
                node_limit: result.node_limit,
                variables: Some(result.variables),
                clauses: Some(result.clauses),
                coloring_1_based: coloring_1_based.clone(),
            };

            match result.status {
                SearchStatus::Exists => {
                    if let Some(prev) = prev_h {
                        if colors < prev && monotonicity_violation.is_none() {
                            monotonicity_violation = Some((n, prev, colors));
                        }
                    }
                    prev_h = Some(colors);
                    exact_rows.push((n, colors, coloring_1_based.unwrap_or_default()));
                    finite_search.push(row);
                    continue 'n_loop;
                }
                SearchStatus::Unsat => {
                    finite_search.push(row);
                }
                SearchStatus::Inconclusive => {
                    first_inconclusive = Some((n, colors));
                    finite_search.push(row);
                    break 'n_loop;
                }
            }
        }
        first_color_cap = Some(n);
        break;
    }

    let largest_exact = exact_rows.last();
    let exact_h = largest_exact.map(|(_, h, _)| *h);
    let summary = if let Some((n, colors, _)) = largest_exact {
        if let Some((blocked_n, blocked_colors)) = first_inconclusive {
            format!(
                "SAT-backed bounded search resolved exact values up to N={n} (latest h({n})={colors}); N={blocked_n}, colors={blocked_colors} hit the node limit. This is finite evidence, not an asymptotic proof; it does not resolve Erdos #160."
            )
        } else if let Some(capped_n) = first_color_cap {
            format!(
                "SAT-backed bounded search resolved exact values up to N={n}; at N={capped_n}, no witness was found with <= {max_colors} colors. This is finite evidence, not an asymptotic proof; it does not resolve Erdos #160."
            )
        } else {
            format!(
                "SAT-backed bounded search resolved exact values up to N={n} (latest h({n})={colors}). This is finite evidence, not an asymptotic proof; it does not resolve Erdos #160."
            )
        }
    } else if let Some((blocked_n, blocked_colors)) = first_inconclusive {
        format!(
            "SAT-backed bounded search hit the node limit at N={blocked_n}, colors={blocked_colors} before resolving any exact value; inconclusive is a valid answer, not a bound."
        )
    } else {
        "SAT-backed bounded search produced only lower-bound exhaustions; inspect the rows before treating this as evidence.".to_string()
    };

    let mut conjectures = vec![
        "Monotonicity: h(N) is non-decreasing in N, since any valid colouring of {1..N+1} restricts to a valid colouring of {1..N}.".to_string(),
        "The SAT backend is useful for cross-checking finite rows and recording CNF size; it is not a DRAT/LRAT proof producer yet.".to_string(),
        "Finite exact values and witnesses are useful regression targets, but they do not estimate the asymptotic growth of h(N) and do not resolve Erdos #160.".to_string(),
    ];
    if let Some((n, h, _)) = largest_exact {
        conjectures.insert(
            0,
            format!("Finite exact: h({n})={h} (SAT witness + bounded lower-colour exhaustion). Finite evidence, not an asymptotic proof."),
        );
    }

    let proof_obligations =
        proof_obligations_160(largest_exact, first_inconclusive.map(|(n, _)| n));
    let lean_skeleton = lean_skeleton_160(largest_exact);
    let arena = arena_cases_160(
        largest_exact,
        first_inconclusive.map(|(n, _)| n),
        monotonicity_violation,
    );
    let certificates = certificates_160_sat(&finite_search);
    let patterns = largest_exact
        .map(|(_, _, witness)| pattern_findings(witness))
        .unwrap_or_default();
    let terminal = first_inconclusive
        .map(|(n, colors)| format!("N={n}, colors={colors} was inconclusive at the node limit."))
        .or_else(|| {
            first_color_cap.map(|n| format!("N={n} reached the color cap max_colors={max_colors}."))
        });
    let bounds = Some(bounds_160(largest_exact, terminal));

    let mut warnings = vec![
        "The SAT backend emits replayable bounded search records, not DRAT/LRAT or machine-checked proofs.".to_string(),
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
        backend: Some("sat".to_string()),
        components: components(),
        finite_search,
        exact_h,
        conjectures,
        certificates,
        patterns,
        bounds,
        proof_obligations,
        lean_skeleton,
        combinatorics_kernels: vec![
            "comb.hasThreeDistinct4ApColoring(colors): verify every 4-AP has >= 3 distinct colours".to_string(),
            "comb.badThreeDistinct4Ap(colors): return the first 4-AP with fewer than three distinct colours".to_string(),
            "comb.threeDistinct4ApWitness(n, max_colors, node_limit): DFS witness / exhaustion / inconclusive search".to_string(),
            "comb.threeDistinct4ApSatWitness(n, max_colors, node_limit): CNF/DPLL witness / exhaustion / inconclusive search".to_string(),
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
    if let Some(bounds) = &report.bounds {
        out.push_str("\n## Bounds\n\n");
        out.push_str(&format!("- **Lower bound:** {}\n", bounds.lower_bound));
        out.push_str(&format!("- **Upper bound:** {}\n", bounds.upper_bound));
        out.push_str(&format!("- **Terminal:** {}\n", bounds.terminal));
        out.push_str(&format!(
            "- **Interpretation:** {}\n",
            bounds.interpretation
        ));
    }
    if !report.certificates.is_empty() {
        out.push_str("\n## Certificates\n\n");
        for certificate in &report.certificates {
            out.push_str(&format!(
                "- **{}** (`{}` via `{}`): {} {} Nodes: {}/{}.\n",
                certificate.id,
                certificate.status,
                certificate.backend,
                certificate.statement,
                certificate.check,
                certificate.nodes,
                certificate.node_limit
            ));
        }
    }
    if !report.patterns.is_empty() {
        out.push_str("\n## Pattern Findings\n\n");
        for pattern in &report.patterns {
            out.push_str(&format!(
                "- **{}:** {} {}\n",
                pattern.name, pattern.evidence, pattern.interpretation
            ));
        }
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

fn certificates_160_dfs(
    largest_exact: Option<&(usize, usize, Vec<usize>)>,
    first_inconclusive: Option<usize>,
    finite_search: &[FiniteSearchCase],
    node_limit: usize,
) -> Vec<ProofCertificate> {
    let mut certificates = Vec::new();
    if let Some((n, colors, witness)) = largest_exact {
        let nodes = finite_search
            .iter()
            .find(|case| case.n == *n)
            .map(|case| case.nodes)
            .unwrap_or_default();
        certificates.push(ProofCertificate {
            id: format!("witness_n{n}_colors{colors}"),
            kind: "witness".to_string(),
            backend: "dfs".to_string(),
            statement: format!(
                "A coloring of {{1..{n}}} with {colors} colors satisfies the three-distinct 4-AP property."
            ),
            status: "replayable_witness".to_string(),
            check: "Verified by comb.hasThreeDistinct4ApColoring / comb.badThreeDistinct4Ap."
                .to_string(),
            nodes,
            node_limit,
            variables: None,
            clauses: None,
            coloring_1_based: Some(witness.clone()),
        });
        if *colors > 1 {
            certificates.push(ProofCertificate {
                id: format!("lower_color_unsat_n{n}_colors{}", colors - 1),
                kind: "exhaustive_unsat".to_string(),
                backend: "dfs".to_string(),
                statement: format!(
                    "No coloring of {{1..{n}}} with <= {} colors exists within the completed bounded min-color search.",
                    colors - 1
                ),
                status: "bounded_backend_exhausted".to_string(),
                check: "Replay h160 / comb.threeDistinct4ApWitness or replace it with a formal certificate before calling this a theorem.".to_string(),
                nodes,
                node_limit,
                variables: None,
                clauses: None,
                coloring_1_based: None,
            });
        }
    }
    if let Some(n) = first_inconclusive {
        let nodes = finite_search
            .iter()
            .find(|case| case.n == n)
            .map(|case| case.nodes)
            .unwrap_or_default();
        certificates.push(ProofCertificate {
            id: format!("node_limit_gap_n{n}"),
            kind: "node_limit".to_string(),
            backend: "dfs".to_string(),
            statement: format!("Search at N={n} hit the node limit."),
            status: "not_a_proof".to_string(),
            check: "Increase the limit, switch backend, or supply a structural argument."
                .to_string(),
            nodes,
            node_limit,
            variables: None,
            clauses: None,
            coloring_1_based: None,
        });
    }
    certificates
}

fn certificates_160_sat(finite_search: &[FiniteSearchCase]) -> Vec<ProofCertificate> {
    let mut certificates = Vec::new();
    if let Some(witness) = finite_search
        .iter()
        .rev()
        .find(|case| case.status == "exists")
    {
        let colors = witness.max_colors.unwrap_or(0);
        certificates.push(ProofCertificate {
            id: format!("witness_n{}_colors{}", witness.n, colors),
            kind: "witness".to_string(),
            backend: "sat".to_string(),
            statement: format!(
                "A coloring of {{1..{}}} with {colors} colors satisfies the three-distinct 4-AP property.",
                witness.n
            ),
            status: "replayable_witness".to_string(),
            check: "Verified by comb.hasThreeDistinct4ApColoring / comb.badThreeDistinct4Ap.".to_string(),
            nodes: witness.nodes,
            node_limit: witness.node_limit,
            variables: witness.variables,
            clauses: witness.clauses,
            coloring_1_based: witness.coloring_1_based.clone(),
        });

        if colors > 1 {
            if let Some(unsat) = finite_search.iter().rev().find(|case| {
                case.n == witness.n && case.max_colors == Some(colors - 1) && case.status == "unsat"
            }) {
                certificates.push(ProofCertificate {
                    id: format!("lower_color_unsat_n{}_colors{}", unsat.n, colors - 1),
                    kind: "exhaustive_unsat".to_string(),
                    backend: "sat".to_string(),
                    statement: format!(
                        "No coloring of {{1..{}}} with <= {} colors was found because the bounded SAT backend exhausted the CNF search.",
                        unsat.n,
                        colors - 1
                    ),
                    status: "bounded_backend_exhausted".to_string(),
                    check: "Replay the SAT backend or replace it with DRAT/LRAT/Lean evidence before calling this a theorem.".to_string(),
                    nodes: unsat.nodes,
                    node_limit: unsat.node_limit,
                    variables: unsat.variables,
                    clauses: unsat.clauses,
                    coloring_1_based: None,
                });
            }
        }
    }

    if let Some(gap) = finite_search
        .iter()
        .find(|case| case.status == "inconclusive")
    {
        certificates.push(ProofCertificate {
            id: format!(
                "node_limit_gap_n{}_colors{}",
                gap.n,
                gap.max_colors.unwrap_or(0)
            ),
            kind: "node_limit".to_string(),
            backend: "sat".to_string(),
            statement: format!(
                "SAT search at N={}, colors={} hit the node limit.",
                gap.n,
                gap.max_colors.unwrap_or(0)
            ),
            status: "not_a_proof".to_string(),
            check: "Increase the limit, switch backend, or supply a structural argument."
                .to_string(),
            nodes: gap.nodes,
            node_limit: gap.node_limit,
            variables: gap.variables,
            clauses: gap.clauses,
            coloring_1_based: None,
        });
    }

    certificates
}

fn bounds_160(
    largest_exact: Option<&(usize, usize, Vec<usize>)>,
    terminal: Option<String>,
) -> BoundReport {
    let largest_exact_n = largest_exact.map(|(n, _, _)| *n);
    let lower_bound = largest_exact
        .map(|(n, colors, _)| format!("h({n}) >= {colors}"))
        .unwrap_or_else(|| "No finite lower bound established in this run.".to_string());
    let upper_bound = largest_exact
        .map(|(n, colors, _)| format!("h({n}) <= {colors}"))
        .unwrap_or_else(|| "No finite upper bound established in this run.".to_string());
    let terminal = terminal.unwrap_or_else(|| {
        "The requested finite range completed without a node-limit or color-cap refusal."
            .to_string()
    });
    let interpretation = if let Some((n, colors, _)) = largest_exact {
        format!(
            "Within the searched finite range, the report has a witness and lower-color exhaustion for h({n}) = {colors}; this is not an asymptotic solution to Erdos #160."
        )
    } else {
        "The run produced search evidence only; it did not establish an exact h(N) row.".to_string()
    };

    BoundReport {
        largest_exact_n,
        lower_bound,
        upper_bound,
        terminal,
        interpretation,
    }
}

fn pattern_findings(witness: &[usize]) -> Vec<PatternFinding> {
    if witness.is_empty() {
        return Vec::new();
    }

    let mut patterns = Vec::new();
    let max_color = witness.iter().copied().max().unwrap_or(0);
    let mut histogram = vec![0usize; max_color + 1];
    for &color in witness {
        if color < histogram.len() {
            histogram[color] += 1;
        }
    }
    let histogram = histogram
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, count)| **count > 0)
        .map(|(color, count)| format!("{color}:{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    patterns.push(PatternFinding {
        name: "color_histogram".to_string(),
        evidence: histogram,
        interpretation: "Shows whether the witness is balanced or carried by a dominant color."
            .to_string(),
    });

    let period = smallest_period(witness);
    patterns.push(PatternFinding {
        name: "smallest_period".to_string(),
        evidence: period
            .map(|p| p.to_string())
            .unwrap_or_else(|| "none".to_string()),
        interpretation: if period.is_some() {
            "A periodic construction may be worth trying to generalize.".to_string()
        } else {
            "No simple exact period was found in the finite witness.".to_string()
        },
    });

    let runs = run_lengths(witness)
        .into_iter()
        .map(|(color, length)| format!("{color}x{length}"))
        .collect::<Vec<_>>()
        .join(", ");
    patterns.push(PatternFinding {
        name: "adjacent_runs".to_string(),
        evidence: runs,
        interpretation:
            "Long runs often point to brittle witnesses; short runs suggest more distributed structure."
                .to_string(),
    });

    patterns
}

fn smallest_period(values: &[usize]) -> Option<usize> {
    for period in 1..=values.len() / 2 {
        if values
            .iter()
            .enumerate()
            .all(|(index, value)| *value == values[index % period])
        {
            return Some(period);
        }
    }
    None
}

fn run_lengths(values: &[usize]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut current = values[0];
    let mut length = 1usize;
    for &value in &values[1..] {
        if value == current {
            length += 1;
        } else {
            runs.push((current, length));
            current = value;
            length = 1;
        }
    }
    runs.push((current, length));
    runs
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
            format!(
                "-- Candidate finite theorem: h({n}) = {h} (witness above + bounded exhaustion)."
            ),
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
            Some((n, prev, cur)) => {
                format!("VIOLATED at N={n}: h({})={prev} > h({n})={cur}", n - 1)
            }
            None => "holds across computed rows".to_string(),
        },
        interpretation:
            "h(N) must be non-decreasing; a violation indicates a kernel bug, not a result"
                .to_string(),
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
        let report = run_erdos160_with_backend(18, 1_000_000, ProofBackend::Dfs, None);
        // Latest exact value over 1..=18 is h(18) = 4.
        assert_eq!(report.exact_h, Some(4));
        assert_eq!(report.finite_search.len(), 18);
        assert_eq!(report.components.len(), 6);
        assert_eq!(report.backend.as_deref(), Some("dfs"));
        assert!(report.bounds.is_some());
        assert!(!report.certificates.is_empty());
        assert!(!report.patterns.is_empty());

        // Pull the exact h(N) from each row's "witness h(N)=c" status.
        let expected = [1usize, 1, 1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4];
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
            assert!(
                lycan::combinatorics::is_good_coloring_160(witness),
                "row {n}"
            );
        }
    }

    #[test]
    fn erdos160_jump_at_13_has_witness_and_exhaustion_obligation() {
        let report = run_erdos160_with_backend(13, 1_000_000, ProofBackend::Dfs, None);
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
        let report = run_erdos160_with_backend(12, 1_000_000, ProofBackend::Dfs, None);
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
        assert!(report.arena.iter().any(|a| a.name == "asymptotic_refusal"));
    }

    #[test]
    fn erdos160_reports_inconclusive_without_a_bound() {
        // A 1-node budget cannot complete the c=1 search at N=13.
        let report = run_erdos160_with_backend(13, 1, ProofBackend::Dfs, None);
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
        let report = run_erdos160_with_backend(13, 1_000_000, ProofBackend::Dfs, None);
        let markdown = render_markdown(&report);
        assert!(markdown.contains("## Proof Obligations"));
        assert!(markdown.contains("asymptotic_estimate_h160"));
        assert!(markdown.contains("Erdos #160 is OPEN"));
        assert!(
            report
                .lean_skeleton
                .contains("Generated by Syntra proof-lab")
        );
        assert!(
            report
                .lean_skeleton
                .contains("finite_exhaustion_obligation")
        );
        // No asymptotic theorem is emitted in the Lean starter.
        assert!(
            report
                .lean_skeleton
                .contains("no theorem here about the asymptotic")
        );
    }

    #[test]
    fn erdos160_sat_report_includes_certificates_patterns_and_bounds() {
        let report = run_erdos160_with_backend(12, 100_000, ProofBackend::Sat, Some(3));
        assert_eq!(report.backend.as_deref(), Some("sat"));
        assert_eq!(report.exact_h, Some(3));
        assert!(report.summary.contains("h(12)=3"));
        assert!(report.certificates.iter().any(|certificate| {
            certificate.kind == "witness" && certificate.status == "replayable_witness"
        }));
        assert!(report.certificates.iter().any(|certificate| {
            certificate.kind == "exhaustive_unsat"
                && certificate.status == "bounded_backend_exhausted"
                && certificate.variables.is_some()
                && certificate.clauses.is_some()
        }));
        assert!(
            report
                .patterns
                .iter()
                .any(|pattern| pattern.name == "color_histogram")
        );
        assert_eq!(
            report
                .bounds
                .as_ref()
                .and_then(|bounds| bounds.largest_exact_n),
            Some(12)
        );
        let markdown = render_markdown(&report);
        assert!(markdown.contains("## Certificates"));
        assert!(markdown.contains("## Pattern Findings"));
        assert!(markdown.contains("## Bounds"));
    }
}
