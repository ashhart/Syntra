#!/usr/bin/env python3
"""Online learning: Syntra and Vowpal Wabbit on learning_bench's environments.

    .venv-bench/bin/python benchmarks/learning_vs_vw.py [--rounds 20000] [--seeds 30] [--tune-seeds 10] [--jobs N]

1. Syntra: `cargo run --release --example learning_bench` (its table, kept
   verbatim) and `benchmarks/learning_seeds`, which compiles the same example
   code and prints one line per seed. The script checks that the per-seed
   means equal learning_bench's table.
2. The Python port of the environments (`envs.py`) running learning_bench's
   uniform policy must equal Syntra's uniform policy bit for bit on every
   seed: same contexts, items, reward draws, sampling and metrics.
3. Vowpal Wabbit on the port, sampling from VW's PMF with Syntra's sampler
   and generator stream:
   - defaults: `--cb_explore_adf --squarecb -q ca` and
     `--cb_explore_adf --epsilon 0.1 -q ca`;
   - matched: the same plus `--ignore_linear c`, Syntra's 5% exploration
     floor applied to VW's PMF, and (epsilon-greedy) unweighted updates, so
     only the regression learner differs;
   - tuned: the learning-rate schedule (`-l`, `--power_t`) and, for
     SquareCB, `--gamma_scale` and its minimum probability (`--epsilon`),
     chosen per environment on held-out seeds (2000 onwards) by mean regret
     per round, then run on the report seeds.
4. Syntra's learning rate tuned the same way on the same seeds.
   learning_bench exposes no other setting, so VW gets more knobs.

Results: benchmarks/results/learning_vs_vw.{json,md}.
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import sys
import time
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import common  # noqa: E402
import envs  # noqa: E402

ENV_NAMES = ["segments", "drift", "catalog"]
ENV_LABELS = {
    "segments": "segments (4 segments x 3 actions)",
    "drift": "drift (best actions rotate halfway)",
    "catalog": "catalog (20 of 200 items, 2-D features)",
}
TUNE_FIRST_SEED = 2000
REPORT_FIRST_SEED = 1000  # learning_bench's seeds: 1000 + s

# Learning-rate schedules a VW user would try: -l around the default with
# AdaGrad decay (power_t 0.5, the default), and constant rates (power_t 0)
# as used for non-stationary problems.
LR_SCHEDULES = [
    "-l 0.1", "-l 0.5", "-l 2",
    "-l 0.01 --power_t 0", "-l 0.03 --power_t 0", "-l 0.1 --power_t 0",
]
GAMMA_SCALES = [1, 10, 100, 1000]
# SquareCB's own minimum probability (`--epsilon e` keeps every action at
# e/K or more), off by default; 0.05 is the value of Syntra's floor.
SQUARECB_MIN_PROB = [None, 0.05]
SYNTRA_LEARNING_RATES = [0.1, 0.25, 0.5, 1.0, 2.0]

BASE = {
    "squarecb": "--cb_explore_adf --squarecb -q ca",
    "epsilon": "--cb_explore_adf --epsilon 0.1 -q ca",
}
SYNTRA_POLICY = {"squarecb": "squarecb", "epsilon": "epsilon-greedy 0.1"}


# ---------------------------------------------------------------- VW policy

def _num(v: float) -> str:
    return repr(float(v))


def context_line(context: dict) -> str:
    """Syntra's flattening in VW text: strings become `name=value`
    indicators, numbers `name:value`; zeros are dropped, as Syntra's
    canonical vectors drop exact zeros."""
    parts = []
    for k, v in context.items():
        if isinstance(v, str):
            parts.append(f"{k}={v}")
        elif v != 0:
            parts.append(f"{k}:{_num(v)}")
    return "shared |c " + " ".join(parts)


def action_line(action: envs.Action) -> str:
    """The action namespace: the `id=<id>` indicator plus its features."""
    parts = [f"id={action.id}"]
    for k, v in action.features.items():
        if v != 0:
            parts.append(f"{k}:{_num(v)}")
    return "|a " + " ".join(parts)


class VWPolicy(envs.Policy):
    """A VW workspace behind the run loop's policy interface. Rewards go in
    as costs (-reward). `floor` mixes Syntra's exploration floor into VW's
    PMF before sampling; `unweighted` logs probability 1, so VW's MTR
    importance weight 1/p becomes a constant."""

    def __init__(self, args: str, floor=None, unweighted: bool = False) -> None:
        import vowpalwabbit

        self.ws = vowpalwabbit.Workspace(args + " --quiet")
        self.floor = floor
        self.unweighted = unweighted
        self.lines = []

    def pmf(self, rnd: envs.Round):
        self.lines = [context_line(rnd.context)] + [action_line(a) for a in rnd.actions]
        pmf = [float(p) for p in self.ws.predict(self.lines)]
        if self.floor:
            envs.apply_floor(pmf, self.floor)
        return pmf

    def learn(self, rnd, chosen, probability, reward) -> None:
        lines = list(self.lines)
        p = 1.0 if self.unweighted else probability
        lines[chosen + 1] = f"0:{_num(-reward)}:{_num(p)} " + lines[chosen + 1]
        self.ws.learn(lines)

    def close(self) -> None:
        self.ws.finish()


def configs_for(family: str):
    """The VW tuning grid of one family; each entry: (key, args)."""
    out = []
    for lr in LR_SCHEDULES:
        if family == "squarecb":
            for g in GAMMA_SCALES:
                for e in SQUARECB_MIN_PROB:
                    extra = f"{lr} --gamma_scale {g}" + (f" --epsilon {e}" if e else "")
                    out.append((f"squarecb {extra}", f"{BASE[family]} {extra}"))
        else:
            out.append((f"epsilon {lr}", f"{BASE[family]} {lr}"))
    return out


def default_configs():
    return {
        "vw squarecb default": {"args": BASE["squarecb"]},
        "vw epsilon default": {"args": BASE["epsilon"]},
        "vw squarecb matched": {"args": BASE["squarecb"] + " --ignore_linear c", "floor": 0.05},
        "vw epsilon matched": {"args": BASE["epsilon"] + " --ignore_linear c", "floor": 0.05,
                               "unweighted": True},
    }


def run_task(task):
    env_name, key, cfg, seed, rounds = task
    t0 = time.perf_counter()
    if cfg is None:
        policy = envs.UniformPolicy()
    else:
        policy = VWPolicy(cfg["args"], cfg.get("floor"), cfg.get("unweighted", False))
    o = envs.run(envs.ENVIRONMENTS[env_name](), policy, rounds, seed)
    return {"env": env_name, "config": key, "seed": seed, "tailShare": o.tail_share,
            "regretPerRound": o.regret_per_round, "seconds": time.perf_counter() - t0}


def run_tasks(tasks, jobs):
    # Longest first (catalog) for load balance.
    tasks = sorted(tasks, key=lambda t: t[0] != "catalog")
    out = []
    with mp.get_context("spawn").Pool(jobs) as pool:
        for i, r in enumerate(pool.imap_unordered(run_task, tasks, chunksize=1), 1):
            out.append(r)
            if i % 50 == 0 or i == len(tasks):
                common.log(f"  {i}/{len(tasks)} runs")
    return out


# ------------------------------------------------------------------- Syntra

def syntra_learning_bench(rounds: int, seeds: int, lr=None) -> str:
    args = ["run", "--release", "--example", "learning_bench", "--",
            "--rounds", str(rounds), "--seeds", str(seeds)]
    if lr is not None:
        args += ["--learning-rate", str(lr)]
    return common.cargo(args)


def syntra_per_seed(rounds: int, seeds: int, first_seed: int, lr=None):
    args = ["run", "--release", "--manifest-path", "benchmarks/learning_seeds/Cargo.toml", "--",
            "--rounds", str(rounds), "--seeds", str(seeds), "--first-seed", str(first_seed)]
    if lr is not None:
        args += ["--learning-rate", str(lr)]
    return [json.loads(line) for line in common.cargo(args).splitlines() if line.strip()]


def parse_learning_bench(text: str):
    """{(env, policy): (share, regret)} from learning_bench's Markdown table."""
    out = {}
    for line in text.splitlines():
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) == 4 and cells[0] in ENV_NAMES:
            out[(cells[0], cells[1])] = (cells[2], cells[3])
    return out


