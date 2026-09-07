/// Lycan Improvement Protocol — adaptive discovery via `emit_brief` /
/// `apply_proposal`.

use crate::graph::*;
use crate::graph_executor::GraphExecutor;

/// Generate an improvement brief for all strategy nodes in a graph.
pub fn emit_brief(graph: &NeuralGraph) -> String {
    let mut briefs = Vec::new();

    for node in &graph.nodes {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) {
            continue;
        }
        if node.weights.is_empty() { continue; }

        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };

        let mut options = Vec::new();
        for i in 0..n_options {
            let mut tries: u64 = 0;
            let mut total_ns: u128 = 0;
            let mut correct: u64 = 0;

            if let Some(slot) = node.state_slot {
                let base = slot as usize + i * 3;
                if base + 2 < graph.state.len() {
                    tries = graph.state[base] as u64;
                    total_ns = graph.state[base + 1] as u128;
                    correct = graph.state[base + 2] as u64;
                }
            }

            let avg_ms = if tries > 0 {
                (total_ns as f64 / tries as f64) / 1_000_000.0
            } else { 0.0 };

            let correctness = if tries > 0 {
                correct as f64 / tries as f64
            } else { 0.0 };

            let weight = node.weights.get(i).copied().unwrap_or(0.0);

            options.push(format!(
                "    {{\"option\": {}, \"tries\": {}, \"avg_ms\": {:.3}, \"correct_rate\": {:.2}, \"weight\": {:.4}}}",
                i, tries, avg_ms, correctness, weight
            ));
        }

        let mut best_idx = 0;
        let mut best_w = 0.0f64;
        for i in 0..n_options {
            let w = node.weights.get(i).copied().unwrap_or(0.0);
            if w > best_w { best_w = w; best_idx = i; }
        }

        let contract_str = match node.contract {
            Contract::None => "None",
            Contract::SameOutput => "SameOutput",
            Contract::Validated => "Validated",
            Contract::WithinTolerance => "WithinTolerance",
        };

        let annotation = node.annotation
            .map(|idx| graph.get_string(idx))
            .unwrap_or_default();

        let brief = format!(r#"{{
  "target_strategy": {},
  "node_op": "{:?}",
  "contract": "{}",
  "total_activations": {},
  "current_winner": {},
  "current_winner_weight": {:.4},
  "n_options": {},{}
  "options": [
{}
  ],
  "goal": "Generate a faster correct option that satisfies the {} contract",
  "constraints": ["pure", "no IO", "must return same type as existing options"],
  "proposal_format": {{
    "name": "string — descriptive name for the new strategy",
    "source": "string — Lycan source code for a function that takes the same args",
    "insert_into_strategy": {}
  }}
}}"#,
            node.id, node.op, contract_str,
            node.activation_count, best_idx, best_w, n_options,
            if !annotation.is_empty() {
                format!("\n  \"description\": \"{}\",", annotation.replace('"', "\\\""))
            } else { String::new() },
            options.join(",\n"),
            contract_str, node.id,
        );

        briefs.push(brief);
    }

    if briefs.len() == 1 { briefs.remove(0) }
    else { format!("[{}]", briefs.join(",\n")) }
}

/// Result of applying a proposal.
#[derive(Debug)]
pub struct ProposalResult {
    pub accepted: bool,
    pub reason: String,
    pub candidate_ms: f64,
    pub winner_ms: f64,
    #[allow(dead_code)]
    pub candidate_correct: bool,
}

/// Proposal input parsed from JSON.
pub struct Proposal {
    pub name: String,
    pub source: String,
    pub target_strategy: u32,
    pub expected_output: Option<String>,
}

/// Detected weakness in a strategy node.
#[derive(Debug)]
pub struct WeaknessReport {
    pub node_id: u32,
    pub objective: String,
    pub contract: String,
    pub issue: String,
    pub reason: String,
    pub goal: String,
    pub options: Vec<OptionReport>,
    pub current_winner: usize,
    pub winner_weight: f64,
}

#[derive(Debug)]
pub struct OptionReport {
    pub index: usize,
    pub tries: u64,
    pub correct: u64,
    pub avg_ms: f64,
    pub weight: f64,
}

