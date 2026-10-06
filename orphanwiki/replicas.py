"""Bulk reader for PAWS / Wikimedia Cloud. Credentials never enter exported data."""
import json
import os
import re
from collections.abc import MutableMapping
from datetime import datetime, timezone
from pathlib import Path

import pymysql

from .core import replica_database


class DiskPages(MutableMapping):
    """Local staging, O(largest exported page) client memory; no remote writes.

    Links/categories are stored separately to avoid rewriting an article for
    every edge. All populations and parsed replica relationships are retained.
    """
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
        value = {k: v for k, v in value.items() if k not in ('links', 'categories')}
        self.connection.execute('INSERT OR REPLACE INTO pages VALUES(?,?)', (key, json.dumps(value, ensure_ascii=False)))

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
        for pid, encoded in self.connection.execute('SELECT id,data FROM pages ORDER BY id'):
            page = json.loads(encoded)
            for kind in ('links', 'categories'):
                page[kind] = [r[0] for r in self.connection.execute(f'SELECT target FROM {kind} WHERE id=? ORDER BY rowid', (pid,))]
            yield page


def decode(value):
    return value.decode('utf-8') if isinstance(value, bytes) else value


def replica_options(language, credentials=None, database=None):
    database = replica_database(language, database)
    options = {'host': f'{database}.analytics.db.svc.wikimedia.cloud', 'database': database + '_p',
               'charset': 'utf8mb4', 'connect_timeout': 30, 'read_timeout': 10800,
               'cursorclass': pymysql.cursors.SSCursor, 'autocommit': True}
    if credentials:
        config = Path(credentials).expanduser()
        if not config.is_file():
            raise RuntimeError(f'File credenziali non trovato: {config}')
        options['read_default_file'] = str(config)
    else:
        config = next((path for path in (Path.home() / 'replica.my.cnf', Path.home() / '.my.cnf')
                       if path.is_file()), None)
        if config is not None:
            options['read_default_file'] = str(config)
        else:
            for user_key, password_key in (
                ('MYSQL_USERNAME', 'MYSQL_PASSWORD'),
                ('TOOL_REPLICA_USER', 'TOOL_REPLICA_PASSWORD'),
            ):
                if os.environ.get(user_key) and os.environ.get(password_key):
                    options.update(user=os.environ[user_key], password=os.environ[password_key])
                    break
            else:
                raise RuntimeError(
                    'Credenziali delle Wiki Replicas non trovate. Cercati ~/replica.my.cnf, '
                    '~/.my.cnf, MYSQL_USERNAME/MYSQL_PASSWORD e '
                    'TOOL_REPLICA_USER/TOOL_REPLICA_PASSWORD. '
                    'Specificare --credentials con un file esistente; non usare la password Wikipedia.'
                )
    return database, options


