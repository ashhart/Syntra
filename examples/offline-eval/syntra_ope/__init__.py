"""syntra_ope — Offline Policy Evaluation for Syntra capsules.

Implements Inverse Propensity Score (IPS) and Doubly Robust (DR) estimators
as described in Dudik, Langford, Li, "Doubly Robust Policy Evaluation and
Learning" (ICML 2011).

Usage:
    from syntra_ope import load_csv, ips_estimate, dr_estimate, bootstrap_ci, EvalPolicy

Stdlib only: csv, statistics, random, math, json, collections.
"""

from __future__ import annotations

import csv
import json
import math
import random
import statistics
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Sequence, Tuple


# ---------------------------------------------------------------------------
# Data types
# ---------------------------------------------------------------------------


@dataclass
class LoggedRow:
    """One row from a logged-decisions CSV.

    Fields
    ------
    decision_id : str
    context_key : str
        Discrete context identifier (matches Syntra's contextKey).
    action : str
        The action the logging policy chose.
    propensity : float
        P(action | context) under the logging policy.
    reward : float
        Observed reward after the action was taken.
    """

    decision_id: str
    context_key: str
    action: str
    propensity: float
    reward: float


@dataclass
class OPEResult:
    mean: float
    ci_5: float
    ci_95: float


@dataclass
class EvaluationOutput:
    n_rows: int
    logging_policy_mean_reward: float
    eval_policy_estimates: Dict[str, OPEResult]
    warnings: List[str]

    def to_dict(self) -> dict:
        return {
            "n_rows": self.n_rows,
            "logging_policy_mean_reward": round(self.logging_policy_mean_reward, 6),
            "eval_policy_estimates": {
                name: {
                    "mean": round(est.mean, 6),
                    "ci_5": round(est.ci_5, 6),
                    "ci_95": round(est.ci_95, 6),
                }
                for name, est in self.eval_policy_estimates.items()
            },
            "warnings": self.warnings,
        }


# ---------------------------------------------------------------------------
# CSV loading
# ---------------------------------------------------------------------------

REQUIRED_COLUMNS = {"decision_id", "context_key", "action", "propensity", "reward"}


class MissingPropensityError(ValueError):
    """Raised when a row is missing a propensity value."""


class CSVSchemaError(ValueError):
    """Raised when the CSV is missing required columns."""


def load_csv(path: str) -> List[LoggedRow]:
    """Load a logged-decisions CSV file into a list of LoggedRow objects.

    Parameters
    ----------
    path : str
        Filesystem path to the CSV file.

    Returns
    -------
    list[LoggedRow]

    Raises
    ------
    CSVSchemaError
        If required columns are absent.
    MissingPropensityError
        If any row has a blank propensity value.
    """
    with open(path, newline="", encoding="utf-8") as fh:
        reader = csv.DictReader(fh)
        if reader.fieldnames is None:
            raise CSVSchemaError("CSV file is empty or has no header row.")
        actual_cols = set(reader.fieldnames)
        missing = REQUIRED_COLUMNS - actual_cols
        if missing:
            raise CSVSchemaError(
                f"CSV is missing required columns: {sorted(missing)}. "
                f"Found: {sorted(actual_cols)}."
            )
        rows: List[LoggedRow] = []
        for i, raw in enumerate(reader, start=2):  # line 1 is header
            prop_str = raw["propensity"].strip()
            if not prop_str:
                raise MissingPropensityError(
                    f"Row {i} (decision_id={raw['decision_id']!r}) has a blank "
                    "propensity value. All rows must have a propensity score for "
                    "IPS and DR estimation. Either fill in the values or exclude "
                    "the rows before passing the file to this tool."
                )
            rows.append(
                LoggedRow(
                    decision_id=raw["decision_id"].strip(),
                    context_key=raw["context_key"].strip(),
                    action=raw["action"].strip(),
                    propensity=float(prop_str),
                    reward=float(raw["reward"].strip()),
                )
            )
    return rows


# ---------------------------------------------------------------------------
# Eval policy interface
# ---------------------------------------------------------------------------


