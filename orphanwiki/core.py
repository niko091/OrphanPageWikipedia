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


def replica_database(language, database=None):
    """Resolve a Wikipedia replica name, permitting explicit historical names."""
    if not isinstance(language, str) or not re.fullmatch(r'[a-z][a-z0-9-]{0,19}', language) or language == 'all':
        raise ValueError('Codice lingua non valido')
    database = language.replace('-', '_') + 'wiki' if database is None else database
    if not isinstance(database, str) or not re.fullmatch(r'[a-z][a-z0-9_]{0,40}wiki', database):
        raise ValueError('Nome database Wikipedia non valido (senza suffisso _p)')
    return database


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
    """One native graph call, then bounded fastText batches over PAWS QIDs.

    O(V+E+R) graph work and O(V+E) extra adjacency/sets. The model is loaded
    once outside this function. No text, API enrichment or downloaded article
    predictions are used. Empty and entirely out-of-vocabulary inputs remain
    missing, rather than receiving prior-only predictions.
    """
    if (raw['metadata'].get('complete') is not True or
            raw['metadata'].get('item_mapping') != {'source': 'page_props.wikibase_item', 'complete': True}):
        raise ValueError('I temi richiedono grafo e identificativi PAWS completi')
    database = replica_database(raw['metadata']['language'], raw['metadata'].get('database'))
    if not raw['metadata'].get('demo') and raw['metadata'].get('source') != 'Wiki Replicas / ' + database:
        raise ValueError('Usare esclusivamente censimenti PAWS per il modello')
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
    return {'metadata': {'language': raw['metadata']['language'], 'source': 'local_model_on_PAWS_replica_outlinks',
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
            meta.get('source') != 'local_model_on_PAWS_replica_outlinks' or
            bool(meta.get('demo')) != bool(raw['metadata'].get('demo')) or
            meta.get('input_sha256') != json_sha256(raw)):
        raise ValueError('Supplemento dei temi non corrispondente al grafo PAWS')
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
        replica_database(language, meta.get('database'))
        if meta['language'] != language or meta.get('complete') is not True:
            raise ValueError('L’aggregato richiede censimenti completi, uno per lingua')
        if not re.fullmatch(r'[a-zA-Z0-9_.-]+\.json', files[language]):
            raise ValueError('Nome dataset non valido')
        members.append({'language': language, 'file': files[language],
                        'page_count': len(result['pages']), 'metadata': meta})
    if len({bool(m['metadata'].get('demo')) for m in members}) != 1:
        raise ValueError('Non mescolare dati sintetici e dati reali')
    age_rules = {m['metadata'].get('age_rule') for m in members}
    from ._native import collection_interval
    start, finish = collection_interval([(m['metadata']['started_at'], m['metadata']['finished_at']) for m in members])
    return {'metadata': {
        'kind': 'aggregate', 'language': 'all', 'languages': sorted(results),
        'complete': True, 'demo': bool(members[0]['metadata'].get('demo')),
        'source': 'Wiki Replicas / PAWS (aggregate of local censuses)',
        'started_at': start,
        'finished_at': finish,
        'temporal_consistency': 'independent_live_collection_intervals',
        'observation_unit': 'language_page',
        'age_rule': next(iter(age_rules)) if len(age_rules) == 1 else 'mixed_per_language',
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
        CREATE TABLE edges(language TEXT, target INTEGER, source INTEGER,
            PRIMARY KEY(language,target,source)) WITHOUT ROWID;
        CREATE TABLE candidates(language TEXT, id INTEGER, data TEXT,
            PRIMARY KEY(language,id)) WITHOUT ROWID;''')


def _comparison_analysis(raw, rules, creators, topics):
    meta = raw['metadata']
    language = meta['language']
    database = replica_database(language, meta.get('database'))
    if not meta.get('demo') and meta.get('source') != 'Wiki Replicas / ' + database:
        raise ValueError('Usare esclusivamente raccolte Wiki Replicas tramite PAWS')
    if meta.get('item_mapping') != {'source': 'page_props.wikibase_item', 'complete': True}:
        raise ValueError(f'{language}: identificativi interlingua non raccolti; ripetere collect con il codice aggiornato')
    result = analyze(raw, rules, creators, include_sources=True, topics=topics)
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
        # CROSS JOIN fixes the lookup order: find the target's incoming edges
        # before their source items. Otherwise SQLite may scan every item of a
        # reference wiki for each orphan, despite the available edge index.
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
            'collections': provenance, 'matching_source': 'local page_props.wikibase_item via PAWS',
            'ambiguous_items_excluded': ambiguous, 'mapped_articles': mapped,
            'temporal_consistency': 'independent_live_collection_intervals',
            'interpretation': 'candidates for manual review, not validated recommendations'}
        results[language] = {'metadata': meta, 'pages': _StagedPages(connection, language, count),
                             'link_candidates': _StagedCandidates(connection, language)}
        print(f'Confronto completato: {language}; {processed} orfane', flush=True)
    return results


def compare_languages(raws, rules=None, creators=None, topics=None):
    """Compatibility API for already-loaded graphs; file CLI uses bounded staging.

    Both paths share one matching algorithm. This API deliberately materializes
    its return value; use compare_files for large on-disk collections.
    """
    creators, topics = creators or {}, topics or {}
    languages = [r['metadata']['language'] for r in raws]
    if set(creators) - set(languages) or set(topics) - set(languages):
        raise ValueError('Supplemento senza la corrispondente raccolta')
    with closing(sqlite3.connect(':memory:')) as connection:
        _comparison_schema(connection)
        for raw in raws:
            language = raw['metadata']['language']
            _stage_comparison(connection, _comparison_analysis(raw, rules, creators.get(language), topics.get(language)))
        return {lang: {**result, 'pages': list(result['pages']), 'link_candidates': dict(result['link_candidates'].items())}
                for lang, result in _finish_comparison(connection).items()}


def _comparison_file_worker(path, staging, rules, creators, topics, sender):
    """A sequential spawned worker releases all graph/JSON/native allocations."""
    try:
        raw = json.loads(Path(path).read_text())
        language = raw['metadata']['language']
        print(f'Analisi integrale {language}: {len(raw["pages"])} voci', flush=True)
        creator = creators.get(language)
        checkpoint = Path(path).with_name(Path(path).stem.removesuffix('-raw') + '-creators.json')
        if creator is None and not raw.get('creator_data') and checkpoint.exists():
            creator = checkpoint
        topic = topics.get(language)
        checkpoint = Path(path).with_name(Path(path).stem.removesuffix('-raw') + '-topics.json')
        if topic is None and checkpoint.exists():
            topic = checkpoint
        extra = json.loads(Path(creator).read_text()) if creator is not None else None
        predictions = json.loads(Path(topic).read_text()) if topic is not None else None
        result = _comparison_analysis(raw, rules, extra, predictions)
        del raw, extra, predictions
        print(f'Grafo {language} risolto; salvataggio degli indici su disco', flush=True)
        with closing(sqlite3.connect(staging)) as connection:
            connection.executescript('PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;')
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
def compare_files(paths, rules=None, creators=None, topics=None, work_dir=None):
    """O(largest graph + largest orphan candidate list) RAM, SQLite disk staging.

    Every census is analyzed once in an isolated sequential worker. Resolved
    matchable edges, article observations and candidates are streamed to disk.
    Results must be exported within this context, before closing the database.
    """
    if len(paths) < 2:
        raise ValueError('Servono almeno due raccolte complete di lingue diverse')
    def supplements(files):
        result = {}
        for path in files or []:
            data = json.loads(Path(path).read_text())
            language = data['metadata']['language']
            if language in result:
                raise ValueError('Un solo supplemento per lingua')
            result[language] = str(path)
        return result
    creators, topics = supplements(creators), supplements(topics)
    context = multiprocessing.get_context('spawn')
    with tempfile.TemporaryDirectory(prefix='orphanwiki-compare-', dir=work_dir) as folder:
        staging = str(Path(folder) / 'comparison.sqlite')
        with closing(sqlite3.connect(staging)) as connection:
            _comparison_schema(connection)
            languages = set()
            for position, path in enumerate(paths, 1):
                receiver, sender = context.Pipe(duplex=False)
                worker = context.Process(target=_comparison_file_worker,
                    args=(str(path), staging, rules, creators, topics, sender))
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
            if set(creators) - languages or set(topics) - languages:
                raise ValueError('Supplemento senza la corrispondente raccolta')
            yield _finish_comparison(connection)


def analyze(raw, rules=None, creators=None, include_sources=False, topics=None):
    from .categories import classify
    if creators is None:
        creators = raw.get('creator_data')
    language = raw['metadata']['language']
    replica_database(language, raw['metadata'].get('database'))
    if 'biographies' in raw['metadata'] or any('biography' in p for p in raw['pages']):
        raise ValueError('Dataset arricchito da una fonte esterna: usare la raccolta originale della lingua ottenuta su PAWS.')
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
    creation_events = raw['metadata'].get('creation_date_rule') == 'creation_event_v1' or (creators or {}).get('metadata', {}).get('creation_date_rule') == 'creation_event_v1'
    rows = []
    for p, incoming, outgoing in zip(raw['pages'], ins, outs):
        created = p.get('page_created_at') if creation_events else p.get('created_at')
        rows.append({
            'page_id': p['page_id'], 'title': p['title'], 'revision_id': p.get('revision_id'),
            'wikibase_item': p.get('wikibase_item'),
            **choices[p['page_id']], 'categories': p.get('categories', []),
            'created_at': created, 'length_bytes': p.get('length_bytes'),
            'first_public_revision_at': p.get('first_public_revision_at', p.get('created_at')),
            'page_created_at': p.get('page_created_at'),
            'creation_date_source': p.get('creation_date_source'),
            'creation_date_status': p.get('creation_date_status', 'not_collected'),
            'in_degree': incoming, 'out_degree': outgoing,
            'orphan': incoming == 0,
        })
    creator_metadata = attach_creators(rows, raw['metadata'], creators)
    ages = article_ages(raw['metadata']['finished_at'],
                        [r['page_created_at'] if creation_events else r['created_at'] for r in rows])
    for row, (age, after_collection) in zip(rows, ages):
        row['created_at'] = row['page_created_at'] if creation_events else row['created_at']
        row['age_days'] = age
        if after_collection:
            row['creation_date_status'] = 'after_graph_collection'
    result = {'metadata': {**raw['metadata'], 'creators': creator_metadata, 'unresolved_target_occurrences': unresolved,
                         'category_rule': 'structural_specificity_v1', 'category_rules': rules or {}, 'engine': 'rust',
                         'age_rule': 'page_creation_event' if creation_events else 'first_public_revision_legacy',
                         'length_unit': 'UTF-8 wikitext bytes'}, 'pages': rows}
    if topics is not None:
        result['metadata']['topics'] = attach_topics(rows, raw, topics)
        result['metadata']['article_features'] = {**result['metadata'].get('article_features', {}), 'topics': 'local_model_predictions'}
    if include_sources:
        result['_incoming_sources'] = sources
    return result


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
    'out_degree', 'created_at', 'age_days', 'length_bytes', 'origin',
    'creator_key', 'creator_data_status', 'creator_account_type',
    'creator_tenure_days', 'creator_prior_edits_main', 'creator_prior_edits_other',
    'creator_prior_articles', 'creator_articles_created_total',
    'creator_articles_in_census', 'creator_orphans_current',
    'creator_orphans_current_share', 'creator_orphans_at_30_days',
    'creator_attribution', 'creator_registered_at', 'creator_tenure_status',
    'creator_experience_status', 'topic_labels', 'topic_status', 'topic_input_count',
    'wikibase_item', 'page_created_at', 'first_public_revision_at',
    'creation_date_source', 'creation_date_status',
    'creator_account_type_at_creation', 'creator_is_bot_now',
    'creator_languages_created', 'creator_languages_before', 'creation_tags',
    'import_log_observed', 'move_log_observed', 'creator_orphans_at_30_days_status',
)


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
    try:
        section, _, metadata = next(events)
        if section != 'metadata' or metadata.get('complete') is not True:
            raise ValueError('Raccolta completa richiesta')
        replica_database(metadata.get('language'))
        if expected and (metadata['language'] != expected['language'] or
                         bool(metadata.get('demo')) != bool(expected.get('demo')) or
                         metadata.get('finished_at') != expected['date']):
            raise ValueError('Manifest e raccolta non corrispondenti')
        generation = json_sha256([metadata, initial.st_size, initial.st_mtime_ns])[:12]
        directory = source.stem + '-connections-' + generation
        filenames = {'observations_file': source.stem + '-observations.json',
                     'cards_file': source.stem + '-cards.json', 'connections_dir': directory}
        if source.resolve() in {(folder / name).resolve() for name in filenames.values()}:
            raise ValueError('I trasporti devono essere distinti dal dataset originale')
        orphans, summaries, shortlists, seen = {}, {}, {}, set()

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
                    if record['orphan']:
                        orphans[str(pid)] = record
                    yield record
                elif section == 'link_candidates':
                    if identifier not in orphans or identifier in summaries:
                        raise ValueError('Candidate non corrispondenti a una voce orfana')
                    summary = {'status': record.get('status'), 'candidate_count': record.get('candidate_count')}
                    summaries[identifier] = summary
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
                    shortlists[identifier] = {**summary, 'candidates': selected}
                    detail(identifier, record)
            if (source.stat().st_size, source.stat().st_mtime_ns) != (initial.st_size, initial.st_mtime_ns):
                raise ValueError('Esportazione modificata durante la conversione')

        write_observations(folder / filenames['observations_file'],
                           {'metadata': metadata, 'pages': pages(), 'link_candidates': summaries})
        for identifier in orphans.keys() - summaries.keys():
            detail(identifier, {})
        write_json(folder / filenames['cards_file'],
                   {'format': 'orphan_cards_v1', 'metadata': metadata, 'census_page_count': len(seen),
                    'pages': None, 'link_candidates': None}, arrays={'pages': orphans.values()},
                   mappings={'link_candidates': shortlists.items()})
        return filenames
    finally:
        events.close()


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


def attach_creators(rows, graph_metadata, creators):
    """Attach a PAWS-only supplementary census without changing the observed graph."""
    fields = (
        'creator_key', 'creator_attribution', 'creator_account_type', 'creator_is_bot_now',
        'creator_account_type_at_creation', 'creator_registered_at', 'creator_tenure_days',
        'creator_tenure_status', 'creator_prior_edits_main', 'creator_prior_edits_other',
        'creator_prior_articles', 'creator_articles_created_total',
        'creator_languages_created', 'creator_languages_before', 'origin', 'creation_tags',
        'import_log_observed', 'move_log_observed', 'creator_experience_status',
    )
    by_id = {}
    metadata = {'status': 'not_collected', 'languages_scanned': [], 'matched_pages': 0,
                'orphan_30d_status': 'unavailable_no_historical_link_graph'}
    if creators is not None:
        cm = creators['metadata']
        if cm.get('language') != graph_metadata['language'] or cm.get('complete') is not True:
            raise ValueError('La raccolta dei creatori deve essere completa e della stessa lingua del grafo')
        database = replica_database(graph_metadata['language'], graph_metadata.get('database'))
        expected_source = 'Wiki Replicas / ' + database
        if replica_database(cm['language'], cm.get('database')) != database:
            raise ValueError('Le raccolte del grafo e dei creatori devono usare la stessa replica')
        if cm.get('source') != expected_source and not (cm.get('demo') and graph_metadata.get('demo')):
            raise ValueError('Fonte dei creatori non consentita: usare le repliche tramite PAWS')
        by_id = {p['page_id']: p for p in creators['pages']}
        if len(by_id) != len(creators['pages']):
            raise ValueError('Identificativi duplicati nella raccolta dei creatori')
        metadata = {**cm, 'status': 'collected', 'matched_pages': 0,
                    'input_sha256': json_sha256(creators)}
    from ._native import creator_matches, creator_counts
    matches = creator_matches([
        (row.get('first_public_revision_at'), record.get('article_created_at'),
         'page_created_at' in record, record.get('page_created_at'))
        for row in rows for record in [by_id.get(row['page_id'], {})]])
    for row, (matched, unconfirmed) in zip(rows, matches):
        row.update({field: None for field in fields})
        row['origin'] = 'unknown'
        record = by_id.get(row['page_id'])
        row['creator_data_status'] = 'not_collected' if creators is None else 'missing_page'
        if record is not None:
            if matched:
                row.update({field: record.get(field) for field in fields})
                if 'page_created_at' in record:
                    for field in ('page_created_at', 'creation_date_source', 'creation_date_status'):
                        row[field] = record.get(field)
                    if unconfirmed:
                        # Experience from the first revision cannot be relabeled
                        # as experience at a different or unknown creation event.
                        for field in fields:
                            if field.startswith('creator_'):
                                row[field] = None
                        row['creator_attribution'] = 'creation_event_unconfirmed'
                row['creator_data_status'] = 'matched'
                metadata['matched_pages'] += 1
            else:
                row['creator_data_status'] = 'creation_date_mismatch'
        row['creator_orphans_at_30_days'] = None
        row['creator_orphans_at_30_days_status'] = 'unavailable_no_historical_link_graph'
    counts = creator_counts([(row['creator_key'], row['orphan']) for row in rows])
    for row, (total, orphans, share) in zip(rows, counts):
        row.update(creator_articles_in_census=total, creator_orphans_current=orphans,
                   creator_orphans_current_share=share)
    metadata['unmatched_pages'] = len(rows) - metadata['matched_pages']
    return metadata
