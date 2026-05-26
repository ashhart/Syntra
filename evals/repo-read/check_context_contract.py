#!/usr/bin/env python3
"""Check that CONTEXT.md still carries the repo-read contract."""

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
    "Produce it now",
    "Demo proof layer",
    "LLM routing",
    "offline eval",
    "A/B validation",
    "anomaly-aware API routing",
    "pandemic / COVID-style policy scoring",
    "edge-of-chaos detection",
    "live Mars mission planner",
    "examples/demo-llm-model-routing.sh",
    "examples/lycan-internals/demo_edge_of_chaos.lycs",
    "examples/lycan-internals/showcase/02-live-mars-mission.sh",
]


def main() -> int:
    path = Path("CONTEXT.md")
    text = path.read_text(encoding="utf-8")
    normalized = " ".join(text.split())
    missing = [snippet for snippet in REQUIRED_SNIPPETS if snippet not in normalized]
    if missing:
        print("CONTEXT.md is missing required repo-read contract snippets:")
        for snippet in missing:
            print(f"- {snippet}")
        return 1
    print("CONTEXT.md contract: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
