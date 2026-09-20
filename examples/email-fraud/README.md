# Email fraud demo, without LLMs

**96 of the first 100 emails correct; 97.40–97.51% across 3,501 held-out emails.**
A local statistical text scorer supplies ten numeric features to Syntra, which
learns to choose the corpus's safe or phishing label from delayed feedback.
No LLM, embeddings API, hosted classifier, or API key is involved.

This is an email classification benchmark using a public labelled corpus,
not a validation of payment fraud detection or an email-security product.
The optional review mode holds uncertain predictions for a person; it does
not simulate a reviewer or count deferred cases as correct.

## Run it

From the repository root, with Python 3, Rust, and curl installed:

```bash
python3 scripts/demo-email-fraud.py
```

The runner downloads a pinned 52 MB CSV, checks its SHA-256, prepares disjoint
splits, builds the Rust example, and runs three predeclared seeds twice.
It also reruns with every test label inverted and requires identical prediction
digests, proving that held-out labels do not enter the decision path.
The default output is ignored `target/email-fraud-demo/`, including `results.json`.
Use `--csv /path/to/Phishing_Email.csv` to reuse the exact pinned download,
`--out /private/output` to choose a local directory, or `--no-build` after building.

Raw email text stays in that local download and process memory, and is never
committed here or sent to a model provider.
Only aggregate measurements are published in [results.json](results.json).
Treat the downloaded corpus as untrusted text; the runner never opens its
links or executes its contents.

## What makes the decision

```text
email text
  -> local naive Bayes score and simple text features
  -> Syntra LinUCB arm scores
  -> compiled Lycan AdaptiveChoice: safe | phishing
  -> optional low-margin review flag
```

The Python scorer fits a 20,000-term multinomial naive Bayes vocabulary on
8,750 emails, then emits its score plus length, unknown-word fraction, link,
credential, urgency, and payment-word indicators.
The feature definitions were fixed before evaluating test results.
The Rust runner trains two of Syntra's `LinUcbState` arms on a separate 3,500
emails, receiving only correct/incorrect reward for the action it selected,
with an imposed 32-decision feedback delay.
It uses regularization 1 and training exploration coefficient 1.

Arm scores feed a one-hot choice into the verified `(choice 0 1)` graph through
Syntra's `GraphExecutor`, matching the handoff used by the HTTP LinUCB path.
Evaluation freezes learning and uses coefficient 0 for greedy decisions.
This tests a fixed embedded LinUCB configuration, not the automatic meta-bandit
portfolio, HTTP service, durable decision journal, or production mail gateway.

The review threshold is the tenth percentile of prediction margins on a
separate 1,751-email calibration split, without using its labels.
A small score margin is an uncertainty heuristic, not a calibrated probability
or safety guarantee.
The threshold is frozen before evaluating the test set.

## Measured results

Recorded on 21 September 2026, Apple M5 Max, macOS, ARM64, release build,
seeds 7, 42, and 2026; all three use the same 3,501 test emails.

| Method | Correct / total | Accuracy | Automated coverage |
| --- | ---: | ---: | ---: |
| Majority label | 2,196 / 3,501 | 62.72% | 100% |
| Text scorer alone, 8,750 training emails | 3,390 / 3,501 | 96.83% | 100% |
| Naive Bayes reference, equal total training data | 3,393 / 3,501 | 96.92% | 100% |
| Text scorer + Syntra | 3,410–3,414 / 3,501 | 97.40–97.51% | 100% |
| Text scorer + Syntra, defer low-margin cases | 3,167–3,172 correct among 3,200–3,207 automated | 98.91–99.00% on automated cases | 91.40–91.60% |

The equal-data reference gets all 12,250 labels available across the two-stage
pipeline's scorer and policy training splits.
The observed improvement is small and on one corpus; it does not establish
broad superiority over statistical classifiers.
The three seeds are repeated policies on the same test set, not three
independent datasets.
For seed 7, full-coverage accuracy has a 95% Wilson interval of 96.94–97.98%.

At full coverage, the three policies miss **33–40 of 1,305 phishing emails**
and flag **51–54 of 2,196 safe emails**.
Review mode defers 294–301 emails and still has 11–14 missed phishing emails
among automated decisions; reviewers' final outcomes and costs are unmeasured.
The JSON includes confusion matrices, recall, precision, false-positive rate,
review counts by class, and uncertainty intervals.

The fixed first-100 showcase scores 96/100 for every Syntra seed and 95/100 for
the shared text scorer alone.
These 100 emails were selected by the fixed split order, not by searching for
a favorable batch.

## Timing and cost

For that 100-email batch, Python text scoring took **4.71 ms** and the embedded
Syntra decisions took about **0.04 ms total**.
Across all 3,501 emails, text scoring took 170.90 ms and Syntra decisions took
1.32–1.35 ms total, with decision p99 of 0.46–0.50 microseconds.
These are separately timed computation stages on already loaded, normalized
text and numeric features; they are **not end-to-end service latency**.
Downloads, normalization, training, CSV/JSON parsing, process startup, IPC,
HTTP, logging, durable writes, and human review are excluded.
The runner measures these stages anew on each machine, and reports maxima
and the count of decisions exceeding one millisecond.

There were zero LLM calls and $0 in external model API charges.
Local hardware, electricity, and review costs are not measured or claimed free.
The Jev post used a different dataset, model pipeline, and measurement boundary,
so these results do not constitute a head-to-head comparison.

## Dataset and limits

Source: [zefang-liu/phishing-email-dataset](https://huggingface.co/datasets/zefang-liu/phishing-email-dataset),
with LGPL-3.0 declared by its publisher.
The revision and byte digest are pinned in the runner and recorded in results;
this repository does not redistribute the corpus or a vocabulary learned from it.

The loader removes missing/empty rows, lowercases and collapses whitespace,
limits model input to 10,000 characters, and removes duplicate normalized
inputs before splitting; conflicting-label groups are excluded entirely.
It retains 17,502 unique records and uses a stratified 50/20/10/20 split.
This prevents exact normalized duplicates crossing splits, but does not prove
absence of near-duplicate campaigns or source-specific artifacts.
The public dataset's labels are accepted as provided, not independently audited.
This random split does not establish performance on future campaigns, new
organizations, adversarial messages, or different languages.

## Checks

```bash
python3 -m unittest discover -s examples/email-fraud -p 'test_*.py'
cargo test --release --locked --example email_fraud_decisions
```

These offline checks run in CI without downloading email data.
They cover split isolation, duplicate/conflict handling, bounded input,
frozen vocabulary, and correct accounting of review coverage and intervals.
The full runner additionally checks reproducibility and test-label isolation.