class EvalPolicy:
    """Maps (context_key, action) -> probability under the evaluation policy.

    Parameters
    ----------
    policy_table : dict[str, str]
        Maps context_key -> action (the deterministic best action for that
        context). Built from a converged-policy JSON.
    fallback_action : str | None
        Action to return for unseen contexts.  If None and the context is
        unseen, the policy assigns probability 0 to all actions
        (treated as "no coverage").
    """

    def __init__(
        self,
        policy_table: Dict[str, str],
        fallback_action: Optional[str] = None,
    ) -> None:
        self._table = policy_table
        self._fallback = fallback_action

    @classmethod
    def from_json(cls, path: str, fallback_action: Optional[str] = None) -> "EvalPolicy":
        """Load a policy JSON file (dict mapping context_key -> action)."""
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
        return cls(data, fallback_action=fallback_action)

    def action_for(self, context_key: str) -> Optional[str]:
        """Return the action this policy would choose for the given context.

        Returns None if the context is unseen and no fallback is configured.
        """
        if context_key in self._table:
            return self._table[context_key]
        return self._fallback

    def probability(self, context_key: str, action: str) -> float:
        """Return P(action | context) under this deterministic policy.

        A deterministic policy assigns probability 1 to the chosen action and
        0 to all others. Unseen contexts with no fallback return 0.
        """
        chosen = self.action_for(context_key)
        if chosen is None:
            return 0.0
        return 1.0 if chosen == action else 0.0

    @classmethod
    def from_syntra_bandit(
        cls,
        log: Sequence[LoggedRow],
        syntra_url: str,
        capsule_yaml_path: str,
        admin_key: str = "dev-key",
        tenant: str = "ope",
        job_id: str = "ope-job",
        capsule_id: str = "ope-capsule",
    ) -> "EvalPolicy":
        """Build an eval policy by replaying the log against a live Syntra.

        Sends each row in sequence to Syntra /decide (without feedback),
        collects the chosen action per context, and returns the resulting
        deterministic policy table.

        This is the --mode bandit path. It requires a running Syntra server.
        """
        import urllib.request

        base = syntra_url.rstrip("/")
        base_path = f"/tenants/{tenant}/jobs/{job_id}/capsules/{capsule_id}"

        # Install capsule
        req = urllib.request.Request(
            f"{base}{base_path}/install",
            data=open(capsule_yaml_path, "rb").read(),
            method="POST",
        )
        req.add_header("Authorization", f"Bearer {admin_key}")
        req.add_header("Content-Type", "application/octet-stream")
        with urllib.request.urlopen(req, timeout=15) as r:
            r.read()

        policy_table: Dict[str, str] = {}
        for row in log:
            body = json.dumps({"contextKey": row.context_key}).encode()
            req = urllib.request.Request(
                f"{base}{base_path}/decide",
                data=body,
                method="POST",
            )
            req.add_header("Authorization", f"Bearer {admin_key}")
            req.add_header("Content-Type", "application/json")
            with urllib.request.urlopen(req, timeout=10) as r:
                resp = json.loads(r.read().decode())

            decisions = resp.get("decisions") or []
            if decisions:
                chosen_option = decisions[0].get("chosen_option")
                if chosen_option is not None:
                    policy_table[row.context_key] = str(chosen_option)

            # Feed back the actual reward so the bandit evolves
            decision_id = resp.get("decisionId")
            if decision_id:
                fb_body = json.dumps(
                    {"decisionId": decision_id, "reward": row.reward}
                ).encode()
                fb_req = urllib.request.Request(
                    f"{base}{base_path}/feedback",
                    data=fb_body,
                    method="POST",
                )
                fb_req.add_header("Authorization", f"Bearer {admin_key}")
                fb_req.add_header("Content-Type", "application/json")
                with urllib.request.urlopen(fb_req, timeout=10) as r:
                    r.read()

        return cls(policy_table, fallback_action=None)


# ---------------------------------------------------------------------------
# Reward model (per-(context, action) sample mean)
# ---------------------------------------------------------------------------


class RewardModel:
    """Lookup-table reward model: per-(context, action) sample mean.

    Fitted on a list of LoggedRow objects. Used by the DR estimator to
    provide a direct-method estimate that the IPS correction augments.
    """

    def __init__(self) -> None:
        self._sums: Dict[Tuple[str, str], float] = defaultdict(float)
        self._counts: Dict[Tuple[str, str], int] = defaultdict(int)

    def fit(self, log: Sequence[LoggedRow]) -> None:
        for row in log:
            key = (row.context_key, row.action)
            self._sums[key] += row.reward
            self._counts[key] += 1

    def predict(self, context_key: str, action: str) -> Optional[float]:
        """Return the mean reward for (context, action), or None if unseen."""
        key = (context_key, action)
        if self._counts[key] == 0:
            return None
        return self._sums[key] / self._counts[key]

    def global_mean(self) -> float:
        """Fallback: global mean across all (context, action) pairs."""
        total = sum(self._sums.values())
        count = sum(self._counts.values())
        return total / count if count > 0 else 0.0


