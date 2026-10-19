"""Regression tests for the validated bulk Python/Rust boundary."""
import json
import unittest
from pathlib import Path

from orphanwiki import _native


class NativeAnalysisTests(unittest.TestCase):
    def test_history_activity_matches_definition_for_many_unsorted_creation_dates(self):
        from collections import Counter
        from datetime import datetime, timedelta
        from random import Random
        rng = Random(42)
        start = datetime(2020, 1, 1)
        events = [(start + timedelta(hours=12 * i), rng.choice((-1, 0, 0, 1, 2))) for i in range(1200)]
        queries = [start + timedelta(days=rng.randrange(-30, 660)) for _ in range(300)]
        queries.extend(queries[:5])
        rng.shuffle(events)
        actual = _native.history_activity(
            [(at.isoformat(sep=' ') + '.0', ns) for at, ns in events],
            [at.isoformat(sep=' ') + '.0' for at in queries])
        months = Counter((at.year, at.month) for at, _ in events)
        expected = []
        for creation in queries:
            before = [ns for at, ns in events if at < creation]
            main = before.count(0)
            month = creation.replace(day=1) - timedelta(days=1)
            streak = 0
            while months[month.year, month.month] >= 5:
                streak += 1
                month = month.replace(day=1) - timedelta(days=1)
            bucket = next(name for limit, name in ((0, 'none'), (1, 'one'), (2, 'two'),
                          (6, 'three_to_six'), (12, 'seven_to_twelve'), (24, 'thirteen_to_twenty_four'),
                          (float('inf'), 'over_twenty_four')) if streak <= limit)
            expected.append((main, len(before) - main, streak, bucket))
        self.assertEqual(actual, expected)

    def test_age_dates_match_instants_and_keep_unknowns(self):
        ages = _native.article_ages('2026-01-02T00:00:00Z', [
            None, '2026-01-01T01:00:00+01:00',
            '2026-01-01T23:59:59.500Z', '2026-01-03T00:00:00Z'])
        self.assertEqual(ages[0], (None, False))
        self.assertEqual(ages[1], (1.0, False))
        self.assertAlmostEqual(ages[2][0], 0.5 / 86400)
        self.assertEqual(ages[3], (None, True))
        with self.assertRaises(ValueError):
            _native.article_ages('2026-01-02T00:00:00Z', ['invalid'])

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

    def test_bounded_topic_inputs_match_full_graph_resolution(self):
        titles = ['Source', 'Target', 'Duplicate QID', 'Unknown', 'No QID']
        items = ['Q1', 'Q2', 'Q2', 'Q99', None]
        redirects = {'Alias': 'Target', 'Loop A': 'Loop B', 'Loop B': 'Loop A'}
        links = [['Alias', 'Target', 'Source', 'Unknown', 'Loop A'],
                 ['No QID'], ['Target'], [], ['Unknown']]
        bounded = _native.TopicInputAccumulator(titles, items, redirects, {'Q1'})
        actual = bounded.add_batch(links[:2]) + bounded.add_batch(links[2:])
        bounded.finish()
        _, _, _, incoming = _native.graph_details(titles, links, redirects)
        unique, _ = _native.unique_items(items)
        self.assertEqual(actual, _native.topic_inputs(len(titles), unique, {'Q1'}, incoming))
        with self.assertRaises(ValueError):
            bounded.add_batch([[]])
        incomplete = _native.TopicInputAccumulator(titles, items, redirects, set())
        with self.assertRaises(ValueError):
            incomplete.finish()

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
