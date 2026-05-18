#!/usr/bin/env python3
"""
Task 2a discriminator analysis.

For each seed, extract per-week intervention levels for Syntra and the
myopic oracle. Compute:
  i.   Mean intervention level across the run for each policy
  ii.  Earliest week each policy first escalates to level 2+ (moderate)
  iii. Earliest week each policy first escalates to level 4 (lockdown)
  iv.  Active-case count at the moment of each policy's first escalation
       to each level

Interpretation:
  - If Syntra escalates at LOWER case counts than the myopic oracle → supports
    interpretation (A): "Syntra learns a multi-step pattern single-week
    optimization misses"
  - If Syntra sits at HIGHER intervention levels uniformly without earlier
    escalation → supports interpretation (B): "Syntra over-restricts because
    its reward function over-values prevention"
"""

import os
import statistics
import sys
import urllib.request
import json

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import benchmark as bm

N_SEEDS = 10
N_WEEKS = 52
N_REGIONS = 4


def first_escalation_week(levels, region_idx, n_regions, threshold):
    """Earliest week index where this region's level >= threshold."""
    for w in range(N_WEEKS):
        if levels[w * n_regions + region_idx] >= threshold:
            return w
    return None


def main():
    syntra = bm.SyntraClient("http://localhost:8787", "dev-key",
                              "outbreak_disc", "main", "policy")
    capsule = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                            "intervention_policy.lyc")

    # Probe the runtime for the live AdaptiveChoice node ID
    syntra.reset()
    syntra.setup(capsule)
    syntra.configure_learning()
    probe = syntra._request("POST", f"{syntra.base_path}/decide",
                             {"contextKey": "probe", "input": {}})
    bm.SYNTRA_NODE_ID = probe["decisions"][0]["node_id"]
    syntra.reset()  # wipe probe state

    syntra_levels_all = []
    oracle_levels_all = []
    syntra_first_lvl2 = []
    oracle_first_lvl2 = []
    syntra_first_lvl4 = []
    oracle_first_lvl4 = []
    syntra_cases_at_lvl2 = []
    oracle_cases_at_lvl2 = []
    syntra_cases_at_lvl4 = []
    oracle_cases_at_lvl4 = []
    # Cumulative intervention cost spent BEFORE the first death in each region.
    # Distinguishes "proactive heavy spend before any deaths" from "reactive
    # heavy spend after early deaths" — different policy shapes.
    syntra_cost_before_first_death = []
    oracle_cost_before_first_death = []

    for i in range(N_SEEDS):
        seed = 2000 + i
        result = bm.run_seed(seed, N_WEEKS, N_REGIONS, syntra, capsule)
        syntra_outcomes = result["outcomes"]["syntra"]
        oracle_outcomes = result["outcomes"]["myopic_oracle"]

        s_levels = [o.level for o in syntra_outcomes]
        o_levels = [o.level for o in oracle_outcomes]
        syntra_levels_all.extend(s_levels)
        oracle_levels_all.extend(o_levels)

        # Per-region: cases at first lvl2 and first lvl4 escalation;
        # cumulative cost spent before the first death.
        for r_idx in range(N_REGIONS):
            s_w2 = first_escalation_week(s_levels, r_idx, N_REGIONS, 2)
            o_w2 = first_escalation_week(o_levels, r_idx, N_REGIONS, 2)
            s_w4 = first_escalation_week(s_levels, r_idx, N_REGIONS, 4)
            o_w4 = first_escalation_week(o_levels, r_idx, N_REGIONS, 4)

            s_cost = 0.0
            for w in range(N_WEEKS):
                out = syntra_outcomes[w * N_REGIONS + r_idx]
                if out.deaths > 0:
                    break
                s_cost += out.econ_cost
            syntra_cost_before_first_death.append(s_cost)

            o_cost = 0.0
            for w in range(N_WEEKS):
                out = oracle_outcomes[w * N_REGIONS + r_idx]
                if out.deaths > 0:
                    break
                o_cost += out.econ_cost
            oracle_cost_before_first_death.append(o_cost)
            if s_w2 is not None:
                syntra_first_lvl2.append(s_w2)
                cases = syntra_outcomes[s_w2 * N_REGIONS + r_idx].new_cases_true
                syntra_cases_at_lvl2.append(cases)
            if o_w2 is not None:
                oracle_first_lvl2.append(o_w2)
                cases = oracle_outcomes[o_w2 * N_REGIONS + r_idx].new_cases_true
                oracle_cases_at_lvl2.append(cases)
            if s_w4 is not None:
                syntra_first_lvl4.append(s_w4)
                cases = syntra_outcomes[s_w4 * N_REGIONS + r_idx].new_cases_true
                syntra_cases_at_lvl4.append(cases)
            if o_w4 is not None:
                oracle_first_lvl4.append(o_w4)
                cases = oracle_outcomes[o_w4 * N_REGIONS + r_idx].new_cases_true
                oracle_cases_at_lvl4.append(cases)

        print(f"  seed {seed}: syntra mean lvl={statistics.mean(s_levels):.2f}, "
              f"myopic_oracle mean lvl={statistics.mean(o_levels):.2f}")

    def fmt(name, syntra_data, oracle_data):
        sd = (statistics.mean(syntra_data), min(syntra_data), max(syntra_data)) if syntra_data else (None, None, None)
        od = (statistics.mean(oracle_data), min(oracle_data), max(oracle_data)) if oracle_data else (None, None, None)
        print(f"  {name}:")
        print(f"    syntra:         mean={sd[0]}, min={sd[1]}, max={sd[2]}, n={len(syntra_data)}")
        print(f"    myopic_oracle:  mean={od[0]}, min={od[1]}, max={od[2]}, n={len(oracle_data)}")

    print()
    print("=" * 72)
    print("  DISCRIMINATOR ANALYSIS — outbreak (Task 2a)")
    print("=" * 72)
    print()
    print("(i) Mean intervention level across the run")
    print(f"    syntra:         {statistics.mean(syntra_levels_all):.3f}")
    print(f"    myopic_oracle:  {statistics.mean(oracle_levels_all):.3f}")
    print()
    print("(ii) Earliest week first escalating to level 2+ (per region)")
    fmt("first_lvl2_week", syntra_first_lvl2, oracle_first_lvl2)
    print()
    print("(iii) Earliest week first escalating to level 4 (per region)")
    fmt("first_lvl4_week", syntra_first_lvl4, oracle_first_lvl4)
    print()
    print("(iv) Active TRUE new-case count at first escalation")
    fmt("cases_at_first_lvl2", syntra_cases_at_lvl2, oracle_cases_at_lvl2)
    fmt("cases_at_first_lvl4", syntra_cases_at_lvl4, oracle_cases_at_lvl4)
    print()
    print("(v) Cumulative intervention cost spent BEFORE first death (per region)")
    fmt("cost_before_first_death", syntra_cost_before_first_death, oracle_cost_before_first_death)
    print("    Distinguishes proactive heavy-spend (cost > 0 before any deaths)")
    print("    from reactive heavy-spend (cost low until deaths start, then ramps).")
    print()
    print("Interpretation:")
    if syntra_cases_at_lvl4 and oracle_cases_at_lvl4:
        s_med = statistics.median(syntra_cases_at_lvl4)
        o_med = statistics.median(oracle_cases_at_lvl4)
        print(f"  Median cases at first lvl4: syntra={s_med}, oracle={o_med}")
        if s_med < o_med * 0.5:
            print(f"  → syntra escalates to lockdown at <50% of oracle's case count threshold.")
            print(f"    Supports (A): multi-step pattern — escalates earlier than myopic optimum.")
        elif s_med > o_med * 1.5:
            print(f"  → syntra escalates to lockdown only at higher case counts than oracle.")
            print(f"    This is unusual and merits checking the implementation.")
        else:
            print(f"  → syntra and oracle escalate at comparable case counts.")
            print(f"    Mean-level difference (syntra {statistics.mean(syntra_levels_all):.2f} vs")
            print(f"    oracle {statistics.mean(oracle_levels_all):.2f}) is uniform escalation, not earlier.")
            print(f"    Supports (B): reward-function-driven uniform over-restriction.")


if __name__ == "__main__":
    main()
