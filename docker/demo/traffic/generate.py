#!/usr/bin/env python3
"""Capsule-aware traffic generator for the Syntra demo.

Drives ~1 /decide + 1 /feedback per second against the capsule selected
by ``SYNTRA_DEMO_CAPSULE`` (default: ``predictive-autoscaling``). The
per-capsule generators below produce features matching each capsule's
context spec and a synthetic reward that makes the meta-bandit converge
on a sensible leader.

Stdlib only.
"""
from __future__ import annotations

import json
import math
import os
import random
import sys
import time
import urllib.error
import urllib.request

ADMIN_KEY = os.environ["LYCAN_ADMIN_KEY"]
SYNTRA_URL = os.environ.get("SYNTRA_URL", "http://127.0.0.1:8787")
CAPSULE = os.environ.get("SYNTRA_DEMO_CAPSULE", "predictive-autoscaling")
TICK_SECONDS = float(os.environ.get("SYNTRA_TRAFFIC_INTERVAL", "1.0"))


CAPSULE_PATHS: dict[str, str] = {
    "predictive-autoscaling":         "/tenants/demo/jobs/autoscale/capsules/orders",
    "anomaly-routing":                "/tenants/demo/jobs/routing/capsules/api",
    "seasonal-fraud-threshold":       "/tenants/demo/jobs/fraud/capsules/threshold",
    "shared-state-action-embeddings": "/tenants/demo/jobs/embeddings/capsules/router",
    "hierarchical-region-routing":    "/tenants/demo/jobs/region/capsules/router",
}

# Option label lists in the order the .lycs (choice ...) node enumerates them.
# Index into this list using `decisions[0].chosen_option` from the /decide
# response — except for hierarchical capsules, which return `leafName` instead
# of an integer index (see the driver loop for the special-case lookup).
OPTIONS: dict[str, list[str]] = {
    "predictive-autoscaling":         ["hold", "forecast_match", "forecast_headroom", "p95_safe"],
    "anomaly-routing":                ["primary", "secondary", "degraded_cache_only", "circuit_break"],
    "seasonal-fraud-threshold":       ["loose", "baseline", "tight", "very_tight"],
    "shared-state-action-embeddings": ["A", "B", "C", "D", "E", "F"],
    "hierarchical-region-routing":    [
        "us_small", "us_medium", "us_large", "eu_small", "eu_medium", "eu_large",
    ],
}


