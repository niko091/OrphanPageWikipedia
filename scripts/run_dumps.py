"""Run verified Wikimedia dump collection, import and analysis sequentially.

This is orchestration only: the existing CLI owns downloads, checkpoints,
scientific calculations and publication.
"""

import argparse
import hashlib
import json
import os
import shlex
import signal
import sqlite3
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]


def project_path(value):
    path = Path(value).expanduser()
    return (path if path.is_absolute() else PROJECT_ROOT / path).resolve()


def checksum(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def signature(path):
    try:
        info = Path(path).stat()
    except OSError:
        return None
    return [info.st_size, info.st_mtime_ns] if info.st_size else None


def complete_json(path):
    if signature(path) is None:
        return False
    with Path(path).open('rb') as source:
        source.seek(-1, os.SEEK_END)
        return source.read() in (b'}', b']')


class StepState:
    """Fast reuse of steps completed by this script with unchanged file stats."""

    def __init__(self, path):
        self.path = path
        if path.exists():
            data = json.loads(path.read_text(encoding='utf-8'))
            if data.get('version') != 1 or not isinstance(data.get('steps'), dict):
                raise ValueError(f'Stato della pipeline non valido: {path}')
            self.steps = data['steps']
        else:
            self.steps = {}

    def matches(self, key, inputs, outputs):
        saved = self.steps.get(key)
        return (saved is not None and saved.get('inputs') == inputs and
                saved.get('outputs') == {str(path): signature(path) for path in outputs} and
                all(signature(path) is not None for path in outputs))

    def save(self, key, inputs, outputs):
        if not all(signature(path) is not None for path in outputs):
            raise ValueError(f'Output mancante per il checkpoint {key}')
        self.steps[key] = {'inputs': inputs,
                           'outputs': {str(path): signature(path) for path in outputs}}
        self.path.parent.mkdir(parents=True, exist_ok=True)
        prepared = self.path.with_suffix('.json.tmp')
        prepared.write_text(json.dumps({'version': 1, 'steps': self.steps},
                                       ensure_ascii=False), encoding='utf-8')
        prepared.replace(self.path)


def manifest_for(dump_root, language, database, sql_release, history_release):
    path = dump_root / database / f'manifest-{sql_release}-{history_release}.json'
    if not complete_json(path):
        return None
    data = json.loads(path.read_text(encoding='utf-8'))
    if (data.get('language') != language or data.get('database') != database
            or data.get('sql_release') != sql_release
            or data.get('history_release') != history_release
            or not isinstance(data.get('files'), dict)):
        return None
    return path, data


def fetched_files(record):
    return [Path(item['path']) for item in record['files'].values()]


def imported_files_match(graph, history, stage, manifest_path, manifest):
    """Reuse an import only when source identities and staging agree."""
    if not all(complete_json(path) for path in (graph, history)) or signature(stage) is None:
        return False
    try:
        from orphanwiki.dumps import SQL_TABLES, _read_first_metadata
        graph_meta = _read_first_metadata(graph)
        history_meta = _read_first_metadata(history)
        if (graph_meta.get('source') != 'Wikimedia SQL dumps / ' + manifest['database']
                or graph_meta.get('language') != manifest['language']
                or graph_meta.get('sql_release') != manifest['sql_release']
                or graph_meta.get('complete') is not True
                or graph_meta.get('dump_manifest_sha256') != checksum(manifest_path)
                or history_meta.get('source') != 'MediaWiki History dumps / ' + manifest['database']
                or history_meta.get('language') != manifest['language']
                or history_meta.get('sql_release') != manifest['sql_release']
                or history_meta.get('history_release') != manifest['history_release']
                or history_meta.get('complete') is not True):
            return False
        history_files = {name: item['sha256_local'] for name, item in manifest['files'].items()
                         if name.endswith('.tsv.bz2')}
        if history_meta.get('history_files') != history_files:
            return False
        with sqlite3.connect(f'file:{stage}?mode=ro', uri=True) as connection:
            sql = dict(connection.execute('SELECT table_name,sha256 FROM imported'))
            scanned = {(name, phase): digest for name, phase, digest in
                       connection.execute('SELECT file,phase,sha256 FROM history_imported')}
        return (all(sql.get(name) == manifest['files'][name]['sha256_local'] for name in SQL_TABLES)
                and all(scanned.get((name, phase)) == digest
                        for name, digest in history_files.items()
                        for phase in ('initial', 'activity')))
    except (OSError, ValueError, KeyError, sqlite3.Error):
        return False


def topics_match(path, graph, graph_checksum, language, model_spec, census_pages):
    if not complete_json(path):
        return False
    try:
        from orphanwiki.dumps import _read_first_metadata
        graph_meta = _read_first_metadata(graph)
        meta = _read_first_metadata(path)
        return (meta.get('language') == language and meta.get('complete') is True
                and meta.get('source') == 'local_model_on_verified_SQL_dump_outlinks'
                and meta.get('graph_finished_at') == graph_meta.get('finished_at')
                and meta.get('input_file_sha256') == graph_checksum
                and meta.get('model') == model_spec
                and meta.get('predicted_pages', -1) + meta.get('missing_pages', -1) == census_pages)
    except (OSError, ValueError, KeyError, TypeError):
        return False


def approved_outputs(output_dir, files):
    manifest = output_dir / 'manifest.json'
    if not complete_json(manifest):
        return None
    entries = json.loads(manifest.read_text(encoding='utf-8'))
    if not isinstance(entries, list):
        return None
    indexed = {(entry.get('language'), entry.get('file')): entry
               for entry in entries if isinstance(entry, dict)}
    if len(indexed) != len(entries):
        return None
    if any((language, filename) not in indexed for language, filename in files.items()):
        return None
    paths = [manifest]
    for language, filename in files.items():
        entry = indexed[language, filename]
        if not prepared_summary_matches(output_dir, entry, [filename]):
            return None
        for key in ('file', 'observations_file', 'cards_file'):
            name = entry.get(key)
            if not isinstance(name, str) or not name or Path(name).name != name:
                return None
            path = output_dir / name
            if not complete_json(path):
                return None
            paths.append(path)
        detail_dir = entry.get('connections_dir')
        if (not isinstance(detail_dir, str) or not detail_dir or
                Path(detail_dir).name != detail_dir or
                not (output_dir / detail_dir).is_dir()):
            return None
    return paths


def prepared_summary_matches(output_dir, entry, filenames):
    from orphanwiki.core import prepared_summaries_match
    from orphanwiki.dumps import _read_first_metadata
    try:
        if any(not isinstance(filename, str) or Path(filename).name != filename or
               not filename.endswith('.json') for filename in [entry.get('file'), *filenames]):
            return False
        names = [entry.get('summary_file'), entry.get('creator_index_file')]
        if any(not isinstance(name, str) or Path(name).name != name or not name.endswith('.json')
               for name in names):
            return False
        sources = [{'language': _read_first_metadata(output_dir / filename)['language'],
                    'file': filename, 'size': (output_dir / filename).stat().st_size,
                    'mtime_ns': (output_dir / filename).stat().st_mtime_ns}
                   for filename in filenames]
        metadata = _read_first_metadata(output_dir / entry['file'])
        return prepared_summaries_match(output_dir / names[0], output_dir / names[1],
                                        metadata, sources)
    except (OSError, ValueError, KeyError, TypeError):
        return False


def collected_metadata_matches(meta, language, database, manifest_hash, topic_spec):
    """Check the source and complete topic supplement expected by the pipeline."""
    topics = meta.get('topics')
    return (meta.get('language') == language
            and meta.get('database') == database
            and meta.get('source') == 'Wikimedia SQL dumps / ' + database
            and meta.get('dump_manifest_sha256') == manifest_hash
            and meta.get('historical_age_rule') == 'mediawiki_history_first_observed_revision_v1'
            and isinstance(topics, dict) and topics.get('status') == 'predicted'
            and topics.get('model') == topic_spec)


def comparison_matches(output_dir, selected, manifest_hashes, topic_spec):
    languages = [language for language, _ in selected]
    manifest = output_dir / 'manifest.json'
    if not complete_json(manifest):
        return None
    try:
        entries = json.loads(manifest.read_text(encoding='utf-8'))
        for entry in entries:
            if not isinstance(entry, dict) or entry.get('kind') != 'aggregate':
                continue
            filename = entry.get('file')
            if not isinstance(filename, str) or Path(filename).name != filename:
                continue
            aggregate = output_dir / filename
            if not complete_json(aggregate):
                continue
            document = json.loads(aggregate.read_text(encoding='utf-8'))
            members = {member['language']: member for member in document['members']}
            if (document['metadata'].get('languages') != sorted(languages)
                    or len(document['members']) != len(languages) or set(members) != set(languages)):
                continue
            for language, database in selected:
                meta = members[language]['metadata']
                if (not collected_metadata_matches(meta, language, database,
                                                   manifest_hashes[language], topic_spec)
                        or meta.get('comparison', {}).get('status') != 'compared'
                        or set(meta['comparison'].get('reference_languages', [])) != set(languages) - {language}):
                    break
            else:
                paths = approved_outputs(output_dir, {language: members[language]['file']
                                                      for language in languages})
                if paths is not None and prepared_summary_matches(
                        output_dir, entry, [members[language]['file'] for language in sorted(languages)]):
                    return [*paths, aggregate]
        return None
    except (OSError, ValueError, KeyError, TypeError):
        return None


def analysis_matches(output_dir, language, database, manifest_hash, topic_spec):
    manifest = output_dir / 'manifest.json'
    if not complete_json(manifest):
        return None
    try:
        from orphanwiki.dumps import _read_first_metadata
        entries = json.loads(manifest.read_text(encoding='utf-8'))
        for entry in entries:
            if not isinstance(entry, dict) or entry.get('language') != language or entry.get('kind', 'wiki') == 'aggregate':
                continue
            filename = entry.get('file')
            if not isinstance(filename, str) or Path(filename).name != filename:
                continue
            source = output_dir / filename
            if not complete_json(source):
                continue
            meta = _read_first_metadata(source)
            if not collected_metadata_matches(meta, language, database, manifest_hash, topic_spec):
                continue
            paths = approved_outputs(output_dir, {language: filename})
            if paths is not None:
                return paths
        return None
    except (OSError, ValueError, KeyError):
        return None


def log_line(log, message):
    line = f'[{datetime.now(timezone.utc).isoformat(timespec="seconds")}] {message}\n'
    sys.stdout.write(line)
    sys.stdout.flush()
    log.write(line)
    log.flush()


def run_step(log, name, command):
    log_line(log, f'INIZIO {name}: {shlex.join(str(value) for value in command)}')
    process = subprocess.Popen(command, cwd=PROJECT_ROOT, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True, bufsize=1)
    try:
        for line in process.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()
            log.write(line)
            log.flush()
        status = process.wait()
    except BaseException:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        raise
    finally:
        process.stdout.close()
    if status:
        log_line(log, f'ERRORE {name}: codice di uscita {status}')
        raise subprocess.CalledProcessError(status, command)
    log_line(log, f'COMPLETATO {name}')


def require_file(path, description):
    if not path.is_file() or path.stat().st_size == 0:
        raise ValueError(f'{description} mancante o vuoto: {path}')


def interrupt_for_shutdown(_signum, _frame):
    raise InterruptedError('SIGTERM')


def run_pipeline(args, log):
    from orphanwiki.core import configured_languages
    configured = configured_languages(project_path(args.languages_file))
    if args.languages:
        if len(args.languages) != len(set(args.languages)) or set(args.languages) - {code for code, _ in configured}:
            raise ValueError('Le lingue richieste devono essere distinte e presenti nella configurazione')
        selected = [pair for pair in configured if pair[0] in args.languages]
    else:
        selected = configured
    languages = [language for language, _ in selected]
    databases = dict(selected)
    dump_root = project_path(args.dump_root)
    import_dir = project_path(args.import_dir)
    output_dir = project_path(args.output_dir)
    work_dir = project_path(args.work_dir) if args.work_dir else dump_root / 'staging'
    # Keep the venv executable path: resolving this symlink starts the system
    # interpreter without the virtual environment's site-packages.
    python = Path(args.python).expanduser()
    if not python.is_absolute():
        python = PROJECT_ROOT / python
    require_file(python, 'Interprete Python')
    base = [str(python), '-m', 'orphanwiki']
    state = StepState(dump_root / 'pipeline-state.json') if not args.plan else None
    source = ['--languages-file', str(project_path(args.languages_file))]
    fetch = [*base, 'fetch-dumps', *source, '--sql-release', args.sql_release,
             '--history-release', args.history_release, '--root', str(dump_root)]
    if args.plan:
        run_step(log, 'piano dei download', [*fetch, '--plan'])
        return
    manifests = [manifest_for(dump_root, language, database, args.sql_release, args.history_release)
                 for language, database in selected]
    previous = state.steps.get('fetch', {})
    old_inputs = previous.get('inputs', {})
    old_outputs = previous.get('outputs', {})
    missing = []
    for (language, database), existing in zip(selected, manifests):
        inputs = {'language': language, 'database': database, 'sql_release': args.sql_release,
                  'history_release': args.history_release, 'dump_root': str(dump_root)}
        outputs = [existing[0], *fetched_files(existing[1])] if existing else []
        inherited = (old_inputs.get('sql_release') == args.sql_release
                     and old_inputs.get('history_release') == args.history_release
                     and old_inputs.get('dump_root') == str(dump_root)
                     and [language, database] in old_inputs.get('languages', [])
                     and outputs and all(old_outputs.get(str(path)) == signature(path) for path in outputs))
        if not args.force and outputs and (state.matches(f'fetch:{language}', inputs, outputs) or inherited):
            log_line(log, f'SALTA download {language}: file verificati e invariati')
            state.save(f'fetch:{language}', inputs, outputs)
        else:
            missing.append(language)
    if missing:
        # Verify or resume only editions without an unchanged completed checkpoint.
        run_step(log, 'download verificato: ' + ' '.join(missing), [*fetch, '--languages', *missing])
        manifests = [manifest_for(dump_root, language, database, args.sql_release, args.history_release)
                     for language, database in selected]
        if not all(manifests):
            raise ValueError('Download terminato senza tutti i manifest richiesti')
        for (language, database), existing in zip(selected, manifests):
            inputs = {'language': language, 'database': database, 'sql_release': args.sql_release,
                      'history_release': args.history_release, 'dump_root': str(dump_root)}
            outputs = [existing[0], *fetched_files(existing[1])]
            state.save(f'fetch:{language}', inputs, outputs)
    else:
        log_line(log, 'SALTA download: tutte le edizioni hanno file verificati invariati')
    manifest_hashes = {}
    graph_paths = []
    for (language, database), (manifest, metadata) in zip(selected, manifests):
        manifest_hashes[language] = checksum(manifest)
        graph = import_dir / f'{language}-raw.json'
        history = import_dir / f'{language}-history.json'
        stage = work_dir / f'{database}-{args.sql_release}.sqlite'
        import_inputs = {'manifest_sha256': manifest_hashes[language],
                         'graph': str(graph), 'history': str(history), 'stage': str(stage)}
        outputs = [graph, history, stage]
        if not args.force and (state.matches(f'import:{language}', import_inputs, outputs) or imported_files_match(
                graph, history, stage, manifest, metadata)):
            log_line(log, f'SALTA importazione {language}: grafo, cronologia e checkpoint già completi')
            state.save(f'import:{language}', import_inputs, outputs)
        else:
            run_step(log, f'importazione {language}',
                     [*base, 'import-dumps', '--manifest', str(manifest),
                      '--output', str(graph), '--history-output', str(history),
                      '--work-dir', str(work_dir)])
            state.save(f'import:{language}', import_inputs, outputs)
        require_file(graph, 'Grafo importato')
        require_file(history, 'Cronologia importata')
        graph_paths.append(graph)

    topic_spec = json.loads(project_path(args.model_config).read_text(encoding='utf-8'))
    missing_topics = []
    census_by_language = {}
    for language in languages:
        graph = import_dir / f'{language}-raw.json'
        checkpoint = import_dir / f'{language}-topics.json'
        graph_hash = checksum(graph)
        with sqlite3.connect(f'file:{work_dir / (databases[language] + "-" + args.sql_release + ".sqlite")}?mode=ro',
                             uri=True) as connection:
            census = connection.execute('SELECT COUNT(*) FROM pages').fetchone()[0]
        census_by_language[language] = census
        if not args.force and topics_match(checkpoint, graph, graph_hash, language, topic_spec, census):
            log_line(log, f'SALTA temi {language}: checkpoint completo del modello e grafo invariato')
        else:
            missing_topics.append(language)
    if missing_topics:
        run_step(log, 'modello dei 64 temi: ' + ' '.join(missing_topics),
                 [*base, 'topics', '--input-dir', str(import_dir), '--output-dir', str(import_dir),
                  '--languages-file', str(project_path(args.languages_file)),
                  '--languages', *missing_topics, '--model', str(project_path(args.model)),
                  '--model-config', str(project_path(args.model_config)),
                  '--work-dir', str(work_dir),
                  '--time-budget-seconds', str(args.topic_budget_seconds)])
        for language in missing_topics:
            checkpoint = import_dir / f'{language}-topics.json'
            graph = import_dir / f'{language}-raw.json'
            if not topics_match(checkpoint, graph, checksum(graph), language, topic_spec,
                                census_by_language[language]):
                raise ValueError(f'Inferenza dei temi incompleta per {language}; ripetere lo script')
    if len(selected) > 1:
        existing = None if args.force else comparison_matches(
            output_dir, selected, manifest_hashes, topic_spec)
        if existing:
            log_line(log, 'SALTA confronto interlingua: aggregato e candidate già pubblicati')
            return
        run_step(log, 'confronto interlingua: ' + ' '.join(languages),
                 [*base, 'compare', *(str(path) for path in graph_paths),
                  '--histories', *(str(import_dir / f'{language}-history.json') for language in languages),
                  '--output-dir', str(output_dir), '--work-dir', str(work_dir)])
        if not comparison_matches(output_dir, selected, manifest_hashes, topic_spec):
            raise ValueError('Confronto terminato senza pubblicazione completa di tutte le lingue')
        return
    language = languages[0]
    output = output_dir / f'{language}-{args.sql_release}-{args.history_release}.json'
    if not args.force and analysis_matches(
            output_dir, language, databases[language], manifest_hashes[language], topic_spec):
        log_line(log, f'SALTA analisi {language}: export già pubblicato')
        return
    run_step(log, f'analisi {language}',
             [*base, 'analyze', str(graph_paths[0]), '--history', str(import_dir / f'{language}-history.json'),
              '--output', str(output), '--work-dir', str(work_dir)])
    if not analysis_matches(output_dir, language, databases[language], manifest_hashes[language], topic_spec):
        raise ValueError(f'Analisi {language} terminata senza pubblicazione completa')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--languages-file', default='config/languages.json',
                        help='Unico elenco delle Wikipedie complete da elaborare')
    parser.add_argument('--languages', nargs='+', help='Sottoinsieme della configurazione, senza cambiare gli algoritmi')
    parser.add_argument('--sql-release', required=True, help='Release SQL: YYYYMMDD')
    parser.add_argument('--history-release', required=True, help='Release History: YYYY-MM')
    parser.add_argument('--dump-root', default='data/dumps')
    parser.add_argument('--import-dir', default='data/imported')
    parser.add_argument('--output-dir', default='web/data')
    parser.add_argument('--work-dir', help='Staging SQLite; predefinito: <dump-root>/staging')
    parser.add_argument('--log-dir', default='data/logs')
    local_python = PROJECT_ROOT / '.venv' / 'bin' / 'python'
    parser.add_argument('--python', default=str(local_python if local_python.is_file() else sys.executable),
                        help='Python con estensione nativa e fastText; predefinito: .venv/bin/python se presente')
    parser.add_argument('--model', default='data/models/johnson-outlink-20220727.bin')
    parser.add_argument('--model-config', default='config/topics.json')
    parser.add_argument('--topic-budget-seconds', type=float, default=86400)
    parser.add_argument('--force', action='store_true', help='Ricalcola tutte le fasi, mantenendo i download verificati')
    parser.add_argument('--plan', action='store_true', help='Solo piano dei download, senza importare')
    args = parser.parse_args(argv)
    log_dir = project_path(args.log_dir)
    log_dir.mkdir(parents=True, exist_ok=True)
    name = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    log_path = log_dir / f'dumps-{name}-{os.getpid()}.log'
    with log_path.open('x', encoding='utf-8', buffering=1) as log:
        log_line(log, f'LOG {log_path}')
        previous_handler = signal.getsignal(signal.SIGTERM)
        signal.signal(signal.SIGTERM, interrupt_for_shutdown)
        try:
            run_pipeline(args, log)
        except KeyboardInterrupt:
            log_line(log, 'INTERROTTO dall’utente; i checkpoint già verificati restano disponibili')
            return 130
        except InterruptedError as error:
            log_line(log, f'INTERROTTO {error}; i checkpoint già verificati restano disponibili')
            return 143
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            log_line(log, f'PIPELINE FALLITA: {error}')
            return 1
        finally:
            signal.signal(signal.SIGTERM, previous_handler)
        log_line(log, 'PIPELINE COMPLETATA')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
