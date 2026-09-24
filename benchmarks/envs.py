"""Python port of the simulated environments in examples/learning_bench.rs.

Everything that decides what a policy sees and earns is ported operation
for operation, so a policy run here faces exactly the problem Syntra faces
in learning_bench on the same seed:

- SplitMix64, the generator behind both the environment stream
  (`SplitMix64(seed)`: contexts, catalog items, reward draws) and the
  sampling stream (`SplitMix64(seed ^ 0xD1CE)`: one seed per decision);
- the three environments (`segments`, `drift`, `catalog`) with the same
  means, contexts, action features and item ids;
- Syntra's PMF sampling (`explore::sample`, inverse CDF with one uniform
  draw) and exploration floor (`explore::apply_floor`), and the PMF that
  learning_bench's `uniform` policy uses (epsilon-greedy with epsilon 1,
  then the default 5% floor);
- the run loop and both metrics: share of the oracle's expected reward over
  the final 10% of rounds, and mean regret per round.

Sums are plain left-to-right loops, as in Rust, not `sum()` (compensated
since Python 3.12) or numpy (pairwise), so floating-point results match bit
for bit. `learning_vs_vw.py` checks this: the uniform policy run here must
equal learning_bench's uniform policy on every seed.
"""

from __future__ import annotations

import math
from dataclasses import dataclass
from typing import Callable, List, Optional, Sequence

MASK64 = (1 << 64) - 1


class SplitMix64:
    """src/decision/rng.rs, bit for bit."""

    __slots__ = ("state",)

    def __init__(self, seed: int) -> None:
        self.state = seed & MASK64

    def next_u64(self) -> int:
        self.state = (self.state + 0x9E3779B97F4A7C15) & MASK64
        z = self.state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK64
        return z ^ (z >> 31)

    def next_f64(self) -> float:
        return (self.next_u64() >> 11) * (1.0 / (1 << 53))


def _seq_sum(values: Sequence[float]) -> float:
    total = 0.0
    for v in values:
        total += v
    return total


def _finish(pmf: List[float], minimum: float) -> None:
    """explore::finish: renormalize, then raise entries below `minimum`."""
    total = _seq_sum(pmf)
    if total > 0.0 and math.isfinite(total) and total != 1.0:
        for i in range(len(pmf)):
            pmf[i] /= total
    for i in range(len(pmf)):
        if pmf[i] < minimum:
            pmf[i] = minimum


def argmax(predictions: Sequence[float]) -> int:
    """explore::argmax: ties go to the lowest index."""
    best = 0
    for i in range(1, len(predictions)):
        if predictions[i] > predictions[best]:
            best = i
    return best


def epsilon_greedy(predictions: Sequence[float], epsilon: float) -> List[float]:
    """explore::epsilon_greedy."""
    k = len(predictions)
    best = argmax(predictions)
    pmf = [epsilon / k] * k
    others = _seq_sum([p for a, p in enumerate(pmf) if a != best])
    pmf[best] = 1.0 - others
    _finish(pmf, 0.0)
    return pmf


def apply_floor(pmf: List[float], floor: float) -> None:
    """explore::apply_floor: p = (1 - floor) p + floor / K, renormalized."""
    if not pmf:
        return
    minimum = floor / len(pmf)
    for i in range(len(pmf)):
        pmf[i] = (1.0 - floor) * pmf[i] + minimum
    _finish(pmf, minimum)


def sample(pmf: Sequence[float], rng: SplitMix64) -> int:
    """explore::sample: invert the CDF with one uniform draw."""
    u = rng.next_f64()
    cumulative = 0.0
    for i, p in enumerate(pmf):
        cumulative += p
        if u < cumulative and p > 0.0:
            return i
    for i in range(len(pmf) - 1, -1, -1):
        if pmf[i] > 0.0:
            return i
    return len(pmf) - 1


def uniform_pmf(k: int) -> List[float]:
    """The PMF of learning_bench's `uniform` policy: the engine with
    epsilon-greedy at epsilon 1 and the default floor of 0.05. It never
    learns, so every prediction is 0 and the argmax is action 0."""
    pmf = epsilon_greedy([0.0] * k, 1.0)
    apply_floor(pmf, 0.05)
    return pmf


@dataclass
class Action:
    id: str
    features: dict  # numeric features, as in the ActionSpec's `features`


