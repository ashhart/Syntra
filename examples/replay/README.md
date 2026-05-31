# Syntra replay promotion gate

This example turns shadow-mode or historical decision logs into a promotion
artifact.

```bash
syntra replay \
  --events examples/replay/decisions.jsonl \
  --policy-json examples/replay/candidate-policy.json \
  --gates examples/replay/promotion.yaml \
  --format markdown \
  --out promotion-report.md \
  --fail-on-gate
```

The report answers the production question: did the candidate policy beat the
baseline enough, without increasing cost, latency, or hurting a segment?

Use this before promoting an adaptive capsule out of shadow mode.
