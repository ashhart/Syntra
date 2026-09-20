#!/usr/bin/env python3
"""LLM-free email classification using a statistical text scorer and Syntra.

Downloads a pinned public labelled corpus into ignored local output, removes
normalized duplicates BEFORE splitting, fits text statistics on one split,
trains Syntra on another, calibrates review coverage on a third, and freezes
all learned state before testing. No email content is sent to a model API.
"""
import argparse
from collections import Counter
import csv
import hashlib
import json
import math
from pathlib import Path
import platform
import random
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
REVISION = '34085a032c123ca237f314a01a67909cdea35e34'
SOURCE = 'https://huggingface.co/datasets/zefang-liu/phishing-email-dataset'
URL = f'{SOURCE}/resolve/{REVISION}/Phishing_Email.csv'
SHA256 = '18ef4fff1acb8986f0ab01e83ede6025420c829656af5a377560fce17e153b97'
MAX_CHARS = 10000
TOKEN = re.compile(r'[a-z]{2,30}')


def normalized(text):
    # Same bounded representation for duplicate detection and both classifiers.
    return ' '.join(text.lower().split())[:MAX_CHARS]


def clean_rows(raw):
    unique = {}
    invalid = duplicates = 0
    conflicts = set()
    for text, label in raw:
        text = normalized(text)
        if label not in (0, 1) or text in ('', 'empty', 'nan'):
            invalid += 1
            continue
        key = hashlib.sha256(text.encode()).hexdigest()
        if key in unique:
            duplicates += 1
            if unique[key][1] != label:
                conflicts.add(key)
        else:
            unique[key] = (text, label, key)
    rows = [r for k, r in sorted(unique.items()) if k not in conflicts]
    return rows, {'invalid_rows': invalid, 'duplicate_rows': duplicates,
                  'conflicting_groups_removed': len(conflicts), 'unique_rows': len(rows)}


