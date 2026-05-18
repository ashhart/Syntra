/// Autonomous capsule evolution loop.
///
/// Closes the loop: observe → diagnose → request improvement → receive
/// proposal → verify → graft → benchmark → accept/reject → snapshot →
/// journal → continue.
///
/// Three modes:
///   1. Agent mode: brief → subprocess → proposal → apply
///   2. Proposal mode: skip agent, use local proposal file
///   3. No-agent mode: generate brief only, print, exit

use sha2::{Sha256, Digest};
use std::path::Path;

use crate::evolve;
use crate::graph::NeuralGraph;

/// Configuration for the evolution loop.
pub struct EvolutionConfig {
    pub iterations: usize,
    pub budget_ms: u64,
    pub min_improvement: f64,
    pub dry_run: bool,
    pub agent_command: Option<String>,
    pub proposal_path: Option<String>,
    #[allow(dead_code)]
    pub json_output: bool,
    pub policy: Option<crate::context::ExecutionPolicy>,
}

/// Outcome of a single evolution iteration.
pub struct EvolutionOutcome {
    pub accepted: bool,
    pub reason: String,
    #[allow(dead_code)]
    pub before_score: Option<f64>,
    #[allow(dead_code)]
    pub after_score: Option<f64>,
    pub before_hash: String,
    #[allow(dead_code)]
    pub after_hash: Option<String>,
    pub target_strategy: u32,
    pub proposal_name: String,
}

/// Result of the entire evolution run.
pub struct EvolutionResult {
    pub iterations_run: usize,
    pub proposals_received: usize,
    pub proposals_accepted: usize,
    pub proposals_rejected: usize,
    pub outcomes: Vec<EvolutionOutcome>,
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn timestamp_str() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    // Approximate date from epoch — good enough for filenames
    let days = secs / 86400;
    let y = 1970 + days / 365;
    let doy = days % 365;
    let mo = doy / 30 + 1;
    let day = doy % 30 + 1;
    format!("{y:04}{mo:02}{day:02}_{h:02}{m:02}{s:02}")
}

/// Resolve paths for .lyc file or .lycap directory.
struct EvolvePaths {
    /// The .lyc binary path
    lyc_path: String,
    /// Base path for external files (without .lyc extension for files, or the dir for capsules)
    base: String,
    /// Whether this is a capsule directory
    is_capsule: bool,
}

impl EvolvePaths {
    fn from_path(path: &str) -> Result<Self, String> {
        let p = Path::new(path);
        if p.is_dir() {
            // Capsule directory
            let lyc = format!("{}/program.lyc", path.trim_end_matches('/'));
            if !Path::new(&lyc).exists() {
                return Err(format!("capsule directory {path} has no program.lyc"));
            }
            Ok(Self {
                lyc_path: lyc,
                base: path.trim_end_matches('/').to_string(),
                is_capsule: true,
            })
        } else if p.exists() {
            Ok(Self {
                lyc_path: path.to_string(),
                base: path.to_string(),
                is_capsule: false,
            })
        } else {
            Err(format!("path does not exist: {path}"))
        }
    }

    fn lock_path(&self) -> String {
        if self.is_capsule {
            format!("{}/.evolve.lock", self.base)
        } else {
            format!("{}.evolve.lock", self.base)
        }
    }

    fn journal_path(&self) -> String {
        if self.is_capsule {
            format!("{}/evolution.jsonl", self.base)
        } else {
            format!("{}.evolution.jsonl", self.base)
        }
    }

    fn snapshots_dir(&self) -> String {
        if self.is_capsule {
            format!("{}/snapshots", self.base)
        } else {
            format!("{}.snapshots", self.base)
        }
    }
}

/// Append one line to the external evolution journal.
fn journal_append(paths: &EvolvePaths, entry: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open(paths.journal_path())
    {
        let _ = writeln!(f, "{}", entry);
    }
}

