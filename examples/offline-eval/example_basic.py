"""Minimal demonstration of syntra_ope offline policy evaluation.

This script runs purely in static mode — no running Syntra server required.
It loads the bundled example_data.csv, applies the example converged policy,
and prints IPS and DR estimates with 95% bootstrap confidence intervals.

Usage:
    cd Syntra/examples/offline-eval
    python3 example_basic.py

Output: JSON to stdout, informational notes to stderr.
"""

from __future__ import annotations

import json
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from syntra_ope import EvalPolicy, evaluate, load_csv

# Paths relative to this script
CSV_PATH = os.path.join(_HERE, "example_data.csv")
POLICY_PATH = os.path.join(_HERE, "examples", "converged_policy.json")


def main() -> None:
    # 1. Load the logged decisions
    log = load_csv(CSV_PATH)
    print(f"Loaded {len(log)} logged decisions.", file=sys.stderr)

    # 2. Define the evaluation policy from a converged-policy JSON.
    #    The policy says: for low_risk contexts prefer policy_a,
    #    for high_risk contexts prefer policy_b.
    eval_policy = EvalPolicy.from_json(POLICY_PATH, fallback_action=None)

    # 3. Run evaluation (IPS + DR with bootstrap CIs)
    result = evaluate(
        log=log,
        eval_policy=eval_policy,
        n_bootstrap=200,
        bootstrap_seed=42,
    )

    # 4. Print results as JSON
    print(json.dumps(result.to_dict(), indent=2))

    # 5. Human-readable interpretation
    ips = result.eval_policy_estimates["ips"]
    dr = result.eval_policy_estimates["dr"]
    logging_mean = result.logging_policy_mean_reward

    print("\nInterpretation:", file=sys.stderr)
    print(
        f"  Logging policy mean reward : {logging_mean:.4f}",
        file=sys.stderr,
    )
    print(
        f"  IPS estimate               : {ips.mean:.4f} "
        f"(95% CI: [{ips.ci_5:.4f}, {ips.ci_95:.4f}])",
        file=sys.stderr,
    )
    print(
        f"  DR  estimate               : {dr.mean:.4f} "
        f"(95% CI: [{dr.ci_5:.4f}, {dr.ci_95:.4f}])",
        file=sys.stderr,
    )

    if ips.mean > logging_mean:
        print(
            f"  Eval policy looks better than logging policy "
            f"by IPS: +{ips.mean - logging_mean:.4f}",
            file=sys.stderr,
        )
    elif ips.mean < logging_mean:
        print(
            f"  Eval policy looks worse than logging policy "
            f"by IPS: {ips.mean - logging_mean:.4f}",
            file=sys.stderr,
        )
    else:
        print("  Eval policy matches logging policy (IPS).", file=sys.stderr)

    if result.warnings:
        print("\nWarnings:", file=sys.stderr)
        for w in result.warnings:
            print(f"  - {w}", file=sys.stderr)


if __name__ == "__main__":
    main()