const MIN_TRIES_FOR_ANALYSIS: u64 = 3;
const CONFIDENCE_THRESHOLD: f64 = 0.75;
const FRAGILE_MARGIN: f64 = 0.15;

/// Analyze all strategy nodes and detect weaknesses.
pub fn improve_report(graph: &NeuralGraph) -> Vec<WeaknessReport> {
    let mut reports = Vec::new();

    for node in &graph.nodes {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
        if node.weights.is_empty() { continue; }

        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };
        if n_options == 0 { continue; }

        // Load stats
        let mut options = Vec::new();
        let mut total_tries: u64 = 0;
        for i in 0..n_options {
            let (tries, total_ns, correct) = if let Some(slot) = node.state_slot {
                let base = slot as usize + i * 3;
                if base + 2 < graph.state.len() {
                    (graph.state[base] as u64, graph.state[base + 1] as u128, graph.state[base + 2] as u64)
                } else { (0, 0, 0) }
            } else { (0, 0, 0) };

            let avg_ms = if tries > 0 { (total_ns as f64 / tries as f64) / 1_000_000.0 } else { 0.0 };
            let weight = node.weights.get(i).copied().unwrap_or(0.0);
            total_tries += tries;
            options.push(OptionReport { index: i, tries, correct, avg_ms, weight });
        }

        // Skip nodes without enough data
        if total_tries < MIN_TRIES_FOR_ANALYSIS * n_options as u64 { continue; }

        // Find winner
        let mut winner_idx = 0;
        let mut winner_weight = 0.0f64;
        let mut runner_up_weight = 0.0f64;
        for o in &options {
            if o.weight > winner_weight {
                runner_up_weight = winner_weight;
                winner_weight = o.weight;
                winner_idx = o.index;
            } else if o.weight > runner_up_weight {
                runner_up_weight = o.weight;
            }
        }

        let objective = match node.objective {
            Objective::Speed => "Speed", Objective::Accuracy => "Accuracy",
            Objective::Reliability => "Reliability", Objective::Cost => "Cost",
            Objective::Risk => "Risk", Objective::Confidence => "Confidence",
            Objective::Reward => "Reward", Objective::MultiObjective => "MultiObjective",
            Objective::None => "General",
        };
        let contract = match node.contract {
            Contract::None => "None", Contract::SameOutput => "SameOutput",
            Contract::Validated => "Validated", Contract::WithinTolerance => "WithinTolerance",
        };

        // Detect issues
        let mut issues = Vec::new();

        // Low confidence: no option >= 0.75
        if winner_weight < CONFIDENCE_THRESHOLD {
            issues.push(("low_confidence",
                format!("{} activations but top weight is only {:.2}", node.activation_count, winner_weight),
                "propose an option that clearly outperforms existing ones".to_string()));
        }

        // Fragile winner: margin too small
        if winner_weight - runner_up_weight < FRAGILE_MARGIN && winner_weight >= CONFIDENCE_THRESHOLD {
            issues.push(("fragile_winner",
                format!("winner weight {:.2} vs runner-up {:.2} — margin only {:.2}",
                    winner_weight, runner_up_weight, winner_weight - runner_up_weight),
                "propose an option that clearly dominates".to_string()));
        }

        // Plateau: many activations, no clear winner
        if node.activation_count > 20 && winner_weight < 0.5 {
            issues.push(("plateau",
                format!("{} activations but no option above 0.50", node.activation_count),
                "current options may be equivalent — propose a fundamentally different approach".to_string()));
        }

        // High failure: any option with low correctness
        for o in &options {
            if o.tries >= MIN_TRIES_FOR_ANALYSIS && o.correct < o.tries / 2 {
                issues.push(("high_failure",
                    format!("option {} correct only {}/{}", o.index, o.correct, o.tries),
                    format!("option {} is unreliable — replace or improve it", o.index)));
            }
        }

        // Unused options: near-zero weight after many tries
        for o in &options {
            if o.tries >= MIN_TRIES_FOR_ANALYSIS && o.weight < 0.02 {
                issues.push(("unused_option",
                    format!("option {} has weight {:.3} after {} tries — effectively dead",
                        o.index, o.weight, o.tries),
                    format!("option {} contributes nothing — consider replacing", o.index)));
            }
        }

        if issues.is_empty() { continue; }

        // Take the most important issue
        let (issue, reason, goal) = issues.remove(0);
        reports.push(WeaknessReport {
            node_id: node.id,
            objective: objective.to_string(),
            contract: contract.to_string(),
            issue: issue.to_string(),
            reason,
            goal,
            options,
            current_winner: winner_idx,
            winner_weight,
        });
    }

    reports
}

