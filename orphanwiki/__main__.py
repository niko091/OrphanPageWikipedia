import argparse
import csv
import hashlib
import json
import math
import signal
import sys
import re
import tempfile
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

from .core import (
    operational_span,
    operational_phase,
    phase_counts,
    aggregate_index,
    analyze,
    compare_files,
    comparison_collections,
    configured_languages,
    select_languages,
    json_sha256,
    load_topic_model,
    prepared_summaries_match,
    predict_topics,
    wiki_database,
    write_dashboard_exports,
    write_aggregate_summary,
    write_summary_files,
    write_json,
)


def export(raw, output, rules, topics=None, history=None, quality=None):
    result = analyze(raw, rules, topics=topics, history=history, quality=quality)
    result['metadata']['input_sha256'] = json_sha256(raw)
    export_result(result, output)


@operational_phase('export.wiki')
def export_result(result, output, update_manifest=True, announce=True):
    phase_counts(language=result['metadata'].get('language'), rows=len(result['pages']))
    output = Path(output)
    with operational_span('export.canonical_json', rows=len(result['pages'])) as span:
        write_json(output, result, arrays={'pages': result['pages']},
                   mappings={'link_candidates': result['link_candidates'].items()} if 'link_candidates' in result else None)
        span['bytes'] = output.stat().st_size if output.is_file() else None
    with operational_span('export.csv', rows=len(result['pages'])) as span, \
            output.with_suffix('.csv').open('w', encoding='utf-8', newline='') as stream:
        fields = ['page_id', 'title', 'category', 'category_status', 'orphan', 'in_degree', 'out_degree', 'created_at', 'age_days', 'history_first_revision_at', 'history_first_revision_age_days', 'length_bytes', 'origin', 'creator_key', 'creator_data_status', 'creator_account_type', 'creator_tenure_days', 'creator_prior_edits_main', 'creator_prior_edits_other', 'creator_prior_articles', 'creator_articles_created_total', 'creator_articles_in_census', 'creator_orphans_current', 'creator_orphans_current_share', 'creator_orphans_at_30_days', 'history_creator_key', 'history_creator_status', 'history_data_status', 'creator_groups_at_creation_history', 'creator_continuity_months_before_creation', 'creator_continuity_bucket_before_creation', 'creator_prior_edits_main_history', 'creator_prior_edits_other_history']
        fields += ['creator_attribution', 'creator_registered_at', 'creator_tenure_status', 'creator_experience_status',
                   'topic_labels', 'topic_status', 'topic_input_count',
                   'wikibase_item', 'candidate_count', 'candidate_status',
                   'page_created_at', 'first_public_revision_at', 'creation_date_source', 'creation_date_status',
                   'creator_account_type_at_creation', 'creator_is_bot_now',
                   'creator_languages_created', 'creator_languages_before', 'creation_tags',
                   'import_log_observed', 'move_log_observed', 'creator_orphans_at_30_days_status',
                   'quality_score', 'quality_status', 'content_revision_id', 'quality_revision_relation']
        writer = csv.DictWriter(stream, fieldnames=fields, extrasaction='ignore')
        writer.writeheader()
        pages = result['pages']
        candidate_map = result.get('link_candidates', {})
        rows = (pages.csv_rows(candidate_map) if hasattr(pages, 'csv_rows') else
                ((page, candidate_map.get(str(page['page_id']), {})) for page in pages))
        for page, candidates in rows:
            writer.writerow({**page, 'candidate_count': candidates.get('candidate_count'),
                             'candidate_status': candidates.get('status')})
        stream.flush()
        span['bytes'] = stream.tell()
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
            'published_at': datetime.now(timezone.utc).isoformat(),
            'demo': meta.get('demo', False), 'kind': meta.get('kind', 'wiki'),
            **(transports or {})}