# ---------------------------------------------------------------------------
# IPS estimator
# ---------------------------------------------------------------------------


def ips_estimate(
    log: Sequence[LoggedRow],
    eval_policy: EvalPolicy,
    warnings: Optional[List[str]] = None,
) -> Tuple[float, int]:
    """Inverse Propensity Score estimator.

    For each logged row:
        weight = I[pi_eval(action|context) > 0] * pi_eval(action|context) / propensity
        contribution = weight * reward

    Returns the mean weighted reward across all rows.

    Parameters
    ----------
    log : sequence of LoggedRow
    eval_policy : EvalPolicy
    warnings : list, optional
        If provided, dropped-row warnings are appended here.

    Returns
    -------
    (estimate, n_effective)
        estimate   : float, the IPS mean
        n_effective : int, number of rows included (after dropping zero-propensity)
    """
    if warnings is None:
        warnings = []

    total = 0.0
    n_effective = 0
    n_zero_propensity = 0
    n_total = len(log)

    for row in log:
        if row.propensity <= 0.0:
            n_zero_propensity += 1
            continue
        pi_eval = eval_policy.probability(row.context_key, row.action)
        weight = pi_eval / row.propensity
        total += weight * row.reward
        n_effective += 1

    if n_zero_propensity > 0:
        warnings.append(
            f"{n_zero_propensity} row(s) had propensity <= 0 and were dropped "
            f"({n_zero_propensity}/{n_total} rows)."
        )

    if n_effective == 0:
        return 0.0, 0

    return total / n_total, n_effective


# ---------------------------------------------------------------------------
# DR estimator
# ---------------------------------------------------------------------------


def dr_estimate(
    log: Sequence[LoggedRow],
    eval_policy: EvalPolicy,
    reward_model: RewardModel,
    warnings: Optional[List[str]] = None,
) -> Tuple[float, int]:
    """Doubly Robust estimator.

    Combines direct-method (reward model) with IPS correction:

        DR = (1/N) * sum_i [
            mu(c_i, pi_eval(c_i))          # model prediction for eval action
            + I[pi_eval(a_i|c_i) > 0] * (pi_eval(a_i|c_i) / p_i) * (r_i - mu(c_i, a_i))
        ]

    where:
        mu(c, a) = reward model prediction for (context, action)
        a_i      = action taken by logging policy
        pi_eval(a_i|c_i) = eval policy probability for the logged action
        p_i      = logging policy propensity

    The DR estimator is consistent if either the reward model OR the
    propensity weights are correct (doubly robust property).

    Returns
    -------
    (estimate, n_effective)
    """
    if warnings is None:
        warnings = []

    global_fallback = reward_model.global_mean()
    total = 0.0
    n_effective = 0
    n_zero_propensity = 0
    n_total = len(log)

    for row in log:
        # Direct-method term: what the model predicts for the eval policy's action
        eval_action = eval_policy.action_for(row.context_key)
        if eval_action is not None:
            mu_eval = reward_model.predict(row.context_key, eval_action)
            if mu_eval is None:
                mu_eval = global_fallback
        else:
            mu_eval = global_fallback

        # IPS correction term
        if row.propensity <= 0.0:
            n_zero_propensity += 1
            # Fall back to direct-method only for this row
            total += mu_eval
            n_effective += 1
            continue

        pi_eval = eval_policy.probability(row.context_key, row.action)
        mu_logged = reward_model.predict(row.context_key, row.action)
        if mu_logged is None:
            mu_logged = global_fallback

        ips_correction = (pi_eval / row.propensity) * (row.reward - mu_logged)
        total += mu_eval + ips_correction
        n_effective += 1

    if n_zero_propensity > 0:
        warnings.append(
            f"DR: {n_zero_propensity} row(s) had propensity <= 0; "
            "fell back to direct-method for those rows."
        )

    if n_effective == 0:
        return 0.0, 0

    return total / n_total, n_effective


# ---------------------------------------------------------------------------
# Bootstrap confidence intervals
# ---------------------------------------------------------------------------