def collect_replicas(language, credentials=None, database=None, page_store=None):
    database, options = replica_options(language, credentials, database) if database else replica_options(language, credentials)
    started = datetime.now(timezone.utc).isoformat()
    conn = pymysql.connect(**options)
    try:
        def rows(sql):
            with conn.cursor() as cursor:
                cursor.execute(sql)
                for row in cursor:
                    yield tuple(decode(x) for x in row)
        def columns(table):
            return {row[0] for row in rows('SHOW COLUMNS FROM ' + table)}
        pages = {} if page_store is None else page_store
        for pid, title, length, revision in rows('SELECT page_id,page_title,page_len,page_latest FROM page WHERE page_namespace=0 AND page_is_redirect=0'):
            title = title.replace('_', ' ')
            pages[pid] = {'page_id': pid, 'title': title, 'length_bytes': length, 'revision_id': revision,
                          'links': [], 'categories': [], 'created_at': None}
        if not pages:
            raise ValueError('Nessuna voce trovata')
        for page in pages.values():
            page['wikibase_item'] = None
            pages[page['page_id']] = page
        for pid, item in rows("SELECT pp_page,pp_value FROM page_props JOIN page ON page_id=pp_page WHERE pp_propname='wikibase_item' AND page_namespace=0 AND page_is_redirect=0"):
            if pid in pages and re.fullmatch(r'Q[1-9][0-9]*', item or ''):
                page = pages[pid]
                page['wikibase_item'] = item
                pages[pid] = page
        print(f'Voci: {len(pages)}; estrazione massiva collegamenti', flush=True)
        if 'pl_target_id' in columns('pagelinks'):
            sql = 'SELECT pl_from,lt_title FROM pagelinks JOIN linktarget ON pl_target_id=lt_id WHERE pl_from_namespace=0 AND lt_namespace=0'
        else:
            sql = 'SELECT pl_from,pl_title FROM pagelinks WHERE pl_from_namespace=0 AND pl_namespace=0'
        for pid, target in rows(sql):
            if pid in pages:
                if page_store is None:
                    pages[pid]['links'].append(target.replace('_', ' '))
                else:
                    pages.add('links', pid, target.replace('_', ' '))
        redirects = {}
        for source, target in rows("SELECT page_title,rd_title FROM redirect JOIN page ON page_id=rd_from WHERE page_namespace=0 AND rd_namespace=0 AND (rd_interwiki IS NULL OR rd_interwiki='')"):
            redirects[source.replace('_', ' ')] = target.replace('_', ' ')
        hidden = {title.replace('_', ' ') for (title,) in rows("SELECT page_title FROM page JOIN page_props ON page_id=pp_page WHERE page_namespace=14 AND pp_propname='hiddencat'")}
        category_titles = {pid: title.replace('_', ' ') for pid, title in rows('SELECT page_id,page_title FROM page WHERE page_namespace=14')}
        parents = {}
        if 'cl_target_id' in columns('categorylinks'):
            sql = 'SELECT cl_from,lt_title FROM categorylinks JOIN linktarget ON cl_target_id=lt_id WHERE lt_namespace=14'
        else:
            sql = 'SELECT cl_from,cl_to FROM categorylinks'
        for pid, category in rows(sql):
            category = category.replace('_', ' ')
            if category in hidden:
                continue
            if pid in pages:
                if page_store is None:
                    pages[pid]['categories'].append(category)
                else:
                    pages.add('categories', pid, category)
            elif pid in category_titles:
                parents.setdefault(category_titles[pid], []).append(category)
        print('Categorie acquisite; verifica della creazione delle pagine', flush=True)
        collect_creation_dates(rows, pages)
        for page in pages.values():
            page['created_at'] = page['page_created_at']
            pages[page['page_id']] = page
        return {'metadata': {'language': language, 'database': database, 'source': 'Wiki Replicas / ' + database, 'complete': True, 'demo': False,
                             'started_at': started, 'finished_at': datetime.now(timezone.utc).isoformat(),
                             'temporal_consistency': 'live_collection_interval',
                             'creation_date_rule': 'creation_event_v1',
                             'article_features': {
                                 'age': 'creation_event_v1', 'length': 'page.page_len',
                                 'topic_inputs': 'pagelinks + page_props.wikibase_item',
                                 'topics': 'unavailable_no_local_model',
                                 'quality': 'unavailable_no_wikitext_in_replicas',
                                 'pageviews': 'unavailable_not_in_wiki_replicas',
                                 'biographical_gender': 'unavailable_not_in_local_replica',
                                 'bot_at_creation': 'unavailable_no_complete_historical_membership',
                             },
                             'item_mapping': {'source': 'page_props.wikibase_item', 'complete': True}},
                'pages': list(pages.values()) if page_store is None else [], 'redirects': redirects, 'category_parents': parents}
    finally:
        conn.close()


def iso_timestamp(value):
    """Public replica dates may contain NUL/zero sentinels or invalid calendars.

    Accept only a complete ASCII MediaWiki timestamp; never repair, trim or
    replace an unknown date with zero/the first usable later revision.
    """
    if isinstance(value, bytes):
        try:
            value = value.decode('ascii')
        except UnicodeDecodeError:
            return None
    if not isinstance(value, str) or not re.fullmatch(r'[0-9]{14}', value):
        return None
    try:
        return datetime.strptime(value, '%Y%m%d%H%M%S').replace(tzinfo=timezone.utc).isoformat()
    except ValueError:
        return None


