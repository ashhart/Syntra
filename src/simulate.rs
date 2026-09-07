use crate::capsule_compiler;
use crate::capsule_spec::{CapsuleSpec, RewardType};

// Legacy types — backward compat with prior callers.

#[allow(dead_code)]
pub struct SimOptions {
    pub rounds: usize,
    pub true_arm_rewards: Vec<f64>,
    pub seed: u64,
    pub noise_std: f64,
    pub trace_every: usize,
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct SimResult {
    pub algorithm: &'static str,
    pub rounds: usize,
    pub cumulative_regret: f64,
    pub final_weights: Vec<f64>,
    pub picks: Vec<u64>,
    pub share_best_arm_last_500: f64,
    pub regret_trace: Vec<(usize, f64)>,
}

#[allow(dead_code)]
pub fn run(spec: &CapsuleSpec, opts: &SimOptions) -> Result<SimResult, String> {
    let traffic = TrafficSpec {
        arms: opts.true_arm_rewards.clone(),
        noise_std: opts.noise_std,
        regime_shifts: Vec::new(),
        context_distribution: None,
        feature_distribution: None,
    };
    let ext = ExtSimOptions {
        rounds: opts.rounds,
        seeds: vec![opts.seed],
        trace_every: opts.trace_every,
        compare_vw: false,
    };
    let report = run_traffic(spec, &traffic, &ext)?;
    let first = report
        .seed_results
        .into_iter()
        .next()
        .ok_or_else(|| "no seed result produced".to_string())?;
    Ok(SimResult {
        algorithm: first.algorithm,
        rounds: first.rounds,
        cumulative_regret: first.cumulative_regret,
        final_weights: first.final_weights,
        picks: first.picks,
        share_best_arm_last_500: first.share_best_arm_last_500,
        regret_trace: first.regret_trace,
    })
}

impl SimResult {
    #[allow(dead_code)]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "algorithm": self.algorithm,
            "rounds": self.rounds,
            "cumulativeRegret": round4(self.cumulative_regret),
            "finalWeights": self.final_weights,
            "picks": self.picks,
            "shareBestArmLast500": round4(self.share_best_arm_last_500),
            "regretTrace": self.regret_trace.iter()
                .map(|(t, r)| serde_json::json!([t, round4(*r)]))
                .collect::<Vec<_>>(),
        })
    }
}

// Traffic spec.

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TrafficSpec {
    pub arms: Vec<f64>,
    #[serde(default = "default_noise_std")]
    pub noise_std: f64,
    #[serde(default)]
    pub regime_shifts: Vec<RegimeShift>,
    #[serde(default)]
    pub context_distribution: Option<ContextDistribution>,
    #[serde(default)]
    pub feature_distribution: Option<FeatureDistribution>,
}

