import tempfile
import sqlite3
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

from orphanwiki.replicas import DiskPages, collect_creation_dates, collect_replicas, iso_timestamp


class Cursor:
    def __init__(self, connection):
        self.connection = connection
    def __enter__(self):
        return self
    def __exit__(self, *args):
        pass
    def execute(self, sql):
        self.connection.queries.append(sql)
        if self.connection.fail:
            raise RuntimeError('database unavailable')
        new = self.connection.new_schema
        if sql == 'SHOW COLUMNS FROM pagelinks':
            self.rows = [('pl_target_id' if new else 'pl_title',)]
        elif sql == 'SHOW COLUMNS FROM categorylinks':
            self.rows = [('cl_target_id' if new else 'cl_to',)]
        elif sql.startswith('SELECT page_id,page_title,page_len'):
            self.rows = [(1, b'Article_A', 120, 100), (2, b'Article_B', 340, 101), (3, b'Isolated', 12, 102)]
        elif sql.startswith('SELECT pl_from'):
            self.rows = [(1, b'Redirect'), (1, b'Article_B'), (99, b'Isolated')]
        elif sql.startswith('SELECT page_title,rd_title'):
            self.rows = [(b'Redirect', b'Article_B')]
        elif "pp_propname='wikibase_item'" in sql:
            self.rows = [(1, b'Q1'), (2, b'Q2'), (3, b'invalid'), (99, b'Q99')]
        elif 'pp_propname' in sql:
            self.rows = [(b'Hidden',)]
        elif sql.startswith('SELECT page_id,page_title FROM page'):
            self.rows = [(10, b'Specific'), (11, b'Broad')]
        elif sql.startswith('SELECT cl_from'):
            self.rows = [(1, b'Specific'), (1, b'Hidden'), (2, b'Broad'), (10, b'Broad')]
        elif sql.startswith('SELECT rev_page'):
            self.rows = [(1, b'20200101000000', b'20200101000000', 1), (2, b'20210203040506', None, 0)]
        elif sql.startswith('SELECT log_page'):
            self.rows = [(2, b'20210201000000', 1, 0)]
        else:
            raise AssertionError(sql)
    def __iter__(self):
        return iter(self.rows)


class Connection:
    def __init__(self, new_schema=True, fail=False):
        self.new_schema, self.fail = new_schema, fail
        self.closed = False
        self.queries = []
    def cursor(self):
        return Cursor(self)
    def close(self):
        self.closed = True


