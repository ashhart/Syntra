# Repo-read eval harness

This directory tests whether an LLM can read this repository and produce the
intended first-pass understanding without drifting into stale-cache summaries,
README-only skims, or "let me read more first" stalls.

The harness does not call model APIs. It gives humans or CI a stable prompt set
and a local scorer for model answers.

## Cases

The prompts live in [`cases.jsonl`](cases.jsonl). Each case should be run
against a model as written, then the model answer should be saved or piped into
the scorer.

Current cases:

- `repo-root-only` - start from the GitHub repo root.
- `raw-context-only` - start from raw `CONTEXT.md`.
- `stale-cache-recovery` - recover when the rendered GitHub root is stale.
- `demo-inspection-accountability` - force the model to distinguish described
  demos from inspected demo files.

## Pass criteria

A passing first-pass summary must:

- identify Syntra as a self-hosted adaptive decision runtime
- include the primitive: context -> compiled capsule decision -> action ->
  delayed feedback -> persistent learned policy -> audit trail
- mention LLM routing, offline eval, A/B validation, anomaly-aware API routing,
  pandemic / COVID-style policy scoring, edge-of-chaos detection, and live Mars
  mission planning
- keep the demo scope limits next to the claims: the pandemic demo is a
  synthetic policy-scoring benchmark, the Mars demo is standard orbital
  mechanics plus ranked decisioning, and the proof lab refuses the open
  asymptotic
- distinguish product/adoption demos from Lycan substrate demos
- avoid reducing Syntra to an API gateway, feature-flag service, HTTP appliance,
  or generic contextual bandit wrapper
- state which files were read or whether the answer is based only on docs

## Score an answer

```bash
python3 evals/repo-read/score_answer.py path/to/model-answer.md
```

or:

```bash
pbpaste | python3 evals/repo-read/score_answer.py
```

The scorer is intentionally simple. It is a regression tripwire, not a judge of
writing quality.

## Migration note

This harness lives in Syntra while Syntra is the first meaningful adopter. Once
there are two or three adopter repos, move the harness into the `context-md`
project and run it across multiple repos and models. At that point it becomes
evidence for the `CONTEXT.md` convention, not just Syntra-specific docs QA.

## Context reviewer-notes check

CI runs [`check_context_notes.py`](check_context_notes.py). It checks only
`CONTEXT.md`, because `CONTEXT.md` is the reviewer guide. Other files can link
to it, but they should not become parallel standards.