@operational_phase('export.publish')
def _publish_prepared(prepared, destination):
    """Move prepared transports, then publish their manifest last."""
    prepared, destination = Path(prepared), Path(destination)
    manifest = prepared / 'manifest.json'
    for path in sorted(prepared.rglob('*')):
        if path.is_dir():
            (destination / path.relative_to(prepared)).mkdir(parents=True, exist_ok=True)
        elif path.is_file() and path != manifest:
            target = destination / path.relative_to(prepared)
            target.parent.mkdir(parents=True, exist_ok=True)
            path.replace(target)
    manifest.replace(destination / 'manifest.json')


def comparison_filename(language, result, languages):
    """Name dump comparisons by release and inputs so older exports remain addressable."""
    meta = result['metadata']
    release = meta.get('sql_release')
    if not isinstance(release, str) or not re.fullmatch(r'\d{8}', release):
        return f"{language}{'-demo' if meta.get('demo') else ''}.json"
    identity = {'language': language, 'release': release,
                'input': meta.get('input_sha256'), 'history': meta.get('history'),
                'topics': meta.get('topics'), 'quality': meta.get('quality'),
                'age_rule': meta.get('age_rule'), 'measures_rule': meta.get('measures_rule'),
                'references': meta.get('comparison', {}).get('collections') or sorted(languages)}
    digest = hashlib.sha256(json.dumps(identity, sort_keys=True, ensure_ascii=False,
                                       default=str).encode()).hexdigest()[:12]
    return f'{language}-{release}-{digest}.json'


def _matching_aggregate_report(report, metadata, sources, count):
    if not isinstance(report, dict) or report.get('sources') != sources or report.get('count') != count:
        return None
    previous = report.get('metadata')
    if not isinstance(previous, dict):
        return None
    if ({key: value for key, value in previous.items()
         if key not in ('creators', 'history', 'comparison_coverage')}
            != {key: value for key, value in metadata.items() if key != 'comparison_coverage'}):
        return None
    if not isinstance(report.get('reports'), str) or not isinstance(report.get('article_preview'), list):
        return None
    return {**{key: value for key, value in previous.items() if key != 'comparison_coverage'}, **metadata}


