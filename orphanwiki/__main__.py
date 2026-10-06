import argparse
import csv
import json
import math
import re
import sqlite3
import tempfile
import time
from contextlib import closing
from pathlib import Path

from .core import (
    aggregate_index,
    analyze,
    compare_files,
    json_sha256,
    load_topic_model,
    predict_topics,
    replica_database,
    write_dashboard_exports,
    write_json,
)


def export(raw, output, rules, creators=None, topics=None):
    result = analyze(raw, rules, creators, topics=topics)
    result['metadata']['input_sha256'] = json_sha256(raw)
    export_result(result, output)


def export_result(result, output, update_manifest=True, announce=True):
    output = Path(output)
    write_json(output, result, arrays={'pages': result['pages']},
               mappings={'link_candidates': result['link_candidates'].items()} if 'link_candidates' in result else None)
    with output.with_suffix('.csv').open('w', encoding='utf-8', newline='') as stream:
        fields = ['page_id', 'title', 'category', 'category_status', 'orphan', 'in_degree', 'out_degree', 'created_at', 'age_days', 'length_bytes', 'origin', 'creator_key', 'creator_data_status', 'creator_account_type', 'creator_tenure_days', 'creator_prior_edits_main', 'creator_prior_edits_other', 'creator_prior_articles', 'creator_articles_created_total', 'creator_articles_in_census', 'creator_orphans_current', 'creator_orphans_current_share', 'creator_orphans_at_30_days']
        fields += ['creator_attribution', 'creator_registered_at', 'creator_tenure_status', 'creator_experience_status',
                   'topic_labels', 'topic_status', 'topic_input_count',
                   'wikibase_item', 'candidate_count', 'candidate_status',
                   'page_created_at', 'first_public_revision_at', 'creation_date_source', 'creation_date_status',
                   'creator_account_type_at_creation', 'creator_is_bot_now',
                   'creator_languages_created', 'creator_languages_before', 'creation_tags',
                   'import_log_observed', 'move_log_observed', 'creator_orphans_at_30_days_status']
        writer = csv.DictWriter(stream, fieldnames=fields, extrasaction='ignore')
        writer.writeheader()
        for page in result['pages']:
            candidates = result.get('link_candidates', {}).get(str(page['page_id']), {})
            writer.writerow({**page, 'candidate_count': candidates.get('candidate_count'),
                             'candidate_status': candidates.get('status')})
    transports = write_dashboard_exports(output, output.parent)
    if update_manifest:
        manifest_path = output.parent / 'manifest.json'
        manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else []
        manifest = [x for x in manifest if x['file'] != output.name and
                    (result['metadata'].get('demo') or not x.get('demo'))]
        manifest.append(manifest_entry(result, output.name, transports))
        write_json(manifest_path, manifest)
    if announce:
        print(f"Esportate {len(result['pages'])} voci: {output}")
    return transports


def manifest_entry(result, filename, transports=None):
    meta = result['metadata']
    return {'language': meta['language'], 'file': filename, 'date': meta['finished_at'],
            'demo': meta.get('demo', False), 'kind': meta.get('kind', 'wiki'),
            **(transports or {'observations_file': filename.removesuffix('.json') + '-observations.json'}
               if meta.get('kind') != 'aggregate' else {})}