/// Create a journal event JSON string using serde_json for correctness.
fn journal_event(event: &str, fields: &[(&str, &str)]) -> String {
    let mut map = serde_json::Map::new();
    map.insert("event".to_string(), serde_json::Value::String(event.to_string()));
    map.insert("timestamp".to_string(), serde_json::Value::String(timestamp_str()));
    for (k, v) in fields {
        // Parse numbers as numbers, keep strings as strings
        if let Ok(n) = v.parse::<f64>() {
            if let Ok(i) = v.parse::<i64>() {
                map.insert(k.to_string(), serde_json::Value::Number(serde_json::Number::from(i)));
            } else {
                map.insert(k.to_string(), serde_json::json!(n));
            }
        } else if *v == "true" {
            map.insert(k.to_string(), serde_json::Value::Bool(true));
        } else if *v == "false" {
            map.insert(k.to_string(), serde_json::Value::Bool(false));
        } else {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| "{}".to_string())
}

/// Acquire a lock file. Returns error if already locked.
fn acquire_lock(path: &str) -> Result<(), String> {
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => {
            use std::io::Write;
            write!(f, "{}", std::process::id()).ok();
            Ok(())
        }
        Err(_) => Err(format!("evolution lock exists: {path} — another evolution may be running")),
    }
}

/// Release a lock file.
fn release_lock(path: &str) {
    let _ = std::fs::remove_file(path);
}

/// Take a snapshot of the current .lyc binary.
fn snapshot(paths: &EvolvePaths, data: &[u8]) -> Result<String, String> {
    let dir = paths.snapshots_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create snapshots dir: {e}"))?;
    let name = timestamp_str();
    let snap_path = format!("{dir}/{name}.lyc");
    std::fs::write(&snap_path, data)
        .map_err(|e| format!("cannot write snapshot: {e}"))?;
    Ok(name)
}

