#!/usr/bin/env python3
"""Evaluate the phrase-aware LLM-free email policy on development and external data.

Run with the optional pinned requirements in examples/email-fraud/requirements.txt.
Raw datasets, model vocabulary and feature caches are kept outside tracked files.
"""
import argparse
import csv
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import platform
import subprocess
import time

from email_fraud_model import PhraseScorer

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('legacy_email', ROOT / 'scripts/demo-email-fraud.py')
legacy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(legacy)
TRANSFER_URL = 'https://zenodo.org/records/8339691/files/CEAS_08.csv'
DIAGNOSTIC_URL = 'https://zenodo.org/records/8339691/files/TREC_07.csv'
EXTERNAL_URL = 'https://zenodo.org/records/8339691/files/TREC_06.csv'
TRANSFER_SHA256 = '22375e7d5f5a8229dbe987914ee9b3705656c590038662a7df6054629b376074'
DIAGNOSTIC_SHA256 = '48c79ed8bd173587a73d9171d1c315229c44b7ee201f83a95883feed241d6c4b'
EXTERNAL_SHA256 = '8ca6b1249f30f0a74790b8a34d4dbe248d526cfb0b13227b41970a1ebd638be5'


def canonical(text):
    # Label-independent extra overlap filter, beyond exact normalized strings.
    return ' '.join(re.findall(r'[a-z0-9]+', legacy.normalized(text)))


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def exclude_seen(rows, known):
    blocked = {digest(canonical(r[0])) for r in known}
    return [r for r in rows if digest(canonical(r[0])) not in blocked]


def fetch(path, url, expected, limit):
    if not path.exists():
        subprocess.run(['curl', '-fsSL', '--max-time', '180', '--max-filesize', str(limit),
                        url, '-o', str(path)], check=True)
    if path.stat().st_size > limit or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise ValueError('Dataset digest mismatch')


def read_primary(path):
    with path.open(newline='', encoding='utf-8') as f:
        return legacy.clean_rows((r['Email Text'], {'Safe Email': 0, 'Phishing Email': 1}.get(r['Email Type']))
                                 for r in csv.DictReader(f))


def encode(group, scorer, reference=None):
    texts = [r[0] for r in group]
    start = time.perf_counter_ns()
    values = scorer.encode_batch(texts)
    elapsed = (time.perf_counter_ns() - start) / 1e6
    references = reference.encode_batch(texts) if reference else values
    return [{'x': x, 'y': row[1], 'baseline': a, 'reference': ref[1], 'review_guard': guard}
            for row, (x, a, guard), ref in zip(group, values, references)], elapsed


def old_encode(group, scorer):
    out = []
    for text, label, _ in group:
        x, a = scorer.encode(text)
        out.append({'x': x, 'y': label, 'baseline': a, 'reference': a})
    return out


def execute(path, data, phrase=False):
    path.write_text(json.dumps(data))
    command = [str(ROOT / 'target/release/examples/email_fraud_decisions'), str(path)]
    result = json.loads(subprocess.check_output(command))
    if phrase:
        b = result['baselines']
        b['phrase_scorer'] = b.pop('naive_bayes')
        b['phrase_scorer_first100'] = b.pop('naive_bayes_first100')
        b['phrase_scorer_equal_data'] = b.pop('naive_bayes_equal_data')
    return result


def errors(result):
    return [r['full']['samples'] - r['full']['correct'] for r in result['runs']]


def promotion_checks(results):
    promotion = {}
    for name, evaluation in results.items():
        before = [r['full'] for r in evaluation['old_policy']['runs']]
        after = [r['full'] for r in evaluation['new_policy']['runs']]
        promotion[name] = {
            'accuracy_non_regression': min(r['accuracy'] for r in after) >= max(r['accuracy'] for r in before),
            'missed_risk_non_regression': max(r['confusion_matrix'][1][0] for r in after) <= min(r['confusion_matrix'][1][0] for r in before),
            'false_positive_non_regression': max(r['confusion_matrix'][0][1] for r in after) <= min(r['confusion_matrix'][0][1] for r in before)}
    return promotion


