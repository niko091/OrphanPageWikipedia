import bisect
import hashlib
import itertools
import json
import multiprocessing
import os
import re
import sqlite3
import tempfile
import time
import uuid
from collections.abc import Mapping
from contextlib import closing, contextmanager
from contextvars import ContextVar
from datetime import datetime, timezone
from functools import wraps
from pathlib import Path

HISTOGRAM_RULE = 'shared_p2_p98_tails_v1'
MEASURES_RULE = 'creation_quality_creators_v3'
QUALITY_REVISION_RULE = 'selected_official_xml_revision_v1'
ARTICLE_PREVIEW_LIMIT = 25
TOPIC_LOOKUP_SHARD_SIZE = 1024
TRANSPORT_RULE = 'dashboard_transports_v3'
COMPARISON_CACHE_RULE = 'verified_graph_and_ordered_pairs_v1'
SUMMARY_FIELDS = (
    'page_id', 'title', 'orphan', 'category', 'topic_status', 'topic_labels',
    'page_created_at', 'age_days', 'length_bytes', 'quality_score', 'quality_status',
    'out_degree', 'origin', 'creator_key', 'creator_account_type',
    'creator_articles_created_total', 'creator_languages_created', 'creator_is_bot_now',
    'creator_prior_edits_main', 'creator_prior_edits_other', 'creator_prior_articles',
    'creator_tenure_days', 'creator_orphans_current', 'history_creator_key',
    'creator_prior_edits_main_history', 'creator_prior_edits_other_history',
    'creator_continuity_months_before_creation', 'creator_groups_at_creation_history',
)
_PHASE_CONTEXT = ContextVar('operational_phase', default=None)
_LOCAL_RUN_ID = uuid.uuid4().hex


@contextmanager
def operational_span(name, **details):
    """Optional operational JSONL, never part of scientific/checkpoint identity.

    Durations are inclusive. Reconstruct exclusive time by subtracting the
    union of child intervals, never by adding children to their parent.
    Start records allow the runner to close spans after abrupt worker death.
    Missing counters stay null; rows/bytes describe only the named operation.
    """
    path = os.environ.get('ORPHANWIKI_SPANS_FILE')
    if not path:
        yield {}
        return
    parent = _PHASE_CONTEXT.get()
    record = {'run_id': os.environ.get('ORPHANWIKI_RUN_ID') or (parent['run_id'] if parent else _LOCAL_RUN_ID),
              'span_id': uuid.uuid4().hex,
              'parent_id': parent['span_id'] if parent else os.environ.get('ORPHANWIKI_PARENT_SPAN'),
              'pid': os.getpid(), 'name': name, 'rows': None, 'bytes': None, **details}
    def emit(event, **values):
        # Each process appends one complete small record, with no shared Python
        # buffering. Diagnostic I/O failures must not change collection/reuse.
        try:
            with open(path, 'ab', buffering=0) as stream:
                stream.write((json.dumps({**record, 'event': event,
                    'monotonic_ns': time.monotonic_ns(), **values}, allow_nan=False) + '\n').encode())
        except OSError:
            pass
    started = time.monotonic_ns()
    emit('span_start', started_ns=started)
    token = _PHASE_CONTEXT.set(record)
    status, error = 'completed', None
    try:
        yield record
    except BaseException as exception:
        status = 'interrupted' if isinstance(exception, (KeyboardInterrupt, InterruptedError, SystemExit)) else 'failed'
        error = type(exception).__name__
        raise
    finally:
        finished = time.monotonic_ns()
        emit('span_end', started_ns=started, finished_ns=finished,
             wall_seconds=(finished - started) / 1e9, status=status, error=error)
        _PHASE_CONTEXT.reset(token)


def operational_phase(name):
    """Instrument a regular function; streaming iterators use explicit spans."""
    def decorate(function):
        @wraps(function)
        def measured(*args, **kwargs):
            with operational_span(name):
                return function(*args, **kwargs)
        return measured
    return decorate


def phase_counts(**values):
    record = _PHASE_CONTEXT.get()
    if record is not None:
        record.update(values)


def operational_environment():
    environment = dict(os.environ)
    current = _PHASE_CONTEXT.get()
    if current is not None:
        environment['ORPHANWIKI_PARENT_SPAN'] = current['span_id']
        environment['ORPHANWIKI_RUN_ID'] = current['run_id']
    elif environment.get('ORPHANWIKI_SPANS_FILE') and not environment.get('ORPHANWIKI_RUN_ID'):
        environment['ORPHANWIKI_RUN_ID'] = _LOCAL_RUN_ID
    return environment


def _summary_page(record, language=None):
    page = {key: record[key] for key in SUMMARY_FIELDS if key in record}
    if language is not None:
        page['language'] = language
    return page


def prepared_summaries_match(summary_path, creators_path, metadata, sources, page_count=None):
    """Validate cached chart and creator transports against their source files."""
    try:
        summary = json.loads(Path(summary_path).read_text(encoding='utf-8'))
        creators = json.loads(Path(creators_path).read_text(encoding='utf-8'))
    except (OSError, ValueError, TypeError):
        return False
    if not isinstance(summary, dict) or not isinstance(creators, dict):
        return False
    summary_metadata = summary.get('metadata')
    if metadata.get('kind') == 'aggregate' and isinstance(summary_metadata, dict):
        metadata_matches = {key: value for key, value in summary_metadata.items()
                            if key not in ('creators', 'history')} == metadata
    else:
        metadata_matches = summary_metadata == metadata
    count = summary.get('census_page_count')
    return (summary.get('format') == 'dashboard_summary_v1'
            and summary.get('measures_rule') == metadata.get('measures_rule', MEASURES_RULE)
            and summary.get('histogram_rule') == HISTOGRAM_RULE
            and isinstance(summary.get('reports'), dict)
            and isinstance(summary['reports'].get('overview'), dict)
            and type(summary['reports']['overview'].get('orphans')) is int
            and isinstance(summary.get('article_preview'), list)
            and len(summary['article_preview']) == min(ARTICLE_PREVIEW_LIMIT,
                summary['reports']['overview']['orphans'])
            and all(isinstance(page, dict) and page.get('orphan') is True
                    and type(page.get('page_id')) is int and isinstance(page.get('title'), str)
                    for page in summary['article_preview'])
            and creators.get('format') == 'creator_index_v1'
            and metadata_matches and creators.get('metadata') == summary_metadata
            and summary.get('sources') == sources == creators.get('sources')
            and type(count) is int and count >= 0 and count == creators.get('census_page_count')
            and (page_count is None or count == page_count))


def wiki_database(language, database=None):
    """Resolve the configured database name for a Wikipedia edition."""
    if not isinstance(language, str) or not re.fullmatch(r'[a-z][a-z0-9-]{0,19}', language) or language == 'all':
        raise ValueError('Codice lingua non valido')
    database = language.replace('-', '_') + 'wiki' if database is None else database
    if not isinstance(database, str) or not re.fullmatch(r'[a-z][a-z0-9_]{0,40}wiki', database):
        raise ValueError('Nome database Wikipedia non valido (senza suffisso _p)')
    return database


def configured_languages(path='config/languages.json'):
    """Return every configured edition; this is the sole collection selection."""
    config = json.loads(Path(path).read_text(encoding='utf-8'))
    if not isinstance(config, dict) or set(config) - {'version', 'languages', 'databases'}:
        raise ValueError('Configurazione lingue non valida')
    languages = config.get('languages')
    databases = config.get('databases', {})
    if (config.get('version') != 1 or not isinstance(languages, list) or not languages
            or any(not isinstance(code, str) for code in languages)
            or len(languages) != len(set(languages)) or not isinstance(databases, dict)
            or set(databases) - set(languages)):
        raise ValueError('Configurazione lingue non valida')
    return [(code, wiki_database(code, databases.get(code))) for code in languages]


def _approved_graph_source(metadata, database):
    if metadata.get('demo'):
        return True
    return (metadata.get('source') == 'Wikimedia SQL dumps / ' + database
            and isinstance(metadata.get('sql_release'), str)
            and re.fullmatch(r'20[0-9]{6}', metadata['sql_release']) is not None
            and isinstance(metadata.get('dump_manifest_sha256'), str)
            and re.fullmatch(r'[0-9a-f]{64}', metadata['dump_manifest_sha256']) is not None)


@operational_phase('model')
def load_topic_model(path, configuration='config/topics.json'):
    """Load only an explicitly supplied, checksum-verified local model artifact."""
    spec = json.loads(Path(configuration).read_text())
    labels = spec.get('labels', [])
    checksum = spec.get('model_sha512')
    if (spec.get('version') != 1 or spec.get('threshold') != 0.5 or
            not isinstance(labels, list) or len(labels) != 64 or
            any(not isinstance(label, str) or '.' not in label for label in labels) or
            len(set(labels)) != 64 or not isinstance(checksum, str) or
            not re.fullmatch(r'[0-9a-f]{128}', checksum)):
        raise ValueError('Configurazione del modello dei temi non valida')
    digest = hashlib.sha512()
    with operational_span('hash.model', algorithm='sha512') as span:
        size = 0
        with Path(path).open('rb') as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b''):
                digest.update(block)
                size += len(block)
        span['bytes'] = size
        if digest.hexdigest() != spec['model_sha512']:
            raise ValueError('Checksum dei pesi del modello non corrispondente')
    try:
        import fasttext
    except ImportError as error:
        raise RuntimeError('Installare il supporto locale: pip install "fasttext==0.9.3" "numpy>=1.26,<3"') from error
    with operational_span('model.load'):
        model = fasttext.load_model(str(path))
    actual = {label.removeprefix('__label__').replace('_', ' ') for label in model.get_labels()}
    if actual != set(labels):
        raise ValueError('Il modello non contiene la tassonomia completa prevista')
    return model, spec


@operational_phase('topics')
def predict_topics(raw, model, spec, batch_size=256, deadline=None):
    """One native graph call, then bounded fastText batches over dump QIDs.

    O(V+E+R) graph work and O(V+E) extra adjacency/sets. The model is loaded
    once outside this function. No text, API enrichment or downloaded article
    predictions are used. Empty and entirely out-of-vocabulary inputs remain
    missing, rather than receiving prior-only predictions.
    """
    if (raw['metadata'].get('complete') is not True or
            raw['metadata'].get('item_mapping') != {'source': 'page_props.wikibase_item', 'complete': True}):
        raise ValueError('I temi richiedono un grafo completo con identificativi di pagina')
    database = wiki_database(raw['metadata']['language'], raw['metadata'].get('database'))
    if not _approved_graph_source(raw['metadata'], database):
        raise ValueError('Usare esclusivamente dump SQL verificati')
    if not isinstance(batch_size, int) or isinstance(batch_size, bool) or not 1 <= batch_size <= 10000:
        raise ValueError('Dimensione del blocco non valida')
    def check_deadline():
        if deadline is not None and time.monotonic() >= deadline:
            raise TimeoutError('Budget di tempo esaurito; nessun supplemento incompleto pubblicato')
    check_deadline()
    started = datetime.now(timezone.utc).isoformat()
    begin = time.monotonic()
    from ._native import graph_details
    pages = raw['pages']
    if len({p['page_id'] for p in pages}) != len(pages):
        raise ValueError('Identificativi di pagina duplicati')
    _, _, _, incoming = graph_details([p['title'] for p in pages], [p.get('links', []) for p in pages], raw.get('redirects', {}))
    graph_seconds = time.monotonic() - begin
    check_deadline()
    items = [p.get('wikibase_item') if isinstance(p.get('wikibase_item'), str) else None for p in pages]
    from ._native import unique_items, topic_inputs, threshold_topics
    unique, _ = unique_items(items)
    known = {item for item in unique if model.get_word_id(item) >= 0}
    inputs = topic_inputs(len(pages), unique, known, incoming)
    del incoming
    mapping_seconds = time.monotonic() - begin - graph_seconds
    records = [{'page_id': p['page_id'], 'revision_id': p.get('revision_id'),
                'topic_labels': None, 'topic_input_count': inputs[i][1],
                'topic_status': 'no_model_vocabulary' if inputs[i][2] else 'no_mapped_outlinks'}
               for i, p in enumerate(pages)]
    labels = set(spec['labels'])
    eligible = [i for i, (text, _, _) in enumerate(inputs) if text]
    prediction_started = time.monotonic()
    for offset in range(0, len(eligible), batch_size):
        check_deadline()
        indexes = eligible[offset:offset + batch_size]
        with operational_span('model.predict', rows=len(indexes)):
            predicted, probabilities = model.predict([inputs[i][0] for i in indexes], k=-1, threshold=0.0)
        if len(predicted) != len(indexes) or len(probabilities) != len(indexes):
            raise ValueError('Risposta del modello incompleta')
        selected = threshold_topics(labels, [list(row) for row in predicted],
                                    [[float(score) for score in row] for row in probabilities])
        for index, topic_labels in zip(indexes, selected):
            records[index].update(topic_labels=topic_labels, topic_status='predicted')
        if offset == 0 or offset + batch_size >= len(eligible):
            elapsed = time.monotonic() - prediction_started
            done = min(offset + batch_size, len(eligible))
            print(f"Temi {raw['metadata']['language']}: {done}/{len(eligible)} voci; stima inferenza totale {elapsed / done * len(eligible):.1f}s", flush=True)
    check_deadline()
    prediction_seconds = time.monotonic() - prediction_started
    topic_source = ('local_model_on_synthetic_outlinks' if raw['metadata'].get('demo')
                    else 'local_model_on_verified_SQL_dump_outlinks')
    return {'metadata': {'language': raw['metadata']['language'], 'source': topic_source,
                         'demo': raw['metadata'].get('demo', False), 'complete': True,
                         'started_at': started, 'finished_at': datetime.now(timezone.utc).isoformat(),
                         'input_sha256': json_sha256(raw), 'graph_finished_at': raw['metadata']['finished_at'],
                         'model': spec, 'top_level_rule': 'any_child_probability_gt_0.5',
                         'scope': 'model topic labels, not biographical gender observations',
                         'timing': {'native_graph_seconds': graph_seconds, 'mapping_seconds': mapping_seconds,
                                    'prediction_seconds': prediction_seconds},
                         'predicted_pages': len(eligible), 'missing_pages': len(pages) - len(eligible)}, 'pages': records}