@operational_phase('export.available_aggregate')
def publish_available_aggregate(output_dir, prepared_report=None):
    """Index the latest complete dump export per language without reading article arrays.

    Independently analyzed editions remain in the aggregate, but their absent
    cross-language comparisons are identified rather than treated as zero.
    """
    output_dir = Path(output_dir)
    manifest_path = output_dir / 'manifest.json'
    manifest = json.loads(manifest_path.read_text())
    if not isinstance(manifest, list):
        raise ValueError('Manifest non valido')
    latest = {}
    for entry in manifest:
        if not isinstance(entry, dict) or entry.get('kind') == 'aggregate' or entry.get('demo'):
            continue
        language, filename, cards = entry.get('language'), entry.get('file'), entry.get('cards_file')
        if not all(isinstance(value, str) and re.fullmatch(r'[a-zA-Z0-9_.-]+\.json', value)
                   for value in (filename, cards)) or not isinstance(language, str):
            raise ValueError('Manifest con file non valido')
        if language not in latest or (entry['date'], entry.get('published_at', '')) > (
                latest[language]['date'], latest[language].get('published_at', '')):
            latest[language] = entry
    if len(latest) < 2:
        return None
    from .dumps import _read_first_metadata
    results, files = {}, {}
    for language, entry in sorted(latest.items()):
        filename = entry['file']
        meta = _read_first_metadata(output_dir / filename)
        if (meta.get('language') != language or meta.get('complete') is not True or
                meta.get('source') != 'Wikimedia SQL dumps / ' + meta.get('database', '')):
            raise ValueError(f'Export non verificato per {language}')
        with (output_dir / entry['cards_file']).open('rb') as stream:
            header = stream.read(1024 * 1024)
        match = re.search(rb'"census_page_count"\s*:\s*(\d+)', header)
        if match is None:
            raise ValueError(f'Trasporto delle schede incompleto per {language}')
        count = int(match.group(1))
        results[language] = {'metadata': meta, 'pages': range(count)}
        files[language] = filename
    index = aggregate_index(results, files)
    if any(set(results[language]['metadata'].get('comparison', {}).get('reference_languages', []))
           != set(results) - {language} for language in results):
        index['metadata']['comparison_coverage'] = 'partial'
    else:
        index['metadata']['comparison_coverage'] = 'complete'
    release = index['metadata']['finished_at'][:10].replace('-', '')
    digest = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()[:12]
    filename = f'all-{release}-available-{digest}.json'
    summary_name = filename.removesuffix('.json') + '-summary.json'
    creator_name = filename.removesuffix('.json') + '-creators.json'
    sources = [{'language': member['language'], 'path': output_dir / member['file'],
                'page_count': member['page_count'], 'metadata': member['metadata']}
               for member in index['members']]
    fingerprints = []
    for source in sources:
        info = source['path'].stat()
        fingerprints.append({'language': source['language'], 'file': source['path'].name,
                             'size': info.st_size, 'mtime_ns': info.st_mtime_ns})
    fingerprints_by_language = {item['language']: item for item in fingerprints}
    with tempfile.TemporaryDirectory(prefix='orphanwiki-aggregate-', dir=output_dir.parent) as folder:
        prepared = Path(folder)
        new_member_summaries = []
        for member in index['members']:
            entry = latest[member['language']]
            existing_name = entry.get('summary_file')
            existing_creators = entry.get('creator_index_file')
            fingerprint = [fingerprints_by_language[member['language']]]
            if (existing_name and re.fullmatch(r'[a-zA-Z0-9_.-]+\.json', existing_name) and
                    existing_creators and re.fullmatch(r'[a-zA-Z0-9_.-]+\.json', existing_creators) and
                    prepared_summaries_match(output_dir / existing_name, output_dir / existing_creators,
                                             member['metadata'], fingerprint, member['page_count'])):
                continue
            name = member['file'].removesuffix('.json') + '-summary.json'
            write_aggregate_summary([{'language': member['language'],
                                      'path': output_dir / member['file'],
                                      'page_count': member['page_count'],
                                      'metadata': member['metadata']}],
                                    prepared / name, member['metadata'], aggregate=False)
            entry['summary_file'] = name
            entry['creator_index_file'] = name.removesuffix('-summary.json') + '-creators.json'
            new_member_summaries.extend((name, entry['creator_index_file']))
        write_json(prepared / filename, index)
        prior_summary = output_dir / summary_name
        prior_creators = output_dir / creator_name
        reuse = prepared_summaries_match(prior_summary, prior_creators, index['metadata'],
                                         fingerprints, sum(source['page_count'] for source in sources))
        if not reuse:
            reused_metadata = _matching_aggregate_report(
                prepared_report, index['metadata'], fingerprints,
                sum(source['page_count'] for source in sources))
            if reused_metadata is None:
                write_aggregate_summary(sources, prepared / summary_name, index['metadata'])
            else:
                write_summary_files(prepared / summary_name, prepared / creator_name,
                                    reused_metadata, prepared_report['count'], prepared_report['reports'],
                                    fingerprints, prepared_report['article_preview'])
        manifest = [entry for entry in manifest if entry.get('file') != filename]
        manifest.append({**manifest_entry(index, filename), 'summary_file': summary_name,
                         'creator_index_file': creator_name})
        write_json(prepared / 'manifest.json', manifest)
        for name in new_member_summaries:
            (prepared / name).replace(output_dir / name)
        (prepared / filename).replace(output_dir / filename)
        if not reuse:
            (prepared / summary_name).replace(prior_summary)
            (prepared / creator_name).replace(prior_creators)
        (prepared / 'manifest.json').replace(manifest_path)
    return filename


