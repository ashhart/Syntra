#!/usr/bin/env python3
"""Off-policy evaluation accuracy: `syntra evaluate` and Open Bandit Pipeline.

    .venv-bench/bin/python benchmarks/ope_vs_obp.py [--repeats 200] [--sizes 1000,10000,100000] [--jobs N]

Logged data come from OBP's SyntheticBanditDataset (10 actions, 5-dimensional
Gaussian contexts, Bernoulli rewards). The expected reward is OBP's
`logistic_reward_function` (coefficients from seed 12345, the dataset's
default) with its logits standardized by fixed constants, so q(x, a) is one
function for every sample and the true value of a policy is well defined.
Each repeat draws a fresh dataset (its own random_state) and evaluates the
same target policy with:

- Syntra: `syntra evaluate --policy target-column --w-max inf --bootstrap
  1000` on the rows as JSONL: DM, IPS, SNIPS and cross-fitted DR (5 folds,
  its own linear reward model), each with a 95% percentile-bootstrap
  interval;
- OBP 0.5.7: IPW, SNIPW, and DM/DR with a RegressionModel cross-fitted over 5
  folds, with two base models: LogisticRegression(C=100) on [context,
  one-hot action] (OBP's example setup; additive, so it cannot represent the
  context-action interaction in the true reward) and the same
  LogisticRegression on pairwise interaction features (which can); 95%
  bootstrap intervals from OBP's estimate_interval (1000 resamples) for IPW,
  SNIPW and both DRs.

Truth: the target's value over the context distribution, from 4 million
contexts. Reported: mean relative error |estimate - truth| / truth per
estimator and sample size, the share of 95% intervals that contain the truth,
and how closely Syntra's IPS and SNIPS agree with OBP's on identical data.

Results: benchmarks/results/ope_vs_obp.{json,md}.
"""

from __future__ import annotations

import os

# One BLAS thread per process: the repeats run in parallel.
for _v in ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "VECLIB_MAXIMUM_THREADS"):
    os.environ.setdefault(_v, "1")

import argparse  # noqa: E402
import json  # noqa: E402
import logging  # noqa: E402
import multiprocessing as mp  # noqa: E402
import subprocess  # noqa: E402
import sys  # noqa: E402
import tempfile  # noqa: E402
import time  # noqa: E402
import warnings  # noqa: E402
from collections import defaultdict  # noqa: E402
from pathlib import Path  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))

import common  # noqa: E402

N_ACTIONS = 10
DIM_CONTEXT = 5
ENV_SEED = 12345  # reward-function coefficients: SyntheticBanditDataset's default random_state
EPSILON_TARGET = 0.1
TRUTH_CONTEXTS = 4_000_000
N_BOOTSTRAP = 1000
FOLDS = 5

# Logging policies: SyntheticBanditDataset's softmax(beta * q(x, .)).
PAIRS = {
    "uniform": {"beta": 0.0, "label": "uniform logging (beta = 0)"},
    "aligned": {"beta": 3.0, "label": "logging softmax(3q), leaning towards the target"},
    "opposed": {"beta": -3.0, "label": "logging softmax(-3q), leaning away from the target"},
}

SYNTRA_ESTIMATORS = ["dm", "ips", "snips", "dr"]
OBP_ESTIMATORS = ["ipw", "snipw", "dm_lr", "dr_lr", "dm_lrx", "dr_lrx"]
LABELS = {
    "syntra/dm": "Syntra DM", "syntra/ips": "Syntra IPS", "syntra/snips": "Syntra SNIPS",
    "syntra/dr": "Syntra DR",
    "obp/ipw": "OBP IPW", "obp/snipw": "OBP SNIPW",
    "obp/dm_lr": "OBP DM (LR, additive)", "obp/dr_lr": "OBP DR (LR, additive)",
    "obp/dm_lrx": "OBP DM (LR, interactions)", "obp/dr_lrx": "OBP DR (LR, interactions)",
}
OBP_INTERVALS = ["ipw", "snipw", "dr_lr", "dr_lrx"]

_STANDARDIZE = {}


def raw_logits(context):
    """OBP's logistic reward logits (z_score=False), recovered from its
    sigmoid output."""
    from obp.dataset import logistic_reward_function
    from scipy.special import logit

    import numpy as np

    q = logistic_reward_function(context, np.eye(N_ACTIONS), random_state=ENV_SEED, z_score=False)
    return logit(q)


