"""Verified, resumable downloads of public Wikimedia dump files.

The caller selects editions and two immutable release identifiers. Downloads
are sequential so this client stays below Wikimedia's connection limit.
"""

import hashlib
import gzip
import bz2
import json
import re
import sqlite3
import tempfile
from collections.abc import MutableMapping
from contextlib import closing
from datetime import datetime, timezone
from html.parser import HTMLParser
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from .core import wiki_database, write_json


BASE = 'https://dumps.wikimedia.org'
SQL_TABLES = ('page', 'pagelinks', 'linktarget', 'redirect', 'page_props',
              'categorylinks', 'user_groups')
USER_AGENT = 'OrphanWiki/0.1 (public Wikimedia dump research importer)'


class DiskPages(MutableMapping):
    """Stage every article and its parsed links without holding the graph in Python."""

    def __init__(self, connection):
        self.connection = connection
        connection.executescript('''CREATE TABLE pages(id INTEGER PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE links(id INTEGER, target TEXT);
            CREATE TABLE categories(id INTEGER, target TEXT);''')

    def __getitem__(self, key):
        row = self.connection.execute('SELECT data FROM pages WHERE id=?', (key,)).fetchone()
        if row is None:
            raise KeyError(key)
        return json.loads(row[0])

    def __setitem__(self, key, value):
        value = {name: item for name, item in value.items() if name not in ('links', 'categories')}
        self.connection.execute('INSERT OR REPLACE INTO pages VALUES(?,?)',
                                (key, json.dumps(value, ensure_ascii=False)))

    def __delitem__(self, key):
        raise TypeError('Il censimento non permette di eliminare pagine')

    def __iter__(self):
        return (row[0] for row in self.connection.execute('SELECT id FROM pages ORDER BY id'))

    def __len__(self):
        return self.connection.execute('SELECT COUNT(*) FROM pages').fetchone()[0]

    def __contains__(self, key):
        return self.connection.execute('SELECT 1 FROM pages WHERE id=?', (key,)).fetchone() is not None

    def add(self, kind, key, target):
        if kind not in ('links', 'categories'):
            raise ValueError('Tipo di relazione non valido')
        self.connection.execute(f'INSERT INTO {kind} VALUES(?,?)', (key, target))

    def prepare_export(self):
        self.connection.executescript('CREATE INDEX links_page ON links(id); CREATE INDEX categories_page ON categories(id);')
        self.connection.commit()

    def export_pages(self):
        for page_id, encoded in self.connection.execute('SELECT id,data FROM pages ORDER BY id'):
            page = json.loads(encoded)
            for kind in ('links', 'categories'):
                page[kind] = [row[0] for row in self.connection.execute(
                    f'SELECT target FROM {kind} WHERE id=? ORDER BY rowid', (page_id,))]
            yield page


class _Links(HTMLParser):
    def __init__(self):
        super().__init__()
        self.names = []

    def handle_starttag(self, tag, attrs):
        if tag == 'a':
            self.names.extend(value for key, value in attrs if key == 'href')


def _open(url, *, offset=0, method='GET'):
    if not url.startswith(BASE + '/'):
        raise ValueError('URL fuori dall’archivio Wikimedia')
    headers = {'User-Agent': USER_AGENT}
    if offset:
        headers['Range'] = f'bytes={offset}-'
    return urlopen(Request(url, headers=headers, method=method), timeout=120)


def _json(url):
    with _open(url) as response:
        return json.load(response)


def _names(url):
    with _open(url) as response:
        parser = _Links()
        parser.feed(response.read().decode('utf-8'))
    return parser.names


def _hash(path, algorithm):
    digest = hashlib.new(algorithm)
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def _fetch(url, path, expected_size, expected_sha1=None):
    if type(expected_size) is not int or expected_size <= 0:
        raise ValueError('Dimensione attesa non valida')
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists() and path.stat().st_size == expected_size:
        if expected_sha1 is None or _hash(path, 'sha1') == expected_sha1:
            return _hash(path, 'sha256')
    partial = path.with_name(path.name + '.part')
    offset = partial.stat().st_size if partial.exists() else 0
    if offset == expected_size:
        if expected_sha1 is None or _hash(partial, 'sha1') == expected_sha1:
            checksum = _hash(partial, 'sha256')
            partial.replace(path)
            return checksum
        partial.unlink()
        offset = 0
    if offset > expected_size:
        partial.unlink()
        offset = 0
    try:
        response = _open(url, offset=offset)
    except HTTPError as error:
        if error.code != 416:
            raise
        partial.unlink(missing_ok=True)
        offset, response = 0, _open(url)
    with response:
        # Some servers ignore Range. Starting from byte zero prevents duplicated
        # data; the final published checksum guards the remaining cases.
        resumed = offset > 0 and response.status == 206
        if resumed:
            expected_range = f'bytes {offset}-{expected_size - 1}/{expected_size}'
            if response.headers.get('Content-Range') != expected_range:
                raise ValueError(f'Risposta Range inattesa: {path.name}')
        with partial.open('ab' if resumed else 'wb') as output:
            for block in iter(lambda: response.read(1024 * 1024), b''):
                output.write(block)
    if partial.stat().st_size != expected_size:
        raise ValueError(f'Download incompleto: {path.name}')
    if expected_sha1 is not None and _hash(partial, 'sha1') != expected_sha1:
        partial.unlink()
        raise ValueError(f'Checksum Wikimedia errato: {path.name}')
    checksum = _hash(partial, 'sha256')
    partial.replace(path)
    return checksum


def _history_partitions(names, release, database):
    """Reject missing or overlapping years/months before fetching an edition."""
    pattern = re.compile(rf'{re.escape(release)}\.{re.escape(database)}\.(all-time|[0-9]{{4}}(?:-[0-9]{{2}})?)\.tsv\.bz2')
    selected = sorted({name for name in names if pattern.fullmatch(name)})
    periods = [pattern.fullmatch(name).group(1) for name in selected]
    if not periods:
        raise ValueError(f'Nessuna partizione MediaWiki History: {database}')
    if 'all-time' in periods:
        if len(periods) != 1:
            raise ValueError(f'Partizioni MediaWiki History sovrapposte: {database}')
        return selected
    covered = set()
    for period in periods:
        if len(period) == 4:
            months = [(int(period), month) for month in range(1, 13)]
        else:
            year, month = map(int, period.split('-'))
            if not 1 <= month <= 12:
                raise ValueError(f'Mese MediaWiki History non valido: {period}')
            months = [(year, month)]
        for month in months:
            if month in covered:
                raise ValueError(f'Partizioni MediaWiki History sovrapposte: {database}')
            covered.add(month)
    first = min(year * 12 + month for year, month in covered)
    last = max(year * 12 + month for year, month in covered)
    if len(covered) != last - first + 1:
        raise ValueError(f'Partizioni MediaWiki History mancanti: {database}')
    return selected


def _verify_bzip2(path):
    """History has no publisher checksum: require a complete compressed stream."""
    with bz2.open(path, 'rb') as stream:
        for _ in iter(lambda: stream.read(1024 * 1024), b''):
            pass


