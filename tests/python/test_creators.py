"""Synthetic creator histories: no network calls or real account data."""
import copy
import json
import random
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

import pymysql

from fixtures import demo, demo_creators
from orphanwiki.__main__ import main
from orphanwiki.core import analyze
from orphanwiki.replicas import collect_creators, creator_records, disk_creator_records, experience_before, iso_timestamp


class History:
    def __init__(self, modern=True):
        self.modern = modern
        self.queries = []
        # pid, revision, timestamp, parent, deleted, actor, user, registration, temporary, bot
        self.first = [
            (1, 10, '20200102000000', 0, 0, 7, 70, '20200101000000', 0, 0),
            (2, 20, '20200103000000', 0, 0, 7, 70, '20200101000000', 0, 0),
            (3, 30, '20200103000000', 0, 0, 7, 70, '20200101000000', 0, 0),
            (4, 40, '20200104000000', 0, 0, 7, 70, '20200101000000', 0, 0),
            (5, 50, '20200102000000', 1, 0, 8, 80, None, 0, 0),
            (6, 60, '20200102000000', 0, 4, None, None, None, None, 0),
            (7, 70, '20200102000000', 0, 0, 9, None, None, None, 0),
            (8, 80, '20200102000000', 0, 0, 10, 100, None, 1, 0),
            (9, 90, '20200102000000', 0, 0, 11, 110, '20200105000000', 0, 1),
        ]
        self.events = [(7, '20200101000000', 0), (7, '20200101000000', 1),
                       (7, '20200102000000', 0), (7, '20200103000000', 0),
                       (7, '20200103000000', 0), (7, '20250101000000', 0)]

    def rows(self, sql, params=()):
        self.queries.append((sql, params))
        if sql.startswith('SHOW'):
            return iter([('user_is_temp',)] if self.modern else [])
        if sql.startswith('WITH'):
            self.assert_safe_sql(sql)
            return iter(self.first if self.modern else [r[:8] + (None,) + r[9:] for r in self.first])
        if 'FROM change_tag' in sql:
            return iter([(1, 10, 'contenttranslation-v2'), (2, 999, 'contenttranslation')])
        if sql.startswith('SELECT rev_page,MIN'):
            return iter((r[0], r[2], r[2] if r[3] == 0 else None, int(r[3] == 0)) for r in self.first)
        if sql.startswith('SELECT log_page'):
            return iter([(4, None, 0, 1)])
        if 'FROM logging' in sql:
            return iter([(4, 'import'), (2, 'move')])
        if 'WHERE rev_actor IN' in sql:
            assert 'FROM revision_userindex' in sql
            assert '(rev_deleted & 4)=0' in sql
            return (r for r in self.events if r[0] in params[:-1] and r[1] < params[-1])
        raise AssertionError(sql)

    @staticmethod
    def assert_safe_sql(sql):
        assert 'ORDER BY rev_timestamp, rev_id' in sql
        assert '(r.rev_deleted & 4)=0' in sql
        assert 'actor_name' not in sql

    def collect(self):
        return creator_records(self.rows, lambda table: {'user_is_temp'} if self.modern else set(), 'vecwiki')


