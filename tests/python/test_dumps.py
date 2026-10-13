"""Offline regressions for dump discovery, resumption and SQL import."""

import gzip
import bz2
import hashlib
import io
import json
import sqlite3
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

from orphanwiki.core import aggregate_index, analyze, attach_history, json_sha256, predict_topics
from orphanwiki.dumps import (_fetch, _history_partitions, fetch_dumps,
                              import_graph, import_history, analyze_imported_dump, predict_imported_topics,
                              _stage_comparison_items)
from orphanwiki._native import (GraphAccumulator, graph_counts,
                                CategoryBatchClassifier, classify_categories)


class Response(io.BytesIO):
    def __init__(self, payload, status=200, headers=None):
        super().__init__(payload)
        self.status = status
        self.headers = {} if headers is None else headers


class DumpTests(unittest.TestCase):
    def test_staged_item_mapping_excludes_ambiguous_and_invalid_qids(self):
        from orphanwiki.core import _comparison_schema
        with closing(sqlite3.connect(':memory:')) as source, closing(sqlite3.connect(':memory:')) as destination:
            source.execute('CREATE TABLE pages(id INTEGER PRIMARY KEY, data TEXT NOT NULL)')
            source.executemany('INSERT INTO pages VALUES(?,?)',
                ((page_id, json.dumps({'wikibase_item': item})) for page_id, item in enumerate(
                    ('Q1', 'Q1', 'Q2', 'Q0', 'Q١', None), 1)))
            _comparison_schema(destination)
            eligible = _stage_comparison_items(source, destination, 'vec', list(range(1, 7)))
            self.assertEqual(eligible, [2])
            self.assertEqual(destination.execute('SELECT item,id FROM items').fetchall(), [('Q2', 3)])
            self.assertEqual(destination.execute('SELECT item FROM ambiguous').fetchall(), [('Q1',)])

    def test_fetch_cli_passes_only_requested_configured_databases(self):
        from orphanwiki.__main__ import main
        config = {'version': 1, 'languages': ['vec', 'lmo'],
                  'databases': {'vec': 'vecwiki', 'lmo': 'lmowiki'}}
        with tempfile.TemporaryDirectory() as folder:
            config_path = Path(folder) / 'languages.json'
            config_path.write_text(json.dumps(config), encoding='utf-8')
            argv = ['orphanwiki', 'fetch-dumps', '--languages-file', str(config_path),
                    '--languages', 'vec', '--sql-release', '20261001',
                    '--history-release', '2026-09', '--root', folder, '--plan']
            with patch.object(sys, 'argv', argv), \
                    patch('orphanwiki.dumps.fetch_dumps') as download:
                main()
            download.assert_called_once_with(
                ['vec'], '20261001', '2026-09', folder,
                {'vec': 'vecwiki'}, plan_only=True)

    def test_bounded_native_graph_and_category_batches_match_bulk(self):
        titles = ['A', 'B', 'C', 'D']
        links = [['R', 'R', 'A', 'missing'], ['A', 'X'], [], ['C', 'B']]
        redirects = {'R': 'B', 'X': 'Y', 'Y': 'X'}
        graph = GraphAccumulator(titles, redirects)
        graph.add_batch(links[:1])
        graph.add_batch(links[1:])
        self.assertEqual(graph.finish(), graph_counts(titles, links, redirects))
        eligible = GraphAccumulator(titles, redirects, [0, 1, 2])
        observed_edges = eligible.add_batch_edges(links[:2]) + eligible.add_batch_edges(links[2:])
        self.assertEqual(set(observed_edges), {(1, 0), (0, 1)})
        self.assertEqual(eligible.finish(), graph_counts(titles, links, redirects))
        with self.assertRaisesRegex(ValueError, 'Troppi blocchi'):
            graph.add_batch([[]])
        pages = [(1, ['Specific', 'Broad']), (2, ['Specific']), (3, []),
                 (4, ['Other', 'Other'])]
        parents = {'Specific': ['Broad'], 'Broad': ['Specific']}
        rules = ([], set(), {'Other': 2.0}, {'2': 'Specific'}, 5)
        categories = CategoryBatchClassifier(len(pages), parents, rules)
        with self.assertRaisesRegex(ValueError, 'incomplete'):
            categories.classify_batch(pages[:1])
        categories.add_support_batch([labels for _, labels in pages[:2]])
        categories.add_support_batch([labels for _, labels in pages[2:]])
        self.assertEqual(categories.classify_batch(pages[:1]) + categories.classify_batch(pages[1:]),
                         classify_categories(pages, parents, rules))

    def test_aggregate_preserves_independent_dump_intervals(self):
        def result(language, source):
            return {'metadata': {'language': language, 'complete': True, 'demo': False,
                                 'source': source, 'age_rule': 'unavailable',
                                 'started_at': '2026-10-01T00:00:00+00:00',
                                 'finished_at': '2026-10-02T00:00:00+00:00'},
                    'pages': [{'page_id': 1}]}
        aggregate = aggregate_index({
            'vec': result('vec', 'Wikimedia SQL dumps / vecwiki'),
            'lmo': result('lmo', 'Wikimedia SQL dumps / lmowiki')},
            {'vec': 'vec.json', 'lmo': 'lmo.json'})
        self.assertIn('SQL dumps', aggregate['metadata']['source'])
        self.assertEqual(aggregate['metadata']['temporal_consistency'],
                         'independent_source_specific_intervals')

    def test_history_join_keeps_reconstructed_and_observed_dates_separate(self):
        date = '2026-03-01T12:00:00+00:00'
        rows = [{'page_id': 1, 'page_created_at': date,
                 'first_public_revision_at': date, 'creator_key': 'vecwiki:actor:9'},
                {'page_id': 2, 'page_created_at': None,
                 'first_public_revision_at': date, 'creator_key': None}]
        history = {'metadata': {'language': 'vec', 'database': 'vecwiki',
                                'source': 'MediaWiki History dumps / vecwiki',
                                'history_release': '2026-09', 'sql_release': '20261001',
                                'complete': True},
                   'pages': [{'page_id': page_id, 'article_created_at': date,
                              'creator_attribution': 'history_initial_revision_observed',
                              'creator_history_status': 'observed_surviving_history',
                              'creator_key': 'vecwiki:history-user:42',
                              'creator_groups_at_creation': ['sysop'],
                              'creator_continuity_months_before_creation': 2,
                              'creator_continuity_bucket_before_creation': 'two',
                              'creator_prior_edits_main_history': 10,
                              'creator_prior_edits_other_history': 0}
                             for page_id in (1, 2)]}
        meta = attach_history(rows, {'language': 'vec', 'database': 'vecwiki',
                                     'source': 'Wikimedia SQL dumps / vecwiki',
                                     'sql_release': '20261001'}, history)
        self.assertEqual(meta['matched_pages'], 2)
        self.assertEqual(rows[0]['creator_key'], 'vecwiki:actor:9')
        self.assertEqual(rows[0]['creator_groups_at_creation_history'], ['sysop'])
        self.assertEqual(rows[1]['history_data_status'], 'matched_reconstructed_history')
        self.assertIsNone(rows[1]['page_created_at'])
        self.assertEqual(rows[1]['history_first_revision_at'], date)
        raw = {'metadata': {'language': 'vec', 'database': 'vecwiki',
                            'source': 'Wikimedia SQL dumps / vecwiki',
                            'sql_release': '20261001', 'complete': True,
                            'started_at': '2026-10-01T00:00:00+00:00',
                            'finished_at': '2026-10-02T00:00:00+00:00'},
               'pages': [{'page_id': 1, 'title': 'A', 'links': [], 'categories': []},
                         {'page_id': 2, 'title': 'B', 'links': [], 'categories': []}],
               'redirects': {}, 'category_parents': {}}
        result = analyze(raw, history=history)
        self.assertEqual(result['metadata']['historical_age_rule'],
                         'mediawiki_history_first_observed_revision_v1')
        self.assertTrue(all(page['age_days'] is None for page in result['pages']))
        self.assertTrue(all(page['history_first_revision_age_days'] > 0 for page in result['pages']))
        self.assertTrue(all(page['created_at'] is None for page in result['pages']))

    def test_history_partitions_are_complete_and_nonoverlapping(self):
        prefix = '2026-09.vecwiki.'
        suffix = '.tsv.bz2'
        names = [prefix + '2001' + suffix, prefix + '2002-02' + suffix]
        with self.assertRaisesRegex(ValueError, 'mancanti'):
            _history_partitions(names, '2026-09', 'vecwiki')
        self.assertEqual(len(_history_partitions([prefix + 'all-time' + suffix],
                                                  '2026-09', 'vecwiki')), 1)
        with self.assertRaisesRegex(ValueError, 'sovrapposte'):
            _history_partitions([prefix + '2001' + suffix,
                                 prefix + '2001-01' + suffix], '2026-09', 'vecwiki')
        months = [prefix + f'2001-{month:02}' + suffix for month in range(1, 13)]
        months.append(prefix + '2002' + suffix)
        self.assertEqual(len(_history_partitions(months, '2026-09', 'vecwiki')), 13)

    def test_resume_requires_matching_content_range_and_checksum(self):
        payload = b'0123456789'
        checksum = hashlib.sha1(payload).hexdigest()
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'sample.sql.gz'
            (Path(folder) / 'sample.sql.gz.part').write_bytes(payload[:4])
            with patch('orphanwiki.dumps._open', return_value=Response(
                    payload[4:], 206, {'Content-Range': 'bytes 4-9/10'})):
                digest = _fetch('https://dumps.wikimedia.org/sample.sql.gz', path,
                                len(payload), checksum)
            self.assertEqual(path.read_bytes(), payload)
            self.assertEqual(digest, hashlib.sha256(payload).hexdigest())
            path.unlink()
            (Path(folder) / 'sample.sql.gz.part').write_bytes(payload[:4])
            with patch('orphanwiki.dumps._open', return_value=Response(
                    payload[4:], 206, {'Content-Range': 'bytes 3-9/10'})):
                with self.assertRaisesRegex(ValueError, 'Range inattesa'):
                    _fetch('https://dumps.wikimedia.org/sample.sql.gz', path,
                           len(payload), checksum)

    def test_manifest_uses_complete_edition_and_database_override(self):
        from orphanwiki.dumps import SQL_TABLES
        release = '20261001'
        database = 'be_x_oldwiki'
        details = {}
        for table in SQL_TABLES:
            name = f'{database}-{release}-{table}.sql.gz'
            details[name] = {'url': f'/{database}/{release}/{name}',
                             'size': 1, 'sha1': '0' * 40}
        status = {'jobs': {'one': {'status': 'done', 'files': details}}}
        names = ['2026-09.be_x_oldwiki.all-time.tsv.bz2']
        with tempfile.TemporaryDirectory() as folder, \
                patch('orphanwiki.dumps._json', return_value=status), \
                patch('orphanwiki.dumps._names', return_value=names), \
                patch('orphanwiki.dumps._open', return_value=Response(b'', headers={'Content-Length': '1'})), \
                patch('orphanwiki.dumps._fetch', return_value='1' * 64), \
                patch('orphanwiki.dumps._verify_bzip2'):
            result = fetch_dumps(['be-tarask'], release, '2026-09', folder,
                                 {'be-tarask': database})
            manifest = Path(folder) / database / f'manifest-{release}-2026-09.json'
            self.assertEqual(result[0]['database'], database)
            self.assertEqual(len(result[0]['files']), 8)
            self.assertEqual(json.loads(manifest.read_text())['files']['page']['bytes'], 1)

    def test_plan_reports_space_without_downloading_or_publishing(self):
        from orphanwiki.dumps import SQL_TABLES
        release, database = '20261001', 'vecwiki'
        files = {f'{database}-{release}-{table}.sql.gz': {
            'url': f'/{database}/{release}/{database}-{release}-{table}.sql.gz',
            'size': 10, 'sha1': '0' * 40} for table in SQL_TABLES}
        with tempfile.TemporaryDirectory() as folder, \
                patch('orphanwiki.dumps._json', return_value={'jobs': {'sql': {'status': 'done', 'files': files}}}), \
                patch('orphanwiki.dumps._names', return_value=['2026-09.vecwiki.all-time.tsv.bz2']), \
                patch('orphanwiki.dumps._open', return_value=Response(b'', headers={'Content-Length': '100'})), \
                patch('orphanwiki.dumps._fetch') as download:
            plan = fetch_dumps(['vec'], release, '2026-09', folder, plan_only=True)
            self.assertEqual(plan[0]['compressed_bytes'], 170)
            self.assertEqual(plan[0]['history_partitions'], 1)
            self.assertFalse(list(Path(folder).rglob('manifest-*.json')))
            download.assert_not_called()

    def test_import_keeps_isolated_pages_and_resolved_link_inputs(self):
        from orphanwiki.dumps import SQL_TABLES
        schemas = {
            'page': ('page_id,page_namespace,page_title,page_is_redirect,page_len,page_latest',
                     "(1,0,'First',0,11,5),(2,0,'Second',0,12,6),(3,0,'Redirect',1,1,7)"),
            'pagelinks': ('pl_from,pl_from_namespace,pl_target_id', '(1,0,10)'),
            'linktarget': ('lt_id,lt_namespace,lt_title', "(10,0,'Second')"),
            'redirect': ('rd_from,rd_namespace,rd_title,rd_interwiki', "(3,0,'Second','')"),
            'page_props': ('pp_page,pp_propname,pp_value', "(1,'wikibase_item','Q1'),(2,'wikibase_item','Q2')"),
            'categorylinks': ('cl_from,cl_target_id', '(1,11)'),
            'user_groups': ('ug_user,ug_group,ug_expiry', "(42,'bot',NULL)"),
        }
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            files = {}
            for table in SQL_TABLES:
                columns, values = schemas[table]
                if table == 'linktarget':
                    values += ",(11,14,'Topics')"
                path = root / f'{table}.sql.gz'
                body = (f'CREATE TABLE `{table}` (\n' +
                        ''.join(f'  `{column}` text,\n' for column in columns.split(',')) +
                        f');\nINSERT INTO `{table}` VALUES\n{values};\n')
                with gzip.open(path, 'wb') as stream:
                    stream.write(body.encode())
                files[table] = {'path': str(path), 'bytes': path.stat().st_size,
                                'sha256_local': hashlib.sha256(path.read_bytes()).hexdigest()}
            manifest = root / 'manifest.json'
            manifest.write_text(json.dumps({'language': 'vec', 'database': 'vecwiki',
                                            'sql_release': '20261001', 'files': files}))
            output = root / 'out.json'
            self.assertEqual(import_graph(manifest, output, root / 'staging'), 2)
            raw = json.loads(output.read_text())
            self.assertEqual([page['page_id'] for page in raw['pages']], [1, 2])
            self.assertEqual(raw['pages'][0]['links'], ['Second'])
            self.assertEqual(raw['pages'][0]['categories'], ['Topics'])
            self.assertIsNone(raw['pages'][0]['created_at'])
            taxonomy = json.loads(Path('config/topics.json').read_text())

            class Model:
                def get_word_id(self, _item):
                    return 1

                def predict(self, texts, k, threshold):
                    return ([[f'__label__{label.replace(" ", "_")}' for label in taxonomy['labels']]
                             for _ in texts], [[0.8] + [0.1] * 63 for _ in texts])

            topic_path = root / 'vec-topics.json'
            inferred = predict_imported_topics(output, topic_path, Model(), taxonomy, 1,
                                                work_dir=root / 'staging')
            expected_topics = predict_topics(raw, Model(), taxonomy)
            self.assertEqual(inferred['input_sha256'], json_sha256(raw))
            self.assertEqual(json.loads(topic_path.read_text())['pages'], expected_topics['pages'])
            self.assertEqual(inferred['input_file_sha256'], hashlib.sha256(output.read_bytes()).hexdigest())
            analyzed = analyze(raw)
            self.assertEqual(analyzed['metadata']['age_rule'],
                             'unavailable_no_approved_creation_event')
            self.assertTrue(all(page['age_days'] is None for page in analyzed['pages']))
            staged = root / 'staged' / 'vec.json'
            analyze_imported_dump(output, None, staged, {}, root / 'staging')
            from_stage = json.loads(staged.read_text())
            self.assertEqual(from_stage['pages'], analyzed['pages'])
            self.assertEqual(from_stage['metadata']['unresolved_target_occurrences'],
                             analyzed['metadata']['unresolved_target_occurrences'])
            history_path = root / 'vec-history.json'
            history = {'metadata': {'language': 'vec', 'database': 'vecwiki',
                                    'source': 'MediaWiki History dumps / vecwiki',
                                    'history_release': '2026-09', 'sql_release': '20261001',
                                    'complete': True},
                       'pages': [
                           {'page_id': 1, 'article_created_at': '2020-03-01T12:00:00+00:00',
                            'creator_attribution': 'history_initial_revision_observed',
                            'creator_history_status': 'observed_surviving_history',
                            'creator_key': 'vecwiki:history-user:42',
                            'creator_groups_at_creation': ['sysop'],
                            'creator_prior_edits_main_history': 3},
                           {'page_id': 2, 'article_created_at': None,
                            'creator_history_status': 'no_surviving_revision'}]}
            history_path.write_text(json.dumps(history))
            analyze_imported_dump(output, history_path, staged, {}, root / 'staging')
            with_history = json.loads(staged.read_text())
            ordinary = analyze(raw, history=history)
            self.assertEqual(with_history['pages'], ordinary['pages'])
            self.assertEqual(with_history['metadata']['historical_age_rule'],
                             ordinary['metadata']['historical_age_rule'])
            from orphanwiki.core import _comparison_analysis, _comparison_schema, _stage_comparison
            with closing(sqlite3.connect(root / 'bounded-comparison.sqlite')) as bounded, \
                    closing(sqlite3.connect(root / 'ordinary-comparison.sqlite')) as regular:
                _comparison_schema(bounded)
                _comparison_schema(regular)
                analyze_imported_dump(output, history_path, root / 'unused-comparison.json', {},
                                      root / 'staging', comparison_connection=bounded)
                _stage_comparison(regular, _comparison_analysis(raw, {}, None, history))
                for table in ('pages', 'items', 'ambiguous', 'edges'):
                    self.assertEqual(bounded.execute(f'SELECT * FROM {table} ORDER BY 1,2').fetchall(),
                                     regular.execute(f'SELECT * FROM {table} ORDER BY 1,2').fetchall())
                self.assertEqual(bounded.execute('SELECT target,source FROM edges').fetchall(), [(2, 1)])
                self.assertFalse((root / 'unused-comparison.json').exists())
            self.assertIsNone(with_history['pages'][0]['age_days'])
            self.assertGreater(with_history['pages'][0]['history_first_revision_age_days'], 0)
            self.assertIsNone(with_history['pages'][1]['history_first_revision_age_days'])
            topic_metadata = {'language': 'vec', 'source': 'local_model_on_verified_SQL_dump_outlinks',
                              'complete': True, 'graph_finished_at': raw['metadata']['finished_at'],
                              'input_file_sha256': hashlib.sha256(output.read_bytes()).hexdigest(),
                              'model': taxonomy, 'predicted_pages': 1, 'missing_pages': 1}
            topic_records = [
                {'page_id': 1, 'revision_id': 5, 'topic_labels': [taxonomy['labels'][0]],
                 'topic_status': 'predicted', 'topic_input_count': 1},
                {'page_id': 2, 'revision_id': 6, 'topic_labels': None,
                 'topic_status': 'no_mapped_outlinks', 'topic_input_count': 0},
            ]
            topic_path.write_text(json.dumps({'metadata': topic_metadata, 'pages': topic_records}))
            analyze_imported_dump(output, None, staged, {}, root / 'staging', topic_path)
            with_topics = json.loads(staged.read_text())
            self.assertEqual(with_topics['pages'][0]['topic_labels'], [taxonomy['labels'][0]])
            self.assertEqual(with_topics['pages'][1]['topic_status'], 'no_mapped_outlinks')
            self.assertTrue(with_topics['metadata']['topics']['input_checksum_verified'])
            topic_metadata['input_file_sha256'] = '0' * 64
            topic_path.write_text(json.dumps({'metadata': topic_metadata, 'pages': topic_records}))
            with self.assertRaisesRegex(ValueError, 'Temi non corrispondenti'):
                analyze_imported_dump(output, None, staged, {}, root / 'staging', topic_path)
            self.assertEqual(json.loads(staged.read_text()), with_topics)
            previous_manifest = (staged.parent / 'manifest.json').read_text()
            staged.write_text('previous complete export')
            with patch('orphanwiki.__main__.export_result', side_effect=RuntimeError('preparation failed')):
                with self.assertRaisesRegex(RuntimeError, 'preparation failed'):
                    analyze_imported_dump(output, None, staged, {}, root / 'staging')
            self.assertEqual(staged.read_text(), 'previous complete export')
            self.assertEqual((staged.parent / 'manifest.json').read_text(), previous_manifest)

    def test_history_continuity_and_groups_stay_distinct_from_paws(self):
        def row(entity, kind, timestamp, revision='', parent='', page='1', group=''):
            fields = [''] * 78
            fields[0] = 'vecwiki'
            fields[2:5] = [entity, kind, timestamp]
            fields[6] = '42'
            fields[12] = group
            fields[28] = page
            fields[31] = '0'
            fields[60] = revision
            fields[61] = parent
            if revision == '100':
                fields[37:39] = [timestamp, timestamp]
            return '\t'.join(fields)

        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            stage = root / 'stage'
            stage.mkdir()
            with closing(sqlite3.connect(stage / 'vecwiki-20261001.sqlite')) as database:
                database.execute('CREATE TABLE dump_page(id INTEGER, ns INTEGER, redirect INTEGER)')
                database.execute('INSERT INTO dump_page VALUES(1,0,0)')
                database.commit()
            lines = []
            for day in range(1, 6):
                lines.append(row('revision', 'create', f'2026-01-{day:02} 10:00:00.0',
                                 str(day), '1', '999'))
                lines.append(row('revision', 'create', f'2026-02-{day:02} 10:00:00.0',
                                 str(day + 10), '1', '999'))
            created = '2026-03-01 12:00:00.0'
            lines += [row('page', 'create', created),
                      row('revision', 'create', created, '100', '0', group='sysop')]
            path = root / '2026-09.vecwiki.all-time.tsv.bz2'
            path.write_bytes(bz2.compress(('\n'.join(lines) + '\n').encode()))
            manifest = root / 'manifest.json'
            manifest.write_text(json.dumps({
                'language': 'vec', 'database': 'vecwiki',
                'sql_release': '20261001', 'history_release': '2026-09',
                'files': {path.name: {'path': str(path), 'bytes': path.stat().st_size,
                                      'sha256_local': hashlib.sha256(path.read_bytes()).hexdigest()}}}))
            output = root / 'history.json'
            self.assertEqual(import_history(manifest, output, stage), 1)
            record = json.loads(output.read_text())['pages'][0]
            self.assertEqual(record['creator_attribution'], 'history_initial_revision_observed')
            self.assertEqual(record['creator_groups_at_creation'], ['sysop'])
            self.assertEqual(record['creator_continuity_months_before_creation'], 2)
            self.assertEqual(record['creator_prior_edits_main_history'], 10)
            self.assertNotIn('page_created_at', record)
            self.assertEqual(import_history(manifest, output, stage), 1)


if __name__ == '__main__':
    unittest.main()