def check_reproduces(per_seed, table_text):
    """The per-seed means, rounded as learning_bench prints them, must equal
    its table."""
    table = parse_learning_bench(table_text)
    groups = defaultdict(list)
    for r in per_seed:
        groups[(r["env"], r["policy"])].append(r)
    mismatches = []
    for key, rows in groups.items():
        share = sum_seq(r["tailShare"] for r in rows) / len(rows)
        regret = sum_seq(r["regretPerRound"] for r in rows) / len(rows)
        got = (f"{share:.3f}", f"{regret:.4f}")
        if table.get(key) != got:
            mismatches.append((key, table.get(key), got))
    return mismatches


def sum_seq(xs):
    total = 0.0
    for x in xs:
        total += x
    return total


# ------------------------------------------------------------------ reports

def summarize(rows):
    shares = [r["tailShare"] for r in rows]
    regrets = [r["regretPerRound"] for r in rows]
    return {"n": len(rows), "share": common.mean(shares), "shareSe": common.se(shares),
            "regret": common.mean(regrets), "regretSe": common.se(regrets)}


def paired(a_rows, b_rows):
    """Mean and 95% t-interval of a - b, paired by seed."""
    from scipy import stats

    a = {r["seed"]: r for r in a_rows}
    b = {r["seed"]: r for r in b_rows}
    seeds = sorted(set(a) & set(b))
    out = {"n": len(seeds)}
    q = stats.t.ppf(0.975, len(seeds) - 1)
    for metric in ("tailShare", "regretPerRound"):
        d = [a[s][metric] - b[s][metric] for s in seeds]
        m, e = common.mean(d), common.se(d)
        out[metric] = {"mean": m, "lower": m - q * e, "upper": m + q * e}
    return out


