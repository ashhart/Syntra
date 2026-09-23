"""LLM routing: choose the model per request, learn from quality, cost and
latency.

``ModelRouter`` wraps any completion function (``litellm.completion``,
``litellm.acompletion``, a provider SDK, your own gateway client). For each
request it decides which model to call with a ``LocalDecider`` (in-process,
microseconds), calls it, measures latency and cost, and reports a reward
once the quality of the answer is known: immediately if you pass
``quality`` or a ``judge``, or later through ``report_quality``. A request
whose quality never arrives gets the capsule's ``reward.default`` after its
wait, if the spec sets one.

    import litellm
    from syntra import LocalDecider
    from syntra.llm import ModelRouter

    router = ModelRouter(
        LocalDecider(URL, token=TOKEN, tenant="acme", job="llm", capsule="router"),
        models={
            "openai/gpt-4o-mini": {"tier": "small", "costPer1MInput": 0.15},
            "anthropic/claude-sonnet-5": {"tier": "large", "costPer1MInput": 3.0},
        },
        completion=litellm.completion,
        cost=lambda response: litellm.completion_cost(response),
    )
    result = router.completion(messages=[{"role": "user", "content": "..."}],
                               context={"task": "code"})
    ...  # later, when you know how good the answer was:
    router.report_quality(result.decision_id, 0.9)

The reward is ``quality - cost_weight * cost_usd - latency_weight *
latency_s`` (failures score ``failure_reward``), so set the weights to what
a dollar and a second are worth to you in quality points. Nothing here
depends on LiteLLM; it is passed in.
"""

from __future__ import annotations

import threading
import time
from collections import OrderedDict
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable, Mapping, Optional, Sequence

__all__ = ["ModelRouter", "RoutedCompletion", "Outcome", "prompt_features"]


@dataclass(frozen=True)
class Outcome:
    """What happened on one routed call."""

    model: str
    latency_s: float
    cost_usd: Optional[float]
    ok: bool
    error: Optional[str] = None


@dataclass
class RoutedCompletion:
    """The model's response, the decision that chose the model, and what
    the call cost."""

    response: Any
    decision_id: str
    model: str
    probability: float
    outcome: Outcome
    #: The reward reported so far (None until the quality is known).
    reward: Optional[float] = field(default=None)


def prompt_features(messages: Sequence[Mapping[str, Any]], **kwargs: Any) -> dict:
    """Cheap request features: size, turns, tools, and a few task hints.

    Uses only what is in the request, in microseconds; add your own
    (tenant tier, product surface, detected language) through ``context``.
    """
    chars = 0
    code = False
    for m in messages:
        content = m.get("content")
        if isinstance(content, str):
            chars += len(content)
            code = code or "```" in content or "def " in content or "function " in content
        elif isinstance(content, list):
            for part in content:
                if isinstance(part, Mapping) and isinstance(part.get("text"), str):
                    chars += len(part["text"])
    features: dict = {
        "promptTokens": round(chars / 4),
        "turns": sum(1 for m in messages if m.get("role") == "user"),
        "hasSystem": any(m.get("role") == "system" for m in messages),
        "looksLikeCode": code,
    }
    if kwargs.get("tools"):
        features["tools"] = len(kwargs["tools"])
    if kwargs.get("response_format"):
        features["structuredOutput"] = True
    return features


@dataclass(frozen=True)
class _Decision:
    decision_id: str
    action: str
    probability: float

    @staticmethod
    def of(d: Any) -> "_Decision":
        if isinstance(d, Mapping):
            return _Decision(str(d["decisionId"]), str(d["action"]), float(d["probability"]))
        return _Decision(d.decision_id, d.action, d.probability)