def collect_sizes(language, credentials=None, database=None):
    """Read the full census size and stored wikitext bytes, not JSON/RAM estimates."""
    database, options = replica_options(language, credentials, database) if database else replica_options(language, credentials)
    started = datetime.now(timezone.utc).isoformat()
    conn = pymysql.connect(**options)
    try:
        with conn.cursor() as cursor:
            cursor.execute('SELECT COUNT(*),SUM(page_len) FROM page WHERE page_namespace=0 AND page_is_redirect=0')
            count, length = next(iter(cursor))
        return {'language': language, 'database': database, 'source': 'Wiki Replicas / ' + database,
                'started_at': started, 'finished_at': datetime.now(timezone.utc).isoformat(),
                'census_pages': int(count), 'wikitext_bytes': int(length or 0)}
    finally:
        conn.close()


def collect_creation_dates(rows, pages, page_ids=None):
    """Creation events, never an unconditional minimum revision timestamp.

    Prefer a unique public create/create log tied to the page ID. Otherwise use
    a unique initial revision (parent zero), excluding observed imports/merges.
    Older or incomplete histories may remain unknown. Two bulk queries, O(P)
    client memory; server aggregation scans the surviving revision history.
    """
    for pid in pages:
        page = pages[pid]
        page.update(page_created_at=None, first_public_revision_at=None,
                    creation_date_source=None, creation_date_status='missing_revision')
        pages[pid] = page
    restricted = ''
    if page_ids is not None:
        if not page_ids or any(type(pid) is not int or pid <= 0 for pid in page_ids):
            raise ValueError('ID di pagina non validi')
        restricted = ' AND page_id IN (' + ','.join(map(str, page_ids)) + ')'
    sql = '''SELECT rev_page,MIN(rev_timestamp),
        MIN(CASE WHEN rev_parent_id=0 THEN rev_timestamp END),
        SUM(CASE WHEN rev_parent_id=0 THEN 1 ELSE 0 END)
        FROM revision JOIN page ON page_id=rev_page
        WHERE page_namespace=0 AND page_is_redirect=0''' + restricted + ' GROUP BY rev_page'
    for pid, first, initial, roots in rows(sql):
        if pid not in pages:
            continue
        page = pages[pid]
        first_date, initial_date = iso_timestamp(first), iso_timestamp(initial)
        page['first_public_revision_at'] = first_date
        page['creation_date_status'] = 'creation_revision_unavailable'
        if roots > 1:
            page['creation_date_status'] = 'ambiguous_creation_revisions'
        elif first_date is None or (roots == 1 and initial_date is None):
            page['creation_date_status'] = 'invalid_revision_timestamp'
        elif roots == 1 and initial_date == first_date:
            page.update(page_created_at=initial_date,
                        creation_date_source='initial_revision', creation_date_status='observed')
        elif roots == 1:
            page['creation_date_status'] = 'inconsistent_revision_history'
        pages[pid] = page
    sql = '''SELECT log_page,
        MIN(CASE WHEN log_type='create' AND log_action='create' THEN log_timestamp END),
        SUM(CASE WHEN log_type='create' AND log_action='create' THEN 1 ELSE 0 END),
        MAX(CASE WHEN log_type IN ('import','merge') THEN 1 ELSE 0 END)
        FROM logging WHERE log_type IN ('create','import','merge')
        AND log_page>0 AND (log_deleted & 1)=0'''
    if page_ids is not None:
        sql += ' AND log_page IN (' + ','.join(map(str, page_ids)) + ')'
    sql += ' GROUP BY log_page'
    for pid, created, count, imported in rows(sql):
        if pid not in pages:
            continue
        page = pages[pid]
        if count == 1:
            created_date = iso_timestamp(created)
            page.update(page_created_at=created_date,
                        creation_date_source='creation_log' if created_date else None,
                        creation_date_status='observed' if created_date else 'invalid_creation_log_timestamp')
        elif count > 1 or imported:
            page.update(page_created_at=None, creation_date_source=None,
                        creation_date_status='ambiguous_creation_logs' if count > 1 else 'imported_or_merged_history')
        pages[pid] = page