def rebuild_dashboard(folder):
    """Derive observations, cards and per-orphan evidence without SQL.

    Read one exported article/orphan record at a time. Prepare a distinct
    transport generation before switching the manifest; retain prior files.
    """
    folder = Path(folder)
    manifest = json.loads((folder / 'manifest.json').read_text())
    if not isinstance(manifest, list) or not manifest:
        raise ValueError('Manifest non valido')
    files = set()
    for entry in manifest:
        name = entry.get('file', '')
        if not re.fullmatch(r'[a-zA-Z0-9_.-]+\.json', name) or name in files:
            raise ValueError('Nome dataset non valido o duplicato')
        files.add(name)
        if entry.get('kind') != 'aggregate':
            wiki_database(entry['language'])
    generation = uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix='orphanwiki-dashboard-', dir=folder.parent) as temporary:
        prepared = Path(temporary)
        for entry in manifest:
            if entry.get('kind') == 'aggregate':
                continue
            source = folder / entry['file']
            transports = write_dashboard_exports(source, prepared, expected=entry,
                                                 transport_generation=generation)
            if set(transports.values()) & (files | {'manifest.json'}):
                raise ValueError('I file compatti devono essere distinti dai dataset originali')
            if any((folder / name).exists() for name in transports.values()):
                raise ValueError('Generazione dei trasporti già presente')
            entry.update(transports)
            filename = entry['cards_file']
            print(f"Dashboard: {entry['language']}; schede "
                  f"{(prepared / filename).stat().st_size / 2**20:.1f} MiB; evidenze separate per voce", flush=True)
        write_json(prepared / 'manifest.json', manifest)
        _publish_prepared(prepared, folder)


def discover_collections(folder):
    """One standard census per language; reject ambiguity rather than guess freshness."""
    paths = sorted(Path(folder).glob('*-raw.json'))
    if not paths:
        raise ValueError('Nessuna raccolta *-raw.json nella cartella dati')
    return paths