def verdict(share_ci, regret_ci):
    """Syntra better / worse / no clear difference, from a Syntra - VW
    interval on both metrics (higher share and lower regret are better)."""
    better = share_ci["lower"] > 0 and regret_ci["upper"] < 0
    worse = share_ci["upper"] < 0 and regret_ci["lower"] > 0
    if better:
        return "Syntra better on both"
    if worse:
        return "VW better on both"
    s = "Syntra" if share_ci["lower"] > 0 else "VW" if share_ci["upper"] < 0 else None
    g = "Syntra" if regret_ci["upper"] < 0 else "VW" if regret_ci["lower"] > 0 else None
    if s is None and g is None:
        return "no significant difference"
    parts = []
    if s:
        parts.append(f"{s} higher final share")
    if g:
        parts.append(f"{g} lower regret")
    return "; ".join(parts)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rounds", type=int, default=20_000)
    ap.add_argument("--seeds", type=int, default=30, help="report seeds, 1000 onwards")
    ap.add_argument("--tune-seeds", type=int, default=10, help="held-out tuning seeds, 2000 onwards")
    ap.add_argument("--jobs", type=int, default=common.jobs_default())
    ap.add_argument("--out", default=str(common.RESULTS_DIR / "learning_vs_vw"))
    args = ap.parse_args()
    started = time.time()
    rounds, S, T = args.rounds, args.seeds, args.tune_seeds
    report_seeds = list(range(REPORT_FIRST_SEED, REPORT_FIRST_SEED + S))
    tune_seeds = list(range(TUNE_FIRST_SEED, TUNE_FIRST_SEED + T))
    import vowpalwabbit  # fail early if it is missing

    # 1. Syntra.
    common.log("Syntra: learning_bench and per-seed runs")
    lb_readme = syntra_learning_bench(rounds, 5)
    lb_report = syntra_learning_bench(rounds, S)
    syn = syntra_per_seed(rounds, S, REPORT_FIRST_SEED)
    mismatches = check_reproduces(syn, lb_report)
    if mismatches:
        raise SystemExit(f"per-seed means do not reproduce learning_bench: {mismatches}")
    syn_tune = {lr: syntra_per_seed(rounds, T, TUNE_FIRST_SEED, lr) for lr in SYNTRA_LEARNING_RATES}

    # 2 and 3. The port's uniform policy, VW defaults and matched variants,
    # and the VW tuning grid on held-out seeds.
    fixed = default_configs()
    tasks = [(e, "uniform", None, s, rounds) for e in ENV_NAMES for s in report_seeds]
    tasks += [(e, k, c, s, rounds) for e in ENV_NAMES for k, c in fixed.items() for s in report_seeds]
    grid = {fam: configs_for(fam) for fam in BASE}
    tasks += [(e, key, {"args": a}, s, rounds)
              for e in ENV_NAMES for fam in BASE for key, a in grid[fam] for s in tune_seeds]
    common.log(f"VW and the Python port: {len(tasks)} runs on {args.jobs} processes")
    vw_started = time.time()
    results = run_tasks(tasks, args.jobs)
    by = defaultdict(list)
    for r in results:
        by[(r["env"], r["config"])].append(r)

    # The port must reproduce learning_bench's uniform policy exactly.
    syn_by = defaultdict(list)
    for r in syn:
        syn_by[(r["env"], r["policy"])].append(r)
    port_mismatch = []
    for e in ENV_NAMES:
        rust = {r["seed"]: r for r in syn_by[(e, "uniform")]}
        for r in by[(e, "uniform")]:
            x = rust[r["seed"]]
            if (r["tailShare"], r["regretPerRound"]) != (x["tailShare"], x["regretPerRound"]):
                port_mismatch.append((e, r["seed"]))
    if port_mismatch:
        raise SystemExit(f"the Python port differs from learning_bench's uniform policy: {port_mismatch}")

    # Tuning: the grid point with the lowest mean regret on held-out seeds.
    chosen = {}
    tuning = {}
    for e in ENV_NAMES:
        for fam in BASE:
            scores = []
            for key, a in grid[fam]:
                rows = by[(e, key)]
                scores.append((common.mean([r["regretPerRound"] for r in rows]), key, a))
            scores.sort()
            chosen[(e, fam)] = scores[0]
            tuning[f"{e}/{fam}"] = [{"config": k, "meanRegretTuneSeeds": m} for m, k, _ in scores]
        for fam in BASE:
            pol = SYNTRA_POLICY[fam]
            best = min(
                (common.mean([r["regretPerRound"] for r in syn_tune[lr] if r["env"] == e and r["policy"] == pol]), lr)
                for lr in SYNTRA_LEARNING_RATES)
            chosen[(e, "syntra " + fam)] = best
            tuning[f"{e}/syntra {fam}"] = sorted(
                ({"learningRate": lr, "meanRegretTuneSeeds": common.mean(
                    [r["regretPerRound"] for r in syn_tune[lr] if r["env"] == e and r["policy"] == pol])}
                 for lr in SYNTRA_LEARNING_RATES), key=lambda x: x["meanRegretTuneSeeds"])

    tuned_tasks = []
    for e in ENV_NAMES:
        for fam in BASE:
            _, key, a = chosen[(e, fam)]
            tuned_tasks += [(e, f"vw {fam} tuned: {key}", {"args": a}, s, rounds) for s in report_seeds]
    common.log(f"VW tuned configurations on the report seeds: {len(tuned_tasks)} runs")
    for r in run_tasks(tuned_tasks, args.jobs):
        by[(r["env"], r["config"])].append(r)
    vw_seconds = time.time() - vw_started

    syn_tuned_rows = {}
    syn_tuned_lb = {}
    for lr in sorted({chosen[(e, "syntra " + fam)][1] for e in ENV_NAMES for fam in BASE}):
        rows = syntra_per_seed(rounds, S, REPORT_FIRST_SEED, lr)
        syn_tuned_lb[lr] = syntra_learning_bench(rounds, S, lr)
        if check_reproduces(rows, syn_tuned_lb[lr]):
            raise SystemExit(f"per-seed means do not reproduce learning_bench at learning rate {lr}")
        for r in rows:
            syn_tuned_rows.setdefault((r["env"], r["policy"], lr), []).append(r)

    # ---------------------------------------------------------------- tables
    md = []
    md.append("# Online learning: Syntra and Vowpal Wabbit\n")
    md.append(f"{rounds} rounds, seeds {report_seeds[0]}-{report_seeds[-1]} ({S} seeds) for every row; "
              f"tuning on seeds {tune_seeds[0]}-{tune_seeds[-1]}. Mean ± standard error across seeds. "
              f"Final share: mean expected reward over the last 10% of rounds as a share of the oracle's. "
              f"Regret: mean over all rounds of (best mean - chosen mean).\n")
    summary = {}
    pair_rows = []
    for e in ENV_NAMES:
        rows_md = []

        def add(learner, config, rows, key):
            s = summarize(rows)
            summary[f"{e}/{key}"] = s
            rows_md.append([learner, config, common.pm(s["share"], s["shareSe"], 4),
                            common.pm(s["regret"], s["regretSe"], 5)])

        for fam in BASE:
            pol = SYNTRA_POLICY[fam]
            add("Syntra", f"{'SquareCB' if fam == 'squarecb' else 'epsilon-greedy 0.1'} (default)",
                syn_by[(e, pol)], f"syntra {fam} default")
            lr = chosen[(e, "syntra " + fam)][1]
            add("Syntra", f"same, learning rate tuned: {lr}", syn_tuned_rows[(e, pol, lr)], f"syntra {fam} tuned")
            add("VW", f"`{BASE[fam]}` (defaults)", by[(e, f"vw {fam} default")], f"vw {fam} default")
            _, key, a = chosen[(e, fam)]
            add("VW", f"same, tuned: `{key.split(' ', 1)[1]}`", by[(e, f"vw {fam} tuned: {key}")], f"vw {fam} tuned")
            add("VW", "matched to Syntra (see notes)", by[(e, f"vw {fam} matched")], f"vw {fam} matched")
        add("either", "uniform random", by[(e, "uniform")], "uniform")
        md.append(f"## {ENV_LABELS[e]}\n")
        md.append(common.md_table(["Learner", "Configuration", "Final share of oracle", "Regret per round"],
                                  rows_md, "llrr") + "\n")

        for fam in BASE:
            pol = SYNTRA_POLICY[fam]
            _, key, _ = chosen[(e, fam)]
            lr = chosen[(e, "syntra " + fam)][1]
            for label, a_rows, b_rows in [
                ("defaults", syn_by[(e, pol)], by[(e, f"vw {fam} default")]),
                ("both tuned", syn_tuned_rows[(e, pol, lr)], by[(e, f"vw {fam} tuned: {key}")]),
                ("VW matched", syn_by[(e, pol)], by[(e, f"vw {fam} matched")]),
            ]:
                p = paired(a_rows, b_rows)
                pair_rows.append({"env": e, "family": fam, "comparison": label, **p})

    # Compact tables: one row per configuration, one column per environment.
    compact = [
        ("Syntra SquareCB, defaults", "syntra squarecb default"),
        ("VW `--squarecb`, defaults", "vw squarecb default"),
        ("Syntra SquareCB, learning rate tuned", "syntra squarecb tuned"),
        ("VW `--squarecb`, tuned", "vw squarecb tuned"),
        ("VW `--squarecb`, matched to Syntra", "vw squarecb matched"),
        ("Syntra epsilon-greedy 0.1, defaults", "syntra epsilon default"),
        ("VW `--epsilon 0.1`, defaults", "vw epsilon default"),
        ("Syntra epsilon-greedy 0.1, learning rate tuned", "syntra epsilon tuned"),
        ("VW `--epsilon 0.1`, tuned", "vw epsilon tuned"),
        ("VW `--epsilon 0.1`, matched to Syntra", "vw epsilon matched"),
        ("Uniform random (either)", "uniform"),
    ]
    for title, metric, se_key, digits in [("Final share of the oracle's expected reward", "share", "shareSe", 4),
                                          ("Regret per round", "regret", "regretSe", 5)]:
        md.append(f"## {title}, all environments\n")
        rows_md = [[label] + [common.pm(summary[f"{e}/{key}"][metric], summary[f"{e}/{key}"][se_key], digits)
                              for e in ENV_NAMES] for label, key in compact]
        md.append(common.md_table(["Configuration"] + ENV_NAMES, rows_md, "lrrr") + "\n")
    md.append("Tuned settings (lowest mean regret on the tuning seeds): " + "; ".join(
        [f"{e} SquareCB: VW `{chosen[(e, 'squarecb')][1].split(' ', 1)[1]}`, Syntra {chosen[(e, 'syntra squarecb')][1]}; "
         f"{e} epsilon-greedy: VW `{chosen[(e, 'epsilon')][1].split(' ', 1)[1]}`, "
         f"Syntra {chosen[(e, 'syntra epsilon')][1]}" for e in ENV_NAMES]) + ".\n")
    md.append("## Paired differences, Syntra minus VW\n")
    md.append("Same seeds, so both learners see the same contexts and reward draws; 95% t-intervals over "
              f"{S} seed pairs. Positive share and negative regret favour Syntra.\n")
    rows_md = []
    for p in pair_rows:
        sh, rg = p["tailShare"], p["regretPerRound"]
        rows_md.append([p["env"], "SquareCB" if p["family"] == "squarecb" else "epsilon-greedy",
                        p["comparison"],
                        f"{sh['mean']:+.3f} [{sh['lower']:+.3f}, {sh['upper']:+.3f}]",
                        f"{rg['mean']:+.4f} [{rg['lower']:+.4f}, {rg['upper']:+.4f}]",
                        verdict(sh, rg)])
    md.append(common.md_table(["Environment", "Exploration", "Comparison", "Δ final share", "Δ regret", "Reading"],
                              rows_md, "lllrrl") + "\n")
    md.append("Verdicts from the intervals above, per environment and exploration:\n")
    rows_md = []
    for e in ENV_NAMES:
        for fam in BASE:
            cells = []
            for label in ("defaults", "both tuned", "VW matched"):
                p = next(x for x in pair_rows if x["env"] == e and x["family"] == fam and x["comparison"] == label)
                cells.append(verdict(p["tailShare"], p["regretPerRound"]))
            rows_md.append([e, "SquareCB" if fam == "squarecb" else "epsilon-greedy"] + cells)
    md.append(common.md_table(["Environment", "Exploration", "Defaults", "Both tuned", "VW matched to Syntra"],
                              rows_md) + "\n")
    md.append("## learning_bench output\n")
    md.append(f"`cargo run --release --example learning_bench -- --rounds {rounds} --seeds 5`:\n")
    md.append("```text\n" + lb_readme.strip() + "\n```\n")
    md.append(f"`cargo run --release --example learning_bench -- --rounds {rounds} --seeds {S}`:\n")
    md.append("```text\n" + lb_report.strip() + "\n```\n")
    for lr, text in syn_tuned_lb.items():
        md.append(f"`cargo run --release --example learning_bench -- --rounds {rounds} --seeds {S} "
                  f"--learning-rate {lr}`:\n")
        md.append("```text\n" + text.strip() + "\n```\n")
    elapsed = time.time() - started
    md.append(f"Checks: benchmarks/learning_seeds reproduces every learning_bench table above; the Python "
              f"port's uniform policy equals learning_bench's on all {3 * S} (environment, seed) pairs. "
              f"Wall time {elapsed:.0f} s ({vw_seconds:.0f} s of it VW and the port, {args.jobs} processes); "
              f"{common.hardware()}.\n")

    out = {
        "benchmark": "learning_vs_vw",
        "hardware": common.hardware(),
        "commit": common.git_commit(),
        "versions": {**common.versions("numpy", "scipy"), "vowpalwabbit": vowpalwabbit.__version__},
        "rounds": rounds,
        "reportSeeds": report_seeds,
        "tuneSeeds": tune_seeds,
        "vwConfigs": {**{k: v for k, v in default_configs().items()}, "base": BASE,
                      "lrSchedules": LR_SCHEDULES, "gammaScales": GAMMA_SCALES,
                      "squarecbMinProb": SQUARECB_MIN_PROB},
        "syntraLearningRates": SYNTRA_LEARNING_RATES,
        "tuning": tuning,
        "summary": summary,
        "paired": pair_rows,
        "learningBench": {"seeds5": lb_readme, f"seeds{S}": lb_report,
                          **{f"seeds{S}_lr{lr}": t for lr, t in syn_tuned_lb.items()}},
        "perSeed": {
            "syntra": syn,
            "syntraTuned": [r for rows in syn_tuned_rows.values() for r in rows],
            "vwAndPort": [r for rows in by.values() for r in rows if r["seed"] in report_seeds],
            "vwTuning": [r for rows in by.values() for r in rows if r["seed"] in tune_seeds],
        },
        "wallSeconds": elapsed,
        "vwSeconds": vw_seconds,
        "jobs": args.jobs,
    }
    common.write_json(Path(args.out + ".json"), out, compact=True)
    common.write_text(Path(args.out + ".md"), "\n".join(md))
    print("\n".join(md))


if __name__ == "__main__":
    main()