/// Format reports as JSON.
pub fn reports_to_json(reports: &[WeaknessReport]) -> String {
    if reports.is_empty() { return "[]".to_string(); }
    let mut out = String::from("[\n");
    for (i, r) in reports.iter().enumerate() {
        out.push_str(&format!("  {{\n    \"node_id\": {},\n    \"objective\": \"{}\",\n    \"contract\": \"{}\",\n",
            r.node_id, r.objective, r.contract));
        out.push_str(&format!("    \"issue\": \"{}\",\n    \"reason\": \"{}\",\n    \"goal\": \"{}\",\n",
            r.issue, r.reason.replace('"', "\\\""), r.goal.replace('"', "\\\"")));
        out.push_str(&format!("    \"current_winner\": {},\n    \"winner_weight\": {:.4},\n",
            r.current_winner, r.winner_weight));
        out.push_str("    \"options\": [\n");
        for (j, o) in r.options.iter().enumerate() {
            out.push_str(&format!(
                "      {{\"option\": {}, \"tries\": {}, \"correct\": {}, \"avg_ms\": {:.3}, \"weight\": {:.4}}}",
                o.index, o.tries, o.correct, o.avg_ms, o.weight));
            if j < r.options.len() - 1 { out.push(','); }
            out.push('\n');
        }
        out.push_str("    ],\n");
        out.push_str(&format!("    \"requirements\": [\"pure\", \"no IO\", \"must beat current winner on {}\"],\n", r.objective.to_lowercase()));
        out.push_str(&format!("    \"proposal_format\": {{\"name\": \"string\", \"source\": \"string\", \"expected_output\": \"string\", \"insert_into_strategy\": {}}}\n", r.node_id));
        out.push_str("  }");
        if i < reports.len() - 1 { out.push(','); }
        out.push('\n');
    }
    out.push(']');
    out
}

/// Parse a proposal JSON string. Minimal parser — no serde dependency.
pub fn parse_proposal(json: &str) -> Result<Proposal, String> {
    let name = extract_json_string(json, "name")
        .ok_or("proposal missing 'name' field")?;
    let source = extract_json_string(json, "source")
        .ok_or("proposal missing 'source' field")?;
    let target = extract_json_number(json, "insert_into_strategy")
        .ok_or("proposal missing 'insert_into_strategy' field")?;
    let expected_output = extract_json_string(json, "expected_output");
    Ok(Proposal { name, source, target_strategy: target as u32, expected_output })
}

/// Apply a proposed new strategy option to a `.lyc` graph: compile,
/// purity-check, benchmark, accept iff correct AND faster, journal.
pub fn apply_proposal(
    lyc_path: &str,
    proposal: &Proposal,
    eval_runs: usize,
) -> Result<ProposalResult, String> {
    apply_proposal_with_policy(lyc_path, proposal, eval_runs, None)
}