@dataclass
class Round:
    context: dict
    actions: List[Action]
    means: List[float]


SEGMENT_NAMES = ("free", "pro", "team", "enterprise")
SEGMENT_MEANS = (
    (0.7, 0.5, 0.4),
    (0.4, 0.7, 0.5),
    (0.3, 0.5, 0.6),
    (0.5, 0.6, 0.9),
)
SEGMENT_ACTIONS = [Action(a, {}) for a in ("small", "medium", "large")]


class Segments:
    """Four user segments x three actions; `drift` rotates the means of
    every segment left by one halfway through the run."""

    def __init__(self, drift: bool) -> None:
        self.drift = drift
        self.name = "drift" if drift else "segments"

    def round(self, t: int, total: int, rng: SplitMix64) -> Round:
        s = rng.next_u64() % 4
        means = list(SEGMENT_MEANS[s])
        if self.drift and t >= total // 2:
            means = means[1:] + means[:1]
        return Round(
            context={"segment": SEGMENT_NAMES[s], "hour": (t // 97) % 24},
            actions=SEGMENT_ACTIONS,
            means=means,
        )


_ITEM_FEATURES = {}


def item_features(item: int):
    """Fixed item features from the item number (SplitMix64 on a mix)."""
    feat = _ITEM_FEATURES.get(item)
    if feat is None:
        ir = SplitMix64(item * 0x9E3779B9 + 1)
        feat = (ir.next_f64(), ir.next_f64())
        _ITEM_FEATURES[item] = feat
    return feat


class Catalog:
    """20 distinct items of 200 per request, two features each; reward
    0.9 - 0.8 * distance(user, item), clamped to [0.05, 0.95]."""

    name = "catalog"

    def round(self, t: int, total: int, rng: SplitMix64) -> Round:
        user = (rng.next_f64(), rng.next_f64())
        items: List[int] = []
        while len(items) < 20:
            item = rng.next_u64() % 200
            if item not in items:
                items.append(item)
        actions = []
        means = []
        for item in items:
            fx, fy = item_features(item)
            actions.append(Action(f"item{item}", {"x": fx, "y": fy}))
            dx = user[0] - fx
            dy = user[1] - fy
            dist = math.sqrt(dx * dx + dy * dy)
            m = 0.9 - 0.8 * dist
            # f64::clamp(0.05, 0.95)
            if m < 0.05:
                m = 0.05
            elif m > 0.95:
                m = 0.95
            means.append(m)
        return Round(context={"x": user[0], "y": user[1]}, actions=actions, means=means)


ENVIRONMENTS = {"segments": lambda: Segments(False), "drift": lambda: Segments(True), "catalog": Catalog}


class Policy:
    """What the run loop needs from a learner: a PMF over the round's
    actions, and an update after the reward."""

    def pmf(self, rnd: Round) -> List[float]:
        raise NotImplementedError

    def learn(self, rnd: Round, chosen: int, probability: float, reward: float) -> None:
        pass

    def close(self) -> None:
        pass


class UniformPolicy(Policy):
    def pmf(self, rnd: Round) -> List[float]:
        return uniform_pmf(len(rnd.actions))


@dataclass
class Outcome:
    tail_share: float
    regret_per_round: float


def run(env, policy: Policy, rounds: int, seed: int,
        on_round: Optional[Callable[[int, Round, int], None]] = None) -> Outcome:
    """learning_bench's `run`: the same generator streams and metrics."""
    rng = SplitMix64(seed)
    draws = SplitMix64(seed ^ 0xD1CE)
    tail_from = rounds - rounds // 10
    tail_got = tail_best = regret = 0.0
    for t in range(rounds):
        r = env.round(t, rounds, rng)
        pmf = policy.pmf(r)
        chosen = sample(pmf, SplitMix64(draws.next_u64()))
        mean = r.means[chosen]
        best = max(r.means)
        reward = 1.0 if rng.next_f64() < mean else 0.0
        policy.learn(r, chosen, pmf[chosen], reward)
        regret += best - mean
        if t >= tail_from:
            tail_got += mean
            tail_best += best
        if on_round is not None:
            on_round(t, r, chosen)
    policy.close()
    return Outcome(tail_share=tail_got / tail_best, regret_per_round=regret / rounds)
