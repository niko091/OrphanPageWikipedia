"""Regression tests for the validated bulk Python/Rust boundary."""
import json
import unittest
from pathlib import Path

from orphanwiki import _native


class NativeAnalysisTests(unittest.TestCase):
    def test_age_and_creator_dates_match_instants_and_keep_unknowns(self):
        ages = _native.article_ages('2026-01-02T00:00:00Z', [
            None, '2026-01-01T01:00:00+01:00',
            '2026-01-01T23:59:59.500Z', '2026-01-03T00:00:00Z'])
        self.assertEqual(ages[0], (None, False))
        self.assertEqual(ages[1], (1.0, False))
        self.assertAlmostEqual(ages[2][0], 0.5 / 86400)
        self.assertEqual(ages[3], (None, True))
        self.assertEqual(_native.creator_matches([
            ('2026-01-01T00:00:00Z', '2026-01-01T01:00:00+01:00', False, None),
            (None, None, True, None),
            ('2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', True, None),
        ]), [(True, False), (False, False), (True, True)])
        with self.assertRaises(ValueError):
            _native.article_ages('2026-01-02T00:00:00Z', ['invalid'])

    def test_creator_counts_keep_local_identity_and_missing(self):
        self.assertEqual(_native.creator_counts([
            ('vec:1', True), ('lmo:1', False), (None, True), ('vec:1', False),
        ]), [(2, 1, 0.5), (1, 0, 0.0), (None, None, None), (2, 1, 0.5)])

    def test_adjacency_bounds_are_python_errors_not_panics(self):
        for incoming, unique in [([[]], {'Q1': 1}), ([[2]], {'Q1': 0}), ([], {})]:
            with self.subTest(incoming=incoming, unique=unique):
                with self.assertRaises(ValueError):
                    _native.matchable_edges(1, unique, incoming)
                with self.assertRaises(ValueError):
                    _native.topic_inputs(1, unique, {'Q1'}, incoming)
        self.assertEqual(_native.topic_inputs(
            3, {'Q1': 0, 'Q2': 1}, {'Q2'}, [[], [0, 0], []]),
            [('Q2', 1, True), ('', 0, False), ('', 0, False)])
        self.assertEqual(_native.topic_inputs(
            2, {'Q2': 1}, set(), [[], [0]]), [('', 0, True), ('', 0, False)])

    def test_threshold_is_strict_and_requires_all_topics(self):
        taxonomy = json.loads(Path('config/topics.json').read_text())['labels']
        labels = ['__label__' + label.replace(' ', '_') for label in taxonomy]
        probabilities = [0.1] * 64
        probabilities[0] = 0.5
        probabilities[1] = 0.50001
        self.assertEqual(_native.threshold_topics(set(taxonomy), [labels], [probabilities]),
                         [[taxonomy[1]]])
        for scores in [probabilities[:-1], [float('nan')] * 64, [1.01] * 64]:
            with self.subTest(scores=scores), self.assertRaises(ValueError):
                _native.threshold_topics(set(taxonomy), [labels], [scores])

    def test_evidence_union_keeps_every_language_and_rejects_inconsistent_pages(self):
        references = [(1, 'Source', 0, f'wiki{i}', i + 1, 'Reference', 100, 'Target')
                      for i in range(8)]
        ranked = _native.rank_candidates(references + references)
        self.assertEqual(len(ranked), 1)
        self.assertEqual(len(ranked[0][3]), 8)
        with self.assertRaises(ValueError):
            _native.rank_candidates(references + [(1, 'Other title', 0, 'lmo', 2, 'X', 3, 'Y')])

    def test_collection_interval_validates_and_compares_offsets(self):
        start = '2026-01-01T01:00:00+02:00'
        finish = '2026-01-02T00:00:00Z'
        self.assertEqual(_native.collection_interval([
            ('2026-01-01T00:00:00Z', finish), (start, '2026-01-01T00:00:00Z')]),
            (start, finish))
        for intervals in [[], [(finish, start)], [('invalid', finish)]]:
            with self.subTest(intervals=intervals), self.assertRaises(ValueError):
                _native.collection_interval(intervals)
