"""Offline regression checks for benchmark leakage and data handling."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('email_demo', Path(__file__).resolve().parents[2] / 'scripts/demo-email-fraud.py')
demo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(demo)


class DataChecks(unittest.TestCase):
    def test_normalized_duplicates_stay_together(self):
        rows, report = demo.clean_rows([('Routine NOTICE', 0), ('routine  notice', 0)])
        self.assertEqual(len(rows), 1)
        self.assertEqual(report['duplicate_rows'], 1)

    def test_conflicting_labels_exclude_entire_group(self):
        rows, report = demo.clean_rows([('same text', 0), ('SAME TEXT', 1), ('same text', 0)])
        self.assertEqual(rows, [])
        self.assertEqual(report['conflicting_groups_removed'], 1)

    def test_split_is_disjoint_and_reproducible(self):
        rows, _ = demo.clean_rows([(f'message {i}', i % 2) for i in range(200)])
        first = demo.split_rows(rows)
        self.assertEqual(first, demo.split_rows(rows))
        ids = [r[2] for group in first for r in group]
        self.assertEqual(len(ids), len(set(ids)))
        self.assertEqual([len(g) for g in first], [100, 40, 20, 40])

    def test_invalid_empty_rows_are_excluded(self):
        rows, _ = demo.clean_rows([(None, 0), ('', 0), ('NaN', 1), ('empty', 0), ('hello', None)])
        self.assertEqual(rows, [])

    def test_features_do_not_mutate_training_vocabulary(self):
        scorer = demo.TextScorer([('ordinary schedule', 0, 'a'), ('suspicious payment', 1, 'b')])
        before = dict(scorer.odds)
        x, prediction = scorer.encode('unseen terminology')
        self.assertEqual(scorer.odds, before)
        self.assertNotIn('unseen', scorer.vocabulary)
        self.assertEqual(len(x), 10)
        self.assertIn(prediction, (0, 1))

    def test_duplicate_key_matches_truncated_model_input(self):
        rows, _ = demo.clean_rows([('a' * demo.MAX_CHARS + ' x', 0), ('a' * demo.MAX_CHARS + ' y', 0)])
        self.assertEqual(len(rows), 1)


if __name__ == '__main__':
    unittest.main()