@operational_phase('export.comparison')
def publish_comparison(results, output_dir):
    output_dir = Path(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    files = {}
    manifest_path = output_dir / 'manifest.json'
    manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else []
    if not isinstance(manifest, list):
        raise ValueError('Manifest non valido')
    with tempfile.TemporaryDirectory(prefix='orphanwiki-publish-', dir=output_dir.parent) as folder:
        prepared = Path(folder)
        for language, result in results.items():
            files[language] = comparison_filename(language, result, results)
            print(f"Preparazione JSON/CSV: {language}; {len(result['pages'])} voci", flush=True)
            transports = export_result(result, prepared / files[language], update_manifest=False, announce=False)
            manifest = [entry for entry in manifest if entry.get('file') != files[language]]
            manifest.append(manifest_entry(result, files[language], transports))
        index = aggregate_index(results, files)
        releases = {result['metadata'].get('sql_release') for result in results.values()}
        if len(releases) == 1 and isinstance(next(iter(releases)), str) and re.fullmatch(r'\d{8}', next(iter(releases))):
            release = next(iter(releases))
            digest = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()[:12]
            filename = f'all-{release}-{digest}.json'
        else:
            filename = 'all-demo.json' if index['metadata']['demo'] else 'all.json'
        write_json(prepared / filename, index)
        summary_name = filename.removesuffix('.json') + '-summary.json'
        creator_name = filename.removesuffix('.json') + '-creators.json'
        aggregate_report = write_aggregate_summary(
            [{'language': member['language'], 'path': prepared / member['file'],
              'page_count': member['page_count'], 'metadata': member['metadata']}
             for member in index['members']], prepared / summary_name, index['metadata'])
        manifest = [entry for entry in manifest if entry.get('file') != filename]
        manifest.append({**manifest_entry(index, filename), 'summary_file': summary_name,
                         'creator_index_file': creator_name})
        write_json(prepared / 'manifest.json', manifest)
        _publish_prepared(prepared, output_dir)
    for language, result in results.items():
        print(f"Esportate {len(result['pages'])} voci: {output_dir / files[language]}", flush=True)
    if all(result['metadata'].get('source', '').startswith('Wikimedia SQL dumps / ')
           for result in results.values()):
        publish_available_aggregate(output_dir, aggregate_report)


def main():
    parser = argparse.ArgumentParser(description='Censimento e analisi delle pagine orfane')
    sub = parser.add_subparsers(dest='command', required=True)
    dashboard = sub.add_parser('dashboard', help='Prepara grafici, schede e dettagli dagli export esistenti; nessuna nuova query')
    dashboard.add_argument('--data-dir', default='web/data')
    content = sub.add_parser('fetch-content', help='Scarica i dump ufficiali del solo contenuto corrente')
    content.add_argument('--languages-file', default='config/languages.json')
    content.add_argument('--languages', nargs='+')
    content.add_argument('--content-release', required=True)
    content.add_argument('--root', default='data/dumps')
    content.add_argument('--plan', action='store_true')
    quality = sub.add_parser('quality', help='Calcola Johnson V2 dai dump XML correnti verificati')
    quality.add_argument('--languages-file', default='config/languages.json')
    quality.add_argument('--languages', nargs='+')
    quality.add_argument('--content-release', required=True)
    quality.add_argument('--root', default='data/dumps')
    quality.add_argument('--input-dir', default='data/imported')
    quality.add_argument('--output-dir', default='data/imported')
    quality.add_argument('--work-dir', default='data/dumps/staging')
    refresh = sub.add_parser('refresh-measures', help='Aggiorna età, autori e qualità riutilizzando grafo, temi e candidate pubblicati')
    refresh.add_argument('--languages-file', default='config/languages.json')
    refresh.add_argument('--languages', nargs='+')
    refresh.add_argument('--input-dir', default='data/imported')
    refresh.add_argument('--data-dir', default='web/data')
    downloads = sub.add_parser('fetch-dumps', help='Scarica e verifica dump Wikimedia completi per le lingue richieste')
    downloads.add_argument('--languages-file', default='config/languages.json')
    downloads.add_argument('--languages', nargs='+', help=argparse.SUPPRESS)
    downloads.add_argument('--sql-release', required=True, help='Data SQL in formato YYYYMMDD')
    downloads.add_argument('--history-release', required=True, help='Release della cronologia in formato YYYY-MM')
    downloads.add_argument('--root', default='data/dumps')
    downloads.add_argument('--plan', action='store_true', help='Mostra file e dimensioni senza scaricare')
    imported = sub.add_parser('import-dumps', help='Importa grafo SQL e cronologia verificata, con evidenze storiche separate')
    imported.add_argument('--manifest', required=True, help='Manifest prodotto da fetch-dumps')
    imported.add_argument('--output', help='Predefinito: data/imported/<lingua>-raw.json')
    imported.add_argument('--history-output', help='Predefinito: data/imported/<lingua>-history.json')
    imported.add_argument('--work-dir', default='data/dumps/staging')
    history_import = sub.add_parser('import-history', help='Riprende la cronologia da uno staging SQL già importato')
    history_import.add_argument('--manifest')
    history_import.add_argument('--languages-file', default='config/languages.json')
    history_import.add_argument('--languages', nargs='+')
    history_import.add_argument('--root', default='data/dumps')
    history_import.add_argument('--sql-release')
    history_import.add_argument('--history-release')
    history_import.add_argument('--output-dir', default='data/imported')
    history_import.add_argument('--output', help='Predefinito: data/imported/<lingua>-history.json')
    history_import.add_argument('--work-dir', default='data/dumps/staging')
    for history_parser in (imported, history_import):
        history_parser.add_argument('--history-cache-mib', type=int, default=256,
                                    help='Cache SQLite History in MiB, da 1 a 65536; predefinita 256')
        history_parser.add_argument('--history-commit-rows', type=int, default=250000,
                                    help='Record tra commit History, da 1 a 10000000; nessun limite al censimento')
    compare = sub.add_parser('compare', help='Confronto interlingua in tutte le direzioni')
    compare.add_argument('inputs', nargs='*', help='Raccolte complete, una per lingua; senza argomenti legge tutti i *-raw.json')
    compare.add_argument('--input-dir', default='data')
    compare.add_argument('--languages-file', default='config/languages.json')
    compare.add_argument('--output-dir', default='web/data')
    compare.add_argument('--work-dir', help='Indici incrementali SQLite; predefinita: staging accanto ai grafi di input')
    compare.add_argument('--topics', nargs='*', default=[], help='Predizioni locali opzionali; altrimenti usa i file fratelli *-topics.json')
    compare.add_argument('--qualities', nargs='*', default=[], help='Qualità V2 verificata per lingua')
    compare.add_argument('--histories', nargs='*', default=[], help='Cronologie dei dump verificate, una per lingua')
    compare.add_argument('--rules', default='config/categories.json')
    topics = sub.add_parser('topics', help='Modello locale sui collegamenti dei dump importati')
    topics.add_argument('--input-dir', default='data')
    topics.add_argument('--output-dir', default='data')
    topics.add_argument('--languages-file', default='config/languages.json')
    topics.add_argument('--languages', nargs='+', help=argparse.SUPPRESS)
    topics.add_argument('--model', default='data/models/johnson-outlink-20220727.bin')
    topics.add_argument('--model-config', default='config/topics.json')
    topics.add_argument('--batch-size', type=int, default=256)
    topics.add_argument('--work-dir', help='Staging SQL verificato per inferenza a blocchi')
    topics.add_argument('--time-budget-seconds', type=float, default=3300, help='Budget locale; stop tra blocchi, nessun supplemento incompleto pubblicato')
    command = sub.add_parser('analyze', help='Analisi nativa di un censimento completo')
    command.add_argument('input')
    command.add_argument('--topics', help='Predizioni locali; altrimenti usa il file fratello *-topics.json')
    command.add_argument('--quality', help='Supplemento qualità V2; altrimenti usa il file fratello *-quality.json')
    command.add_argument('--history', help='Supplemento storico verificato da MediaWiki History')
    command.add_argument('--work-dir', help='Staging SQLite dell’importazione SQL, se non usa il percorso predefinito')
    command.add_argument('--output')
    command.add_argument('--rules', default='config/categories.json')
    args = parser.parse_args()
    if hasattr(args, 'batch_size') and not 1 <= args.batch_size <= 10000:
        parser.error('--batch-size deve essere compreso tra 1 e 10000; non limita il numero di voci')
    if hasattr(args, 'history_cache_mib') and not 1 <= args.history_cache_mib <= 65536:
        parser.error('--history-cache-mib deve essere compreso tra 1 e 65536')
    if hasattr(args, 'history_commit_rows') and not 1 <= args.history_commit_rows <= 10000000:
        parser.error('--history-commit-rows deve essere compreso tra 1 e 10000000')
    if args.command in ('fetch-content', 'quality', 'refresh-measures', 'fetch-dumps', 'topics') or (
            args.command == 'import-history' and not args.manifest):
        try:
            selected = select_languages(configured_languages(args.languages_file), args.languages)
        except ValueError as error:
            parser.error(str(error))
        languages = args.languages or [language for language, _ in selected]
        databases = dict(selected)
    if args.command in ('fetch-content', 'quality', 'refresh-measures'):
        from .dumps import fetch_content, infer_quality, refresh_measures
        if args.command == 'fetch-content':
            fetch_content(languages, args.content_release, args.root, databases, args.plan)
        elif args.command == 'quality':
            for language in languages:
                infer_quality(Path(args.input_dir) / f'{language}-raw.json',
                              Path(args.root) / databases[language] / f'content-{args.content_release}.json',
                              Path(args.output_dir) / f'{language}-quality.json', args.work_dir)
        else:
            for language in languages:
                quality_path = Path(args.input_dir) / f'{language}-quality.json'
                refresh_measures(Path(args.input_dir) / f'{language}-raw.json',
                                 Path(args.input_dir) / f'{language}-history.json', args.data_dir,
                                 quality_path if quality_path.is_file() else None)
            publish_available_aggregate(args.data_dir)
    elif args.command == 'dashboard':
        rebuild_dashboard(args.data_dir)
    elif args.command == 'fetch-dumps':
        from .dumps import fetch_dumps
        fetch_dumps(languages, args.sql_release, args.history_release, args.root, databases,
                    plan_only=args.plan)
    elif args.command in ('import-dumps', 'import-history'):
        from .dumps import import_graph, import_history
        if args.command == 'import-dumps' or args.manifest:
            if args.command == 'import-history' and (args.languages or args.sql_release or args.history_release):
                parser.error('--manifest non può essere combinato con una selezione di release/lingue')
            manifest = json.loads(Path(args.manifest).read_text())
            language = manifest['language']
            wiki_database(language, manifest.get('database'))
            if args.command == 'import-dumps':
                import_graph(args.manifest, args.output or f'data/imported/{language}-raw.json', args.work_dir)
                output = args.history_output
            else:
                output = args.output
            output = output or f'data/imported/{language}-history.json'
            import_history(args.manifest, output, args.work_dir,
                           cache_mib=args.history_cache_mib, commit_rows=args.history_commit_rows)
        else:
            if not args.sql_release or not args.history_release or args.output:
                parser.error('Senza --manifest indicare --sql-release e --history-release; usare --output-dir')
            for language, database in selected:
                import_history(Path(args.root) / database / f'manifest-{args.sql_release}-{args.history_release}.json',
                               Path(args.output_dir) / f'{language}-history.json', args.work_dir,
                               cache_mib=args.history_cache_mib, commit_rows=args.history_commit_rows)
    elif args.command == 'topics':
        if not math.isfinite(args.time_budget_seconds) or args.time_budget_seconds <= 0:
            parser.error('Il budget deve essere un numero positivo finito')
        began = time.monotonic()
        deadline = began + args.time_budget_seconds
        paths = [Path(args.input_dir) / f'{code}-raw.json' for code in languages]
        if any(not path.is_file() for path in paths):
            parser.error('Manca un censimento completo: importare prima i dump')
        model, spec = load_topic_model(args.model, args.model_config)
        report = {'time_budget_seconds': args.time_budget_seconds, 'completed_languages': [], 'deferred_languages': [], 'wikis': []}
        rate = None
        for position, path in enumerate(paths):
            remaining = deadline - time.monotonic()
            if remaining <= 0 or (rate is not None and path.stat().st_size * rate * 2 > remaining):
                report['deferred_languages'] = languages[position:]
                break
            before = time.monotonic()
            output = Path(args.output_dir) / f'{languages[position]}-topics.json'
            from .dumps import _read_first_metadata, predict_imported_topics
            metadata = _read_first_metadata(path)
            if metadata['language'] != languages[position]:
                raise ValueError('Lingua del censimento non corrispondente al file selezionato')
            try:
                if metadata.get('source', '').startswith('Wikimedia SQL dumps / '):
                    topic_meta = predict_imported_topics(
                        path, output, model, spec, args.batch_size, deadline, args.work_dir)
                    census = topic_meta['predicted_pages'] + topic_meta['missing_pages']
                else:
                    raw = json.loads(path.read_text())
                    supplement = predict_topics(raw, model, spec, args.batch_size, deadline)
                    write_json(output, supplement)
                    topic_meta = supplement['metadata']
                    census = len(raw['pages'])
                    del raw, supplement
            except TimeoutError:
                report['deferred_languages'] = languages[position:]
                break
            elapsed = time.monotonic() - before
            rate = max(rate or 0, elapsed / max(1, path.stat().st_size))
            report['completed_languages'].append(languages[position])
            report['wikis'].append({'language': languages[position], 'seconds': elapsed, 'census_pages': census,
                                   'predicted_pages': topic_meta['predicted_pages'], 'timing': topic_meta['timing']})
            print(f'Temi salvati: {output}; {elapsed:.1f}s', flush=True)
        report['elapsed_seconds'] = time.monotonic() - began
        write_json(Path(args.output_dir) / 'topic-run.json', report)
        if report['deferred_languages']:
            print('Rinviate senza campionamento: ' + ' '.join(report['deferred_languages']), flush=True)
    elif args.command == 'compare':
        paths = [Path(p) for p in args.inputs] if args.inputs else discover_collections(args.input_dir)
        output_dir = Path(args.output_dir)
        output_dir.mkdir(parents=True, exist_ok=True)
        allowed = {code for code, _ in configured_languages(args.languages_file)}
        paths, published = comparison_collections(paths, output_dir, allowed)
        with compare_files(paths, json.loads(Path(args.rules).read_text()),
                           args.topics, args.work_dir, args.histories, args.qualities,
                           allowed_languages=allowed, published=published) as results:
            protected = {Path(path).resolve() for path in [*paths, *args.topics, *args.histories, *args.qualities]}
            filenames = ['manifest.json', 'all.json', 'all-demo.json']
            for language, result in results.items():
                name = language + ('-demo' if result['metadata'].get('demo') else '')
                filenames.extend([name + '.json', name + '.csv', name + '-observations.json'])
            if protected & {(output_dir / name).resolve() for name in filenames}:
                raise ValueError('I file di output devono essere distinti dagli input del confronto')
            publish_comparison(results, output_dir)
    else:
        rules = json.loads(Path(args.rules).read_text())
        topic_path = Path(args.topics) if args.topics else Path(args.input).with_name(Path(args.input).stem.removesuffix('-raw') + '-topics.json')
        if args.topics and not topic_path.is_file():
            raise ValueError('File dei temi esplicitamente richiesto non trovato')
        quality_path = Path(args.quality) if args.quality else Path(args.input).with_name(Path(args.input).stem.removesuffix('-raw') + '-quality.json')
        if args.quality and not quality_path.is_file():
            raise ValueError('File qualità esplicitamente richiesto non trovato')
        source = Path(args.input)
        if source.stat().st_size >= 128 * 1024**2:
            from .dumps import _read_first_metadata, analyze_imported_dump
            metadata = _read_first_metadata(source)
        else:
            metadata = {}
        if metadata.get('source', '').startswith('Wikimedia SQL dumps / '):
            analyze_imported_dump(source, args.history,
                                  args.output or f"web/data/{metadata['language']}.json",
                                  rules, args.work_dir, topic_path if topic_path.is_file() else None,
                                  quality_path=quality_path if quality_path.is_file() else None)
        else:
            raw = json.loads(source.read_text())
            topics = json.loads(topic_path.read_text()) if topic_path.exists() else None
            history = json.loads(Path(args.history).read_text()) if args.history else None
            export(raw, args.output or f"web/data/{raw['metadata']['language']}.json",
                   rules, topics, history, json.loads(quality_path.read_text()) if quality_path.exists() else None)


if __name__ == '__main__':
    def interrupted(_signum, _frame):
        raise InterruptedError('SIGTERM')

    signal.signal(signal.SIGTERM, interrupted)
    if '--plan' in sys.argv:
        main()
    else:
        with operational_span('command', command=sys.argv[1] if len(sys.argv) > 1 else None):
            main()