def fetch_dumps(languages, sql_release, history_release, directory, databases=None, plan_only=False):
    """Fetch full releases, failing on absent partitions or unfinished SQL jobs."""
    if not re.fullmatch(r'20[0-9]{6}', sql_release):
        raise ValueError('Data SQL richiesta in formato YYYYMMDD')
    if not re.fullmatch(r'20[0-9]{2}-(0[1-9]|1[0-2])', history_release):
        raise ValueError('Release MediaWiki History richiesta in formato YYYY-MM')
    if not languages or len(set(languages)) != len(languages):
        raise ValueError('Specificare lingue distinte')
    root = Path(directory)
    databases = {} if databases is None else databases
    if not isinstance(databases, dict) or set(databases) - set(languages):
        raise ValueError('Mappatura database non valida')
    results = []
    for language in languages:
        database = wiki_database(language, databases.get(language))
        sql_url = f'{BASE}/{database}/{sql_release}/dumpstatus.json'
        status = _json(sql_url)
        files = {}
        file_updates = {}
        for job in status.get('jobs', {}).values():
            if job.get('status') == 'done':
                files.update(job.get('files', {}))
                file_updates.update({name: job.get('updated') for name in job.get('files', {})})
        selected = {}
        for table in SQL_TABLES:
            name = f'{database}-{sql_release}-{table}.sql.gz'
            detail = files.get(name)
            if not isinstance(detail, dict) or not re.fullmatch(r'[0-9a-f]{40}', detail.get('sha1', '')):
                raise ValueError(f'Dump SQL incompleto o senza SHA-1: {name}')
            if detail.get('url') != f'/{database}/{sql_release}/{name}':
                raise ValueError(f'Percorso del dump SQL inatteso: {name}')
            if type(detail.get('size')) is not int or detail['size'] <= 0:
                raise ValueError(f'Dimensione del dump non valida: {name}')
            selected[table] = (name, detail)
        history_url = f'{BASE}/other/mediawiki_history/{history_release}/{database}/'
        history_names = _history_partitions(_names(history_url), history_release, database)
        history_sizes = {}
        for name in history_names:
            with _open(history_url + name, method='HEAD') as response:
                size = response.headers.get('Content-Length')
            if size is None or not size.isascii() or not size.isdecimal() or int(size) <= 0:
                raise ValueError(f'Dimensione MediaWiki History mancante: {name}')
            history_sizes[name] = int(size)
        planned_size = sum(detail['size'] for _, detail in selected.values()) + sum(history_sizes.values())
        print(f'{language}: {len(selected)} SQL, {len(history_names)} partizioni History, '
              f'{planned_size / 1024**3:.2f} GiB compressi', flush=True)
        if plan_only:
            results.append({'language': language, 'database': database,
                            'sql_release': sql_release, 'history_release': history_release,
                            'compressed_bytes': planned_size, 'history_partitions': len(history_names)})
            continue
        result = {'language': language, 'database': database,
                  'sql_release': sql_release, 'history_release': history_release,
                  'sql_status_url': sql_url, 'history_index_url': history_url,
                  'files': {}}
        for table, (name, detail) in selected.items():
            size = detail.get('size')
            path = root / database / sql_release / name
            checksum = _fetch(BASE + detail['url'], path, size, detail['sha1'])
            result['files'][table] = {'path': str(path.resolve()), 'bytes': size,
                                      'sha1_publisher': detail['sha1'], 'sha256_local': checksum,
                                      'job_updated': file_updates[name]}
            print(f'Dump {database}: {name} verificato', flush=True)
        for name in history_names:
            url = history_url + name
            size = history_sizes[name]
            path = root / database / history_release / name
            checksum = _fetch(url, path, size)
            try:
                _verify_bzip2(path)
            except (OSError, EOFError):
                path.unlink(missing_ok=True)
                checksum = _fetch(url, path, size)
                _verify_bzip2(path)
            result['files'][name] = {'path': str(path.resolve()), 'bytes': size,
                                     'sha256_local': checksum, 'checksum_scope': 'local_only'}
            print(f'Dump {database}: {name} verificato', flush=True)
        manifest = root / database / f'manifest-{sql_release}-{history_release}.json'
        write_json(manifest, result)
        print(f'Manifest verificato: {manifest}', flush=True)
        results.append(result)
    return results


def _required(columns, *names):
    missing = set(names) - set(columns)
    if missing:
        raise ValueError('Colonne del dump mancanti: ' + ', '.join(sorted(missing)))
    return [columns.index(name) for name in names]


def _read_sql(path, table):
    """Yield bounded native-decoded INSERT blocks and the table's source columns."""
    from ._native import parse_dump_insert
    columns = []
    statement = []
    inside = False
    header = f'INSERT INTO `{table}` VALUES'
    with gzip.open(path, 'rb') as stream:
        for raw_line in stream:
            line = raw_line.decode('utf-8', 'replace')
            if not inside and line.startswith('  `'):
                found = re.match(r'  `([^`]+)`\s', line)
                if found:
                    columns.append(found.group(1))
            if line.startswith(header):
                if not columns:
                    raise ValueError(f'Schema SQL assente: {table}')
                statement = [line]
                inside = True
            elif inside:
                statement.append(line)
            if inside and line.rstrip().endswith(';'):
                yield columns, parse_dump_insert(''.join(statement))
                statement, inside = [], False
    if inside:
        raise ValueError(f'INSERT SQL troncato: {table}')


def _stage_schema(connection):
    connection.executescript('''
      CREATE TABLE IF NOT EXISTS imported(table_name TEXT PRIMARY KEY, sha256 TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS dump_page(id INTEGER PRIMARY KEY, ns INTEGER NOT NULL,
          title TEXT NOT NULL, redirect INTEGER NOT NULL, length INTEGER NOT NULL, revision INTEGER);
      CREATE TABLE IF NOT EXISTS dump_target(id INTEGER PRIMARY KEY, ns INTEGER NOT NULL, title TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS dump_link(source_id INTEGER NOT NULL, target_id INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS dump_redirect(page_id INTEGER NOT NULL, ns INTEGER NOT NULL,
          title TEXT NOT NULL, interwiki TEXT);
      CREATE TABLE IF NOT EXISTS dump_prop(page_id INTEGER NOT NULL, name TEXT NOT NULL, value TEXT);
      CREATE TABLE IF NOT EXISTS dump_category(source_id INTEGER NOT NULL, target_id INTEGER, title TEXT);
      CREATE TABLE IF NOT EXISTS dump_group(user_id INTEGER NOT NULL, name TEXT NOT NULL, expiry TEXT);
    ''')


