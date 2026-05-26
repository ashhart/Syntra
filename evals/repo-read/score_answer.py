#!/usr/bin/env python3
"""Score a model answer against the Syntra repo-read contract."""

from __future__ import annotations

import re
import sys
from pathlib import Path


CHECKS: list[tuple[str, str]] = [
    ("identity", r"self[- ]hosted adaptive decision runtime"),
    ("primitive context", r"context\s*(?:->|→)\s*compiled capsule decision"),
    ("primitive feedback", r"delayed feedback\s*(?:->|→)\s*persistent learned policy"),
    ("demo heading", r"demo proof layer"),
    ("llm routing", r"llm (?:model )?routing"),
    ("offline eval", r"offline eval(?:uation)?"),
    ("a/b validation", r"(?:a/b|ab) (?:validation|harness|testing|test)"),
    ("anomaly routing", r"anomaly[- ]aware (?:api )?routing"),
    ("pandemic policy", r"pandemic|covid"),
    ("edge of chaos", r"edge[- ]of[- ]chaos"),
    ("mars mission", r"mars mission|live mars|horizons.*lambert"),
    ("adoption demos", r"product/adoption|adoption demos|commercial wedge"),
    ("substrate demos", r"substrate demos|lycan substrate|substrate breadth"),
    ("read status", r"files? (?:i )?read|read status|fetch status|demo inspection status"),
]

MISREADINGS: list[tuple[str, str]] = [
    ("only api gateway", r"\b(?:just|only|merely) an? api gateway\b"),
    ("only feature flags", r"\b(?:just|only|merely) (?:a )?feature[- ]flag"),
    ("only bandit wrapper", r"\b(?:just|only|merely) (?:a )?(?:contextual[- ])?bandit wrapper\b"),
]


def read_answer() -> str:
    if len(sys.argv) > 2:
        print("usage: score_answer.py [answer-file]", file=sys.stderr)
        sys.exit(2)
    if len(sys.argv) == 2:
        return Path(sys.argv[1]).read_text(encoding="utf-8")
    return sys.stdin.read()


def main() -> int:
    answer = read_answer()
    haystack = answer.lower()

    missing = [name for name, pattern in CHECKS if not re.search(pattern, haystack)]
    bad = [name for name, pattern in MISREADINGS if re.search(pattern, haystack)]

    if missing:
        print("missing required signals:")
        for name in missing:
            print(f"- {name}")
    if bad:
        print("contains forbidden misreadings:")
        for name in bad:
            print(f"- {name}")

    if missing or bad:
        return 1

    print("repo-read answer: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
