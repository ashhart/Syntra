#!/usr/bin/env python3
"""Run the science demos and check computed results, saving reproducible receipts."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]


def number(text, pattern):
    match = re.search(pattern, text, re.MULTILINE)
    if match is None:
        raise ValueError(f"missing result: {pattern}")
    value = float(match.group(1))
    if not math.isfinite(value):
        raise ValueError("non-finite result")
    return value


def check_pandemic(text):
    # Independent arithmetic oracle for the documented synthetic scoring rules.
    expected = []
    for policy in range(5):
        total, failures = 0.0, 0
        for case in range(120):
            r = 0.85 + case % 8 * 0.08
            h = 0.25 + (case // 8) % 6 * 0.12
            tests = 0.95 - (case // 48) % 5 * 0.10
            compliance = 0.88 - case % 5 * 0.07
            control = [0.25-r*0.10, 0.82*compliance, 0.48,
                       0.58+tests*0.17+compliance*0.14, 0.42][policy]
            overload = [0.30-h*0.40, 0.72, 0.62-h*0.08,
                        0.86-h*0.08, 0.76-h*0.05][policy]
            cost = [0.08, 0.82, 0.31, 0.34, 0.44][policy]
            failures += overload < 0.55
            total += control*450 + overload*470 - cost*260 - (overload < 0.55)*650
        expected.append(total/120)
        score = number(text, rf"^{policy} \S+ score (\S+) failures")
        actual_failures = number(text, rf"^{policy} \S+ score \S+ failures (\d+)")
        if not math.isclose(score, expected[-1], abs_tol=1e-8) or actual_failures != failures:
            raise ValueError(f"policy {policy}: score or failure count disagrees with oracle")
    winner = int(number(text, r"Best pandemic policy: (\d+)"))
    if winner != max(range(5), key=expected.__getitem__):
        raise ValueError("selected best policy is not the highest-scoring policy")
    return {"cases": 120, "winning_policy": winner, "scores": expected,
            "scope": "Synthetic policy scoring, not an epidemiological prediction."}


def check_mars(text):
    c3 = number(text, r"^\s+C3:\s+(\S+) / 100") / 100
    limit = number(text, r"max C3: (\S+)")
    tof = number(text, r"^\s+TOF:\s+(\S+) days")
    bounds = re.search(r"TOF range: (\S+) - (\S+) days", text)
    if bounds is None:
        raise ValueError("missing time-of-flight bounds")
    if not (0 < c3 <= limit and float(bounds[1]) <= tof <= float(bounds[2])):
        raise ValueError("Mars result violates energy or time-of-flight constraints")
    return {"departure_c3_km2_s2": c3, "maximum_c3": limit, "flight_days": tof,
            "scope": "Lambert search over bundled ephemerides, not flight certification."}


def check_chaos(text):
    error = number(text, r"Error \(Feigenbaum\):?\s+(\S+)")
    if not 0 <= error < 0.005:
        raise ValueError(f"edge-of-chaos numerical error out of tolerance: {error}")
    return {"absolute_boundary_error": error, "maximum_error": 0.005}


def check_approximate_window(text):
    c3 = number(text, r"Departure C3:\s+(\S+) / 100") / 100
    tof = number(text, r"TOF:\s+(\S+) days")
    day = number(text, r"Days from Jan 1:\s+(\S+)")
    if not (0 < c3 < 100 and 0 < tof < 1000 and 0 <= day < 731):
        raise ValueError("approximate launch-window search returned an invalid result")
    return {"departure_c3_km2_s2": c3, "flight_days": tof, "departure_day": day,
            "scope": "Simplified Keplerian approximation; use the Lambert demo for constraints."}


CASES = [
    ("mars-lambert", "demo_mars_decide", check_mars),
    ("launch-window", "demo_mars_transfer", check_approximate_window),
    ("pandemic", "demo_pandemic_policy", check_pandemic),
    ("edge-of-chaos", "demo_edge_of_chaos", check_chaos),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true", help="use an already-built release binary")
    parser.add_argument("--out", type=Path, default=ROOT / "target/science-demo")
    args = parser.parse_args()
    if not args.no_build:
        subprocess.run(["cargo", "build", "--locked", "--release"], cwd=ROOT, check=True)
    args.out.mkdir(parents=True, exist_ok=True)
    results = []
    for label, stem, validate in CASES:
        source = ROOT / "examples/lycan-internals" / (stem + ".lycs")
        print(f"\n=== {label} ===", flush=True)
        started = time.monotonic()
        row = {"demo": label, "source": str(source.relative_to(ROOT)),
               "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest()}
        try:
            run = subprocess.run([str(ROOT / "target/release/lycan"), str(source)],
                                 cwd=ROOT, capture_output=True, text=True, timeout=120)
            output = run.stdout + run.stderr
            (args.out / (label + ".log")).write_text(output)
            print(output, end="" if output.endswith("\n") else "\n")
            if run.returncode:
                raise ValueError(f"runtime exited {run.returncode}")
            row.update(status="pass", checks=validate(output),
                       output_sha256=hashlib.sha256(output.encode()).hexdigest())
        except (ValueError, OSError, subprocess.TimeoutExpired) as error:
            row.update(status="fail", error=str(error))
        row["seconds"] = round(time.monotonic() - started, 3)
        results.append(row)
        print(f"{row['status'].upper()}: {label} ({row['seconds']}s)", flush=True)
    report = args.out / "results.json"
    report.write_text(json.dumps(results, indent=2) + "\n")
    print(f"\nReceipts: {report}")
    return int(any(row["status"] != "pass" for row in results))


if __name__ == "__main__":
    sys.exit(main())
