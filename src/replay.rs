use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct ReplayEvent {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(
        default,
        alias = "contextKey",
        alias = "context_key",
        alias = "context"
    )]
    pub context_key: Option<String>,
    #[serde(default)]
    pub segment: Option<String>,
    #[serde(
        default,
        alias = "baselineAction",
        alias = "baseline_action",
        alias = "action",
        alias = "loggingAction",
        alias = "logging_action"
    )]
    pub baseline_action: Option<String>,
    #[serde(default, alias = "candidateAction", alias = "candidate_action")]
    pub candidate_action: Option<String>,
    #[serde(default)]
    pub reward: Option<f64>,
    #[serde(
        default,
        alias = "actionRewards",
        alias = "action_rewards",
        alias = "rewardByAction",
        alias = "reward_by_action",
        alias = "rewards"
    )]
    pub action_rewards: BTreeMap<String, f64>,
    #[serde(
        default,
        alias = "actionCostsUsd",
        alias = "action_costs_usd",
        alias = "actionCosts",
        alias = "costs",
        alias = "costsUsd"
    )]
    pub action_costs_usd: BTreeMap<String, f64>,
    #[serde(
        default,
        alias = "actionLatencyMs",
        alias = "action_latency_ms",
        alias = "actionLatenciesMs",
        alias = "latenciesMs"
    )]
    pub action_latency_ms: BTreeMap<String, f64>,
    #[serde(default, alias = "oracleAction", alias = "oracle_action")]
    pub oracle_action: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Policy {
    by_context: BTreeMap<String, String>,
    default_action: Option<String>,
}

