# Improving detection without LLMs

The optional phrase-aware candidate makes fewer total mistakes, but misses more
risky emails on a separate corpus. It fails the promotion gate and does not
replace the original demo's default classifier.

The input loader now rejects missing text before normalization instead of
crashing. The candidate adds word pairs, TF-IDF and logistic regression to the
local scorer, broader training data, and an explicit review flag for scorer
disagreement, very short messages and mostly unfamiliar vocabulary. Syntra
still selects the action through the compiled Lycan graph; there are no LLM
calls, hosted embeddings, model API charges or API keys.

## Reproduce the experiment

Use Python 3.11 or newer, Rust and curl, from the repository root:

```bash
python3 -m venv .venv
.venv/bin/pip install -r examples/email-fraud/requirements.txt
.venv/bin/python scripts/demo-email-fraud-eval.py --require-promotion
```

This downloads about 262 MB of pinned public CSVs and writes local artifacts
under ignored `target/email-fraud-evaluation/`. The published candidate is
expected to exit **2** after writing `results.json`, because the missed-risk
check fails. Omit `--require-promotion` to collect research results without a
failing exit status. Neither mode changes the default model or deploys anything.
Raw emails, learned vocabulary and numeric feature caches stay outside Git.

The [aggregate results](evaluation-results.json) contain the measured confusion
matrices, review coverage, seeds, source hashes, dataset hashes and timings.
They do not contain email text or vocabulary.

## What changed in the experiment

The original dataset uses separate scorer, policy, development and test splits.
The original test results had already been observed, so subsequent measurements
on those emails are regression checks, not new untouched test evidence.
Three scorer candidates were compared on development data only; the word
TF-IDF logistic model had the lowest mean Syntra error count across three seeds.

That model cut original regression errors to 41 to 43, but raised errors on the
CEAS corpus to 3,946 to 3,986 versus the original policy's 2,286 to 2,336.
We rejected that configuration. CEAS then became additional training and
development data, with a separate regression split. A TREC07 diagnostic exposed
another missed-risk regression. A risky-action score bias was selected using
combined development data only, with a cost of three per missed risky email
and one per false alarm. The selected bias was 0.2.

The final configuration was frozen before the first TREC06 evaluation.
Normalized body matches against every previously used corpus, including
TREC07, were excluded independently of labels. Repeating that evaluation now
checks reproducibility; it is not another fresh test. Token normalization
catches some punctuation changes, but does not eliminate all campaign overlap.

## Final results

Seeds 7, 42 and 2026 share the same messages. Ranges represent different policy
seeds, not independent samples or confidence intervals. The original scorer
uses its original training set; the candidate uses additional CEAS training.
These numbers compare complete configurations, not an isolated model change.

| Evaluation | Emails | Original errors | Candidate errors | Original missed risk | Candidate missed risk |
| --- | ---: | ---: | ---: | ---: | ---: |
| Original regression | 3,501 | 87 to 91 | 61 to 62 | 33 to 40 | 26 to 33 |
| CEAS regression | 7,671 | 482 to 488 | 57 to 66 | See JSON | 5 to 6 |
| Separate TREC06 corpus | 16,303 | 1,226 to 1,273 | 1,044 to 1,053 | 382 to 415 | 547 to 600 |

On TREC06, original accuracy is 92.19% to 92.48% and candidate accuracy is 93.54% to 93.60%,
but it misses more risky messages. That is why accuracy alone cannot approve a
fraud policy. The promotion check compares the worst candidate seed with the
best original seed for accuracy, missed risk and false positives on each
corpus, and rejects any regression.

With review enabled, the candidate is 99.39% to 99.41% accurate on the automated
62.38% to 63.06% of TREC06 messages. The remaining roughly 37% require review;
they are not counted as correct, and reviewer accuracy and cost are unmeasured.
The original policy automates about 76% to 77% at 97.57% to 97.64% accuracy.
The JSON includes a stronger equal-data phrase scorer without Syntra, which
beats the candidate's full-coverage accuracy on CEAS. These results do not
establish that Syntra is universally better than a standalone classifier.

## Data and boundaries

The additional corpora come from the primary publisher's
[Phishing Email Curated Datasets](https://zenodo.org/records/8339691).
They contain historical email and spam labels; TREC06 is derived from 2006
material, not a test of current phishing campaigns. Only message bodies enter
the scorer, with no sender, recipient, subject, timestamp or dataset identity.
Labels are accepted as supplied, without independent fraud adjudication.

Both Python scoring and embedded Rust decisions are timed separately.
HTTP, loading, parsing, training, persistence and human review are excluded;
the measurements are not end-to-end latency. No Jev comparison is claimed
because its data and measurement boundary are different.

CI runs offline tests for missing text, split and vocabulary isolation,
review accounting and the promotion check, without downloading real emails.
The full evaluation repeats predictions in a separate process and inverts
all test labels to verify that labels do not influence the decisions.
