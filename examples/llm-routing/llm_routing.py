#!/usr/bin/env python3
"""LLM routing with syntra.llm.ModelRouter, on SIMULATED models.

Each request goes to one of three model routes: small, medium or large.
No real model is called. The routes, their answer quality per task, their
prices and latencies, and the traffic are all made up (see ROUTES and
TASKS below). What is real is Syntra: a v2 server this script starts on a
fresh store, decisions made in-process by LocalDecider, the server's replay
check of every uploaded decision, learning from the rewards, and the
off-policy evaluation at the end (`syntra evaluate --store`).

    cargo build --release                 # target/release/syntra
    sdk/python/scripts/develop.sh         # the Python extension
    python3 examples/llm-routing/llm_routing.py

Options: --requests N (default 4000), --seed S (default 7), --keep (keep
the store and print the commands to evaluate it yourself). SYNTRA_BIN
picks the syntra binary (default target/release/syntra).
"""

from __future__ import annotations

import argparse
import heapq
import json
import os
import random
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
TENANT, JOB, CAPSULE = "acme", "llm", "router"

try:
    import syntra  # noqa: F401  (installed wheel)
except ImportError:
    sys.path.insert(0, str(ROOT / "sdk" / "python" / "python"))
try:
    from syntra import Client, LocalDecider
    from syntra.llm import ModelRouter
except ImportError as e:
    sys.exit(f"cannot import the syntra SDK ({e}); build it with sdk/python/scripts/develop.sh")

# --- The simulation (made up, not measurements of any real model) --------

# Price per million input/output tokens, latency (a base plus a cost per
# thousand tokens), and mean answer quality (0-1) for each kind of task.
ROUTES = {
    "small": {"inUsd": 0.15, "outUsd": 0.60, "baseS": 0.35, "sPer1k": 0.02,
              "quality": {"chat": 0.84, "code": 0.35, "summarize": 0.55}},
    "medium": {"inUsd": 1.00, "outUsd": 4.00, "baseS": 0.80, "sPer1k": 0.08,
               "quality": {"chat": 0.82, "code": 0.62, "summarize": 0.80}},
    "large": {"inUsd": 5.00, "outUsd": 20.00, "baseS": 1.60, "sPer1k": 0.25,
              "quality": {"chat": 0.85, "code": 0.90, "summarize": 0.84}},
}
# Share of traffic, prompt length range (tokens) and answer length.
TASKS = {
    "chat": (0.5, (50, 600), 200),
    "code": (0.3, (300, 2500), 400),
    "summarize": (0.2, (2000, 8000), 250),
}
QUALITY_NOISE = 0.08  # standard deviation of one graded answer

# What the application trades: a dollar is worth 5 quality points and a
# second of latency 0.04 (per simulated second, see TIME_SCALE).
COST_WEIGHT = 5.0
LATENCY_WEIGHT = 0.04
# A simulated call takes a thousandth of its simulated latency, so the
# router measures real (scaled) latency and the run takes seconds.
TIME_SCALE = 0.001


class SimulatedResponse:
    """Stands in for what a completion function returns."""

    def __init__(self, model: str, task: str, prompt_tokens: int, output_tokens: int):
        self.model = model
        self.task = task
        self.prompt_tokens = prompt_tokens
        self.output_tokens = output_tokens


def latency_s(route: str, prompt_tokens: int, output_tokens: int) -> float:
    r = ROUTES[route]
    return r["baseS"] + r["sPer1k"] * (prompt_tokens + output_tokens) / 1000


def cost_usd(route: str, prompt_tokens: int, output_tokens: int) -> float:
    r = ROUTES[route]
    return (prompt_tokens * r["inUsd"] + output_tokens * r["outUsd"]) / 1e6


def simulated_completion(model, messages, sim_task, **kwargs):
    """A completion function with the signature ModelRouter calls. It
    burns the (scaled) latency of the route and returns token counts."""
    prompt_tokens = round(sum(len(m["content"]) for m in messages) / 4)
    output_tokens = TASKS[sim_task][2]
    end = time.perf_counter() + latency_s(model, prompt_tokens, output_tokens) * TIME_SCALE
    while time.perf_counter() < end:  # spin: sleep() is too coarse at this scale
        pass
    return SimulatedResponse(model, sim_task, prompt_tokens, output_tokens)


def simulated_cost(response: SimulatedResponse) -> float:
    return cost_usd(response.model, response.prompt_tokens, response.output_tokens)


def simulated_grade(response: SimulatedResponse, rng: random.Random) -> float:
    """A grader's score for one answer: the route's mean quality plus noise."""
    q = ROUTES[response.model]["quality"][response.task] + rng.gauss(0, QUALITY_NOISE)
    return min(1.0, max(0.0, q))