impl Policy {
    pub fn from_json_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read policy JSON {}: {e}", path.display()))?;
        Self::from_json_str(&text)
            .map_err(|e| format!("invalid policy JSON {}: {e}", path.display()))
    }

    pub fn from_json_str(text: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut by_context = BTreeMap::new();
        let mut default_action = None;

        let obj = value
            .as_object()
            .ok_or_else(|| "policy must be a JSON object".to_string())?;

        if let Some(map) = obj.get("contexts").and_then(|v| v.as_object()) {
            for (k, v) in map {
                let Some(action) = v.as_str() else {
                    return Err(format!("policy.contexts.{k} must be a string"));
                };
                by_context.insert(k.clone(), action.to_string());
            }
            default_action = obj
                .get("default")
                .or_else(|| obj.get("defaultAction"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        } else {
            for (k, v) in obj {
                if k == "default" || k == "defaultAction" {
                    let Some(action) = v.as_str() else {
                        return Err(format!("policy.{k} must be a string"));
                    };
                    default_action = Some(action.to_string());
                } else {
                    let Some(action) = v.as_str() else {
                        return Err(format!("policy.{k} must be a string"));
                    };
                    by_context.insert(k.clone(), action.to_string());
                }
            }
        }

        if by_context.is_empty() && default_action.is_none() {
            return Err("policy must define at least one context or default action".to_string());
        }

        Ok(Self {
            by_context,
            default_action,
        })
    }

    fn choose(&self, context: &str) -> Option<&str> {
        self.by_context
            .get(context)
            .or(self.default_action.as_ref())
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PromotionGate {
    pub min_events: usize,
    pub min_paired_events: usize,
    pub min_candidate_coverage: f64,
    pub min_reward_uplift: f64,
    pub min_reward_uplift_ci_lower: Option<f64>,
    pub max_cost_increase: Option<f64>,
    pub max_latency_p95_increase_ms: Option<f64>,
    pub no_segment_regression: bool,
}

impl Default for PromotionGate {
    fn default() -> Self {
        Self {
            min_events: 1,
            min_paired_events: 1,
            min_candidate_coverage: 0.95,
            min_reward_uplift: 0.0,
            min_reward_uplift_ci_lower: None,
            max_cost_increase: None,
            max_latency_p95_increase_ms: None,
            no_segment_regression: false,
        }
    }
}

impl PromotionGate {
    pub fn from_yaml_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read promotion gate YAML {}: {e}", path.display()))?;
        let gate: PromotionGate = serde_norway::from_str(&text)
            .map_err(|e| format!("invalid promotion gate YAML {}: {e}", path.display()))?;
        gate.validate()?;
        Ok(gate)
    }

    fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.min_candidate_coverage) {
            return Err("min_candidate_coverage must be between 0 and 1".to_string());
        }
        if !self.min_reward_uplift.is_finite() {
            return Err("min_reward_uplift must be finite".to_string());
        }
        if let Some(v) = self.min_reward_uplift_ci_lower {
            if !v.is_finite() {
                return Err("min_reward_uplift_ci_lower must be finite".to_string());
            }
        }
        if let Some(v) = self.max_cost_increase {
            if !v.is_finite() {
                return Err("max_cost_increase must be finite".to_string());
            }
        }
        if let Some(v) = self.max_latency_p95_increase_ms {
            if !v.is_finite() {
                return Err("max_latency_p95_increase_ms must be finite".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct ReplayReport {
    pub ok: bool,
    pub summary: ReplaySummary,
    pub promotion: PromotionDecision,
    pub gates: PromotionGate,
    pub actions: Vec<ActionSummary>,
    pub segments: Vec<SegmentSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ReplaySummary {
    pub events: usize,
    pub paired_events: usize,
    pub candidate_coverage: f64,
    pub baseline_mean_reward: f64,
    pub candidate_mean_reward: f64,
    pub reward_uplift: f64,
    pub reward_uplift_pct: Option<f64>,
    pub reward_uplift_ci90: [f64; 2],
    pub baseline_mean_cost_usd: Option<f64>,
    pub candidate_mean_cost_usd: Option<f64>,
    pub cost_increase_usd: Option<f64>,
    pub baseline_p95_latency_ms: Option<f64>,
    pub candidate_p95_latency_ms: Option<f64>,
    pub latency_p95_increase_ms: Option<f64>,
    pub oracle_match_rate: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct PromotionDecision {
    pub pass: bool,
    pub status: String,
    pub checks: Vec<PromotionCheck>,
}

#[derive(Debug, Serialize)]
pub struct PromotionCheck {
    pub name: String,
    pub pass: bool,
    pub observed: serde_json::Value,
    pub threshold: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ActionSummary {
    pub action: String,
    pub candidate_picks: usize,
    pub mean_reward: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SegmentSummary {
    pub segment: String,
    pub paired_events: usize,
    pub reward_uplift: f64,
    pub pass: bool,
}

#[derive(Debug)]
struct PairedRow {
    baseline_reward: f64,
    candidate_reward: f64,
    baseline_cost: Option<f64>,
    candidate_cost: Option<f64>,
    baseline_latency: Option<f64>,
    candidate_latency: Option<f64>,
    oracle_match: Option<bool>,
    segment: String,
}

pub fn run_file(
    path: &Path,
    policy: Option<&Policy>,
    gates: PromotionGate,
) -> Result<ReplayReport, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read replay events {}: {e}", path.display()))?;
    run_jsonl(&text, policy, gates)
}

pub fn run_jsonl(
    text: &str,
    policy: Option<&Policy>,
    gates: PromotionGate,
) -> Result<ReplayReport, String> {
    gates.validate()?;

    let mut events = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let event: ReplayEvent = serde_json::from_str(line)
            .map_err(|e| format!("invalid replay JSONL at line {}: {e}", idx + 1))?;
        events.push(event);
    }
    if events.is_empty() {
        return Err("replay events file contains no events".to_string());
    }

    evaluate_events(&events, policy, gates)
}

pub fn evaluate_events(
    events: &[ReplayEvent],
    policy: Option<&Policy>,
    gates: PromotionGate,
) -> Result<ReplayReport, String> {
    let mut warnings = Vec::new();
    let mut paired = Vec::new();
    let mut missing_candidate_action = 0usize;
    let mut missing_candidate_reward = 0usize;
    let mut missing_baseline_reward = 0usize;
    let mut action_stats: BTreeMap<String, (usize, f64, usize)> = BTreeMap::new();

    for (idx, event) in events.iter().enumerate() {
        let context = event.context_key.as_deref().unwrap_or("default");
        let baseline_action = event.baseline_action.as_deref().ok_or_else(|| {
            format!(
                "event {} missing baselineAction/action",
                event_label(idx, event)
            )
        })?;
        let candidate_action = match event.candidate_action.as_deref() {
            Some(action) => Some(action),
            None => policy.and_then(|p| p.choose(context)),
        };
        let Some(candidate_action) = candidate_action else {
            missing_candidate_action += 1;
            continue;
        };

        let baseline_reward = reward_for(event, baseline_action);
        let candidate_reward = reward_for(event, candidate_action);
        let Some(baseline_reward) = baseline_reward else {
            missing_baseline_reward += 1;
            continue;
        };
        let Some(candidate_reward) = candidate_reward else {
            missing_candidate_reward += 1;
            continue;
        };

        let segment = event.segment.clone().unwrap_or_else(|| context.to_string());
        let oracle_match = event
            .oracle_action
            .as_deref()
            .map(|oracle| oracle == candidate_action);

        let row = PairedRow {
            baseline_reward,
            candidate_reward,
            baseline_cost: event.action_costs_usd.get(baseline_action).copied(),
            candidate_cost: event.action_costs_usd.get(candidate_action).copied(),
            baseline_latency: event.action_latency_ms.get(baseline_action).copied(),
            candidate_latency: event.action_latency_ms.get(candidate_action).copied(),
            oracle_match,
            segment,
        };
        let entry = action_stats
            .entry(candidate_action.to_string())
            .or_insert((0, 0.0, 0));
        entry.0 += 1;
        entry.1 += candidate_reward;
        entry.2 += 1;
        paired.push(row);
    }

    if missing_candidate_action > 0 {
        warnings.push(format!(
            "{missing_candidate_action} event(s) skipped because no candidateAction was logged and no policy matched"
        ));
    }
    if missing_baseline_reward > 0 {
        warnings.push(format!(
            "{missing_baseline_reward} event(s) skipped because baseline reward was unavailable"
        ));
    }
    if missing_candidate_reward > 0 {
        warnings.push(format!(
            "{missing_candidate_reward} event(s) skipped because candidate reward was unavailable"
        ));
    }

    let paired_events = paired.len();
    if paired_events == 0 {
        return Err(
            "no comparable replay events: provide candidateAction or --policy-json plus actionRewards"
                .to_string(),
        );
    }

    let baseline_rewards: Vec<f64> = paired.iter().map(|r| r.baseline_reward).collect();
    let candidate_rewards: Vec<f64> = paired.iter().map(|r| r.candidate_reward).collect();
    let diffs: Vec<f64> = paired
        .iter()
        .map(|r| r.candidate_reward - r.baseline_reward)
        .collect();

    let baseline_mean_reward = mean(&baseline_rewards).unwrap_or(0.0);
    let candidate_mean_reward = mean(&candidate_rewards).unwrap_or(0.0);
    let reward_uplift = candidate_mean_reward - baseline_mean_reward;
    let reward_uplift_pct = if baseline_mean_reward.abs() > f64::EPSILON {
        Some(reward_uplift / baseline_mean_reward.abs())
    } else {
        None
    };
    let ci = normal_ci90(&diffs);

    let (baseline_mean_cost, candidate_mean_cost, cost_increase) =
        paired_optional_mean_diff(&paired, |r| (r.baseline_cost, r.candidate_cost));
    let baseline_latencies = optional_values(&paired, |r| r.baseline_latency);
    let candidate_latencies = optional_values(&paired, |r| r.candidate_latency);
    let baseline_p95 = percentile(baseline_latencies, 0.95);
    let candidate_p95 = percentile(candidate_latencies, 0.95);
    let latency_p95_increase = match (baseline_p95, candidate_p95) {
        (Some(b), Some(c)) => Some(c - b),
        _ => None,
    };
    let oracle_values: Vec<bool> = paired.iter().filter_map(|r| r.oracle_match).collect();
    let oracle_match_rate = if oracle_values.is_empty() {
        None
    } else {
        Some(oracle_values.iter().filter(|&&v| v).count() as f64 / oracle_values.len() as f64)
    };

    let mut segment_map: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for row in &paired {
        segment_map
            .entry(row.segment.clone())
            .or_default()
            .push(row.candidate_reward - row.baseline_reward);
    }
    let mut segments = Vec::new();
    for (segment, values) in segment_map {
        let uplift = mean(&values).unwrap_or(0.0);
        segments.push(SegmentSummary {
            segment,
            paired_events: values.len(),
            reward_uplift: round6(uplift),
            pass: uplift >= 0.0,
        });
    }

    let actions = action_stats
        .into_iter()
        .map(
            |(action, (picks, reward_sum, reward_count))| ActionSummary {
                action,
                candidate_picks: picks,
                mean_reward: if reward_count > 0 {
                    Some(round6(reward_sum / reward_count as f64))
                } else {
                    None
                },
            },
        )
        .collect();

    let summary = ReplaySummary {
        events: events.len(),
        paired_events,
        candidate_coverage: round6(paired_events as f64 / events.len() as f64),
        baseline_mean_reward: round6(baseline_mean_reward),
        candidate_mean_reward: round6(candidate_mean_reward),
        reward_uplift: round6(reward_uplift),
        reward_uplift_pct: reward_uplift_pct.map(round6),
        reward_uplift_ci90: [round6(ci.0), round6(ci.1)],
        baseline_mean_cost_usd: baseline_mean_cost.map(round6),
        candidate_mean_cost_usd: candidate_mean_cost.map(round6),
        cost_increase_usd: cost_increase.map(round6),
        baseline_p95_latency_ms: baseline_p95.map(round6),
        candidate_p95_latency_ms: candidate_p95.map(round6),
        latency_p95_increase_ms: latency_p95_increase.map(round6),
        oracle_match_rate: oracle_match_rate.map(round6),
    };

    let promotion = decide_promotion(&summary, &segments, &gates);

    Ok(ReplayReport {
        ok: true,
        summary,
        promotion,
        gates,
        actions,
        segments,
        warnings,
    })
}

pub fn render_json(report: &ReplayReport) -> String {
    serde_json::to_string_pretty(report).expect("report serializes")
}

pub fn render_markdown(report: &ReplayReport) -> String {
    let s = &report.summary;
    let mut out = String::new();
    out.push_str("# Syntra Promotion Report\n\n");
    out.push_str(&format!("**Status:** {}\n\n", report.promotion.status));
    out.push_str("## Summary\n\n");
    out.push_str("| Metric | Value |\n|---|---:|\n");
    out.push_str(&format!("| Events | {} |\n", s.events));
    out.push_str(&format!("| Paired events | {} |\n", s.paired_events));
    out.push_str(&format!(
        "| Candidate coverage | {:.2}% |\n",
        s.candidate_coverage * 100.0
    ));
    out.push_str(&format!(
        "| Baseline mean reward | {:.6} |\n",
        s.baseline_mean_reward
    ));
    out.push_str(&format!(
        "| Candidate mean reward | {:.6} |\n",
        s.candidate_mean_reward
    ));
    out.push_str(&format!("| Reward uplift | {:.6} |\n", s.reward_uplift));
    out.push_str(&format!(
        "| Reward uplift CI90 | [{:.6}, {:.6}] |\n",
        s.reward_uplift_ci90[0], s.reward_uplift_ci90[1]
    ));
    if let Some(v) = s.cost_increase_usd {
        out.push_str(&format!("| Cost increase / event | ${:.6} |\n", v));
    }
    if let Some(v) = s.latency_p95_increase_ms {
        out.push_str(&format!("| p95 latency increase | {:.3} ms |\n", v));
    }
    if let Some(v) = s.oracle_match_rate {
        out.push_str(&format!("| Oracle match rate | {:.2}% |\n", v * 100.0));
    }

    out.push_str("\n## Promotion Checks\n\n");
    out.push_str("| Check | Status | Observed | Threshold |\n|---|---|---:|---:|\n");
    for check in &report.promotion.checks {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            check.name,
            if check.pass { "PASS" } else { "FAIL" },
            json_scalar(&check.observed),
            json_scalar(&check.threshold)
        ));
    }

    if !report.segments.is_empty() {
        out.push_str("\n## Segments\n\n");
        out.push_str("| Segment | Paired events | Reward uplift | Status |\n|---|---:|---:|---|\n");
        for segment in &report.segments {
            out.push_str(&format!(
                "| {} | {} | {:.6} | {} |\n",
                escape_md(&segment.segment),
                segment.paired_events,
                segment.reward_uplift,
                if segment.pass { "PASS" } else { "FAIL" }
            ));
        }
    }

    if !report.actions.is_empty() {
        out.push_str("\n## Candidate Actions\n\n");
        out.push_str("| Action | Picks | Mean reward |\n|---|---:|---:|\n");
        for action in &report.actions {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                escape_md(&action.action),
                action.candidate_picks,
                action
                    .mean_reward
                    .map(|v| format!("{v:.6}"))
                    .unwrap_or_else(|| "n/a".to_string())
            ));
        }
    }

    if !report.warnings.is_empty() {
        out.push_str("\n## Warnings\n\n");
        for warning in &report.warnings {
            out.push_str(&format!("- {}\n", escape_md(warning)));
        }
    }

    out
}

fn decide_promotion(
    summary: &ReplaySummary,
    segments: &[SegmentSummary],
    gates: &PromotionGate,
) -> PromotionDecision {
    let mut checks = Vec::new();
    checks.push(check_usize(
        "minimum events",
        summary.events,
        gates.min_events,
        summary.events >= gates.min_events,
    ));
    checks.push(check_usize(
        "minimum paired events",
        summary.paired_events,
        gates.min_paired_events,
        summary.paired_events >= gates.min_paired_events,
    ));
    checks.push(check_f64(
        "candidate coverage",
        summary.candidate_coverage,
        gates.min_candidate_coverage,
        summary.candidate_coverage >= gates.min_candidate_coverage,
    ));
    checks.push(check_f64(
        "reward uplift",
        summary.reward_uplift,
        gates.min_reward_uplift,
        summary.reward_uplift >= gates.min_reward_uplift,
    ));

    if let Some(threshold) = gates.min_reward_uplift_ci_lower {
        checks.push(check_f64(
            "reward uplift CI90 lower bound",
            summary.reward_uplift_ci90[0],
            threshold,
            summary.reward_uplift_ci90[0] >= threshold,
        ));
    }
    if let (Some(observed), Some(threshold)) = (summary.cost_increase_usd, gates.max_cost_increase)
    {
        checks.push(check_f64(
            "cost increase",
            observed,
            threshold,
            observed <= threshold,
        ));
    }
    if let (Some(observed), Some(threshold)) = (
        summary.latency_p95_increase_ms,
        gates.max_latency_p95_increase_ms,
    ) {
        checks.push(check_f64(
            "p95 latency increase",
            observed,
            threshold,
            observed <= threshold,
        ));
    }
    if gates.no_segment_regression {
        let regressions = segments.iter().filter(|s| !s.pass).count();
        checks.push(PromotionCheck {
            name: "no segment regression".to_string(),
            pass: regressions == 0,
            observed: serde_json::json!(regressions),
            threshold: serde_json::json!(0),
        });
    }

    let pass = checks.iter().all(|c| c.pass);
    PromotionDecision {
        pass,
        status: if pass { "PASS" } else { "FAIL" }.to_string(),
        checks,
    }
}

fn reward_for(event: &ReplayEvent, action: &str) -> Option<f64> {
    event
        .action_rewards
        .get(action)
        .copied()
        .or_else(|| match event.baseline_action.as_deref() {
            Some(baseline) if baseline == action => event.reward,
            _ => None,
        })
}

fn event_label(idx: usize, event: &ReplayEvent) -> String {
    event
        .id
        .as_deref()
        .map(|id| format!("{id} (line {})", idx + 1))
        .unwrap_or_else(|| format!("line {}", idx + 1))
}

fn optional_values<T>(rows: &[T], f: impl Fn(&T) -> Option<f64>) -> Vec<f64> {
    rows.iter()
        .filter_map(f)
        .filter(|v| v.is_finite())
        .collect()
}

fn paired_optional_mean_diff<T>(
    rows: &[T],
    f: impl Fn(&T) -> (Option<f64>, Option<f64>),
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let mut base = Vec::new();
    let mut cand = Vec::new();
    for row in rows {
        let (b, c) = f(row);
        if let (Some(b), Some(c)) = (b, c) {
            if b.is_finite() && c.is_finite() {
                base.push(b);
                cand.push(c);
            }
        }
    }
    let base_mean = mean(&base);
    let cand_mean = mean(&cand);
    let diff = match (base_mean, cand_mean) {
        (Some(b), Some(c)) => Some(c - b),
        _ => None,
    };
    (base_mean, cand_mean, diff)
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

fn stddev_sample(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let m = mean(values).unwrap_or(0.0);
    let var = values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    var.sqrt()
}

fn normal_ci90(values: &[f64]) -> (f64, f64) {
    let m = mean(values).unwrap_or(0.0);
    if values.len() < 2 {
        return (m, m);
    }
    let se = stddev_sample(values) / (values.len() as f64).sqrt();
    let radius = 1.6448536269514722 * se;
    (m - radius, m + radius)
}

fn percentile(mut values: Vec<f64>, p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((values.len() - 1) as f64 * p).ceil() as usize;
    values.get(idx).copied()
}

fn round6(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

fn check_usize(name: &str, observed: usize, threshold: usize, pass: bool) -> PromotionCheck {
    PromotionCheck {
        name: name.to_string(),
        pass,
        observed: serde_json::json!(observed),
        threshold: serde_json::json!(threshold),
    }
}

fn check_f64(name: &str, observed: f64, threshold: f64, pass: bool) -> PromotionCheck {
    PromotionCheck {
        name: name.to_string(),
        pass,
        observed: serde_json::json!(round6(observed)),
        threshold: serde_json::json!(round6(threshold)),
    }
}

fn json_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => escape_md(s),
        _ => value.to_string(),
    }
}

fn escape_md(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_gate() -> PromotionGate {
        PromotionGate {
            min_events: 4,
            min_paired_events: 4,
            min_candidate_coverage: 1.0,
            min_reward_uplift: 0.05,
            min_reward_uplift_ci_lower: None,
            max_cost_increase: Some(0.0),
            max_latency_p95_increase_ms: Some(20.0),
            no_segment_regression: true,
        }
    }

    #[test]
    fn replay_policy_passes_promotion_gates() {
        let jsonl = r#"
{"contextKey":"support-low-cost","segment":"support","baselineAction":"balanced","actionRewards":{"cheap_fast":0.82,"balanced":0.70,"expensive_accurate":0.75},"actionCostsUsd":{"cheap_fast":0.01,"balanced":0.03},"actionLatencyMs":{"cheap_fast":220,"balanced":340},"oracleAction":"cheap_fast"}
{"contextKey":"support-low-cost","segment":"support","baselineAction":"balanced","actionRewards":{"cheap_fast":0.80,"balanced":0.70,"expensive_accurate":0.75},"actionCostsUsd":{"cheap_fast":0.01,"balanced":0.03},"actionLatencyMs":{"cheap_fast":210,"balanced":350},"oracleAction":"cheap_fast"}
{"contextKey":"legal-high-accuracy","segment":"legal","baselineAction":"balanced","actionRewards":{"cheap_fast":0.40,"balanced":0.68,"expensive_accurate":0.86},"actionCostsUsd":{"expensive_accurate":0.06,"balanced":0.06},"actionLatencyMs":{"expensive_accurate":620,"balanced":610},"oracleAction":"expensive_accurate"}
{"contextKey":"legal-high-accuracy","segment":"legal","baselineAction":"balanced","actionRewards":{"cheap_fast":0.42,"balanced":0.69,"expensive_accurate":0.88},"actionCostsUsd":{"expensive_accurate":0.06,"balanced":0.06},"actionLatencyMs":{"expensive_accurate":625,"balanced":615},"oracleAction":"expensive_accurate"}
"#;
        let policy = Policy::from_json_str(
            r#"{"support-low-cost":"cheap_fast","legal-high-accuracy":"expensive_accurate"}"#,
        )
        .unwrap();
        let report = run_jsonl(jsonl, Some(&policy), passing_gate()).unwrap();

        assert!(report.promotion.pass, "{:#?}", report.promotion.checks);
        assert_eq!(report.summary.events, 4);
        assert_eq!(report.summary.paired_events, 4);
        assert!(report.summary.reward_uplift > 0.1);
        assert!(report.summary.cost_increase_usd.unwrap() <= 0.0);
        assert_eq!(report.summary.oracle_match_rate, Some(1.0));
        assert!(render_markdown(&report).contains("**Status:** PASS"));
    }

    #[test]
    fn segment_regression_fails_gate() {
        let jsonl = r#"
{"contextKey":"a","segment":"enterprise","baselineAction":"old","candidateAction":"new","actionRewards":{"old":0.8,"new":0.7}}
{"contextKey":"b","segment":"startup","baselineAction":"old","candidateAction":"new","actionRewards":{"old":0.3,"new":0.9}}
"#;
        let gate = PromotionGate {
            min_events: 2,
            min_paired_events: 2,
            min_candidate_coverage: 1.0,
            min_reward_uplift: 0.0,
            min_reward_uplift_ci_lower: None,
            max_cost_increase: None,
            max_latency_p95_increase_ms: None,
            no_segment_regression: true,
        };
        let report = run_jsonl(jsonl, None, gate).unwrap();
        assert!(!report.promotion.pass);
        assert!(
            report
                .segments
                .iter()
                .any(|s| s.segment == "enterprise" && !s.pass)
        );
    }

    #[test]
    fn missing_candidate_rewards_lower_coverage() {
        let jsonl = r#"
{"contextKey":"a","baselineAction":"old","candidateAction":"new","actionRewards":{"old":0.8,"new":0.9}}
{"contextKey":"b","baselineAction":"old","candidateAction":"new","actionRewards":{"old":0.8}}
"#;
        let gate = PromotionGate {
            min_events: 2,
            min_paired_events: 1,
            min_candidate_coverage: 1.0,
            min_reward_uplift: 0.0,
            min_reward_uplift_ci_lower: None,
            max_cost_increase: None,
            max_latency_p95_increase_ms: None,
            no_segment_regression: false,
        };
        let report = run_jsonl(jsonl, None, gate).unwrap();
        assert_eq!(report.summary.paired_events, 1);
        assert_eq!(report.summary.candidate_coverage, 0.5);
        assert!(!report.promotion.pass);
        assert_eq!(report.warnings.len(), 1);
    }
}