def split_rows(rows):
    # Stratified 50/20/10/20 split; fixed seed, fixed ordering, no test selection.
    groups = [[], [], [], []]
    rng = random.Random(20260920)
    for label in (0, 1):
        selected = [r for r in rows if r[1] == label]
        rng.shuffle(selected)
        n = len(selected)
        edges = [0, n // 2, n * 7 // 10, n * 8 // 10, n]
        for i in range(4):
            groups[i].extend(selected[edges[i]:edges[i + 1]])
    for group in groups:
        rng.shuffle(group)
    keys = [{r[2] for r in group} for group in groups]
    assert sum(map(len, keys)) == len(set.union(*keys))
    return groups


class TextScorer:
    """Laplace-smoothed multinomial naive Bayes, training-only vocabulary."""
    def __init__(self, rows):
        counts = [Counter(), Counter()]
        documents = [0, 0]
        for text, label, _ in rows:
            counts[label].update(TOKEN.findall(text))
            documents[label] += 1
        all_terms = counts[0] + counts[1]
        self.vocabulary = {w for w, _ in sorted(all_terms.items(), key=lambda x: (-x[1], x[0]))[:20000]}
        totals = [sum(c[w] for w in self.vocabulary) + len(self.vocabulary) for c in counts]
        self.odds = {w: math.log((counts[1][w] + 1) / totals[1]) -
                         math.log((counts[0][w] + 1) / totals[0]) for w in self.vocabulary}
        self.prior = math.log((documents[1] + 1) / (documents[0] + 1))

    def encode(self, text):
        terms = TOKEN.findall(text)
        counts = Counter(terms)
        log_odds = self.prior + sum(n * self.odds.get(w, 0.) for w, n in counts.items())
        margin = math.tanh(log_odds / 20.)
        known = sum(n for w, n in counts.items() if w in self.vocabulary)
        # Predeclared features; neither a label nor row ID enters the decision.
        x = [1., margin, margin * margin, margin ** 3,
             min(math.log1p(len(terms)) / 10., 1.), 1. - known / max(1, len(terms)),
             float('http' in text or 'www.' in text),
             float(any(w in counts for w in ('password', 'verify', 'login', 'account'))),
             float(any(w in counts for w in ('urgent', 'suspended', 'immediately'))),
             float(any(w in counts for w in ('payment', 'invoice', 'bank', 'transfer')))]
        return x, int(log_odds > 0.)


def quality_only(result):
    return {'baselines': result['baselines'], 'runs': [
        {k: v for k, v in run.items() if 'latency' not in k} for run in result['runs']]}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--out', type=Path, default=ROOT / 'target/email-fraud-demo')
    p.add_argument('--csv', type=Path, help='Existing copy of the exact pinned CSV')
    p.add_argument('--no-build', action='store_true')
    args = p.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    source = args.csv or args.out / 'emails.csv'
    if not source.exists():
        # curl uses the system certificate store; never disable TLS verification.
        subprocess.run(['curl', '--fail', '--location', '--silent', '--show-error',
                        '--max-time', '180', '--max-filesize', '60000000',
                        URL, '--output', str(source)], check=True)
    if source.stat().st_size > 60000000 or hashlib.sha256(source.read_bytes()).hexdigest() != SHA256:
        raise ValueError('Dataset digest mismatch; refusing to benchmark different bytes')
    csv.field_size_limit(60000000)
    with source.open(newline='', encoding='utf-8') as f:
        raw = [(r['Email Text'], {'Safe Email': 0, 'Phishing Email': 1}.get(r['Email Type']))
               for r in csv.DictReader(f)]
    rows, cleaning = clean_rows(raw)
    scorer_rows, train, calibration, test = split_rows(rows)
    print(f"Cleaned {len(rows)} unique emails; scorer/policy/calibration/test sizes "
          f"{len(scorer_rows)}/{len(train)}/{len(calibration)}/{len(test)}", flush=True)
    start = time.perf_counter()
    scorer = TextScorer(scorer_rows)
    fit_s = time.perf_counter() - start
    # A second reference sees every label available to the two-stage pipeline.
    reference = TextScorer(scorer_rows + train)
    encoded = {}; feature_times = []
    for name, group in [('train', train), ('calibration', calibration), ('test', test)]:
        encoded[name] = []
        for text, label, _ in group:
            start = time.perf_counter_ns()
            x, baseline = scorer.encode(text)
            elapsed = time.perf_counter_ns() - start
            encoded[name].append({'x': x, 'y': label, 'baseline': baseline,
                                  'reference': reference.encode(text)[1]})
            if name == 'test':
                feature_times.append(elapsed)
    features = args.out / 'features.json'
    features.write_text(json.dumps(encoded))
    # Private audit trail, no raw messages in the numeric input or result report.
    (args.out / 'split-audit.json').write_text(json.dumps({name: [r[2] for r in group]
        for name, group in zip(('scorer', 'policy', 'calibration', 'test'),
                              (scorer_rows, train, calibration, test))}, indent=2))
    if not args.no_build:
        subprocess.run(['cargo', 'build', '--release', '--locked', '--example', 'email_fraud_decisions'], cwd=ROOT, check=True)
    command = [str(ROOT / 'target/release/examples/email_fraud_decisions'), str(features)]
    first = json.loads(subprocess.check_output(command))
    second = json.loads(subprocess.check_output(command))
    if quality_only(first) != quality_only(second):
        raise AssertionError('Identical seeds did not reproduce predictions')
    # Strong leakage check: changing held-out labels cannot change predictions.
    changed = json.loads(features.read_text())
    for row in changed['test']:
        row['y'] = 1 - row['y']
    permuted = args.out / 'test-label-inversion.json'
    permuted.write_text(json.dumps(changed))
    inverted = json.loads(subprocess.check_output([command[0], str(permuted)]))
    if [r['prediction_digest'] for r in first['runs']] != [r['prediction_digest'] for r in inverted['runs']]:
        raise AssertionError('Held-out labels affected decisions')
    first['dataset'] = {'source': SOURCE, 'revision': REVISION, 'sha256': SHA256,
        'source_license': 'LGPL-3.0 as declared by dataset publisher; raw corpus not redistributed',
        'cleaning': cleaning, 'split_seed': 20260920,
        'split_counts': dict(zip(('text_scorer', 'policy_train', 'review_calibration', 'held_out'),
                                map(len, (scorer_rows, train, calibration, test)))),
        'test_order_sha256': hashlib.sha256(''.join(r[2] for r in test).encode()).hexdigest()}
    first['text_features'] = {'model': 'multinomial naive Bayes, 20000 training-only terms',
        'max_normalized_characters': MAX_CHARS, 'fit_seconds': fit_s,
        'held_out_scoring_ms': sum(feature_times) / 1e6,
        'first100_scoring_ms': sum(feature_times[:100]) / 1e6}
    first['checks'] = {'split_overlap': 0, 'identical_predictions_two_processes': True,
                       'test_label_inversion_preserves_predictions': True}
    first['cost'] = {'llm_calls': 0, 'external_model_api_spend_usd': 0,
                     'local_compute_energy_and_review_cost': 'not measured'}
    first['scope'] = {'task': 'public email corpus classification, not financial fraud validation',
        'policy': 'fixed LinUCB plus compiled AdaptiveChoice; not automatic meta-bandit HTTP service',
        'timing': 'text scoring and embedded scoring+graph execution measured separately; excludes download, training, parsing, IPC, HTTP, journal and durable writes',
        'review': 'abstention only; no human outcomes simulated or counted correct',
        'comparison': 'not the same corpus, hardware or models as the Jev social-media result'}
    first['environment'] = {'os': platform.system(), 'arch': platform.machine(),
        'python': platform.python_version(), 'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip()}
    (args.out / 'results.json').write_text(json.dumps(first, indent=2) + '\n')
    b = first['baselines']['naive_bayes']
    print(f"Statistical scorer baseline: {b['correct']}/{b['samples']} = {b['accuracy']:.2%}")
    equal = first['baselines']['naive_bayes_equal_data']
    print(f"Equal-data naive Bayes reference: {equal['correct']}/{equal['samples']} = {equal['accuracy']:.2%}")
    for run in first['runs']:
        full = run['full']; hundred = run['first100']; review = run['with_review']
        print(f"Syntra seed {run['seed']}: {full['accuracy']:.2%} on {full['samples']}; "
              f"first100={hundred['correct']}/100; p99={run['decision_latency']['p99_us']:.2f}us; "
              f"review={review['review']}, automated accuracy={review['accuracy']:.2%}")
    print('No LLM calls; no raw email content in results.json; reproducibility and leakage checks passed.')
    print(f'Report: {args.out / "results.json"}')


if __name__ == '__main__':
    main()