def attach_topics(rows, raw, supplement):
    """Join a complete local inference run to its exact graph; never infer gender."""
    meta = supplement.get('metadata', {})
    if (meta.get('language') != raw['metadata']['language'] or meta.get('complete') is not True or
            meta.get('source') != ('local_model_on_synthetic_outlinks' if raw['metadata'].get('demo')
                                   else 'local_model_on_verified_SQL_dump_outlinks') or
            bool(meta.get('demo')) != bool(raw['metadata'].get('demo')) or
            meta.get('input_sha256') != json_sha256(raw)):
        raise ValueError('Supplemento dei temi non corrispondente al grafo')
    model_labels = meta.get('model', {}).get('labels', [])
    if (not isinstance(model_labels, list) or len(model_labels) != 64 or
            any(not isinstance(label, str) or '.' not in label for label in model_labels)):
        raise ValueError('Tassonomia dei temi non valida')
    taxonomy = set(model_labels)
    if len(taxonomy) != 64 or meta.get('model', {}).get('threshold') != 0.5:
        raise ValueError('Tassonomia dei temi non valida')
    records = {record['page_id']: record for record in supplement.get('pages', [])}
    if len(records) != len(supplement.get('pages', [])) or set(records) != {row['page_id'] for row in rows}:
        raise ValueError('Il supplemento dei temi deve coprire tutto il censimento')
    for row in rows:
        record = records[row['page_id']]
        labels = record.get('topic_labels')
        if record.get('revision_id') != row.get('revision_id') or (labels is not None and
                (not isinstance(labels, list) or any(label not in taxonomy for label in labels) or len(labels) != len(set(labels)))):
            raise ValueError('Temi o revisione non corrispondenti')
        if (labels is None) == (record.get('topic_status') == 'predicted'):
            raise ValueError('Stato di disponibilità dei temi incoerente')
        if record.get('topic_status') not in ('predicted', 'no_model_vocabulary', 'no_mapped_outlinks'):
            raise ValueError('Stato dei temi sconosciuto')
        count = record.get('topic_input_count')
        if (not isinstance(count, int) or isinstance(count, bool) or count < 0 or
                (record['topic_status'] == 'predicted') != (count > 0)):
            raise ValueError('Numero di collegamenti del modello incoerente')
        row.update(topic_labels=labels, topic_status=record['topic_status'], topic_input_count=record.get('topic_input_count'))
    return {**meta, 'status': 'predicted', 'input_checksum_verified': True}


def aggregate_index(results, files):
    """Reference complete per-wiki exports without duplicating article data.

    The dashboard pools language-page observations, never entities or global
    accounts. Each member retains its independent graph and creator provenance.
    """
    if not results or set(results) != set(files):
        raise ValueError('Indice aggregato senza tutte le raccolte corrispondenti')
    members = []
    for language, result in sorted(results.items()):
        meta = result['metadata']
        wiki_database(language, meta.get('database'))
        if meta['language'] != language or meta.get('complete') is not True:
            raise ValueError('L’aggregato richiede censimenti completi, uno per lingua')
        if not re.fullmatch(r'[a-zA-Z0-9_.-]+\.json', files[language]):
            raise ValueError('Nome dataset non valido')
        members.append({'language': language, 'file': files[language],
                        'page_count': len(result['pages']), 'metadata': meta})
    if len({bool(m['metadata'].get('demo')) for m in members}) != 1:
        raise ValueError('Non mescolare dati sintetici e dati reali')
    age_rules = {m['metadata'].get('age_rule') for m in members}
    historical_age_rules = {m['metadata'].get('historical_age_rule') for m in members}
    measure_rules = {m['metadata'].get('measures_rule') for m in members}
    from ._native import collection_interval
    start, finish = collection_interval([(m['metadata']['started_at'], m['metadata']['finished_at']) for m in members])
    return {'metadata': {
        'kind': 'aggregate', 'language': 'all', 'languages': sorted(results),
        'complete': True, 'demo': bool(members[0]['metadata'].get('demo')),
        'source': ('Synthetic complete graphs' if members[0]['metadata'].get('demo')
                   else 'Wikimedia SQL dumps (aggregate of source-specific releases)'),
        'started_at': start,
        'finished_at': finish,
        'temporal_consistency': 'independent_source_specific_intervals',
        'observation_unit': 'language_page',
        'age_rule': next(iter(age_rules)) if len(age_rules) == 1 else 'mixed_per_language',
        'historical_age_rule': (next(iter(historical_age_rules)) if len(historical_age_rules) == 1
                                else 'mixed_per_language'),
        'measures_rule': (next(iter(measure_rules)) if len(measure_rules) == 1
                          else 'mixed_per_language'),
        'length_unit': 'UTF-8 wikitext bytes',
    }, 'members': members}


def _comparison_schema(connection):
    connection.executescript('''PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;
        CREATE TABLE wikis(language TEXT PRIMARY KEY, metadata TEXT, page_count INTEGER);
        CREATE TABLE pages(language TEXT, id INTEGER, position INTEGER, item TEXT, title TEXT,
            incoming INTEGER, outgoing INTEGER, data TEXT, PRIMARY KEY(language,id)) WITHOUT ROWID;
        CREATE INDEX pages_by_position ON pages(language,position);
        CREATE TABLE items(language TEXT, item TEXT, id INTEGER,
            PRIMARY KEY(language,item), UNIQUE(language,id)) WITHOUT ROWID;
        CREATE INDEX items_by_item ON items(item,language,id);
        CREATE TABLE ambiguous(language TEXT, item TEXT, PRIMARY KEY(language,item)) WITHOUT ROWID;
        CREATE TABLE edges(language TEXT, target INTEGER, source INTEGER);
        CREATE TABLE candidates(language TEXT, id INTEGER, status TEXT, candidate_count INTEGER, data TEXT,
            PRIMARY KEY(language,id)) WITHOUT ROWID;
        CREATE TABLE comparison_inputs(language TEXT PRIMARY KEY, graph_identity TEXT,
            analysis_identity TEXT, sources TEXT);
        CREATE TABLE comparison_metadata(language TEXT PRIMARY KEY, metadata TEXT);
        CREATE TABLE comparison_pairs(language TEXT, reference TEXT, local_identity TEXT,
            reference_identity TEXT, PRIMARY KEY(language,reference)) WITHOUT ROWID;
        CREATE TABLE comparison_matches(language TEXT, reference TEXT, target INTEGER, source INTEGER,
            reference_source INTEGER, reference_target INTEGER,
            PRIMARY KEY(language,reference,target,source)) WITHOUT ROWID;
        CREATE INDEX matches_by_target ON comparison_matches(language,target,source,reference);''')


def _comparison_analysis(raw, rules, topics, history=None, quality=None):
    meta = raw['metadata']
    language = meta['language']
    database = wiki_database(language, meta.get('database'))
    if not _approved_graph_source(meta, database):
        raise ValueError('Usare esclusivamente dump SQL verificati')
    if meta.get('item_mapping') != {'source': 'page_props.wikibase_item', 'complete': True}:
        raise ValueError(f'{language}: identificativi interlingua non raccolti; importare un dump completo')
    result = analyze(raw, rules, include_sources=True, topics=topics, history=history, quality=quality)
    result['metadata']['input_sha256'] = json_sha256(raw)
    return result


@operational_phase('comparison.staging')
def _stage_comparison(connection, result):
    """Persist complete observations and only edges eligible for item matching.

    Native degrees still use every article/edge. Ambiguous/missing QIDs cannot
    support a cross-language edge and are excluded only from this lookup index.
    """
    language = result['metadata']['language']
    if connection.execute('SELECT 1 FROM wikis WHERE language=?', (language,)).fetchone():
        raise ValueError('Una sola raccolta per lingua nel confronto')
    pages = result['pages']
    phase_counts(language=language, rows=len(pages))
    from ._native import unique_items, matchable_edges
    for page in pages:
        if type(page['page_id']) is not int or page['page_id'] <= 0:
            raise ValueError('ID di pagina non valido')
    mapping, ambiguous = unique_items([p.get('wikibase_item') if isinstance(p.get('wikibase_item'), str) else None for p in pages])
    with connection:
        connection.execute('INSERT INTO wikis VALUES(?,?,?)',
                           (language, json.dumps(result['metadata'], ensure_ascii=False), len(pages)))
        connection.executemany('INSERT INTO pages VALUES(?,?,?,?,?,?,?,?)',
            ((language, p['page_id'], index, p['wikibase_item'] if isinstance(p['wikibase_item'], str) else None,
              p['title'], p['in_degree'], p['out_degree'], json.dumps(p, ensure_ascii=False)) for index, p in enumerate(pages)))
        connection.executemany('INSERT INTO items VALUES(?,?,?)',
                               ((language, item, pages[index]['page_id']) for item, index in mapping.items()))
        connection.executemany('INSERT INTO ambiguous VALUES(?,?)', ((language, item) for item in ambiguous))
        incoming = result.pop('_incoming_sources')
        connection.executemany('INSERT INTO edges VALUES(?,?,?)',
            ((language, pages[target]['page_id'], pages[source]['page_id'])
             for target, source in matchable_edges(len(pages), mapping, incoming)))


def _comparison_prefetch(connection, language, orphans):
    """Resolve item status and counterpart metadata for one bounded orphan block."""
    ids = [page_id for page_id, _ in orphans]
    placeholders = ','.join('?' for _ in ids)
    records = {}
    for pid, unique_id, ambiguous in connection.execute(f'''
            SELECT p.id,i.id,a.item FROM pages p
            LEFT JOIN items i ON i.language=p.language AND i.item=p.item
            LEFT JOIN ambiguous a ON a.language=p.language AND a.item=p.item
            WHERE p.language=? AND p.id IN ({placeholders})''', (language, *ids)):
        records[pid] = ('ambiguous_item' if ambiguous is not None else
                        'missing_item' if unique_id is None else 'compared', [])
    for pid, other, counterpart_id, title, incoming, ambiguous in connection.execute(f'''
            SELECT p.id,w.language,cp.id,cp.title,cp.incoming,a.item FROM pages p
            JOIN items local ON local.language=p.language AND local.item=p.item
            CROSS JOIN wikis w
            LEFT JOIN items i ON i.language=w.language AND i.item=p.item
            LEFT JOIN pages cp ON cp.language=i.language AND cp.id=i.id
            LEFT JOIN ambiguous a ON a.language=w.language AND a.item=p.item
            WHERE p.language=? AND p.id IN ({placeholders}) AND w.language!=?
            ORDER BY p.id,w.language''', (language, *ids, language)):
        if counterpart_id is None:
            counterpart = {'language': other, 'status': 'ambiguous_item' if ambiguous else 'missing_counterpart'}
        else:
            counterpart = {'language': other, 'status': 'orphan' if incoming == 0 else 'non_orphan',
                           'page_id': counterpart_id, 'title': title, 'in_degree': incoming}
        records[pid][1].append(counterpart)
    return records


