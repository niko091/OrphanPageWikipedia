import bisect
import hashlib
import json
import multiprocessing
import re
import sqlite3
import tempfile
import time
from collections.abc import Mapping
from contextlib import closing, contextmanager
from datetime import datetime, timezone
from pathlib import Path

HISTOGRAM_RULE = 'shared_p2_p98_tails_v1'
MEASURES_RULE = 'creation_quality_creators_v2'
ARTICLE_PREVIEW_LIMIT = 25
TOPIC_LOOKUP_SHARD_SIZE = 1024


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
            and summary.get('measures_rule') == MEASURES_RULE
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
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    if digest.hexdigest() != spec['model_sha512']:
        raise ValueError('Checksum dei pesi del modello non corrispondente')
    try:
        import fasttext
    except ImportError as error:
        raise RuntimeError('Installare il supporto locale: pip install "fasttext==0.9.3" "numpy>=1.26,<3"') from error
    model = fasttext.load_model(str(path))
    actual = {label.removeprefix('__label__').replace('_', ' ') for label in model.get_labels()}
    if actual != set(labels):
        raise ValueError('Il modello non contiene la tassonomia completa prevista')
    return model, spec


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
        CREATE TABLE candidates(language TEXT, id INTEGER, data TEXT,
            PRIMARY KEY(language,id)) WITHOUT ROWID;''')


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


def _stage_comparison(connection, result):
    """Persist complete observations and only edges eligible for item matching.

    Native degrees still use every article/edge. Ambiguous/missing QIDs cannot
    support a cross-language edge and are excluded only from this lookup index.
    """
    language = result['metadata']['language']
    if connection.execute('SELECT 1 FROM wikis WHERE language=?', (language,)).fetchone():
        raise ValueError('Una sola raccolta per lingua nel confronto')
    pages = result['pages']
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


def _comparison_candidates(connection, language, page_id, item):
    status = 'compared'
    if connection.execute('SELECT 1 FROM ambiguous WHERE language=? AND item=?', (language, item)).fetchone():
        status = 'ambiguous_item'
    elif not connection.execute('SELECT 1 FROM items WHERE language=? AND item=?', (language, item)).fetchone():
        status = 'missing_item'
    candidates, counterparts = [], []
    if status == 'compared':
        for other, pid, title, incoming, ambiguous in connection.execute('''
            SELECT w.language,p.id,p.title,p.incoming,a.item FROM wikis w
            LEFT JOIN items i ON i.language=w.language AND i.item=?
            LEFT JOIN pages p ON p.language=i.language AND p.id=i.id
            LEFT JOIN ambiguous a ON a.language=w.language AND a.item=?
            WHERE w.language!=? ORDER BY w.language''', (item, item, language)):
            if pid is None:
                counterparts.append({'language': other, 'status': 'ambiguous_item' if ambiguous else 'missing_counterpart'})
            else:
                counterparts.append({'language': other, 'status': 'orphan' if incoming == 0 else 'non_orphan',
                                     'page_id': pid, 'title': title, 'in_degree': incoming})
        # Keep the target-edge lookup ahead of source-item joins for each orphan.
        edges = connection.execute('''
            SELECT lp.id,lp.title,lp.outgoing,bt.language,bp.id,bp.title,bt.id,bt.title
            FROM items ti CROSS JOIN edges e ON e.language=ti.language AND e.target=ti.id
            CROSS JOIN items si ON si.language=e.language AND si.id=e.source
            CROSS JOIN items li ON li.language=? AND li.item=si.item
            CROSS JOIN pages lp ON lp.language=li.language AND lp.id=li.id
            CROSS JOIN pages bp ON bp.language=si.language AND bp.id=si.id
            CROSS JOIN pages bt ON bt.language=ti.language AND bt.id=ti.id
            WHERE ti.item=? AND ti.language!=? AND li.id!=?
            ORDER BY ti.language,bp.id''', (language, item, language, page_id))
        from ._native import rank_candidates
        candidates = [{'page_id': pid, 'title': title, 'out_degree': outgoing,
                       'dead_end': outgoing == 0,
                       'evidence': [dict(zip(('language', 'source_page_id', 'source_title', 'target_page_id', 'target_title'), edge)) for edge in evidence]}
                      for pid, title, outgoing, evidence in rank_candidates(list(edges))]
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


def _finish_comparison(connection):
    """Same indexed matching for file and in-memory callers; one orphan at a time."""
    # The edge stream is appended while each wiki is analyzed. Build its
    # lookup index once, after all inserts, to avoid random B-tree writes for
    # hundreds of millions of links in a large SQL collection.
    connection.execute('CREATE UNIQUE INDEX IF NOT EXISTS edges_by_target '
                       'ON edges(language,target,source)')
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
        with connection:
            for pid, item in connection.execute('SELECT id,item FROM pages WHERE language=? AND incoming=0 ORDER BY id', (language,)):
                record = _comparison_candidates(connection, language, pid, item)
                connection.execute('INSERT INTO candidates VALUES(?,?,?)', (language, pid, json.dumps(record, ensure_ascii=False)))
                processed += 1
                if processed % 1000 == 0:
                    print(f'Confronto {language}: {processed} orfane; tutte le lingue di riferimento', flush=True)
        meta['comparison'] = {'rule': 'cross_language_incoming_v1', 'status': 'compared',
            'reference_languages': [other for other, _, _ in wikis if other != language],
            'collections': provenance, 'matching_source': 'local page_props.wikibase_item from SQL dumps',
            'ambiguous_items_excluded': ambiguous, 'mapped_articles': mapped,
            'temporal_consistency': 'independent_source_specific_intervals',
            'interpretation': 'candidates for manual review, not validated recommendations'}
        results[language] = {'metadata': meta, 'pages': _StagedPages(connection, language, count),
                             'link_candidates': _StagedCandidates(connection, language)}
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


def _comparison_file_worker(path, staging, rules, topics, histories, qualities, sender):
    """A sequential spawned worker releases all graph/JSON/native allocations."""
    try:
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


@contextmanager
def compare_files(paths, rules=None, topics=None, work_dir=None, histories=None, qualities=None):
    """Sequential comparisons with verified SQL staging for large imported wikis.

    Each worker streams matchable edges, article observations and candidates to
    SQLite. Large imported editions retain titles and redirect mappings in RAM,
    but never materialize all raw pages, revisions or source adjacency together.
    Results must be exported within this context, before closing the database.
    """
    if len(paths) < 2:
        raise ValueError('Servono almeno due raccolte complete di lingue diverse')
    def supplements(files):
        result = {}
        for path in files or []:
            from .dumps import _read_first_metadata
            language = _read_first_metadata(path)['language']
            if language in result:
                raise ValueError('Un solo supplemento per lingua')
            result[language] = str(path)
        return result
    topics, histories, qualities = supplements(topics), supplements(histories), supplements(qualities)
    context = multiprocessing.get_context('spawn')
    with tempfile.TemporaryDirectory(prefix='orphanwiki-compare-', dir=work_dir) as folder:
        staging = str(Path(folder) / 'comparison.sqlite')
        with closing(sqlite3.connect(staging)) as connection:
            _comparison_schema(connection)
            languages = set()
            for position, path in enumerate(paths, 1):
                receiver, sender = context.Pipe(duplex=False)
                worker = context.Process(target=_comparison_file_worker,
                    args=(str(path), staging, rules, topics, histories, qualities, sender))
                try:
                    worker.start()
                    sender.close()
                    try:
                        response = receiver.recv()
                    except EOFError as error:
                        worker.join()
                        raise RuntimeError(f'Analisi terminata senza risultato: {path}; exit {worker.exitcode}') from error
                    worker.join()
                    if 'error' in response:
                        exception = ValueError if response['type'] == 'ValueError' else RuntimeError
                        raise exception(f'{path}: {response["error"]}')
                    if worker.exitcode != 0:
                        raise RuntimeError(f'Analisi fallita: {path}; exit {worker.exitcode}')
                    languages.add(response['language'])
                    peak = response['peak_rss']
                    memory = f'{peak:.1f} MiB' if peak is not None else 'non disponibile'
                    print(f'Indice pronto {position}/{len(paths)}: {response["language"]}; picco RSS worker {memory}', flush=True)
                finally:
                    if worker.is_alive():
                        worker.terminate()
                        worker.join()
                    receiver.close()
                    sender.close()
                    worker.close()
            if (set(topics) | set(histories) | set(qualities)) - languages:
                raise ValueError('Supplemento senza la corrispondente raccolta')
            yield _finish_comparison(connection)


def analyze(raw, rules=None, include_sources=False, topics=None, history=None, quality=None):
    from .categories import classify
    language = raw['metadata']['language']
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
            or type(meta.get('scored_pages')) is not int or not 0 <= meta['scored_pages'] <= count
            or meta.get('missing_pages') != count - meta['scored_pages']
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
        if (status not in ('scored', 'missing_current_page', 'revision_mismatch', 'missing_wikitext')
                or (status == 'scored' and (type(score) not in (float, int) or not 0 <= score <= 0.998001))
                or (status != 'scored' and score is not None)):
            raise ValueError('Punteggio qualità non valido')
        row.update(quality_score=score, quality_status=status)


def attach_quality(rows, graph, supplement):
    meta = quality_metadata(supplement.get('metadata', {}), graph, len(rows))
    records = supplement.get('pages', [])
    by_id = {record['page_id']: record for record in records}
    if len(by_id) != len(records) or set(by_id) != {row['page_id'] for row in rows}:
        raise ValueError('Censimento qualità incompleto o duplicato')
    _join_quality_rows(rows, by_id)
    if sum(row['quality_status'] == 'scored' for row in rows) != meta.get('scored_pages'):
        raise ValueError('Copertura qualità incoerente')
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


def json_sha256(value):
    """Hash sorted JSON in bounded chunks without allocating the complete document."""
    digest = hashlib.sha256()
    for chunk in _json_chunks(value, ensure_ascii=True, sort_keys=True):
        digest.update(chunk.encode('utf-8'))
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


def write_dashboard_exports(source, folder, expected=None):
    """Project an ordered canonical export into bounded browser transports.

    Cards contain all orphans, exact counts and a small prompt shortlist. A
    separate record per orphan retains every candidate and reference edge for
    the detail page. Read one canonical record at a time; never modify it.
    Canonical candidate order is native dead-end/title/ID order. The overview
    retains the first five eligible candidates from each dead-end group so UI
    ranking can choose its five without loading every reference edge.
    """
    source, folder = Path(source), Path(folder)
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
        generation = json_sha256([metadata, initial.st_size, initial.st_mtime_ns])[:12]
        directory = source.stem + '-connections-' + generation
        filenames = {'observations_file': source.stem + '-observations.json',
                     'summary_file': source.stem + '-summary.json',
                     'creator_index_file': source.stem + '-creators.json',
                     'cards_file': source.stem + '-cards.json', 'connections_dir': directory}
        topic_directory = source.stem + '-profile-topics-' + generation
        filenames['profile_topics_dir'] = topic_directory
        if source.resolve() in {(folder / name).resolve() for name in filenames.values()}:
            raise ValueError('I trasporti devono essere distinti dal dataset originale')
        from ._native import DashboardSummaryAccumulator
        summary = DashboardSummaryAccumulator(metadata['language'], False,
            metadata.get('creators', {}).get('status') not in ('collected', 'partially_collected')
            and metadata.get('history', {}).get('status') in ('collected', 'partially_collected'))
        summary_batch = []
        topic_database = sqlite3.connect(Path(topic_staging.name) / 'topics.sqlite')
        topic_database.execute('CREATE TABLE topics (page_id INTEGER PRIMARY KEY, status TEXT, labels TEXT)')
        orphans, summaries, shortlists, seen, article_preview = {}, {}, {}, set(), []

        def detail(identifier, record):
            write_json(folder / directory / (identifier + '.json'),
                       {'format': 'connections_v1', 'metadata': metadata,
                        'census_page_count': len(seen), 'pages': [orphans[identifier]],
                        'link_candidates': {identifier: record}})

        def pages():
            for section, identifier, record in events:
                if section == 'pages':
                    pid = record.get('page_id')
                    if type(pid) is not int or pid <= 0 or pid in seen or type(record.get('orphan')) is not bool:
                        raise ValueError('Osservazione o identificativo duplicato non valido')
                    seen.add(pid)
                    topic_database.execute('INSERT INTO topics VALUES (?, ?, ?)',
                        (pid, record.get('topic_status'), json.dumps(record.get('topic_labels'), ensure_ascii=False)))
                    add_article_preview(article_preview, record)
                    summary_batch.append(record)
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
                    shortlists[identifier] = {**candidate_summary, 'candidates': selected}
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
        write_summary_files(folder / filenames['summary_file'],
                            folder / filenames['creator_index_file'], metadata,
                            summary_count, summary_json,
                            [{'language': metadata['language'], 'file': source.name,
                              'size': initial.st_size, 'mtime_ns': initial.st_mtime_ns}],
                            [page for _, page in article_preview])
        for identifier in orphans.keys() - summaries.keys():
            detail(identifier, {})
        write_json(folder / filenames['cards_file'],
                   {'format': 'orphan_cards_v1', 'metadata': metadata, 'census_page_count': len(seen),
                    'pages': None, 'link_candidates': None}, arrays={'pages': orphans.values()},
                   mappings={'link_candidates': shortlists.items()})
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
                             'measures_rule': MEASURES_RULE,
                              **shared, 'reports': reports, 'article_preview': article_preview})
    write_json(creators_path, {'format': 'creator_index_v1', **shared, 'creators': creators})


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
                page = {**record, 'language': member['language']} if aggregate else record
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
            if not arrays and not embedded_paths and not mappings:
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
                            stream.writelines(_json_chunks(record))
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
                            stream.writelines(_json_chunks(record))
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