def rebuild_dashboard(folder):
    """Derive observations, cards and per-orphan evidence without SQL.

    Read one exported article/orphan record at a time. Prepare every sidecar
    before publishing the new manifest; do not rewrite canonical JSON/CSV.
    """
    folder = Path(folder)
    manifest = json.loads((folder / 'manifest.json').read_text())
    if not isinstance(manifest, list) or not manifest:
        raise ValueError('Manifest non valido')
    files, languages = set(), set()
    for entry in manifest:
        name = entry.get('file', '')
        if not re.fullmatch(r'[a-zA-Z0-9_.-]+\.json', name) or name in files:
            raise ValueError('Nome dataset non valido o duplicato')
        files.add(name)
        if entry.get('kind') != 'aggregate':
            replica_database(entry['language'])
            if entry['language'] in languages:
                raise ValueError('Una sola raccolta per lingua nel manifest')
            languages.add(entry['language'])
    targets = {entry['file'].removesuffix('.json') + suffix
               for entry in manifest if entry.get('kind') != 'aggregate'
               for suffix in ('-observations.json', '-cards.json')}
    if targets & files or 'manifest.json' in targets:
        raise ValueError('I file compatti devono essere distinti dai dataset originali')
    with tempfile.TemporaryDirectory(prefix='orphanwiki-dashboard-', dir=folder.parent) as temporary:
        prepared = Path(temporary)
        for entry in manifest:
            if entry.get('kind') == 'aggregate':
                continue
            source = folder / entry['file']
            entry.update(write_dashboard_exports(source, prepared, expected=entry))
            filename = entry['cards_file']
            print(f"Dashboard: {entry['language']}; schede "
                  f"{(prepared / filename).stat().st_size / 2**20:.1f} MiB; evidenze separate per voce", flush=True)
        write_json(prepared / 'manifest.json', manifest)
        for path in sorted(prepared.rglob('*')):
            if path.is_file() and path != prepared / 'manifest.json':
                destination = folder / path.relative_to(prepared)
                destination.parent.mkdir(parents=True, exist_ok=True)
                path.replace(destination)
        (prepared / 'manifest.json').replace(folder / 'manifest.json')


def discover_collections(folder):
    """One standard census per language; reject ambiguity rather than guess freshness."""
    paths = sorted(Path(folder).glob('*-raw.json'))
    if not paths:
        raise ValueError('Nessuna raccolta *-raw.json nella cartella dati')
    return paths


def collection_languages(args):
    config = json.loads(Path(args.languages_file).read_text()) if args.languages_file else {}
    if config and config.get('version') != 1:
        raise ValueError('Versione della configurazione lingue non supportata')
    languages = config.get('languages') if args.languages_file else (args.languages or [args.language or 'vec'])
    if not isinstance(languages, list) or not languages or len(languages) != len(set(languages)):
        raise ValueError('Specificare codici lingua distinti')
    if args.database and len(languages) != 1:
        raise ValueError('--database richiede una sola lingua; per più lingue usare languages-file')
    databases = config.get('databases', {})
    if not isinstance(databases, dict) or set(databases) - set(languages):
        raise ValueError('Database configurati senza la lingua corrispondente')
    for language in languages:
        replica_database(language, args.database or databases.get(language))
    validate_size_selection(config)
    return [(language, args.database or databases.get(language)) for language in languages]


def validate_size_selection(config):
    """A configuration policy, independent of wiki codes and extraction limits."""
    policy = config.get('size_selection')
    if policy is None:
        return
    if not isinstance(policy, dict):
        raise ValueError('size_selection deve essere un oggetto')
    languages = config.get('languages', [])
    multiplier = policy.get('multiplier')
    always = policy.get('always_include', [])
    if (policy.get('reference_language') not in languages or
            type(multiplier) not in (int, float) or not math.isfinite(multiplier) or multiplier <= 0 or
            not isinstance(always, list) or any(code not in languages for code in always) or
            len(always) != len(set(always))):
        raise ValueError('Soglia o lingue di size_selection non valide')


def select_by_size(config, records):
    """Select whole wiki censuses using measured counts; equality is excluded."""
    validate_size_selection(config)
    policy = config['size_selection']
    counts = {}
    for record in records:
        language, count = record['language'], record['census_pages']
        if language in counts or type(count) is not int or count < 0:
            raise ValueError('Conteggi PAWS duplicati o non validi')
        counts[language] = count
    if set(counts) != set(config['languages']) or counts[policy['reference_language']] <= 0:
        raise ValueError('La selezione richiede tutte le dimensioni e un censimento di riferimento non vuoto')
    threshold = counts[policy['reference_language']] * policy['multiplier']
    selected = [code for code in config['languages']
                if code in policy.get('always_include', []) or counts[code] < threshold]
    return {'policy': policy, 'threshold_pages': threshold, 'selected_languages': selected,
            'excluded_languages': [code for code in config['languages'] if code not in selected]}