def _comparison_edges(connection, language, page_ids, references=None):
    """One indexed edge lookup per target block; SQLite spills its sort to disk.

    Order candidates and their full evidence in SQL so a large target never
    crosses the Python/Rust boundary as a second, complete edge vector.
    CROSS JOIN fixes the target-first lookup order, including on skewed wikis.
    Unique item mappings give each candidate at most one reference edge per
    language, so language alone orders its full evidence tuple exactly.
    """
    placeholders = ','.join('?' for _ in page_ids)
    reference_filter = (' AND ti.language IN (' + ','.join('?' for _ in references) + ')'
                        if references is not None else '')
    return connection.execute(f'''
        SELECT p.id,lp.id,lp.title,lp.outgoing,ti.language,bp.id,bp.title,bt.id,bt.title
        FROM pages p
        CROSS JOIN items ti ON ti.item=p.item AND ti.language!=p.language
        CROSS JOIN edges e ON e.language=ti.language AND e.target=ti.id
        CROSS JOIN items si ON si.language=e.language AND si.id=e.source
        CROSS JOIN items li ON li.language=p.language AND li.item=si.item
        CROSS JOIN pages lp ON lp.language=li.language AND lp.id=li.id
        CROSS JOIN pages bp ON bp.language=si.language AND bp.id=si.id
        CROSS JOIN pages bt ON bt.language=ti.language AND bt.id=ti.id
        WHERE p.language=? AND p.id IN ({placeholders}) AND li.id!=p.id {reference_filter}
        ORDER BY p.id,lp.outgoing!=0,lp.title,lp.id,ti.language
        ''', (language, *page_ids, *(references or ())))


def _matched_comparison_edges(connection, language, page_ids):
    placeholders = ','.join('?' for _ in page_ids)
    return connection.execute(f'''
        SELECT m.target,lp.id,lp.title,lp.outgoing,m.reference,bp.id,bp.title,bt.id,bt.title
        FROM comparison_matches m
        CROSS JOIN pages lp ON lp.language=m.language AND lp.id=m.source
        CROSS JOIN pages bp ON bp.language=m.reference AND bp.id=m.reference_source
        CROSS JOIN pages bt ON bt.language=m.reference AND bt.id=m.reference_target
        WHERE m.language=? AND m.target IN ({placeholders})
        ORDER BY m.target,lp.outgoing!=0,lp.title,lp.id,m.reference''', (language, *page_ids))


