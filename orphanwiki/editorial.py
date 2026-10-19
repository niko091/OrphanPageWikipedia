"""Shared editorial workflow, separate from immutable research exports.

WSGI entry point for Wikimedia hosting; the stdlib server is for local use.
Only edge identities and human statuses are stored. No article/prompt text.
"""
import argparse
from contextlib import closing, contextmanager
from datetime import datetime, timezone
import json
import mimetypes
import os
from pathlib import Path
import re
import sqlite3
from urllib.parse import parse_qs
from wsgiref.simple_server import make_server


STATUSES = ('fixed', 'not_fixed', 'bad_link')
LANGUAGE = re.compile(r'[a-z][a-z0-9-]{0,19}')
FILENAME = re.compile(r'[a-zA-Z0-9_.-]+\.json')
DIRECTORY = re.compile(r'[a-zA-Z0-9][a-zA-Z0-9_.-]*')
ROOT = Path(__file__).resolve().parent.parent


class ReviewError(Exception):
    def __init__(self, status, message, current=None):
        super().__init__(message)
        self.status, self.current = status, current


def valid_id(value):
    return type(value) is int and 0 < value <= 9007199254740991


class ReviewApplication:
    def __init__(self, web_root=None, database=None, mysql=None):
        self.web_root = Path(web_root or os.environ.get('ORPHANWIKI_WEB_ROOT', ROOT / 'web')).resolve()
        self.database = Path(database or os.environ.get('ORPHANWIKI_REVIEW_SQLITE', ROOT / 'data/editorial.sqlite'))
        self.mysql = mysql

    @contextmanager
    def connection(self):
        database = (self.mysql or {}).get('database') or os.environ.get('ORPHANWIKI_REVIEW_MYSQL_DATABASE')
        if database:
            import pymysql
            settings = dict(self.mysql or {})
            settings.setdefault('database', database)
            settings.setdefault('host', os.environ.get('ORPHANWIKI_REVIEW_MYSQL_HOST', 'tools.db.svc.wikimedia.cloud'))
            settings.setdefault('user', os.environ.get('TOOL_TOOLSDB_USER'))
            settings.setdefault('password', os.environ.get('TOOL_TOOLSDB_PASSWORD'))
            defaults = os.environ.get('ORPHANWIKI_REVIEW_MYSQL_DEFAULTS_FILE')
            if defaults:
                settings['read_default_file'] = defaults
                settings = {key: value for key, value in settings.items() if value is not None}
            elif not settings['user'] or not settings['password']:
                raise ReviewError(503, 'Configurazione ToolsDB incompleta')
            connection = pymysql.connect(**settings, charset='utf8mb4', connect_timeout=5,
                                         read_timeout=10, write_timeout=10, autocommit=False)
            marker, insert = '%s', 'INSERT IGNORE'
        else:
            if os.environ.get('TOOL_DATA_DIR') or os.environ.get('TOOL_TOOLSDB_USER'):
                raise ReviewError(503, 'Configurare ToolsDB per le valutazioni su Toolforge')
            self.database.parent.mkdir(parents=True, exist_ok=True)
            connection = sqlite3.connect(self.database, timeout=10)
            marker, insert = '?', 'INSERT OR IGNORE'
        with closing(connection):
            try:
                with closing(connection.cursor()) as cursor:
                    cursor.execute('CREATE TABLE IF NOT EXISTS editorial_reviews ('
                        'language VARCHAR(20) NOT NULL, source_id BIGINT NOT NULL, target_id BIGINT NOT NULL, '
                        'status VARCHAR(12) NOT NULL, version BIGINT NOT NULL, updated_at VARCHAR(32) NOT NULL, '
                        'PRIMARY KEY (language, target_id, source_id))')
                    yield cursor, marker, insert
                connection.commit()
            except Exception:
                connection.rollback()
                raise

    @staticmethod
    def review(row):
        return dict(zip(('language', 'source_id', 'target_id', 'status', 'version', 'updated_at'), row))

    def members(self, dataset):
        if not isinstance(dataset, str) or not FILENAME.fullmatch(dataset):
            raise ReviewError(400, 'Raccolta non valida')
        folder = self.web_root / 'data'
        entries = json.loads((folder / 'manifest.json').read_text(encoding='utf-8'))
        entry = next((row for row in entries if row.get('file') == dataset and not row.get('demo')), None)
        if entry is None:
            raise ReviewError(404, 'Raccolta non approvata')
        if entry.get('kind') != 'aggregate':
            return [entry]
        index = json.loads((folder / dataset).read_text(encoding='utf-8'))
        members = []
        for member in index.get('members', []):
            approved = next((row for row in entries if row.get('file') == member.get('file') and
                             row.get('language') == member.get('language') and not row.get('demo') and
                             row.get('kind') != 'aggregate'), None)
            if approved is None:
                raise ReviewError(400, 'Membro aggregato non approvato')
            members.append(approved)
        if not members:
            raise ReviewError(400, 'Raccolta aggregata non valida')
        return members

    def validate_edge(self, body):
        keys = {'dataset', 'language', 'source_id', 'target_id', 'status', 'version'}
        if not isinstance(body, dict) or set(body) != keys or body.get('status') not in STATUSES or \
                not isinstance(body.get('language'), str) or not LANGUAGE.fullmatch(body['language']) or \
                body['language'] == 'all' or not valid_id(body.get('source_id')) or \
                not valid_id(body.get('target_id')) or body['source_id'] == body['target_id'] or \
                type(body.get('version')) is not int or not 0 <= body['version'] < 9007199254740991:
            raise ReviewError(400, 'Valutazione non valida')
        entry = next((row for row in self.members(body['dataset']) if row['language'] == body['language']), None)
        directory = entry.get('connections_dir') if entry else None
        if not isinstance(directory, str) or not DIRECTORY.fullmatch(directory):
            raise ReviewError(422, 'Dettagli della coppia non disponibili')
        path = (self.web_root / 'data' / directory / f"{body['target_id']}.json").resolve()
        if not path.is_relative_to((self.web_root / 'data').resolve()):
            raise ReviewError(400, 'Percorso non valido')
        if not path.is_file():
            raise ReviewError(422, 'Coppia non presente nella raccolta')
        detail = json.loads(path.read_text(encoding='utf-8'))
        metadata = detail.get('metadata', {})
        pages = detail.get('pages', [])
        record = detail.get('link_candidates', {}).get(str(body['target_id']), {})
        if detail.get('format') != 'connections_v1' or metadata.get('language') != body['language'] or \
                metadata.get('finished_at') != entry['date'] or metadata.get('complete') is not True or \
                metadata.get('demo') or len(pages) != 1 or pages[0].get('page_id') != body['target_id'] or \
                pages[0].get('orphan') is not True or not any(
                    candidate.get('page_id') == body['source_id'] for candidate in record.get('candidates', [])):
            raise ReviewError(422, 'Coppia non presente nella raccolta approvata')

    def read_reviews(self, dataset):
        languages = [entry['language'] for entry in self.members(dataset)]
        with self.connection() as (cursor, marker, _):
            cursor.execute('SELECT language, source_id, target_id, status, version, updated_at '
                           f"FROM editorial_reviews WHERE language IN ({','.join([marker] * len(languages))}) "
                           'ORDER BY language, target_id, source_id', languages)
            return [self.review(row) for row in cursor.fetchall()]

    def save_review(self, body):
        self.validate_edge(body)
        now = datetime.now(timezone.utc).isoformat(timespec='seconds')
        identity = (body['language'], body['source_id'], body['target_id'])
        with self.connection() as (cursor, marker, insert):
            cursor.execute(f'{insert} INTO editorial_reviews '
                '(language, source_id, target_id, status, version, updated_at) '
                f"VALUES ({','.join([marker] * 6)})", (*identity, 'not_fixed', 0, now))
            cursor.execute(f'UPDATE editorial_reviews SET status={marker}, version=version+1, updated_at={marker} '
                f'WHERE language={marker} AND source_id={marker} AND target_id={marker} AND version={marker}',
                (body['status'], now, *identity, body['version']))
            changed = cursor.rowcount
            cursor.execute('SELECT language, source_id, target_id, status, version, updated_at FROM editorial_reviews '
                f'WHERE language={marker} AND source_id={marker} AND target_id={marker}', identity)
            current = self.review(cursor.fetchone())
            if not changed:
                raise ReviewError(409, 'La coppia è stata valutata da un altro utente. Controlla lo stato aggiornato e riprova.', current)
            return current

    def __call__(self, environ, start_response):
        path, method = environ.get('PATH_INFO', '/'), environ.get('REQUEST_METHOD', 'GET')
        if path == '/api/reviews':
            try:
                if method == 'GET':
                    query = parse_qs(environ.get('QUERY_STRING', ''), strict_parsing=True)
                    if set(query) != {'dataset'} or len(query['dataset']) != 1:
                        raise ReviewError(400, 'Raccolta richiesta')
                    result = {'format': 'editorial_reviews_v1', 'reviews': self.read_reviews(query['dataset'][0])}
                elif method == 'POST':
                    # JSON + same-origin browser requests; never accept form submissions.
                    origin = environ.get('HTTP_ORIGIN')
                    expected = environ.get('HTTP_HOST', '')
                    if origin and origin not in ('https://' + expected, 'http://' + expected):
                        raise ReviewError(403, 'Origine non consentita')
                    if environ.get('HTTP_SEC_FETCH_SITE') in ('cross-site', 'same-site'):
                        raise ReviewError(403, 'Origine non consentita')
                    if environ.get('CONTENT_TYPE', '').split(';')[0] != 'application/json':
                        raise ReviewError(415, 'JSON richiesto')
                    length = int(environ.get('CONTENT_LENGTH', '0'))
                    if not 0 < length <= 2048:
                        raise ReviewError(413, 'Dimensione della richiesta non valida')
                    result = {'review': self.save_review(json.loads(environ['wsgi.input'].read(length)))}
                else:
                    raise ReviewError(405, 'Metodo non consentito')
                return self.json_response(start_response, 200, result)
            except ReviewError as error:
                return self.json_response(start_response, error.status, {'error': str(error), 'current': error.current})
            except (ValueError, TypeError, KeyError):
                return self.json_response(start_response, 400, {'error': 'Richiesta non valida'})
            except Exception:
                return self.json_response(start_response, 503, {'error': 'Servizio valutazioni non disponibile'})
        if method not in ('GET', 'HEAD'):
            return self.json_response(start_response, 405, {'error': 'Metodo non consentito'})
        relative = path.lstrip('/') or 'index.html'
        allowed = relative in ('index.html', 'creators.html', 'deorphanize.html', 'connections.html') or \
            relative.startswith(('assets/', 'data/'))
        target = (self.web_root / relative).resolve()
        if not allowed or any(part.startswith('.') for part in Path(relative).parts) or \
                not target.is_relative_to(self.web_root) or not target.is_file():
            return self.json_response(start_response, 404, {'error': 'File non disponibile'})
        start_response('200 OK', [('Content-Type', mimetypes.guess_type(target.name)[0] or 'application/octet-stream'),
                                 ('Content-Length', str(target.stat().st_size)), ('X-Content-Type-Options', 'nosniff')])
        if method == 'HEAD':
            return []
        stream = target.open('rb')
        wrapper = environ.get('wsgi.file_wrapper')
        if wrapper:
            return wrapper(stream, 65536)
        def chunks():
            with stream:
                while chunk := stream.read(65536):
                    yield chunk
        return chunks()

    @staticmethod
    def json_response(start_response, status, value):
        names = {200: 'OK', 400: 'Bad Request', 403: 'Forbidden', 404: 'Not Found', 405: 'Method Not Allowed',
                 409: 'Conflict', 413: 'Payload Too Large', 415: 'Unsupported Media Type', 422: 'Unprocessable Entity',
                 503: 'Service Unavailable'}
        data = json.dumps(value, ensure_ascii=False).encode('utf-8')
        start_response(f'{status} {names[status]}', [('Content-Type', 'application/json; charset=utf-8'),
            ('Content-Length', str(len(data))), ('Cache-Control', 'no-store'), ('X-Content-Type-Options', 'nosniff')])
        return [data]


app = ReviewApplication()

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description='Dashboard e valutazioni editoriali condivise (sviluppo locale)')
    parser.add_argument('--web-root', default=str(ROOT / 'web'))
    parser.add_argument('--database', default=str(ROOT / 'data/editorial.sqlite'))
    parser.add_argument('--bind', default='127.0.0.1')
    parser.add_argument('--port', type=int, default=8000)
    args = parser.parse_args()
    with make_server(args.bind, args.port, ReviewApplication(args.web_root, args.database)) as server:
        print(f'Dashboard: http://{args.bind}:{args.port}', flush=True)
        server.serve_forever()