def measured_language_selection(args, languages):
    """Sequential PAWS census queries; retain intervals in a separate small report."""
    from .replicas import collect_sizes
    config = json.loads(Path(args.languages_file).read_text())
    records = []
    for language, database in languages:
        record = collect_sizes(language, args.credentials, database)
        records.append(record)
        print(f"Dimensioni PAWS: {language}: {record['census_pages']:,} voci", flush=True)
    selection = select_by_size(config, records)
    report = {'source': 'Wiki Replicas through PAWS', 'wikis': records, 'selection': selection}
    return selection, report


def collect_one(language, credentials, output, creator_output, database=None, low_memory=False, batch_size=1000, work_dir=None):
    from .replicas import collect_replicas, collect_creators
    if low_memory:
        from .replicas import DiskPages
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='orphanwiki-', dir=work_dir or output.parent) as folder:
            with closing(sqlite3.connect(Path(folder) / 'staging.sqlite')) as stage:
                stage.executescript('PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;')
                pages = DiskPages(stage)
                raw = collect_replicas(language, credentials, database, page_store=pages)
                pages.prepare_export()
                raw['metadata'].update(creator_collection_status='pending', collection_storage='sqlite_staging')
                write_json(output, raw, arrays={'pages': pages.export_pages()})
                print(f'Grafo integrale salvato: {output}; raccolta dei creatori a blocchi', flush=True)
                creators = collect_creators(language, credentials, database, stage=stage, batch_size=batch_size)
                creators['metadata']['collection_storage'] = 'sqlite_staging'
                write_json(creator_output, creators, arrays={'pages': creators['pages']})
                print(f'Creatori salvati: {creator_output}; aggiornamento del file completo', flush=True)
                raw['metadata']['creator_collection_status'] = 'collected'
                raw['creator_data'] = None
                write_json(output, raw, arrays={'pages': pages.export_pages()}, embedded_paths={'creator_data': creator_output})
        print(f'Dati grezzi completi, inclusi i creatori: {output}')
        return
    # Retain the two-argument API for ordinary wiki codes.
    options = {'database': database} if database else {}
    raw = collect_replicas(language, credentials, **options)
    raw['metadata']['creator_collection_status'] = 'pending'
    write_json(output, raw)
    print(f'Grafo salvato: {output}; raccolta dei creatori in corso', flush=True)
    creators = collect_creators(language, credentials, **options)
    print(f'Dati dei creatori acquisiti; salvataggio: {creator_output}', flush=True)
    write_json(creator_output, creators)
    print(f'Creatori salvati: {creator_output}; aggiornamento del file completo', flush=True)
    raw['creator_data'] = creators
    raw['metadata']['creator_collection_status'] = 'collected'
    write_json(output, raw)
    print(f'Dati grezzi completi, inclusi i creatori: {output}')