class ReplicaTests(unittest.TestCase):
    def test_timestamp_validation_does_not_coerce_missing_dates(self):
        for invalid in [None, '', '\x00' * 14, b'\x00' * 14, '00000000000000',
                        '20200230000000', '20200101240000', '2020010100000',
                        '202001010000000', '20200101000000 ', '２０２００１０１００００００',
                        b'\xff' * 14, 20200101000000, True]:
            with self.subTest(value=repr(invalid)):
                self.assertIsNone(iso_timestamp(invalid))
        for valid in ['20200229010203', b'20200229010203']:
            self.assertEqual(iso_timestamp(valid), '2020-02-29T01:02:03+00:00')

    def test_invalid_creation_dates_keep_all_pages_and_independent_log_evidence(self):
        pages = {i: {} for i in range(1, 6)}
        valid, invalid = '20200101000000', '\x00' * 14
        def rows(sql):
            if sql.startswith('SELECT rev_page'):
                return [(1, invalid, invalid, 1), (2, valid, invalid, 1),
                        (3, valid, valid, 1), (4, invalid, invalid, 1), (5, invalid, invalid, 1)]
            return [(3, invalid, 1, 0), (4, valid, 1, 0), (5, None, 0, 1)]
        collect_creation_dates(rows, pages)
        self.assertEqual(len(pages), 5)
        for pid, status in [(1, 'invalid_revision_timestamp'), (2, 'invalid_revision_timestamp'),
                            (3, 'invalid_creation_log_timestamp'), (5, 'imported_or_merged_history')]:
            self.assertIsNone(pages[pid]['page_created_at'])
            self.assertIsNone(pages[pid]['creation_date_source'])
            self.assertEqual(pages[pid]['creation_date_status'], status)
        self.assertIsNone(pages[1]['first_public_revision_at'])
        self.assertEqual(pages[2]['first_public_revision_at'], '2020-01-01T00:00:00+00:00')
        self.assertEqual(pages[4]['creation_date_source'], 'creation_log')
        self.assertEqual(pages[4]['page_created_at'], '2020-01-01T00:00:00+00:00')

    def test_null_bytes_in_bulk_sql_preserve_census_and_cleanup_in_both_modes(self):
        from orphanwiki.core import analyze
        class InvalidCursor(Cursor):
            def execute(self, sql):
                super().execute(sql)
                if sql.startswith('SELECT rev_page'):
                    self.rows[0] = (1, b'\x00' * 14, b'\x00' * 14, 1)
        class InvalidConnection(Connection):
            def cursor(self):
                return InvalidCursor(self)
        with tempfile.TemporaryDirectory() as folder:
            credentials = Path(folder) / 'credentials'
            credentials.write_text('[client]\nuser=fixture\npassword=fixture')
            for modern in [True, False]:
                for staged in [True, False]:
                    with self.subTest(modern=modern, staged=staged), closing(sqlite3.connect(':memory:')) as stage:
                        connection = InvalidConnection(modern)
                        pages = DiskPages(stage) if staged else None
                        with patch('orphanwiki.replicas.pymysql.connect', return_value=connection):
                            raw = collect_replicas('vec', credentials, page_store=pages)
                        self.assertTrue(connection.closed)
                        if staged:
                            pages.prepare_export()
                            raw['pages'] = list(pages.export_pages())
                        self.assertTrue(raw['metadata']['complete'])
                        self.assertEqual(len(raw['pages']), 3)
                        self.assertEqual(raw['pages'][0]['creation_date_status'], 'invalid_revision_timestamp')
                        result = analyze(raw)
                        self.assertEqual([p['in_degree'] for p in result['pages']], [0, 1, 0])
                        self.assertIsNone(result['pages'][0]['age_days'])

    def test_creation_evidence_missing_imports_and_ambiguous_histories(self):
        pages = {i: {} for i in range(1, 9)}
        first, created = '20200101000000', '20210101000000'
        queries = []
        def rows(sql):
            queries.append(sql)
            if sql.startswith('SELECT rev_page'):
                return [(1, first, first, 1), (2, first, None, 0), (3, first, first, 1),
                        (4, first, first, 2), (5, first, first, 1), (6, first, created, 1),
                        (7, first, first, 1)]
            return [(2, created, 1, 1), (3, None, 0, 1), (5, created, 2, 0)]
        collect_creation_dates(rows, pages)
        self.assertEqual(pages[1]['creation_date_source'], 'initial_revision')
        self.assertEqual(pages[2]['creation_date_source'], 'creation_log')
        self.assertEqual(pages[2]['page_created_at'], '2021-01-01T00:00:00+00:00')
        for pid, status in [(3, 'imported_or_merged_history'), (4, 'ambiguous_creation_revisions'),
                            (5, 'ambiguous_creation_logs'), (6, 'inconsistent_revision_history'),
                            (8, 'missing_revision')]:
            self.assertIsNone(pages[pid]['page_created_at'])
            self.assertEqual(pages[pid]['creation_date_status'], status)
        self.assertEqual(len(queries), 2)
        self.assertIn('(log_deleted & 1)=0', queries[1])

    def collect(self, connection):
        with tempfile.TemporaryDirectory() as folder:
            file = Path(folder) / 'credentials'
            file.write_text('[client]\nuser=fixture\npassword=fixture')
            with patch('orphanwiki.replicas.pymysql.connect', return_value=connection):
                return collect_replicas('vec', file)

    def test_both_schemas_and_real_analysis_contract(self):
        from orphanwiki.core import analyze
        for new in (True, False):
            with self.subTest(new_schema=new):
                connection = Connection(new)
                raw = self.collect(connection)
                self.assertTrue(connection.closed)
                self.assertEqual(raw['pages'][0]['categories'], ['Specific'])
                self.assertEqual(raw['category_parents'], {'Specific': ['Broad']})
                self.assertEqual(raw['pages'][0]['created_at'], '2020-01-01T00:00:00+00:00')
                self.assertIsNone(raw['pages'][2]['created_at'])
                result = analyze(raw)
                self.assertEqual([p['in_degree'] for p in result['pages']], [0, 1, 0])
                self.assertNotIn('password', str(result))
                self.assertTrue(all(q.startswith(('SELECT', 'SHOW')) for q in connection.queries))
                self.assertEqual(raw['pages'][1]['created_at'], '2021-02-01T00:00:00+00:00')
                self.assertEqual(raw['pages'][1]['first_public_revision_at'], '2021-02-03T04:05:06+00:00')
                self.assertEqual(result['metadata']['age_rule'], 'page_creation_event')
                self.assertEqual([p['wikibase_item'] for p in raw['pages']], ['Q1', 'Q2', None])
                self.assertTrue(raw['metadata']['item_mapping']['complete'])
                features = raw['metadata']['article_features']
                self.assertEqual(features['topic_inputs'], 'pagelinks + page_props.wikibase_item')
                self.assertEqual(features['quality'], 'unavailable_no_wikitext_in_replicas')
                self.assertEqual(features['pageviews'], 'unavailable_not_in_wiki_replicas')
                self.assertEqual(features['topics'], 'unavailable_no_local_model')
                self.assertLessEqual(len(connection.queries), 12)

    def test_connection_closed_on_failure(self):
        connection = Connection(fail=True)
        with self.assertRaises(RuntimeError):
            self.collect(connection)
        self.assertTrue(connection.closed)

    def test_no_credentials_fails_before_connection(self):
        with patch.dict('os.environ', {}, clear=True), patch('orphanwiki.replicas.Path.is_file', return_value=False), patch('orphanwiki.replicas.pymysql.connect') as connect:
            with self.assertRaises(RuntimeError):
                collect_replicas('vec')
            connect.assert_not_called()

    def test_hidden_mysql_config_and_precedence(self):
        for files, expected in [(['.my.cnf'], '.my.cnf'),
                                (['replica.my.cnf', '.my.cnf'], 'replica.my.cnf')]:
            with self.subTest(files=files), tempfile.TemporaryDirectory() as folder:
                for name in files:
                    (Path(folder) / name).write_text('[client]\nuser=fixture\npassword=fixture')
                with patch('orphanwiki.replicas.Path.home', return_value=Path(folder)), patch('orphanwiki.replicas.pymysql.connect', return_value=Connection()) as connect:
                    collect_replicas('vec')
                    self.assertEqual(connect.call_args.kwargs['read_default_file'], str(Path(folder) / expected))

    def test_missing_explicit_file_does_not_fall_back(self):
        with patch.dict('os.environ', {'MYSQL_USERNAME': 'fixture', 'MYSQL_PASSWORD': 'fixture'}), patch('orphanwiki.replicas.Path.is_file', return_value=False), patch('orphanwiki.replicas.pymysql.connect') as connect:
            with self.assertRaisesRegex(RuntimeError, 'File credenziali non trovato'):
                collect_replicas('vec', '/missing/credentials')
            connect.assert_not_called()

    def test_toolforge_environment(self):
        with patch.dict('os.environ', {'TOOL_REPLICA_USER': 'fixture', 'TOOL_REPLICA_PASSWORD': 'fixture'}, clear=True), patch('orphanwiki.replicas.Path.is_file', return_value=False), patch('orphanwiki.replicas.pymysql.connect', return_value=Connection()) as connect:
            collect_replicas('vec')
            self.assertEqual(connect.call_args.kwargs['user'], 'fixture')

    def test_partial_environment_not_combined(self):
        with patch.dict('os.environ', {'MYSQL_USERNAME': 'fixture', 'TOOL_REPLICA_PASSWORD': 'fixture'}, clear=True), patch('orphanwiki.replicas.Path.is_file', return_value=False), patch('orphanwiki.replicas.pymysql.connect') as connect:
            with self.assertRaises(RuntimeError):
                collect_replicas('vec')
            connect.assert_not_called()

    def test_invalid_language(self):
        with self.assertRaises(ValueError):
            collect_replicas('vec; DROP TABLE page')

    def test_paws_environment_credentials(self):
        connection = Connection()
        with patch.dict('os.environ', {'MYSQL_USERNAME': 'test-user', 'MYSQL_PASSWORD': 'test-secret'}), patch('orphanwiki.replicas.Path.is_file', return_value=False), patch('orphanwiki.replicas.pymysql.connect', return_value=connection) as connect:
            collect_replicas('vec')
            self.assertEqual(connect.call_args.kwargs['user'], 'test-user')
            self.assertEqual(connect.call_args.kwargs['host'], 'vecwiki.analytics.db.svc.wikimedia.cloud')

    def test_lombard_uses_same_bulk_collector_with_own_database(self):
        with patch.dict('os.environ', {'MYSQL_USERNAME': 'fixture', 'MYSQL_PASSWORD': 'fixture'}), patch('orphanwiki.replicas.Path.is_file', return_value=False), patch('orphanwiki.replicas.pymysql.connect', return_value=Connection()) as connect:
            raw = collect_replicas('lmo')
            self.assertEqual(raw['metadata']['language'], 'lmo')
            self.assertEqual(raw['metadata']['source'], 'Wiki Replicas / lmowiki')
            self.assertEqual(connect.call_args.kwargs['database'], 'lmowiki_p')
            self.assertEqual(connect.call_args.kwargs['host'], 'lmowiki.analytics.db.svc.wikimedia.cloud')


if __name__ == '__main__':
    unittest.main()