/// Run the evolution loop.
pub fn run_evolution(path: &str, config: &EvolutionConfig) -> Result<EvolutionResult, String> {
    let paths = EvolvePaths::from_path(path)?;
    let lock_path = paths.lock_path();

    // No-agent mode: just generate brief and exit
    if config.agent_command.is_none() && config.proposal_path.is_none() {
        let data = std::fs::read(&paths.lyc_path)
            .map_err(|e| format!("cannot read {}: {e}", paths.lyc_path))?;
        let graph = NeuralGraph::from_bytes(&data)?;
        let reports = evolve::improve_report(&graph);
        if reports.is_empty() {
            eprintln!("no weaknesses detected — program is performing well");
            println!("{}", evolve::emit_brief(&graph));
        } else {
            eprintln!("{} weakness(es) detected", reports.len());
            println!("{}", evolve::emit_brief(&graph));
        }
        return Ok(EvolutionResult {
            iterations_run: 0,
            proposals_received: 0,
            proposals_accepted: 0,
            proposals_rejected: 0,
            outcomes: vec![],
        });
    }

    // Acquire lock (skip for dry-run since it never mutates)
    if !config.dry_run {
        acquire_lock(&lock_path)?;
    }

    let mut result = EvolutionResult {
        iterations_run: 0,
        proposals_received: 0,
        proposals_accepted: 0,
        proposals_rejected: 0,
        outcomes: vec![],
    };

    let start_time = std::time::Instant::now();

    for iteration in 0..config.iterations {
        // Budget check
        if start_time.elapsed().as_millis() as u64 > config.budget_ms {
            eprintln!("budget of {}ms expired after {} iterations", config.budget_ms, iteration);
            break;
        }

        result.iterations_run += 1;

        // 1. Load current state
        let original_data = std::fs::read(&paths.lyc_path)
            .map_err(|e| format!("cannot read {}: {e}", paths.lyc_path))?;
        let before_hash = sha256_hex(&original_data);
        let graph = NeuralGraph::from_bytes(&original_data)?;

        // Log start
        if !config.dry_run {
            journal_append(&paths, &journal_event("EvolutionStarted", &[
                ("iteration", &iteration.to_string()),
                ("hash_before", &before_hash),
            ]));
        }

        // 2. Diagnose — check for weaknesses (skip in proposal mode: user knows what they want)
        let reports = evolve::improve_report(&graph);
        if reports.is_empty() && config.proposal_path.is_none() {
            eprintln!("iteration {}: no weaknesses detected — stopping", iteration + 1);
            if !config.dry_run {
                journal_append(&paths, &journal_event("EvolutionCompleted", &[
                    ("iteration", &iteration.to_string()),
                    ("reason", "no_weaknesses"),
                ]));
            }
            break;
        }

        // 3. Generate brief
        let brief = evolve::emit_brief(&graph);
        if !config.dry_run {
            journal_append(&paths, &journal_event("BriefGenerated", &[
                ("iteration", &iteration.to_string()),
                ("weaknesses", &reports.len().to_string()),
            ]));
        }

        // 4. Get proposal
        let proposal_json = if let Some(ref prop_path) = config.proposal_path {
            std::fs::read_to_string(prop_path)
                .map_err(|e| format!("cannot read proposal {prop_path}: {e}"))?
        } else if let Some(ref agent_cmd) = config.agent_command {
            crate::agent::call_agent(agent_cmd, &brief, config.budget_ms)?
        } else {
            unreachable!("no-agent mode handled above");
        };

        result.proposals_received += 1;

        // 5. Parse proposal
        let proposal = match evolve::parse_proposal(&proposal_json) {
            Ok(p) => p,
            Err(e) => {
                let outcome = EvolutionOutcome {
                    accepted: false,
                    reason: format!("invalid proposal JSON: {e}"),
                    before_score: None,
                    after_score: None,
                    before_hash: before_hash.clone(),
                    after_hash: None,
                    target_strategy: 0,
                    proposal_name: String::new(),
                };
                eprintln!("iteration {}: rejected — {}", iteration + 1, outcome.reason);
                if !config.dry_run {
                    journal_append(&paths, &journal_event("ProposalRejected", &[
                        ("iteration", &iteration.to_string()),
                        ("reason", &outcome.reason),
                        ("hash_before", &before_hash),
                    ]));
                }
                result.proposals_rejected += 1;
                result.outcomes.push(outcome);
                continue;
            }
        };

        if !config.dry_run {
            journal_append(&paths, &journal_event("ProposalReceived", &[
                ("iteration", &iteration.to_string()),
                ("name", &proposal.name),
                ("target", &proposal.target_strategy.to_string()),
            ]));
        }

        // 6. Snapshot (not for dry-run)
        if !config.dry_run {
            snapshot(&paths, &original_data)?;
        }

        // ── CANDIDATE-FIRST EVOLUTION ──
        // Never mutate the live program during evaluation.
        // Apply proposal to a temp candidate, benchmark both fresh, promote only winners.

        // 7. Copy original to temp candidate
        let candidate_path = format!("/tmp/lycan_candidate_{}_{}.lyc", std::process::id(), iteration);
        std::fs::write(&candidate_path, &original_data)
            .map_err(|e| format!("cannot write candidate: {e}"))?;

        // 8. Apply proposal to candidate (never the original)
        let apply_result = evolve::apply_proposal_with_policy(
            &candidate_path, &proposal, 5, config.policy.clone());

        let proposal_result = match apply_result {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_file(&candidate_path);
                let outcome = EvolutionOutcome {
                    accepted: false,
                    reason: format!("apply error: {e}"),
                    before_score: None,
                    after_score: None,
                    before_hash: before_hash.clone(),
                    after_hash: None,
                    target_strategy: proposal.target_strategy,
                    proposal_name: proposal.name.clone(),
                };
                let tag = if config.dry_run { "WOULD_REJECT" } else { "rejected" };
                eprintln!("iteration {}: {tag} — {}", iteration + 1, outcome.reason);
                if !config.dry_run {
                    journal_append(&paths, &journal_event("ProposalRejected", &[
                        ("iteration", &iteration.to_string()),
                        ("name", &proposal.name),
                        ("reason", &outcome.reason),
                        ("hash_before", &before_hash),
                    ]));
                }
                result.proposals_rejected += 1;
                result.outcomes.push(outcome);
                continue;
            }
        };

        // 9. Check apply_proposal gates (purity, correctness, speed)
        if !proposal_result.accepted {
            let _ = std::fs::remove_file(&candidate_path);
            let outcome = EvolutionOutcome {
                accepted: false,
                reason: proposal_result.reason.clone(),
                before_score: Some(proposal_result.winner_ms),
                after_score: Some(proposal_result.candidate_ms),
                before_hash: before_hash.clone(),
                after_hash: None,
                target_strategy: proposal.target_strategy,
                proposal_name: proposal.name.clone(),
            };
            let tag = if config.dry_run { "WOULD_REJECT" } else { "rejected" };
            eprintln!("iteration {}: {tag} — {}", iteration + 1, outcome.reason);
            if !config.dry_run {
                journal_append(&paths, &journal_event("ProposalRejected", &[
                    ("iteration", &iteration.to_string()),
                    ("name", &proposal.name),
                    ("reason", &proposal_result.reason),
                    ("before_ms", &format!("{:.3}", proposal_result.winner_ms)),
                    ("after_ms", &format!("{:.3}", proposal_result.candidate_ms)),
                    ("hash_before", &before_hash),
                ]));
            }
            result.proposals_rejected += 1;
            result.outcomes.push(outcome);
            continue;
        }

        // 10. Baseline gate — no proposal accepted without a measured baseline
        let before_score = proposal_result.winner_ms;
        let after_score = proposal_result.candidate_ms;

        if before_score <= 0.0 || before_score >= f64::MAX {
            let _ = std::fs::remove_file(&candidate_path);
            let reason = "no_baseline: cannot measure current strategy performance".to_string();
            let outcome = EvolutionOutcome {
                accepted: false,
                reason: reason.clone(),
                before_score: None,
                after_score: Some(after_score),
                before_hash: before_hash.clone(),
                after_hash: None,
                target_strategy: proposal.target_strategy,
                proposal_name: proposal.name.clone(),
            };
            let tag = if config.dry_run { "WOULD_REJECT" } else { "rejected" };
            eprintln!("iteration {}: {tag} — {reason}", iteration + 1);
            if !config.dry_run {
                journal_append(&paths, &journal_event("ProposalRejected", &[
                    ("iteration", &iteration.to_string()),
                    ("name", &proposal.name),
                    ("reason", &reason),
                    ("hash_before", &before_hash),
                ]));
            }
            result.proposals_rejected += 1;
            result.outcomes.push(outcome);
            continue;
        }

        if after_score <= 0.0 {
            let _ = std::fs::remove_file(&candidate_path);
            let reason = "no_candidate_score: cannot measure candidate performance".to_string();
            let outcome = EvolutionOutcome {
                accepted: false,
                reason: reason.clone(),
                before_score: Some(before_score),
                after_score: None,
                before_hash: before_hash.clone(),
                after_hash: None,
                target_strategy: proposal.target_strategy,
                proposal_name: proposal.name.clone(),
            };
            let tag = if config.dry_run { "WOULD_REJECT" } else { "rejected" };
            eprintln!("iteration {}: {tag} — {reason}", iteration + 1);
            if !config.dry_run {
                journal_append(&paths, &journal_event("ProposalRejected", &[
                    ("iteration", &iteration.to_string()),
                    ("name", &proposal.name),
                    ("reason", &reason),
                    ("hash_before", &before_hash),
                ]));
            }
            result.proposals_rejected += 1;
            result.outcomes.push(outcome);
            continue;
        }

        // 11. Enforce min_improvement (only for positive thresholds —
        //     apply_proposal already enforces a 10% regression gate)
        let improvement = (before_score - after_score) / before_score;
        if config.min_improvement > 0.0 && improvement < config.min_improvement {
            let _ = std::fs::remove_file(&candidate_path);
            let reason = format!(
                "improvement {:.1}% below threshold {:.1}% ({:.3}ms → {:.3}ms)",
                improvement * 100.0, config.min_improvement * 100.0,
                before_score, after_score
            );
            let outcome = EvolutionOutcome {
                accepted: false,
                reason: reason.clone(),
                before_score: Some(before_score),
                after_score: Some(after_score),
                before_hash: before_hash.clone(),
                after_hash: None,
                target_strategy: proposal.target_strategy,
                proposal_name: proposal.name.clone(),
            };
            let tag = if config.dry_run { "WOULD_REJECT" } else { "rejected" };
            eprintln!("iteration {}: {tag} — {reason}", iteration + 1);
            if !config.dry_run {
                journal_append(&paths, &journal_event("ProposalRejected", &[
                    ("iteration", &iteration.to_string()),
                    ("name", &proposal.name),
                    ("reason", &reason),
                    ("hash_before", &before_hash),
                ]));
            }
            result.proposals_rejected += 1;
            result.outcomes.push(outcome);
            continue;
        }

        // ── ALL GATES PASSED — PROMOTE CANDIDATE ──
        let candidate_data = match std::fs::read(&candidate_path) {
            Ok(d) => d,
            Err(e) => {
                let _ = std::fs::remove_file(&candidate_path);
                let reason = format!("cannot read candidate after graft: {e}");
                if !config.dry_run {
                    journal_append(&paths, &journal_event("ProposalRejected", &[
                        ("iteration", &iteration.to_string()),
                        ("name", &proposal.name),
                        ("reason", &reason),
                    ]));
                }
                result.proposals_rejected += 1;
                result.outcomes.push(EvolutionOutcome {
                    accepted: false, reason, before_score: Some(before_score),
                    after_score: Some(after_score), before_hash: before_hash.clone(),
                    after_hash: None, target_strategy: proposal.target_strategy,
                    proposal_name: proposal.name.clone(),
                });
                continue;
            }
        };
        let after_hash = sha256_hex(&candidate_data);

        if config.dry_run {
            let _ = std::fs::remove_file(&candidate_path);
            eprintln!("iteration {}: WOULD_ACCEPT — {:.1}% improvement ({:.3}ms → {:.3}ms)",
                iteration + 1, improvement * 100.0, before_score, after_score);
        } else {
            // Atomic promotion: rename candidate over original
            if let Err(_) = std::fs::rename(&candidate_path, &paths.lyc_path) {
                // rename may fail across filesystems — fall back to copy
                if let Err(e) = std::fs::write(&paths.lyc_path, &candidate_data) {
                    let _ = std::fs::remove_file(&candidate_path);
                    return Err(format!("CRITICAL: cannot promote candidate: {e}"));
                }
                let _ = std::fs::remove_file(&candidate_path);
            }
            // Verify promotion
            let promoted_data = std::fs::read(&paths.lyc_path)
                .map_err(|e| format!("CRITICAL: cannot verify promotion: {e}"))?;
            let promoted_hash = sha256_hex(&promoted_data);
            if promoted_hash != after_hash {
                return Err("CRITICAL: promotion hash mismatch".to_string());
            }
            eprintln!("iteration {}: accepted — {:.1}% improvement ({:.3}ms → {:.3}ms)",
                iteration + 1, improvement * 100.0, before_score, after_score);
            journal_append(&paths, &journal_event("ProposalAccepted", &[
                ("iteration", &iteration.to_string()),
                ("name", &proposal.name),
                ("target", &proposal.target_strategy.to_string()),
                ("before_ms", &format!("{:.3}", before_score)),
                ("after_ms", &format!("{:.3}", after_score)),
                ("improvement", &format!("{:.3}", improvement)),
                ("hash_before", &before_hash),
                ("hash_after", &after_hash),
            ]));
        }

        let outcome = EvolutionOutcome {
            accepted: true,
            reason: format!(
                "accepted: {:.1}% improvement ({:.3}ms → {:.3}ms)",
                improvement * 100.0, before_score, after_score
            ),
            before_score: Some(before_score),
            after_score: Some(after_score),
            before_hash: before_hash.clone(),
            after_hash: Some(after_hash),
            target_strategy: proposal.target_strategy,
            proposal_name: proposal.name.clone(),
        };

        result.proposals_accepted += 1;
        result.outcomes.push(outcome);
    }

    // Final journal entry
    if !config.dry_run {
        journal_append(&paths, &journal_event("EvolutionCompleted", &[
            ("iterations", &result.iterations_run.to_string()),
            ("accepted", &result.proposals_accepted.to_string()),
            ("rejected", &result.proposals_rejected.to_string()),
        ]));
    }

    // Release lock
    if !config.dry_run {
        release_lock(&lock_path);
    }

    Ok(result)
}