def _load_table(connection, table, path, digest):
    target = {'page': 'dump_page', 'linktarget': 'dump_target', 'pagelinks': 'dump_link',
              'redirect': 'dump_redirect', 'page_props': 'dump_prop',
              'categorylinks': 'dump_category', 'user_groups': 'dump_group'}[table]
    old = connection.execute('SELECT sha256 FROM imported WHERE table_name=?', (table,)).fetchone()
    if old and old[0] == digest:
        return
    connection.execute('DELETE FROM imported WHERE table_name=?', (table,))
    connection.execute(f'DELETE FROM {target}')
    connection.commit()
    column_set = None
    count = 0
    inserts = {
        'page': 'INSERT INTO dump_page VALUES(?,?,?,?,?,?)',
        'linktarget': 'INSERT INTO dump_target VALUES(?,?,?)',
        'pagelinks': 'INSERT INTO dump_link VALUES(?,?)',
        'redirect': 'INSERT INTO dump_redirect VALUES(?,?,?,?)',
        'page_props': 'INSERT INTO dump_prop VALUES(?,?,?)',
        'categorylinks': 'INSERT INTO dump_category VALUES(?,?,?)',
        'user_groups': 'INSERT INTO dump_group VALUES(?,?,?)',
    }
    requirements = {
        'page': ('page_id', 'page_namespace', 'page_title', 'page_is_redirect', 'page_len', 'page_latest'),
        'linktarget': ('lt_id', 'lt_namespace', 'lt_title'),
        'pagelinks': ('pl_from', 'pl_from_namespace', 'pl_target_id'),
        'redirect': ('rd_from', 'rd_namespace', 'rd_title', 'rd_interwiki'),
        'page_props': ('pp_page', 'pp_propname', 'pp_value'),
        'categorylinks': ('cl_from',),
        'user_groups': ('ug_user', 'ug_group', 'ug_expiry'),
    }
    for columns, rows in _read_sql(path, table):
        if column_set is None:
            column_set = columns
            _required(columns, *requirements[table])
            if table == 'categorylinks' and not ({'cl_target_id', 'cl_to'} & set(columns)):
                raise ValueError('Manca la destinazione delle categorie')
        if columns != column_set:
            raise ValueError(f'Schema variato durante {table}')
        index = {name: n for n, name in enumerate(columns)}
        def field(row, name):
            return row[index[name]]
        selected = []
        for row in rows:
            if len(row) != len(columns):
                raise ValueError(f'Numero di colonne errato in {table}')
            if table == 'page':
                title = field(row, 'page_title')
                if title is None or '\ufffd' in title:
                    raise ValueError('Titolo di pagina non decodificabile')
                selected.append((int(field(row, 'page_id')), int(field(row, 'page_namespace')),
                                 title.replace('_', ' '), int(field(row, 'page_is_redirect')),
                                 int(field(row, 'page_len')), int(field(row, 'page_latest'))))
            elif table == 'linktarget':
                title = field(row, 'lt_title')
                if title is None or '\ufffd' in title:
                    raise ValueError('Destinazione del collegamento non decodificabile')
                selected.append((int(field(row, 'lt_id')), int(field(row, 'lt_namespace')),
                                 title.replace('_', ' ')))
            elif table == 'pagelinks':
                if field(row, 'pl_from_namespace') == '0':
                    selected.append((int(field(row, 'pl_from')), int(field(row, 'pl_target_id'))))
            elif table == 'redirect':
                selected.append((int(field(row, 'rd_from')), int(field(row, 'rd_namespace')),
                                 field(row, 'rd_title').replace('_', ' '), field(row, 'rd_interwiki')))
            elif table == 'page_props':
                name = field(row, 'pp_propname')
                if name in ('wikibase_item', 'hiddencat', 'disambiguation'):
                    selected.append((int(field(row, 'pp_page')), name, field(row, 'pp_value')))
            elif table == 'categorylinks':
                selected.append((int(field(row, 'cl_from')),
                                 int(field(row, 'cl_target_id')) if 'cl_target_id' in index and field(row, 'cl_target_id') else None,
                                 field(row, 'cl_to').replace('_', ' ') if 'cl_to' in index and field(row, 'cl_to') else None))
            else:
                selected.append((int(field(row, 'ug_user')), field(row, 'ug_group'),
                                 field(row, 'ug_expiry')))
        if selected:
            connection.executemany(inserts[table], selected)
            count += len(selected)
        if count and count % 1000000 < len(selected):
            connection.commit()
            print(f'Dump {table}: {count:,} righe', flush=True)
    connection.commit()
    connection.execute('INSERT INTO imported VALUES(?,?)', (table, digest))
    connection.commit()
    print(f'Dump {table}: {count:,} righe acquisite', flush=True)


def import_graph(manifest_path, output, work_dir):
    """Build the complete article and parsed-link graph from SQL dumps."""
    manifest = json.loads(Path(manifest_path).read_text())
    language = manifest['language']
    database = wiki_database(language, manifest.get('database'))
    if manifest.get('database') != database:
        raise ValueError('Identità della wiki nel manifest non valida')
    output = Path(output)
    stage_path = Path(work_dir) / f'{database}-{manifest["sql_release"]}.sqlite'
    if output.resolve() == stage_path.resolve() or output.resolve() == Path(manifest_path).resolve():
        raise ValueError('Output e input/staging devono essere distinti')
    stage_path.parent.mkdir(parents=True, exist_ok=True)
    with closing(sqlite3.connect(stage_path)) as connection:
        connection.execute('PRAGMA temp_store=FILE')
        connection.execute('PRAGMA cache_size=-8192')
        _stage_schema(connection)
        for table in SQL_TABLES:
            record = manifest['files'][table]
            path = Path(record['path'])
            if not path.is_file() or path.stat().st_size != record['bytes'] or _hash(path, 'sha256') != record['sha256_local']:
                raise ValueError(f'Dump modificato o incompleto: {table}')
            _load_table(connection, table, path, record['sha256_local'])
        connection.executescript('''
          CREATE INDEX IF NOT EXISTS dump_link_source ON dump_link(source_id);
          CREATE INDEX IF NOT EXISTS dump_prop_page_name ON dump_prop(page_id,name);
          CREATE INDEX IF NOT EXISTS dump_page_ns_title ON dump_page(ns,title);
          CREATE INDEX IF NOT EXISTS dump_category_source ON dump_category(source_id);
        ''')
        expected = connection.execute('SELECT COUNT(*) FROM dump_page WHERE ns=0 AND redirect=0').fetchone()[0]
        if expected == 0:
            raise ValueError('Il dump non contiene voci')
        connection.executescript('DROP TABLE IF EXISTS pages; DROP TABLE IF EXISTS links; DROP TABLE IF EXISTS categories;')
        pages = DiskPages(connection)
        qids = dict(connection.execute("SELECT page_id,value FROM dump_prop WHERE name='wikibase_item'"))
        for pid, title, length, revision in connection.execute('SELECT id,title,length,revision FROM dump_page WHERE ns=0 AND redirect=0 ORDER BY id'):
            item = qids.get(pid)
            pages[pid] = {'page_id': pid, 'title': title, 'length_bytes': length,
                          'revision_id': revision, 'wikibase_item': item if item and re.fullmatch(r'Q[1-9][0-9]*', item) else None,
                          'created_at': None, 'page_created_at': None,
                          'first_public_revision_at': None, 'creation_date_source': None,
                          'creation_date_status': 'dump_history_not_joined'}
        connection.execute('''INSERT INTO links
          SELECT l.source_id,t.title FROM dump_link l JOIN dump_target t ON t.id=l.target_id
          JOIN dump_page p ON p.id=l.source_id
          WHERE t.ns=0 AND p.ns=0 AND p.redirect=0''')
        connection.execute('''INSERT INTO categories
          SELECT c.source_id,COALESCE(t.title,c.title) FROM dump_category c
          LEFT JOIN dump_target t ON t.id=c.target_id
          JOIN dump_page p ON p.id=c.source_id
          WHERE p.ns=0 AND p.redirect=0 AND (t.ns=14 OR c.title IS NOT NULL)
          AND NOT EXISTS (SELECT 1 FROM dump_page h JOIN dump_prop hp ON hp.page_id=h.id
              WHERE h.ns=14 AND hp.name='hiddencat' AND h.title=COALESCE(t.title,c.title))''')
        redirects = {source: target for source, target in connection.execute('''
          SELECT p.title,r.title FROM dump_redirect r JOIN dump_page p ON p.id=r.page_id
          WHERE p.ns=0 AND p.redirect=1 AND r.ns=0 AND COALESCE(r.interwiki,'')='' ''')}
        parents = {}
        for child, parent in connection.execute('''SELECT p.title,COALESCE(t.title,c.title)
          FROM dump_category c JOIN dump_page p ON p.id=c.source_id
          LEFT JOIN dump_target t ON t.id=c.target_id
          WHERE p.ns=14 AND (t.ns=14 OR c.title IS NOT NULL)
          AND NOT EXISTS (SELECT 1 FROM dump_page h JOIN dump_prop hp ON hp.page_id=h.id
              WHERE h.ns=14 AND hp.name='hiddencat' AND h.title=COALESCE(t.title,c.title))'''):
            parents.setdefault(child, []).append(parent)
        pages.prepare_export()
        updated = [_history_timestamp(manifest['files'][table].get('job_updated'))
                   for table in SQL_TABLES]
        start = manifest['sql_release'][:4] + '-' + manifest['sql_release'][4:6] + '-' + manifest['sql_release'][6:] + 'T00:00:00+00:00'
        finish = max((value for value in updated if value), default=start)
        raw = {'metadata': {'language': language, 'database': database,
                             'source': 'Wikimedia SQL dumps / ' + database,
                             'sql_release': manifest['sql_release'],
                             'dump_manifest_sha256': _hash(manifest_path, 'sha256'),
                             'complete': True, 'demo': False,
                             'started_at': start, 'finished_at': finish,
                             'temporal_consistency': 'independent_dump_tables_and_publication_interval',
                             'creation_date_rule': 'unavailable_pending_dump_history_validation',
                             'creator_collection_status': 'pending',
                             'article_features': {'age': 'unavailable_pending_dump_history_validation',
                               'length': 'page.page_len', 'topic_inputs': 'pagelinks + page_props.wikibase_item',
                               'topics': 'unavailable_no_local_model',
                               'quality': 'unavailable_no_wikitext_in_sql_dumps',
                               'pageviews': 'unavailable_not_in_sql_dumps',
                               'biographical_gender': 'unavailable_not_in_sql_dumps',
                               'bot_at_creation': 'unavailable_no_complete_historical_membership'},
                             'item_mapping': {'source': 'page_props.wikibase_item', 'complete': True},
                             'dump_manifest': str(Path(manifest_path).resolve())},
               'pages': [], 'redirects': redirects, 'category_parents': parents}
        write_json(output, raw, arrays={'pages': pages.export_pages()})
        if len(pages) != expected:
            raise ValueError('Popolazione del dump cambiata durante l’esportazione')
    print(f'Grafo da dump: {expected:,} voci; {output}', flush=True)
    return expected


