"""Helpers shared by the benchmark scripts: paths, building and running
Syntra with cargo, environment metadata, statistics and Markdown tables."""

from __future__ import annotations

import json
import math
import os
import platform
import subprocess
import sys
from pathlib import Path
from typing import Iterable, List, Optional, Sequence

BENCH_DIR = Path(__file__).resolve().parent
REPO = BENCH_DIR.parent
RESULTS_DIR = BENCH_DIR / "results"


def cargo(args: Sequence[str], capture: bool = True) -> str:
    """Run cargo in the repository root (honours CARGO_TARGET_DIR) and
    return its stdout. Build chatter on stderr is shown only on failure."""
    proc = subprocess.run(
        ["cargo", *args], cwd=REPO, text=True,
        stdout=subprocess.PIPE if capture else None, stderr=subprocess.PIPE,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(f"cargo {' '.join(args)} failed with exit code {proc.returncode}")
    return proc.stdout if capture else ""


def target_dir() -> Path:
    """Cargo's target directory for the root crate."""
    meta = json.loads(cargo(["metadata", "--format-version", "1", "--no-deps"]))
    return Path(meta["target_directory"])


def syntra_binary() -> Path:
    """Build `syntra` (release) and return the binary's path."""
    cargo(["build", "--release", "--bin", "syntra"])
    return target_dir() / "release" / "syntra"


def hardware() -> str:
    """CPU model and OS family only, e.g. 'Apple M5 Max (macOS)'."""
    cpu = platform.processor() or platform.machine()
    system = platform.system()
    if system == "Darwin":
        try:
            cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], text=True,
                                 capture_output=True, check=True).stdout.strip()
        except (OSError, subprocess.CalledProcessError):
            pass
        system = "macOS"
    return f"{cpu} ({system}), release build"


def git_commit() -> str:
    try:
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=REPO, text=True,
                              capture_output=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def versions(*modules: str) -> dict:
    out = {"python": platform.python_version()}
    for name in modules:
        try:
            mod = __import__(name)
            out[name] = getattr(mod, "__version__", "unknown")
        except ImportError:
            out[name] = "not installed"
    return out


def jobs_default() -> int:
    return max(1, (os.cpu_count() or 2) - 2)


def mean(xs: Sequence[float]) -> float:
    return math.fsum(xs) / len(xs)


def sd(xs: Sequence[float]) -> float:
    if len(xs) < 2:
        return float("nan")
    m = mean(xs)
    return math.sqrt(math.fsum((x - m) ** 2 for x in xs) / (len(xs) - 1))


def se(xs: Sequence[float]) -> float:
    return sd(xs) / math.sqrt(len(xs))


def md_table(header: Sequence[str], rows: Iterable[Sequence[str]], align: Optional[str] = None) -> str:
    """A Markdown table; `align` is one character per column (l or r)."""
    align = align or "l" * len(header)
    marks = ["---:" if a == "r" else "---" for a in align]
    lines = ["| " + " | ".join(header) + " |", "|" + "|".join(marks) + "|"]
    for row in rows:
        lines.append("| " + " | ".join(row) + " |")
    return "\n".join(lines)


def write_json(path: Path, data, compact: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(data, separators=(",", ":")) if compact else json.dumps(data, indent=1)
    path.write_text(text + "\n")


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def fmt(x: float, digits: int) -> str:
    return "n/a" if x is None or (isinstance(x, float) and math.isnan(x)) else f"{x:.{digits}f}"


def pm(m: float, s: float, digits: int) -> str:
    return f"{fmt(m, digits)} ± {fmt(s, digits)}"


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def relative_to_repo(paths: List[str]) -> List[str]:
    return [os.path.relpath(p, REPO) for p in paths]