def expected_reward(route: str, task: str, prompt_tokens: int) -> float:
    """The reward a route earns on average for this request, known here
    only because the models are simulated."""
    out = TASKS[task][2]
    return (ROUTES[route]["quality"][task]
            - COST_WEIGHT * cost_usd(route, prompt_tokens, out)
            - LATENCY_WEIGHT * latency_s(route, prompt_tokens, out))


def next_request(rng: random.Random) -> tuple[str, list]:
    task = rng.choices(list(TASKS), weights=[t[0] for t in TASKS.values()])[0]
    lo, hi = TASKS[task][1]
    tokens = rng.randint(lo, hi)
    opening = {"chat": "Hi! ", "code": "def slow(x):\n", "summarize": "Summarize: "}[task]
    content = opening + "x" * max(0, tokens * 4 - len(opening))
    return task, [{"role": "user", "content": content}]


# --- A private server -----------------------------------------------------

def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start_server(binary: str, store: str, key: str) -> tuple[subprocess.Popen, str]:
    port = free_port()
    env = dict(os.environ, SYNTRA_ADMIN_KEY=key)
    proc = subprocess.Popen(
        [binary, "serve", "--addr", f"127.0.0.1:{port}", "--store", store],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}"
    for _ in range(100):
        try:
            with urllib.request.urlopen(url + "/health", timeout=1):
                return proc, url
        except OSError:
            time.sleep(0.05)
    proc.kill()
    sys.exit("the syntra server did not start")