def collect_creators(language, credentials=None, database=None, stage=None, batch_size=1000):
    """Supplementary PAWS export. Never downloads the graph again or exports names/IPs."""
    database, options = replica_options(language, credentials, database) if database else replica_options(language, credentials)
    started = datetime.now(timezone.utc).isoformat()
    conn = pymysql.connect(**options)
    try:
        def rows(sql, params=()):
            with conn.cursor() as cursor:
                cursor.execute(sql, params or None)
                for row in cursor:
                    yield tuple(decode(x) for x in row)
        def columns(table):
            return {r[0] for r in rows('SHOW COLUMNS FROM ' + table)}
        if stage is None:
            records = creator_records(rows, columns, database, language)
        else:
            records = disk_creator_records(rows, columns, database, language, stage, batch_size)
        print('Esperienza acquisita; verifica delle date di creazione dei creatori', flush=True)
        if stage is None:
            collect_creation_dates(rows, {r['page_id']: r for r in records})
        return {'metadata': {
            'language': language, 'database': database, 'source': 'Wiki Replicas / ' + database,
            'started_at': started, 'finished_at': datetime.now(timezone.utc).isoformat(),
            'complete': True, 'languages_scanned': [language],
            'creation_date_rule': 'creation_event_v1',
            'creator_definition': 'author of earliest public revision with parent_id=0; imported histories excluded',
            'experience_scope': 'public revisions of surviving pages; namespaces at collection, strictly earlier timestamps',
            'creation_counts_scope': 'surviving nonredirect main-namespace pages at creator collection; not lifetime totals',
            'account_registration_scope': 'local wiki registration, not global account creation',
            'account_type_scope': 'at collection; bot status at creation unavailable',
            'origin_scope': 'creation revision tags and public import/move logs; no tag is not proof of original writing',
            'orphan_30d_status': 'unavailable_no_historical_link_graph',
        }, 'pages': records}
    finally:
        conn.close()


def initial_creator_records(rows, columns, database):
    """Yield the same conservative attribution in both collection modes."""
    temp_column = 'u.user_is_temp' if 'user_is_temp' in columns('user') else 'NULL'
    sql = f'''WITH first_revisions AS (
        SELECT rev_page, rev_id, rev_timestamp, rev_actor, rev_parent_id, rev_deleted,
               ROW_NUMBER() OVER (PARTITION BY rev_page ORDER BY rev_timestamp, rev_id) AS position
        FROM revision JOIN page ON page_id=rev_page
        WHERE page_namespace=0 AND page_is_redirect=0
    )
    SELECT r.rev_page, r.rev_id, r.rev_timestamp, r.rev_parent_id, r.rev_deleted,
           a.actor_id, a.actor_user, u.user_registration, {temp_column},
           EXISTS(SELECT 1 FROM user_groups WHERE ug_user=u.user_id AND ug_group='bot'
                  AND (ug_expiry IS NULL OR ug_expiry>DATE_FORMAT(UTC_TIMESTAMP(), '%Y%m%d%H%i%s')))
    FROM first_revisions r
    LEFT JOIN actor_revision a ON a.actor_id=r.rev_actor AND (r.rev_deleted & 4)=0
    LEFT JOIN user u ON u.user_id=a.actor_user
    WHERE r.position=1'''
    for pid, rid, timestamp, parent, deleted, actor, user, registration, temporary, bot in rows(sql):
        created = iso_timestamp(timestamp)
        attribution = 'observed_creation_revision'
        if created is None:
            attribution = 'invalid_revision_timestamp'
        elif parent != 0:
            attribution = 'incomplete_history'
        elif deleted is None or deleted & 4 or actor is None:
            attribution = 'hidden_creator'
        account_type = 'unknown'
        if attribution == 'observed_creation_revision':
            account_type = 'anonymous_legacy' if not user else 'temporary' if temporary == 1 else 'bot' if bot else 'registered' if temporary == 0 else 'unknown'
        record = {
            'page_id': pid, 'first_revision_id': rid, 'article_created_at': created,
            'creator_key': f'{database}:actor:{actor}' if attribution == 'observed_creation_revision' and user else None,
            'creator_attribution': attribution, 'creator_account_type': account_type,
            'creator_is_bot_now': bool(bot) if user and attribution == 'observed_creation_revision' else None,
            'creator_account_type_at_creation': None,
            'creator_registered_at': iso_timestamp(registration) if user and attribution == 'observed_creation_revision' else None,
            'creator_tenure_days': None, 'creator_tenure_status': 'unknown_registration',
            'creator_prior_edits_main': None, 'creator_prior_edits_other': None,
            'creator_prior_articles': None, 'creator_articles_created_total': None,
            'creator_languages_created': None, 'creator_languages_before': None,
            'origin': 'unknown', 'creation_tags': [], 'import_log_observed': False,
            'move_log_observed': False,
        }
        yield record


