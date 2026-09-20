"""Guardrails for the optional phrase-aware benchmark."""
import importlib.util
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
try:
    spec = importlib.util.spec_from_file_location('evaluation', ROOT / 'scripts/demo-email-fraud-eval.py')
    evaluation = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(evaluation)
except ModuleNotFoundError as error:
    if error.name not in ('numpy', 'sklearn', 'scipy'):
        raise
    evaluation = None


@unittest.skipIf(evaluation is None, 'install optional email-fraud requirements')
class ModelBoundaries(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.model = evaluation.PhraseScorer([
            ('ordinary project meeting notes available tomorrow', 0, 'a'),
            ('ordinary project meeting notes available today', 0, 'b'),
            ('urgent verify password account suspended now', 1, 'c'),
            ('urgent verify password account suspended immediately', 1, 'd'),
        ], evaluation.legacy.TextScorer)

    def test_unknown_and_short_input_requires_review(self):
        for text in ['hello', 'zorb xylophone quux frobnicate flibbertigibbet']:
            self.assertTrue(self.model.encode_batch([text])[0][2])

    def test_evaluation_never_refits_vocabulary(self):
        before = dict(self.model.vectorizer.vocabulary_)
        first = self.model.encode_batch(['ordinary meeting notes available tomorrow'])
        self.model.encode_batch(['unexpected new jargon'])
        self.assertEqual(self.model.vectorizer.vocabulary_, before)
        self.assertEqual(first, self.model.encode_batch(['ordinary meeting notes available tomorrow']))

    def test_punctuation_only_changes_cannot_bypass_overlap_exclusion(self):
        known = [('existing, message!', 0, 'a')]
        external = [('EXISTING message', 1, 'b'), ('different message', 0, 'c')]
        self.assertEqual(evaluation.exclude_seen(external, known), [external[1]])

    def test_accuracy_improvement_cannot_hide_more_missed_risk(self):
        def policy(matrix):
            correct = matrix[0][0] + matrix[1][1]
            return {'runs': [{'full': {'accuracy': correct / 100, 'confusion_matrix': matrix}}]}
        checks = evaluation.promotion_checks({'fixture': {
            'old_policy': policy([[70, 10], [2, 18]]),
            'new_policy': policy([[78, 2], [4, 16]])}})
        self.assertTrue(checks['fixture']['accuracy_non_regression'])
        self.assertFalse(checks['fixture']['missed_risk_non_regression'])
        with self.assertRaises(SystemExit) as failed:
            evaluation.enforce_promotion({'passed': False}, True)
        self.assertEqual(failed.exception.code, 2)
        evaluation.enforce_promotion({'passed': True}, True)
        evaluation.enforce_promotion({'passed': False}, False)

    def test_features_are_finite_and_do_not_contain_labels(self):
        import math
        x, prediction, guard = self.model.encode_batch(['project meeting now'])[0]
        self.assertEqual(len(x), 11)
        self.assertTrue(all(math.isfinite(v) for v in x))
        self.assertIn(prediction, (0, 1))
        self.assertIsInstance(guard, bool)


if __name__ == '__main__':
    unittest.main()