fn default_noise_std() -> f64 {
    0.05
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RegimeShift {
    pub at_round: usize,
    pub new_rewards: Vec<f64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ContextDistribution {
    /// Named context values to sample from.
    pub values: Vec<String>,
    /// Optional categorical weights, defaults to uniform.
    #[serde(default)]
    pub weights: Option<Vec<f64>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FeatureDistribution {
    /// Continuous uniform on a hypercube. `low`/`high` per-dim.
    Uniform {
        low: Vec<f64>,
        high: Vec<f64>,
    },
    /// Categorical: pick one of `vectors` (optionally with weights).
    Categorical {
        vectors: Vec<Vec<f64>>,
        #[serde(default)]
        weights: Option<Vec<f64>>,
    },
    /// Cyclic: walk through the vectors in order, wrapping.
    Cyclic {
        vectors: Vec<Vec<f64>>,
    },
}

impl TrafficSpec {
    pub fn from_yaml(yaml: &str) -> Result<Self, String> {
        let spec: TrafficSpec =
            serde_yml::from_str(yaml).map_err(|e| format!("invalid traffic YAML: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.arms.is_empty() {
            return Err("traffic.arms must contain at least one reward".into());
        }
        for (i, r) in self.arms.iter().enumerate() {
            if !r.is_finite() {
                return Err(format!("traffic.arms[{i}] is not finite"));
            }
        }
        if !(self.noise_std.is_finite() && self.noise_std >= 0.0) {
            return Err("traffic.noise_std must be a finite non-negative number".into());
        }
        let mut prev_round: i64 = -1;
        for (i, s) in self.regime_shifts.iter().enumerate() {
            if s.new_rewards.len() != self.arms.len() {
                return Err(format!(
                    "regime_shifts[{i}].new_rewards length {} does not match arms length {}",
                    s.new_rewards.len(),
                    self.arms.len()
                ));
            }
            if (s.at_round as i64) <= prev_round {
                return Err(format!(
                    "regime_shifts must be in strictly increasing order of at_round (offender at index {i})"
                ));
            }
            prev_round = s.at_round as i64;
        }
        if let Some(c) = &self.context_distribution {
            if c.values.is_empty() {
                return Err("context_distribution.values must not be empty".into());
            }
            if let Some(w) = &c.weights {
                if w.len() != c.values.len() {
                    return Err(
                        "context_distribution.weights length must match values length".into()
                    );
                }
                if w.iter().any(|x| !x.is_finite() || *x < 0.0) {
                    return Err("context_distribution.weights must be finite and non-negative".into());
                }
                if w.iter().sum::<f64>() <= 0.0 {
                    return Err("context_distribution.weights must sum to a positive number".into());
                }
            }
        }
        if let Some(f) = &self.feature_distribution {
            match f {
                FeatureDistribution::Uniform { low, high } => {
                    if low.is_empty() || low.len() != high.len() {
                        return Err("feature_distribution.uniform: low/high must be same non-empty length".into());
                    }
                    for (i, (l, h)) in low.iter().zip(high.iter()).enumerate() {
                        if !(l.is_finite() && h.is_finite()) || l > h {
                            return Err(format!(
                                "feature_distribution.uniform dim {i} invalid (low={l}, high={h})"
                            ));
                        }
                    }
                }
                FeatureDistribution::Categorical { vectors, weights } => {
                    if vectors.is_empty() {
                        return Err("feature_distribution.categorical.vectors must not be empty".into());
                    }
                    let dim = vectors[0].len();
                    for (i, v) in vectors.iter().enumerate() {
                        if v.len() != dim {
                            return Err(format!(
                                "feature_distribution.categorical.vectors[{i}] length {} differs from {dim}",
                                v.len()
                            ));
                        }
                    }
                    if let Some(w) = weights {
                        if w.len() != vectors.len() {
                            return Err(
                                "feature_distribution.categorical.weights length must match vectors length".into(),
                            );
                        }
                    }
                }
                FeatureDistribution::Cyclic { vectors } => {
                    if vectors.is_empty() {
                        return Err("feature_distribution.cyclic.vectors must not be empty".into());
                    }
                }
            }
        }
        Ok(())
    }
}

// Extended simulation.

#[derive(Debug, Clone)]
pub struct ExtSimOptions {
    pub rounds: usize,
    pub seeds: Vec<u64>,
    pub trace_every: usize,
    pub compare_vw: bool,
}

#[derive(Debug)]
pub struct SeedRunResult {
    pub seed: u64,
    pub algorithm: &'static str,
    pub rounds: usize,
    pub cumulative_regret: f64,
    pub final_weights: Vec<f64>,
    pub picks: Vec<u64>,
    pub share_best_arm_last_500: f64,
    pub regret_trace: Vec<(usize, f64)>,
    /// Cumulative regret per round (length = rounds).
    pub regret_per_round: Vec<f64>,
    pub per_context_picks: Vec<(String, Vec<u64>)>,
    /// Share of best arm in the last 500 rounds per context.
    pub per_context_convergence: Vec<(String, f64)>,
    pub refusals: u64,
    pub meta_bandit_selections: Vec<(String, u64)>,
    pub meta_bandit_leader: Option<String>,
}

#[derive(Debug)]
pub struct SimReport {
    pub spec_name: String,
    pub seed_results: Vec<SeedRunResult>,
    pub mean_cumulative_regret: f64,
    pub std_cumulative_regret: f64,
    pub mean_refusal_rate: f64,
    pub vw_comparison: Option<VwComparison>,
}

#[derive(Debug)]
pub struct VwComparison {
    pub mean_cumulative_regret: f64,
    pub std_cumulative_regret: f64,
    pub per_seed_regret: Vec<(u64, f64)>,
}

pub fn run_traffic(
    spec: &CapsuleSpec,
    traffic: &TrafficSpec,
    opts: &ExtSimOptions,
) -> Result<SimReport, String> {
    spec.validate()?;
    traffic.validate()?;
    if opts.rounds == 0 {
        return Err("--rounds must be >= 1".into());
    }
    if opts.seeds.is_empty() {
        return Err("--seeds must be >= 1".into());
    }
    if traffic.arms.len() != spec.options.len() {
        return Err(format!(
            "traffic.arms length {} does not match capsule options count {}",
            traffic.arms.len(),
            spec.options.len()
        ));
    }
    for shift in &traffic.regime_shifts {
        if shift.new_rewards.len() != spec.options.len() {
            return Err(format!(
                "regime_shift at round {} has new_rewards length {} (expected {})",
                shift.at_round,
                shift.new_rewards.len(),
                spec.options.len()
            ));
        }
    }

    let mut seed_results = Vec::with_capacity(opts.seeds.len());
    for &seed in &opts.seeds {
        let r = run_single_seed(spec, traffic, opts, seed)?;
        seed_results.push(r);
    }

    let regrets: Vec<f64> = seed_results.iter().map(|r| r.cumulative_regret).collect();
    let (mean, std) = mean_std(&regrets);
    let refusal_rates: Vec<f64> = seed_results
        .iter()
        .map(|r| r.refusals as f64 / r.rounds.max(1) as f64)
        .collect();
    let mean_refusal_rate = mean_std(&refusal_rates).0;

    let vw_comparison = if opts.compare_vw {
        match run_vw_comparison(spec, traffic, opts) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("[warn] VW comparison skipped: {e}");
                None
            }
        }
    } else {
        None
    };

    Ok(SimReport {
        spec_name: spec.name.clone(),
        seed_results,
        mean_cumulative_regret: mean,
        std_cumulative_regret: std,
        mean_refusal_rate,
        vw_comparison,
    })
}

fn run_single_seed(
    spec: &CapsuleSpec,
    traffic: &TrafficSpec,
    opts: &ExtSimOptions,
    seed: u64,
) -> Result<SeedRunResult, String> {
    let n = spec.options.len();
    let resolved = spec.resolved_algorithm();
    let learning_json = build_learning_json_for_sim(spec, resolved);
    let config = lycan::learning::LearningConfig::from_json(&learning_json);

    let mut memory = lycan::learning::CapsuleMemory::default();
    let init_weights = vec![1.0 / n as f64; n];

    let context_keys: Vec<String> = match &traffic.context_distribution {
        Some(cd) => cd.values.clone(),
        None => vec!["sim".to_string()],
    };
    for k in &context_keys {
        let _ = memory.get_or_init_context(0, k, &init_weights, n);
    }

    let mut rng = SimRng::new(seed);
    let mut current_rewards = traffic.arms.clone();
    let mut regime_idx = 0usize;

    let best_arm_index = |rewards: &[f64]| -> usize {
        rewards
            .iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let mut best_arm = best_arm_index(&current_rewards);
    let mut best_mean = current_rewards[best_arm];

    let mut cumulative_regret = 0.0;
    let mut regret_per_round: Vec<f64> = Vec::with_capacity(opts.rounds);
    let mut picks = vec![0u64; n];
    let mut last_500_picks = vec![0u64; n];
    let mut regret_trace = Vec::new();
    let mut refusals: u64 = 0;

    let mut per_context_picks: std::collections::BTreeMap<String, Vec<u64>> =
        context_keys.iter().map(|k| (k.clone(), vec![0u64; n])).collect();
    let mut per_context_last500: std::collections::BTreeMap<String, Vec<u64>> =
        context_keys.iter().map(|k| (k.clone(), vec![0u64; n])).collect();

    // Meta-bandit is instrumented alongside the live algorithm using the
    // observed reward; it doesn't drive selection.
    let candidates = if matches!(
        traffic.feature_distribution,
        Some(FeatureDistribution::Uniform { .. })
            | Some(FeatureDistribution::Categorical { .. })
            | Some(FeatureDistribution::Cyclic { .. })
    ) {
        lycan::meta_bandit::CandidateId::all().to_vec()
    } else {
        lycan::meta_bandit::CandidateId::discrete_only().to_vec()
    };
    let mut meta_bandit = lycan::meta_bandit::MetaBandit::new_with_candidates(&candidates);
    let mut meta_selections: std::collections::BTreeMap<String, u64> = candidates
        .iter()
        .map(|c| (c.as_str().to_string(), 0u64))
        .collect();

    for t in 0..opts.rounds {
        while regime_idx < traffic.regime_shifts.len()
            && t >= traffic.regime_shifts[regime_idx].at_round
        {
            current_rewards = traffic.regime_shifts[regime_idx].new_rewards.clone();
            best_arm = best_arm_index(&current_rewards);
            best_mean = current_rewards[best_arm];
            regime_idx += 1;
        }

        let context_key = sample_context(traffic, &context_keys, &mut rng);
        let _feature = sample_features(traffic, &mut rng);

        let bucket = memory
            .strategies
            .get_mut(&0)
            .and_then(|sm| sm.contexts.get_mut(context_key.as_str()))
            .ok_or_else(|| format!("missing context bucket for {context_key}"))?;

        let (option, reason) = lycan::learning::select_option(bucket, &config, n);
        if reason.contains("no options") {
            refusals += 1;
            regret_per_round.push(cumulative_regret);
            continue;
        }

        let arm_mean = current_rewards.get(option).copied().unwrap_or(0.0);
        let reward = sample_reward(spec.reward.kind, arm_mean, traffic.noise_std, &mut rng);
        let regret = (best_mean - arm_mean).max(0.0);
        cumulative_regret += regret;
        regret_per_round.push(cumulative_regret);
        picks[option] += 1;
        if t + 500 >= opts.rounds {
            last_500_picks[option] += 1;
            if let Some(v) = per_context_last500.get_mut(&context_key) {
                v[option] += 1;
            }
        }
        if let Some(v) = per_context_picks.get_mut(&context_key) {
            v[option] += 1;
        }

        // Use independent RNG draws so the live policy isn't perturbed.
        let r1 = rng.next_f64();
        let r2 = rng.next_f64();
        let (chosen, _explor) = meta_bandit.select(r1, r2);
        meta_bandit.record(chosen, reward);
        *meta_selections.entry(chosen.as_str().to_string()).or_insert(0) += 1;

        let _ = lycan::learning::apply_feedback(bucket, option, reward, &config);

        if opts.trace_every > 0 && (t + 1) % opts.trace_every == 0 {
            regret_trace.push((t + 1, cumulative_regret));
        }
    }

    let per_context_convergence: Vec<(String, f64)> = per_context_last500
        .iter()
        .map(|(k, picks_last)| {
            let total: u64 = picks_last.iter().sum();
            let share = if total > 0 {
                picks_last[best_arm] as f64 / total as f64
            } else {
                0.0
            };
            (k.clone(), share)
        })
        .collect();
    let per_context_picks_vec: Vec<(String, Vec<u64>)> = per_context_picks.into_iter().collect();

    // Average final weights across context buckets for the legacy shape.
    let mut final_weights = vec![0.0_f64; n];
    let mut bucket_count = 0usize;
    if let Some(sm) = memory.strategies.get(&0) {
        for (_k, b) in sm.contexts.iter() {
            for (i, w) in b.weights.iter().take(n).enumerate() {
                final_weights[i] += *w;
            }
            bucket_count += 1;
        }
    }
    if bucket_count > 0 {
        for w in &mut final_weights {
            *w /= bucket_count as f64;
        }
    }

    let share = if opts.rounds >= 500 {
        last_500_picks[best_arm] as f64 / 500.0
    } else if opts.rounds > 0 {
        last_500_picks[best_arm] as f64 / opts.rounds as f64
    } else {
        0.0
    };

    let meta_bandit_selections: Vec<(String, u64)> = meta_selections.into_iter().collect();
    let meta_bandit_leader = meta_bandit.current_leader().map(|c| c.as_str().to_string());

    Ok(SeedRunResult {
        seed,
        algorithm: algorithm_label(resolved),
        rounds: opts.rounds,
        cumulative_regret,
        final_weights,
        picks,
        share_best_arm_last_500: share,
        regret_trace,
        regret_per_round,
        per_context_picks: per_context_picks_vec,
        per_context_convergence,
        refusals,
        meta_bandit_selections,
        meta_bandit_leader,
    })
}

fn sample_context(traffic: &TrafficSpec, keys: &[String], rng: &mut SimRng) -> String {
    match &traffic.context_distribution {
        None => keys
            .first()
            .cloned()
            .unwrap_or_else(|| "sim".to_string()),
        Some(cd) => {
            if let Some(weights) = &cd.weights {
                let sum: f64 = weights.iter().sum();
                let r = rng.next_f64() * sum;
                let mut acc = 0.0;
                for (i, w) in weights.iter().enumerate() {
                    acc += *w;
                    if r < acc {
                        return cd.values[i].clone();
                    }
                }
                cd.values.last().cloned().unwrap_or_else(|| "sim".into())
            } else {
                let idx = (rng.next_f64() * cd.values.len() as f64) as usize;
                cd.values[idx.min(cd.values.len() - 1)].clone()
            }
        }
    }
}

fn sample_features(traffic: &TrafficSpec, rng: &mut SimRng) -> Option<Vec<f64>> {
    let fd = traffic.feature_distribution.as_ref()?;
    Some(match fd {
        FeatureDistribution::Uniform { low, high } => low
            .iter()
            .zip(high.iter())
            .map(|(l, h)| l + (h - l) * rng.next_f64())
            .collect(),
        FeatureDistribution::Categorical { vectors, weights } => {
            let idx = if let Some(w) = weights {
                let sum: f64 = w.iter().sum();
                let r = rng.next_f64() * sum;
                let mut acc = 0.0;
                let mut sel = 0usize;
                for (i, wi) in w.iter().enumerate() {
                    acc += *wi;
                    if r < acc {
                        sel = i;
                        break;
                    }
                    sel = i;
                }
                sel
            } else {
                let i = (rng.next_f64() * vectors.len() as f64) as usize;
                i.min(vectors.len() - 1)
            };
            vectors[idx].clone()
        }
        FeatureDistribution::Cyclic { vectors } => {
            let step = (rng.next_u64() % vectors.len() as u64) as usize;
            vectors[step].clone()
        }
    })
}

// VW comparison (best-effort).

fn run_vw_comparison(
    spec: &CapsuleSpec,
    traffic: &TrafficSpec,
    opts: &ExtSimOptions,
) -> Result<VwComparison, String> {
    let path = which("vw").ok_or_else(|| "vw binary not on PATH".to_string())?;
    let n = spec.options.len();
    let mut per_seed_regret = Vec::with_capacity(opts.seeds.len());

    for &seed in &opts.seeds {
        // Coarse-grained comparison: feed all examples to vw --cb_explore in
        // a single pass with uniform exploration.
        let temp = std::env::temp_dir();
        let stamp = std::process::id();
        let in_path = temp.join(format!("syntra_vw_in_{stamp}_{seed}.dat"));
        let pred_path = temp.join(format!("syntra_vw_pred_{stamp}_{seed}.dat"));
        let mut rng = SimRng::new(seed);
        let mut current_rewards = traffic.arms.clone();
        let mut regime_idx = 0usize;
        let mut input = String::new();
        let mut regrets: Vec<f64> = Vec::with_capacity(opts.rounds);
        for t in 0..opts.rounds {
            while regime_idx < traffic.regime_shifts.len()
                && t >= traffic.regime_shifts[regime_idx].at_round
            {
                current_rewards = traffic.regime_shifts[regime_idx].new_rewards.clone();
                regime_idx += 1;
            }
            let action = (rng.next_f64() * n as f64) as usize;
            let action = action.min(n - 1);
            let arm_mean = current_rewards[action];
            let reward = sample_reward(spec.reward.kind, arm_mean, traffic.noise_std, &mut rng);
            // CB format: action:cost:probability | feature
            let cost = -reward;
            let prob = 1.0 / n as f64;
            input.push_str(&format!("{}:{:.6}:{:.6} | t:{}\n", action + 1, cost, prob, t));
            let best_mean = current_rewards
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            let regret = (best_mean - arm_mean).max(0.0);
            regrets.push(regret);
        }
        std::fs::write(&in_path, &input).map_err(|e| format!("write vw input: {e}"))?;
        let output = std::process::Command::new(&path)
            .args([
                "--cb_explore",
                &n.to_string(),
                "--quiet",
                "-d",
                in_path.to_str().unwrap_or("/dev/null"),
                "-p",
                pred_path.to_str().unwrap_or("/dev/null"),
            ])
            .output()
            .map_err(|e| format!("running vw: {e}"))?;
        let _ = std::fs::remove_file(&in_path);
        let _ = std::fs::remove_file(&pred_path);
        if !output.status.success() {
            return Err(format!(
                "vw exited with status {}",
                output.status.code().unwrap_or(-1)
            ));
        }
        let cum: f64 = regrets.iter().sum();
        per_seed_regret.push((seed, cum));
    }

    let regrets: Vec<f64> = per_seed_regret.iter().map(|(_, r)| *r).collect();
    let (mean, std) = mean_std(&regrets);
    Ok(VwComparison {
        mean_cumulative_regret: mean,
        std_cumulative_regret: std,
        per_seed_regret,
    })
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path) {
        let candidate = entry.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// Output formats.

pub fn render_json(report: &SimReport) -> serde_json::Value {
    let seeds: Vec<serde_json::Value> = report
        .seed_results
        .iter()
        .map(|r| {
            serde_json::json!({
                "seed": r.seed,
                "algorithm": r.algorithm,
                "rounds": r.rounds,
                "cumulativeRegret": round4(r.cumulative_regret),
                "finalWeights": r.final_weights,
                "picks": r.picks,
                "shareBestArmLast500": round4(r.share_best_arm_last_500),
                "regretTrace": r.regret_trace.iter()
                    .map(|(t, v)| serde_json::json!([t, round4(*v)]))
                    .collect::<Vec<_>>(),
                "perContextPicks": r.per_context_picks.iter()
                    .map(|(k, v)| serde_json::json!({ "context": k, "picks": v }))
                    .collect::<Vec<_>>(),
                "perContextConvergence": r.per_context_convergence.iter()
                    .map(|(k, v)| serde_json::json!({ "context": k, "shareBest": round4(*v) }))
                    .collect::<Vec<_>>(),
                "refusals": r.refusals,
                "refusalRate": round4(r.refusals as f64 / r.rounds.max(1) as f64),
                "metaBanditSelections": r.meta_bandit_selections.iter()
                    .map(|(k, v)| serde_json::json!({ "candidate": k, "selections": v }))
                    .collect::<Vec<_>>(),
                "metaBanditLeader": r.meta_bandit_leader,
            })
        })
        .collect();
    let vw = report.vw_comparison.as_ref().map(|v| {
        serde_json::json!({
            "meanCumulativeRegret": round4(v.mean_cumulative_regret),
            "stdCumulativeRegret": round4(v.std_cumulative_regret),
            "perSeed": v.per_seed_regret.iter()
                .map(|(s, r)| serde_json::json!({ "seed": s, "cumulativeRegret": round4(*r) }))
                .collect::<Vec<_>>(),
        })
    });
    serde_json::json!({
        "ok": true,
        "spec": report.spec_name,
        "seeds": seeds,
        "meanCumulativeRegret": round4(report.mean_cumulative_regret),
        "stdCumulativeRegret": round4(report.std_cumulative_regret),
        "meanRefusalRate": round4(report.mean_refusal_rate),
        "vwComparison": vw,
    })
}

pub fn render_table(report: &SimReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Spec: {}\n", report.spec_name));
    out.push_str(&format!(
        "Seeds: {}  rounds: {}\n",
        report.seed_results.len(),
        report.seed_results.first().map(|r| r.rounds).unwrap_or(0)
    ));
    out.push('\n');
    out.push_str("seed   algorithm        regret    refusals  shareBest500  metaLeader\n");
    out.push_str("----   ---------------  --------  --------  ------------  ----------\n");
    for r in &report.seed_results {
        out.push_str(&format!(
            "{:<6} {:<16} {:>8.3}  {:>8}  {:>11.3}   {}\n",
            r.seed,
            r.algorithm,
            r.cumulative_regret,
            r.refusals,
            r.share_best_arm_last_500,
            r.meta_bandit_leader.as_deref().unwrap_or("-")
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "Mean regret: {:.4}  std: {:.4}\n",
        report.mean_cumulative_regret, report.std_cumulative_regret
    ));
    out.push_str(&format!(
        "Mean refusal rate: {:.4}\n",
        report.mean_refusal_rate
    ));

    let ctx_map = aggregate_per_context_convergence(report);
    if !ctx_map.is_empty() {
        out.push('\n');
        out.push_str("Per-context share-best-arm-last-500 (mean across seeds):\n");
        for (k, v) in &ctx_map {
            out.push_str(&format!("  {:<24} {:.3}\n", k, v));
        }
    }

    let meta_map = aggregate_meta_selections(report);
    if !meta_map.is_empty() {
        out.push('\n');
        out.push_str("Meta-bandit selections (mean fraction across seeds):\n");
        for (k, v) in &meta_map {
            out.push_str(&format!("  {:<16} {:.3}\n", k, v));
        }
    }

    if let Some(v) = &report.vw_comparison {
        out.push('\n');
        out.push_str(&format!(
            "VW comparison — mean regret: {:.4}  std: {:.4}\n",
            v.mean_cumulative_regret, v.std_cumulative_regret
        ));
    }
    out
}

pub fn render_sparkline(report: &SimReport, width: usize) -> String {
    if report.seed_results.is_empty() {
        return String::new();
    }
    let rounds = report.seed_results[0].rounds;
    if rounds == 0 {
        return String::new();
    }
    let mut avg = vec![0.0_f64; rounds];
    for r in &report.seed_results {
        for (i, v) in r.regret_per_round.iter().enumerate().take(rounds) {
            avg[i] += *v;
        }
    }
    let nseeds = report.seed_results.len() as f64;
    for v in &mut avg {
        *v /= nseeds;
    }

    let width = width.max(8).min(rounds);
    let bucket = rounds.div_ceil(width).max(1);
    let mut downsampled = Vec::with_capacity(width);
    let mut i = 0;
    while i < rounds {
        let end = (i + bucket).min(rounds);
        let mut s = 0.0;
        for v in &avg[i..end] {
            s += *v;
        }
        downsampled.push(s / (end - i) as f64);
        i = end;
    }
    let max = downsampled.iter().cloned().fold(0.0_f64, f64::max).max(1e-12);
    let bars = ['_', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}'];
    let mut line = String::new();
    for v in &downsampled {
        let ratio = (v / max).clamp(0.0, 1.0);
        let idx = (ratio * (bars.len() - 1) as f64).round() as usize;
        line.push(bars[idx.min(bars.len() - 1)]);
    }
    format!(
        "Cumulative regret trajectory (mean across {} seed{}, max={:.2}):\n{}",
        report.seed_results.len(),
        if report.seed_results.len() == 1 { "" } else { "s" },
        max,
        line
    )
}

fn aggregate_per_context_convergence(report: &SimReport) -> Vec<(String, f64)> {
    let mut sums: std::collections::BTreeMap<String, (f64, usize)> = Default::default();
    for r in &report.seed_results {
        for (k, v) in &r.per_context_convergence {
            let e = sums.entry(k.clone()).or_insert((0.0, 0));
            e.0 += *v;
            e.1 += 1;
        }
    }
    sums.into_iter()
        .map(|(k, (s, n))| (k, if n > 0 { s / n as f64 } else { 0.0 }))
        .collect()
}

fn aggregate_meta_selections(report: &SimReport) -> Vec<(String, f64)> {
    let mut totals: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
    for r in &report.seed_results {
        let total: u64 = r.meta_bandit_selections.iter().map(|(_, v)| v).sum();
        for (k, v) in &r.meta_bandit_selections {
            let e = totals.entry(k.clone()).or_insert((0, 0));
            e.0 += v;
            e.1 += total;
        }
    }
    totals
        .into_iter()
        .map(|(k, (sel, tot))| {
            let frac = if tot > 0 { sel as f64 / tot as f64 } else { 0.0 };
            (k, frac)
        })
        .collect()
}

// Helpers.

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

fn mean_std(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    if values.len() == 1 {
        return (mean, 0.0);
    }
    let var = values
        .iter()
        .map(|v| (v - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64;
    (mean, var.sqrt())
}

fn build_learning_json_for_sim(
    spec: &CapsuleSpec,
    algorithm: crate::capsule_spec::AlgorithmKind,
) -> serde_json::Value {
    let _ = capsule_compiler::compile_to_dir;
    use crate::capsule_spec::AlgorithmKind;
    let alg_name = match algorithm {
        AlgorithmKind::Auto | AlgorithmKind::Thompson => "thompson",
        AlgorithmKind::Ucb => "ucb1",
        AlgorithmKind::EpsilonGreedy => "epsilonGreedy",
        AlgorithmKind::Weighted => "simpleWeighted",
    };
    let selection_mode = match algorithm {
        AlgorithmKind::Weighted => "weighted",
        AlgorithmKind::EpsilonGreedy => "epsilonGreedy",
        _ => "greedy",
    };
    let mut j = serde_json::json!({
        "algorithm": alg_name,
        "safety": {
            "minExploration": spec.learning.min_exploration,
            "selectionMode": selection_mode,
        }
    });
    if matches!(algorithm, AlgorithmKind::EpsilonGreedy) {
        j["epsilon"] = serde_json::json!(0.10);
        j["safety"]["selectionEpsilon"] = serde_json::json!(0.10);
    }
    j
}

fn algorithm_label(a: crate::capsule_spec::AlgorithmKind) -> &'static str {
    use crate::capsule_spec::AlgorithmKind;
    match a {
        AlgorithmKind::Auto | AlgorithmKind::Thompson => "thompson",
        AlgorithmKind::Ucb => "ucb1",
        AlgorithmKind::EpsilonGreedy => "epsilonGreedy",
        AlgorithmKind::Weighted => "simpleWeighted",
    }
}

fn sample_reward(kind: RewardType, mean: f64, noise_std: f64, rng: &mut SimRng) -> f64 {
    match kind {
        RewardType::Bernoulli => {
            if rng.next_f64() < mean {
                1.0
            } else {
                0.0
            }
        }
        RewardType::Continuous | RewardType::SparseContinuous => {
            let u1 = rng.next_f64().max(1e-12);
            let u2 = rng.next_f64();
            let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            (mean + noise_std * z).clamp(-1.0, 1.0)
        }
    }
}

pub struct SimRng {
    state: u64,
}

impl SimRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_mul(0x9E3779B97F4A7C15) | 1,
        }
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weighted_spec_2() -> CapsuleSpec {
        CapsuleSpec::from_yaml(
            r#"
name: w
options: [a, b]
reward: { type: continuous, range: [0.0, 1.0] }
algorithm: { type: weighted }
"#,
        )
        .unwrap()
    }

    fn bernoulli_spec_3() -> CapsuleSpec {
        CapsuleSpec::from_yaml(
            r#"
name: w3
options: [a, b, c]
reward: { type: bernoulli }
algorithm: { type: thompson }
"#,
        )
        .unwrap()
    }

    #[test]
    fn weighted_converges_on_better_arm() {
        let spec = weighted_spec_2();
        let r = run(
            &spec,
            &SimOptions {
                rounds: 2000,
                true_arm_rewards: vec![0.1, 0.9],
                seed: 42,
                noise_std: 0.05,
                trace_every: 0,
            },
        )
        .unwrap();
        assert!(
            r.share_best_arm_last_500 > 0.6,
            "expected better arm dominance, got {}",
            r.share_best_arm_last_500
        );
    }

    #[test]
    fn rejects_arm_length_mismatch() {
        let spec = CapsuleSpec::from_yaml(
            r#"
name: x
options: [a, b, c]
reward: { type: bernoulli }
"#,
        )
        .unwrap();
        let err = run(
            &spec,
            &SimOptions {
                rounds: 100,
                true_arm_rewards: vec![0.5, 0.5],
                seed: 1,
                noise_std: 0.0,
                trace_every: 0,
            },
        )
        .unwrap_err();
        assert!(err.contains("length") || err.contains("does not match"), "got: {err}");
    }

    #[test]
    fn rejects_zero_rounds() {
        let spec = CapsuleSpec::from_yaml("name: x\noptions: [a, b]\nreward: { type: bernoulli }").unwrap();
        let err = run(
            &spec,
            &SimOptions {
                rounds: 0,
                true_arm_rewards: vec![0.5, 0.5],
                seed: 1,
                noise_std: 0.0,
                trace_every: 0,
            },
        )
        .unwrap_err();
        assert!(err.contains(">= 1"), "got: {err}");
    }

    #[test]
    fn cumulative_regret_math_zero_noise() {
        // Weighted with arm rewards 0.0 vs 1.0: each pick of arm 0 adds 1.0
        // to regret, each pick of arm 1 adds 0.0.
        let spec = weighted_spec_2();
        let traffic = TrafficSpec {
            arms: vec![0.0, 1.0],
            noise_std: 0.0,
            regime_shifts: vec![],
            context_distribution: None,
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 100,
            seeds: vec![7],
            trace_every: 0,
            compare_vw: false,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        let r = &report.seed_results[0];
        let manual = r.picks[0] as f64 * 1.0 + r.picks[1] as f64 * 0.0;
        assert!(
            (r.cumulative_regret - manual).abs() < 1e-9,
            "regret {} != manual {}",
            r.cumulative_regret,
            manual
        );
        assert_eq!(r.picks.iter().sum::<u64>(), 100);
        assert!(
            (r.regret_per_round.last().copied().unwrap() - r.cumulative_regret).abs() < 1e-9
        );
    }

    #[test]
    fn regime_shift_changes_best_arm_mid_run() {
        let spec = weighted_spec_2();
        let traffic = TrafficSpec {
            arms: vec![0.9, 0.1],
            noise_std: 0.0,
            regime_shifts: vec![RegimeShift {
                at_round: 200,
                new_rewards: vec![0.1, 0.9],
            }],
            context_distribution: None,
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 400,
            seeds: vec![13],
            trace_every: 0,
            compare_vw: false,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        let r = &report.seed_results[0];
        for w in r.regret_per_round.windows(2) {
            assert!(w[1] >= w[0] - 1e-12, "regret decreased: {} -> {}", w[0], w[1]);
        }
        assert!(r.regret_per_round.len() == 400);
        // Per-round delta on round 200 is bounded by 0.8 (= 0.9 - 0.1).
        let r200 = r.regret_per_round[199];
        let r201 = r.regret_per_round[200];
        assert!(r201 - r200 <= 0.8 + 1e-9);
    }

    #[test]
    fn regime_shift_takes_effect_at_specified_round() {
        // Arms (0.5, 0.5) -> (1.0, 0.0) at round 10. Pre-round-10 regret == 0.
        let spec = weighted_spec_2();
        let traffic = TrafficSpec {
            arms: vec![0.5, 0.5],
            noise_std: 0.0,
            regime_shifts: vec![RegimeShift {
                at_round: 10,
                new_rewards: vec![1.0, 0.0],
            }],
            context_distribution: None,
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 50,
            seeds: vec![99],
            trace_every: 0,
            compare_vw: false,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        let r = &report.seed_results[0];
        for i in 0..10 {
            assert!(
                r.regret_per_round[i].abs() < 1e-9,
                "expected 0 regret pre-shift at round {i}, got {}",
                r.regret_per_round[i]
            );
        }
        assert!(r.regret_per_round[49] >= r.regret_per_round[10]);
    }

    #[test]
    fn vw_comparison_skipped_gracefully_when_binary_absent() {
        let spec = weighted_spec_2();
        let traffic = TrafficSpec {
            arms: vec![0.2, 0.8],
            noise_std: 0.05,
            regime_shifts: vec![],
            context_distribution: None,
            feature_distribution: None,
        };
        // SAFETY: PATH override is read-only and tests are single-threaded.
        let old_path = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", ""); }
        let opts = ExtSimOptions {
            rounds: 50,
            seeds: vec![1],
            trace_every: 0,
            compare_vw: true,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        if let Some(p) = old_path {
            unsafe { std::env::set_var("PATH", p); }
        } else {
            unsafe { std::env::remove_var("PATH"); }
        }
        assert!(report.vw_comparison.is_none());
        assert_eq!(report.seed_results.len(), 1);
    }

    #[test]
    fn seed_reproducibility_same_seed_same_trajectory() {
        // UCB1 is deterministic; with noise_std=0.0 two runs must agree.
        // Thompson/Weighted/EpsilonGreedy use a time-seeded RNG upstream so
        // they aren't reproducible — this test only covers the simulator.
        let spec = CapsuleSpec::from_yaml(
            r#"
name: ucb3
options: [a, b, c]
reward: { type: sparse_continuous, range: [0.0, 1.0] }
algorithm: { type: ucb }
"#,
        )
        .unwrap();
        let traffic = TrafficSpec {
            arms: vec![0.2, 0.5, 0.7],
            noise_std: 0.0,
            regime_shifts: vec![],
            context_distribution: None,
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 300,
            seeds: vec![1234],
            trace_every: 0,
            compare_vw: false,
        };
        let r1 = run_traffic(&spec, &traffic, &opts).unwrap();
        let r2 = run_traffic(&spec, &traffic, &opts).unwrap();
        assert_eq!(r1.seed_results.len(), 1);
        assert_eq!(r2.seed_results.len(), 1);
        let a = &r1.seed_results[0];
        let b = &r2.seed_results[0];
        assert!(
            (a.cumulative_regret - b.cumulative_regret).abs() < 1e-12,
            "regret diverged: {} vs {}",
            a.cumulative_regret,
            b.cumulative_regret
        );
        assert_eq!(a.picks, b.picks);
        assert_eq!(a.regret_per_round.len(), b.regret_per_round.len());
        for (x, y) in a.regret_per_round.iter().zip(b.regret_per_round.iter()) {
            assert!((x - y).abs() < 1e-12);
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let spec = CapsuleSpec::from_yaml(
            r#"
name: ucb3
options: [a, b, c]
reward: { type: sparse_continuous, range: [0.0, 1.0] }
algorithm: { type: ucb }
"#,
        )
        .unwrap();
        let traffic = TrafficSpec {
            arms: vec![0.2, 0.5, 0.7],
            noise_std: 0.2,
            regime_shifts: vec![],
            context_distribution: None,
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 300,
            seeds: vec![1, 2],
            trace_every: 0,
            compare_vw: false,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        let a = &report.seed_results[0];
        let b = &report.seed_results[1];
        assert!(
            a.picks != b.picks || (a.cumulative_regret - b.cumulative_regret).abs() > 1e-9,
            "expected at least one of picks/regret to differ across seeds"
        );
    }

    #[test]
    fn traffic_spec_parses_full() {
        let y = r#"
arms: [0.1, 0.5, 0.9]
noise_std: 0.02
regime_shifts:
  - at_round: 100
    new_rewards: [0.9, 0.5, 0.1]
context_distribution:
  values: ["weekday", "weekend"]
  weights: [5.0, 2.0]
feature_distribution:
  type: uniform
  low: [0.0, 0.0]
  high: [1.0, 1.0]
"#;
        let t = TrafficSpec::from_yaml(y).unwrap();
        assert_eq!(t.arms.len(), 3);
        assert_eq!(t.regime_shifts.len(), 1);
        assert!(t.context_distribution.is_some());
        assert!(t.feature_distribution.is_some());
    }

    #[test]
    fn traffic_spec_rejects_mismatched_regime_shift() {
        let y = r#"
arms: [0.1, 0.5]
regime_shifts:
  - at_round: 100
    new_rewards: [0.9]
"#;
        let err = TrafficSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("new_rewards length"), "got: {err}");
    }

    #[test]
    fn render_table_includes_per_seed_and_summary() {
        let spec = bernoulli_spec_3();
        let traffic = TrafficSpec {
            arms: vec![0.2, 0.5, 0.7],
            noise_std: 0.0,
            regime_shifts: vec![],
            context_distribution: None,
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 50,
            seeds: vec![1, 2, 3],
            trace_every: 0,
            compare_vw: false,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        let t = render_table(&report);
        assert!(t.contains("Mean regret:"));
        assert!(t.contains("Meta-bandit selections"));
        let row_count = t.lines().filter(|l| l.starts_with("1") || l.starts_with("2") || l.starts_with("3")).count();
        assert!(row_count >= 3, "expected at least 3 rows, got:\n{t}");
    }

    #[test]
    fn render_sparkline_produces_some_output() {
        let spec = bernoulli_spec_3();
        let traffic = TrafficSpec {
            arms: vec![0.2, 0.5, 0.7],
            noise_std: 0.0,
            regime_shifts: vec![],
            context_distribution: None,
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 100,
            seeds: vec![1],
            trace_every: 0,
            compare_vw: false,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        let s = render_sparkline(&report, 40);
        assert!(s.contains("Cumulative regret trajectory"));
    }

    #[test]
    fn context_distribution_seeds_buckets() {
        let spec = CapsuleSpec::from_yaml(
            r#"
name: ctx
options: [a, b]
contexts: [day, night]
reward: { type: bernoulli }
algorithm: { type: thompson }
"#,
        )
        .unwrap();
        let traffic = TrafficSpec {
            arms: vec![0.3, 0.7],
            noise_std: 0.0,
            regime_shifts: vec![],
            context_distribution: Some(ContextDistribution {
                values: vec!["day".into(), "night".into()],
                weights: None,
            }),
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 200,
            seeds: vec![5],
            trace_every: 0,
            compare_vw: false,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        let r = &report.seed_results[0];
        assert_eq!(r.per_context_picks.len(), 2);
        for (_k, picks) in &r.per_context_picks {
            assert!(picks.iter().sum::<u64>() > 0);
        }
    }

    #[test]
    fn meta_bandit_selection_reported() {
        let spec = bernoulli_spec_3();
        let traffic = TrafficSpec {
            arms: vec![0.2, 0.5, 0.7],
            noise_std: 0.0,
            regime_shifts: vec![],
            context_distribution: None,
            feature_distribution: None,
        };
        let opts = ExtSimOptions {
            rounds: 200,
            seeds: vec![10],
            trace_every: 0,
            compare_vw: false,
        };
        let report = run_traffic(&spec, &traffic, &opts).unwrap();
        let r = &report.seed_results[0];
        let total: u64 = r.meta_bandit_selections.iter().map(|(_, v)| v).sum();
        assert_eq!(total, r.rounds as u64);
        assert!(r.meta_bandit_leader.is_some());
    }
}