def _history_schema(connection):
    connection.executescript('''
      CREATE TABLE IF NOT EXISTS history_imported(
        file TEXT NOT NULL, phase TEXT NOT NULL, sha256 TEXT NOT NULL,
        PRIMARY KEY(file,phase));
      CREATE TABLE IF NOT EXISTS history_initial(
        file TEXT NOT NULL, page_id INTEGER NOT NULL, revision_id INTEGER NOT NULL,
        at TEXT NOT NULL, user_id INTEGER, hidden INTEGER NOT NULL,
        groups TEXT, tags TEXT, registration TEXT, page_created TEXT,
        page_first TEXT, before_creation INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS history_page_event(
        file TEXT NOT NULL, page_id INTEGER NOT NULL, event TEXT NOT NULL,
        at TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS history_edit(
        file TEXT NOT NULL, revision_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL, at TEXT NOT NULL, namespace INTEGER NOT NULL);
    ''')


def _history_timestamp(value):
    if not isinstance(value, str) or not re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?', value):
        return None
    try:
        return datetime.fromisoformat(value).replace(tzinfo=timezone.utc).isoformat()
    except ValueError:
        return None


def _history_rows(path, database):
    with bz2.open(path, 'rb') as stream:
        for line_number, line in enumerate(stream, 1):
            fields = line.rstrip(b'\n').decode('utf-8').split('\t')
            if len(fields) != 78 or fields[0] != database:
                raise ValueError(f'MediaWiki History: schema o wiki inattesi in {path.name}:{line_number}')
            yield fields


def _history_scan(connection, name, path, digest, phase, database, page_ids, user_ids=None):
    previous = connection.execute('SELECT sha256 FROM history_imported WHERE file=? AND phase=?',
                                  (name, phase)).fetchone()
    if previous and previous[0] == digest:
        return
    table = 'history_initial' if phase == 'initial' else 'history_edit'
    connection.execute(f'DELETE FROM {table} WHERE file=?', (name,))
    if phase == 'initial':
        connection.execute('DELETE FROM history_page_event WHERE file=?', (name,))
    connection.execute('DELETE FROM history_imported WHERE file=? AND phase=?', (name, phase))
    connection.commit()
    batch, events = [], []
    count = 0
    for row in _history_rows(path, database):
        if phase == 'initial':
            page_id = int(row[28]) if row[28].isdecimal() else None
            if page_id not in page_ids:
                continue
            if row[2] == 'page' and row[3] in ('create', 'import', 'merge'):
                events.append((name, page_id, row[3], row[4]))
            elif row[2] == 'revision' and row[3] == 'create' and row[61] == '0':
                if not row[60].isdecimal():
                    raise ValueError(f'Revisione iniziale priva di ID: {page_id}')
                user_id = int(row[6]) if row[6].isdecimal() else None
                deleted = {part.strip() for part in row[63].split(',')}
                batch.append((name, page_id, int(row[60]), row[4], user_id,
                              int('user' in deleted or 'user_is_deleted' in deleted),
                              row[12] or None, row[77] or None, row[23] or None,
                              row[37] or None, row[38] or None,
                              int(row[76] == 'true')))
        elif row[2] == 'revision' and row[3] == 'create' and row[6].isdecimal():
            user_id = int(row[6])
            if user_id in user_ids and row[60].isdecimal():
                historical_ns = row[31] or row[33]
                if historical_ns.lstrip('-').isdecimal():
                    batch.append((name, int(row[60]), user_id, row[4], int(historical_ns)))
        if len(batch) >= 5000 or len(events) >= 5000:
            if batch:
                query = ('INSERT INTO history_initial VALUES(?,?,?,?,?,?,?,?,?,?,?,?)'
                         if phase == 'initial' else 'INSERT INTO history_edit VALUES(?,?,?,?,?)')
                connection.executemany(query, batch)
                count += len(batch)
                batch.clear()
            if events:
                connection.executemany('INSERT INTO history_page_event VALUES(?,?,?,?)', events)
                events.clear()
            connection.commit()
    if batch:
        query = ('INSERT INTO history_initial VALUES(?,?,?,?,?,?,?,?,?,?,?,?)'
                 if phase == 'initial' else 'INSERT INTO history_edit VALUES(?,?,?,?,?)')
        connection.executemany(query, batch)
        count += len(batch)
    if events:
        connection.executemany('INSERT INTO history_page_event VALUES(?,?,?,?)', events)
    connection.commit()
    connection.execute('INSERT INTO history_imported VALUES(?,?,?)', (name, phase, digest))
    connection.commit()
    print(f'MediaWiki History {database}: {name} {phase}, {count:,} righe utili', flush=True)


def _history_records(connection, page_ids, database):
    initial = {}
    for row in connection.execute('''SELECT page_id,revision_id,at,user_id,hidden,groups,tags,
      registration,page_created,page_first,before_creation FROM history_initial
      ORDER BY page_id,at,revision_id'''):
        initial.setdefault(row[0], []).append(row[1:])
    events = {}
    for page_id, event, timestamp in connection.execute(
            'SELECT page_id,event,at FROM history_page_event'):
        events.setdefault(page_id, []).append((event, timestamp))
    records = []
    for page_id in sorted(page_ids):
        revisions = initial.get(page_id, [])
        page_events = events.get(page_id, [])
        record = {'page_id': page_id, 'first_revision_id': None,
                  'article_created_at': None, 'creator_key': None,
                  'creator_user_id': None, 'creator_attribution': 'history_missing_initial_revision',
                  'creator_groups_at_creation': None, 'creator_groups_status': 'unknown',
                  'creator_continuity_months_before_creation': None,
                  'creator_continuity_bucket_before_creation': None,
                  'creator_prior_edits_main_history': None,
                  'creator_prior_edits_other_history': None,
                  'creator_history_status': 'unavailable'}
        if len(revisions) > 1:
            record['creator_attribution'] = 'history_ambiguous_initial_revisions'
        elif revisions:
            rev_id, timestamp, user_id, hidden, groups, tags, registration, created, first, before = revisions[0]
            record.update(first_revision_id=rev_id,
                          article_created_at=_history_timestamp(timestamp))
            matching_creates = [date for event, date in page_events if event == 'create']
            imported = any(event in ('import', 'merge') for event, _ in page_events)
            if not record['article_created_at'] or (created and created != timestamp) or (first and first != timestamp):
                record['creator_attribution'] = 'history_inconsistent_creation_dates'
            elif imported or before or len(matching_creates) > 1:
                record['creator_attribution'] = 'history_imported_or_ambiguous'
            elif matching_creates and matching_creates[0] != timestamp:
                record['creator_attribution'] = 'history_create_event_mismatch'
            elif hidden or user_id is None:
                record['creator_attribution'] = 'history_hidden_or_unregistered_creator'
            else:
                record.update(creator_user_id=user_id,
                              creator_key=f'{database}:history-user:{user_id}',
                              creator_attribution='history_initial_revision_observed',
                              creator_groups_at_creation=groups.split(',') if groups else None,
                              creator_groups_status='observed' if groups else 'unknown',
                              creator_history_status='pending_activity',
                              creator_registered_at=_history_timestamp(registration))
                if tags:
                    record['creation_tags_history'] = tags.split(',')
        records.append(record)
    return records


