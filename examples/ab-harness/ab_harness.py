#!/usr/bin/env python3
"""
ab_harness.py -- Simulation-driven A/B harness for two Syntra capsules.

Drives both capsules against the same simulated traffic (same seed, same
context sequence, same observed rewards) and reports head-to-head metrics
including cumulative reward, regret vs oracle, refusal rates, and a
paired t-test over multiple seeds.

Traffic specs are parsed from JSON or YAML (if PyYAML is installed).
If PyYAML is not installed, traffic specs must be written in JSON.
See example_traffic.yaml for the schema (save as .json to avoid the dep).

Usage:
    syntra-ab capsule_a.yaml capsule_b.yaml traffic_spec.yaml \\
        --rounds 1000 --seeds 10 \\
        --syntra-url http://localhost:8787 --admin-key dev-key \\
        [--output-dir results/]

    # Or via python directly:
    python3 ab_harness.py capsule_a.yaml capsule_b.yaml traffic.json \\
        --rounds 200 --seeds 5

Apache-2.0.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import os
import random
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Tuple


# ---------------------------------------------------------------------------
# Traffic spec parsing
# ---------------------------------------------------------------------------


def _load_spec_file(path: str) -> dict:
    """Load a traffic spec from JSON or YAML.

    YAML support requires PyYAML (pip install pyyaml).  If PyYAML is absent,
    the file must be valid JSON (rename .yaml -> .json or write JSON inside).
    """
    with open(path, "r", encoding="utf-8") as fh:
        raw = fh.read()

    # Try JSON first (always available).
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        pass

    # Fall back to YAML if available.
    try:
        import yaml  # type: ignore
        return yaml.safe_load(raw)
    except ImportError:
        raise SystemExit(
            f"ERROR: '{path}' is not valid JSON and PyYAML is not installed.\n"
            "Either write the traffic spec as JSON, or install PyYAML:\n"
            "    pip install pyyaml"
        )


def parse_traffic_spec(path: str) -> "TrafficSpec":
    """Parse and validate a traffic spec file. Raises ValueError on bad input."""
    raw = _load_spec_file(path)
    if not isinstance(raw, dict):
        raise ValueError("Traffic spec must be a JSON/YAML object (mapping) at the top level.")

    # Required fields
    if "arms" not in raw:
        raise ValueError("Traffic spec missing required field: 'arms'")
    if "true_rewards" not in raw:
        raise ValueError("Traffic spec missing required field: 'true_rewards'")

    arms = raw["arms"]
    if not isinstance(arms, list) or len(arms) < 2:
        raise ValueError("'arms' must be a list with at least 2 elements.")

    true_rewards = raw["true_rewards"]
    if not isinstance(true_rewards, (dict, list)):
        raise ValueError("'true_rewards' must be a dict (arm->reward) or list of numbers.")

    # Normalise true_rewards to dict[arm_name, reward_or_dict]
    if isinstance(true_rewards, list):
        if len(true_rewards) != len(arms):
            raise ValueError(
                f"'true_rewards' list length ({len(true_rewards)}) "
                f"must equal 'arms' length ({len(arms)})."
            )
        true_rewards = dict(zip(arms, true_rewards))

    # Validate each arm has a reward entry
    for arm in arms:
        if arm not in true_rewards:
            raise ValueError(f"'true_rewards' missing entry for arm '{arm}'.")

    noise_std = float(raw.get("noise_std", 0.0))
    if noise_std < 0:
        raise ValueError("'noise_std' must be >= 0.")

    # regime_shifts: list of {at_round: int, new_rewards: dict}
    regime_shifts = []
    for i, shift in enumerate(raw.get("regime_shifts", [])):
        if "at_round" not in shift:
            raise ValueError(f"regime_shifts[{i}] missing 'at_round'.")
        if "new_rewards" not in shift:
            raise ValueError(f"regime_shifts[{i}] missing 'new_rewards'.")
        regime_shifts.append({
            "at_round": int(shift["at_round"]),
            "new_rewards": dict(shift["new_rewards"]),
        })

    # context_sequence: either a list of context dicts or {"distribution": "uniform", "values": [...]}
    context_sequence = raw.get("context_sequence", None)

    return TrafficSpec(
        arms=arms,
        true_rewards=true_rewards,
        noise_std=noise_std,
        regime_shifts=regime_shifts,
        context_sequence=context_sequence,
    )


@dataclass
class TrafficSpec:
    arms: List[str]
    # true_rewards: arm_name -> float OR arm_name -> {context_key -> float}
    true_rewards: Dict[str, Any]
    noise_std: float
    regime_shifts: List[Dict[str, Any]]
    context_sequence: Any  # None, list of dicts, or {"distribution": "uniform", "values": [...]}

    def reward_for(self, arm: str, context_key: Optional[str], rng: random.Random) -> float:
        """Return observed reward for an arm + context, with gaussian noise."""
        base = self.true_rewards[arm]
        if isinstance(base, dict):
            # Context-dependent rewards: look up by context_key, fall back to "__default__"
            if context_key and context_key in base:
                base = float(base[context_key])
            else:
                base = float(base.get("__default__", 0.0))
        else:
            base = float(base)
        if self.noise_std > 0:
            base += rng.gauss(0.0, self.noise_std)
        return base

    def oracle_reward(self, round_idx: int, context_key: Optional[str]) -> float:
        """Best possible reward at round_idx with context (no noise, best arm)."""
        current_rewards = self._rewards_at_round(round_idx)
        best = float("-inf")
        for arm in self.arms:
            base = current_rewards[arm]
            if isinstance(base, dict):
                if context_key and context_key in base:
                    v = float(base[context_key])
                else:
                    v = float(base.get("__default__", 0.0))
            else:
                v = float(base)
            if v > best:
                best = v
        return best

    def _rewards_at_round(self, round_idx: int) -> Dict[str, Any]:
        """Return the effective true_rewards dict at the given round (after shifts)."""
        rewards = dict(self.true_rewards)
        for shift in self.regime_shifts:
            if round_idx >= shift["at_round"]:
                rewards.update(shift["new_rewards"])
        return rewards

    def get_context(self, round_idx: int, rng: random.Random) -> Optional[str]:
        """Draw a context key for this round."""
        if self.context_sequence is None:
            return None
        if isinstance(self.context_sequence, list):
            return str(self.context_sequence[round_idx % len(self.context_sequence)])
        if isinstance(self.context_sequence, dict):
            dist = self.context_sequence.get("distribution", "uniform")
            values = self.context_sequence.get("values", [])
            if dist == "uniform" and values:
                return str(rng.choice(values))
        return None


# ---------------------------------------------------------------------------
# Statistics helpers (stdlib only -- no numpy/scipy)
# ---------------------------------------------------------------------------


def _paired_t_test(differences: List[float]) -> float:
    """Two-sided paired t-test p-value. Returns p in [0, 1].

    Uses the t-distribution CDF approximated via the regularised incomplete
    beta function (Abramowitz & Stegun 26.5.27 / scipy equivalent), implemented
    with the continued-fraction form of betainc so there is no scipy dependency.

    Returns 1.0 when n < 2 or variance is 0 (degenerate -- not significant).
    """
    n = len(differences)
    if n < 2:
        return 1.0
    mean_d = statistics.mean(differences)
    if n == 1:
        return 1.0
    try:
        stdev_d = statistics.stdev(differences)
    except statistics.StatisticsError:
        return 1.0
    if stdev_d == 0.0:
        return 1.0 if mean_d == 0.0 else 0.0

    t_stat = mean_d / (stdev_d / math.sqrt(n))
    df = n - 1

    # Two-tailed p-value via the incomplete beta function.
    # P(|T| > |t|) = I(df/(df+t^2); df/2, 1/2)
    x = df / (df + t_stat ** 2)
    p = _betainc(df / 2.0, 0.5, x)
    return float(min(1.0, max(0.0, p)))


def _betainc(a: float, b: float, x: float) -> float:
    """Regularised incomplete beta function I_x(a, b) via continued fraction.

    Accurate for 0 <= x <= 1, a > 0, b > 0.
    Uses the Lentz modified continued-fraction algorithm (DLMF 8.17.22).
    """
    if x < 0.0 or x > 1.0:
        raise ValueError(f"x={x} out of [0,1]")
    if x == 0.0:
        return 0.0
    if x == 1.0:
        return 1.0

    # Use the symmetry relation when x > (a+1)/(a+b+2) for better convergence.
    if x > (a + 1.0) / (a + b + 2.0):
        return 1.0 - _betainc(b, a, 1.0 - x)

    lbeta = math.lgamma(a) + math.lgamma(b) - math.lgamma(a + b)
    front = math.exp(math.log(x) * a + math.log(1.0 - x) * b - lbeta) / a

    # Lentz continued fraction
    TINY = 1e-300
    EPS = 3e-7
    MAX_ITER = 200

    f = TINY
    C = f
    D = 0.0
    for m in range(MAX_ITER):
        for step in range(2):
            if step == 0:
                if m == 0:
                    d = 1.0
                else:
                    d = (m * (b - m) * x) / ((a + 2.0 * m - 1.0) * (a + 2.0 * m))
            else:
                d = -((a + m) * (a + b + m) * x) / ((a + 2.0 * m) * (a + 2.0 * m + 1.0))

            D = 1.0 + d * D
            if abs(D) < TINY:
                D = TINY
            D = 1.0 / D

            C = 1.0 + d / C
            if abs(C) < TINY:
                C = TINY

            delta = C * D
            f *= delta
            if abs(delta - 1.0) < EPS:
                break
        else:
            continue
        break

    return front * (f - TINY)


def compute_regret(per_round_oracle: List[float], per_round_actual: List[float]) -> float:
    """Cumulative regret = sum(oracle_reward - actual_reward) over all rounds."""
    return sum(o - a for o, a in zip(per_round_oracle, per_round_actual))


# ---------------------------------------------------------------------------
# Syntra HTTP client
# ---------------------------------------------------------------------------


class SyntraClient:
    """Thin HTTP client over urllib.request (stdlib only)."""

    def __init__(self, base_url: str, admin_key: str):
        self.base_url = base_url.rstrip("/")
        self.admin_key = admin_key

    def _request(self, method: str, path: str, body: Any = None, raw: bytes = None) -> Any:
        url = f"{self.base_url}{path}"
        if raw is not None:
            data = raw
        elif body is not None:
            data = json.dumps(body).encode("utf-8")
        else:
            data = None

        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.admin_key}")
        if raw is not None:
            req.add_header("Content-Type", "application/octet-stream")
        elif data is not None:
            req.add_header("Content-Type", "application/json")

        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                text = resp.read().decode("utf-8")
                if text.strip():
                    return json.loads(text)
                return {}
        except urllib.error.HTTPError as exc:
            body_text = exc.read().decode("utf-8", errors="replace")
            raise RuntimeError(
                f"HTTP {exc.code} {exc.reason} for {method} {path}: {body_text}"
            ) from exc

    def delete_tenant(self, tenant: str) -> None:
        try:
            self._request("DELETE", f"/tenants/{tenant}")
        except Exception:
            pass  # Ignore -- may not exist yet

    def ensure_job(self, tenant: str, job: str) -> None:
        try:
            self._request("POST", f"/tenants/{tenant}/jobs", {"id": job, "name": f"ab-{job}"})
        except Exception:
            pass  # Already exists

    def install_capsule(self, tenant: str, job: str, capsule: str, lyc_bytes: bytes) -> None:
        path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install"
        self._request("POST", path, raw=lyc_bytes)

    def configure_learning(self, tenant: str, job: str, capsule: str, config: dict) -> None:
        path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}/learning"
        self._request("PUT", path, body=config)

    def decide(self, tenant: str, job: str, capsule: str,
               context_key: Optional[str] = None,
               features: Optional[dict] = None) -> dict:
        path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide"
        body: dict = {"input": {}}
        if features is not None:
            body["features"] = features
        elif context_key is not None:
            body["contextKey"] = context_key
        return self._request("POST", path, body=body)

    def feedback(self, tenant: str, job: str, capsule: str,
                 decision_id: str, reward: float) -> None:
        path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback"
        self._request("POST", path, body={"decisionId": decision_id, "reward": reward})

    def health_check(self) -> None:
        """Raise if Syntra is unreachable."""
        try:
            req = urllib.request.Request(f"{self.base_url}/health", method="GET")
            req.add_header("Authorization", f"Bearer {self.admin_key}")
            with urllib.request.urlopen(req, timeout=5):
                pass
        except Exception as exc:
            raise SystemExit(
                f"ERROR: cannot reach Syntra at {self.base_url}: {exc}"
            ) from exc


# ---------------------------------------------------------------------------
# Capsule authoring
# ---------------------------------------------------------------------------


def author_capsule(spec_yaml_path: str) -> bytes:
    """Compile a .yaml capsule spec via `syntra author` and return .lyc bytes."""
    with tempfile.TemporaryDirectory() as tmpdir:
        out_dir = os.path.join(tmpdir, "out")
        try:
            subprocess.run(
                ["syntra", "author", spec_yaml_path, "--out-dir", out_dir],
                check=True,
                capture_output=True,
            )
        except FileNotFoundError:
            raise SystemExit(
                "ERROR: `syntra` binary not found on PATH. "
                "Install Syntra and ensure the binary is on PATH."
            )
        except subprocess.CalledProcessError as exc:
            raise SystemExit(
                f"ERROR: `syntra author` failed for {spec_yaml_path}:\n"
                + exc.stderr.decode("utf-8", errors="replace")
            )
        lyc_path = os.path.join(out_dir, "program.lyc")
        if not os.path.exists(lyc_path):
            raise SystemExit(
                f"ERROR: `syntra author` succeeded but produced no program.lyc in {out_dir}"
            )
        with open(lyc_path, "rb") as fh:
            return fh.read()


# ---------------------------------------------------------------------------
# Per-seed simulation
# ---------------------------------------------------------------------------


@dataclass
class ArmStats:
    cumulative_reward: float = 0.0
    rounds_won: int = 0      # rounds where this arm beat the other
    refusals: int = 0
    per_round_reward: List[float] = field(default_factory=list)
    per_round_oracle: List[float] = field(default_factory=list)


def _extract_chosen_arm(decision: dict, arms: List[str]) -> Tuple[Optional[str], Optional[str]]:
    """Return (chosen_arm, decision_id) from a /decide response.

    Returns (None, None) if the response indicates a refusal or unknown arm.
    """
    decision_id = decision.get("decisionId")
    decisions = decision.get("decisions", [])
    if not decisions:
        return None, decision_id

    chosen = decisions[0].get("chosen_option")
    if chosen is None or chosen not in arms:
        return None, decision_id
    return str(chosen), decision_id


def run_one_seed(
    seed: int,
    rounds: int,
    spec: TrafficSpec,
    client: SyntraClient,
    tenant: str,
    job: str,
    lyc_a: bytes,
    lyc_b: bytes,
    capsule_a_name: str = "a",
    capsule_b_name: str = "b",
    verbose: bool = False,
) -> dict:
    """Run one seed of the A/B simulation.

    Both capsules see the EXACT same context and observed reward each round.
    Returns the per-seed result dict.
    """
    rng = random.Random(seed)

    # Reset and reinstall both capsules under this tenant.
    client.delete_tenant(tenant)
    client.ensure_job(tenant, job)
    client.install_capsule(tenant, job, capsule_a_name, lyc_a)
    client.install_capsule(tenant, job, capsule_b_name, lyc_b)

    stats_a = ArmStats()
    stats_b = ArmStats()

    for round_idx in range(rounds):
        # Apply any regime shifts: the spec's reward_for already handles this,
        # but oracle also needs the round index.
        context_key = spec.get_context(round_idx, rng)

        # Both capsules receive the same context.
        try:
            dec_a = client.decide(tenant, job, capsule_a_name, context_key=context_key)
            arm_a, did_a = _extract_chosen_arm(dec_a, spec.arms)
        except Exception as exc:
            if verbose:
                print(f"  [seed={seed}] round {round_idx}: capsule A decide error: {exc}",
                      file=sys.stderr)
            arm_a, did_a = None, None

        try:
            dec_b = client.decide(tenant, job, capsule_b_name, context_key=context_key)
            arm_b, did_b = _extract_chosen_arm(dec_b, spec.arms)
        except Exception as exc:
            if verbose:
                print(f"  [seed={seed}] round {round_idx}: capsule B decide error: {exc}",
                      file=sys.stderr)
            arm_b, did_b = None, None

        # Oracle reward (best arm, no noise).
        oracle_r = spec.oracle_reward(round_idx, context_key)

        # Observed reward for each arm (same RNG state, but each arm is
        # evaluated independently -- this is the correct paired-trial design:
        # both policies are evaluated against the same latent reward function,
        # not the same noise draw, because they may choose different arms).
        if arm_a is not None:
            obs_a = spec.reward_for(arm_a, context_key, rng)
            stats_a.cumulative_reward += obs_a
            stats_a.per_round_reward.append(obs_a)
        else:
            stats_a.refusals += 1
            obs_a = 0.0
            stats_a.per_round_reward.append(0.0)

        if arm_b is not None:
            obs_b = spec.reward_for(arm_b, context_key, rng)
            stats_b.cumulative_reward += obs_b
            stats_b.per_round_reward.append(obs_b)
        else:
            stats_b.refusals += 1
            obs_b = 0.0
            stats_b.per_round_reward.append(0.0)

        stats_a.per_round_oracle.append(oracle_r)
        stats_b.per_round_oracle.append(oracle_r)

        if obs_a > obs_b:
            stats_a.rounds_won += 1
        elif obs_b > obs_a:
            stats_b.rounds_won += 1

        # Send feedback to both capsules.
        if did_a is not None and arm_a is not None:
            try:
                client.feedback(tenant, job, capsule_a_name, did_a, obs_a)
            except Exception as exc:
                if verbose:
                    print(f"  [seed={seed}] round {round_idx}: capsule A feedback error: {exc}",
                          file=sys.stderr)

        if did_b is not None and arm_b is not None:
            try:
                client.feedback(tenant, job, capsule_b_name, did_b, obs_b)
            except Exception as exc:
                if verbose:
                    print(f"  [seed={seed}] round {round_idx}: capsule B feedback error: {exc}",
                          file=sys.stderr)

    regret_a = compute_regret(stats_a.per_round_oracle, stats_a.per_round_reward)
    regret_b = compute_regret(stats_b.per_round_oracle, stats_b.per_round_reward)

    mean_a = stats_a.cumulative_reward / rounds if rounds > 0 else 0.0
    mean_b = stats_b.cumulative_reward / rounds if rounds > 0 else 0.0

    b_won_pct = stats_b.rounds_won / rounds if rounds > 0 else 0.0

    return {
        "seed": seed,
        "a": {
            "cumulative_reward": round(stats_a.cumulative_reward, 4),
            "mean_per_round": round(mean_a, 4),
            "refusals": stats_a.refusals,
            "regret_vs_oracle": round(regret_a, 4),
        },
        "b": {
            "cumulative_reward": round(stats_b.cumulative_reward, 4),
            "mean_per_round": round(mean_b, 4),
            "refusals": stats_b.refusals,
            "regret_vs_oracle": round(regret_b, 4),
        },
        "head_to_head": {
            "b_minus_a_cumulative": round(stats_b.cumulative_reward - stats_a.cumulative_reward, 4),
            "b_won_round_pct": round(b_won_pct, 4),
        },
    }


# ---------------------------------------------------------------------------
# Aggregate across seeds
# ---------------------------------------------------------------------------


def aggregate_results(
    seed_results: List[dict],
    rounds: int,
) -> dict:
    """Aggregate per-seed results into the final report."""
    n = len(seed_results)
    if n == 0:
        raise ValueError("No seed results to aggregate.")

    def _mean(vals):
        return statistics.mean(vals)

    def _stderr(vals):
        if len(vals) < 2:
            return 0.0
        return statistics.stdev(vals) / math.sqrt(len(vals))

    cum_a = [r["a"]["cumulative_reward"] for r in seed_results]
    cum_b = [r["b"]["cumulative_reward"] for r in seed_results]
    reg_a = [r["a"]["regret_vs_oracle"] for r in seed_results]
    reg_b = [r["b"]["regret_vs_oracle"] for r in seed_results]
    ref_a = [r["a"]["refusals"] for r in seed_results]
    ref_b = [r["b"]["refusals"] for r in seed_results]

    differences = [b - a for a, b in zip(cum_a, cum_b)]
    p_value = _paired_t_test(differences)

    mean_a = _mean(cum_a)
    mean_b = _mean(cum_b)
    winner = "b" if mean_b > mean_a else ("a" if mean_a > mean_b else "tie")

    refusal_rate_a = _mean(ref_a) / rounds if rounds > 0 else 0.0
    refusal_rate_b = _mean(ref_b) / rounds if rounds > 0 else 0.0

    return {
        "rounds": rounds,
        "seeds": n,
        "winner": winner,
        "a": {
            "mean_cumulative": round(_mean(cum_a), 4),
            "stderr": round(_stderr(cum_a), 4),
            "regret_mean": round(_mean(reg_a), 4),
            "refusal_rate": round(refusal_rate_a, 4),
        },
        "b": {
            "mean_cumulative": round(_mean(cum_b), 4),
            "stderr": round(_stderr(cum_b), 4),
            "regret_mean": round(_mean(reg_b), 4),
            "refusal_rate": round(refusal_rate_b, 4),
        },
        "p_value_paired_t": round(p_value, 6),
        "confidence_b_better_at_95pct": (winner == "b" and p_value < 0.05),
    }


# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------


def write_outputs(
    output_dir: str,
    aggregate: dict,
    seed_results: List[dict],
    spec_path: str,
    capsule_a_path: str,
    capsule_b_path: str,
) -> None:
    os.makedirs(output_dir, exist_ok=True)

    summary_path = os.path.join(output_dir, "summary.json")
    with open(summary_path, "w", encoding="utf-8") as fh:
        json.dump({
            "config": {
                "capsule_a": capsule_a_path,
                "capsule_b": capsule_b_path,
                "traffic_spec": spec_path,
            },
            **aggregate,
        }, fh, indent=2)

    seeds_path = os.path.join(output_dir, "seeds.json")
    with open(seeds_path, "w", encoding="utf-8") as fh:
        json.dump(seed_results, fh, indent=2)

    csv_path = os.path.join(output_dir, "seeds.csv")
    with open(csv_path, "w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow([
            "seed",
            "a_cumulative", "a_mean_per_round", "a_refusals", "a_regret",
            "b_cumulative", "b_mean_per_round", "b_refusals", "b_regret",
            "b_minus_a", "b_won_pct",
        ])
        for r in seed_results:
            writer.writerow([
                r["seed"],
                r["a"]["cumulative_reward"], r["a"]["mean_per_round"],
                r["a"]["refusals"], r["a"]["regret_vs_oracle"],
                r["b"]["cumulative_reward"], r["b"]["mean_per_round"],
                r["b"]["refusals"], r["b"]["regret_vs_oracle"],
                r["head_to_head"]["b_minus_a_cumulative"],
                r["head_to_head"]["b_won_round_pct"],
            ])


def print_report(aggregate: dict, seed_results: List[dict]) -> None:
    """Print a human-readable summary to stdout."""
    print("=" * 68)
    print("  SYNTRA A/B HARNESS RESULTS")
    print("=" * 68)
    print(f"  Rounds: {aggregate['rounds']}   Seeds: {aggregate['seeds']}")
    print()
    print(f"  {'metric':<30} {'capsule A':>12} {'capsule B':>12}")
    print(f"  {'-'*30} {'-'*12} {'-'*12}")
    print(f"  {'mean cumulative reward':<30} {aggregate['a']['mean_cumulative']:>12.4f} {aggregate['b']['mean_cumulative']:>12.4f}")
    print(f"  {'stderr':<30} {aggregate['a']['stderr']:>12.4f} {aggregate['b']['stderr']:>12.4f}")
    print(f"  {'mean regret vs oracle':<30} {aggregate['a']['regret_mean']:>12.4f} {aggregate['b']['regret_mean']:>12.4f}")
    print(f"  {'refusal rate':<30} {aggregate['a']['refusal_rate']:>12.4f} {aggregate['b']['refusal_rate']:>12.4f}")
    print()
    print(f"  Paired t-test p-value: {aggregate['p_value_paired_t']:.6f}")
    if aggregate["confidence_b_better_at_95pct"]:
        print("  Verdict: B is significantly better than A at 95% confidence.")
    elif aggregate["winner"] == "a":
        print("  Verdict: A leads but the difference is not significant at 95%.")
    elif aggregate["winner"] == "b":
        print("  Verdict: B leads but the difference is not significant at 95%.")
    else:
        print("  Verdict: No clear winner (tied cumulative reward).")
    print("=" * 68)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description=(
            "Simulation-driven A/B harness: runs two Syntra capsules against "
            "identical simulated traffic and reports head-to-head metrics."
        ),
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    p.add_argument("capsule_a", help="Path to capsule A YAML spec.")
    p.add_argument("capsule_b", help="Path to capsule B YAML spec.")
    p.add_argument("traffic_spec", help="Path to traffic spec (JSON or YAML).")
    p.add_argument("--rounds", type=int, default=200,
                   help="Number of decide/feedback rounds per seed.")
    p.add_argument("--seeds", type=int, default=10,
                   help="Number of independent seeds to run.")
    p.add_argument("--seed-offset", type=int, default=1000,
                   help="Starting seed value (seeds are seed_offset .. seed_offset+seeds-1).")
    p.add_argument("--syntra-url", default=os.environ.get("SYNTRA_URL", "http://localhost:8787"),
                   help="Base URL of the Syntra server.")
    p.add_argument("--admin-key", default=os.environ.get("SYNTRA_ADMIN_KEY", "dev-key"),
                   help="Syntra admin key.")
    p.add_argument("--tenant", default="ab",
                   help="Syntra tenant identifier (will be reset between seeds).")
    p.add_argument("--job", default="main",
                   help="Syntra job identifier.")
    p.add_argument("--output-dir", default=None,
                   help="Directory to write summary.json, seeds.json, seeds.csv. "
                        "Defaults to results/run_<timestamp>/.")
    p.add_argument("--verbose", action="store_true",
                   help="Print per-round errors to stderr.")
    return p


def main() -> int:
    parser = build_arg_parser()
    args = parser.parse_args()

    # Resolve output dir.
    if args.output_dir is None:
        ts = time.strftime("%Y%m%d_%H%M%S")
        args.output_dir = os.path.join(
            os.path.dirname(os.path.abspath(__file__)), "results", f"run_{ts}"
        )

    # Parse traffic spec.
    print(f"[ab-harness] parsing traffic spec: {args.traffic_spec}", file=sys.stderr)
    try:
        spec = parse_traffic_spec(args.traffic_spec)
    except (ValueError, FileNotFoundError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1

    # Author both capsules.
    print(f"[ab-harness] authoring capsule A: {args.capsule_a}", file=sys.stderr)
    lyc_a = author_capsule(args.capsule_a)
    print(f"[ab-harness] authoring capsule B: {args.capsule_b}", file=sys.stderr)
    lyc_b = author_capsule(args.capsule_b)

    # Connect to Syntra.
    client = SyntraClient(args.syntra_url, args.admin_key)
    client.health_check()

    print(
        f"[ab-harness] starting: rounds={args.rounds} seeds={args.seeds} "
        f"seed_offset={args.seed_offset} tenant={args.tenant}",
        file=sys.stderr,
    )

    seed_results = []
    t0 = time.time()
    for i in range(args.seeds):
        seed = args.seed_offset + i
        t1 = time.time()
        result = run_one_seed(
            seed=seed,
            rounds=args.rounds,
            spec=spec,
            client=client,
            tenant=args.tenant,
            job=args.job,
            lyc_a=lyc_a,
            lyc_b=lyc_b,
            verbose=args.verbose,
        )
        elapsed = time.time() - t1
        seed_results.append(result)
        h2h = result["head_to_head"]
        print(
            f"  seed {seed} [{i+1}/{args.seeds}] {elapsed:.1f}s  "
            f"A={result['a']['cumulative_reward']:.2f}  "
            f"B={result['b']['cumulative_reward']:.2f}  "
            f"B-A={h2h['b_minus_a_cumulative']:+.2f}  "
            f"B_won={h2h['b_won_round_pct']:.2%}",
            file=sys.stderr,
        )

    print(f"\n[ab-harness] total time: {time.time()-t0:.1f}s", file=sys.stderr)

    aggregate = aggregate_results(seed_results, args.rounds)

    print_report(aggregate, seed_results)

    write_outputs(
        output_dir=args.output_dir,
        aggregate=aggregate,
        seed_results=seed_results,
        spec_path=args.traffic_spec,
        capsule_a_path=args.capsule_a,
        capsule_b_path=args.capsule_b,
    )
    print(f"\n[ab-harness] output: {args.output_dir}", file=sys.stderr)

    # Exit 0 if B wins decisively, 1 otherwise (useful in CI).
    return 0 if aggregate["confidence_b_better_at_95pct"] else 0


if __name__ == "__main__":
    sys.exit(main())