def wait_for_new_model(decider: LocalDecider, timeout: float = 3.0) -> None:
    """The server publishes a learned model at most once a second."""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if decider.sync():
            return
        time.sleep(0.1)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--requests", type=int, default=4000)
    ap.add_argument("--rounds", type=int, default=10)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--keep", action="store_true", help="keep the store and print its path")
    args = ap.parse_args()

    binary = os.environ.get("SYNTRA_BIN", str(ROOT / "target" / "release" / "syntra"))
    if not os.access(binary, os.X_OK):
        sys.exit(f"no syntra binary at {binary}; run `cargo build --release` or set SYNTRA_BIN")

    rng = random.Random(args.seed)
    grader_rng = random.Random(args.seed + 1)
    base = tempfile.mkdtemp(prefix="syntra-llm-routing.")
    store = os.path.join(base, "store")
    key = secrets.token_hex(24)
    server, url = start_server(binary, store, key)
    capsule = f"{TENANT}/{JOB}/{CAPSULE}"
    print("LLM routing on SIMULATED models (made-up quality, price and latency).")
    print(f"Syntra server {url}, store {store}\n")
    try:
        # The router sends the model list with every decision, so the spec
        # declares no actions. Rewards can go below 0 (cost and latency are
        # subtracted from quality), hence the range. At the default learning
        # rate (0.5) the learned choice swings between routes on rewards this
        # noisy; 0.1 holds steady. The seed makes the run repeatable.
        with Client(url, token=key, tenant=TENANT, job=JOB, capsule=CAPSULE) as admin:
            admin.put_spec({"actions": [], "reward": {"range": [-0.5, 1.0]},
                            "learner": {"learningRate": 0.1}, "seed": args.seed})

        decider = LocalDecider(url, token=key, tenant=TENANT, job=JOB, capsule=CAPSULE,
                               sync_interval=None)  # flush and sync by hand, per round
        router = ModelRouter(
            decider,
            models={name: {"tier": name, "inUsdPer1M": r["inUsd"], "outUsdPer1M": r["outUsd"]}
                    for name, r in ROUTES.items()},
            completion=simulated_completion,
            cost=simulated_cost,
            cost_weight=COST_WEIGHT,
            latency_weight=LATENCY_WEIGHT / TIME_SCALE,  # per measured second
        )

        best = {"chat": "small", "code": "large", "summarize": "medium"}
        print("Best route per task in this simulation: "
              + ", ".join(f"{t} -> {r}" for t, r in best.items()) + "\n")
        print(f"{'round':>5} {'requests':>8} {'model':>6}  {'expected reward':>15}  "
              f"{'routed to the best route for the task':<40}")
        grading: list = []  # (due at request n, decision id, quality): a grader that answers later
        rows = []  # (task, prompt tokens, chosen route) for the summary
        uploads = {"decisions_accepted": 0, "decisions_rejected": 0,
                   "rewards_applied": 0, "rewards_failed": 0}

        def count(report: dict) -> None:
            for k in uploads:
                uploads[k] += report[k]
        per_round = args.requests // args.rounds
        n = 0
        for rnd in range(1, args.rounds + 1):
            reward_sum, hits, seen = 0.0, {t: 0 for t in TASKS}, {t: 0 for t in TASKS}
            version = decider.model_version
            for _ in range(per_round):
                task, messages = next_request(rng)
                result = router.completion(messages, context={"task": task}, sim_task=task)
                resp = result.response
                # The answer is graded later (0 to 200 requests on).
                due = n + rng.randint(0, 200)
                heapq.heappush(grading, (due, result.decision_id, simulated_grade(resp, grader_rng)))
                while grading and grading[0][0] <= n:
                    _, decision_id, quality = heapq.heappop(grading)
                    router.report_quality(decision_id, quality)
                rows.append((task, resp.prompt_tokens, result.model))
                reward_sum += expected_reward(result.model, task, resp.prompt_tokens)
                seen[task] += 1
                hits[task] += result.model == best[task]
                n += 1
            share = "  ".join(f"{t} {100 * hits[t] / max(1, seen[t]):3.0f}%" for t in TASKS)
            print(f"{rnd:>5} {n:>8} {version:>6}  {reward_sum / per_round:>15.3f}  {share}")
            count(decider.flush())  # upload decisions, then rewards; the server learns
            wait_for_new_model(decider)

        for _, decision_id, quality in sorted(grading):
            router.report_quality(decision_id, quality)
        count(decider.close())
        print("\nUploaded and verified by replay on the server: "
              + ", ".join(f"{k.replace('_', ' ')} {v}" for k, v in uploads.items()))

        def mean(f):
            return sum(f(t, p) for t, p, _ in rows) / len(rows)

        last = rows[-per_round:]
        print("\nExpected reward per request on the same traffic (from the simulation's")
        print("known profiles; the learner never sees these):")
        print(f"  Syntra, last round        {sum(expected_reward(r, t, p) for t, p, r in last) / len(last):.3f}")
        print(f"  Syntra, whole run         {sum(expected_reward(r, t, p) for t, p, r in rows) / len(rows):.3f}")
        for route in ROUTES:
            print(f"  always {route:<18} {mean(lambda t, p, r=route: expected_reward(r, t, p)):.3f}")
        print(f"  uniform random            {mean(lambda t, p: sum(expected_reward(r, t, p) for r in ROUTES) / 3):.3f}")
        print(f"  best route per request    {mean(lambda t, p: max(expected_reward(r, t, p) for r in ROUTES)):.3f}")

        # Off-policy evaluation from the store, read-only, while the server
        # is still running: what would each policy have earned on the
        # logged traffic?
        truth = {"logged": sum(expected_reward(r, t, p) for t, p, r in rows) / len(rows)}
        for route in ROUTES:
            truth[f"constant:{route}"] = mean(lambda t, p, r=route: expected_reward(r, t, p))
        print("\nOff-policy evaluation of the logged decisions (syntra evaluate --store),")
        print("next to the simulated truth where the script can compute it:")
        print(f"{'policy':<16} {'DR estimate':>11} {'95% interval':>16}  {'lift over logged (DR)':>26}  {'truth':>6}")
        for policy in ["logged", "greedy", "constant:small", "constant:medium", "constant:large"]:
            out = subprocess.run(
                [binary, "evaluate", "--store", store, "--capsule", capsule,
                 "--policy", policy, "--bootstrap", "500"],
                capture_output=True, text=True, check=True)
            rep = json.loads(out.stdout)
            dr, lift = rep["estimators"]["dr"], rep["lift"]["dr"]
            interval = "[%.3f, %.3f]" % (dr["lower"], dr["upper"])
            known = "%.3f" % truth[policy] if policy in truth else "-"
            print(f"{policy:<16} {dr['estimate']:>11.3f} {interval:>16}  "
                  f"{lift['mean']:>+7.3f} [{lift['lower']:+.3f}, {lift['upper']:+.3f}]  {known:>6}")

        # A gated question with a clear answer: would sending everything to
        # the large model have beaten the learned routing? The gates (in
        # gates.yaml) ask for a positive lift at 95% confidence and enough
        # effective samples; with --fail-on-gate a failed gate exits 1.
        gates = HERE / "gates.yaml"
        cmd = [binary, "evaluate", "--store", store, "--capsule", capsule,
               "--policy", "constant:large", "--gates", str(gates), "--fail-on-gate",
               "--format", "markdown"]
        print("\n$ syntra evaluate --store <store> --capsule " + capsule
              + f" --policy constant:large --gates {gates.relative_to(ROOT)}"
              + " --fail-on-gate --format markdown")
        out = subprocess.run(cmd, capture_output=True, text=True)
        print(out.stdout.rstrip())
        if out.returncode == 2:
            print(out.stderr.rstrip(), file=sys.stderr)
            return 2
        print(f"(exit code {out.returncode}: 0 when every gate passes, 1 when one fails)")
        if args.keep:
            print(f"\nStore kept at {store}. Evaluate it yourself with:\n"
                  f"  syntra evaluate --store {store} --capsule {capsule} --policy greedy")
        return 0
    finally:
        server.terminate()
        server.wait(timeout=30)
        if not args.keep:
            shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