def import_history(manifest_path, output, work_dir):
    """Keep reconstructed creator evidence separate from the SQL graph."""
    from ._native import history_activity

    manifest = json.loads(Path(manifest_path).read_text())
    language = manifest['language']
    database = wiki_database(language, manifest.get('database'))
    stage_path = Path(work_dir) / f'{database}-{manifest["sql_release"]}.sqlite'
    if not stage_path.exists():
        raise ValueError('Importare prima il grafo SQL della stessa wiki')
    files = [(name, data) for name, data in manifest['files'].items()
             if name.endswith('.tsv.bz2')]
    if not files:
        raise ValueError('Manifest senza partizioni MediaWiki History')
    output = Path(output)
    if output.resolve() in {stage_path.resolve(), Path(manifest_path).resolve()}:
        raise ValueError('Output e input/staging devono essere distinti')
    with closing(sqlite3.connect(stage_path)) as connection:
        connection.execute('PRAGMA temp_store=FILE')
        connection.execute('PRAGMA cache_size=-8192')
        _history_schema(connection)
        page_ids = {row[0] for row in connection.execute(
            'SELECT id FROM dump_page WHERE ns=0 AND redirect=0')}
        if not page_ids:
            raise ValueError('Staging SQL incompleto')
        for name, data in files:
            path = Path(data['path'])
            if (not path.is_file() or path.stat().st_size != data['bytes']
                    or _hash(path, 'sha256') != data['sha256_local']):
                raise ValueError(f'Partizione MediaWiki History modificata: {name}')
            _history_scan(connection, name, path, data['sha256_local'], 'initial',
                          database, page_ids)
        records = _history_records(connection, page_ids, database)
        user_ids = {record['creator_user_id'] for record in records
                    if record['creator_user_id'] is not None}
        for name, data in files:
            _history_scan(connection, name, Path(data['path']), data['sha256_local'],
                          'activity', database, page_ids, user_ids)
        connection.execute('CREATE INDEX IF NOT EXISTS history_edit_user_time ON history_edit(user_id,at)')
        by_user = {}
        for record in records:
            if record['creator_user_id'] is not None:
                by_user.setdefault(record['creator_user_id'], []).append(record)
        previous = None
        edits = []
        def finish(user, history):
            if user is None:
                return
            group = by_user[user]
            try:
                summary = history_activity(history, [r['article_created_at'].replace('T', ' ').removesuffix('+00:00')
                                                     for r in group])
            except ValueError:
                for record in group:
                    record['creator_history_status'] = 'invalid_history_timestamp'
                return
            for record, (main, other, months, bucket) in zip(group, summary):
                record.update(creator_prior_edits_main_history=main,
                              creator_prior_edits_other_history=other,
                              creator_continuity_months_before_creation=months,
                              creator_continuity_bucket_before_creation=bucket,
                              creator_history_status='observed_surviving_history')
        for user, timestamp, namespace in connection.execute(
                'SELECT user_id,at,namespace FROM history_edit ORDER BY user_id,at'):
            if previous is not None and user != previous:
                finish(previous, edits)
                edits = []
            previous = user
            edits.append((timestamp, namespace))
        finish(previous, edits)
        for user in user_ids - {record['creator_user_id'] for record in records
                                if record['creator_history_status'] == 'observed_surviving_history'}:
            finish(user, [])
        metadata = {'language': language, 'database': database,
                    'source': 'MediaWiki History dumps / ' + database,
                    'sql_release': manifest['sql_release'],
                    'history_release': manifest['history_release'],
                    'complete': True,
                    'creator_definition': 'single visible parent-zero revision in reconstructed history; not an observed creation event',
                    'experience_scope': 'public surviving revisions in the history release, historical namespace, strictly earlier timestamp',
                    'continuity_rule': 'consecutive completed precreation calendar months with at least five edits; Vital Signs stability adaptation',
                    'group_scope': 'nonempty reconstructed historical group list at first revision; blank means unknown',
                    'history_files': {name: data['sha256_local'] for name, data in files}}
        write_json(output, {'metadata': metadata, 'pages': []}, arrays={'pages': records})
    print(f'Cronologia da dump: {len(records):,} voci; {output}', flush=True)
    return len(records)


def _next_for_page(cursor, current, page_id):
    """Consume an ID-sorted SQLite cursor without holding other pages."""
    values = []
    while current is not None and current[0] < page_id:
        raise ValueError('Staging con relazione senza la voce sorgente')
    while current is not None and current[0] == page_id:
        values.append(current[1])
        current = next(cursor, None)
    return values, current


def _stage_page_batches(connection, batch_size, include_links):
    """Merge indexed page, link and category streams in bounded batches."""
    pages = connection.execute('SELECT id,data FROM pages ORDER BY id')
    links = iter(connection.execute('SELECT id,target FROM links ORDER BY id,rowid')) if include_links else iter(())
    categories = iter(connection.execute('SELECT id,target FROM categories ORDER BY id,rowid'))
    next_link, next_category = next(links, None), next(categories, None)
    batch = []
    for page_id, encoded in pages:
        targets, next_link = _next_for_page(links, next_link, page_id)
        labels, next_category = _next_for_page(categories, next_category, page_id)
        batch.append((page_id, json.loads(encoded), targets, labels))
        if len(batch) == batch_size:
            yield batch
            batch = []
    if next_link is not None or next_category is not None:
        raise ValueError('Staging con relazioni non associate a voci')
    if batch:
        yield batch


def _stage_comparison_items(source, destination, language, identifiers):
    """Retain only unambiguous local QIDs before streaming matchable edges."""
    valid = re.compile(r'Q[1-9][0-9]*\Z', re.ASCII)
    unique, ambiguous = {}, set()
    count = 0
    for position, (page_id, encoded) in enumerate(source.execute('SELECT id,data FROM pages ORDER BY id')):
        if position >= len(identifiers) or page_id != identifiers[position]:
            raise ValueError('Staging con ordine o identificativi delle pagine diversi dal dump')
        item = json.loads(encoded).get('wikibase_item')
        if isinstance(item, str) and valid.fullmatch(item):
            if item in unique:
                ambiguous.add(item)
            else:
                unique[item] = (position, page_id)
        count += 1
    if count != len(identifiers):
        raise ValueError('Staging con identificativi delle pagine mancanti')
    for item in ambiguous:
        unique.pop(item, None)
    with destination:
        destination.executemany('INSERT INTO items VALUES(?,?,?)',
                                ((language, item, page_id) for item, (_, page_id) in unique.items()))
        destination.executemany('INSERT INTO ambiguous VALUES(?,?)',
                                ((language, item) for item in ambiguous))
    positions = [position for position, _ in unique.values()]
    return positions


def _read_first_metadata(path):
    from .core import iter_export_sections
    events = iter_export_sections(path)
    try:
        section, _, metadata = next(events)
        if section != 'metadata' or not isinstance(metadata, dict):
            raise ValueError('Metadati JSON mancanti')
        return metadata
    finally:
        events.close()