def _req(method: str, path: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(SYNTRA_URL + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {ADMIN_KEY}")
    req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=5) as resp:
        return json.loads(resp.read())


def _hour() -> float:
    return (time.time() / 3600.0) % 24.0


# --------------------------------------------------------------------------- #
# predictive-autoscaling
# --------------------------------------------------------------------------- #

_load_window: list[float] = []


def _step_predictive(tick: int) -> tuple[dict, dict]:
    """Returns (decide_body, generator_state) for predictive-autoscaling."""
    hour = _hour()
    # Diurnal load shape with Gaussian noise and a rare 5x spike.
    base = 50.0 + 100.0 * math.sin((hour / 24.0) * 2.0 * math.pi)
    noise = random.gauss(0.0, 15.0)
    load = max(0.0, base + noise)
    if random.random() < (1.0 / 60.0):
        load *= 5.0
    _load_window.append(load)
    if len(_load_window) > 10:
        _load_window.pop(0)
    # Pad cold-start with a constant so the first few ticks still produce a
    # 10-point history.
    window = _load_window if len(_load_window) >= 10 else [_load_window[0]] * (10 - len(_load_window)) + _load_window

    # load_trend in [-1, 1]: slope of last 5 minus first 5 over a max delta.
    early = sum(window[:5]) / 5.0
    late = sum(window[5:]) / 5.0
    trend = max(-1.0, min(1.0, (late - early) / 150.0))

    current_instances = 3
    features = {
        "hour":              hour,
        "current_instances": current_instances,
        "load_trend":        trend,
    }
    decide_body = {
        "load_history":       window,
        "current_instances":  current_instances,
        "target_per_instance": 100,
        "min_instances":      1,
        "max_instances":      20,
        "features":           features,
    }
    return decide_body, {"trend": trend}


def _reward_predictive(option_name: str, gen_state: dict) -> float:
    trend = gen_state["trend"]
    # sla_met favours forecast_headroom on rising load, forecast_match on flat.
    if trend > 0.3:
        winner = "forecast_headroom"
    elif trend < -0.3:
        winner = "hold"
    else:
        winner = "forecast_match"
    sla_met = 1.0 if option_name == winner else (0.4 if option_name in {"forecast_match", "forecast_headroom"} else 0.1)
    cost_efficiency = random.uniform(0.5, 0.9)
    # Weighted form matching capsule.yaml (sla_met *0.7, cost_efficiency *0.3).
    return max(-1.0, min(1.0, 0.7 * sla_met + 0.3 * cost_efficiency))


# --------------------------------------------------------------------------- #
# anomaly-routing
# --------------------------------------------------------------------------- #

_latency_window: list[float] = []


def _step_anomaly(tick: int) -> tuple[dict, dict]:
    # Most samples near 120 +- 20 ms; 5-10% outliers at 400-800 ms.
    if random.random() < 0.08:
        lat = random.uniform(400.0, 800.0)
    else:
        lat = max(1.0, random.gauss(120.0, 20.0))
    _latency_window.append(lat)
    if len(_latency_window) > 10:
        _latency_window.pop(0)
    window = _latency_window if len(_latency_window) >= 10 else [_latency_window[0]] * (10 - len(_latency_window)) + _latency_window

    mean = sum(window) / len(window)
    var = sum((x - mean) ** 2 for x in window) / len(window)
    stddev = math.sqrt(var) if var > 0 else 0.0
    z_score = 0.0 if stddev == 0 else (lat - mean) / stddev

    features = {
        "z_score":         z_score,
        "hour":            _hour(),
        "current_latency": lat,
    }
    decide_body = {
        "latency_history": window,
        "current_latency": lat,
        "features":        features,
    }
    return decide_body, {"z": z_score, "lat": lat}


def _reward_anomaly(option_name: str, gen_state: dict) -> float:
    z = abs(gen_state["z"])
    lat = gen_state["lat"]
    if z < 1.0:
        winner = "primary"
    elif z < 3.0:
        winner = "secondary"
    else:
        winner = "circuit_break"
    success_rate = 1.0 if option_name == winner else 0.3
    if option_name == "degraded_cache_only":
        # Cache wins when primary would have failed badly.
        success_rate = 0.6 if z >= 1.0 else 0.4
    # tail_latency_penalty normalised by 2000ms budget (matches capsule.yaml).
    tail_penalty = min(1.0, max(0.0, lat) / 2000.0)
    # Weights from capsule.yaml: success_rate * 0.7, tail_penalty * -0.3.
    return max(-1.0, min(1.0, 0.7 * success_rate - 0.3 * tail_penalty))


# --------------------------------------------------------------------------- #
# seasonal-fraud-threshold
# --------------------------------------------------------------------------- #

_fraud_window: list[float] = []
_fraud_state = {"rate": 0.02, "direction": 1.0}


def _step_fraud(tick: int) -> tuple[dict, dict]:
    # Drift fraud rate slowly between 0.02 and 0.04.
    s = _fraud_state
    s["rate"] += 0.001 * s["direction"]
    if s["rate"] >= 0.04:
        s["direction"] = -1.0
    elif s["rate"] <= 0.02:
        s["direction"] = 1.0
    fr = max(0.0, s["rate"] + random.gauss(0.0, 0.002))
    _fraud_window.append(fr)
    if len(_fraud_window) > 10:
        _fraud_window.pop(0)
    window = _fraud_window if len(_fraud_window) >= 10 else [_fraud_window[0]] * (10 - len(_fraud_window)) + _fraud_window

    hour = _hour()
    is_weekend = 1.0 if int(time.time() / 86400.0) % 7 in (5, 6) else 0.0
    volume = random.uniform(800.0, 3500.0)
    features = {
        "hour":           hour,
        "is_weekend":     is_weekend,
        "current_volume": volume,
    }
    decide_body = {
        "fraud_rate_history": window,
        "current_volume":     volume,
        "features":           features,
    }
    return decide_body, {"rate": s["rate"]}


def _reward_fraud(option_name: str, gen_state: dict) -> float:
    rate = gen_state["rate"]
    # caught_fraud: tighter is better when fraud is elevated.
    if rate > 0.025:
        catch_table = {"loose": 0.2, "baseline": 0.4, "tight": 0.85, "very_tight": 0.95}
    else:
        catch_table = {"loose": 0.5, "baseline": 0.7, "tight": 0.75, "very_tight": 0.8}
    caught = catch_table.get(option_name, 0.5)
    # false_positive_cost: tight policies cost more when fraud is low.
    if rate < 0.02:
        fp_table = {"loose": 0.05, "baseline": 0.1, "tight": 0.3, "very_tight": 0.45}
    else:
        fp_table = {"loose": 0.05, "baseline": 0.08, "tight": 0.15, "very_tight": 0.2}
    fp = fp_table.get(option_name, 0.1)
    # Budget normalisation on fp with budget=0.5 from capsule.yaml.
    fp_norm = min(1.0, fp / 0.5)
    # Weights: caught_fraud * 0.6, false_positive_cost * -0.4.
    return max(-1.0, min(1.0, 0.6 * caught - 0.4 * fp_norm))


# --------------------------------------------------------------------------- #
# shared-state-action-embeddings
# --------------------------------------------------------------------------- #

OPTION_FEATURES = {
    "A": (0.1, 0.1),
    "B": (0.1, 0.9),
    "C": (0.9, 0.1),
    "D": (0.9, 0.9),
    "E": (0.5, 0.5),
    "F": (0.3, 0.7),
}


def _step_shared(tick: int) -> tuple[dict, dict]:
    if tick < 100:
        workload = random.uniform(0.0, 1.0)
    else:
        workload = random.uniform(0.4, 0.6)
    decide_body = {"features": {"workload": workload}}
    return decide_body, {"workload": workload}


def _reward_shared(option_name: str, gen_state: dict) -> float:
    workload = gen_state["workload"]
    x = OPTION_FEATURES.get(option_name, (0.0, 0.0))
    base = 0.10 * workload + 0.40 * x[0] + 0.60 * x[1]
    return max(-1.0, min(1.0, base + random.uniform(-0.05, 0.05)))


# --------------------------------------------------------------------------- #
# hierarchical-region-routing
# --------------------------------------------------------------------------- #
#
# Six leaves arranged as a 2x3 tree: (us, eu) x (small, medium, large). The
# reward function carries a clean per-level signal:
#   region bonus:  us = 0.30, eu = 0.00
#   size bonus:    small = 0.00, medium = 0.40, large = 0.20
# So us_medium = 0.70 (best), eu_small = 0.00 (worst). Both the root meta-
# bandit (region) and the per-region sub-bandits (size) should converge
# clearly within a few hundred rounds. Hierarchical decides ignore the
# context body in v1 — the program graph isn't executed — so we POST an
# empty body.

_REGION_BONUS = {"us": 0.30, "eu": 0.00}
_SIZE_BONUS   = {"small": 0.00, "medium": 0.40, "large": 0.20}


def _step_hier(tick: int) -> tuple[dict, dict]:
    return {}, {}


def _reward_hier(option_name: str, gen_state: dict) -> float:
    # option_name = "<region>_<size>", e.g. "us_medium".
    try:
        region, size = option_name.split("_", 1)
    except ValueError:
        return 0.0
    r = _REGION_BONUS.get(region, 0.0) + _SIZE_BONUS.get(size, 0.0)
    return max(-1.0, min(1.0, r + random.uniform(-0.03, 0.03)))


# --------------------------------------------------------------------------- #
# Driver
# --------------------------------------------------------------------------- #

STEP_FNS = {
    "predictive-autoscaling":         _step_predictive,
    "anomaly-routing":                _step_anomaly,
    "seasonal-fraud-threshold":       _step_fraud,
    "shared-state-action-embeddings": _step_shared,
    "hierarchical-region-routing":    _step_hier,
}

REWARD_FNS = {
    "predictive-autoscaling":         _reward_predictive,
    "anomaly-routing":                _reward_anomaly,
    "seasonal-fraud-threshold":       _reward_fraud,
    "shared-state-action-embeddings": _reward_shared,
    "hierarchical-region-routing":    _reward_hier,
}


def main() -> int:
    if CAPSULE not in CAPSULE_PATHS:
        sys.stderr.write(
            f"[traffic] unknown SYNTRA_DEMO_CAPSULE={CAPSULE!r}; "
            f"valid: {sorted(CAPSULE_PATHS)}\n"
        )
        return 2

    path = CAPSULE_PATHS[CAPSULE]
    options = OPTIONS[CAPSULE]
    step = STEP_FNS[CAPSULE]
    reward_fn = REWARD_FNS[CAPSULE]

    print(f"[traffic] driving {CAPSULE} ({path}) every {TICK_SECONDS}s", flush=True)

    tick = 0
    while True:
        tick += 1
        try:
            decide_body, gen_state = step(tick)
            resp = _req("POST", f"{path}/decide", body=decide_body)
            if resp.get("refused"):
                time.sleep(TICK_SECONDS)
                continue
            decisions = resp.get("decisions") or []
            if not decisions:
                time.sleep(TICK_SECONDS)
                continue
            # Hierarchical decisions carry the chosen leaf by name, not by
            # integer index. Look for `leafName` first; fall back to the
            # flat-path `chosen_option` index lookup for the other flavors.
            leaf_name = decisions[0].get("leafName")
            if leaf_name is not None:
                option_name = leaf_name
            else:
                idx = decisions[0].get("chosen_option")
                if idx is None or not (0 <= idx < len(options)):
                    time.sleep(TICK_SECONDS)
                    continue
                option_name = options[idx]
            did = resp.get("decisionId") or resp.get("decision_id")
            if did is None:
                time.sleep(TICK_SECONDS)
                continue

            reward = float(reward_fn(option_name, gen_state))
            feedback_body = {"decisionId": did, "reward": reward}
            _req("POST", f"{path}/feedback", body=feedback_body)
        except urllib.error.HTTPError as e:
            sys.stderr.write(f"[traffic] HTTP {e.code}: {e.read()[:200]!r}\n")
        except urllib.error.URLError as e:
            sys.stderr.write(f"[traffic] URL error: {e}\n")
        except Exception as e:  # noqa: BLE001 — keep traffic gen alive
            sys.stderr.write(f"[traffic] unexpected: {type(e).__name__}: {e}\n")
        time.sleep(TICK_SECONDS)


if __name__ == "__main__":
    sys.exit(main())