def creator_records(rows, columns, database, language=None, initial=None, creation_counts=None, include_experience=True, logs_collected=False):
    """Bulk extraction with bounded SQL batches and a streaming experience sweep."""
    from bisect import bisect_left
    from collections import defaultdict

    records, actor_pages = {}, defaultdict(list)
    for record in initial_creator_records(rows, columns, database) if initial is None else initial:
        records[record['page_id']] = record
        if record['creator_key']:
            actor = int(record['creator_key'].rsplit(':', 1)[1])
            actor_pages[actor].append(record)
    print(f'Prime revisioni: {len(records)}; acquisizione dei tag e dei log', flush=True)
    # These are explicit evidence, not a claim that all translations/imports are tagged.
    page_filter = '' if initial is None else ' AND rev_page IN (' + ','.join(map(str, records)) + ')'
    for pid, revision, tag in rows('''SELECT rev_page, ct_rev_id, ctd_name
            FROM change_tag JOIN change_tag_def ON ct_tag_id=ctd_id
            JOIN revision ON rev_id=ct_rev_id JOIN page ON page_id=rev_page
            WHERE page_namespace=0 AND page_is_redirect=0 AND rev_parent_id=0''' + page_filter):
        if pid in records and revision == records[pid]['first_revision_id']:
            records[pid]['creation_tags'].append(tag)
    log_filter = '' if initial is None else ' AND log_page IN (' + ','.join(map(str, records)) + ')'
    logs = [] if logs_collected else rows("SELECT DISTINCT log_page,log_type FROM logging WHERE log_type IN ('import','move') AND log_page IS NOT NULL" + log_filter)
    for pid, kind in logs:
        if pid in records:
            records[pid]['import_log_observed' if kind == 'import' else 'move_log_observed'] = True
    for record in records.values():
        tags = set(record['creation_tags'])
        if record['import_log_observed']:
            record['origin'] = 'imported_history_observed'
            record['creator_attribution'] = 'imported_history_uncertain'
            record['creator_key'] = None
            record['creator_registered_at'] = None
            record['creator_account_type'] = 'unknown'
            record['creator_is_bot_now'] = None
        elif record['creator_attribution'] != 'observed_creation_revision':
            record['origin'] = 'unknown'
        elif tags & {'contenttranslation', 'contenttranslation-v2', 'sectiontranslation'}:
            record['origin'] = 'translation_tagged'
        else:
            record['origin'] = 'new_page_unclassified'
    actor_pages = {actor: [r for r in group if r['creator_key']] for actor, group in actor_pages.items()}
    actor_pages = {actor: group for actor, group in actor_pages.items() if group}
    for group in actor_pages.values():
        dates = sorted(r['article_created_at'] for r in group)
        for record in group:
            total, prior = creation_counts(record['page_id']) if creation_counts else (len(group), bisect_left(dates, record['article_created_at']))
            record['creator_articles_created_total'] = total
            record['creator_prior_articles'] = prior
            language = language or database[:-4].replace('_', '-')
            record['creator_languages_created'] = [language]
            record['creator_languages_before'] = [language] if record['creator_prior_articles'] else []
            registration = record['creator_registered_at']
            if registration:
                days = (datetime.fromisoformat(record['article_created_at']) - datetime.fromisoformat(registration)).total_seconds() / 86400
                record['creator_tenure_status'] = 'observed' if days >= 0 else 'registration_after_revision'
                record['creator_tenure_days'] = days if days >= 0 else None
    actors = sorted(actor_pages)
    if not include_experience:
        return list(records.values())
    for offset in range(0, len(actors), 250):
        batch = actors[offset:offset + 250]
        placeholders = ','.join(['%s'] * len(batch))
        cutoff = max(r['article_created_at'] for actor in batch for r in actor_pages[actor])
        cutoff = datetime.fromisoformat(cutoff).strftime('%Y%m%d%H%M%S')
        events = rows(f'''SELECT rev_actor,rev_timestamp,page_namespace FROM revision_userindex
            JOIN page ON page_id=rev_page
            WHERE rev_actor IN ({placeholders}) AND (rev_deleted & 4)=0 AND rev_timestamp < %s
            ORDER BY rev_actor,rev_timestamp,rev_id''', (*batch, cutoff))
        experience_before({actor: actor_pages[actor] for actor in batch}, events)
        print(f'Esperienza precedente: {min(offset + 250, len(actors))}/{len(actors)} autori', flush=True)
    return list(records.values())


