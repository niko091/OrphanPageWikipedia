"""Isolated WSGI/SQL tests; no Wikimedia, providers or production databases."""
import io
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import types
import unittest
from unittest.mock import patch

from orphanwiki.editorial import ReviewApplication


class EditorialTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.web = self.root / 'web'
        (self.web / 'data').mkdir(parents=True)
        (self.web / 'index.html').write_text('<h1>Dashboard</h1>')
        self.metadata = {'language': 'vec', 'complete': True, 'demo': False, 'finished_at': '2026-10-01T00:00:00Z'}
        self.entries = []
        for language in ('vec', 'lmo'):
            directory = self.web / 'data' / f'{language}-connections'
            directory.mkdir()
            detail = {'format': 'connections_v1', 'metadata': {**self.metadata, 'language': language},
                      'pages': [{'page_id': 1, 'orphan': True}],
                      'link_candidates': {'1': {'candidates': [{'page_id': 2}, {'page_id': 3}]}}}
            (directory / '1.json').write_text(json.dumps(detail))
            self.entries.append({'language': language, 'file': f'{language}.json', 'kind': 'wiki',
                                 'date': self.metadata['finished_at'], 'connections_dir': directory.name})
        self.entries.append({'language': 'all', 'file': 'all.json', 'kind': 'aggregate'})
        (self.web / 'data' / 'all.json').write_text(json.dumps({'members': self.entries[:2]}))
        self.write_manifest()
        self.database = self.root / 'reviews.sqlite'
        self.app = ReviewApplication(self.web, self.database)
        self.body = {'dataset': 'vec.json', 'language': 'vec', 'source_id': 2, 'target_id': 1,
                     'status': 'fixed', 'version': 0}
        self.environment = patch.dict(os.environ, {}, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def write_manifest(self):
        (self.web / 'data' / 'manifest.json').write_text(json.dumps(self.entries))

    def request(self, method='GET', path='/api/reviews', body=None, query='dataset=vec.json', app=None, **headers):
        encoded = json.dumps(body).encode() if body is not None else b''
        environ = {'REQUEST_METHOD': method, 'PATH_INFO': path, 'QUERY_STRING': query,
                   'HTTP_HOST': 'example.toolforge.org', 'CONTENT_TYPE': 'application/json',
                   'CONTENT_LENGTH': str(len(encoded)), 'wsgi.input': io.BytesIO(encoded), **headers}
        response = []
        chunks = (app or self.app)(environ, lambda status, fields: response.append((status, dict(fields))))
        data = b''.join(chunks)
        status, fields = response[0]
        return int(status.split()[0]), json.loads(data) if fields['Content-Type'].startswith('application/json') else data

    def test_shared_persistence_independent_readers_languages_and_reopening(self):
        self.assertEqual(self.request()[1]['reviews'], [])
        before = {str(p): p.read_bytes() for p in self.web.rglob('*') if p.is_file()}
        code, result = self.request('POST', body=self.body)
        self.assertEqual(code, 200)
        self.assertEqual(result['review']['version'], 1)
        reader = ReviewApplication(self.web, self.database)
        self.assertEqual(self.request(app=reader)[1]['reviews'], [result['review']])
        self.assertEqual(self.request(query='dataset=lmo.json')[1]['reviews'], [])
        self.assertEqual(self.request(query='dataset=all.json')[1]['reviews'], [result['review']])
        for version, status in [(1, 'bad_link'), (2, 'not_fixed')]:
            code, result = self.request('POST', body={**self.body, 'status': status, 'version': version})
            self.assertEqual(code, 200)
            self.assertEqual(result['review']['status'], status)
        self.assertEqual({str(p): p.read_bytes() for p in self.web.rglob('*') if p.is_file()}, before)
        with sqlite3.connect(self.database) as connection:
            self.assertEqual([row[1] for row in connection.execute('PRAGMA table_info(editorial_reviews)')],
                             ['language', 'source_id', 'target_id', 'status', 'version', 'updated_at'])
        # Release changes do not change the language/local-page identity.
        self.entries[0]['date'] = '2026-11-01T00:00:00Z'
        self.write_manifest()
        self.assertEqual(self.request()[1]['reviews'][0]['status'], 'not_fixed')

    def test_conflict_does_not_overwrite_other_users(self):
        self.request('POST', body=self.body)
        code, result = self.request('POST', body={**self.body, 'status': 'bad_link'})
        self.assertEqual(code, 409)
        self.assertEqual(result['current']['status'], 'fixed')
        self.assertEqual(self.request()[1]['reviews'][0]['status'], 'fixed')

    def test_only_manifest_approved_real_edges_and_statuses_are_writable(self):
        for update in [{'status': 'anything'}, {'source_id': True}, {'target_id': 0}, {'source_id': 1},
                       {'language': '../vec'}, {'dataset': '../vec.json'}, {'version': -1},
                       {'version': True}, {'source_id': 9007199254740992}, {'wikitext': 'must not store'},
                       {'source_id': 999}, {'dataset': 'unapproved.json'}, {'language': 'en'}]:
            with self.subTest(update=update):
                self.assertIn(self.request('POST', body={**self.body, **update})[0], (400, 404, 422))
        self.entries[0]['demo'] = True
        self.write_manifest()
        self.assertEqual(self.request('POST', body=self.body)[0], 404)
        self.entries[0]['demo'] = False
        self.entries[0]['connections_dir'] = '../outside'
        self.write_manifest()
        self.assertEqual(self.request('POST', body=self.body)[0], 422)

    def test_stale_or_wrong_detail_identity_is_rejected(self):
        path = self.web / 'data/vec-connections/1.json'
        original = json.loads(path.read_text())
        for update in [{'finished_at': 'old'}, {'language': 'en'}, {'demo': True}, {'complete': False}]:
            path.write_text(json.dumps({**original, 'metadata': {**original['metadata'], **update}}))
            self.assertEqual(self.request('POST', body=self.body)[0], 422)
        self.assertFalse(self.database.exists())

    def test_cross_origin_forms_large_bodies_and_static_path_escape_are_rejected(self):
        self.assertEqual(self.request('POST', body=self.body, HTTP_ORIGIN='https://other.example')[0], 403)
        self.assertEqual(self.request('POST', body=self.body, HTTP_SEC_FETCH_SITE='cross-site')[0], 403)
        self.assertEqual(self.request('POST', body=self.body, CONTENT_TYPE='text/plain')[0], 415)
        self.assertEqual(self.request('POST', body=self.body, CONTENT_LENGTH='3000')[0], 413)
        self.assertEqual(self.request('DELETE')[0], 405)
        self.assertEqual(self.request(query='dataset=vec.json&dataset=lmo.json')[0], 400)
        self.assertEqual(self.request(path='/')[1], b'<h1>Dashboard</h1>')
        (self.root / 'secret').write_text('secret')
        (self.web / 'data/outside').symlink_to(self.root / 'secret')
        for path in ('/../secret', '/data/../../secret', '/data/outside', '/.env', '/editorial.sqlite'):
            self.assertEqual(self.request(path=path)[0], 404)
        self.assertFalse(self.database.exists())

    def test_toolforge_requires_explicit_toolsdb_configuration(self):
        with patch.dict(os.environ, {'TOOL_DATA_DIR': '/unused-tool-data'}):
            self.assertEqual(self.request()[0], 503)
        self.assertFalse(self.database.exists())

    def test_mysql_adapter_uses_toolsdb_credentials_parameters_and_same_conflict_contract(self):
        # Exercise the MySQL SQL branch against an isolated SQLite translation;
        # this verifies adapter/query semantics, not an actual MariaDB deployment.
        calls = []
        database = self.database

        class Cursor:
            def __init__(self, cursor):
                self.cursor = cursor
            def execute(self, sql, parameters=()):
                calls.append((sql, parameters))
                self.cursor.execute(sql.replace('INSERT IGNORE', 'INSERT OR IGNORE').replace('%s', '?'), parameters)
            @property
            def rowcount(self):
                return self.cursor.rowcount
            def fetchone(self):
                return self.cursor.fetchone()
            def fetchall(self):
                return self.cursor.fetchall()
            def close(self):
                self.cursor.close()

        class Connection:
            def __init__(self):
                self.connection = sqlite3.connect(database)
            def cursor(self):
                return Cursor(self.connection.cursor())
            def commit(self):
                self.connection.commit()
            def rollback(self):
                self.connection.rollback()
            def close(self):
                self.connection.close()

        settings = []
        def connect(**kwargs):
            settings.append(kwargs)
            return Connection()
        with patch.dict('sys.modules', {'pymysql': types.SimpleNamespace(connect=connect)}), patch.dict(os.environ, {
                'ORPHANWIKI_REVIEW_MYSQL_DATABASE': 's123__editorial',
                'TOOL_TOOLSDB_USER': 's123', 'TOOL_TOOLSDB_PASSWORD': 'synthetic-password'}):
            self.assertEqual(self.request('POST', body=self.body)[0], 200)
            self.assertEqual(self.request()[1]['reviews'][0]['status'], 'fixed')
            self.assertEqual(self.request('POST', body=self.body)[0], 409)
        self.assertEqual(settings[0]['host'], 'tools.db.svc.wikimedia.cloud')
        self.assertEqual(settings[0]['user'], 's123')
        self.assertEqual(settings[0]['database'], 's123__editorial')
        self.assertTrue(any('INSERT IGNORE' in sql for sql, _ in calls))
        self.assertTrue(any('%s' in sql and parameters for sql, parameters in calls))


if __name__ == '__main__':
    unittest.main()