def publish_comparison(results, output_dir):
    output_dir = Path(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    files, manifest = {}, []
    # Prepare every JSON/CSV before replacing any previous published dataset.
    with tempfile.TemporaryDirectory(prefix='orphanwiki-publish-', dir=output_dir.parent) as folder:
        prepared = Path(folder)
        for language, result in results.items():
            suffix = '-demo' if result['metadata'].get('demo') else ''
            files[language] = f'{language}{suffix}.json'
            print(f"Preparazione JSON/CSV: {language}; {len(result['pages'])} voci", flush=True)
            transports = export_result(result, prepared / files[language], update_manifest=False, announce=False)
            manifest.append(manifest_entry(result, files[language], transports))
        index = aggregate_index(results, files)
        filename = 'all-demo.json' if index['metadata']['demo'] else 'all.json'
        write_json(prepared / filename, index)
        manifest.append(manifest_entry(index, filename))
        write_json(prepared / 'manifest.json', manifest)
        for path in sorted(prepared.rglob('*')):
            if path.is_file() and path != prepared / 'manifest.json':
                destination = output_dir / path.relative_to(prepared)
                destination.parent.mkdir(parents=True, exist_ok=True)
                path.replace(destination)
        # Publish only this complete comparison set; discard stale selector entries.
        (prepared / 'manifest.json').replace(output_dir / 'manifest.json')
    for language, result in results.items():
        print(f"Esportate {len(result['pages'])} voci: {output_dir / files[language]}", flush=True)


def main():
    parser = argparse.ArgumentParser(description='Censimento e analisi delle pagine orfane')
    sub = parser.add_subparsers(dest='command', required=True)
    dashboard = sub.add_parser('dashboard', help='Prepara grafici, schede e dettagli dagli export esistenti; nessuna nuova query')
    dashboard.add_argument('--data-dir', default='web/data')
    collect = sub.add_parser('collect', help='Raccogli grafo, categorie, date, provenienza e creatori da PAWS')
    selection = collect.add_mutually_exclusive_group()
    selection.add_argument('--language')
    selection.add_argument('--languages', nargs='+')
    selection.add_argument('--languages-file', help='Configurazione JSON con version, languages e databases opzionali')
    collect.add_argument('--database', help='Nome replica esplicito per codici storici, senza _p')
    collect.add_argument('--credentials')
    collect.add_argument('--output-dir', default='data')
    collect.add_argument('--low-memory', action='store_true', help='Staging SQLite su disco e creatori a blocchi; conserva l’intero censimento')
    collect.add_argument('--batch-size', type=int, default=1000, help='Voci per blocco di arricchimento dei creatori, non limite al censimento')
    collect.add_argument('--work-dir', help='Cartella con spazio per lo staging su disco')
    collect.add_argument('--output', help='Predefinito: data/<lingua>-raw.json')
    collect.add_argument('--creators-output', help='Copia separata dei creatori; predefinito: <nome-grafo senza -raw>-creators.json')
    compare = sub.add_parser('compare', help='Confronto interlingua in tutte le direzioni; solo dati PAWS')
    compare.add_argument('inputs', nargs='*', help='Raccolte complete, una per lingua; senza argomenti legge tutti i *-raw.json')
    compare.add_argument('--input-dir', default='data')
    compare.add_argument('--output-dir', default='web/data')
    compare.add_argument('--work-dir', help='Directory dello staging SQLite temporaneo; predefinita: directory di output')
    compare.add_argument('--creators', nargs='*', default=[], help='Supplementi opzionali, una raccolta per lingua')
    compare.add_argument('--topics', nargs='*', default=[], help='Predizioni locali opzionali; altrimenti usa i file fratelli *-topics.json')
    compare.add_argument('--rules', default='config/categories.json')
    creators_parser = sub.add_parser('collect-creators', help='Query aggiuntiva su PAWS: provenienza ed esperienza dei creatori')
    creators_parser.add_argument('--language', default='vec')
    creators_parser.add_argument('--database')
    creators_parser.add_argument('--credentials')
    creators_parser.add_argument('--low-memory', action='store_true')
    creators_parser.add_argument('--batch-size', type=int, default=1000)
    creators_parser.add_argument('--work-dir')
    creators_parser.add_argument('--output', help='Predefinito: data/<lingua>-creators.json')
    sizes = sub.add_parser('sizes', help='Dimensioni del censimento da PAWS; nessuna raccolta del grafo')
    selection = sizes.add_mutually_exclusive_group()
    selection.add_argument('--language')
    selection.add_argument('--languages', nargs='+')
    selection.add_argument('--languages-file')
    sizes.add_argument('--database')
    sizes.add_argument('--credentials')
    sizes.add_argument('--output', default='data/sizes.json')
    topics = sub.add_parser('topics', help='Modello Johnson locale sui soli collegamenti PAWS; nessuna query o API')
    topics.add_argument('--input-dir', default='data')
    topics.add_argument('--output-dir', default='data')
    selection = topics.add_mutually_exclusive_group()
    selection.add_argument('--languages', nargs='+')
    selection.add_argument('--languages-file')
    topics.add_argument('--model', default='data/models/johnson-outlink-20220727.bin')
    topics.add_argument('--model-config', default='config/topics.json')
    topics.add_argument('--batch-size', type=int, default=256)
    topics.add_argument('--time-budget-seconds', type=float, default=3300, help='Budget locale; stop tra blocchi, nessun supplemento incompleto pubblicato')
    command = sub.add_parser('analyze', help='Analisi nativa di un censimento PAWS completo')
    command.add_argument('input')
    command.add_argument('--creators', help='Supplemento creatori raccolto su PAWS')
    command.add_argument('--topics', help='Predizioni locali; altrimenti usa il file fratello *-topics.json')
    command.add_argument('--output')
    command.add_argument('--rules', default='config/categories.json')
    args = parser.parse_args()
    if hasattr(args, 'batch_size') and not 1 <= args.batch_size <= 10000:
        parser.error('--batch-size deve essere compreso tra 1 e 10000; non limita il numero di voci')
    if args.command == 'dashboard':
        rebuild_dashboard(args.data_dir)
    elif args.command == 'topics':
        if not math.isfinite(args.time_budget_seconds) or args.time_budget_seconds <= 0:
            parser.error('Il budget deve essere un numero positivo finito')
        began = time.monotonic()
        deadline = began + args.time_budget_seconds
        config_path = Path(args.languages_file or 'config/languages.json')
        if args.languages:
            languages = args.languages
        else:
            config = json.loads(config_path.read_text())
            if config.get('version') != 1:
                parser.error('Versione della configurazione lingue non supportata')
            languages = config.get('topic_languages', config.get('languages'))
        if (not isinstance(languages, list) or not languages or
                any(not isinstance(code, str) for code in languages) or len(set(languages)) != len(languages)):
            parser.error('Selezionare lingue distinte per il modello')
        for code in languages:
            replica_database(code)
        paths = [Path(args.input_dir) / f'{code}-raw.json' for code in languages]
        if any(not path.is_file() for path in paths):
            parser.error('Manca un censimento completo delle lingue selezionate; eseguire collect su PAWS')
        model, spec = load_topic_model(args.model, args.model_config)
        report = {'time_budget_seconds': args.time_budget_seconds, 'completed_languages': [], 'deferred_languages': [], 'wikis': []}
        rate = None
        for position, path in enumerate(paths):
            remaining = deadline - time.monotonic()
            if remaining <= 0 or (rate is not None and path.stat().st_size * rate * 2 > remaining):
                report['deferred_languages'] = languages[position:]
                break
            before = time.monotonic()
            raw = json.loads(path.read_text())
            if raw['metadata']['language'] != languages[position]:
                raise ValueError('Lingua del censimento non corrispondente al file selezionato')
            try:
                supplement = predict_topics(raw, model, spec, args.batch_size, deadline)
            except TimeoutError:
                report['deferred_languages'] = languages[position:]
                break
            output = Path(args.output_dir) / f'{languages[position]}-topics.json'
            write_json(output, supplement)
            elapsed = time.monotonic() - before
            rate = max(rate or 0, elapsed / max(1, path.stat().st_size))
            report['completed_languages'].append(languages[position])
            report['wikis'].append({'language': languages[position], 'seconds': elapsed, 'census_pages': len(raw['pages']),
                                   'predicted_pages': supplement['metadata']['predicted_pages'], 'timing': supplement['metadata']['timing']})
            print(f'Temi salvati: {output}; {elapsed:.1f}s', flush=True)
            del raw, supplement
        report['elapsed_seconds'] = time.monotonic() - began
        write_json(Path(args.output_dir) / 'topic-run.json', report)
        if report['deferred_languages']:
            print('Rinviate senza campionamento: ' + ' '.join(report['deferred_languages']), flush=True)
    elif args.command == 'collect':
        selected = collection_languages(args)
        config = json.loads(Path(args.languages_file).read_text()) if args.languages_file else {}
        if len(selected) > 1 and (args.output or args.creators_output):
            parser.error('Per più lingue usare --output-dir, senza percorsi dei singoli file')
        paths, jobs = set(), []
        for language, database in selected:
            output = Path(args.output or Path(args.output_dir) / f'{language}-raw.json')
            creator_output = Path(args.creators_output) if args.creators_output else output.with_name(output.stem.removesuffix('-raw') + '-creators.json')
            for path in (output, creator_output):
                pair = {path.resolve(), path.with_suffix(path.suffix + '.tmp').resolve()}
                if paths & pair:
                    parser.error('Il file dei creatori deve essere distinto dal grafo e dai file temporanei')
                paths.update(pair)
            jobs.append((language, database, output, creator_output))
        if config.get('size_selection') is not None:
            report_path = Path(args.output_dir) / 'language-selection.json'
            if {report_path.resolve(), report_path.with_suffix('.json.tmp').resolve()} & paths:
                parser.error('Il rapporto dimensioni deve essere distinto dai dati raccolti')
            selection, report = measured_language_selection(args, selected)
            jobs = [job for job in jobs if job[0] in selection['selected_languages']]
            write_json(report_path, report)
            print('Lingue selezionate: ' + ' '.join(selection['selected_languages']), flush=True)
        for language, database, output, creator_output in jobs:
            print(f'Raccolta integrale: {language}', flush=True)
            collect_one(language, args.credentials, output, creator_output, database, args.low_memory, args.batch_size, args.work_dir)
    elif args.command == 'collect-creators':
        from .replicas import collect_creators
        output = Path(args.output or f'data/{args.language}-creators.json')
        if args.low_memory:
            output.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='orphanwiki-', dir=args.work_dir or output.parent) as folder:
                with closing(sqlite3.connect(Path(folder) / 'staging.sqlite')) as stage:
                    stage.executescript('PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;')
                    raw = collect_creators(args.language, args.credentials, args.database, stage=stage, batch_size=args.batch_size)
                    raw['metadata']['collection_storage'] = 'sqlite_staging'
                    write_json(output, raw, arrays={'pages': raw['pages']})
            print(f'Dati dei creatori: {output}')
            return
        options = {'database': args.database} if args.database else {}
        raw = collect_creators(args.language, args.credentials, **options)
        write_json(output, raw)
        print(f'Dati dei creatori: {output}')
    elif args.command == 'sizes':
        from .replicas import collect_sizes
        records = []
        for language, database in collection_languages(args):
            record = collect_sizes(language, args.credentials, database)
            records.append(record)
            print(f"{language}: {record['census_pages']:,} pagine; {record['wikitext_bytes'] / 2**20:,.1f} MiB di wikitext (non peso JSON o RAM)", flush=True)
        report = {'source': 'Wiki Replicas through PAWS', 'wikis': records}
        if args.languages_file:
            config = json.loads(Path(args.languages_file).read_text())
            if config.get('size_selection') is not None:
                selection = select_by_size(config, records)
                report['selection'] = selection
                print('Lingue sotto soglia o incluse esplicitamente: ' + ' '.join(selection['selected_languages']), flush=True)
        write_json(args.output, report)
    elif args.command == 'compare':
        paths = [Path(p) for p in args.inputs] if args.inputs else discover_collections(args.input_dir)
        output_dir = Path(args.output_dir)
        output_dir.mkdir(parents=True, exist_ok=True)
        with compare_files(paths, json.loads(Path(args.rules).read_text()),
                           args.creators, args.topics, args.work_dir or output_dir) as results:
            protected = {Path(path).resolve() for path in [*paths, *args.creators, *args.topics]}
            filenames = ['manifest.json', 'all.json', 'all-demo.json']
            for language, result in results.items():
                name = language + ('-demo' if result['metadata'].get('demo') else '')
                filenames.extend([name + '.json', name + '.csv', name + '-observations.json'])
            if protected & {(output_dir / name).resolve() for name in filenames}:
                raise ValueError('I file di output devono essere distinti dagli input del confronto')
            publish_comparison(results, output_dir)
    else:
        rules = json.loads(Path(args.rules).read_text())
        raw = json.loads(Path(args.input).read_text())
        creators = json.loads(Path(args.creators).read_text()) if args.creators else None
        topics = None
        topic_path = Path(args.topics) if args.topics else Path(args.input).with_name(Path(args.input).stem.removesuffix('-raw') + '-topics.json')
        if args.topics and not topic_path.is_file():
            raise ValueError('File dei temi esplicitamente richiesto non trovato')
        if topic_path.exists():
            topics = json.loads(topic_path.read_text())
        export(raw, args.output or f"web/data/{raw['metadata']['language']}.json", rules, creators, topics)


if __name__ == '__main__':
    main()