def _resample(log: Sequence[LoggedRow], rng: random.Random) -> List[LoggedRow]:
    n = len(log)
    log_list = list(log)
    return [log_list[rng.randint(0, n - 1)] for _ in range(n)]


def bootstrap_ci(
    log: Sequence[LoggedRow],
    eval_policy: EvalPolicy,
    n_bootstrap: int = 200,
    seed: int = 42,
    quantiles: Tuple[float, float] = (0.05, 0.95),
) -> Dict[str, OPEResult]:
    """Compute bootstrap confidence intervals for IPS and DR estimators.

    Resamples the log B times with replacement, computes IPS and DR for each
    resample, then reports the requested quantile range.

    Parameters
    ----------
    log : sequence of LoggedRow
    eval_policy : EvalPolicy
    n_bootstrap : int
        Number of bootstrap resamples (default 200).
    seed : int
        Random seed for reproducibility.
    quantiles : tuple of two floats
        Lower and upper quantile for the CI (default 5th/95th percentile).

    Returns
    -------
    dict with keys "ips" and "dr", each an OPEResult(mean, ci_5, ci_95).
    """
    rng = random.Random(seed)
    ips_samples: List[float] = []
    dr_samples: List[float] = []

    # Fit a global reward model on the full dataset (used across all resamples
    # for the DM component; the IPS correction still uses per-resample rewards).
    global_model = RewardModel()
    global_model.fit(log)

    for _ in range(n_bootstrap):
        resample = _resample(log, rng)

        # Fit per-resample reward model for DR
        model = RewardModel()
        model.fit(resample)

        ips_val, _ = ips_estimate(resample, eval_policy)
        dr_val, _ = dr_estimate(resample, eval_policy, model)

        ips_samples.append(ips_val)
        dr_samples.append(dr_val)

    def _quantile(samples: List[float], q: float) -> float:
        sorted_s = sorted(samples)
        idx = q * (len(sorted_s) - 1)
        lo = int(idx)
        hi = min(lo + 1, len(sorted_s) - 1)
        frac = idx - lo
        return sorted_s[lo] * (1 - frac) + sorted_s[hi] * frac

    q_lo, q_hi = quantiles

    return {
        "ips": OPEResult(
            mean=statistics.mean(ips_samples),
            ci_5=_quantile(ips_samples, q_lo),
            ci_95=_quantile(ips_samples, q_hi),
        ),
        "dr": OPEResult(
            mean=statistics.mean(dr_samples),
            ci_5=_quantile(dr_samples, q_lo),
            ci_95=_quantile(dr_samples, q_hi),
        ),
    }


# ---------------------------------------------------------------------------
# High-level runner
# ---------------------------------------------------------------------------


def evaluate(
    log: Sequence[LoggedRow],
    eval_policy: EvalPolicy,
    n_bootstrap: int = 200,
    bootstrap_seed: int = 42,
) -> EvaluationOutput:
    """Run IPS + DR estimation with bootstrap CIs on a log.

    Parameters
    ----------
    log : sequence of LoggedRow
    eval_policy : EvalPolicy
    n_bootstrap : int
    bootstrap_seed : int

    Returns
    -------
    EvaluationOutput
    """
    warnings: List[str] = []

    if not log:
        return EvaluationOutput(
            n_rows=0,
            logging_policy_mean_reward=0.0,
            eval_policy_estimates={
                "ips": OPEResult(0.0, 0.0, 0.0),
                "dr": OPEResult(0.0, 0.0, 0.0),
            },
            warnings=["Log is empty — no estimates produced."],
        )

    logging_mean = statistics.mean(r.reward for r in log)

    model = RewardModel()
    model.fit(log)

    ci_results = bootstrap_ci(
        log, eval_policy, n_bootstrap=n_bootstrap, seed=bootstrap_seed
    )

    return EvaluationOutput(
        n_rows=len(log),
        logging_policy_mean_reward=logging_mean,
        eval_policy_estimates=ci_results,
        warnings=warnings,
    )


__all__ = [
    "LoggedRow",
    "OPEResult",
    "EvaluationOutput",
    "EvalPolicy",
    "RewardModel",
    "load_csv",
    "ips_estimate",
    "dr_estimate",
    "bootstrap_ci",
    "evaluate",
    "MissingPropensityError",
    "CSVSchemaError",
]
