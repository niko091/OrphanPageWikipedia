"""General collection and pooled dashboard exports. Synthetic data only."""
import copy
import json
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

from fixtures import demo, demo_creators
from orphanwiki.__main__ import main
from orphanwiki.core import aggregate_index, compare_languages, replica_database, write_json
from orphanwiki.replicas import DiskPages, collect_creation_dates, collect_replicas, collect_sizes, disk_creator_records
from test_creators import History
from test_replicas import Connection


class MultilingualTests(unittest.TestCase):
    def test_generic_codes_database_overrides_and_validation(self):
        for code in ['it', 'fur', 'nap', 'roa-tara', 'en', 'zh-min-nan']:
            self.assertEqual(replica_database(code), code.replace('-', '_') + 'wiki')
        self.assertEqual(replica_database('be-tarask', 'be_x_oldwiki'), 'be_x_oldwiki')
        for code, database in [('all', None), ('../vec', None), ('vec', 'wikidatawiki_p'), ('vec', 'bad;wiki')]:
            with self.assertRaises(ValueError):
                replica_database(code, database)

    def test_selected_languages_are_configuration_only(self):
        config = json.loads(Path('config/languages.json').read_text())
        self.assertEqual(set(config['languages']), {'vec', 'lmo', 'fur', 'roa-tara', 'nap', 'pms', 'scn', 'sc', 'lij', 'eml', 'co', 'lld'})
        self.assertEqual(config['size_selection']['multiplier'], 1.5)
        with tempfile.TemporaryDirectory() as folder:
            file = Path(folder) / 'selection.json'
            file.write_text(json.dumps({'version': 1, 'languages': ['en', 'roa-tara', 'it']}))
            def raw(language, credentials):
                result = demo()
                result['metadata']['language'] = language
                return result
            def creators(language, credentials):
                return demo_creators(raw(language, credentials))
            with patch('sys.argv', ['orphanwiki', 'collect', '--languages-file', str(file), '--output-dir', folder]), \
                    patch('orphanwiki.replicas.collect_replicas', side_effect=raw) as graph, \
                    patch('orphanwiki.replicas.collect_creators', side_effect=creators) as authors:
                main()
            self.assertEqual([c.args[0] for c in graph.call_args_list], ['en', 'roa-tara', 'it'])
            self.assertEqual(graph.call_count, authors.call_count)
            for code in ['en', 'roa-tara', 'it']:
                data = json.loads((Path(folder) / f'{code}-raw.json').read_text())
                self.assertEqual(len(data['pages']), 240)
                self.assertEqual(data['metadata']['creator_collection_status'], 'collected')

    def test_comparison_discovers_every_standard_raw_and_creator_checkpoint(self):
        with tempfile.TemporaryDirectory() as folder:
            for language in ['vec', 'lmo', 'it', 'fur']:
                raw = demo()
                raw['metadata']['language'] = language
                write_json(Path(folder) / f'{language}-raw.json', raw)
                write_json(Path(folder) / f'{language}-creators.json', demo_creators(raw))
            output = Path(folder) / 'export'
            output.mkdir()
            write_json(output / 'manifest.json', [{'language': 'stale', 'file': 'old.json'}])
            with patch('sys.argv', ['orphanwiki', 'compare', '--input-dir', folder, '--output-dir', str(output)]):
                main()
            manifest = json.loads((output / 'manifest.json').read_text())
            self.assertEqual({e['language'] for e in manifest}, {'vec', 'lmo', 'it', 'fur', 'all'})
            for language in ['vec', 'lmo', 'it', 'fur']:
                result = json.loads((output / f'{language}-demo.json').read_text())
                self.assertEqual(set(result['metadata']['comparison']['reference_languages']), {'vec', 'lmo', 'it', 'fur'} - {language})
                self.assertEqual(result['metadata']['creators']['matched_pages'], 240)

    def test_aggregate_keeps_independent_provenance_without_duplicate_page_exports(self):
        a, b = demo(), demo()
        b['metadata'].update(language='en', started_at='2026-09-28T00:00:00+00:00')
        a['creator_data'] = demo_creators(a)
        results = compare_languages([a, b])
        index = aggregate_index(results, {'vec': 'vec.json', 'en': 'en.json'})
        self.assertEqual(index['metadata']['observation_unit'], 'language_page')
        self.assertEqual(index['metadata']['started_at'], b['metadata']['started_at'])
        self.assertEqual(sum(m['page_count'] for m in index['members']), 480)
        self.assertEqual(index['members'][1]['metadata']['creators']['input_sha256'], results['vec']['metadata']['creators']['input_sha256'])
        self.assertNotIn('pages', index)
        invalid = copy.deepcopy(results)
        invalid['en']['metadata']['complete'] = False
        with self.assertRaises(ValueError): aggregate_index(invalid, {'vec': 'vec.json', 'en': 'en.json'})
        with self.assertRaises(ValueError): aggregate_index(results, {'vec': '../vec.json', 'en': 'en.json'})

    def test_disk_graph_matches_memory_graph_for_both_replica_schemas(self):
        for modern in (True, False):
            with self.subTest(modern=modern), tempfile.TemporaryDirectory() as folder:
                credential = Path(folder) / 'credential'
                credential.write_text('[client]\nuser=synthetic\npassword=synthetic')
                with patch('orphanwiki.replicas.pymysql.connect', return_value=Connection(modern)):
                    reference = collect_replicas('it', credential)
                with closing(sqlite3.connect(Path(folder) / 'stage.sqlite')) as stage:
                    store = DiskPages(stage)
                    with patch('orphanwiki.replicas.pymysql.connect', return_value=Connection(modern)):
                        raw = collect_replicas('it', credential, page_store=store)
                    store.prepare_export()
                    self.assertEqual(list(store.export_pages()), reference['pages'])
                    self.assertEqual(raw['category_parents'], reference['category_parents'])
                    self.assertEqual(raw['redirects'], reference['redirects'])
                    file = Path(folder) / 'raw.json'
                    write_json(file, raw, arrays={'pages': store.export_pages()})
                    self.assertEqual(json.loads(file.read_text())['pages'], reference['pages'])

    def test_disk_creators_match_all_counts_across_batches_and_schema_variants(self):
        for modern in (True, False):
            for batch in (1, 2, 4, 1000):
                with self.subTest(modern=modern, batch=batch), closing(sqlite3.connect(':memory:')) as stage:
                    history = History(modern)
                    reference = history.collect()
                    collect_creation_dates(history.rows, {r['page_id']: r for r in reference})
                    history.queries.clear()
                    result = list(disk_creator_records(history.rows, lambda _: {'user_is_temp'} if modern else set(), 'vecwiki', 'vec', stage, batch))
                    self.assertEqual(result, reference)
                    initial_queries = [q for q, _ in history.queries if q.startswith('WITH')]
                    self.assertEqual(len(initial_queries), 1)
                    edit_queries = [q for q, _ in history.queries if 'WHERE rev_actor IN' in q]
                    self.assertEqual(len(edit_queries), 1)
                    for sql, _ in history.queries:
                        if 'FROM change_tag' in sql or sql.startswith('SELECT rev_page,MIN'):
                            self.assertIn(' IN (', sql)

    def test_disk_creator_histories_use_bounded_actor_queries_for_large_groups(self):
        history = History()
        history.first = [(i, i * 10, '20200102000000', 0, 0, i, i * 100, '20200101000000', 0, 0)
                         for i in range(1, 503)]
        history.events = [(i, '20200101000000', i % 2) for i in range(1, 503)]
        reference = history.collect()
        collect_creation_dates(history.rows, {r['page_id']: r for r in reference})
        history.queries.clear()
        with closing(sqlite3.connect(':memory:')) as stage:
            result = list(disk_creator_records(history.rows, lambda _: {'user_is_temp'}, 'vecwiki', 'vec', stage, 25))
        self.assertEqual(result, reference)
        queries = [(sql, params) for sql, params in history.queries if 'WHERE rev_actor IN' in sql]
        self.assertEqual(len(queries), 3)
        self.assertTrue(all(len(params) <= 251 for _, params in queries))
        self.assertEqual(len({actor for _, params in queries for actor in params[:-1]}), 501)

    def test_historical_database_source_and_supplement_must_agree(self):
        raw, other = demo(), demo()
        raw['metadata'].update(language='be-tarask', database='be_x_oldwiki', source='Wiki Replicas / be_x_oldwiki')
        other['metadata']['language'] = 'en'
        supplement = demo_creators(raw)
        supplement['metadata'].update(database='be_x_oldwiki', source='Wiki Replicas / be_x_oldwiki')
        raw['creator_data'] = supplement
        results = compare_languages([raw, other])
        self.assertEqual(results['be-tarask']['metadata']['creators']['matched_pages'], 240)
        supplement['metadata']['database'] = 'be_taraskwiki'
        with self.assertRaises(ValueError): compare_languages([raw, other])

    def test_streaming_sections_atomic_checkpoint_and_iterator_failure(self):
        with tempfile.TemporaryDirectory() as folder:
            output, checkpoint = Path(folder) / 'raw.json', Path(folder) / 'creators.json'
            write_json(checkpoint, {'metadata': {'complete': True}, 'pages': [1, 2]})
            write_json(output, {'metadata': {}, 'pages': [], 'creator_data': None},
                       arrays={'pages': iter([{'page_id': 1}, {'page_id': 2}])}, embedded_paths={'creator_data': checkpoint})
            combined = json.loads(output.read_text())
            self.assertEqual(combined['creator_data'], json.loads(checkpoint.read_text()))
            self.assertEqual(len(combined['pages']), 2)
            before = output.read_bytes()
            def failed():
                yield {'page_id': 1}
                raise OSError('synthetic iterator failure')
            with self.assertRaises(OSError): write_json(output, {'pages': []}, arrays={'pages': failed()})
            self.assertEqual(output.read_bytes(), before)
            self.assertFalse(output.with_suffix('.json.tmp').exists())

    def test_low_memory_cli_full_collection_and_failed_publication_keep_checkpoint(self):
        history = History()
        class HistoryCursor:
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def execute(self, sql, params=None): self.records = history.rows(sql, params or ())
            def __iter__(self): return iter(self.records)
        class HistoryConnection:
            closed = False
            def cursor(self): return HistoryCursor()
            def close(self): self.closed = True
        for fail in (False, True):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as folder:
                output = Path(folder) / 'it-raw.json'
                author_connection, graph_connection = HistoryConnection(), Connection()
                def save(path, value, **kwargs):
                    if fail and kwargs.get('embedded_paths'):
                        raise OSError('synthetic publication failure')
                    write_json(path, value, **kwargs)
                with patch('sys.argv', ['orphanwiki', 'collect', '--language', 'it', '--low-memory', '--batch-size', '1', '--output', str(output)]), \
                        patch('orphanwiki.replicas.replica_options', return_value=('itwiki', {})), \
                        patch('orphanwiki.replicas.pymysql.connect', side_effect=[graph_connection, author_connection]), \
                        patch('orphanwiki.__main__.write_json', side_effect=save):
                    if fail:
                        with self.assertRaises(OSError): main()
                    else:
                        main()
                self.assertTrue(graph_connection.closed and author_connection.closed)
                graph = json.loads(output.read_text())
                checkpoint = json.loads((Path(folder) / 'it-creators.json').read_text())
                self.assertEqual(len(graph['pages']), 3)
                self.assertEqual(len(checkpoint['pages']), 9)
                self.assertEqual(graph['metadata']['creator_collection_status'], 'pending' if fail else 'collected')
                self.assertEqual(graph['metadata']['language'], 'it')
                if not fail:
                    self.assertEqual(graph['creator_data'], checkpoint)
                self.assertFalse(list(Path(folder).glob('orphanwiki-*')))

    def test_sizes_queries_real_census_definition_and_closes_connection(self):
        class SizeCursor:
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def execute(self, sql):
                self.sql = sql
                self.assertions = 'page_namespace=0' in sql and 'page_is_redirect=0' in sql
            def __iter__(self): return iter([(1234, 567890)])
        class SizeConnection:
            closed = False
            cursor_instance = SizeCursor()
            def cursor(self): return self.cursor_instance
            def close(self): self.closed = True
        connection = SizeConnection()
        with patch('orphanwiki.replicas.replica_options', return_value=('roa_tarawiki', {})), \
                patch('orphanwiki.replicas.pymysql.connect', return_value=connection):
            result = collect_sizes('roa-tara')
        self.assertTrue(connection.closed and connection.cursor_instance.assertions)
        self.assertEqual(result['census_pages'], 1234)
        self.assertEqual(result['wikitext_bytes'], 567890)

    def test_size_selection_strict_boundary_generic_reference_and_full_censuses(self):
        from orphanwiki.__main__ import select_by_size
        config = {'version': 1, 'languages': ['x', 'y', 'z', 'it'],
                  'size_selection': {'reference_language': 'x', 'multiplier': 1.5, 'always_include': ['it']}}
        records = [{'language': code, 'census_pages': count} for code, count in [('x', 100), ('y', 149), ('z', 150), ('it', 2000000)]]
        selection = select_by_size(config, records)
        self.assertEqual(selection['selected_languages'], ['x', 'y', 'it'])
        self.assertEqual(selection['excluded_languages'], ['z'])
        self.assertEqual(selection['threshold_pages'], 150)
        for invalid in [records[:-1], records + records[:1], [{**r, 'census_pages': -1} for r in records]]:
            with self.assertRaises(ValueError): select_by_size(config, invalid)
        for multiplier in [0, -1, True, float('inf'), float('nan'), '1.5']:
            invalid = copy.deepcopy(config)
            invalid['size_selection']['multiplier'] = multiplier
            with self.assertRaises(ValueError): select_by_size(invalid, records)

    def test_collect_with_size_policy_queries_all_then_collects_selected_with_creators(self):
        config = {'version': 1, 'languages': ['en', 'fr', 'de', 'it'],
                  'size_selection': {'reference_language': 'en', 'multiplier': 1.5, 'always_include': ['it']}}
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'selection.json'
            write_json(path, config)
            sizes = [{'language': code, 'census_pages': count, 'started_at': '2026-10-05', 'finished_at': '2026-10-05'}
                     for code, count in [('en', 100), ('fr', 149), ('de', 150), ('it', 2000000)]]
            with patch('sys.argv', ['orphanwiki', 'collect', '--languages-file', str(path), '--output-dir', folder, '--low-memory']), \
                    patch('orphanwiki.replicas.collect_sizes', side_effect=sizes) as query, \
                    patch('orphanwiki.__main__.collect_one') as collect:
                main()
            self.assertEqual([call.args[0] for call in query.call_args_list], config['languages'])
            self.assertEqual([call.args[0] for call in collect.call_args_list], ['en', 'fr', 'it'])
            self.assertTrue(all(call.args[6] for call in collect.call_args_list))
            report = json.loads((Path(folder) / 'language-selection.json').read_text())
            self.assertEqual(report['selection']['excluded_languages'], ['de'])
            self.assertEqual(len(report['wikis']), 4)
            self.assertEqual(json.loads(path.read_text()), config)

    def test_size_failure_never_collects_partial_language_selection(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'selection.json'
            write_json(path, {'version': 1, 'languages': ['vec', 'it'],
                             'size_selection': {'reference_language': 'vec', 'multiplier': 1.5}})
            with patch('sys.argv', ['orphanwiki', 'collect', '--languages-file', str(path), '--output-dir', folder]), \
                    patch('orphanwiki.replicas.collect_sizes', side_effect=RuntimeError('connection failed')), \
                    patch('orphanwiki.__main__.collect_one') as collect:
                with self.assertRaises(RuntimeError): main()
            collect.assert_not_called()
            self.assertFalse((Path(folder) / 'language-selection.json').exists())


if __name__ == '__main__':
    unittest.main()
