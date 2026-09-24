#!/usr/bin/env python3
"""Per-decision latency from Python: Syntra's LocalDecider.decide and
Vowpal Wabbit's predict, one thread.

    .venv-bench/bin/python benchmarks/latency_vs_vw.py [--calls 200000] [--warmup 20000]

Builds the `syntra` binary and the Python extension (sdk/python/scripts/
develop.sh), starts `syntra serve` on a fresh store in a temporary directory,
and creates a capsule with the three actions of learning_bench's `segments`
environment (SquareCB, the defaults). Both models first learn from the same
2,000 simulated rounds of that environment (Syntra over HTTP, VW in
process); then each call below is timed on its own with
time.perf_counter_ns, in ten blocks that alternate between the variants, with
the garbage collector off during the timed loops. Contexts cycle through
1,000 pre-generated `segments` contexts.

Variants:
- Syntra `LocalDecider.decide(context_dict)`, background sync every second
  (the default): dict to JSON value, flattening and hashing, predictions for
  three actions, SquareCB PMF with the floor, a seeded draw, the decision id,
  queueing the decision for upload, and the returned Decision object.
- The same with no background thread (sync_interval=None).
- VW `predict(lines)` on `--cb_explore_adf --squarecb -q ca`, with the four
  text lines (shared context and three actions) built in advance: parsing
  the text, predictions and the SquareCB PMF, returned as a list. No action
  is drawn and nothing is logged.
- VW, the same plus drawing the action from the PMF in Python.
- VW, building the text lines from the same context dict first.
- VW `predict` on examples parsed once and reused (no parsing; a lower bound
  that a real request cannot reach, since its context is new).
- An empty call through the same timing loop (the loop and timer overhead).

Results: benchmarks/results/latency_vs_vw.{json,md}.
"""

from __future__ import annotations

import argparse
import gc
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

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import common  # noqa: E402
import envs  # noqa: E402
from learning_vs_vw import action_line, context_line  # noqa: E402

TRAIN_ROUNDS = 2000
N_CONTEXTS = 1000
BLOCKS = 10
VW_ARGS = "--cb_explore_adf --squarecb -q ca"