class ModelRouter:
    """Decide the model per request with a ``LocalDecider`` and learn from
    how the call went.

    ``models`` maps each model name (as your ``completion`` function
    expects it) to its features (tier, price, context window, provider),
    which let the router generalize across models. ``completion`` is called
    as ``completion(model=name, messages=messages, **kwargs)``; ``cost``
    returns the call's cost in USD from the response (omit it to learn from
    quality and latency only).
    """

    def __init__(
        self,
        decider: Any,
        models: Mapping[str, Mapping[str, Any]],
        completion: Callable[..., Any],
        *,
        cost: Optional[Callable[[Any], Optional[float]]] = None,
        judge: Optional[Callable[[Any], Optional[float]]] = None,
        cost_weight: float = 1.0,
        latency_weight: float = 0.0,
        failure_reward: float = 0.0,
        features: Callable[..., dict] = prompt_features,
        max_pending: int = 100_000,
    ) -> None:
        if not models:
            raise ValueError("models must name at least one model")
        self.decider = decider
        self.models = {k: dict(v) for k, v in models.items()}
        self.completion_fn = completion
        self.cost_fn = cost
        self.judge = judge
        self.cost_weight = float(cost_weight)
        self.latency_weight = float(latency_weight)
        self.failure_reward = float(failure_reward)
        self.features = features
        # Calls waiting for their quality, oldest first. Beyond the bound
        # the oldest are forgotten; the capsule's `reward.default` then
        # rewards them after its wait.
        self._pending: "OrderedDict[str, Outcome]" = OrderedDict()
        self._max_pending = max(1, int(max_pending))
        self._lock = threading.Lock()

    # -- deciding ----------------------------------------------------------

    def _actions(self, allowed: Optional[Sequence[str]]) -> list:
        names = list(self.models) if allowed is None else [m for m in allowed if m in self.models]
        if not names:
            raise ValueError("none of the allowed models is configured")
        return [{"id": n, "features": self.models[n]} for n in names]

    def choose(
        self,
        messages: Sequence[Mapping[str, Any]],
        *,
        context: Optional[Mapping[str, Any]] = None,
        allowed: Optional[Sequence[str]] = None,
        **kwargs: Any,
    ) -> Any:
        """Only decide (no call): the decision, with ``action`` the model."""
        ctx = dict(self.features(messages, **kwargs))
        if context:
            ctx.update(context)
        d = self.decider.decide(ctx, actions=self._actions(allowed))
        # `syntra.Client` (HTTP) answers a dict; `LocalDecider` a Decision.
        return _Decision.of(d)

    # -- calling -----------------------------------------------------------

    def completion(
        self,
        messages: Sequence[Mapping[str, Any]],
        *,
        context: Optional[Mapping[str, Any]] = None,
        allowed: Optional[Sequence[str]] = None,
        quality: Optional[float] = None,
        **kwargs: Any,
    ) -> RoutedCompletion:
        """Route one call. Raises what the completion function raises,
        after recording the failure as the decision's outcome."""
        d = self.choose(messages, context=context, allowed=allowed, **kwargs)
        started = time.perf_counter()
        try:
            response = self.completion_fn(model=d.action, messages=messages, **kwargs)
        except Exception as e:  # recorded, then re-raised
            self._finish(d, None, time.perf_counter() - started, e)
            raise
        return self._finish(d, response, time.perf_counter() - started, None, quality)

    async def acompletion(
        self,
        messages: Sequence[Mapping[str, Any]],
        *,
        context: Optional[Mapping[str, Any]] = None,
        allowed: Optional[Sequence[str]] = None,
        quality: Optional[float] = None,
        **kwargs: Any,
    ) -> RoutedCompletion:
        """``completion`` for an async completion function
        (``litellm.acompletion``)."""
        d = self.choose(messages, context=context, allowed=allowed, **kwargs)
        started = time.perf_counter()
        try:
            call: Awaitable[Any] = self.completion_fn(model=d.action, messages=messages, **kwargs)
            response = await call
        except Exception as e:
            self._finish(d, None, time.perf_counter() - started, e)
            raise
        return self._finish(d, response, time.perf_counter() - started, None, quality)

    def _finish(
        self,
        d: Any,
        response: Any,
        latency_s: float,
        error: Optional[BaseException],
        quality: Optional[float] = None,
    ) -> RoutedCompletion:
        cost = None
        if response is not None and self.cost_fn is not None:
            try:
                cost = self.cost_fn(response)
            except Exception:  # a pricing lookup must not fail the call
                cost = None
        outcome = Outcome(
            model=d.action,
            latency_s=latency_s,
            cost_usd=None if cost is None else float(cost),
            ok=error is None,
            error=None if error is None else f"{type(error).__name__}: {error}",
        )
        result = RoutedCompletion(
            response=response,
            decision_id=d.decision_id,
            model=d.action,
            probability=d.probability,
            outcome=outcome,
        )
        if error is not None:
            result.reward = self._send(d.decision_id, outcome, None)
            return result
        if quality is None and self.judge is not None:
            quality = self.judge(response)
        if quality is None:
            with self._lock:
                self._pending[d.decision_id] = outcome
                while len(self._pending) > self._max_pending:
                    self._pending.popitem(last=False)
        else:
            result.reward = self._send(d.decision_id, outcome, quality)
        return result

    # -- learning ----------------------------------------------------------

    def reward_for(self, outcome: Outcome, quality: Optional[float]) -> float:
        """The reward for an outcome; override to change the trade-off."""
        if not outcome.ok or quality is None:
            return self.failure_reward
        r = float(quality)
        if outcome.cost_usd is not None:
            r -= self.cost_weight * outcome.cost_usd
        r -= self.latency_weight * outcome.latency_s
        return r

    def _send(self, decision_id: str, outcome: Outcome, quality: Optional[float]) -> float:
        reward = self.reward_for(outcome, quality)
        detail = {
            "model": outcome.model,
            "latencyMs": round(outcome.latency_s * 1000, 3),
            "ok": outcome.ok,
        }
        if outcome.cost_usd is not None:
            detail["costUsd"] = outcome.cost_usd
        if quality is not None:
            detail["quality"] = quality
        if outcome.error:
            detail["error"] = outcome.error[:500]
        self.decider.reward(decision_id, reward, detail=detail)
        return reward

    def report_quality(self, decision_id: str, quality: float) -> float:
        """The quality of an earlier call's answer (a grader, a user rating,
        a downstream success signal). Returns the reward sent."""
        with self._lock:
            outcome = self._pending.pop(decision_id, None)
        if outcome is None:
            raise KeyError(f"no pending call for decision {decision_id!r}")
        return self._send(decision_id, outcome, quality)

    @property
    def pending(self) -> int:
        """Calls waiting for their quality."""
        with self._lock:
            return len(self._pending)
