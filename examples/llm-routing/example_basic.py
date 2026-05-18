"""Minimal end-to-end example for the syntra_llm domain pack.

Prerequisites:
  • Syntra running (e.g. `docker run -p 8787:8787 syntra:demo` or bare metal).
  • LLM routing capsule installed: `python setup_capsule.py`.
  • Env vars set: SYNTRA_ADMIN_KEY, optionally SYNTRA_URL and
    SYNTRA_CAPSULE_PATH.

If OPENAI_API_KEY is set the example makes real OpenAI calls; otherwise it
simulates LLM responses with random synthetic quality/latency/cost values so
you can exercise the Syntra feedback loop without an API key.
"""
from __future__ import annotations

import os
import random
import time

from syntra_llm import LLMRouter


# Approximate cost (USD) and expected latency (ms) per model — used for
# synthetic simulation when no real API key is present.
_SYNTHETIC_MODEL_PARAMS: dict[str, dict] = {
    "cheap_fast":         {"cost": 0.002, "latency_ms": 400,  "quality": 0.65},
    "balanced":           {"cost": 0.015, "latency_ms": 900,  "quality": 0.82},
    "expensive_accurate": {"cost": 0.080, "latency_ms": 2000, "quality": 0.95},
}


def _synthetic_call(model_name: str, prompt: str) -> tuple[str, float, float, float]:
    """Simulate an LLM call. Returns (response_text, quality, latency_ms, cost_usd)."""
    params = _SYNTHETIC_MODEL_PARAMS.get(
        model_name, _SYNTHETIC_MODEL_PARAMS["balanced"]
    )
    # Add realistic jitter.
    latency_ms = params["latency_ms"] * random.uniform(0.8, 1.4)
    time.sleep(latency_ms / 1000.0)
    quality = min(1.0, max(0.0, params["quality"] + random.gauss(0, 0.05)))
    cost_usd = params["cost"] * random.uniform(0.9, 1.1)
    response_text = f"[synthetic {model_name}] answer to: {prompt[:40]}"
    return response_text, quality, latency_ms, cost_usd


def _openai_call(model_name: str, prompt: str, api_key: str) -> tuple[str, float, float, float]:
    """Make a real OpenAI call. Maps syntra model names to OpenAI model IDs."""
    import urllib.request
    import urllib.error
    import json

    model_map = {
        "cheap_fast":         "gpt-3.5-turbo",
        "balanced":           "gpt-4o-mini",
        "expensive_accurate": "gpt-4o",
    }
    openai_model = model_map.get(model_name, "gpt-4o-mini")

    payload = json.dumps({
        "model": openai_model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 128,
    }).encode()

    req = urllib.request.Request(
        "https://api.openai.com/v1/chat/completions",
        data=payload,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
    )
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.loads(resp.read())
    latency_ms = (time.time() - t0) * 1000.0

    response_text = data["choices"][0]["message"]["content"]
    # Approximate cost from token counts (rough heuristic, not billing).
    usage = data.get("usage", {})
    total_tokens = usage.get("total_tokens", 200)
    price_per_token = {"gpt-3.5-turbo": 2e-6, "gpt-4o-mini": 3e-7, "gpt-4o": 5e-6}
    cost_usd = total_tokens * price_per_token.get(openai_model, 5e-6)

    # Quality is hard to measure automatically — use a proxy (response length).
    quality = min(1.0, len(response_text) / 300.0)
    return response_text, quality, latency_ms, cost_usd


def main() -> None:
    syntra_url = os.environ.get("SYNTRA_URL", "http://localhost:8787")
    admin_key = os.environ["SYNTRA_ADMIN_KEY"]
    capsule_path = os.environ.get(
        "SYNTRA_CAPSULE_PATH",
        "/tenants/myteam/jobs/llm/capsules/router",
    )
    openai_key = os.environ.get("OPENAI_API_KEY")

    router = LLMRouter(
        syntra_url=syntra_url,
        capsule_path=capsule_path,
        admin_key=admin_key,
    )

    prompts = [
        ("What is 2+2?", 0.1, "free"),
        ("Summarize the history of the Roman Empire.", 0.6, "pro"),
        ("Write a detailed legal analysis of GDPR Article 17.", 0.95, "enterprise"),
        ("Translate 'hello' to Spanish.", 0.05, "free"),
        ("Explain quantum entanglement to a PhD physicist.", 0.9, "enterprise"),
    ]

    for prompt, complexity, tier in prompts:
        token_count = len(prompt.split()) * 4  # rough token estimate

        decision = router.choose(
            prompt_token_count=token_count,
            task_complexity=complexity,
            customer_tier=tier,
        )

        print(
            f"tier={tier:10s} complexity={complexity:.2f} "
            f"tokens={token_count:4d} -> model={decision.model_name}"
        )

        if openai_key:
            response_text, quality, latency_ms, cost_usd = _openai_call(
                decision.model_name, prompt, openai_key
            )
        else:
            response_text, quality, latency_ms, cost_usd = _synthetic_call(
                decision.model_name, prompt
            )

        print(
            f"  quality={quality:.2f} latency={latency_ms:.0f}ms "
            f"cost=${cost_usd:.4f}  response: {response_text[:60]}"
        )

        router.report(
            decision_id=decision.decision_id,
            model_name=decision.model_name,
            quality=quality,
            latency_ms=latency_ms,
            cost_usd=cost_usd,
        )


if __name__ == "__main__":
    main()