pub fn apply_proposal_with_policy(
    lyc_path: &str,
    proposal: &Proposal,
    eval_runs: usize,
    policy: Option<crate::context::ExecutionPolicy>,
) -> Result<ProposalResult, String> {
    // Load the current graph
    let data = std::fs::read(lyc_path)
        .map_err(|e| format!("cannot read {lyc_path}: {e}"))?;
    let original_graph = NeuralGraph::from_bytes(&data)?;

    // 1. Verify target strategy exists
    let target_node = original_graph.nodes.get(proposal.target_strategy as usize)
        .ok_or_else(|| format!("strategy node #{} does not exist", proposal.target_strategy))?;
    if !matches!(target_node.op, OpCode::Strategy | OpCode::AdaptiveChoice) {
        return Err(format!("node #{} is {:?}, not Strategy/AdaptiveChoice",
            proposal.target_strategy, target_node.op));
    }
    let n_existing = if target_node.contract == Contract::WithinTolerance && target_node.weights.len() > 1 {
        target_node.weights.len() - 1
    } else {
        target_node.weights.len()
    };

    // 2. Compile the candidate source
    let mut lexer = crate::lexer::Lexer::new(&proposal.source);
    let tokens = lexer.tokenize()
        .map_err(|e| format!("candidate compile error: {e}"))?;
    let mut parser = crate::parser::Parser::new(tokens);
    let candidate_ast = parser.parse_program()
        .map_err(|e| format!("candidate parse error: {e}"))?;

    let compiler = crate::graph_compiler::GraphCompiler::new();
    let candidate_graph = compiler.compile(&candidate_ast);

    // 3. Purity check
    for node in &candidate_graph.nodes {
        if matches!(node.op, OpCode::Print | OpCode::ReadLine | OpCode::Adapt |
                    OpCode::Spawn | OpCode::Prune) {
            return Ok(ProposalResult {
                accepted: false,
                reason: format!("candidate contains effectful opcode {:?} — must be pure", node.op),
                candidate_ms: 0.0, winner_ms: 0.0, candidate_correct: false,
            });
        }
    }

    // Fresh baseline; persisted stats are advisory only.
    let mut baseline_total_ns: u128 = 0;
    let mut baseline_runs: usize = 0;
    for _ in 0..eval_runs {
        let mut base_executor = match &policy {
            Some(p) => GraphExecutor::new_with_context(
                original_graph.clone(),
                crate::context::ExecutionContext::with_policy(p.clone())),
            None => GraphExecutor::new(original_graph.clone()),
        };
        let start = std::time::Instant::now();
        match base_executor.run() {
            Ok(_) => {
                baseline_total_ns += start.elapsed().as_nanos();
                baseline_runs += 1;
            }
            Err(_) => {} // baseline run failed — will result in no_baseline
        }
    }

    let winner_ms = if baseline_runs > 0 {
        (baseline_total_ns as f64 / baseline_runs as f64) / 1_000_000.0
    } else {
        f64::MAX // no_baseline
    };

    // Run candidate eval_runs times and measure
    let mut candidate_total_ns: u128 = 0;
    let mut candidate_results = Vec::new();
    for _ in 0..eval_runs {
        let mut cand_executor = match &policy {
            Some(p) => GraphExecutor::new_with_context(
                candidate_graph.clone(),
                crate::context::ExecutionContext::with_policy(p.clone())),
            None => GraphExecutor::new(candidate_graph.clone()),
        };
        let start = std::time::Instant::now();
        match cand_executor.run() {
            Ok(val) => {
                candidate_total_ns += start.elapsed().as_nanos();
                candidate_results.push(format!("{val}"));
            }
            Err(e) => {
                return Ok(ProposalResult {
                    accepted: false,
                    reason: format!("candidate execution error: {e}"),
                    candidate_ms: 0.0, winner_ms, candidate_correct: false,
                });
            }
        }
    }

    let candidate_ms = (candidate_total_ns as f64 / eval_runs as f64) / 1_000_000.0;

    // 5. Correctness gate A: candidate results must be consistent
    let candidate_output = &candidate_results[0];
    let all_consistent = candidate_results.iter().all(|r| r == candidate_output);
    if !all_consistent {
        return Ok(ProposalResult {
            accepted: false,
            reason: "candidate produces inconsistent results across runs".to_string(),
            candidate_ms, winner_ms, candidate_correct: false,
        });
    }

    // Check if candidate output is non-trivial
    if candidate_output == "null" || candidate_output.is_empty() {
        return Ok(ProposalResult {
            accepted: false,
            reason: "candidate returned null/empty — likely missing function call".to_string(),
            candidate_ms, winner_ms, candidate_correct: false,
        });
    }

    // 5b. Correctness gate: candidate output must match expected_output.
    //     expected_output is REQUIRED. Without it, wrong candidates could
    //     be accepted just because they're fast.
    let expected = match &proposal.expected_output {
        Some(e) => e,
        None => {
            return Ok(ProposalResult {
                accepted: false,
                reason: "expected_output is required — cannot verify correctness without it".to_string(),
                candidate_ms, winner_ms, candidate_correct: false,
            });
        }
    };
    {
        let candidate_correct = if candidate_output == expected {
            true
        } else {
            // Try numeric tolerance comparison
            let c_val: Option<f64> = candidate_output.parse().ok();
            let e_val: Option<f64> = expected.parse().ok();
            match (c_val, e_val) {
                (Some(c), Some(e)) => (c - e).abs() < 1e-6,
                _ => false,
            }
        };

        if !candidate_correct {
            return Ok(ProposalResult {
                accepted: false,
                reason: format!(
                    "candidate output '{}' does not match expected '{}' — wrong answer",
                    candidate_output, expected
                ),
                candidate_ms, winner_ms, candidate_correct: false,
            });
        }
    }

    // 6. Correctness verified — now GRAFT, then benchmark the full grafted program.
    //    The speed gate is applied AFTER grafting, comparing full-program benchmarks.
    let mut graph = original_graph;
    let target = proposal.target_strategy;

    // 7a. Copy all candidate nodes into host graph, renumbering IDs.
    let id_offset = graph.nodes.len() as u32;
    let candidate_entry = candidate_graph.entry;

    for cnode in &candidate_graph.nodes {
        let new_id = cnode.id + id_offset;
        let new_operands: Vec<Operand> = cnode.operands.iter().map(|op| {
            match op {
                Operand::NodeRef(old_id) => Operand::NodeRef(old_id + id_offset),
                other => other.clone(),
            }
        }).collect();
        // Copy string table entries if any StringRef operands
        // (simplified: candidate strings get new indices)
        let mut final_operands = Vec::new();
        for op in &new_operands {
            match op {
                Operand::StringRef(old_idx) => {
                    let s = candidate_graph.get_string(*old_idx);
                    let new_idx = graph.intern_string(&s);
                    final_operands.push(Operand::StringRef(new_idx));
                }
                other => final_operands.push(other.clone()),
            }
        }
        let mut new_node = GraphNode {
            id: new_id,
            op: cnode.op,
            operands: final_operands,
            weights: cnode.weights.clone(),
            bias: cnode.bias,
            activation_count: 0,
            state_slot: None,
            weight_kind: cnode.weight_kind,
            annotation: None,
            contract: cnode.contract,
            objective: cnode.objective,
        };
        // Copy annotation if present
        if let Some(ann_idx) = cnode.annotation {
            let s = candidate_graph.get_string(ann_idx);
            new_node.annotation = Some(graph.intern_string(&s));
        }
        graph.nodes.push(new_node);
    }
    // Update header
    graph.header.node_count = graph.nodes.len() as u32;

    // 7b. The candidate's entry Sequence node (renumbered) produces the result.
    let candidate_result_node = candidate_entry + id_offset;

    // 7c. Add as new operand on the target Strategy node.
    let strategy = &mut graph.nodes[target as usize];
    let has_tolerance = strategy.contract == Contract::WithinTolerance;

    if has_tolerance {
        // Insert before the epsilon (last weight element)
        let epsilon = strategy.weights.pop().unwrap_or(1e-6);
        let n = strategy.weights.len();
        let avg_weight = if n > 0 { strategy.weights.iter().sum::<f64>() / n as f64 } else { 0.5 };
        // Give new option the average weight (fair start)
        strategy.weights.push(avg_weight);
        strategy.weights.push(epsilon); // put epsilon back
        strategy.operands.push(Operand::NodeRef(candidate_result_node));
    } else {
        let n = strategy.weights.len();
        let avg_weight = if n > 0 { strategy.weights.iter().sum::<f64>() / n as f64 } else { 0.5 };
        strategy.weights.push(avg_weight);
        strategy.operands.push(Operand::NodeRef(candidate_result_node));
    }

    // 7d. Extend state slots for the new option's stats (tries, time, correct).
    if let Some(slot) = strategy.state_slot {
        let n_options_now = if has_tolerance {
            strategy.weights.len() - 1
        } else {
            strategy.weights.len()
        };
        let needed = slot as usize + n_options_now * 3;
        if needed > graph.state.len() {
            graph.state.resize(needed, 0.0);
        }
    }

    // Normalize weights (excluding epsilon if WithinTolerance)
    let n_sel = if has_tolerance { strategy.weights.len() - 1 } else { strategy.weights.len() };
    let sum: f64 = graph.nodes[target as usize].weights[..n_sel].iter().sum();
    if sum > 0.0 {
        for i in 0..n_sel {
            graph.nodes[target as usize].weights[i] /= sum;
        }
    }

    // 7e. Benchmark the FULL GRAFTED program — the actual binary that would be promoted.
    //     This is the correct comparison: full original vs full grafted, same conditions.
    let mut grafted_total_ns: u128 = 0;
    let mut grafted_runs: usize = 0;
    for _ in 0..eval_runs {
        let mut grafted_executor = match &policy {
            Some(p) => GraphExecutor::new_with_context(
                graph.clone(),
                crate::context::ExecutionContext::with_policy(p.clone())),
            None => GraphExecutor::new(graph.clone()),
        };
        let start = std::time::Instant::now();
        match grafted_executor.run() {
            Ok(_) => {
                grafted_total_ns += start.elapsed().as_nanos();
                grafted_runs += 1;
            }
            Err(_) => {}
        }
    }
    let grafted_ms = if grafted_runs > 0 {
        (grafted_total_ns as f64 / grafted_runs as f64) / 1_000_000.0
    } else {
        f64::MAX
    };

    // Speed gate: full grafted program must not be slower than full original
    if winner_ms < f64::MAX && grafted_ms > winner_ms * 1.1 {
        return Ok(ProposalResult {
            accepted: false,
            reason: format!(
                "grafted program slower ({:.3}ms vs original {:.3}ms) — rejected",
                grafted_ms, winner_ms
            ),
            candidate_ms: grafted_ms, winner_ms, candidate_correct: true,
        });
    }

    // 7f. Journal the mutation.
    let name_idx = graph.intern_string(&proposal.name);
    let old_count = n_existing;
    let new_count = n_sel;
    graph.journal.push(JournalEntry {
        run_number: graph.nodes.get(graph.entry as usize)
            .map(|n| n.activation_count).unwrap_or(0),
        node_id: target,
        mutation: MutationKind::NodeSpawned,
        reason: name_idx,
    });

    // 7g. Save.
    let updated_bytes = graph.to_bytes();
    std::fs::write(lyc_path, &updated_bytes)
        .map_err(|e| format!("cannot write {lyc_path}: {e}"))?;

    Ok(ProposalResult {
        accepted: true,
        reason: format!(
            "Candidate '{}' INSERTED into strategy #{}: options {} -> {}. \
             Grafted {:.3}ms (vs original {:.3}ms). Graph mutated and saved.",
            proposal.name, target, old_count, new_count,
            grafted_ms,
            if winner_ms < f64::MAX { winner_ms } else { 0.0 },
        ),
        candidate_ms: grafted_ms,
        winner_ms,
        candidate_correct: true,
    })
}

// ── Minimal JSON helpers (no serde) ──

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let start = json.find(&pattern)?;
    let after_key = &json[start + pattern.len()..];
    // Skip : and whitespace
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    if !after_colon.starts_with('"') { return None; }
    let content = &after_colon[1..];
    // Find closing quote (handle escapes)
    let mut end = 0;
    let chars: Vec<char> = content.chars().collect();
    while end < chars.len() {
        if chars[end] == '\\' { end += 2; continue; }
        if chars[end] == '"' { break; }
        end += 1;
    }
    Some(content[..end].replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn extract_json_number(json: &str, key: &str) -> Option<f64> {
    let pattern = format!("\"{}\"", key);
    let start = json.find(&pattern)?;
    let after_key = &json[start + pattern.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    let mut end = 0;
    let chars: Vec<char> = after_colon.chars().collect();
    while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.' || chars[end] == '-') {
        end += 1;
    }
    after_colon[..end].parse().ok()
}