def enforce_promotion(promotion, required):
    if required and not promotion['passed']:
        raise SystemExit(2)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--out', type=Path, default=ROOT / 'target/email-fraud-evaluation')
    p.add_argument('--csv', type=Path)
    p.add_argument('--transfer-csv', type=Path)
    p.add_argument('--diagnostic-csv', type=Path)
    p.add_argument('--external-csv', type=Path)
    p.add_argument('--no-build', action='store_true')
    p.add_argument('--require-promotion', action='store_true',
                   help='exit 2 after saving results if any accuracy, missed-risk or false-positive check regresses')
    args = p.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    csv.field_size_limit(80000000)
    primary = args.csv or args.out / 'primary.csv'
    transfer = args.transfer_csv or args.out / 'transfer.csv'
    diagnostic = args.diagnostic_csv or args.out / 'diagnostic.csv'
    external = args.external_csv or args.out / 'external.csv'
    fetch(primary, legacy.URL, legacy.SHA256, 60000000)
    # External bytes are not parsed until configuration freeze below.
    fetch(transfer, TRANSFER_URL, TRANSFER_SHA256, 75000000)
    fetch(diagnostic, DIAGNOSTIC_URL, DIAGNOSTIC_SHA256, 110000000)
    fetch(external, EXTERNAL_URL, EXTERNAL_SHA256, 50000000)
    rows, cleaning = read_primary(primary)
    original_scorer, original_train, original_dev, regression = legacy.split_rows(rows)
    with transfer.open(newline='', encoding='utf-8') as f:
        transfer_rows, transfer_cleaning = legacy.clean_rows(
            (r['body'], int(r['label']) if r['label'] in ('0', '1') else None) for r in csv.DictReader(f))
    transfer_unseen = exclude_seen(transfer_rows, rows)
    transfer_scorer, transfer_train, transfer_dev, transfer_regression = legacy.split_rows(transfer_unseen)
    scorer_rows = original_scorer + transfer_scorer
    train = original_train + transfer_train
    development = original_dev + transfer_dev
    if not args.no_build:
        subprocess.run(['cargo', 'build', '--release', '--locked', '--example', 'email_fraud_decisions'], cwd=ROOT, check=True)
    old = legacy.TextScorer(original_scorer)
    print('Fitting phrase-aware scorer on disjoint original and transfer training splits.', flush=True)
    scorer = PhraseScorer(scorer_rows, legacy.TextScorer)
    encoded_train, _ = encode(train, scorer)
    encoded_dev, _ = encode(development, scorer)
    old_train, old_dev = old_encode(original_train, old), old_encode(original_dev, old)
    old_result = execute(args.out / 'old-development.json', {'train': old_train, 'calibration': old_dev, 'test': old_encode(development, old)})
    new_result = execute(args.out / 'new-development.json', {'train': encoded_train, 'calibration': encoded_dev, 'test': encoded_dev}, True)
    if max(errors(new_result)) >= min(errors(old_result)):
        raise AssertionError('Development improvement gate failed; external evaluation was not run')
    print('Development errors:', errors(old_result), '->', errors(new_result), flush=True)
    # Operating threshold selected only from development outcomes; missed-risk
    # cost is three times a false alarm, fixed before the final corpus is read.
    bias_trials = []
    for bias in [0., .025, .05, .1, .2]:
        trial = execute(args.out / f'development-bias-{bias}.json',
                        {'train': encoded_train, 'calibration': encoded_dev, 'test': encoded_dev, 'risk_bias': bias}, True)
        matrices = [r['full']['confusion_matrix'] for r in trial['runs']]
        cost = sum(3*c[1][0] + c[0][1] for c in matrices) / len(matrices)
        bias_trials.append({'bias': bias, 'cost': cost, 'confusion_matrices': matrices})
    selected_bias = min(bias_trials, key=lambda r: (r['cost'], r['bias']))['bias']
    new_result = execute(args.out / 'selected-development.json',
                         {'train': encoded_train, 'calibration': encoded_dev, 'test': encoded_dev, 'risk_bias': selected_bias}, True)
    freeze = {'risk_bias_selection': {'trials': bias_trials, 'selected': selected_bias, 'missed_risk_cost': 3, 'false_positive_cost': 1}, 'selection_data': 'phrase model selected on original development only; broader training verified on combined development after first transfer failure',
        'selection_rule': 'lowest mean full-coverage development errors across seeds 7,42,2026',
        'candidates': {'binary_bigram_nb_logistic': [33, 32, 32],
                       'word_tfidf_logistic': [27, 29, 30],
                       'word_tfidf_complement_nb': [29, 30, 31]},
        'selected': 'word_tfidf_logistic',
        'training_split_counts': {'scorer': len(scorer_rows), 'policy': len(train), 'development': len(development)},
        'transfer_failure': 'Original-data-only phrase model reduced original errors but regressed on CEAS; CEAS is now training/development data, not a fresh validation claim',
        'parameters': {'word_ngrams': [1, 2], 'max_features': 100000, 'min_df': 2,
                       'sublinear_tf': True, 'C': 4, 'solver': 'liblinear', 'random_state': 7},
        'guard': 'review for scorer disagreement, fewer than 5 tokens, or more than 50% unknown words; also bottom-decile Syntra calibration margin',
        'files_sha256': {str(f.relative_to(ROOT)): hashlib.sha256(f.read_bytes()).hexdigest()
                         for f in [Path(__file__), ROOT / 'scripts/email_fraud_model.py', ROOT / 'scripts/demo-email-fraud.py', ROOT / 'examples/email_fraud_decisions.rs']}}
    (args.out / 'configuration-freeze.json').write_text(json.dumps(freeze, indent=2) + '\n')
    # On the initial run, this corpus was unseen during model selection;
    # subsequent reproductions check the published result, not fresh evidence.
    with external.open(newline='', encoding='utf-8') as f:
        external_rows, external_cleaning = legacy.clean_rows(
            (r['body'], int(r['label']) if r['label'] in ('0', '1') else None) for r in csv.DictReader(f))
    with diagnostic.open(newline='', encoding='utf-8') as f:
        diagnostic_rows, _ = legacy.clean_rows(
            (r['body'], int(r['label']) if r['label'] in ('0', '1') else None) for r in csv.DictReader(f))
    unseen = exclude_seen(external_rows, rows + transfer_rows + diagnostic_rows)
    # Body only; no sender, recipient, date, subject, or source IDs in features.
    print('External unique emails after overlap removal:', len(unseen), flush=True)
    reference = PhraseScorer(scorer_rows + train, legacy.TextScorer)
    results = {}
    for name, group in [('previous_test_regression', regression), ('ceas_regression', transfer_regression), ('external_trec06', unseen)]:
        features, elapsed = encode(group, scorer, reference)
        data = {'train': encoded_train, 'calibration': encoded_dev, 'test': features, 'risk_bias': selected_bias}
        result = execute(args.out / f'{name}-features.json', data, True)
        repeated = execute(args.out / f'{name}-repeat.json', data, True)
        if legacy.quality_only(result) != legacy.quality_only(repeated):
            raise AssertionError('Repeated frozen predictions differ')
        inverted = dict(data, test=[dict(r, y=1-r['y']) for r in features])
        inversion = execute(args.out / f'{name}-inverted.json', inverted, True)
        if [r['prediction_digest'] for r in result['runs']] != [r['prediction_digest'] for r in inversion['runs']]:
            raise AssertionError('Test labels influenced decisions')
        old_data = {'train': old_train, 'calibration': old_dev, 'test': old_encode(group, old)}
        before = execute(args.out / f'{name}-old.json', old_data)
        results[name] = {'old_policy': before, 'new_policy': result, 'text_scoring_ms': elapsed,
                         'split_order_sha256': digest(''.join(r[2] for r in group))}
        print(name, 'old errors', errors(before), 'new errors', errors(result), flush=True)
    promotion = promotion_checks(results)
    report = {'configuration': freeze,
        'promotion': {'passed': all(all(g.values()) for g in promotion.values()), 'per_corpus': promotion},
        'environment': {'os': platform.system(), 'arch': platform.machine(), 'python': platform.python_version()},
        'development': {'old': old_result, 'new': new_result}, 'evaluations': results,
        'data': {'primary_url': legacy.URL, 'primary_sha256': legacy.SHA256, 'primary_cleaning': cleaning,
                 'transfer_url': TRANSFER_URL, 'transfer_sha256': TRANSFER_SHA256,
                 'transfer_cleaning': transfer_cleaning, 'transfer_overlap_removed': len(transfer_rows)-len(transfer_unseen),
                 'diagnostic_overlap_exclusion_url': DIAGNOSTIC_URL, 'diagnostic_sha256': DIAGNOSTIC_SHA256,
                 'external_url': EXTERNAL_URL, 'external_sha256': EXTERNAL_SHA256,
                 'external_cleaning': external_cleaning, 'external_overlap_removed': len(external_rows)-len(unseen),
                 'external_rows': len(unseen),
                 'external_scope': 'TREC 2006-derived email/spam labels, published by curators in 2023; separate corpus, not a newer-campaign temporal test'},
        'checks': {'development_gate_passed': True, 'configuration_frozen_before_external_evaluation': True,
                   'reproduction_note': 'TREC06 was unseen in the first final evaluation; rerunning is reproducibility evidence, not a new untouched test',
                   'repeat_quality_identical': True, 'test_label_inversion_preserves_predictions': True},
        'cost': {'llm_calls': 0, 'model_api_spend_usd': 0, 'local_compute_cost': 'not measured'},
        'timing_scope': 'batch text scoring and embedded decision calls separately; no HTTP, disk persistence, parsing or training in timings'}
    (args.out / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    print('Saved aggregate-only results; no raw messages or vocabulary in report.', flush=True)
    print('Promotion gate:', 'PASS' if report['promotion']['passed'] else 'FAIL; retain existing default', flush=True)
    enforce_promotion(report['promotion'], args.require_promotion)


if __name__ == '__main__':
    main()