class CreatorTests(unittest.TestCase):
    def test_invalid_creator_registration_and_edit_dates_in_both_modes(self):
        def history():
            result = History()
            first = list(result.first[0]); first[2] = '\x00' * 14
            result.first[0] = tuple(first)
            second = list(result.first[1]); second[7] = '00000000000000'
            result.first[1] = tuple(second)
            result.events.append((7, '\x00' * 14, 0))
            return result
        ordinary = history().collect()
        by_id = {r['page_id']: r for r in ordinary}
        self.assertEqual(by_id[1]['creator_attribution'], 'invalid_revision_timestamp')
        self.assertIsNone(by_id[1]['article_created_at'])
        self.assertIsNone(by_id[1]['creator_key'])
        self.assertIsNone(by_id[2]['creator_registered_at'])
        self.assertIsNone(by_id[2]['creator_tenure_days'])
        self.assertEqual(by_id[2]['creator_articles_created_total'], 2)
        for pid in [2, 3]:
            self.assertIsNone(by_id[pid]['creator_prior_edits_main'])
            self.assertIsNone(by_id[pid]['creator_prior_edits_other'])
            self.assertEqual(by_id[pid]['creator_experience_status'], 'invalid_revision_timestamp')
        for batch in [1, 4]:
            h = history()
            with closing(sqlite3.connect(':memory:')) as stage:
                staged = list(disk_creator_records(h.rows, lambda _: {'user_is_temp'}, 'vecwiki', 'vec', stage, batch))
            for record in staged:
                for key, expected in by_id[record['page_id']].items():
                    self.assertEqual(record[key], expected, (batch, record['page_id'], key))

    def test_history_origin_types_and_strict_prior_experience(self):
        history = History()
        pages = {p['page_id']: p for p in history.collect()}
        a, b = pages[1], pages[2]
        self.assertEqual(a['origin'], 'translation_tagged')
        self.assertEqual(b['origin'], 'new_page_unclassified')
        self.assertTrue(b['move_log_observed'])
        self.assertEqual(a['creator_tenure_days'], 1)
        self.assertEqual((a['creator_prior_edits_main'], a['creator_prior_edits_other']), (1, 1))
        self.assertEqual((b['creator_prior_edits_main'], b['creator_prior_edits_other']), (2, 1))
        self.assertEqual(b['creator_prior_articles'], 1)
        self.assertEqual(pages[3]['creator_prior_articles'], 1)  # same second excluded
        self.assertEqual(a['creator_articles_created_total'], 3)  # import excluded
        self.assertEqual(a['creator_languages_created'], ['vec'])
        self.assertEqual(a['creator_languages_before'], [])
        self.assertEqual(b['creator_languages_before'], ['vec'])
        for pid in (4, 5, 6, 7):
            self.assertIsNone(pages[pid]['creator_key'])
            self.assertIsNone(pages[pid]['creator_prior_edits_main'])
        self.assertEqual(pages[4]['origin'], 'imported_history_observed')
        self.assertIsNone(pages[4]['creator_registered_at'])
        self.assertEqual(pages[5]['creator_attribution'], 'incomplete_history')
        self.assertEqual(pages[6]['creator_attribution'], 'hidden_creator')
        self.assertEqual(pages[7]['creator_account_type'], 'anonymous_legacy')
        self.assertEqual(pages[8]['creator_account_type'], 'temporary')
        self.assertIsNone(pages[8]['creator_tenure_days'])
        self.assertEqual(pages[8]['creator_prior_edits_main'], 0)
        self.assertEqual(pages[9]['creator_account_type'], 'bot')
        self.assertIsNone(pages[9]['creator_tenure_days'])
        self.assertEqual(pages[9]['creator_tenure_status'], 'registration_after_revision')
        self.assertTrue(all(p['creator_account_type_at_creation'] is None for p in pages.values()))
        self.assertEqual(len(history.queries), 4)  # one bulk edit query, not per page

    def test_older_user_schema_does_not_guess_temporary_status(self):
        pages = History(modern=False).collect()
        self.assertEqual(pages[0]['creator_account_type'], 'unknown')
        self.assertEqual(pages[-1]['creator_account_type'], 'bot')

    def test_streaming_sweep_matches_brute_force_random_histories(self):
        rng = random.Random(120)
        for _ in range(60):
            stamp = lambda day: f'202001{day:02}000000'
            groups = {a: [{'article_created_at': iso_timestamp(stamp(rng.randrange(1, 29)))}
                          for _ in range(rng.randrange(1, 12))] for a in range(5)}
            events = sorted((rng.randrange(5), stamp(rng.randrange(1, 29)), rng.choice([0, 1, 14]))
                            for _ in range(rng.randrange(100)))
            experience_before(groups, iter(events))
            for actor, pages in groups.items():
                for p in pages:
                    earlier = [ns for a, t, ns in events if a == actor and iso_timestamp(t) < p['article_created_at']]
                    self.assertEqual(p['creator_prior_edits_main'], earlier.count(0))
                    self.assertEqual(p['creator_prior_edits_other'], len(earlier) - earlier.count(0))

    def test_queries_are_batched(self):
        h = History()
        h.first = [(i, i, '20200102000000', 0, 0, i, i, None, 0, 0) for i in range(100, 602)]
        h.collect()
        batches = [params for sql, params in h.queries if 'WHERE rev_actor IN' in sql]
        self.assertEqual([len(b) - 1 for b in batches], [250, 250, 2])

    def test_connection_cleanup_and_driver_percent_formatting(self):
        for fail in (False, True):
            history = History()
            class Cursor:
                def __enter__(self): return self
                def __exit__(self, *args): pass
                def execute(self, sql, params=None):
                    if fail:
                        raise RuntimeError('fixture failure')
                    # Exercise real PyMySQL interpolation without a network connection.
                    driver = pymysql.Connection(defer_connect=True)
                    driver.server_status = 0
                    driver.cursor().mogrify(sql, params)
                    self.data = history.rows(sql, params or ())
                def __iter__(self): return iter(self.data)
            class Connection:
                closed = False
                def cursor(self): return Cursor()
                def close(self): self.closed = True
            conn = Connection()
            with patch('orphanwiki.replicas.replica_options', return_value=('vecwiki', {})), patch('orphanwiki.replicas.pymysql.connect', return_value=conn):
                if fail:
                    with self.assertRaises(RuntimeError): collect_creators('vec')
                else:
                    result = collect_creators('vec')
                    self.assertEqual(result['metadata']['languages_scanned'], ['vec'])
                    self.assertEqual(result['metadata']['source'], 'Wiki Replicas / vecwiki')
                    self.assertTrue(result['metadata']['complete'])
            self.assertTrue(conn.closed)

    def test_join_missing_mismatch_and_current_orphans(self):
        raw = demo()
        extra = demo_creators(raw)
        extra['pages'][0]['article_created_at'] = '1900-01-01T00:00:00Z'
        extra['pages'].pop(1)
        result = analyze(raw, creators=extra)
        pages = result['pages']
        self.assertEqual(pages[0]['creator_data_status'], 'creation_date_mismatch')
        self.assertEqual(pages[1]['creator_data_status'], 'missing_page')
        self.assertIsNone(pages[0]['creator_orphans_current'])
        for p in pages[2:]:
            group = [r for r in pages if r['creator_key'] == p['creator_key']]
            self.assertEqual(p['creator_articles_in_census'], len(group))
            self.assertEqual(p['creator_orphans_current'], sum(r['orphan'] for r in group))
            self.assertIsNone(p['creator_orphans_at_30_days'])
        self.assertEqual(result['metadata']['creators']['matched_pages'], len(pages) - 2)
        self.assertEqual(len(result['metadata']['creators']['input_sha256']), 64)

    def test_reject_incompatible_creator_data(self):
        raw = demo()
        raw['metadata']['demo'] = False
        baseline = demo_creators(raw)
        baseline['metadata'].update(source='Wiki Replicas / vecwiki', demo=False)
        for field, value in [('language', 'it'), ('complete', False), ('source', 'external')]:
            extra = copy.deepcopy(baseline)
            extra['metadata'][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                analyze(raw, creators=extra)
        baseline['pages'].append(baseline['pages'][0])
        with self.assertRaisesRegex(ValueError, 'duplicati'):
            analyze(raw, creators=baseline)

    def test_old_input_keeps_unknowns_missing(self):
        result = analyze(demo())
        self.assertEqual(result['metadata']['creators']['status'], 'not_collected')
        self.assertTrue(all(p['creator_prior_articles'] is None and p['creator_orphans_current'] is None
                            for p in result['pages']))

    def test_collect_cli_bundles_creators_and_analysis_uses_them(self):
        raw = demo()
        extra = demo_creators(raw)
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'vec-raw.json'
            with patch('sys.argv', ['orphanwiki', 'collect', '--language', 'vec', '--credentials', 'fixture.cnf', '--output', str(output)]), \
                    patch('orphanwiki.replicas.collect_replicas', return_value=raw) as graph, \
                    patch('orphanwiki.replicas.collect_creators', return_value=extra) as creators:
                main()
            graph.assert_called_once_with('vec', 'fixture.cnf')
            creators.assert_called_once_with('vec', 'fixture.cnf')
            bundled = json.loads(output.read_text())
            self.assertEqual(json.loads((Path(folder) / 'vec-creators.json').read_text()), extra)
            self.assertEqual(bundled['creator_data'], extra)
            result = analyze(bundled)
            self.assertEqual(result['metadata']['creators']['matched_pages'], len(raw['pages']))
            self.assertEqual(result['pages'], analyze(raw, creators=extra)['pages'])

    def test_collect_failure_keeps_graph_and_reports_incomplete_bundle(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'vec-raw.json'
            with patch('sys.argv', ['orphanwiki', 'collect', '--output', str(output)]), \
                    patch('orphanwiki.replicas.collect_replicas', return_value=demo()), \
                    patch('orphanwiki.replicas.collect_creators', side_effect=RuntimeError('fixture failure')):
                with self.assertRaisesRegex(RuntimeError, 'fixture failure'):
                    main()
            saved = json.loads(output.read_text())
            self.assertNotIn('creator_data', saved)
            self.assertEqual(saved['metadata']['creator_collection_status'], 'pending')

    def test_creator_checkpoint_survives_failed_combined_export(self):
        from orphanwiki.core import write_json
        raw = demo()
        extra = demo_creators(raw)
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'vec-raw.json'
            def save(path, value):
                if 'creator_data' in value:
                    raise OSError('fixture combined export failure')
                write_json(path, value)
            with patch('sys.argv', ['orphanwiki', 'collect', '--output', str(output)]), \
                    patch('orphanwiki.replicas.collect_replicas', return_value=raw), \
                    patch('orphanwiki.replicas.collect_creators', return_value=extra), \
                    patch('orphanwiki.__main__.write_json', side_effect=save):
                with self.assertRaisesRegex(OSError, 'fixture combined export failure'):
                    main()
            self.assertEqual(json.loads((Path(folder) / 'vec-creators.json').read_text()), extra)
            graph = json.loads(output.read_text())
            self.assertEqual(graph['metadata']['creator_collection_status'], 'pending')
            result = analyze(graph, creators=extra)
            self.assertEqual(result['metadata']['creators']['matched_pages'], len(raw['pages']))

    def test_creator_checkpoint_path_is_validated_before_queries(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'vec.json'
            with patch('sys.argv', ['orphanwiki', 'collect', '--output', str(output), '--creators-output', str(output)]), \
                    patch('orphanwiki.replicas.collect_replicas') as collect:
                with self.assertRaises(SystemExit):
                    main()
                collect.assert_not_called()

    def test_standalone_creator_recovery_does_not_collect_the_graph(self):
        extra = demo_creators(demo())
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'vec-creators.json'
            with patch('sys.argv', ['orphanwiki', 'collect-creators', '--language', 'vec', '--output', str(output)]), \
                    patch('orphanwiki.replicas.collect_creators', return_value=extra), \
                    patch('orphanwiki.replicas.collect_replicas') as graph, \
                    patch('json.dumps', side_effect=AssertionError('Whole-document serialization prohibited')):
                main()
            graph.assert_not_called()
            self.assertEqual(json.loads(output.read_text()), extra)

    def test_comparison_uses_bundled_creators_without_extra_arguments(self):
        from orphanwiki.core import compare_languages
        raw = demo()
        other = copy.deepcopy(raw)
        other['metadata']['language'] = 'lmo'
        for dataset in (raw, other):
            dataset['creator_data'] = demo_creators(dataset)
        results = compare_languages([raw, other])
        for result in results.values():
            self.assertEqual(result['metadata']['creators']['status'], 'collected')
            self.assertTrue(all(page['creator_key'] for page in result['pages']))

    def test_new_supplement_updates_age_without_replacing_graph(self):
        raw = demo()
        del raw['metadata']['creation_date_rule']
        for page in raw['pages']:
            page.pop('page_created_at')
        extra = demo_creators(raw)
        extra['metadata']['creation_date_rule'] = 'creation_event_v1'
        extra['pages'][0].update(page_created_at='2026-09-28T12:00:00Z', creation_date_source='creation_log', creation_date_status='observed')
        result = analyze(raw, creators=extra)
        self.assertEqual(result['pages'][0]['age_days'], 0.5)
        self.assertIsNone(result['pages'][0]['creator_prior_edits_main'])
        self.assertIsNone(result['pages'][0]['creator_key'])
        self.assertIsNone(result['pages'][1]['age_days'])
        self.assertEqual(result['metadata']['age_rule'], 'page_creation_event')
        self.assertEqual([p['in_degree'] for p in result['pages']], [p['in_degree'] for p in analyze(raw)['pages']])


if __name__ == '__main__':
    unittest.main()