@operational_phase('matching.incremental_pairs')
def _update_comparison_pairs(connection):
    identities = dict(connection.execute('SELECT language,graph_identity FROM comparison_inputs'))
    completed = {(local, reference): (a, b) for local, reference, a, b in
                 connection.execute('SELECT * FROM comparison_pairs')}
    for language, identity in sorted(identities.items()):
        references = [other for other in sorted(identities) if other != language and
                      completed.get((language, other)) != (identity, identities[other])]
        reused = len(identities) - 1 - len(references)
        if reused:
            print(f'CHECKPOINT matching {language}: verified_reuse; {reused} coppie', flush=True)
        if not references:
            continue
        with operational_span('matching.new_pairs', language=language, references=references) as span, connection:
            for other in references:
                connection.execute('DELETE FROM comparison_matches WHERE language=? AND reference=?', (language, other))
                connection.execute('DELETE FROM comparison_pairs WHERE language=? AND reference=?', (language, other))
            orphans = connection.execute('''SELECT p.id FROM pages p
                JOIN items i ON i.language=p.language AND i.id=p.id
                WHERE p.language=? AND p.incoming=0 ORDER BY p.id''', (language,))
            edge_count = targets = 0
            block_size = 512
            while block := orphans.fetchmany(block_size):
                edges = iter(_comparison_edges(connection, language, [row[0] for row in block], references))
                count = 0
                while batch := list(itertools.islice(edges, 512)):
                    connection.executemany('INSERT INTO comparison_matches VALUES(?,?,?,?,?,?)',
                        ((language, row[4], row[0], row[1], row[5], row[7]) for row in batch))
                    count += len(batch)
                targets += len(block)
                edge_count += count
                block_size = max(1, min(512, len(block) * 8192 // max(1, count)))
            connection.executemany('INSERT INTO comparison_pairs VALUES(?,?,?,?)',
                ((language, other, identity, identities[other]) for other in references))
            span.update(rows=targets, reference_edges=edge_count)
        print(f'Matching incrementale {language}: {len(references)} coppie nuove '
              f'({" ".join(references)}); {edge_count} evidenze', flush=True)


def _comparison_candidates(edges, prepared):
    status, counterparts = prepared
    candidates = []
    if status == 'compared':
        for (pid, title, outgoing), group in itertools.groupby(edges, key=lambda row: row[1:4]):
            if pid <= 0 or not title or outgoing < 0:
                raise ValueError('invalid candidate evidence')
            evidence = []
            for row in group:
                if not row[4] or row[5] <= 0 or not row[6] or row[7] <= 0 or not row[8]:
                    raise ValueError('invalid candidate evidence')
                evidence.append(dict(zip(('language', 'source_page_id', 'source_title',
                                          'target_page_id', 'target_title'), row[4:])))
            candidates.append({'page_id': pid, 'title': title, 'out_degree': outgoing,
                               'dead_end': outgoing == 0, 'evidence': evidence})
        if not any(c['status'] in ('orphan', 'non_orphan') for c in counterparts):
            status = 'counterpart_unavailable'
    return {'status': status, 'candidate_count': len(candidates) if status == 'compared' else None,
            'counterparts': counterparts,
            'candidates': candidates}


class _StagedPages:
    def __init__(self, connection, language, count):
        self.connection, self.language, self.count = connection, language, count

    def __len__(self):
        return self.count

    def __iter__(self):
        for row in self.connection.execute('SELECT data FROM pages WHERE language=? ORDER BY position', (self.language,)):
            yield json.loads(row[0])

    def csv_rows(self, candidates):
        if candidates.connection is not self.connection or candidates.language != self.language:
            raise ValueError('Staging CSV e candidati non corrispondenti')
        for encoded, status, count in self.connection.execute('''
                SELECT p.data,c.status,c.candidate_count FROM pages p
                LEFT JOIN candidates c ON c.language=p.language AND c.id=p.id
                WHERE p.language=? ORDER BY p.position''', (self.language,)):
            yield json.loads(encoded), {'status': status, 'candidate_count': count}


class _StagedCandidates(Mapping):
    def __init__(self, connection, language):
        self.connection, self.language = connection, language

    def __len__(self):
        return self.connection.execute('SELECT COUNT(*) FROM candidates WHERE language=?', (self.language,)).fetchone()[0]

    def __iter__(self):
        for row in self.connection.execute('SELECT id FROM candidates WHERE language=? ORDER BY id', (self.language,)):
            yield str(row[0])

    def __getitem__(self, key):
        row = self.connection.execute('SELECT data FROM candidates WHERE language=? AND id=?', (self.language, key)).fetchone()
        if row is None:
            raise KeyError(key)
        return json.loads(row[0])

    def items(self):
        for pid, encoded in self.connection.execute('SELECT id,data FROM candidates WHERE language=? ORDER BY id', (self.language,)):
            yield str(pid), json.loads(encoded)


@operational_phase('comparison')
def _finish_comparison(connection, incremental=False):
    """Batch scalar and edge lookups, retaining one target's final record at a time."""
    # The edge stream is appended while each wiki is analyzed. Build its
    # lookup index once, after all inserts, to avoid random B-tree writes for
    # hundreds of millions of links in a large SQL collection.
    with operational_span('comparison.edge_index'):
        connection.execute('CREATE UNIQUE INDEX IF NOT EXISTS edges_by_target '
                           'ON edges(language,target,source)')
    if incremental:
        _update_comparison_pairs(connection)
        connection.execute('DELETE FROM candidates')
        connection.execute('DELETE FROM comparison_metadata')
        connection.commit()
    wikis = [(language, json.loads(meta), count) for language, meta, count in
             connection.execute('SELECT language,metadata,page_count FROM wikis ORDER BY language')]
    if len(wikis) < 2:
        raise ValueError('Servono almeno due raccolte complete di lingue diverse')
    if len({bool(meta.get('demo')) for _, meta, _ in wikis}) != 1:
        raise ValueError('Non mescolare dati sintetici e dati reali')
    provenance = [{key: meta[key] for key in ('language', 'source', 'started_at', 'finished_at', 'input_sha256')}
                  for _, meta, _ in wikis]
    counts = lambda table: {lang: connection.execute(f'SELECT COUNT(*) FROM {table} WHERE language=?', (lang,)).fetchone()[0]
                            for lang, _, _ in wikis}
    mapped, ambiguous = counts('items'), counts('ambiguous')
    results = {}
    for language, meta, count in wikis:
        processed = 0
        with connection, operational_span('matching.wiki', language=language) as wiki_span:
            orphans = connection.execute('SELECT id,item FROM pages WHERE language=? AND incoming=0 ORDER BY id',
                                         (language,))
            block_size = 512
            while block := orphans.fetchmany(block_size):
                with operational_span('matching.block', language=language) as block_span:
                    prepared = _comparison_prefetch(connection, language, block)
                    block_span.update(rows=len(block), reference_edges=0, candidate_rows=0,
                                      max_target_evidence_rows=0, edge_fetch_batch_rows=1)
                    eligible = [pid for pid, _ in block if prepared[pid][0] == 'compared']
                    edges = ((_matched_comparison_edges(connection, language, eligible) if incremental else
                              _comparison_edges(connection, language, eligible)) if eligible else ())
                    groups = iter(itertools.groupby(edges, key=lambda row: row[0]))
                    current = next(groups, None)
                    for pid, item in block:
                        has_edges = current is not None and current[0] == pid
                        record = _comparison_candidates(current[1] if has_edges else (), prepared[pid])
                        if has_edges:
                            current = next(groups, None)
                        edge_count = sum(len(candidate['evidence']) for candidate in record['candidates'])
                        block_span['reference_edges'] += edge_count
                        block_span['candidate_rows'] += len(record['candidates'])
                        block_span['max_target_evidence_rows'] = max(block_span['max_target_evidence_rows'], edge_count)
                        connection.execute('INSERT INTO candidates VALUES(?,?,?,?,?)',
                                           (language, pid, record['status'], record['candidate_count'],
                                            json.dumps(record, ensure_ascii=False)))
                        del record
                        processed += 1
                        if processed % 1000 == 0:
                            print(f'Confronto {language}: {processed} orfane; tutte le lingue di riferimento', flush=True)
                    # The first block has at most 512 targets. Subsequent sorts
                    # aim for 8192 evidence rows using observed fanout. Even an
                    # exceptional single target is streamed in full, never cut.
                    block_size = max(1, min(512, len(block) * 8192 // max(1, block_span['reference_edges'])))
            wiki_span['rows'] = processed
        meta['comparison'] = {'rule': 'cross_language_incoming_v1', 'status': 'compared',
            'reference_languages': [other for other, _, _ in wikis if other != language],
            'collections': provenance, 'matching_source': 'local page_props.wikibase_item from SQL dumps',
            'ambiguous_items_excluded': ambiguous, 'mapped_articles': mapped,
            'temporal_consistency': 'independent_source_specific_intervals',
            'interpretation': 'candidates for manual review, not validated recommendations'}
        results[language] = {'metadata': meta, 'pages': _StagedPages(connection, language, count),
                             'link_candidates': _StagedCandidates(connection, language)}
        if incremental:
            connection.execute('INSERT INTO comparison_metadata VALUES(?,?)',
                               (language, json.dumps(meta, ensure_ascii=False)))
        print(f'Confronto completato: {language}; {processed} orfane', flush=True)
    return results


def compare_languages(raws, rules=None, topics=None, histories=None, qualities=None):
    """Materialized comparison for small synthetic parity tests.

    Both paths share one matching algorithm. This API deliberately materializes
    its return value; use compare_files for large on-disk collections.
    """
    topics, histories, qualities = topics or {}, histories or {}, qualities or {}
    languages = [r['metadata']['language'] for r in raws]
    if (set(topics) | set(histories) | set(qualities)) - set(languages):
        raise ValueError('Supplemento senza la corrispondente raccolta')
    with closing(sqlite3.connect(':memory:')) as connection:
        _comparison_schema(connection)
        for raw in raws:
            language = raw['metadata']['language']
            _stage_comparison(connection, _comparison_analysis(raw, rules, topics.get(language),
                                                                histories.get(language), qualities.get(language)))
        return {lang: {**result, 'pages': list(result['pages']), 'link_candidates': dict(result['link_candidates'].items())}
                for lang, result in _finish_comparison(connection).items()}


def _comparison_file_worker(path, staging, rules, topics, histories, qualities, sender,
                            parent_id=None, run_id=None):
    """A sequential spawned worker releases all graph/JSON/native allocations."""
    try:
        if parent_id is not None:
            os.environ['ORPHANWIKI_PARENT_SPAN'] = parent_id
        if run_id is not None:
            os.environ['ORPHANWIKI_RUN_ID'] = run_id
        from .dumps import _read_first_metadata
        graph_metadata = _read_first_metadata(path)
        language = graph_metadata['language']
        topic = topics.get(language)
        checkpoint = Path(path).with_name(Path(path).stem.removesuffix('-raw') + '-topics.json')
        if topic is None and checkpoint.exists():
            topic = checkpoint
        history = histories.get(language)
        checkpoint = Path(path).with_name(Path(path).stem.removesuffix('-raw') + '-history.json')
        if history is None and checkpoint.exists():
            history = checkpoint
        quality = qualities.get(language)
        checkpoint = Path(path).with_name(Path(path).stem.removesuffix('-raw') + '-quality.json')
        if quality is None and checkpoint.exists():
            quality = checkpoint
        bounded = (Path(path).stat().st_size >= 128 * 1024**2 and
                   graph_metadata.get('source', '').startswith('Wikimedia SQL dumps / '))
        with closing(sqlite3.connect(staging)) as connection:
            # Random target-key inserts dominate the large SQL comparison.
            # Keep its B-tree pages in a bounded cache instead of rereading
            # them from disk for each source-page batch.
            cache_kib = 262144 if bounded else 8192
            connection.execute(f'PRAGMA cache_size=-{cache_kib}')
            connection.execute('PRAGMA temp_store=FILE')
            if bounded:
                # This database is unpublished staging and is discarded on a
                # failed comparison; per-batch disk sync is unnecessary.
                connection.execute('PRAGMA synchronous=OFF')
            if bounded:
                from .dumps import analyze_imported_dump
                candidate_stage = Path(staging).parent.parent
                database = wiki_database(language, graph_metadata.get('database'))
                release = graph_metadata['sql_release']
                import_work_dir = (candidate_stage if (candidate_stage / f'{database}-{release}.sqlite').is_file()
                                   else None)
                print(f'Analisi SQL a blocchi {language}', flush=True)
                analyze_imported_dump(path, history, Path(staging).parent / 'comparison-analysis.json',
                                      rules, import_work_dir, topic, comparison_connection=connection, quality_path=quality)
            else:
                raw = json.loads(Path(path).read_text())
                print(f'Analisi integrale {language}: {len(raw["pages"])} voci', flush=True)
                predictions = json.loads(Path(topic).read_text()) if topic is not None else None
                historical = json.loads(Path(history).read_text()) if history is not None else None
                quality_data = json.loads(Path(quality).read_text()) if quality is not None else None
                result = _comparison_analysis(raw, rules, predictions, historical, quality_data)
                del raw, predictions
                print(f'Grafo {language} risolto; salvataggio degli indici su disco', flush=True)
                _stage_comparison(connection, result)
        try:
            import resource
            import sys
            scale = 2**20 if sys.platform == 'darwin' else 1024
            peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / scale
        except ImportError:
            peak = None
        sender.send({'language': language, 'peak_rss': peak})
    except Exception as error:
        sender.send({'error': str(error), 'type': type(error).__name__})
    finally:
        sender.close()


def comparison_collections(paths, output_dir, allowed_languages):
    """Include previously published configured editions, with their local raw sources.

    This only reads approved metadata. It never acquires or substitutes a
    release for an old member; missing local sources stop an expanded run.
    """
    from .dumps import _read_first_metadata
    supplied = {_read_first_metadata(path)['language']: Path(path) for path in paths}
    if len(supplied) != len(paths):
        raise ValueError('Una sola raccolta per lingua nel confronto')
    manifest_path = Path(output_dir) / 'manifest.json'
    exports = {}
    if manifest_path.exists():
        entries = json.loads(manifest_path.read_text())
        if not isinstance(entries, list):
            raise ValueError('Manifest non valido')
        latest = {}
        for entry in entries:
            if (not isinstance(entry, dict) or entry.get('kind', 'wiki') != 'wiki'
                    or entry.get('demo') or entry.get('language') not in allowed_languages):
                continue
            language = entry['language']
            if language not in latest or (entry.get('date', ''), entry.get('published_at', '')) > (
                    latest[language].get('date', ''), latest[language].get('published_at', '')):
                latest[language] = entry
        for language, entry in latest.items():
            filename = entry.get('file')
            if not isinstance(filename, str) or not re.fullmatch(r'[a-zA-Z0-9_.-]+\.json', filename):
                raise ValueError('Nome export approvato non valido')
            export = Path(output_dir) / filename
            metadata = _read_first_metadata(export)
            if (metadata.get('language') != language or metadata.get('complete') is not True
                    or not _approved_graph_source(metadata, wiki_database(language, metadata.get('database')))):
                if language in supplied:
                    # A selected edition may be repairing an obsolete export.
                    # Recompute it from the explicit verified source instead.
                    continue
                raise ValueError('Export precedente non verificabile per il confronto incrementale')
            exports[language] = export
            if language not in supplied:
                options = {Path(path).parent / f'{language}-raw.json' for path in paths}
                found = [path for path in options if path.is_file()]
                if len(found) != 1:
                    raise ValueError(f'{language}: conservare il grafo locale già raccolto per estendere il confronto')
                supplied[language] = found[0]
    return [supplied[language] for language in sorted(supplied)], exports


def _comparison_source(path, rules, topics, histories, qualities, signatures=None):
    from .dumps import _read_first_metadata, _hash
    path = Path(path).resolve()
    metadata = _read_first_metadata(path)
    language = metadata['language']
    def checksum(source):
        source = Path(source).resolve()
        before = source.stat()
        digest = _hash(source, 'sha256')
        after = source.stat()
        signature = (before.st_size, before.st_mtime_ns, before.st_ctime_ns, before.st_ino)
        if signature != (after.st_size, after.st_mtime_ns, after.st_ctime_ns, after.st_ino):
            raise ValueError('Input modificato durante la verifica del confronto')
        if signatures is not None:
            signatures[str(source)] = signature
        return digest
    sources = {'raw': str(path)}
    digests = {'raw': checksum(path)}
    for kind, supplied in (('topics', topics), ('history', histories), ('quality', qualities)):
        supplement = supplied.get(language)
        if supplement is None:
            candidate = path.with_name(path.stem.removesuffix('-raw') + f'-{kind}.json')
            supplement = candidate if candidate.exists() else None
        sources[kind] = str(Path(supplement).resolve()) if supplement is not None else None
        digests[kind] = checksum(supplement) if supplement is not None else None
    graph = json_sha256({'rule': COMPARISON_CACHE_RULE, 'raw': digests['raw']})
    analysis = json_sha256({'rule': COMPARISON_CACHE_RULE, 'inputs': digests,
                           'categories': rules or {}, 'measures': MEASURES_RULE,
                           'quality_revision': QUALITY_REVISION_RULE})
    return language, graph, analysis, sources


def _cached_comparison_results(connection):
    return {language: {'metadata': json.loads(metadata),
                      'pages': _StagedPages(connection, language, count),
                      'link_candidates': _StagedCandidates(connection, language)}
            for language, count, metadata in connection.execute('''
                SELECT w.language,w.page_count,m.metadata FROM wikis w
                JOIN comparison_metadata m ON m.language=w.language ORDER BY w.language''')}


@contextmanager
def _comparison_cache(work_dir):
    """Serialize writers; immutable, hashed generations survive failed updates."""
    import fcntl
    from .dumps import _hash
    cache = Path(work_dir) / 'comparison-cache'
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / 'writer.lock').open('a+b') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RuntimeError('Un altro confronto usa lo stesso indice incrementale') from error
        try:
            manifest = cache / 'manifest.json'
            current = None
            if manifest.exists():
                record = json.loads(manifest.read_text())
                name = record.get('file') if isinstance(record, dict) else None
                if (not isinstance(record, dict) or record.get('format') != COMPARISON_CACHE_RULE or not isinstance(name, str)
                        or not re.fullmatch(r'[0-9a-f]{32}\.sqlite', name)):
                    raise ValueError('Identità della cache del confronto non valida')
                current = cache / name
                if (current.stat().st_size != record.get('bytes')
                        or _hash(current, 'sha256') != record.get('sha256')):
                    raise ValueError('Checksum della cache del confronto non valido; conservare la generazione precedente')
            yield cache, current
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


@operational_phase('matching.adopt_published_pairs')
def _seed_comparison_pairs(connection, exports):
    """Adopt complete approved old comparisons after verifying their graph identities.

    The full source document is parsed, with bounded SQL buffers. Every adopted
    edge must exist in the verified graph and map to the stated local QIDs.
    Missing/ambiguous/zero outcomes are retained by explicit pair checkpoints.
    """
    from .dumps import _read_first_metadata
    wikis = {language: json.loads(metadata) for language, metadata in
             connection.execute('SELECT language,metadata FROM wikis')}
    identities = dict(connection.execute('SELECT language,graph_identity FROM comparison_inputs'))
    connection.execute('CREATE UNIQUE INDEX IF NOT EXISTS edges_by_target ON edges(language,target,source)')
    for language, path in exports.items():
        if language not in wikis:
            continue
        metadata = _read_first_metadata(path)
        comparison = metadata.get('comparison', {})
        if (comparison.get('rule') != 'cross_language_incoming_v1'
                or comparison.get('status') != 'compared'
                or metadata.get('input_sha256') != wikis[language].get('input_sha256')):
            continue
        collections = {row['language']: row for row in comparison.get('collections', [])}
        references = [other for other in comparison.get('reference_languages', []) if other in wikis and
            other != language and other in collections and all(collections[other].get(key) == wikis[other].get(key)
            for key in ('source', 'started_at', 'finished_at', 'input_sha256')) and not connection.execute(
                'SELECT 1 FROM comparison_pairs WHERE language=? AND reference=?', (language, other)).fetchone()]
        if not references:
            continue
        original = path.stat()
        connection.executescript('''
            DROP TABLE IF EXISTS temp.seed_seen;
            DROP TABLE IF EXISTS temp.seed_edges;
            CREATE TEMP TABLE seed_seen(id INTEGER PRIMARY KEY);
            CREATE TEMP TABLE seed_edges(target INTEGER,source INTEGER,reference TEXT,
                reference_source INTEGER,reference_target INTEGER, title TEXT,outgoing INTEGER,
                source_title TEXT,target_title TEXT,
                PRIMARY KEY(reference,target,source)) WITHOUT ROWID;''')
        buffer = []
        def flush():
            connection.executemany('INSERT INTO seed_edges VALUES(?,?,?,?,?,?,?,?,?)', buffer)
            buffer.clear()
        with connection:
            pages = 0
            for section, identifier, record in iter_export_sections(path):
                if section == 'pages':
                    pages += 1
                elif section == 'link_candidates':
                    if not identifier.isdecimal() or int(identifier) <= 0:
                        raise ValueError('ID orfana non valido nel confronto precedente')
                    target = int(identifier)
                    connection.execute('INSERT INTO seed_seen VALUES(?)', (target,))
                    candidates = record.get('candidates')
                    if (not isinstance(candidates, list) or record.get('candidate_count') != (
                            len(candidates) if record.get('status') == 'compared' else None)
                            or (record.get('status') != 'compared' and candidates)
                            or len({candidate['page_id'] for candidate in candidates}) != len(candidates)):
                        raise ValueError('Candidati precedenti incompleti o duplicati')
                    for candidate in candidates:
                        if (not candidate.get('evidence') or
                                candidate.get('dead_end') != (candidate.get('out_degree') == 0)):
                            raise ValueError('Candidato precedente privo di evidenze o grado coerente')
                        for edge in candidate['evidence']:
                            if edge['language'] not in references:
                                continue
                            buffer.append((target, candidate['page_id'], edge['language'], edge['source_page_id'],
                                edge['target_page_id'], candidate['title'], candidate['out_degree'],
                                edge['source_title'], edge['target_title']))
                            if len(buffer) == 512:
                                flush()
            flush()
            count = connection.execute('SELECT page_count FROM wikis WHERE language=?', (language,)).fetchone()[0]
            orphans = connection.execute('SELECT COUNT(*) FROM pages WHERE language=? AND incoming=0', (language,)).fetchone()[0]
            if (pages != count or connection.execute('SELECT COUNT(*) FROM seed_seen').fetchone()[0] != orphans
                    or connection.execute('''SELECT 1 FROM seed_seen s LEFT JOIN pages p
                        ON p.language=? AND p.id=s.id WHERE p.id IS NULL OR p.incoming!=0 LIMIT 1''', (language,)).fetchone()):
                raise ValueError('Censimento delle orfane precedente incompleto')
            invalid = connection.execute('''SELECT 1 FROM seed_edges s
                LEFT JOIN pages lp ON lp.language=? AND lp.id=s.source
                LEFT JOIN pages bp ON bp.language=s.reference AND bp.id=s.reference_source
                LEFT JOIN pages bt ON bt.language=s.reference AND bt.id=s.reference_target
                LEFT JOIN items li ON li.language=? AND li.id=s.source
                LEFT JOIN items si ON si.language=s.reference AND si.id=s.reference_source AND si.item=li.item
                LEFT JOIN items ti ON ti.language=? AND ti.id=s.target
                LEFT JOIN items ri ON ri.language=s.reference AND ri.id=s.reference_target AND ri.item=ti.item
                LEFT JOIN edges e ON e.language=s.reference AND e.target=s.reference_target AND e.source=s.reference_source
                WHERE lp.id IS NULL OR bp.id IS NULL OR bt.id IS NULL OR si.id IS NULL OR ri.id IS NULL
                    OR e.source IS NULL OR s.target=s.source OR lp.title!=s.title OR lp.outgoing!=s.outgoing
                    OR bp.title!=s.source_title OR bt.title!=s.target_title LIMIT 1''', (language, language, language)).fetchone()
            if invalid or (path.stat().st_size, path.stat().st_mtime_ns) != (original.st_size, original.st_mtime_ns):
                raise ValueError('Evidenze precedenti incoerenti con i grafi verificati')
            connection.execute('''INSERT INTO comparison_matches
                SELECT ?,reference,target,source,reference_source,reference_target FROM seed_edges''', (language,))
            connection.executemany('INSERT INTO comparison_pairs VALUES(?,?,?,?)',
                ((language, other, identities[language], identities[other]) for other in references))
        connection.executescript('DROP TABLE temp.seed_seen; DROP TABLE temp.seed_edges;')
        print(f'CHECKPOINT matching {language}: adottate {len(references)} coppie pubblicate', flush=True)


@contextmanager
def compare_files(paths, rules=None, topics=None, work_dir=None, histories=None, qualities=None,
                  allowed_languages=None, published=None):
    """Reuse verified observations/indices and completed ordered language pairs.

    Cache generations are immutable and hashed. An update copies the previous
    SQLite indices, stages only changed editions, computes only missing pairs,
    and atomically approves the completed generation. Publication can fail and
    retry from this completed operational checkpoint without redoing matching.
    """
    from .dumps import _read_first_metadata, _hash
    if not paths:
        raise ValueError('Servono raccolte complete per il confronto')
    work_dir = Path(work_dir or Path(paths[0]).parent / 'staging').resolve()
    work_dir.mkdir(parents=True, exist_ok=True)
    def supplements(files):
        result = {}
        for path in files or []:
            language = _read_first_metadata(path)['language']
            if language in result:
                raise ValueError('Un solo supplemento per lingua')
            result[language] = str(path)
        return result
    topics, histories, qualities = supplements(topics), supplements(histories), supplements(qualities)
    context = multiprocessing.get_context('spawn')
    with _comparison_cache(work_dir) as (cache, current):
        previous = {}
        if current is not None:
            with closing(sqlite3.connect(current.as_uri() + '?mode=ro', uri=True)) as source:
                previous = {language: (graph, analysis, json.loads(sources)) for language, graph, analysis, sources in
                            source.execute('SELECT * FROM comparison_inputs')}
        inputs, signatures = {}, {}
        for path in paths:
            item = _comparison_source(path, rules, topics, histories, qualities, signatures)
            if item[0] in inputs:
                raise ValueError('Una sola raccolta per lingua nel confronto')
            inputs[item[0]] = item[1:]
        for language, (_, _, saved) in previous.items():
            if language in inputs or (allowed_languages is not None and language not in allowed_languages):
                continue
            saved = {key: str(work_dir / value) if value is not None else None for key, value in saved.items()}
            selected = []
            for kind, supplied in (('topics', topics), ('history', histories), ('quality', qualities)):
                path = supplied.get(language, saved.get(kind))
                selected.append({language: path} if path else {})
            item = _comparison_source(saved['raw'], rules, *selected, signatures=signatures)
            if item[0] != language:
                raise ValueError('Identità della lingua cambiata nel grafo conservato')
            inputs[language] = item[1:]
        if len(inputs) < 2:
            raise ValueError('Servono almeno due raccolte complete di lingue diverse')
        # Store paths relative to the operational work root, so moving a complete
        # local workspace does not embed a contributor's absolute paths.
        saved_sources = {language: {kind: os.path.relpath(path, work_dir) if path is not None else None
                                   for kind, path in sources.items()}
                         for language, (_, _, sources) in inputs.items()}
        dirty = {language for language, (graph, analysis, _) in inputs.items()
                 if language not in previous or previous[language][:2] != (graph, analysis)}
        retired = set(previous) - inputs.keys()
        paths_changed = any(language not in previous or previous[language][2] != saved_sources[language]
                            for language in inputs)
        if current is not None and not dirty and not retired and not paths_changed:
            print('CHECKPOINT confronto incrementale: verified_reuse; indici e tutte le coppie', flush=True)
            with closing(sqlite3.connect(current.as_uri() + '?mode=ro', uri=True)) as connection:
                results = _cached_comparison_results(connection)
                if set(results) != set(inputs):
                    raise ValueError('Cache del confronto incompleta')
                yield results
            return
        with tempfile.TemporaryDirectory(prefix='orphanwiki-compare-', dir=work_dir) as folder:
            staging = Path(folder) / 'comparison.sqlite'
            with closing(sqlite3.connect(staging)) as connection:
                connection.execute('PRAGMA cache_size=-8192')
                connection.execute('PRAGMA temp_store=FILE')
                if current is not None:
                    with closing(sqlite3.connect(current.as_uri() + '?mode=ro', uri=True)) as source:
                        with operational_span('comparison.copy_verified_indices', bytes=current.stat().st_size):
                            source.backup(connection, pages=4096)
                else:
                    _comparison_schema(connection)
                with connection:
                    for language in sorted(dirty | retired):
                        graph_changed = (language in retired or language not in previous or
                                         inputs[language][0] != previous[language][0])
                        if graph_changed:
                            for table in ('comparison_pairs', 'comparison_matches'):
                                connection.execute(f'DELETE FROM {table} WHERE language=? OR reference=?', (language, language))
                        for table in ('wikis', 'pages', 'items', 'ambiguous', 'edges', 'candidates',
                                      'comparison_inputs', 'comparison_metadata'):
                            connection.execute(f'DELETE FROM {table} WHERE language=?', (language,))
                for language in sorted(dirty):
                    _, _, sources = inputs[language]
                    receiver, sender = context.Pipe(duplex=False)
                    trace = operational_environment()
                    worker = context.Process(target=_comparison_file_worker,
                        args=(sources['raw'], str(staging), rules,
                              {language: sources['topics']} if sources['topics'] else {},
                              {language: sources['history']} if sources['history'] else {},
                              {language: sources['quality']} if sources['quality'] else {}, sender,
                              trace.get('ORPHANWIKI_PARENT_SPAN'), trace.get('ORPHANWIKI_RUN_ID')))
                    try:
                        worker.start()
                        sender.close()
                        try:
                            response = receiver.recv()
                        except EOFError as error:
                            worker.join()
                            raise RuntimeError(f'Analisi terminata senza risultato: {sources["raw"]}; exit {worker.exitcode}') from error
                        worker.join()
                        if 'error' in response:
                            exception = ValueError if response['type'] == 'ValueError' else RuntimeError
                            raise exception(f'{sources["raw"]}: {response["error"]}')
                        if worker.exitcode != 0 or response['language'] != language:
                            raise RuntimeError(f'Analisi fallita: {sources["raw"]}; exit {worker.exitcode}')
                        peak = response['peak_rss']
                        memory = f'{peak:.1f} MiB' if peak is not None else 'non disponibile'
                        print(f'Indice pronto: {language}; picco RSS worker {memory}', flush=True)
                    finally:
                        if worker.is_alive():
                            worker.terminate()
                            worker.join()
                        sender.close()
                        receiver.close()
                with connection:
                    for language, (graph, analysis, _) in inputs.items():
                        connection.execute('INSERT OR REPLACE INTO comparison_inputs VALUES(?,?,?,?)',
                            (language, graph, analysis, json.dumps(saved_sources[language], sort_keys=True)))
                if dirty or retired:
                    _seed_comparison_pairs(connection, published or {})
                    _finish_comparison(connection, incremental=True)
                connection.commit()
            for path, signature in signatures.items():
                info = Path(path).stat()
                if (info.st_size, info.st_mtime_ns, info.st_ctime_ns, info.st_ino) != signature:
                    raise ValueError('Input modificato durante il confronto incrementale')
            name = uuid.uuid4().hex + '.sqlite'
            checkpoint = {'format': COMPARISON_CACHE_RULE, 'file': name,
                          'bytes': staging.stat().st_size, 'sha256': _hash(staging, 'sha256')}
            staging.replace(cache / name)
            write_json(cache / 'manifest.json', checkpoint)
        with closing(sqlite3.connect((cache / name).as_uri() + '?mode=ro', uri=True)) as connection:
            yield _cached_comparison_results(connection)


@operational_phase('analysis.wiki')
def analyze(raw, rules=None, include_sources=False, topics=None, history=None, quality=None):
    from .categories import classify
    language = raw['metadata']['language']
    phase_counts(language=language, rows=len(raw['pages']))
    wiki_database(language, raw['metadata'].get('database'))
    if 'biographies' in raw['metadata'] or any('biography' in p for p in raw['pages']):
        raise ValueError('Dataset arricchito da una fonte esterna: usare i dump ufficiali non modificati')
    if raw['metadata'].get('complete') is not True:
        raise ValueError('Il grafo deve comprendere tutta la wiki: un campione genera false orfane.')
    redirects = raw.get('redirects', {})
    if len({p['page_id'] for p in raw['pages']}) != len(raw['pages']):
        raise ValueError('Identificativi di pagina duplicati')
    from ._native import graph_counts, graph_details, article_ages
    titles = [p['title'] for p in raw['pages']]
    links = [p.get('links', []) for p in raw['pages']]
    sources = None
    if include_sources:
        ins, outs, unresolved, sources = graph_details(titles, links, redirects)
    else:
        ins, outs, unresolved = graph_counts(titles, links, redirects)
    choices = classify(raw['pages'], raw.get('category_parents', {}), rules or {})
    creation_events = raw['metadata'].get('creation_date_rule') == 'creation_event_v1'
    rows = []
    for p, incoming, outgoing in zip(raw['pages'], ins, outs):
        rows.append(observation_row(p, choices[p['page_id']], incoming, outgoing, creation_events))
    creator_metadata = initialize_creator_rows(rows)
    history_metadata = attach_history(rows, raw['metadata'], history)
    quality_metadata = attach_quality(rows, raw['metadata'], quality) if quality is not None else None
    reconstructed = history_metadata.get('creation_rule') == 'mediawiki_history_page_creation_v2'
    ages = article_ages(raw['metadata']['finished_at'],
                        [r['page_created_at'] if creation_events or reconstructed else r['created_at'] for r in rows])
    history_ages = article_ages(raw['metadata']['finished_at'],
                                [r['history_first_revision_at'] for r in rows])
    for row, (age, after_collection), (history_age, _) in zip(rows, ages, history_ages):
        row['created_at'] = row['page_created_at'] if creation_events or reconstructed else row['created_at']
        row['age_days'] = age
        row['history_first_revision_age_days'] = history_age
        if after_collection:
            row['creation_date_status'] = 'after_graph_collection'
    result = {'metadata': {**raw['metadata'], 'creators': creator_metadata, 'history': history_metadata,
                         'unresolved_target_occurrences': unresolved,
                         'category_rule': 'structural_specificity_v1', 'category_rules': rules or {}, 'engine': 'rust',
                         'measures_rule': MEASURES_RULE,
                         'age_rule': ('mediawiki_history_page_creation_v2' if reconstructed else
                                      'page_creation_event' if creation_events else
                                      'unavailable_no_approved_creation_event' if raw['metadata'].get('source', '').startswith('Wikimedia SQL dumps / ')
                                      else 'first_public_revision_legacy'),
                         'historical_age_rule': ('mediawiki_history_first_observed_revision_v1'
                                                 if history_metadata['status'] == 'collected' else 'not_collected'),
                         'length_unit': 'UTF-8 wikitext bytes'}, 'pages': rows}
    if topics is not None:
        result['metadata']['topics'] = attach_topics(rows, raw, topics)
        result['metadata']['article_features'] = {**result['metadata'].get('article_features', {}), 'topics': 'local_model_predictions'}
    if quality_metadata is not None:
        result['metadata']['quality'] = quality_metadata
        result['metadata']['article_features'] = {**result['metadata'].get('article_features', {}), 'quality': 'johnson_v2_2022_01'}
    if reconstructed:
        result['metadata']['article_features'] = {**result['metadata'].get('article_features', {}),
                                                 'age': 'MediaWiki History / page_creation_timestamp'}
    if include_sources:
        result['_incoming_sources'] = sources
    return result


def quality_metadata(meta, graph, count, file_checksum=None):
    approved = json.loads((Path(__file__).resolve().parents[1] / 'config/quality.json').read_text())
    if (meta.get('language') != graph.get('language') or meta.get('database') != graph.get('database')
            or meta.get('sql_release') != graph.get('sql_release') or meta.get('complete') is not True
            or meta.get('source') != 'Johnson V2 / mediawiki_content_current' or meta.get('model') != approved
            or meta.get('dump_manifest_sha256') != graph.get('dump_manifest_sha256')
            or meta.get('graph_finished_at') != graph.get('finished_at')
            or meta.get('census_page_count') != count
            or meta.get('quality_revision_rule') != QUALITY_REVISION_RULE
            or type(meta.get('scored_pages')) is not int or not 0 <= meta['scored_pages'] <= count
            or meta.get('missing_pages') != count - meta['scored_pages']
            or any(type(meta.get(key)) is not int or meta[key] < 0 for key in
                   ('same_revision_pages', 'different_revision_pages', 'unavailable_revision_pages'))
            or sum(meta[key] for key in ('same_revision_pages', 'different_revision_pages',
                                         'unavailable_revision_pages')) != count
            or (file_checksum is not None and meta.get('input_file_sha256') != file_checksum)):
        raise ValueError('Qualità non corrispondente al grafo SQL completo')
    normalizers = meta.get('normalizers')
    if (not isinstance(normalizers, list) or len(normalizers) != 6 or
            any(type(value) not in (int, float) or not 0 < value < float('inf') for value in normalizers)):
        raise ValueError('Normalizzatori qualità non validi')
    return {**meta, 'status': 'collected'}


def _join_quality_rows(rows, by_id):
    for row in rows:
        record = by_id.get(row['page_id'])
        if record is None or record.get('revision_id') != row.get('revision_id'):
            raise ValueError('Qualità mancante o revisione diversa')
        score, status = record.get('quality_score'), record.get('quality_status')
        relation = record.get('quality_revision_relation')
        content_revision = record.get('content_revision_id')
        if (status not in ('scored', 'missing_current_page', 'missing_wikitext')
                or relation not in ('same', 'different', 'unavailable')
                or (relation == 'unavailable') != (content_revision is None)
                or (content_revision is not None and (type(content_revision) is not int or content_revision <= 0))
                or (relation == 'same' and content_revision != row.get('revision_id'))
                or (relation == 'different' and content_revision == row.get('revision_id'))
                or (status == 'missing_current_page') != (relation == 'unavailable')
                or (status == 'scored' and (type(score) not in (float, int) or not 0 <= score <= 0.998001))
                or (status != 'scored' and score is not None)):
            raise ValueError('Punteggio qualità non valido')
        row.update(quality_score=score, quality_status=status,
                   content_revision_id=content_revision, quality_revision_relation=relation)


def attach_quality(rows, graph, supplement):
    meta = quality_metadata(supplement.get('metadata', {}), graph, len(rows))
    records = supplement.get('pages', [])
    by_id = {record['page_id']: record for record in records}
    if len(by_id) != len(records) or set(by_id) != {row['page_id'] for row in rows}:
        raise ValueError('Censimento qualità incompleto o duplicato')
    _join_quality_rows(rows, by_id)
    if sum(row['quality_status'] == 'scored' for row in rows) != meta.get('scored_pages'):
        raise ValueError('Copertura qualità incoerente')
    for relation, key in (('same', 'same_revision_pages'), ('different', 'different_revision_pages'),
                          ('unavailable', 'unavailable_revision_pages')):
        if sum(row['quality_revision_relation'] == relation for row in rows) != meta[key]:
            raise ValueError('Relazione delle revisioni qualità incoerente')
    return {**meta, 'input_sha256': json_sha256(supplement)}


def observation_row(page, choice, incoming, outgoing, creation_events=False):
    """Build one article observation from native graph and category results."""
    created = page.get('page_created_at') if creation_events else page.get('created_at')
    return {
        'page_id': page['page_id'], 'title': page['title'], 'revision_id': page.get('revision_id'),
        'wikibase_item': page.get('wikibase_item'), **choice,
        'categories': page.get('categories', []), 'created_at': created,
        'length_bytes': page.get('length_bytes'),
        'first_public_revision_at': page.get('first_public_revision_at', page.get('created_at')),
        'page_created_at': page.get('page_created_at'),
        'creation_date_source': page.get('creation_date_source'),
        'creation_date_status': page.get('creation_date_status', 'not_collected'),
        'in_degree': incoming, 'out_degree': outgoing, 'orphan': incoming == 0,
    }


def _json_chunks(value, ensure_ascii=False, sort_keys=False):
    """Buffer ~64 KiB plus the largest scalar, not the document.

    Sorted checksums also retain sorted key lists for active nested objects.
    """
    encoder = json.JSONEncoder(ensure_ascii=ensure_ascii, sort_keys=sort_keys, allow_nan=False)
    buffered, size = [], 0
    for chunk in encoder.iterencode(value):
        buffered.append(chunk)
        size += len(chunk)
        if size >= 65536:
            yield ''.join(buffered)
            buffered, size = [], 0
    if buffered:
        yield ''.join(buffered)


def _json_record_chunks(value, encoder):
    """Use the C encoder for bounded leaves, streaming candidate and evidence lists."""
    large_key = ('candidates' if isinstance(value, dict) and isinstance(value.get('candidates'), list)
                 else 'evidence' if isinstance(value, dict) and isinstance(value.get('evidence'), list)
                 else None)
    if large_key is None:
        yield encoder.encode(value)
        return
    yield '{'
    for position, (key, item) in enumerate(value.items()):
        if position:
            yield ', '
        yield encoder.encode(key)
        yield ': '
        if key == large_key:
            yield '['
            for index, record in enumerate(item):
                if index:
                    yield ', '
                yield from _json_record_chunks(record, encoder)
            yield ']'
        else:
            yield encoder.encode(item)
    yield '}'


def _detail_chunks(value, encoder):
    yield '{'
    for position, (key, item) in enumerate(value.items()):
        if position:
            yield ', '
        yield encoder.encode(key)
        yield ': '
        if key == 'link_candidates':
            yield '{'
            for index, (identifier, record) in enumerate(item.items()):
                if index:
                    yield ', '
                yield encoder.encode(identifier)
                yield ': '
                yield from _json_record_chunks(record, encoder)
            yield '}'
        else:
            yield encoder.encode(item)
    yield '}'


@operational_phase('hash.canonical_json')
def json_sha256(value):
    """Hash sorted JSON in bounded chunks without allocating the complete document."""
    digest = hashlib.sha256()
    size = 0
    for chunk in _json_chunks(value, ensure_ascii=True, sort_keys=True):
        encoded = chunk.encode('utf-8')
        size += len(encoded)
        digest.update(encoded)
    phase_counts(bytes=size)
    return digest.hexdigest()


# A transport projection for the two analysis pages, not a second census.
# Complete observations and link evidence remain in the canonical JSON/CSV.
OBSERVATION_COLUMNS = (
    'page_id', 'title', 'category', 'category_status', 'orphan', 'in_degree',
    'out_degree', 'created_at', 'age_days', 'history_first_revision_age_days', 'length_bytes', 'origin',
    'creator_key', 'creator_data_status', 'creator_account_type',
    'creator_tenure_days', 'creator_prior_edits_main', 'creator_prior_edits_other',
    'creator_prior_articles', 'creator_articles_created_total',
    'creator_articles_in_census', 'creator_orphans_current',
    'creator_orphans_current_share', 'creator_orphans_at_30_days',
    'creator_attribution', 'creator_registered_at', 'creator_tenure_status',
    'creator_experience_status', 'topic_labels', 'topic_status', 'topic_input_count',
    'wikibase_item', 'page_created_at', 'first_public_revision_at', 'history_first_revision_at',
    'creation_date_source', 'creation_date_status',
    'creator_account_type_at_creation', 'creator_is_bot_now',
    'creator_languages_created', 'creator_languages_before', 'creation_tags',
    'import_log_observed', 'move_log_observed', 'creator_orphans_at_30_days_status',
    'history_creator_key', 'history_creator_status', 'history_data_status',
    'creator_groups_at_creation_history', 'creator_continuity_months_before_creation',
    'creator_continuity_bucket_before_creation', 'creator_prior_edits_main_history',
    'creator_prior_edits_other_history', 'quality_score', 'quality_status',
    'content_revision_id', 'quality_revision_relation',
)

ARTICLE_INDEX_COLUMNS = (
    'page_id', 'title', 'orphan', 'category', 'topic_status', 'topic_labels',
    'length_bytes', 'created_at', 'in_degree',
)


def add_article_preview(preview, page, language=None):
    """Keep the first 25 orphan titles from a complete page stream."""
    if not page['orphan']:
        return
    key = (page['title'].casefold(), page['title'], language or '', page['page_id'])
    if len(preview) == ARTICLE_PREVIEW_LIMIT and key >= preview[-1][0]:
        return
    projected = {field: page.get(field) for field in OBSERVATION_COLUMNS}
    if language is not None:
        projected['language'] = language
    bisect.insort(preview, (key, projected))
    if len(preview) > ARTICLE_PREVIEW_LIMIT:
        preview.pop()


def write_observations(path, result):
    """Stream every article's display values, excluding candidate evidence.

    Column names occur once; missing values remain null. Memory is bounded by
    one article/one candidate record plus the writer's buffer. The canonical
    result is never modified, sampled or reclassified.
    """
    if 'biographies' in result['metadata']:
        raise ValueError('Dataset con arricchimento esterno non autorizzato')
    compact = {'format': 'observations_v1', 'metadata': result['metadata'],
               'columns': OBSERVATION_COLUMNS, 'rows': None, 'link_candidates': None}
    def rows():
        for page in result['pages']:
            if 'biography' in page or 'gender_group' in page:
                raise ValueError('Dataset con arricchimento esterno non autorizzato')
            yield [page.get(field) for field in OBSERVATION_COLUMNS]
    def counts():
        for key, record in result.get('link_candidates', {}).items():
            yield key, {'status': record.get('status'), 'candidate_count': record.get('candidate_count')}
    write_json(path, compact, arrays={'rows': rows()}, mappings={'link_candidates': counts()})


@operational_phase('export.transports')
def write_dashboard_exports(source, folder, expected=None, *, transport_generation=None):
    """Project an ordered canonical export into bounded browser transports.

    Card indices contain every orphan, exact counts and shortlist identities;
    rich shortlists live in separate visible-card records. Complete detail
    records retain every candidate and reference edge for the detail page.
    Read one canonical record at a time; never modify it.
    Canonical candidate order is native dead-end/title/ID order. The overview
    retains the first five eligible candidates from each dead-end group so UI
    ranking can choose its five without loading every reference edge.
    """
    source, folder = Path(source), Path(folder)
    if transport_generation is not None and not re.fullmatch(r'[0-9a-f]{32}', transport_generation):
        raise ValueError('Generazione dei trasporti non valida')
    initial = source.stat()
    events = iter_export_sections(source)
    topic_staging = tempfile.TemporaryDirectory(prefix='orphanwiki-profile-')
    topic_database = None
    try:
        section, _, metadata = next(events)
        if section != 'metadata' or metadata.get('complete') is not True:
            raise ValueError('Raccolta completa richiesta')
        wiki_database(metadata.get('language'))
        if expected and (metadata['language'] != expected['language'] or
                         bool(metadata.get('demo')) != bool(expected.get('demo')) or
                         metadata.get('finished_at') != expected['date']):
            raise ValueError('Manifest e raccolta non corrispondenti')
        generation = json_sha256([TRANSPORT_RULE, metadata, initial.st_size, initial.st_mtime_ns])[:12]
        stem = source.stem + '-' + (transport_generation or generation)
        directory = stem + '-connections'
        card_identity = json_sha256(['orphan_cards_v2', metadata, initial.st_size,
                                     initial.st_mtime_ns, transport_generation])
        connection_identity = json_sha256(['connections_v2', metadata, initial.st_size,
                                           initial.st_mtime_ns, transport_generation])
        card_directory = stem + '-cards'
        filenames = {'observations_file': stem + '-observations.json',
                     'articles_file': stem + '-articles.json',
                     'summary_file': stem + '-summary.json',
                     'creator_index_file': stem + '-creators.json',
                     'cards_file': stem + '-cards-v2.json', 'cards_dir': card_directory,
                     'connections_dir': directory,
                     'connections_metadata_file': stem + '-connections-metadata.json'}
        topic_directory = stem + '-profile-topics'
        filenames['profile_topics_dir'] = topic_directory
        if source.resolve() in {(folder / name).resolve() for name in filenames.values()}:
            raise ValueError('I trasporti devono essere distinti dal dataset originale')
        for name in (directory, card_directory):
            (folder / name).mkdir(parents=True, exist_ok=True)
        from ._native import DashboardSummaryAccumulator
        summary = DashboardSummaryAccumulator(metadata['language'], False,
            metadata.get('creators', {}).get('status') not in ('collected', 'partially_collected')
            and metadata.get('history', {}).get('status') in ('collected', 'partially_collected'))
        summary_batch = []
        topic_database = sqlite3.connect(Path(topic_staging.name) / 'topics.sqlite')
        topic_database.execute('CREATE TABLE topics (page_id INTEGER PRIMARY KEY, status TEXT, labels TEXT, '
                               'position INTEGER NOT NULL, article TEXT NOT NULL)')
        orphans, summaries, shortlists, seen, article_preview = {}, {}, {}, set(), []

        def detail(identifier, record):
            write_json(folder / directory / (identifier + '.json'),
                       {'format': 'connections_v2', 'connection_identity': connection_identity,
                        'census_page_count': len(seen), 'pages': [orphans[identifier]],
                        'link_candidates': {identifier: record}})

        def card(identifier, record):
            write_json(folder / card_directory / (identifier + '.json'),
                       {'format': 'orphan_card_v1', 'card_identity': card_identity,
                        'census_page_count': len(seen), 'page': orphans[identifier],
                        'candidate_record': record})

        def pages():
            for section, identifier, record in events:
                if section == 'pages':
                    pid = record.get('page_id')
                    if type(pid) is not int or pid <= 0 or pid in seen or type(record.get('orphan')) is not bool:
                        raise ValueError('Osservazione o identificativo duplicato non valido')
                    seen.add(pid)
                    topic_database.execute('INSERT INTO topics VALUES (?, ?, ?, ?, ?)',
                        (pid, record.get('topic_status'), json.dumps(record.get('topic_labels'), ensure_ascii=False),
                         len(seen) - 1, json.dumps([record.get(field) for field in ARTICLE_INDEX_COLUMNS], ensure_ascii=False)))
                    add_article_preview(article_preview, record)
                    summary_batch.append(_summary_page(record))
                    if len(summary_batch) == 512:
                        summary.add_batch(json.dumps(summary_batch, ensure_ascii=False))
                        summary_batch.clear()
                    if record['orphan']:
                        orphans[str(pid)] = record
                    yield record
                elif section == 'link_candidates':
                    if identifier not in orphans or identifier in summaries:
                        raise ValueError('Candidate non corrispondenti a una voce orfana')
                    candidate_summary = {'status': record.get('status'),
                                         'candidate_count': record.get('candidate_count')}
                    summaries[identifier] = candidate_summary
                    selected, group_sizes, selected_ids = [], [0, 0], set()
                    for candidate in record.get('candidates', []):
                        pid, title = candidate.get('page_id'), candidate.get('title')
                        group = 0 if candidate.get('dead_end') is True else 1
                        if (group_sizes[group] >= 5 or type(pid) is not int or pid <= 0 or
                                pid == int(identifier) or pid in selected_ids or not isinstance(title, str) or not title.strip()):
                            continue
                        reference = next((edge for edge in candidate.get('evidence', [])
                                          if isinstance(edge.get('language'), str) and
                                          re.fullmatch(r'[a-z][a-z0-9-]{0,19}', edge['language']) and
                                          edge['language'] not in ('all', metadata['language']) and
                                          all(isinstance(edge.get(key), str) and edge[key].strip()
                                              for key in ('source_title', 'target_title'))), None)
                        if reference is not None:
                            selected.append({**candidate, 'evidence': [reference]})
                            group_sizes[group] += 1
                            selected_ids.add(pid)
                    shortlists[identifier] = {**candidate_summary,
                                             'candidate_ids': [candidate['page_id'] for candidate in selected]}
                    card(identifier, {**candidate_summary, 'candidates': selected})
                    detail(identifier, record)
            if (source.stat().st_size, source.stat().st_mtime_ns) != (initial.st_size, initial.st_mtime_ns):
                raise ValueError('Esportazione modificata durante la conversione')

        write_observations(folder / filenames['observations_file'],
                           {'metadata': metadata, 'pages': pages(), 'link_candidates': summaries})
        topic_shards, topic_rows, topic_bucket = [], [], None

        def write_topic_shard():
            if topic_bucket is not None:
                write_json(folder / topic_directory / f'{topic_bucket}.json',
                           {'format': 'profile_topics_shard_v1', 'metadata': metadata,
                            'bucket': topic_bucket, 'rows': topic_rows})
                topic_shards.append(topic_bucket)

        for pid, status, labels in topic_database.execute('SELECT page_id, status, labels FROM topics ORDER BY page_id'):
            bucket = pid // TOPIC_LOOKUP_SHARD_SIZE
            if bucket != topic_bucket:
                write_topic_shard()
                topic_rows, topic_bucket = [], bucket
            topic_rows.append([pid, status, json.loads(labels)])
        write_topic_shard()
        write_json(folder / topic_directory / 'index.json',
                   {'format': 'profile_topics_index_v1', 'metadata': metadata,
                    'shard_size': TOPIC_LOOKUP_SHARD_SIZE, 'shards': topic_shards,
                    'census_page_count': len(seen)})
        if summary_batch:
            summary.add_batch(json.dumps(summary_batch, ensure_ascii=False))
        summary_count, summary_json = summary.finish()
        if summary_count != len(seen):
            raise ValueError('Riepilogo del censimento incompleto')
        write_json(folder / filenames['connections_metadata_file'],
                   {'format': 'connections_metadata_v1', 'connection_identity': connection_identity,
                    'metadata': metadata, 'census_page_count': len(seen)})
        write_json(folder / filenames['articles_file'],
                   {'format': 'article_index_v1', 'metadata': metadata, 'census_page_count': len(seen),
                    'columns': ARTICLE_INDEX_COLUMNS, 'rows': None, 'link_candidates': None},
                   arrays={'rows': (json.loads(row[0]) for row in
                                   topic_database.execute('SELECT article FROM topics ORDER BY position'))},
                   mappings={'link_candidates': summaries.items()})
        write_summary_files(folder / filenames['summary_file'],
                            folder / filenames['creator_index_file'], metadata,
                            summary_count, summary_json,
                            [{'language': metadata['language'], 'file': source.name,
                              'size': initial.st_size, 'mtime_ns': initial.st_mtime_ns}],
                            [page for _, page in article_preview])
        for identifier in orphans.keys() - summaries.keys():
            detail(identifier, {})
            card(identifier, None)
        write_json(folder / filenames['cards_file'],
                   {'format': 'orphan_cards_v2', 'metadata': metadata, 'census_page_count': len(seen),
                    'card_identity': card_identity, 'columns': ARTICLE_INDEX_COLUMNS,
                    'rows': None, 'link_candidates': None},
                   arrays={'rows': ([page.get(field) for field in ARTICLE_INDEX_COLUMNS] for page in orphans.values())},
                   mappings={'link_candidates': shortlists.items()})
        phase_counts(rows=len(seen), language=metadata['language'], input_bytes=initial.st_size)
        return filenames
    finally:
        events.close()
        if topic_database is not None:
            topic_database.close()
        topic_staging.cleanup()


def write_summary_files(summary_path, creators_path, metadata, count, report_json, sources, article_preview):
    reports = json.loads(report_json)
    creators = reports.pop('creators')
    # The category chart renders its first 20 ranked bars. The full category
    # census remains in the canonical/observation exports for the explorer.
    reports['categories'] = reports['categories'][:20]
    shared = {'metadata': metadata, 'sources': sources, 'census_page_count': count}
    write_json(summary_path, {'format': 'dashboard_summary_v1', 'histogram_rule': HISTOGRAM_RULE,
                             'measures_rule': metadata.get('measures_rule', MEASURES_RULE),
                              **shared, 'reports': reports, 'article_preview': article_preview})
    write_json(creators_path, {'format': 'creator_index_v1', **shared, 'creators': creators})


@operational_phase('export.aggregate_summary')
def write_aggregate_summary(members, output, metadata, aggregate=True):
    """Prepare exact Rust chart reports from canonical page streams once.

    The largest graph remains on disk; only chart fields enter the native
    accumulator. Candidate evidence is not parsed or duplicated.
    """
    from ._native import DashboardSummaryAccumulator
    metadata = dict(metadata)
    if aggregate:
        collected_creators = [member['metadata']['creators'] for member in members
                              if member['metadata'].get('creators', {}).get('status') in
                              ('collected', 'partially_collected')]
        metadata['creators'] = {
            'status': ('collected' if len(collected_creators) == len(members) else
                       'partially_collected' if collected_creators else 'not_collected'),
            'languages_scanned': [member['language'] for member in members
                                  if member['metadata'].get('creators', {}).get('status') in
                                  ('collected', 'partially_collected')],
            'collected_languages': len(collected_creators),
        }
        collected_history = [member['metadata']['history'] for member in members
                             if member['metadata'].get('history', {}).get('status') in
                             ('collected', 'partially_collected')]
        metadata['history'] = {
            'status': ('collected' if len(collected_history) == len(members) else
                       'partially_collected' if collected_history else 'not_collected'),
            'matched_pages': sum(item.get('matched_pages', 0) for item in collected_history),
        }
    historical = all(member['metadata'].get('creators', {}).get('status') not in
                     ('collected', 'partially_collected') for member in members)
    historical = historical and any(member['metadata'].get('history', {}).get('status') in
                                    ('collected', 'partially_collected') for member in members)
    summary = DashboardSummaryAccumulator(metadata['language'], aggregate, historical)
    sources, article_preview = [], []
    for member in members:
        path = Path(member['path'])
        initial = path.stat()
        sources.append({'language': member['language'], 'file': path.name,
                        'size': initial.st_size, 'mtime_ns': initial.st_mtime_ns})
        events = iter_export_sections(path)
        count, batch = 0, []
        try:
            first = next(events)
            if first[0] != 'metadata' or first[2] != member['metadata']:
                raise ValueError('Metadati del riepilogo aggregato non corrispondenti')
            for section, _, record in events:
                if section != 'pages':
                    break
                page = _summary_page(record, member['language'] if aggregate else None)
                add_article_preview(article_preview, record, member['language'] if aggregate else None)
                batch.append(page)
                count += 1
                if len(batch) == 512:
                    summary.add_batch(json.dumps(batch, ensure_ascii=False))
                    batch.clear()
            if batch:
                summary.add_batch(json.dumps(batch, ensure_ascii=False))
        finally:
            events.close()
        current = path.stat()
        if (count != member['page_count'] or
                (current.st_size, current.st_mtime_ns) != (initial.st_size, initial.st_mtime_ns)):
            raise ValueError('Censimento modificato o incompleto durante il riepilogo aggregato')
    total, report_json = summary.finish()
    if total != sum(member['page_count'] for member in members):
        raise ValueError('Popolazione del riepilogo aggregato incompleta')
    creators_output = Path(output).with_name(Path(output).name.replace('-summary.json', '-creators.json'))
    if creators_output == Path(output):
        raise ValueError('Il file del riepilogo deve terminare con -summary.json')
    write_summary_files(output, creators_output, metadata, total, report_json, sources,
                        [page for _, page in article_preview])
    phase_counts(rows=total, languages=len(members))
    if aggregate:
        return {'metadata': metadata, 'count': total, 'reports': report_json,
                'sources': sources, 'article_preview': [page for _, page in article_preview]}


def iter_export_sections(path):
    """Read our ordered JSON exports without materializing the complete file.

    Yield metadata, individual articles and individual orphan records. The
    decoder retains at most its buffer and the largest individual record;
    geometrically increasing reads avoid repeatedly parsing tiny increments
    of a large orphan's evidence. This is not a parser for arbitrary JSON.
    """
    decoder = json.JSONDecoder()
    with Path(path).open(encoding='utf-8') as stream:
        buffer, position = '', 0

        def read(size=65536):
            nonlocal buffer, position
            chunk = stream.read(size)
            buffer, position = buffer[position:] + chunk, 0
            return bool(chunk)

        def peek():
            nonlocal position
            while True:
                while position < len(buffer) and buffer[position].isspace():
                    position += 1
                if position < len(buffer):
                    return buffer[position]
                if not read():
                    return ''

        def take(token):
            nonlocal position
            if peek() != token:
                raise ValueError('Esportazione JSON incompleta o non valida')
            position += 1

        def value():
            nonlocal position
            peek()
            size = 65536
            while True:
                try:
                    result, end = decoder.raw_decode(buffer, position)
                    position = end
                    return result
                except json.JSONDecodeError as error:
                    if not read(size):
                        raise ValueError('Esportazione JSON incompleta o non valida') from error
                    size = min(size * 2, 16777216)

        take('{')
        for section in ('metadata', 'pages', 'link_candidates'):
            if section == 'link_candidates' and peek() == '}':
                break
            if section != 'metadata':
                take(',')
            if value() != section:
                raise ValueError('Ordine delle sezioni JSON non supportato; rigenerare con analyze o compare')
            take(':')
            if section == 'metadata':
                metadata = value()
                if not isinstance(metadata, dict):
                    raise ValueError('Metadati dell’esportazione non validi')
                yield section, None, metadata
            else:
                opening, closing = ('[', ']') if section == 'pages' else ('{', '}')
                take(opening)
                first = True
                while peek() != closing:
                    if not first:
                        take(',')
                    first = False
                    key = None
                    if section == 'link_candidates':
                        key = value()
                        if not isinstance(key, str):
                            raise ValueError('ID delle candidate non valido')
                        take(':')
                    record = value()
                    if not isinstance(record, dict):
                        raise ValueError('Osservazione JSON non valida')
                    yield section, key, record
                take(closing)
        take('}')
        if peek():
            raise ValueError('Contenuto inatteso dopo l’esportazione JSON')


def write_json(path, value, arrays=None, embedded_paths=None, mappings=None):
    """Stream to a temporary file, publishing only after successful serialization."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.tmp')
    created = False
    try:
        with temporary.open('w', encoding='utf-8') as stream:
            created = True
            record_encoder = json.JSONEncoder(ensure_ascii=False, allow_nan=False)
            if not arrays and not embedded_paths and not mappings:
                if (isinstance(value, dict) and value.get('format') in ('connections_v1', 'connections_v2')
                        and isinstance(value.get('link_candidates'), dict)):
                    stream.writelines(_detail_chunks(value, record_encoder))
                else:
                    stream.writelines(_json_chunks(value))
            else:
                arrays, embedded_paths, mappings = arrays or {}, embedded_paths or {}, mappings or {}
                sections = list(arrays) + list(embedded_paths) + list(mappings)
                if len(sections) != len(set(sections)) or set(sections) - set(value):
                    raise ValueError('Sezioni JSON in streaming non valide')
                stream.write('{')
                for position, (key, item) in enumerate(value.items()):
                    if position:
                        stream.write(',')
                    stream.writelines(_json_chunks(key))
                    stream.write(':')
                    if key in arrays:
                        stream.write('[')
                        for index, record in enumerate(arrays[key]):
                            if index:
                                stream.write(',')
                            stream.writelines(_json_record_chunks(record, record_encoder))
                        stream.write(']')
                    elif key in mappings:
                        stream.write('{')
                        seen = set()
                        for index, (identifier, record) in enumerate(mappings[key]):
                            if not isinstance(identifier, str) or identifier in seen:
                                raise ValueError('Chiave JSON in streaming non valida')
                            seen.add(identifier)
                            if index:
                                stream.write(',')
                            stream.writelines(_json_chunks(identifier))
                            stream.write(':')
                            stream.writelines(_json_record_chunks(record, record_encoder))
                        stream.write('}')
                    elif key in embedded_paths:
                        # Only embed a completed checkpoint produced by this collector.
                        with Path(embedded_paths[key]).open(encoding='utf-8') as source:
                            while chunk := source.read(65536):
                                stream.write(chunk)
                    else:
                        stream.writelines(_json_chunks(item))
                stream.write('}')
        temporary.replace(path)
    finally:
        if created:
            temporary.unlink(missing_ok=True)


def initialize_creator_rows(rows):
    """Keep unsupported creator measures explicitly missing in dump observations."""
    fields = (
        'creator_key', 'creator_attribution', 'creator_account_type', 'creator_is_bot_now',
        'creator_account_type_at_creation', 'creator_registered_at', 'creator_tenure_days',
        'creator_tenure_status', 'creator_prior_edits_main', 'creator_prior_edits_other',
        'creator_prior_articles', 'creator_articles_created_total',
        'creator_languages_created', 'creator_languages_before', 'origin', 'creation_tags',
        'import_log_observed', 'move_log_observed', 'creator_experience_status',
    )
    for row in rows:
        row.update({field: None for field in fields})
        row['origin'] = 'unknown'
        row['creator_data_status'] = 'not_collected'
        row['creator_orphans_at_30_days'] = None
        row['creator_orphans_at_30_days_status'] = 'unavailable_no_historical_link_graph'
        row.update(creator_articles_in_census=None, creator_orphans_current=None,
                   creator_orphans_current_share=None)
    return {'status': 'not_collected', 'languages_scanned': [], 'matched_pages': 0,
            'unmatched_pages': len(rows),
            'orphan_30d_status': 'unavailable_no_historical_link_graph'}


def attach_history(rows, graph_metadata, supplement):
    """Join separately labelled dump history without rewriting creation dates."""
    if supplement is None:
        _join_history_rows(rows, {}, missing_status='not_collected')
        return {'status': 'not_collected', 'matched_pages': 0}
    meta = supplement.get('metadata', {})
    database = wiki_database(graph_metadata['language'], graph_metadata.get('database'))
    if (meta.get('language') != graph_metadata['language'] or meta.get('database') != database
            or meta.get('source') != 'MediaWiki History dumps / ' + database
            or meta.get('complete') is not True
            or not isinstance(meta.get('history_release'), str)):
        raise ValueError('Supplemento MediaWiki History non valido per questa wiki')
    if (graph_metadata.get('source') == 'Wikimedia SQL dumps / ' + database
            and graph_metadata.get('sql_release') != meta.get('sql_release')):
        raise ValueError('Release SQL del grafo e della cronologia diverse')
    by_id = {record['page_id']: record for record in supplement.get('pages', [])}
    if len(by_id) != len(supplement.get('pages', [])):
        raise ValueError('Identificativi duplicati nella cronologia')
    matched = _join_history_rows(rows, by_id)
    return {**meta, 'status': 'collected', 'matched_pages': matched,
            'unmatched_pages': len(rows) - matched,
            'input_sha256': json_sha256(supplement)}


def _join_history_rows(rows, by_id, missing_status='missing_page'):
    """Apply one verified history mapping to a bounded observation batch."""
    fields = ('history_first_revision_at', 'history_creator_key', 'history_creator_status',
              'creator_groups_at_creation_history', 'creator_continuity_months_before_creation',
              'creator_continuity_bucket_before_creation', 'creator_prior_edits_main_history',
              'creator_prior_edits_other_history')
    for row in rows:
        row.update({field: None for field in fields})
        row['history_data_status'] = missing_status
    matched = 0
    for row in rows:
        record = by_id.get(row['page_id'])
        if record is None:
            continue
        if 'page_created_at' in record:
            row.update(page_created_at=record['page_created_at'], created_at=record['page_created_at'],
                       creation_date_source='MediaWiki History / page_creation_timestamp',
                       creation_date_status=record.get('creation_date_status', 'missing_history_creation'))
        historical_date = record.get('article_created_at')
        if not historical_date:
            row['history_data_status'] = 'history_initial_revision_unknown'
            continue
        row['history_first_revision_at'] = historical_date
        row['history_creator_status'] = record.get('creator_attribution')
        if record.get('creator_history_status') != 'observed_surviving_history':
            row['history_data_status'] = 'unattributed_history'
            continue
        row.update(history_creator_key=record.get('creator_key'),
                   creator_registered_at=record.get('creator_registered_at'),
                   creator_tenure_days=record.get('creator_tenure_days'),
                   creator_tenure_status=('local_registration_to_reconstructed_creation'
                                         if record.get('creator_tenure_days') is not None else 'missing_or_inconsistent_registration'),
                   creator_prior_articles=record.get('creator_prior_articles'),
                   creator_account_type=record.get('creator_account_type'),
                   creator_is_bot_now=record.get('creator_is_bot_now'),
                   creator_groups_at_creation_history=record.get('creator_groups_at_creation'),
                   creator_continuity_months_before_creation=record.get('creator_continuity_months_before_creation'),
                   creator_continuity_bucket_before_creation=record.get('creator_continuity_bucket_before_creation'),
                   creator_prior_edits_main_history=record.get('creator_prior_edits_main_history'),
                   creator_prior_edits_other_history=record.get('creator_prior_edits_other_history'),
                   history_data_status='matched_reconstructed_history')
        matched += 1
    return matched
