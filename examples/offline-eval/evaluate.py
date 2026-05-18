#!/usr/bin/env python3
"""syntra-ope evaluate — Offline Policy Evaluation CLI.

Usage
-----
Static mode (no running Syntra required):
    python evaluate.py <logged_data.csv> \\
        --policy-json examples/converged_policy.json \\
        --mode static \\
        --format json

Bandit mode (replays log against a live Syntra):
    python evaluate.py <logged_data.csv> \\
        --capsule path/to/capsule.yaml \\
        --mode bandit \\
        --syntra-url http://localhost:8787 \\
        --format json

The static mode evaluates a converged policy (supplied as a JSON mapping
context_key -> action) without contacting Syntra. The bandit mode installs a
fresh capsule, replays the log row-by-row sending /decide and /feedback, and
builds the eval policy from Syntra's actual choices.
"""

from __future__ import annotations

import argparse
import json
import sys
import os

# Allow running from the project root without installing the package.
_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from syntra_ope import (
    EvalPolicy,
    RewardModel,
    bootstrap_ci,
    evaluate,
    load_csv,
)


def parse_args(argv=None):
    p = argparse.ArgumentParser(
        prog="syntra-ope evaluate",
        description="Offline policy evaluation: estimates how Syntra would have "
        "performed on a log of decisions made by an existing system.",
    )
    p.add_argument("csv_path", metavar="logged_data.csv",
                   help="Path to the logged-decisions CSV file.")
    p.add_argument("--mode", choices=["static", "bandit"], default="static",
                   help="Evaluation mode. 'static' uses a converged policy JSON; "
                        "'bandit' replays the log against a live Syntra server. "
                        "(default: static)")
    p.add_argument("--policy-json", metavar="PATH",
                   help="[static mode] JSON file mapping context_key -> action "
                        "(the converged evaluation policy).")
    p.add_argument("--capsule", metavar="PATH",
                   help="[bandit mode] Path to the capsule YAML to install on Syntra.")
    p.add_argument("--syntra-url", default="http://localhost:8787",
                   help="[bandit mode] Base URL of the Syntra server. "
                        "(default: http://localhost:8787)")
    p.add_argument("--admin-key", default="dev-key",
                   help="[bandit mode] Syntra admin API key. (default: dev-key)")
    p.add_argument("--fallback-action", default=None,
                   help="Action to use for contexts not covered by the eval policy. "
                        "If omitted, uncovered contexts contribute 0 weight to IPS.")
    p.add_argument("--bootstrap", type=int, default=200, metavar="N",
                   help="Number of bootstrap resamples for confidence intervals. "
                        "(default: 200)")
    p.add_argument("--bootstrap-seed", type=int, default=42,
                   help="Random seed for bootstrap resampling. (default: 42)")
    p.add_argument("--format", choices=["json", "text"], default="json",
                   dest="output_format",
                   help="Output format. (default: json)")
    return p.parse_args(argv)


def build_static_policy(args) -> EvalPolicy:
    if not args.policy_json:
        print(
            "ERROR: --mode static requires --policy-json <path>.",
            file=sys.stderr,
        )
        sys.exit(1)
    if not os.path.isfile(args.policy_json):
        print(
            f"ERROR: policy JSON file not found: {args.policy_json}",
            file=sys.stderr,
        )
        sys.exit(1)
    return EvalPolicy.from_json(args.policy_json, fallback_action=args.fallback_action)


def build_bandit_policy(log, args) -> EvalPolicy:
    if not args.capsule:
        print(
            "ERROR: --mode bandit requires --capsule <path>.",
            file=sys.stderr,
        )
        sys.exit(1)
    if not os.path.isfile(args.capsule):
        print(
            f"ERROR: capsule file not found: {args.capsule}",
            file=sys.stderr,
        )
        sys.exit(1)
    print(
        f"Replaying {len(log)} rows against Syntra at {args.syntra_url} ...",
        file=sys.stderr,
    )
    return EvalPolicy.from_syntra_bandit(
        log=log,
        syntra_url=args.syntra_url,
        capsule_yaml_path=args.capsule,
        admin_key=args.admin_key,
    )


def format_text(result) -> str:
    lines = [
        f"Rows evaluated   : {result.n_rows}",
        f"Logging policy   : mean reward = {result.logging_policy_mean_reward:.4f}",
        "",
        "Eval policy estimates:",
    ]
    for name, est in result.eval_policy_estimates.items():
        lines.append(
            f"  {name.upper():4s}  mean={est.mean:.4f}  "
            f"CI [{est.ci_5:.4f}, {est.ci_95:.4f}]"
        )
    if result.warnings:
        lines.append("")
        lines.append("Warnings:")
        for w in result.warnings:
            lines.append(f"  - {w}")
    return "\n".join(lines)


def main(argv=None):
    args = parse_args(argv)

    # Load data
    try:
        log = load_csv(args.csv_path)
    except FileNotFoundError:
        print(f"ERROR: CSV file not found: {args.csv_path}", file=sys.stderr)
        sys.exit(1)
    except Exception as exc:
        print(f"ERROR loading CSV: {exc}", file=sys.stderr)
        sys.exit(1)

    if not log:
        print("ERROR: CSV file contains no data rows.", file=sys.stderr)
        sys.exit(1)

    # Build eval policy
    if args.mode == "static":
        eval_policy = build_static_policy(args)
    else:
        eval_policy = build_bandit_policy(log, args)

    # Run evaluation
    result = evaluate(
        log=log,
        eval_policy=eval_policy,
        n_bootstrap=args.bootstrap,
        bootstrap_seed=args.bootstrap_seed,
    )

    # Output
    if args.output_format == "json":
        print(json.dumps(result.to_dict(), indent=2))
    else:
        print(format_text(result))


if __name__ == "__main__":
    main()