def _history_stage(path, connection, graph_metadata, expected_count):
    """Validate and index a complete supplement without materializing it."""
    from .core import iter_export_sections
    metadata = _read_first_metadata(path)
    database = graph_metadata['database']
    if (metadata.get('language') != graph_metadata['language'] or metadata.get('database') != database
            or metadata.get('source') != 'MediaWiki History dumps / ' + database
            or metadata.get('complete') is not True
            or metadata.get('sql_release') != graph_metadata['sql_release']):
        raise ValueError('Cronologia non corrispondente al grafo SQL')
    connection.execute('CREATE TABLE history(id INTEGER PRIMARY KEY, data TEXT NOT NULL)')
    encoder = json.JSONEncoder(ensure_ascii=True, sort_keys=True, allow_nan=False)
    digest = hashlib.sha256()
    digest.update(b'{"metadata": ')
    for chunk in encoder.iterencode(metadata):
        digest.update(chunk.encode('utf-8'))
    digest.update(b', "pages": [')
    count = matched = 0
    events = iter_export_sections(path)
    try:
        next(events)
        batch = []
        for section, _, record in events:
            if section != 'pages' or type(record.get('page_id')) is not int:
                raise ValueError('Cronologia con record non valido')
            if count:
                digest.update(b', ')
            for chunk in encoder.iterencode(record):
                digest.update(chunk.encode('utf-8'))
            count += 1
            if (record.get('article_created_at')
                    and record.get('creator_history_status') == 'observed_surviving_history'):
                matched += 1
            batch.append((record['page_id'], json.dumps(record, ensure_ascii=False)))
            if len(batch) == 1000:
                with connection:
                    connection.executemany('INSERT INTO history VALUES(?,?)', batch)
                batch.clear()
                if count % 100000 == 0:
                    print(f'Cronologia SQL {graph_metadata["language"]}: {count:,}/{expected_count:,} voci', flush=True)
        if batch:
            with connection:
                connection.executemany('INSERT INTO history VALUES(?,?)', batch)
    finally:
        events.close()
    digest.update(b']}')
    if count != expected_count:
        raise ValueError('Cronologia incompleta rispetto al censimento SQL')
    return {**metadata, 'status': 'collected', 'matched_pages': matched,
            'unmatched_pages': expected_count - matched, 'input_sha256': digest.hexdigest()}


def _topic_stage(path, connection, graph_metadata, expected_count, raw_checksum):
    """Index a complete, exact-graph topic checkpoint without loading it in RAM."""
    from .core import iter_export_sections
    metadata = _read_first_metadata(path)
    taxonomy = json.loads(Path('config/topics.json').read_text())
    if (metadata.get('language') != graph_metadata['language']
            or metadata.get('source') != 'local_model_on_verified_SQL_dump_outlinks'
            or metadata.get('complete') is not True
            or metadata.get('graph_finished_at') != graph_metadata['finished_at']
            or metadata.get('input_file_sha256') != raw_checksum
            or metadata.get('model') != taxonomy):
        raise ValueError('Temi non corrispondenti al grafo SQL e al modello configurato')
    labels = set(taxonomy['labels'])
    connection.execute('CREATE TABLE topics(id INTEGER PRIMARY KEY, data TEXT NOT NULL)')
    events = iter_export_sections(path)
    count = predicted = 0
    try:
        next(events)
        batch = []
        for section, _, record in events:
            if section != 'pages' or type(record.get('page_id')) is not int:
                raise ValueError('Checkpoint dei temi con record non valido')
            status = record.get('topic_status')
            values = record.get('topic_labels')
            inputs = record.get('topic_input_count')
            if (type(inputs) is not int or inputs < 0
                    or status not in ('predicted', 'no_model_vocabulary', 'no_mapped_outlinks')
                    or (status == 'predicted') != (inputs > 0)
                    or (status == 'predicted') != isinstance(values, list)
                    or (isinstance(values, list) and (
                        any(not isinstance(label, str) for label in values)
                        or len(values) != len(set(values))
                        or any(label not in labels for label in values)))):
                raise ValueError('Predizione dei temi non valida')
            predicted += status == 'predicted'
            count += 1
            batch.append((record['page_id'], json.dumps(record, ensure_ascii=False)))
            if len(batch) == 1000:
                with connection:
                    connection.executemany('INSERT INTO topics VALUES(?,?)', batch)
                batch.clear()
        if batch:
            with connection:
                connection.executemany('INSERT INTO topics VALUES(?,?)', batch)
    finally:
        events.close()
    if (count != expected_count or metadata.get('predicted_pages') != predicted
            or metadata.get('missing_pages') != count - predicted):
        raise ValueError('Checkpoint dei temi incompleto')
    return {**metadata, 'status': 'predicted', 'input_checksum_verified': True}