def build_extension() -> None:
    proc = subprocess.run(["sh", "sdk/python/scripts/develop.sh"], cwd=common.REPO, text=True,
                          capture_output=True)
    if proc.returncode != 0:
        sys.stderr.write(proc.stdout + proc.stderr)
        raise SystemExit("building the Python extension failed")
    sys.path.insert(0, str(common.REPO / "sdk" / "python" / "python"))


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Server:
    def __init__(self, binary: Path) -> None:
        self.store = tempfile.mkdtemp(prefix="syntra-bench-")
        self.key = secrets.token_hex(24)
        env = dict(os.environ, SYNTRA_RATE_LIMIT_RPS="10000000", SYNTRA_RATE_LIMIT_BURST="10000000")
        self.addr = f"127.0.0.1:{free_port()}"
        self.proc = subprocess.Popen(
            [str(binary), "serve", "--addr", self.addr, "--store", self.store, "--admin-key", self.key],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                with urllib.request.urlopen(f"http://{self.addr}/health", timeout=1) as r:
                    if r.status == 200:
                        return
            except OSError:
                time.sleep(0.05)
        self.close()
        raise SystemExit("syntra serve did not come up")

    @property
    def url(self) -> str:
        return f"http://{self.addr}"

    def upload_counts(self) -> dict:
        """Uploaded decisions the server replayed and accepted or rejected,
        from /metrics."""
        req = urllib.request.Request(f"{self.url}/metrics", headers={"Authorization": f"Bearer {self.key}"})
        with urllib.request.urlopen(req, timeout=10) as r:
            text = r.read().decode()
        out = {"accepted": 0, "rejected": 0}
        for line in text.splitlines():
            for k in out:
                if line.startswith(f"syntra_uploaded_decisions_{k}_total"):
                    out[k] += int(float(line.rsplit(" ", 1)[1]))
        return out

    def close(self) -> None:
        self.proc.kill()
        self.proc.wait()
        shutil.rmtree(self.store, ignore_errors=True)


def segment_rounds(n: int, seed: int):
    env = envs.Segments(False)
    rng = envs.SplitMix64(seed)
    out = []
    for t in range(n):
        r = env.round(t, n, rng)
        u = rng.next_f64()
        out.append((r, u))
    return out


def percentile(sorted_ns, p):
    """Nearest-rank percentile of a sorted list."""
    k = max(0, min(len(sorted_ns) - 1, int(round(p / 100.0 * len(sorted_ns) + 0.5)) - 1))
    return sorted_ns[k]


def stats(ns):
    s = sorted(ns)
    return {"calls": len(s), "p50": percentile(s, 50), "p90": percentile(s, 90), "p99": percentile(s, 99),
            "p999": percentile(s, 99.9), "max": s[-1], "mean": sum(s) / len(s)}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--calls", type=int, default=200_000, help="timed calls per variant")
    ap.add_argument("--warmup", type=int, default=20_000, help="untimed calls per variant first")
    ap.add_argument("--out", default=str(common.RESULTS_DIR / "latency_vs_vw"))
    args = ap.parse_args()
    started = time.time()

    common.log("building syntra and the Python extension")
    binary = common.syntra_binary()
    build_extension()
    import syntra
    import vowpalwabbit

    server = Server(binary)
    try:
        spec = {"actions": [{"id": a.id} for a in envs.SEGMENT_ACTIONS]}
        client = syntra.Client(server.url, token=server.key, tenant="bench", job="latency", capsule="segments")
        client.put_spec(spec)
        # Both models learn from the same simulated rounds first.
        train = segment_rounds(TRAIN_ROUNDS, 99)
        for r, u in train:
            d = client.decide(r.context)
            idx = [a.id for a in r.actions].index(d["action"])
            client.reward(d["decisionId"], 1.0 if u < r.means[idx] else 0.0)
        vw = vowpalwabbit.Workspace(VW_ARGS + " --quiet")
        draw = envs.SplitMix64(7)
        for r, u in train:
            lines = [context_line(r.context)] + [action_line(a) for a in r.actions]
            pmf = [float(p) for p in vw.predict(lines)]
            i = envs.sample(pmf, draw)
            lines[i + 1] = f"0:{-1.0 if u < r.means[i] else 0.0}:{pmf[i]!r} " + lines[i + 1]
            vw.learn(lines)
        client.close()

        max_queue = 4 * (args.calls + args.warmup) + 10
        decider = syntra.LocalDecider(server.url, token=server.key, tenant="bench", job="latency",
                                      capsule="segments", max_queue=max_queue)
        quiet = syntra.LocalDecider(server.url, token=server.key, tenant="bench", job="latency",
                                    capsule="segments", sync_interval=None, max_queue=max_queue)
        model_version = decider.model_version

        pool = [r for r, _ in segment_rounds(N_CONTEXTS, 1234)]
        contexts = [r.context for r in pool]
        texts = [[context_line(r.context)] + [action_line(a) for a in r.actions] for r in pool]
        parsed = [vw.parse(t) for t in texts]
        actions = envs.SEGMENT_ACTIONS
        rnd = random.Random(5)

        def vw_sample(lines):
            pmf = vw.predict(lines)
            u = rnd.random()
            c = 0.0
            for i, p in enumerate(pmf):
                c += p
                if u < c:
                    return i
            return len(pmf) - 1

        def vw_from_dict(ctx):
            return vw.predict([context_line(ctx)] + [action_line(a) for a in actions])

        variants = {
            "syntra": ("Syntra `LocalDecider.decide(dict)`, background sync every 1 s (default)",
                       decider.decide, contexts),
            "syntra_nosync": ("Syntra `LocalDecider.decide(dict)`, no background thread",
                              quiet.decide, contexts),
            "vw_text": ("VW `predict(lines)`, text built in advance", vw.predict, texts),
            "vw_text_sample": ("VW `predict(lines)` + drawing the action in Python", vw_sample, texts),
            "vw_dict": ("VW: build the text from the dict, then `predict`", vw_from_dict, contexts),
            "vw_parsed": ("VW `predict` on a pre-parsed example (no parsing; lower bound)", vw.predict, parsed),
            "empty": ("Empty call (timing-loop overhead)", lambda x: None, contexts),
        }
        timings = {k: [] for k in variants}
        per_block = args.calls // BLOCKS
        warm = args.warmup
        for k, (_, f, inputs) in variants.items():
            for i in range(warm):
                f(inputs[i % N_CONTEXTS])
        clock = time.perf_counter_ns
        load_before = os.getloadavg()
        common.log(f"timing {per_block * BLOCKS} calls per variant in {BLOCKS} alternating blocks")
        for b in range(BLOCKS):
            order = list(variants)
            if b % 2:
                order.reverse()
            for k in order:
                _, f, inputs = variants[k]
                out = timings[k]
                gc.disable()
                try:
                    for i in range(per_block):
                        x = inputs[i % N_CONTEXTS]
                        t0 = clock()
                        f(x)
                        t1 = clock()
                        out.append(t1 - t0)
                finally:
                    gc.enable()
        load_after = os.getloadavg()
        decider.close()
        quiet.close()
        uploads = server.upload_counts()
        for p in parsed:
            vw.finish_example(p)
        vw.finish()
    finally:
        server.close()

    res = {k: {"label": variants[k][0], **stats(v)} for k, v in timings.items()}
    rows = []
    for k, r in res.items():
        rows.append([r["label"], f"{r['p50'] / 1000:.2f}", f"{r['p90'] / 1000:.2f}", f"{r['p99'] / 1000:.2f}",
                     f"{r['p999'] / 1000:.2f}", f"{r['mean'] / 1000:.2f}"])
    md = ["# Decision latency from Python: Syntra and Vowpal Wabbit\n",
          f"{per_block * BLOCKS:,} timed calls per variant after {warm:,} warm-up calls, one thread, "
          f"microseconds per call (time.perf_counter_ns around each call; includes the loop and timer, "
          f"see the last row). Both models learned from the same {TRAIN_ROUNDS:,} rounds of the `segments` "
          f"environment first (Syntra model version {model_version}). {common.hardware()}.\n",
          common.md_table(["Call", "p50", "p90", "p99", "p99.9", "mean"], rows, "lrrrrr") + "\n",
          f"Every local decision was uploaded and replayed by the server: "
          f"{uploads['accepted']:,} accepted and {uploads['rejected']:,} rejected of "
          f"{2 * (per_block * BLOCKS + warm):,} made (both deciders, warm-up included).\n"]
    elapsed = time.time() - started
    md.append(f"System load average (1, 5, 15 min) before the timed loops "
              f"{', '.join(f'{x:.1f}' for x in load_before)}, after {', '.join(f'{x:.1f}' for x in load_after)} "
              f"({os.cpu_count()} logical CPUs). Wall time {elapsed:.0f} s.\n")
    out = {
        "benchmark": "latency_vs_vw",
        "hardware": common.hardware(),
        "commit": common.git_commit(),
        "versions": {**common.versions(), "vowpalwabbit": vowpalwabbit.__version__,
                     "syntra-python": getattr(syntra, "__version__", "unknown")},
        "vwArgs": VW_ARGS,
        "trainRounds": TRAIN_ROUNDS,
        "callsPerVariant": per_block * BLOCKS,
        "warmupPerVariant": warm,
        "blocks": BLOCKS,
        "results": res,
        "uploads": {**uploads, "made": 2 * (per_block * BLOCKS + warm)},
        "loadAverage": {"before": load_before, "after": load_after, "cpus": os.cpu_count()},
        "wallSeconds": elapsed,
    }
    common.write_json(Path(args.out + ".json"), out)
    common.write_text(Path(args.out + ".md"), "\n".join(md))
    print("\n".join(md))


if __name__ == "__main__":
    main()
