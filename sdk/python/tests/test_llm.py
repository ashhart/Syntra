"""ModelRouter against a real server, with simulated models.

The three "models" below are stand-ins with made-up quality, cost and
latency profiles; the point is that the router learns which one is worth
it per task, not how any real model behaves.
"""

import asyncio
import random
import sys
import time
import unittest

from test_syntra import Server  # the shared test server

from syntra.llm import ModelRouter, prompt_features

MODELS = {
    "small": {"tier": "small", "costPer1M": 0.15},
    "medium": {"tier": "medium", "costPer1M": 1.0},
    "large": {"tier": "large", "costPer1M": 10.0},
}
# Simulated answer quality by task, and cost per call in USD.
QUALITY = {
    "chat": {"small": 0.85, "medium": 0.86, "large": 0.88},
    "code": {"small": 0.30, "medium": 0.60, "large": 0.95},
}
COST = {"small": 0.0002, "medium": 0.002, "large": 0.02}


class FakeResponse:
    def __init__(self, model, task):
        self.model = model
        self.task = task
        self.text = f"{model} answering a {task} request"


def fake_completion(model, messages, **kwargs):
    task = kwargs.pop("task_hint")
    return FakeResponse(model, task)


def quality_of(response, rng):
    return QUALITY[response.task][response.model] + rng.uniform(-0.03, 0.03)


def messages_for(task):
    if task == "code":
        return [{"role": "user", "content": "```python\ndef f(x):\n    return x\n```\nwhy is this slow?"}]
    return [{"role": "user", "content": "hi! what's a good name for a cat?"}]


class ModelRouterTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv = Server()

    @classmethod
    def tearDownClass(cls):
        cls.srv.close()

    def router(self, capsule, **kw):
        self.srv.client(capsule).put_spec({"actions": [], "exploration": {"kind": "epsilonGreedy", "epsilon": 0.2}})
        decider = self.srv.decider(capsule, sync_interval=None)
        self.addCleanup(decider.close)
        router = ModelRouter(
            decider,
            MODELS,
            fake_completion,
            cost=lambda r: COST[r.model],
            cost_weight=10.0,
            **kw,
        )
        return router, decider

    def test_learns_the_model_worth_paying_for(self):
        router, decider = self.router("learn")
        rng = random.Random(7)
        for i in range(600):
            task = "code" if i % 2 else "chat"
            r = router.completion(messages_for(task), context={"task": task}, task_hint=task)
            router.report_quality(r.decision_id, quality_of(r.response, rng))
            if i % 100 == 99:
                decider.flush()
                time.sleep(1.05)  # a new model publishes at most once a second
                decider.sync()
        self.assertEqual(router.pending, 0)
        picks = {"chat": {}, "code": {}}
        for i in range(200):
            task = "code" if i % 2 else "chat"
            d = router.choose(messages_for(task), context={"task": task})
            picks[task][d.action] = picks[task].get(d.action, 0) + 1
        print(f"\n  learned picks: {picks}", file=sys.stderr)
        # Code is worth the large model; chat is not (small and medium are
        # within noise of each other there, 0.848 vs 0.840 net).
        self.assertGreater(picks["code"].get("large", 0), 60, picks)
        cheap = picks["chat"].get("small", 0) + picks["chat"].get("medium", 0)
        self.assertGreater(cheap, 70, picks)
        self.assertLess(picks["chat"].get("large", 0), 30, picks)

    def test_quality_now_later_or_judged(self):
        router, decider = self.router("paths")
        r = router.completion(messages_for("chat"), quality=0.9, task_hint="chat")
        self.assertAlmostEqual(r.reward, 0.9 - 10.0 * COST[r.model])
        r2 = router.completion(messages_for("chat"), task_hint="chat")
        self.assertIsNone(r2.reward)
        self.assertEqual(router.pending, 1)
        self.assertAlmostEqual(router.report_quality(r2.decision_id, 0.5), 0.5 - 10.0 * COST[r2.model])
        with self.assertRaises(KeyError):
            router.report_quality(r2.decision_id, 0.5)
        judged, _ = self.router("judged", judge=lambda resp: 1.0)
        r3 = judged.completion(messages_for("code"), task_hint="code")
        self.assertAlmostEqual(r3.reward, 1.0 - 10.0 * COST[r3.model])
        self.assertEqual(decider.flush()["rewards_applied"], 2)

    def test_failures_are_rewarded_and_raised(self):
        def broken(model, messages, **kwargs):
            raise TimeoutError("upstream timed out")

        self.srv.client("fail").put_spec({"actions": []})
        decider = self.srv.decider("fail", sync_interval=None)
        self.addCleanup(decider.close)
        router = ModelRouter(decider, MODELS, broken, failure_reward=-1.0)
        with self.assertRaises(TimeoutError):
            router.completion(messages_for("chat"))
        report = decider.flush()
        self.assertEqual(report["rewards_applied"], 1, report)
        client = self.srv.client("fail")
        listed = client._call("GET", "decisions?limit=10")["decisions"]
        stored = client.decision(listed[0]["decisionId"])
        self.assertEqual(stored["rewards"][0]["reward"], -1.0)
        self.assertEqual(stored["rewards"][0]["detail"]["ok"], False)
        self.assertIn("TimeoutError", stored["rewards"][0]["detail"]["error"])

    def test_async_completion(self):
        async def acompletion(model, messages, **kwargs):
            await asyncio.sleep(0)
            return FakeResponse(model, kwargs["task_hint"])

        self.srv.client("async").put_spec({"actions": []})
        decider = self.srv.decider("async", sync_interval=None)
        self.addCleanup(decider.close)
        router = ModelRouter(decider, MODELS, acompletion, cost=lambda r: COST[r.model])
        r = asyncio.run(router.acompletion(messages_for("code"), quality=0.8, task_hint="code"))
        self.assertIn(r.model, MODELS)
        self.assertIsNotNone(r.reward)

    def test_http_client_as_decider_and_allowed_models(self):
        self.srv.client("http").put_spec({"actions": []})
        router = ModelRouter(self.srv.client("http"), MODELS, fake_completion)
        for _ in range(20):
            r = router.completion(messages_for("chat"), allowed=["small", "medium"], quality=0.7, task_hint="chat")
            self.assertIn(r.model, ("small", "medium"))
        with self.assertRaises(ValueError):
            router.choose(messages_for("chat"), allowed=["nope"])

    def test_prompt_features(self):
        f = prompt_features(messages_for("code"), tools=[{"type": "function"}])
        self.assertTrue(f["looksLikeCode"])
        self.assertEqual(f["tools"], 1)
        self.assertEqual(f["turns"], 1)
        self.assertGreater(f["promptTokens"], 5)


if __name__ == "__main__":
    unittest.main()
