"""Offline regressions for dump discovery, resumption and SQL import."""

import gzip
import bz2
import copy
import hashlib
import io
import json
import sqlite3
import sys
import tempfile
import unittest
from contextlib import closing, redirect_stdout, redirect_stderr
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
    def test_sql_export_streams_relations_without_queries_per_article(self):
        from orphanwiki.dumps import DiskPages
        with closing(sqlite3.connect(':memory:')) as db:
            pages = DiskPages(db)
            expected = []
            for pid in range(1, 1101):
                page = {'page_id': pid * 2, 'title': f'Vòce {pid}', 'revision_id': pid}
                pages[pid * 2] = page
                links = ['Target', f'Àlias {pid}', 'Target'] if pid % 3 else []
                categories = ['Category B', 'Category A'] if pid % 2 else []
                for target in links:
                    pages.add('links', pid * 2, target)
                for category in categories:
                    pages.add('categories', pid * 2, category)
                expected.append({**page, 'links': links, 'categories': categories})
            pages.prepare_export()
            selects = []
            db.set_trace_callback(lambda sql: selects.append(sql) if sql.startswith('SELECT') else None)
            self.assertEqual(list(pages.export_pages()), expected)
            self.assertLessEqual(len(selects), 3)

    def test_history_cli_passes_operational_controls_without_changing_sources(self):
        from orphanwiki.__main__ import main
        with tempfile.TemporaryDirectory() as folder:
            manifest = Path(folder) / 'manifest.json'
            manifest.write_text(json.dumps({'language': 'vec', 'database': 'vecwiki'}))
            for command in ('import-dumps', 'import-history'):
                with self.subTest(command=command):
                    argv = ['orphanwiki', command, '--manifest', str(manifest),
                            '--work-dir', folder, '--history-cache-mib', '1024',
                            '--history-commit-rows', '100000']
                    with patch.object(sys, 'argv', argv), \
                            patch('orphanwiki.dumps.import_graph') as graph_import, \
                            patch('orphanwiki.dumps.import_history') as history_import:
                        main()
                    history_import.assert_called_once_with(
                        str(manifest), 'data/imported/vec-history.json', folder,
                        cache_mib=1024, commit_rows=100000)
                    self.assertEqual(graph_import.call_count, int(command == 'import-dumps'))
            for option, value in (('--history-cache-mib', '0'), ('--history-cache-mib', '65537'),
                                  ('--history-commit-rows', '0'), ('--history-commit-rows', '10000001')):
                with self.subTest(option=option, value=value), patch.object(sys, 'argv', [
                        'orphanwiki', 'import-history', '--manifest', 'does-not-exist', option, value]), \
                        redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                    main()
                self.assertEqual(error.exception.code, 2)

    def test_history_deduplication_preserves_conflicts_and_initial_revisions(self):
        from orphanwiki.dumps import _history_schema, _history_scan, _history_records
        def rows():
            for cycle in range(3):
                for pid in range(1, 6002):
                    row = [''] * 78
                    row[0], row[2], row[3], row[4] = 'vecwiki', 'revision', 'create', '2019-01-01 00:00:00.0'
                    row[6], row[28], row[37] = '42', str(pid), row[4]
                    row[60], row[61] = str(pid + cycle * 6001), '0' if cycle == 0 else '1'
                    row[63] = 'user' if pid == 2 else ''
                    yield row
            row = row.copy()
            row[28], row[37], row[61] = '1', '2020-01-01 00:00:00.0', '1'
            yield row
            row = row.copy()
            row[28], row[37] = '3', 'invalid'
            yield row
        snapshots = []
        for commit_rows, cache_size in ((1, 2), (250000, 100000)):
            with self.subTest(commit_rows=commit_rows), closing(sqlite3.connect(':memory:')) as db:
                _history_schema(db)
                creation_inserts, scratch_inserts = [], []
                def trace(sql):
                    if sql.startswith(('INSERT INTO history_creation SELECT',
                                       'INSERT OR IGNORE INTO history_creation VALUES')):
                        creation_inserts.append(1)
                    elif sql.startswith('INSERT INTO history_creation_pending VALUES'):
                        scratch_inserts.append(1)
                db.set_trace_callback(trace)
                log = io.StringIO()
                with patch('orphanwiki.dumps._history_rows', side_effect=lambda *args: rows()), \
                        patch('orphanwiki.dumps.HISTORY_CREATION_CACHE', cache_size), redirect_stdout(log):
                    _history_scan(db, 'partition', None, 'digest', 'initial', 'vecwiki',
                                  set(range(1, 6002)), commit_rows=commit_rows)
                snapshot = {table: db.execute(f'SELECT * FROM {table} ORDER BY 1,2,3').fetchall()
                            for table in ('history_initial', 'history_creation', 'history_imported')}
                snapshots.append(snapshot)
                self.assertEqual(len(snapshot['history_initial']), 6001)
                self.assertEqual(len(snapshot['history_creation']), 6003)
                records = _history_records(db, {1, 2, 3}, 'vecwiki')
                self.assertEqual(records[0]['creation_date_status'], 'ambiguous_history_creation')
                self.assertEqual(records[1]['creator_attribution'], 'history_hidden_or_unregistered_creator')
                self.assertEqual(records[2]['creation_date_status'], 'ambiguous_history_creation')
                self.assertEqual(len(creation_inserts), 6003 if cache_size == 100000 else 2)
                self.assertIn('6,003 creazioni finali', log.getvalue())
                if cache_size == 100000:
                    self.assertEqual(len(scratch_inserts), 0)
                else:
                    self.assertGreater(len(scratch_inserts), 6003)
                with patch('orphanwiki.dumps._history_rows', side_effect=AssertionError('must reuse checkpoint')), \
                        redirect_stdout(io.StringIO()):
                    _history_scan(db, 'partition', None, 'digest', 'initial', 'vecwiki',
                                  set(range(1, 6002)), commit_rows=5000)
        self.assertEqual(snapshots[0], snapshots[1])

    def test_history_interrupted_partition_retries_without_duplicates_or_losing_completed_files(self):
        from orphanwiki.dumps import _history_schema, _history_scan
        def rows(limit, fail=False):
            for pid in range(1, limit + 1):
                row = [''] * 78
                row[0], row[2:5] = 'vecwiki', ['revision', 'create', '2020-01-01 00:00:00.0']
                row[6], row[28], row[31], row[37], row[60], row[61] = '42', str(pid), '0', row[4], str(pid), '0'
                yield row
            if fail:
                raise ValueError('synthetic truncated partition')
        for phase, table in (('initial', 'history_initial'), ('activity', 'history_edit')):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as folder:
                path = Path(folder) / 'stage.sqlite'
                with closing(sqlite3.connect(path)) as db, redirect_stdout(io.StringIO()):
                    _history_schema(db)
                    with patch('orphanwiki.dumps._history_rows', side_effect=lambda *args: rows(2)):
                        _history_scan(db, 'complete', None, 'old', phase, 'vecwiki', {1, 2}, {42})
                    with patch('orphanwiki.dumps._history_rows', side_effect=lambda *args: rows(5001, fail=True)), \
                            self.assertRaisesRegex(ValueError, 'truncated'):
                        _history_scan(db, 'partial', None, 'new', phase, 'vecwiki',
                                      set(range(1, 6002)), {42}, commit_rows=1)
                with closing(sqlite3.connect(path)) as db, redirect_stdout(io.StringIO()):
                    self.assertEqual(db.execute(f'SELECT COUNT(*) FROM {table} WHERE file="partial"').fetchone()[0], 5000)
                    self.assertIsNone(db.execute('SELECT 1 FROM history_imported WHERE file="partial"').fetchone())
                    with patch('orphanwiki.dumps._history_rows', side_effect=lambda *args: rows(6001)):
                        _history_scan(db, 'partial', None, 'new', phase, 'vecwiki',
                                      set(range(1, 6002)), {42}, commit_rows=250000)
                    self.assertEqual(db.execute(f'SELECT COUNT(*) FROM {table} WHERE file="partial"').fetchone()[0], 6001)
                    self.assertEqual(db.execute(f'SELECT COUNT(*) FROM {table} WHERE file="complete"').fetchone()[0], 2)
                    self.assertEqual(db.execute('SELECT COUNT(*) FROM history_imported').fetchone()[0], 2)
                    if phase == 'initial':
                        self.assertEqual(db.execute('SELECT COUNT(*) FROM history_creation WHERE file="partial"').fetchone()[0], 6001)

    def test_history_creation_merge_and_checkpoint_failure_retry_only_incomplete_partition(self):
        from orphanwiki.dumps import _history_schema, _history_scan

        class CheckpointConnection(sqlite3.Connection):
            fail_checkpoint = False

            def commit(self):
                if self.fail_checkpoint and self.execute(
                        "SELECT 1 FROM history_imported WHERE file='partial'").fetchone():
                    raise sqlite3.OperationalError('synthetic checkpoint commit')
                return super().commit()

        def rows(limit):
            for pid in range(1, limit + 1):
                row = [''] * 78
                row[0], row[2:5] = 'vecwiki', ['revision', 'create', '2020-01-01 00:00:00.0']
                row[6], row[28], row[37], row[60], row[61] = '42', str(pid), row[4], str(pid), '0'
                yield row

        for failure in ('merge', 'checkpoint'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as folder, \
                    patch('orphanwiki.dumps.HISTORY_CREATION_CACHE', 2):
                path = Path(folder) / 'stage.sqlite'
                with closing(sqlite3.connect(path, factory=CheckpointConnection)) as db, redirect_stdout(io.StringIO()):
                    _history_schema(db)
                    with patch('orphanwiki.dumps._history_rows', side_effect=lambda *args: rows(2)):
                        _history_scan(db, 'complete', None, 'old', 'initial', 'vecwiki', {1, 2})
                    if failure == 'merge':
                        db.execute("""CREATE TEMP TRIGGER fail_merge BEFORE INSERT ON history_creation
                                      WHEN NEW.file='partial' AND NEW.page_id>5000
                                      BEGIN SELECT RAISE(ABORT,'synthetic final merge'); END""")
                    else:
                        db.fail_checkpoint = True
                    with patch('orphanwiki.dumps._history_rows', side_effect=lambda *args: rows(6001)), \
                            self.assertRaisesRegex(sqlite3.Error, 'synthetic'):
                        _history_scan(db, 'partial', None, 'new', 'initial', 'vecwiki',
                                      set(range(1, 6002)), commit_rows=1)
                    self.assertIsNone(db.execute("SELECT 1 FROM sqlite_temp_master WHERE name IN ('history_creation_pending','history_creation_ordered')").fetchone())
                with closing(sqlite3.connect(path)) as db, redirect_stdout(io.StringIO()):
                    self.assertIsNone(db.execute("SELECT 1 FROM history_imported WHERE file='partial'").fetchone())
                    self.assertEqual(db.execute("SELECT COUNT(*) FROM history_creation WHERE file='partial'").fetchone()[0],
                                     5000 if failure == 'merge' else 6001)
                    with patch('orphanwiki.dumps._history_rows', side_effect=lambda *args: rows(6001)):
                        _history_scan(db, 'partial', None, 'new', 'initial', 'vecwiki',
                                      set(range(1, 6002)), commit_rows=250000)
                    self.assertEqual(db.execute("SELECT COUNT(*) FROM history_creation WHERE file='partial'").fetchone()[0], 6001)
                    self.assertEqual(db.execute("SELECT COUNT(*) FROM history_creation WHERE file='complete'").fetchone()[0], 2)
                    self.assertEqual(db.execute('SELECT COUNT(*) FROM history_imported').fetchone()[0], 2)

    def test_history_progress_includes_scanned_rows_even_when_none_are_retained(self):
        from orphanwiki.dumps import _history_schema, _history_scan
        with closing(sqlite3.connect(':memory:')) as db:
            _history_schema(db)
            row = [''] * 78
            row[28] = '999'
            log = io.StringIO()
            with patch('orphanwiki.dumps._history_rows', return_value=iter([row] * 10000)), \
                    patch('orphanwiki.dumps.time.monotonic', side_effect=range(0, 10000, 31)), redirect_stdout(log):
                _history_scan(db, 'empty', None, 'digest', 'initial', 'vecwiki', {1})
            messages = log.getvalue().splitlines()
            self.assertTrue(any('10,000 righe lette; 0 righe utili' in message and 'COMPLETATO' not in message
                                for message in messages))
            self.assertIn('COMPLETATO', messages[-1])

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

    def test_fetch_reuse_verifies_both_digests_in_one_file_read(self):
        from orphanwiki.dumps import _verified_sha256
        payload = b'official synthetic content' * 100
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'content.xml.bz2'
            path.write_bytes(payload)
            sha1 = hashlib.sha1(payload).hexdigest()
            sha256 = hashlib.sha256(payload).hexdigest()
            with patch('orphanwiki.dumps._open', side_effect=AssertionError('network forbidden')), \
                    patch('orphanwiki.dumps._verified_sha256', wraps=_verified_sha256) as verify:
                self.assertEqual(_fetch('https://dumps.wikimedia.org/synthetic', path,
                                        len(payload), sha1, sha256), sha256)
                self.assertEqual(verify.call_count, 1)
            self.assertIsNone(_verified_sha256(path, sha1, '0' * 64))
            self.assertIsNone(_verified_sha256(path, '0' * 40, sha256))

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
            self.assertEqual(record['page_created_at'], '2026-03-01T12:00:00+00:00')
            self.assertEqual(import_history(manifest, output, stage), 1)

    def test_creation_from_all_events_without_visible_initial_creator(self):
        from orphanwiki.dumps import _history_schema, _history_scan, _history_records, HISTORY_RULE
        def event(pid, created, initial=False, hidden=False):
            row = [''] * 78
            row[0], row[2], row[3], row[4] = 'vecwiki', 'revision', 'create', '2020-01-01 00:00:00.0'
            row[6], row[28], row[37] = '42', str(pid), created
            row[60], row[61] = str(pid), '0' if initial else '1'
            row[63] = 'user' if hidden else ''
            return '\t'.join(row)
        with tempfile.TemporaryDirectory() as folder, closing(sqlite3.connect(':memory:')) as db:
            _history_schema(db)
            path = Path(folder) / 'history.bz2'
            path.write_bytes(bz2.compress(('\n'.join([
                event(1, '2020-01-01 00:00:00.0', True, True),
                event(2, '2019-01-01 00:00:00.0'),
                event(3, '2019-01-01 00:00:00.0'), event(3, '2020-01-01 00:00:00.0'),
                event(4, 'invalid'), event(5, '')]) + '\n').encode()))
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            # Legacy checkpoint must not conceal new evidence columns.
            db.execute('INSERT INTO history_imported VALUES(?,?,?)', ('history', 'initial', digest))
            db.commit()
            _history_scan(db, 'history', path, digest, 'initial', 'vecwiki', set(range(1, 6)))
            records = _history_records(db, set(range(1, 6)), 'vecwiki')
            self.assertEqual(records[0]['page_created_at'], '2020-01-01T00:00:00+00:00')
            self.assertIsNone(records[0]['creator_key'])
            self.assertEqual(records[1]['page_created_at'], '2019-01-01T00:00:00+00:00')
            self.assertIsNone(records[1]['article_created_at'])
            self.assertEqual(records[2]['creation_date_status'], 'ambiguous_history_creation')
            self.assertIsNone(records[3]['page_created_at'])
            self.assertIsNone(records[4]['page_created_at'])
            self.assertEqual(db.execute('SELECT sha256 FROM history_imported').fetchone()[0], digest + ':' + HISTORY_RULE)
            with patch('orphanwiki.dumps._history_rows', side_effect=AssertionError('already scanned')):
                _history_scan(db, 'history', path, digest, 'initial', 'vecwiki', set(range(1, 6)))

    def test_quality_regex_reference_and_native_formula(self):
        from orphanwiki.dumps import quality_features
        from orphanwiki._native import quality_transforms, quality_scores
        spec = json.loads(Path('config/quality.json').read_text())
        text = 'è<ref>a</ref><ref name="a"/>\n==A==\n===B===\n====C====\n[[Foo]][[Category:C]][[Imàjine:X]]'
        self.assertEqual(quality_features(text, 'vec', spec), [len(text.encode()), 2, 2, 1, 1, 1])
        self.assertEqual(quality_features('[[PictureLocal:X]][[TopicLocal:Y]]', 'vec', spec,
                                         {6: 'PictureLocal', 14: 'TopicLocal'})[3:], [0, 1, 1])
        # Preserve the published regex extractor's comment/nowiki limitation.
        self.assertEqual(quality_features('<!--<ref>x</ref>--><nowiki>[[Foo]]</nowiki>', 'vec', spec)[1:4], [1, 0, 1])
        self.assertEqual(quality_transforms([[10000, 15, 10, 100, 2, 5]])[0], [100, .15, .1, .1, 2, 5])
        self.assertAlmostEqual(quality_scores([[10000, 15, 10, 100, 2, 5]], spec['minimums'])[0], .998)
        self.assertEqual(quality_scores([[0, 0, 0, 0, 0, 0]], spec['minimums']), [0.0])
        with self.assertRaises(ValueError):
            quality_scores([[1] * 6], [0] * 6)

    def test_creator_registration_bot_snapshot_and_strict_prior_census(self):
        from orphanwiki.dumps import _stage_schema, HISTORY_RULE
        def revision(pid, at, registered, user=42):
            row = [''] * 78
            row[0], row[2:5] = 'vecwiki', ['revision', 'create', at]
            row[6], row[12], row[23], row[28] = str(user), 'sysop', registered, str(pid)
            row[37], row[38], row[60], row[61] = at, at, str(pid), '0'
            return '\t'.join(row)
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            stage = root / 'stage'
            stage.mkdir()
            with closing(sqlite3.connect(stage / 'vecwiki-20261001.sqlite')) as db:
                _stage_schema(db)
                db.executemany('INSERT INTO dump_page VALUES(?,?,?,?,?,?)', ((i, 0, str(i), 0, 0, i) for i in range(1, 5)))
                db.execute("INSERT INTO imported VALUES('user_groups','verified')")
                db.executemany('INSERT INTO dump_group VALUES(?,?,?)', [(42, 'bot', '20250101000000'), (43, 'bot', None)])
                db.commit()
            at = '2020-01-01 00:00:00.0'
            lines = [revision(1, at, '2019-01-01 00:00:00.0'), revision(2, at, '2019-01-01 00:00:00.0'),
                     revision(3, '2021-01-01 00:00:00.0', '2019-01-01 00:00:00.0'),
                     revision(4, at, '2021-01-01 00:00:00.0', 43)]
            path = root / 'history.tsv.bz2'
            path.write_bytes(bz2.compress(('\n'.join(lines)+'\n').encode()))
            manifest = root / 'manifest.json'
            manifest.write_text(json.dumps({'language': 'vec', 'database': 'vecwiki', 'sql_release': '20261001',
                'history_release': '2026-09', 'files': {path.name: {'path': str(path), 'bytes': path.stat().st_size,
                        'sha256_local': hashlib.sha256(path.read_bytes()).hexdigest()}}}))
            output = root / 'history.json'
            import_history(manifest, output, stage)
            history = json.loads(output.read_text())
            records = history['pages']
            self.assertEqual(history['metadata']['creation_rule'], HISTORY_RULE)
            self.assertEqual([r['creator_prior_articles'] for r in records], [0, 0, 2, 0])
            self.assertEqual(records[0]['creator_tenure_days'], 365)
            self.assertIsNone(records[3]['creator_tenure_days'])
            self.assertEqual([r['creator_account_type'] for r in records], ['no_bot_flag'] * 3 + ['bot'])
            rows = [{'page_id': i, 'page_created_at': None} for i in range(1, 5)]
            attach_history(rows, {'language': 'vec', 'database': 'vecwiki', 'source': 'Wikimedia SQL dumps / vecwiki', 'sql_release': '20261001'}, history)
            self.assertEqual(rows[2]['creator_prior_articles'], 2)
            self.assertEqual(rows[0]['creator_registered_at'], '2019-01-01T00:00:00+00:00')
            self.assertEqual(rows[3]['creator_account_type'], 'bot')

    def test_content_plan_uses_publisher_names_and_never_writes(self):
        from orphanwiki.dumps import fetch_content
        from io import BytesIO
        class Response(BytesIO):
            headers = {'Content-Length': '123'}
        filename = 'vecwiki-2026-10-01-p1p5.xml.bz2'
        def opened(url, **kwargs):
            return Response(('a' * 64 + '  ' + filename + '\n').encode() if url.endswith('SHA256SUMS') else b'')
        with tempfile.TemporaryDirectory() as folder, patch('orphanwiki.dumps._open', side_effect=opened), patch('orphanwiki.dumps._fetch') as fetch:
            result = fetch_content(['vec'], '2026-10-01', folder, {'vec': 'vecwiki'}, True)
            self.assertEqual(result[0]['files'][filename]['bytes'], 123)
            self.assertFalse(list(Path(folder).iterdir()))
            fetch.assert_not_called()

    def test_quality_partition_resume_revision_coverage_and_bounded_parity(self):
        from orphanwiki.dumps import _stage_schema, infer_quality, _content_pages, SQL_TABLES
        from orphanwiki.core import attach_quality
        from xml.sax.saxutils import escape
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            stage = root / 'staging'
            stage.mkdir()
            files = {name: {'sha256_local': name} for name in SQL_TABLES}
            sql_manifest = root / 'sql.json'
            sql_manifest.write_text(json.dumps({'language': 'vec', 'database': 'vecwiki', 'sql_release': '20261001', 'files': files}))
            pages = [{'page_id': pid, 'title': f'P{pid}', 'revision_id': pid, 'length_bytes': 0,
                      'links': [], 'categories': []} for pid in range(1, 6)]
            pages[-1]['links'] = ['P2']
            raw = {'metadata': {'language': 'vec', 'database': 'vecwiki', 'sql_release': '20261001',
                'complete': True, 'source': 'Wikimedia SQL dumps / vecwiki',
                'dump_manifest': str(sql_manifest),
                'dump_manifest_sha256': hashlib.sha256(sql_manifest.read_bytes()).hexdigest(),
                'started_at': '2026-10-01T00:00:00+00:00', 'finished_at': '2026-10-02T00:00:00+00:00'}, 'pages': pages}
            raw_path = root / 'vec-raw.json'
            raw_path.write_text(json.dumps(raw))
            with closing(sqlite3.connect(stage / 'vecwiki-20261001.sqlite')) as db:
                _stage_schema(db)
                db.executemany('INSERT INTO imported VALUES(?,?)', ((name, name) for name in SQL_TABLES))
                db.executemany('INSERT INTO dump_page VALUES(?,?,?,?,?,?)',
                               ((pid, 0, f'P{pid}', 0, 0, pid) for pid in range(1, 6)))
                db.executescript('CREATE TABLE pages(id INTEGER PRIMARY KEY,data TEXT); CREATE TABLE links(id INTEGER,target TEXT); CREATE TABLE categories(id INTEGER,target TEXT);')
                db.executemany('INSERT INTO pages VALUES(?,?)', ((p['page_id'], json.dumps(p)) for p in pages))
                db.execute("INSERT INTO links VALUES(5,'P2')")
                db.commit()
            def page(pid, rid, text):
                tag = '<text deleted="deleted"/>' if text is None else '<text>' + escape(text) + '</text>'
                return f'<page><title>P{pid}</title><ns>0</ns><id>{pid}</id><revision><id>{rid}</id><model>wikitext</model>{tag}</revision></page>'
            content_files = {}
            for number, body in enumerate([page(1, 1, 'è<ref>x</ref>\n==A==') + page(2, 22, ''),
                                           page(3, 33, 'x') + page(4, 44, None)]):
                path = root / f'vecwiki-2026-10-01-p{number}.xml.bz2'
                path.write_bytes(bz2.compress(('<mediawiki xmlns="http://www.mediawiki.org/xml/export-0.11/"><siteinfo><dbname>vecwiki</dbname></siteinfo>' + body + '</mediawiki>').encode()))
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                content_files[path.name] = {'path': str(path), 'bytes': path.stat().st_size,
                                           'sha256_local': digest, 'sha256_publisher': digest}
            content = root / 'content.json'
            content.write_text(json.dumps({'language': 'vec', 'database': 'vecwiki', 'source': 'mediawiki_content_current',
                'content_release': '2026-10-01', 'complete': True, 'files': content_files}))
            output = root / 'vec-quality.json'
            original_reader = _content_pages
            def interrupted(path, database):
                if path.name.endswith('p1.xml.bz2'):
                    raise RuntimeError('interrupted')
                yield from original_reader(path, database)
            with patch('orphanwiki.dumps._content_pages', side_effect=interrupted), self.assertRaisesRegex(RuntimeError, 'interrupted'):
                infer_quality(raw_path, content, output, stage)
            self.assertFalse(output.exists())
            with patch('orphanwiki.dumps._content_pages', wraps=original_reader) as reader:
                infer_quality(raw_path, content, output, stage)
                self.assertEqual(reader.call_count, 1)  # completed partition reused
            quality = json.loads(output.read_text())
            self.assertEqual([p['quality_status'] for p in quality['pages']],
                             ['scored', 'scored', 'scored', 'missing_wikitext', 'missing_current_page'])
            self.assertEqual([p['quality_revision_relation'] for p in quality['pages']],
                             ['same', 'different', 'different', 'different', 'unavailable'])
            self.assertEqual(quality['metadata']['same_revision_pages'], 1)
            self.assertEqual(quality['metadata']['different_revision_pages'], 3)
            self.assertEqual(quality['pages'][2]['content_revision_id'], 33)
            self.assertIsNotNone(quality['pages'][2]['quality_score'])
            self.assertEqual(quality['pages'][1]['quality_score'], 0)
            self.assertIsNone(quality['pages'][3]['quality_score'])
            self.assertEqual(quality['metadata']['scored_pages'], 3)
            memory = analyze(raw, quality=quality)
            bounded_path = root / 'analysis' / 'vec.json'
            analyze_imported_dump(raw_path, None, bounded_path, {}, stage, quality_path=output)
            bounded = json.loads(bounded_path.read_text())
            self.assertEqual(memory['pages'], bounded['pages'])
            self.assertEqual(memory['metadata']['quality'], bounded['metadata']['quality'])
            from orphanwiki.dumps import refresh_measures, HISTORY_RULE
            from orphanwiki.core import write_json
            historical = {'metadata': {'language': 'vec', 'database': 'vecwiki',
                'source': 'MediaWiki History dumps / vecwiki', 'sql_release': '20261001',
                'history_release': '2026-09', 'complete': True, 'creation_rule': HISTORY_RULE},
                'pages': [{'page_id': pid, 'page_created_at': f'2020-01-0{pid}T00:00:00+00:00',
                          'article_created_at': '2019-01-01T00:00:00+00:00', 'creator_key': 'vecwiki:history-user:42',
                          'creator_history_status': 'observed_surviving_history', 'creator_account_type': 'no_bot_flag',
                          'creator_tenure_days': 100, 'creator_prior_articles': pid - 1} for pid in range(1, 6)]}
            history_path = root / 'vec-history.json'
            write_json(history_path, historical)
            bounded['metadata']['comparison'] = {'status': 'compared', 'reference_languages': ['lmo']}
            bounded['link_candidates'] = {'1': {'status': 'compared', 'candidate_count': 1,
                'counterparts': [{'language': 'lmo', 'status': 'non_orphan', 'page_id': 101}],
                'candidates': [{'page_id': 2, 'title': 'P2', 'out_degree': 0, 'dead_end': True,
                    'evidence': [{'language': 'lmo', 'source_page_id': 102, 'source_title': 'P2_lmo',
                                  'target_page_id': 101, 'target_title': 'P1_lmo'}]}]}}
            write_json(bounded_path, bounded)
            original_bytes = bounded_path.read_bytes()
            with patch('orphanwiki._native.graph_counts', side_effect=AssertionError('graph recomputed')):
                name = refresh_measures(raw_path, history_path, bounded_path.parent, output)
            updated = json.loads((bounded_path.parent / name).read_text())
            self.assertEqual(bounded_path.read_bytes(), original_bytes)
            self.assertEqual(updated['link_candidates'], bounded['link_candidates'])
            self.assertEqual([(p['in_degree'], p['out_degree'], p['orphan']) for p in updated['pages']],
                             [(p['in_degree'], p['out_degree'], p['orphan']) for p in bounded['pages']])
            self.assertTrue(all(p['age_days'] > 0 for p in updated['pages']))
            self.assertTrue(all(p['history_first_revision_at'].startswith('2019') for p in updated['pages']))
            summary_path = bounded_path.parent / name.replace('.json', '-summary.json')
            summary = json.loads(summary_path.read_text())
            self.assertEqual(summary['reports']['numeric']['age_days']['known'], 5)
            self.assertEqual(summary['reports']['numeric']['creator_tenure_days']['known'], 5)
            self.assertEqual(summary['reports']['profiles']['features']['rows'][2]['known'], 3)
            summary_path.unlink()  # rebuild transports without replacing canonical observations
            self.assertEqual(refresh_measures(raw_path, history_path, bounded_path.parent, output), name)
            self.assertTrue(summary_path.exists())
            with patch('orphanwiki.dumps._content_pages', side_effect=AssertionError('reparsed')):
                infer_quality(raw_path, content, output, stage)
            # A stale checksum never replaces the completed supplement.
            previous = output.read_bytes()
            record = next(iter(content_files.values()))
            record['sha256_publisher'] = '0' * 64
            content.write_text(json.dumps({'language': 'vec', 'database': 'vecwiki', 'source': 'mediawiki_content_current',
                'content_release': '2026-10-01', 'complete': True, 'files': content_files}))
            with self.assertRaisesRegex(ValueError, 'Checksum'):
                infer_quality(raw_path, content, output, stage)
            self.assertEqual(output.read_bytes(), previous)
            legacy = copy.deepcopy(quality)
            legacy['metadata'].pop('quality_revision_rule')
            with self.assertRaisesRegex(ValueError, 'Qualità'):
                attach_quality(memory['pages'], raw['metadata'], legacy)
            quality['pages'][0]['revision_id'] = 99
            with self.assertRaisesRegex(ValueError, 'revisione'):
                attach_quality(memory['pages'], raw['metadata'], quality)
            legacy_export = copy.deepcopy(updated)
            legacy_export['metadata']['measures_rule'] = 'creation_quality_creators_v2'
            legacy_export['metadata']['quality'].pop('quality_revision_rule')
            for page_record in legacy_export['pages']:
                page_record.pop('content_revision_id', None)
                page_record.pop('quality_revision_relation', None)
            legacy_path = bounded_path.parent / 'legacy.json'
            write_json(legacy_path, legacy_export)
            manifest_path = bounded_path.parent / 'manifest.json'
            published = json.loads(manifest_path.read_text())
            published.append({'language': 'vec', 'file': legacy_path.name,
                              'date': raw['metadata']['finished_at'],
                              'published_at': '9999-01-01T00:00:00+00:00'})
            write_json(manifest_path, published)
            retained_name = refresh_measures(raw_path, history_path, bounded_path.parent)
            retained = json.loads((bounded_path.parent / retained_name).read_text())
            self.assertEqual(retained['metadata']['measures_rule'], 'creation_quality_creators_v2')
            self.assertNotIn('quality_revision_relation', retained['pages'][0])


if __name__ == '__main__':
    unittest.main()