def standardizer():
    """Mean and standard deviation of the logits over 10^6 contexts, fixed
    once (obp's own z_score option uses each batch's statistics)."""
    if not _STANDARDIZE:
        import numpy as np

        big = np.random.default_rng(0).normal(size=(1_000_000, DIM_CONTEXT))
        r = raw_logits(big)
        _STANDARDIZE["mu"], _STANDARDIZE["sd"] = float(r.mean()), float(r.std())
    return _STANDARDIZE["mu"], _STANDARDIZE["sd"]


def reward_function(context, action_context, random_state=None):
    """q(x, a): OBP's logistic reward, logits standardized by fixed
    constants. `random_state` is ignored so the function is the same for
    every dataset."""
    from scipy.special import expit

    mu, sd = standardizer()
    return expit((raw_logits(context) - mu) / sd)


def target_pmf(q):
    """Epsilon-greedy on the true best action: 1 - eps + eps/K on it."""
    import numpy as np

    n = q.shape[0]
    pmf = np.full((n, N_ACTIONS), EPSILON_TARGET / N_ACTIONS)
    pmf[np.arange(n), q.argmax(axis=1)] += 1.0 - EPSILON_TARGET
    return pmf


def true_value(chunk=500_000):
    import numpy as np

    rng = np.random.default_rng(424242)
    total, count, sq = 0.0, 0, 0.0
    for _ in range(TRUTH_CONTEXTS // chunk):
        x = rng.normal(size=(chunk, DIM_CONTEXT))
        q = reward_function(x, None)
        v = (target_pmf(q) * q).sum(axis=1)
        total += float(v.sum())
        sq += float((v * v).sum())
        count += chunk
    m = total / count
    return m, ((sq / count - m * m) / count) ** 0.5


def write_rows(path, bf, pi_e):
    context = bf["context"].tolist()
    pmf = bf["pi_b"][:, :, 0].tolist()
    action = bf["action"].tolist()
    pscore = bf["pscore"].tolist()
    reward = bf["reward"].tolist()
    target = pi_e.tolist()
    actions = [{"id": f"a{k}"} for k in range(N_ACTIONS)]
    eligible = list(range(N_ACTIONS))
    with open(path, "w") as f:
        for i in range(len(action)):
            f.write(json.dumps({
                "decisionId": f"r{i}",
                "context": {f"x{j}": v for j, v in enumerate(context[i])},
                "actions": actions, "eligible": eligible, "pmf": pmf[i],
                "chosen": action[i], "probability": pscore[i],
                "reward": float(reward[i]), "targetPmf": target[i],
            }) + "\n")


def run_one(task):
    """One repeat: a fresh dataset, Syntra's evaluation and OBP's."""
    pair, n, rep, seed, syntra, tmp, truth, (mu, sd) = task
    _STANDARDIZE.update(mu=mu, sd=sd)
    import numpy as np
    from sklearn.linear_model import LogisticRegression
    from sklearn.pipeline import make_pipeline
    from sklearn.preprocessing import PolynomialFeatures

    logging.disable(logging.WARNING)
    warnings.filterwarnings("ignore")
    from obp.dataset import SyntheticBanditDataset
    from obp.ope import DirectMethod, DoublyRobust, RegressionModel
    from obp.ope import InverseProbabilityWeighting as IPW
    from obp.ope import SelfNormalizedInverseProbabilityWeighting as SNIPW

    ds = SyntheticBanditDataset(n_actions=N_ACTIONS, dim_context=DIM_CONTEXT, reward_type="binary",
                                reward_function=reward_function, beta=PAIRS[pair]["beta"], random_state=seed)
    bf = ds.obtain_batch_bandit_feedback(n_rounds=n)
    q = bf["expected_reward"]
    pi_e = target_pmf(q)
    out = {"pair": pair, "n": n, "rep": rep, "seed": seed, "truth": truth,
           "inSampleValue": float((pi_e * q).sum(axis=1).mean()),
           "loggedMean": float(bf["reward"].mean())}

    # Syntra.
    path = os.path.join(tmp, f"rows-{pair}-{n}-{rep}.jsonl")
    t0 = time.perf_counter()
    write_rows(path, bf, pi_e)
    t1 = time.perf_counter()
    proc = subprocess.run([syntra, "evaluate", "--input", path, "--policy", "target-column",
                           "--w-max", "inf", "--bootstrap", str(N_BOOTSTRAP), "--folds", str(FOLDS),
                           "--seed", str(rep + 1), "--format", "json"],
                          capture_output=True, text=True)
    t2 = time.perf_counter()
    os.unlink(path)
    if proc.returncode != 0:
        raise RuntimeError(f"syntra evaluate failed: {proc.stderr}")
    rep_json = json.loads(proc.stdout)
    for k in SYNTRA_ESTIMATORS:
        e = rep_json["estimators"][k]
        entry = {"estimate": e["estimate"]}
        # Syntra reports DM without an interval (null) since this benchmark
        # found its fixed-model intervals miscalibrated.
        if e.get("lower") is not None:
            entry.update(lower=e["lower"], upper=e["upper"],
                         normalLower=e["normalLower"], normalUpper=e["normalUpper"])
        out[f"syntra/{k}"] = entry
    out["syntraEss"] = rep_json["diagnostics"]["ess"]
    out["syntraMaxWeight"] = rep_json["diagnostics"]["maxWeight"]
    out["seconds"] = {"syntraWriteRows": t1 - t0, "syntraEvaluate": t2 - t1}

    # OBP.
    t3 = time.perf_counter()
    action_dist = pi_e[:, :, np.newaxis]
    base = dict(reward=bf["reward"], action=bf["action"], pscore=bf["pscore"], action_dist=action_dist)
    ests = {"ipw": (IPW(), None), "snipw": (SNIPW(), None)}
    lr = dict(C=100, max_iter=10000, random_state=12345)
    for name, model in [("lr", LogisticRegression(**lr)),
                        ("lrx", make_pipeline(PolynomialFeatures(degree=2, interaction_only=True,
                                                                 include_bias=False),
                                              LogisticRegression(**lr)))]:
        reg = RegressionModel(n_actions=N_ACTIONS, base_model=model)
        q_hat = reg.fit_predict(context=bf["context"], action=bf["action"], reward=bf["reward"],
                                n_folds=FOLDS, random_state=rep)
        ests[f"dm_{name}"] = (DirectMethod(), q_hat)
        ests[f"dr_{name}"] = (DoublyRobust(), q_hat)
    t4 = time.perf_counter()
    for k, (est, q_hat) in ests.items():
        kw = dict(base)
        if q_hat is not None:
            kw["estimated_rewards_by_reg_model"] = q_hat
        r = {"estimate": float(est.estimate_policy_value(**kw))}
        if k in OBP_INTERVALS:
            ci = est.estimate_interval(**kw, alpha=0.05, n_bootstrap_samples=N_BOOTSTRAP, random_state=rep)
            r["lower"] = float(ci["95.0% CI (lower)"])
            r["upper"] = float(ci["95.0% CI (upper)"])
        out[f"obp/{k}"] = r
    t5 = time.perf_counter()
    out["seconds"].update({"obpRegression": t4 - t3, "obpEstimatesAndIntervals": t5 - t4})
    return out


PAIR_SHORT = {"uniform": "uniform", "aligned": "aligned", "opposed": "opposed"}


def render(out) -> str:
    """The Markdown report, from the JSON the run wrote (so `--render`
    can redraw it without rerunning)."""
    setup, summary, runs = out["setup"], out["summary"], out["runs"]
    truth, pairs, sizes = setup["truth"], list(setup["pairs"]), setup["sizes"]
    keys = [f"syntra/{k}" for k in SYNTRA_ESTIMATORS] + [f"obp/{k}" for k in OBP_ESTIMATORS]
    cells = [(p, n) for p in pairs for n in sizes]
    head = ["Estimator"] + [f"{PAIR_SHORT.get(p, p)}, {n:,}" for p, n in cells]
    align = "l" + "r" * len(cells)
    md = ["# Off-policy evaluation: Syntra and Open Bandit Pipeline\n",
          f"Target: epsilon-greedy ({setup['targetEpsilon']}) on the true best action, true value "
          f"{truth:.4f} (Monte Carlo SE {setup['truthSe']:.1e}). {setup['repeats']} independent datasets per "
          f"cell; columns are the logging policy and the number of logged rows. Logging: "
          + "; ".join(f"{p}: {setup['pairs'][p]['label']}" for p in pairs)
          + ". No weight clipping anywhere (Syntra `--w-max inf`, OBP `lambda_=inf`).\n"]
    md.append("Mean relative error, |estimate - truth| / truth averaged over datasets:\n")
    md.append(common.md_table(head, [[LABELS[k]] + [f"{100 * summary[f'{p}/{n}'][k]['meanRelError']:.2f}%"
                                                  for p, n in cells] for k in keys], align) + "\n")
    ci_keys = [k for k in keys if "coverage" in summary[f"{pairs[0]}/{sizes[0]}"][k]]
    md.append("Share of 95% intervals containing the true value (percentile bootstrap, 1,000 resamples):\n")
    md.append(common.md_table(head, [[LABELS[k]] + [f"{100 * summary[f'{p}/{n}'][k]['coverage']:.1f}%"
                                                  for p, n in cells] for k in ci_keys], align) + "\n")
    md.append("Mean width of those intervals:\n")
    md.append(common.md_table(head, [[LABELS[k]] + [f"{summary[f'{p}/{n}'][k]['meanWidth']:.3f}"
                                                  for p, n in cells] for k in ci_keys], align) + "\n")
    pooled = []
    for k in ci_keys:
        hits = [1.0 if r[k]["lower"] <= truth <= r[k]["upper"] else 0.0 for r in runs]
        c = common.mean(hits)
        pooled.append(f"{LABELS[k]} {100 * c:.1f}% (±{100 * (c * (1 - c) / len(hits)) ** 0.5:.1f})")
    md.append(f"Pooled over all {len(runs):,} datasets: " + "; ".join(pooled) + ".\n")
    rows_md = []
    for p, n in cells:
        s = summary[f"{p}/{n}"]
        pdr = s["pairedDr"]
        rows_md.append([PAIR_SHORT.get(p, p), f"{n:,}", f"{s['meanLoggedReward']:.3f}", f"{s['meanEss']:.0f}",
                        f"{s['meanMaxWeight']:.1f}"]
                       + ["{:+.2f}% [{:+.2f}, {:+.2f}]".format(*(100 * pdr[f"syntra/dr - {o}"][x]
                                                                 for x in ("mean", "lower", "upper")))
                          for o in ("obp/dr_lr", "obp/dr_lrx")])
    md.append("Per cell: logged mean reward, effective sample size and largest importance weight "
              "(Syntra's diagnostics, averaged over datasets), and Syntra DR's absolute error minus OBP DR's "
              "on the same data as a share of the true value, with a 95% t-interval (negative favours "
              "Syntra):\n")
    md.append(common.md_table(["Logging", "n", "Logged mean", "ESS", "Max weight",
                               "Syntra DR - OBP DR (additive)", "Syntra DR - OBP DR (interactions)"],
                              rows_md, "lrrrrrr") + "\n")
    worst = {k: max(summary[f"{p}/{n}"]["agreement"][k]["maxRelDiff"] for p, n in cells)
             for k in summary[f"{pairs[0]}/{sizes[0]}"]["agreement"]}
    md.append("Agreement on identical data (largest relative difference over all datasets): "
              + "; ".join(f"{k}: {v:.1e}" for k, v in worst.items()) + ".\n")
    per = {n: {k: common.mean([summary[f"{p}/{n}"]["seconds"][k] for p in pairs])
               for k in summary[f"{pairs[0]}/{n}"]["seconds"]} for n in sizes}
    md.append("Mean seconds per dataset, one process each: " + "; ".join(
        f"n = {n:,}: " + ", ".join(f"{k} {v:.2f}" for k, v in d.items()) for n, d in per.items()) + ".\n")
    md.append(f"Wall time {out['wallSeconds']:.0f} s on {out['jobs']} processes; {out['hardware']}.\n")
    return "\n".join(md)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repeats", type=int, default=200)
    ap.add_argument("--sizes", default="1000,10000,100000")
    ap.add_argument("--pairs", default=",".join(PAIRS))
    ap.add_argument("--jobs", type=int, default=common.jobs_default())
    ap.add_argument("--tmp", default=None, help="directory for the temporary JSONL files")
    ap.add_argument("--out", default=str(common.RESULTS_DIR / "ope_vs_obp"))
    ap.add_argument("--render", action="store_true",
                    help="redraw OUT.md from OUT.json without running anything")
    args = ap.parse_args()
    if args.render:
        md = render(json.loads(Path(args.out + ".json").read_text()))
        common.write_text(Path(args.out + ".md"), md)
        print(md)
        return
    started = time.time()
    sizes = [int(s) for s in args.sizes.split(",")]
    pairs = args.pairs.split(",")
    syntra = str(common.syntra_binary())
    common.log(f"truth from {TRUTH_CONTEXTS} contexts")
    truth, truth_se = true_value()
    mu, sd = standardizer()
    common.log(f"target value {truth:.6f} (Monte Carlo SE {truth_se:.1e})")

    with tempfile.TemporaryDirectory(dir=args.tmp) as tmp:
        tasks = []
        for pi, pair in enumerate(pairs):
            for si, n in enumerate(sizes):
                for rep in range(args.repeats):
                    seed = 1_000_000 * (pi + 1) + 10_000 * si + rep
                    tasks.append((pair, n, rep, seed, syntra, tmp, truth, (mu, sd)))
        tasks.sort(key=lambda t: -t[1])  # largest first
        common.log(f"{len(tasks)} evaluations on {args.jobs} processes")
        results = []
        with mp.get_context("spawn").Pool(args.jobs) as pool:
            for i, r in enumerate(pool.imap_unordered(run_one, tasks, chunksize=1), 1):
                results.append(r)
                if i % 100 == 0 or i == len(tasks):
                    common.log(f"  {i}/{len(tasks)}")

    # ------------------------------------------------------------ summaries
    groups = defaultdict(list)
    for r in results:
        groups[(r["pair"], r["n"])].append(r)
    keys = [f"syntra/{k}" for k in SYNTRA_ESTIMATORS] + [f"obp/{k}" for k in OBP_ESTIMATORS]
    summary = {}
    for (pair, n), rows in groups.items():
        s = {}
        for k in keys:
            est = [r[k]["estimate"] for r in rows]
            rel = [abs(e - truth) / truth for e in est]
            err = [e - truth for e in est]
            d = {"meanRelError": common.mean(rel), "meanRelErrorSe": common.se(rel),
                 "bias": common.mean(err), "sd": common.sd(est),
                 "rmse": common.mean([e * e for e in err]) ** 0.5}
            if "lower" in rows[0][k]:
                d["coverage"] = common.mean([1.0 if r[k]["lower"] <= truth <= r[k]["upper"] else 0.0 for r in rows])
                d["meanWidth"] = common.mean([r[k]["upper"] - r[k]["lower"] for r in rows])
            if "normalLower" in rows[0][k]:
                d["coverageNormal"] = common.mean(
                    [1.0 if r[k]["normalLower"] <= truth <= r[k]["normalUpper"] else 0.0 for r in rows])
            s[k] = d
        agree = {}
        for a, b in [("syntra/ips", "obp/ipw"), ("syntra/snips", "obp/snipw")]:
            diffs = [abs(r[a]["estimate"] - r[b]["estimate"]) for r in rows]
            agree[f"{a} vs {b}"] = {"maxAbsDiff": max(diffs),
                                   "maxRelDiff": max(abs(r[a]["estimate"] - r[b]["estimate"]) / abs(r[b]["estimate"])
                                                     for r in rows)}
        s["agreement"] = agree
        # Paired: Syntra DR's absolute error minus OBP DR's, same data.
        from scipy import stats

        paired = {}
        for other in ("obp/dr_lr", "obp/dr_lrx"):
            d = [abs(r["syntra/dr"]["estimate"] - truth) - abs(r[other]["estimate"] - truth) for r in rows]
            m, e = common.mean(d), common.se(d)
            qt = stats.t.ppf(0.975, len(d) - 1)
            paired[f"syntra/dr - {other}"] = {"mean": m / truth, "lower": (m - qt * e) / truth,
                                              "upper": (m + qt * e) / truth}
        s["pairedDr"] = paired
        s["repeats"] = len(rows)
        s["meanLoggedReward"] = common.mean([r["loggedMean"] for r in rows])
        s["meanEss"] = common.mean([r["syntraEss"] for r in rows])
        s["meanMaxWeight"] = common.mean([r["syntraMaxWeight"] for r in rows])
        s["seconds"] = {k: common.mean([r["seconds"][k] for r in rows]) for k in rows[0]["seconds"]}
        summary[f"{pair}/{n}"] = s

    elapsed = time.time() - started
    import numpy
    import obp
    import scipy
    import sklearn

    out = {
        "benchmark": "ope_vs_obp",
        "hardware": common.hardware(),
        "commit": common.git_commit(),
        "versions": {"python": sys.version.split()[0], "obp": obp.__version__, "numpy": numpy.__version__,
                     "scipy": scipy.__version__, "scikit-learn": sklearn.__version__},
        "setup": {"nActions": N_ACTIONS, "dimContext": DIM_CONTEXT, "envSeed": ENV_SEED,
                  "logitStandardization": {"mean": mu, "sd": sd}, "targetEpsilon": EPSILON_TARGET,
                  "pairs": {p: PAIRS[p] for p in pairs}, "sizes": sizes, "repeats": args.repeats,
                  "bootstrap": N_BOOTSTRAP, "folds": FOLDS, "truth": truth, "truthSe": truth_se,
                  "truthContexts": TRUTH_CONTEXTS},
        "summary": summary,
        "runs": sorted(results, key=lambda r: (r["pair"], r["n"], r["rep"])),
        "wallSeconds": elapsed,
        "jobs": args.jobs,
    }
    common.write_json(Path(args.out + ".json"), out, compact=True)
    md = render(out)
    common.write_text(Path(args.out + ".md"), md)
    print(md)


if __name__ == "__main__":
    main()
