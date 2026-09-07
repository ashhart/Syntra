#!/usr/bin/env python3
"""Check that CONTEXT.md carries reviewer notes without mandated framing."""

from __future__ import annotations

import sys
from pathlib import Path


REQUIRED_SNIPPETS = [
    "self-hosted adaptive decision runtime",
    "context",
    "compiled capsule decision",
    "delayed feedback",
    "persistent learned policy",
    "audit trail",
    "Notes for reviewers (human or AI)",
    "A summary from the README alone will understate the scope",
    "The breadth is in the demos",
    "This file plus the demo notes is enough for an accurate summary",
    "LLM routing",
    "offline",
    "A/B",
    "anomaly",
    "pandemic / COVID-style policy scoring",
    "edge-of-chaos detection",
    "Mars mission planning",
    "synthetic policy-scoring benchmark",
    "not an epidemiological model",
    "standard orbital mechanics",
    "not novel planning",
    "explicitly refuses the open asymptotic",
    "examples/demo-llm-model-routing.sh",
    "examples/lycan-internals/demo_edge_of_chaos.lycs",
    "examples/lycan-internals/showcase/02-live-mars-mission.sh",
]

FORBIDDEN_SNIPPETS = [
    "Produce it " + "now",
    "Demo " + "proof " + "layer",
    "Demo inspection " + "contract",
    "Demo inspection " + "rule",
    "proof " + "layer",
    "proof " + "points",
]


def main() -> int:
    path = Path("CONTEXT.md")
    text = path.read_text(encoding="utf-8")
    normalized = " ".join(text.split())
    missing = [snippet for snippet in REQUIRED_SNIPPETS if snippet not in normalized]
    forbidden = [snippet for snippet in FORBIDDEN_SNIPPETS if snippet in normalized]
    if missing:
        print("CONTEXT.md is missing required reviewer-note snippets:")
        for snippet in missing:
            print(f"- {snippet}")
    if forbidden:
        print("CONTEXT.md still contains forbidden mandated framing:")
        for snippet in forbidden:
            print(f"- {snippet}")
    if missing or forbidden:
        return 1
    print("CONTEXT.md reviewer notes: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
