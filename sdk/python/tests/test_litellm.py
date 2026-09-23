"""ModelRouter with LiteLLM's own functions: completion, acompletion,
completion_cost and its exception types.

Skipped unless litellm is installed. No provider is called and no key is
needed: ``mock_response`` makes LiteLLM return a genuine ``ModelResponse``
(or raise its real exception type) without network access, and
``LITELLM_LOCAL_MODEL_COST_MAP`` keeps its price table local.
"""

import asyncio
import importlib.util
import os
import unittest

os.environ.setdefault("LITELLM_LOCAL_MODEL_COST_MAP", "True")

HAVE_LITELLM = importlib.util.find_spec("litellm") is not None
if HAVE_LITELLM:
    import litellm

from test_syntra import Server  # the shared test server

from syntra.llm import ModelRouter

MODELS = {
    "openai/gpt-4o-mini": {"tier": "small"},
    "openai/gpt-4o": {"tier": "large"},
}
MESSAGES = [{"role": "user", "content": "Summarize: the cat sat on the mat."}]


@unittest.skipUnless(HAVE_LITELLM, "litellm is not installed")
class LiteLLMRouterTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv = Server()

    @classmethod
    def tearDownClass(cls):
        cls.srv.close()

    def router(self, capsule, completion, **kw):
        self.srv.client(capsule).put_spec({"actions": []})
        decider = self.srv.decider(capsule, sync_interval=None)
        self.addCleanup(decider.close)
        return ModelRouter(decider, MODELS, completion, **kw), decider

    def stored_reward(self, capsule, decision_id):
        return self.srv.client(capsule).decision(decision_id)["rewards"][0]

    def test_completion_and_cost(self):
        def completion(model, messages, **kwargs):
            return litellm.completion(model=model, messages=messages, mock_response="The cat sat.", **kwargs)

        router, decider = self.router(
            "sync",
            completion,
            cost=lambda response: litellm.completion_cost(completion_response=response),
            cost_weight=100.0,
        )
        r = router.completion(MESSAGES, context={"task": "summarize"}, quality=0.8)
        self.assertIsInstance(r.response, litellm.ModelResponse)
        self.assertEqual(r.response.choices[0].message.content, "The cat sat.")
        self.assertIn(r.model, MODELS)
        # LiteLLM prices the mock call from its local table: a real cost,
        # small but not zero, and the reward subtracts it.
        self.assertIsNotNone(r.outcome.cost_usd)
        self.assertGreater(r.outcome.cost_usd, 0.0)
        self.assertLess(r.outcome.cost_usd, 0.01)
        expected = router.reward_for(r.outcome, 0.8)
        self.assertAlmostEqual(r.reward, expected)
        report = decider.flush()
        self.assertEqual(report["decisions_accepted"], 1, report)
        self.assertEqual(report["rewards_applied"], 1, report)
        stored = self.stored_reward("sync", r.decision_id)
        self.assertAlmostEqual(stored["reward"], expected)
        self.assertEqual(stored["detail"]["model"], r.model)

    def test_acompletion(self):
        async def acompletion(model, messages, **kwargs):
            return await litellm.acompletion(model=model, messages=messages, mock_response="ok", **kwargs)

        router, decider = self.router("async", acompletion)

        async def run():
            return await router.acompletion(MESSAGES, quality=1.0)

        r = asyncio.run(run())
        self.assertIsInstance(r.response, litellm.ModelResponse)
        self.assertEqual(r.response.choices[0].message.content, "ok")
        self.assertTrue(r.outcome.ok)
        self.assertEqual(decider.flush()["rewards_applied"], 1)

    def test_provider_errors_are_rewarded_and_raised(self):
        def completion(model, messages, **kwargs):
            # LiteLLM raises its RateLimitError for this mock.
            return litellm.completion(
                model=model, messages=messages, mock_response="litellm.RateLimitError", **kwargs
            )

        router, decider = self.router("errors", completion, failure_reward=-1.0)
        with self.assertRaises(litellm.RateLimitError):
            router.completion(MESSAGES)
        self.assertEqual(decider.flush()["rewards_applied"], 1)
        listed = self.srv.client("errors")._call("GET", "decisions?limit=10")["decisions"]
        stored = self.stored_reward("errors", listed[0]["decisionId"])
        self.assertEqual(stored["reward"], -1.0)
        self.assertFalse(stored["detail"]["ok"])
        self.assertIn("RateLimitError", stored["detail"]["error"])


if __name__ == "__main__":
    unittest.main()