def predict_imported_topics(raw_path, output, model, spec, batch_size=256, deadline=None, work_dir=None):
    """Infer all SQL-dump topics from indexed outlinks, with bounded edge batches.

    The model, title/QID index and redirects stay resident. Article links and
    predictions cross Python/Rust in batches; the completed checkpoint alone
    is published. The canonical graph hash is reconstructed from the verified
    staging records using the same sorted-JSON contract as ``json_sha256``.
    """
    import time
    from ._native import TopicInputAccumulator, threshold_topics, unique_items
    from .core import _json_chunks, wiki_database, write_json

    raw_path, output = Path(raw_path), Path(output)
    if type(batch_size) is not int or not 1 <= batch_size <= 10000:
        raise ValueError('Dimensione del blocco non valida')
    metadata = _read_first_metadata(raw_path)
    language = metadata.get('language')
    database = wiki_database(language, metadata.get('database'))
    if (metadata.get('complete') is not True
            or metadata.get('item_mapping') != {'source': 'page_props.wikibase_item', 'complete': True}
            or metadata.get('source') != 'Wikimedia SQL dumps / ' + database):
        raise ValueError('Serve un grafo SQL verificato con QID completi')
    manifest_path = Path(metadata['dump_manifest'])
    manifest = json.loads(manifest_path.read_text())
    if (_hash(manifest_path, 'sha256') != metadata.get('dump_manifest_sha256')
            or manifest.get('language') != language or manifest.get('database') != database
            or manifest.get('sql_release') != metadata.get('sql_release')):
        raise ValueError('Manifest SQL non corrispondente al grafo importato')
    stage_dir = Path(work_dir) if work_dir else manifest_path.parent.parent / 'staging'
    stage_path = stage_dir / f'{database}-{metadata["sql_release"]}.sqlite'
    if (not stage_path.is_file() or output.resolve() in {raw_path.resolve(), stage_path.resolve()}):
        raise ValueError('Staging SQL mancante o output sovrapposto agli input')

    def check_deadline():
        if deadline is not None and time.monotonic() >= deadline:
            raise TimeoutError('Budget di tempo esaurito; nessun supplemento incompleto pubblicato')

    check_deadline()
    started = datetime.now(timezone.utc).isoformat()
    begin = time.monotonic()
    with closing(sqlite3.connect(f'file:{stage_path.resolve()}?mode=ro', uri=True)) as source:
        imported = dict(source.execute('SELECT table_name,sha256 FROM imported'))
        if any(imported.get(table) != manifest['files'][table]['sha256_local'] for table in SQL_TABLES):
            raise ValueError('Staging SQL incompleto o non corrispondente al manifest')
        expected = source.execute('SELECT COUNT(*) FROM dump_page WHERE ns=0 AND redirect=0').fetchone()[0]
        if expected == 0 or source.execute('SELECT COUNT(*) FROM pages').fetchone()[0] != expected:
            raise ValueError('Popolazione dello staging SQL incompleta')
        identities = []
        for page_id, encoded in source.execute('SELECT id,data FROM pages ORDER BY id'):
            page = json.loads(encoded)
            identities.append((page_id, page['title'], page.get('wikibase_item'), page.get('revision_id')))
        if len(identities) != expected or len({row[0] for row in identities}) != expected:
            raise ValueError('Identità delle pagine SQL incomplete')
        titles = [row[1] for row in identities]
        items = [row[2] if isinstance(row[2], str) else None for row in identities]
        unique, _ = unique_items(items)
        known = {item for item in unique if model.get_word_id(item) >= 0}
        del unique
        redirects = {page: target for page, target in source.execute('''
            SELECT p.title,r.title FROM dump_redirect r JOIN dump_page p ON p.id=r.page_id
            WHERE p.ns=0 AND p.redirect=1 AND r.ns=0 AND COALESCE(r.interwiki,'')='' ''')}
        parents = {}
        for child, parent in source.execute('''SELECT p.title,COALESCE(t.title,c.title)
            FROM dump_category c JOIN dump_page p ON p.id=c.source_id
            LEFT JOIN dump_target t ON t.id=c.target_id
            WHERE p.ns=14 AND (t.ns=14 OR c.title IS NOT NULL)
            AND NOT EXISTS (SELECT 1 FROM dump_page h JOIN dump_prop hp ON hp.page_id=h.id
                WHERE h.ns=14 AND hp.name='hiddencat' AND h.title=COALESCE(t.title,c.title))'''):
            parents.setdefault(child, []).append(parent)
        inputs = TopicInputAccumulator(titles, items, redirects, known)
        del titles, items, known
        graph_seconds = time.monotonic() - begin
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='orphanwiki-topics-', dir=output.parent) as temporary:
            spool = Path(temporary) / 'records.jsonl'
            digest = hashlib.sha256()

            def update(value):
                for chunk in _json_chunks(value, ensure_ascii=True, sort_keys=True):
                    digest.update(chunk.encode('utf-8'))

            digest.update(b'{"category_parents": ')
            update(parents)
            digest.update(b', "metadata": ')
            update(metadata)
            digest.update(b', "pages": [')
            labels = set(spec['labels'])
            predicted_count = processed = 0
            with spool.open('w', encoding='utf-8') as stream:
                for batch in _stage_page_batches(source, batch_size, include_links=True):
                    check_deadline()
                    results = inputs.add_batch([links for _, _, links, _ in batch])
                    eligible = [position for position, (tokens, _, _) in enumerate(results) if tokens]
                    selected = []
                    if eligible:
                        predicted, probabilities = model.predict(
                            [results[position][0] for position in eligible], k=-1, threshold=0.0)
                        if len(predicted) != len(eligible) or len(probabilities) != len(eligible):
                            raise ValueError('Risposta del modello incompleta')
                        selected = threshold_topics(labels, [list(row) for row in predicted],
                                                    [[float(score) for score in row] for row in probabilities])
                    predictions = dict(zip(eligible, selected))
                    for position, (page_id, page, links, categories) in enumerate(batch):
                        if identities[processed][0] != page_id:
                            raise ValueError('Staging con ordine delle pagine diverso dal dump')
                        if processed:
                            digest.update(b', ')
                        update({**page, 'links': links, 'categories': categories})
                        _, count, mapped = results[position]
                        status = 'predicted' if position in predictions else (
                            'no_model_vocabulary' if mapped else 'no_mapped_outlinks')
                        record = {'page_id': page_id, 'revision_id': identities[processed][3],
                                  'topic_labels': predictions.get(position), 'topic_input_count': count,
                                  'topic_status': status}
                        stream.write(json.dumps(record, ensure_ascii=False, separators=(',', ':')) + '\n')
                        processed += 1
                    predicted_count += len(eligible)
                    if processed % 100000 < len(batch):
                        print(f'Temi SQL {language}: {processed:,}/{expected:,} voci', flush=True)
            inputs.finish()
            if processed != expected:
                raise ValueError('Predizioni incomplete rispetto al censimento SQL')
            digest.update(b'], "redirects": ')
            update(redirects)
            digest.update(b'}')
            check_deadline()
            raw_checksum = _hash(raw_path, 'sha256')
            topic_metadata = {'language': language, 'source': 'local_model_on_verified_SQL_dump_outlinks',
                              'demo': False, 'complete': True, 'started_at': started,
                              'finished_at': datetime.now(timezone.utc).isoformat(),
                              'input_sha256': digest.hexdigest(), 'input_file_sha256': raw_checksum,
                              'graph_finished_at': metadata['finished_at'], 'model': spec,
                              'top_level_rule': 'any_child_probability_gt_0.5',
                              'scope': 'model topic labels, not biographical gender observations',
                              'timing': {'native_graph_seconds': graph_seconds,
                                         'prediction_seconds': time.monotonic() - begin - graph_seconds},
                              'predicted_pages': predicted_count, 'missing_pages': expected - predicted_count}

            def records():
                with spool.open(encoding='utf-8') as stream:
                    for line in stream:
                        yield json.loads(line)

            write_json(output, {'metadata': topic_metadata, 'pages': []}, arrays={'pages': records()})
            return topic_metadata