def disk_creator_records(rows, columns, database, language, stage, batch_size):
    """Stage all creators; batch enrichment but count experience over full histories.

    Each actor history is streamed once per actor batch, regardless of article
    count. SQLite windows preserve strictly-earlier creation counts across all
    enrichment batches, including prolific accounts and same-second creations.
    """
    if type(batch_size) is not int or not 1 <= batch_size <= 10000:
        raise ValueError('Dimensione blocco non valida: da 1 a 10000')
    stage.executescript('''CREATE TABLE creators(id INTEGER PRIMARY KEY, actor TEXT, created TEXT,
        data TEXT, edits_main INTEGER, edits_other INTEGER);
        CREATE INDEX creators_actor ON creators(actor,created,id);''')
    for record in initial_creator_records(rows, columns, database):
        stage.execute('INSERT INTO creators(id,actor,created,data) VALUES(?,?,?,?)',
                      (record['page_id'], record['creator_key'], record['article_created_at'], json.dumps(record)))
    for pid, kind in rows("SELECT DISTINCT log_page,log_type FROM logging WHERE log_type IN ('import','move') AND log_page IS NOT NULL"):
        found = stage.execute('SELECT data FROM creators WHERE id=?', (pid,)).fetchone()
        if found is not None:
            record = json.loads(found[0])
            record['import_log_observed' if kind == 'import' else 'move_log_observed'] = True
            stage.execute('UPDATE creators SET data=?,actor=? WHERE id=?',
                          (json.dumps(record), None if record['import_log_observed'] else record['creator_key'], pid))
    stage.executescript('''CREATE TABLE creation_counts AS SELECT id,
        COUNT(*) OVER (PARTITION BY actor) AS total,
        RANK() OVER (PARTITION BY actor ORDER BY created)-1 AS prior
        FROM creators WHERE actor IS NOT NULL;
        CREATE UNIQUE INDEX creation_counts_id ON creation_counts(id);''')
    def counts(pid):
        found = stage.execute('SELECT total,prior FROM creation_counts WHERE id=?', (pid,)).fetchone()
        if found is None:
            raise ValueError('Conteggio globale del creatore non disponibile')
        return found
    last_id, processed = 0, 0
    while True:
        initial = [json.loads(r[0]) for r in stage.execute('SELECT data FROM creators WHERE id>? ORDER BY id LIMIT ?', (last_id, batch_size))]
        if not initial:
            break
        last_id = initial[-1]['page_id']
        records = creator_records(rows, columns, database, language, initial, counts,
                                  include_experience=False, logs_collected=True)
        collect_creation_dates(rows, {r['page_id']: r for r in records}, [r['page_id'] for r in records])
        for record in records:
            stage.execute('UPDATE creators SET data=?,actor=? WHERE id=?', (json.dumps(record), record['creator_key'], record['page_id']))
        processed += len(records)
        stage.commit()
        print(f'Creatori: {processed} voci elaborate a blocchi', flush=True)
    actor_cursor = stage.execute('SELECT DISTINCT actor FROM creators WHERE actor IS NOT NULL ORDER BY actor')
    while actors := actor_cursor.fetchmany(250):
        keys = [r[0] for r in actors]
        actor_ids = [int(key.rsplit(':', 1)[1]) for key in keys]
        pending, iterators, counters = {}, {}, {}
        cutoff = ''
        for actor, key in zip(actor_ids, keys):
            latest = stage.execute('SELECT MAX(created) FROM creators WHERE actor=?', (key,)).fetchone()[0]
            cutoff = max(cutoff, latest)
            iterators[actor] = iter(stage.execute('SELECT id,created FROM creators WHERE actor=? ORDER BY created,id', (key,)))
            pending[actor] = next(iterators[actor], None)
            counters[actor] = [0, 0]
        placeholders = ','.join(['%s'] * len(actor_ids))
        cutoff = datetime.fromisoformat(cutoff).strftime('%Y%m%d%H%M%S')
        events = rows(f'''SELECT rev_actor,rev_timestamp,page_namespace FROM revision_userindex
            JOIN page ON page_id=rev_page WHERE rev_actor IN ({placeholders})
            AND (rev_deleted & 4)=0 AND rev_timestamp < %s
            ORDER BY rev_actor,rev_timestamp,rev_id''', (*actor_ids, cutoff))
        invalid_actors = set()
        for actor, timestamp, namespace in events:
            if actor not in counters:
                continue
            date = iso_timestamp(timestamp)
            if date is None:
                invalid_actors.add(actor)
                continue
            while pending[actor] is not None and pending[actor][1] <= date:
                stage.execute('UPDATE creators SET edits_main=?,edits_other=? WHERE id=?', (*counters[actor], pending[actor][0]))
                pending[actor] = next(iterators[actor], None)
            counters[actor][0 if namespace == 0 else 1] += 1
        for actor in actor_ids:
            while pending[actor] is not None:
                stage.execute('UPDATE creators SET edits_main=?,edits_other=? WHERE id=?', (*counters[actor], pending[actor][0]))
                pending[actor] = next(iterators[actor], None)
        for actor, key in zip(actor_ids, keys):
            if actor in invalid_actors:
                stage.execute('UPDATE creators SET edits_main=NULL,edits_other=NULL WHERE actor=?', (key,))
        stage.commit()
        print(f'Esperienza precedente: acquisita per {len(actor_ids)} autori', flush=True)
    # Return a lazy iterator only after all SQL has succeeded, so the interval
    # and complete flag describe a finished collection, not an in-flight stream.
    def exported():
        for encoded, main, other in stage.execute('SELECT data,edits_main,edits_other FROM creators ORDER BY id'):
            record = json.loads(encoded)
            if record['creator_key']:
                record['creator_prior_edits_main'], record['creator_prior_edits_other'] = main, other
                if main is None or other is None:
                    record['creator_experience_status'] = 'invalid_revision_timestamp'
            yield record
    return exported()


def experience_before(actor_pages, events):
    """Sweep ordered events once; same-second events are excluded, not tie-broken."""
    ordered = {actor: sorted(group, key=lambda r: r['article_created_at']) for actor, group in actor_pages.items()}
    positions = {actor: 0 for actor in ordered}
    counts = {actor: [0, 0] for actor in ordered}
    invalid_actors = set()
    for actor, timestamp, namespace in events:
        if actor not in ordered:
            continue
        time = iso_timestamp(timestamp)
        if time is None:
            invalid_actors.add(actor)
            continue
        group, position = ordered[actor], positions[actor]
        while position < len(group) and group[position]['article_created_at'] <= time:
            group[position]['creator_prior_edits_main'], group[position]['creator_prior_edits_other'] = counts[actor]
            position += 1
        positions[actor] = position
        counts[actor][0 if namespace == 0 else 1] += 1
    for actor, group in ordered.items():
        for record in group[positions[actor]:]:
            record['creator_prior_edits_main'], record['creator_prior_edits_other'] = counts[actor]
        if actor in invalid_actors:
            for record in group:
                record.update(creator_prior_edits_main=None, creator_prior_edits_other=None,
                              creator_experience_status='invalid_revision_timestamp')