def analyze_imported_dump(raw_path, history_path, output, rules, work_dir=None, topic_path=None,
                          comparison_connection=None):
    """Analyze a complete imported SQL edition with bounded Python/Rust batches.

    The verified import stage is the source of graph relationships; the raw
    file supplies its recorded identity. Memory is O(titles + redirects +
    category hierarchy + one batch), independent of link count. A comparison
    connection receives resolved QID edges and observations directly; otherwise
    the output retains the ordinary single-edition export contract.
    """
    from ._native import GraphAccumulator, CategoryBatchClassifier, article_ages
    from .categories import format_choices, native_rules
    from .core import initialize_creator_rows, observation_row, _join_history_rows
    from .__main__ import _publish_prepared, export_result, manifest_entry

    raw_path, output = Path(raw_path), Path(output)
    graph_meta = _read_first_metadata(raw_path)
    language = graph_meta.get('language')
    database = wiki_database(language, graph_meta.get('database'))
    if (graph_meta.get('complete') is not True
            or graph_meta.get('source') != 'Wikimedia SQL dumps / ' + database
            or graph_meta.get('sql_release') is None):
        raise ValueError('Serve un grafo SQL importato e completo')
    if (comparison_connection is not None and
            graph_meta.get('item_mapping') != {'source': 'page_props.wikibase_item', 'complete': True}):
        raise ValueError('Identificativi interlingua non raccolti dal dump SQL completo')
    manifest_path = Path(graph_meta['dump_manifest'])
    manifest = json.loads(manifest_path.read_text())
    if (_hash(manifest_path, 'sha256') != graph_meta.get('dump_manifest_sha256')
            or manifest.get('language') != language or manifest.get('database') != database
            or manifest.get('sql_release') != graph_meta['sql_release']):
        raise ValueError('Manifest SQL non corrispondente al grafo importato')
    stage_dir = Path(work_dir) if work_dir else manifest_path.parent.parent / 'staging'
    stage_path = stage_dir / f'{database}-{graph_meta["sql_release"]}.sqlite'
    if not stage_path.is_file() or output.resolve() in {raw_path.resolve(), stage_path.resolve()}:
        raise ValueError('Staging SQL mancante o output sovrapposto agli input')
    source = sqlite3.connect(f'file:{stage_path.resolve()}?mode=ro', uri=True)
    try:
        imported = dict(source.execute('SELECT table_name,sha256 FROM imported'))
        if any(imported.get(table) != manifest['files'][table]['sha256_local'] for table in SQL_TABLES):
            raise ValueError('Staging SQL incompleto o non corrispondente al manifest')
        expected = source.execute('SELECT COUNT(*) FROM dump_page WHERE ns=0 AND redirect=0').fetchone()[0]
        if expected == 0 or source.execute('SELECT COUNT(*) FROM pages').fetchone()[0] != expected:
            raise ValueError('Popolazione dello staging SQL incompleta')
        identities = list(source.execute('SELECT id,title FROM dump_page WHERE ns=0 AND redirect=0 ORDER BY id'))
        identifiers = [page_id for page_id, _ in identities]
        titles = [title for _, title in identities]
        del identities
        redirects = {page: target for page, target in source.execute('''
            SELECT p.title,r.title FROM dump_redirect r JOIN dump_page p ON p.id=r.page_id
            WHERE p.ns=0 AND p.redirect=1 AND r.ns=0 AND COALESCE(r.interwiki,'')='' ''')}
        parents = {}
        for child, parent in source.execute('''SELECT p.title,COALESCE(t.title,c.title)
            FROM dump_category c JOIN dump_page p ON p.id=c.source_id
            LEFT JOIN dump_target t ON t.id=c.target_id
            WHERE p.ns=14 AND (t.ns=14 OR c.title IS NOT NULL)
            AND NOT EXISTS (SELECT 1 FROM dump_page h JOIN dump_prop hp ON hp.page_id=h.id
                WHERE h.ns=14 AND hp.name='hiddencat' AND h.title=COALESCE(t.title,c.title))'''):
            parents.setdefault(child, []).append(parent)
        eligible = (_stage_comparison_items(source, comparison_connection, language, identifiers)
                    if comparison_connection is not None else None)
        graph = GraphAccumulator(titles, redirects, eligible)
        del eligible
        categories = CategoryBatchClassifier(expected, parents, native_rules(rules))
        del titles, redirects, parents
        position = 0
        for batch in _stage_page_batches(source, 512, include_links=True):
            ids = [page_id for page_id, _, _, _ in batch]
            if ids != identifiers[position:position + len(ids)]:
                raise ValueError('Staging con ordine o identificativi delle pagine diversi dal dump')
            links = [targets for _, _, targets, _ in batch]
            if comparison_connection is None:
                graph.add_batch(links)
            else:
                edges = graph.add_batch_edges(links)
                with comparison_connection:
                    comparison_connection.executemany('INSERT INTO edges VALUES(?,?,?)',
                        ((language, identifiers[target], identifiers[origin])
                         for target, origin in edges))
            categories.add_support_batch([labels for _, _, _, labels in batch])
            position += len(batch)
            if position % 100000 < len(batch):
                print(f'Analisi SQL {language}: {position:,}/{expected:,} voci e collegamenti', flush=True)
        incoming, outgoing, unresolved = graph.finish()
        del graph
        if position != expected:
            raise ValueError('Analisi SQL non completa')
        print(f'Grafo SQL {language} completo; preparazione della cronologia e degli export', flush=True)

        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='orphanwiki-analyze-', dir=output.parent) as temporary:
            temporary = Path(temporary)
            history_db = sqlite3.connect(temporary / 'history.sqlite')
            try:
                history_db.execute('PRAGMA cache_size=-8192')
                history_db.execute('PRAGMA temp_store=FILE')
                raw_checksum = _hash(raw_path, 'sha256')
                history_meta = (_history_stage(history_path, history_db, graph_meta, expected)
                                if history_path else {'status': 'not_collected', 'matched_pages': 0})
                topic_meta = (_topic_stage(topic_path, history_db, graph_meta, expected, raw_checksum)
                              if topic_path else None)
                creator_meta = initialize_creator_rows([])

                class ObservationRows:
                    def __len__(self):
                        return expected

                    def __iter__(self):
                        history_cursor = iter(history_db.execute('SELECT id,data FROM history ORDER BY id')) if history_path else iter(())
                        next_history = next(history_cursor, None)
                        topic_cursor = iter(history_db.execute('SELECT id,data FROM topics ORDER BY id')) if topic_path else iter(())
                        next_topic = next(topic_cursor, None)
                        index = 0
                        for batch in _stage_page_batches(source, 512, include_links=False):
                            formatted = format_choices(
                                [{'page_id': pid, 'categories': labels} for pid, _, _, labels in batch],
                                categories.classify_batch([(pid, labels) for pid, _, _, labels in batch]))
                            rows, by_id = [], {}
                            for pid, page, _, labels in batch:
                                if pid != identifiers[index]:
                                    raise ValueError('Ordine dello staging cambiato durante l’export')
                                page['categories'] = labels
                                rows.append(observation_row(page, formatted[pid], incoming[index], outgoing[index]))
                                history_records, next_history = _next_for_page(history_cursor, next_history, pid)
                                if len(history_records) > 1:
                                    raise ValueError('Cronologia con ID duplicato')
                                if history_records:
                                    by_id[pid] = json.loads(history_records[0])
                                topic_records, next_topic = _next_for_page(topic_cursor, next_topic, pid)
                                if topic_path:
                                    if len(topic_records) != 1:
                                        raise ValueError('Temi mancanti o duplicati per la voce')
                                    prediction = json.loads(topic_records[0])
                                    if prediction.get('revision_id') != page.get('revision_id'):
                                        raise ValueError('Temi riferiti a una revisione diversa')
                                    rows[-1].update(topic_labels=prediction['topic_labels'],
                                                    topic_status=prediction['topic_status'],
                                                    topic_input_count=prediction['topic_input_count'])
                                index += 1
                            initialize_creator_rows(rows)
                            _join_history_rows(rows, by_id,
                                               missing_status='missing_page' if history_path else 'not_collected')
                            ages = article_ages(graph_meta['finished_at'], [row['created_at'] for row in rows])
                            history_ages = article_ages(graph_meta['finished_at'],
                                                       [row['history_first_revision_at'] for row in rows])
                            for row, (age, after), (history_age, _) in zip(rows, ages, history_ages):
                                row['age_days'] = age
                                row['history_first_revision_age_days'] = history_age
                                if after:
                                    row['creation_date_status'] = 'after_graph_collection'
                                yield row
                        if index != expected or next_history is not None or next_topic is not None:
                            raise ValueError('Esportazione, cronologia o temi incompleti')

                metadata = {**graph_meta, 'creators': creator_meta, 'history': history_meta,
                            'unresolved_target_occurrences': unresolved,
                            'category_rule': 'structural_specificity_v1', 'category_rules': rules,
                            'engine': 'rust', 'age_rule': 'unavailable_no_approved_creation_event',
                            'historical_age_rule': ('mediawiki_history_first_observed_revision_v1'
                                                    if history_path else 'not_collected'),
                            'length_unit': 'UTF-8 wikitext bytes',
                            'input_sha256': raw_checksum,
                            'input_checksum_format': 'raw_file_bytes_v1',
                            'analysis_source': 'verified_import_stage'}
                if topic_meta:
                    metadata['topics'] = topic_meta
                    metadata['article_features'] = {
                        **metadata.get('article_features', {}), 'topics': 'local_model_predictions'}
                result = {'metadata': metadata, 'pages': ObservationRows()}
                if comparison_connection is not None:
                    position = 0
                    batch = []
                    for row in result['pages']:
                        batch.append((language, row['page_id'], position,
                                      row.get('wikibase_item') if isinstance(row.get('wikibase_item'), str) else None,
                                      row['title'], row['in_degree'], row['out_degree'],
                                      json.dumps(row, ensure_ascii=False)))
                        position += 1
                        if len(batch) == 512:
                            with comparison_connection:
                                comparison_connection.executemany('INSERT INTO pages VALUES(?,?,?,?,?,?,?,?)', batch)
                            batch.clear()
                            if position % 100000 < 512:
                                print(f'Osservazioni {language}: {position:,}/{expected:,}', flush=True)
                    if batch:
                        with comparison_connection:
                            comparison_connection.executemany('INSERT INTO pages VALUES(?,?,?,?,?,?,?,?)', batch)
                    if position != expected:
                        raise ValueError('Osservazioni del confronto incomplete')
                    with comparison_connection:
                        comparison_connection.execute('INSERT INTO wikis VALUES(?,?,?)',
                            (language, json.dumps(metadata, ensure_ascii=False), expected))
                    return expected
                prepared = temporary / output.name
                transports = export_result(result, prepared, update_manifest=False, announce=False)
                existing = output.parent / 'manifest.json'
                manifest_entries = json.loads(existing.read_text()) if existing.exists() else []
                manifest_entries = [entry for entry in manifest_entries if entry.get('file') != output.name]
                manifest_entries.append(manifest_entry(result, output.name, transports))
                write_json(temporary / 'manifest.json', manifest_entries)
                _publish_prepared(temporary, output.parent, excluded=(temporary / 'history.sqlite',))
                from .__main__ import publish_available_aggregate
                publish_available_aggregate(output.parent)
                print(f'Esportate {expected:,} voci: {output}', flush=True)
            finally:
                history_db.close()
    finally:
        source.close()
