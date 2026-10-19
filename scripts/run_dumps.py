"""Run verified Wikimedia dump collection, import and analysis sequentially.

This is orchestration only: the existing CLI owns downloads, checkpoints,
scientific calculations and publication.
"""

import argparse
import hashlib
import json
import math
import os
import platform
import resource
import shlex
import shutil
import signal
import sqlite3
import subprocess
import sys
import threading
import time
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


def imported_files_match(graph, history, stage, manifest_path, manifest, history_required=True):
    """Reuse an import only when source identities and staging agree."""
    if not all(complete_json(path) for path in ((graph, history) if history_required else (graph,))) or signature(stage) is None:
        return False
    try:
        from orphanwiki.dumps import SQL_TABLES, _read_first_metadata, HISTORY_RULE
        graph_meta = _read_first_metadata(graph)
        history_meta = _read_first_metadata(history) if history_required else {}
        if (graph_meta.get('source') != 'Wikimedia SQL dumps / ' + manifest['database']
                or graph_meta.get('language') != manifest['language']
                or graph_meta.get('sql_release') != manifest['sql_release']
                or graph_meta.get('complete') is not True
                or graph_meta.get('dump_manifest_sha256') != checksum(manifest_path)):
            return False
        with sqlite3.connect(stage.resolve().as_uri() + '?mode=ro', uri=True) as connection:
            sql = dict(connection.execute('SELECT table_name,sha256 FROM imported'))
            if any(sql.get(name) != manifest['files'][name]['sha256_local'] for name in SQL_TABLES):
                return False
            if not history_required:
                return True
        if (history_meta.get('source') != 'MediaWiki History dumps / ' + manifest['database']
                or history_meta.get('language') != manifest['language']
                or history_meta.get('sql_release') != manifest['sql_release']
                or history_meta.get('history_release') != manifest['history_release']
                or history_meta.get('complete') is not True
                or history_meta.get('creation_rule') != HISTORY_RULE):
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
                and all(scanned.get((name, phase)) == digest + ':' + HISTORY_RULE
                        for name, digest in history_files.items()
                        for phase in ('initial',))
                and all(scanned.get((name, 'activity'), '').startswith(digest + ':')
                        for name, digest in history_files.items()))
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


def collected_metadata_matches(meta, language, database, manifest_hash, topic_spec, measures=None):
    """Check the source and complete topic supplement expected by the pipeline."""
    topics = meta.get('topics')
    return (meta.get('language') == language
            and meta.get('database') == database
            and meta.get('source') == 'Wikimedia SQL dumps / ' + database
            and meta.get('dump_manifest_sha256') == manifest_hash
            and meta.get('age_rule') == 'mediawiki_history_page_creation_v2'
            and meta.get('measures_rule') == 'creation_quality_creators_v2'
            and (measures is None or all(all(meta.get(section, {}).get(key) == value for key, value in expected.items())
                for section, expected in measures.items()))
            and isinstance(topics, dict) and topics.get('status') == 'predicted'
            and topics.get('model') == topic_spec)


def comparison_matches(output_dir, selected, manifest_hashes, topic_spec, measures=None):
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
                                                   manifest_hashes[language], topic_spec, measures.get(language) if measures else None)
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


def analysis_matches(output_dir, language, database, manifest_hash, topic_spec, measures=None):
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
            if not collected_metadata_matches(meta, language, database, manifest_hash, topic_spec, measures):
                continue
            paths = approved_outputs(output_dir, {language: filename})
            if paths is not None:
                return paths
        return None
    except (OSError, ValueError, KeyError):
        return None


def reusable_graph_exports(output_dir, selected, manifest_hashes, topic_spec):
    """Graph/topic identity allows refreshing measures without repeating edges."""
    from orphanwiki.dumps import _read_first_metadata
    try:
        entries = json.loads((output_dir / 'manifest.json').read_text())
        languages = {code for code, _ in selected}
        for language, database in selected:
            matches = [e for e in entries if e.get('language') == language and e.get('kind', 'wiki') == 'wiki' and not e.get('demo')]
            if not matches:
                return False
            entry = max(matches, key=lambda e: (e.get('date', ''), e.get('published_at', '')))
            if not isinstance(entry.get('file'), str) or Path(entry['file']).name != entry['file']:
                return False
            meta = _read_first_metadata(output_dir / entry['file'])
            if (meta.get('complete') is not True or meta.get('source') != 'Wikimedia SQL dumps / ' + database
                    or meta.get('dump_manifest_sha256') != manifest_hashes[language]
                    or meta.get('topics', {}).get('model') != topic_spec
                    or meta.get('topics', {}).get('status') != 'predicted'):
                return False
            if len(selected) > 1 and (meta.get('comparison', {}).get('status') != 'compared'
                    or set(meta['comparison'].get('reference_languages', [])) != languages - {language}):
                return False
        return True
    except (OSError, ValueError, KeyError, TypeError):
        return False


def log_line(log, message):
    line = f'[{datetime.now(timezone.utc).isoformat(timespec="seconds")}] {message}\n'
    sys.stdout.write(line)
    sys.stdout.flush()
    log.write(line)
    log.flush()


def linux_resources(pid):
    """Read live tree RSS (shared pages may repeat), swap and host pressure.

    Missing /proc data stays null. Short-lived processes and between-sample
    peaks can be missed; these are not kernel high-water marks or PSS.
    """
    pending, seen = [pid], set()
    rss, swap, cpu = [], [], []
    while pending:
        current = pending.pop()
        if current in seen:
            continue
        seen.add(current)
        root = Path('/proc') / str(current)
        try:
            status = dict(line.split(':', 1) for line in (root / 'status').read_text().splitlines())
            if 'VmRSS' in status:
                rss.append(int(status['VmRSS'].split()[0]) * 1024)
            if 'VmSwap' in status:
                swap.append(int(status['VmSwap'].split()[0]) * 1024)
            fields = (root / 'stat').read_text().rsplit(')', 1)[1].split()
            # Own CPU plus waited-for descendants; still-live descendants are
            # visited separately. The interval estimate can race process exits.
            cpu.append(sum(int(value) for value in fields[11:15]) / os.sysconf('SC_CLK_TCK'))
            # A descendant can be owned by any thread, not only the main one.
            for task in (root / 'task').iterdir():
                try:
                    pending.extend(int(value) for value in (task / 'children').read_text().split())
                except (OSError, ValueError):
                    pass
        except (OSError, ValueError, IndexError):
            continue
    try:
        memory = dict(line.split(':', 1) for line in Path('/proc/meminfo').read_text().splitlines())
        available = int(memory['MemAvailable'].split()[0]) * 1024
    except (OSError, ValueError, KeyError):
        available = None
    try:
        # Linux accounts waited-for descendants here, avoiding a sum of
        # per-process I/O counters that could count reaped workers twice.
        io = dict(line.split(':', 1) for line in Path(f'/proc/{pid}/io').read_text().splitlines())
        read_bytes, write_bytes = int(io['read_bytes']), int(io['write_bytes'])
    except (OSError, ValueError, KeyError):
        read_bytes = write_bytes = None
    pressure = {}
    for kind in ('cpu', 'memory', 'io'):
        try:
            pressure[kind] = (Path('/proc/pressure') / kind).read_text().strip()
        except OSError:
            pressure[kind] = None
    return {'tree_rss_bytes': sum(rss) if rss else None,
            'tree_swap_bytes': sum(swap) if swap else None,
            'tree_cpu_seconds': sum(cpu) if cpu else None,
            'host_available_memory_bytes': available,
            'controller_reaped_read_bytes': read_bytes,
            'controller_reaped_write_bytes': write_bytes,
            'host_pressure': pressure,
            'host_load_average': list(os.getloadavg())}


class PipelineBenchmark:
    """Local operational measurements, separate from all research exports."""

    def __init__(self, log, path, args):
        self.log, self.path, self.args = log, path, args
        self.events = path.with_suffix('.metrics.jsonl').open('x', encoding='utf-8', buffering=1)
        self.lock, self.stop = threading.Lock(), threading.Event()
        self.started = time.monotonic()
        self.active = None
        self.steps = []
        self.peak_rss = None
        self.monitor_error = None
        self.last_cpu_sample = None
        self.thread = threading.Thread(target=self.monitor, name='pipeline-metrics', daemon=True)

    def emit(self, event, **values):
        with self.lock:
            self.events.write(json.dumps({'event': event, 'at': datetime.now(timezone.utc).isoformat(),
                                         'elapsed_seconds': time.monotonic() - self.started,
                                         **values}, ensure_ascii=False, allow_nan=False) + '\n')

    def start(self):
        disks = {}
        for key in ('dump_root', 'import_dir', 'output_dir', 'work_dir'):
            value = getattr(self.args, key) or str(project_path(self.args.dump_root) / 'staging')
            path = project_path(value)
            existing = path
            while not existing.exists():
                existing = existing.parent
            usage = shutil.disk_usage(existing)
            disks[key] = {'path': str(path), 'device': existing.stat().st_dev,
                          'total_bytes': usage.total, 'free_bytes': usage.free}
        host = {'platform': platform.platform(), 'controller_python': sys.version,
                'cpu_count': os.cpu_count(),
                'cpu_affinity': sorted(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else None,
                'disks': disks}
        for key, filename in (('cpuinfo', '/proc/cpuinfo'), ('memory', '/proc/meminfo'),
                              ('cgroup_cpu_max', '/sys/fs/cgroup/cpu.max'),
                              ('cgroup_memory_max', '/sys/fs/cgroup/memory.max')):
            try:
                raw = Path(filename).read_text()
                if key == 'cpuinfo':
                    raw = next((line.split(':', 1)[1].strip() for line in raw.splitlines()
                                if line.startswith('model name')), None)
                host[key] = raw
            except OSError:
                host[key] = None
        identity = {}
        for name, command in (('git_revision', ['git', 'rev-parse', 'HEAD']),
                              ('git_tracked_dirty', ['git', 'diff', '--quiet', 'HEAD'])):
            try:
                result = subprocess.run(command, cwd=PROJECT_ROOT, capture_output=True, text=True, check=False)
            except OSError:
                identity[name] = None
                continue
            identity[name] = ((result.returncode == 1 if result.returncode in (0, 1) else None)
                              if name == 'git_tracked_dirty' else
                              (result.stdout.strip() if result.returncode == 0 else None))
        self.metadata = {'format': 'pipeline_benchmark_v1', 'arguments': vars(self.args), 'host': host,
                         'code': {**identity, 'runner_sha256': checksum(__file__)},
                         'config_sha256': {str(path): checksum(path) for path in
                             (project_path(self.args.languages_file), project_path(self.args.model_config),
                              PROJECT_ROOT / 'config/quality.json')},
                         'measurement': {'interval_seconds': self.args.metrics_interval,
                             'rss': 'sampled sum of controller and live descendants; shared pages may repeat; not PSS',
                             'cpu': 'getrusage children deltas per completed step; 100 percent = one core',
                             'io': 'controller plus reaped descendants; physical storage bytes, excludes network and cache hits',
                             'host': 'load, pressure and available memory include unrelated workloads; cgroup files are mount-root limits'}}
        self.emit('run_start', **self.metadata)
        self.sample()
        self.thread.start()

    def sample(self):
        with self.lock:
            active = self.active
        values = linux_resources(os.getpid())
        now, cpu = time.monotonic(), values['tree_cpu_seconds']
        percent = None
        if self.last_cpu_sample is not None and cpu is not None:
            elapsed, delta = now - self.last_cpu_sample[0], cpu - self.last_cpu_sample[1]
            if elapsed > 0 and delta >= 0:
                percent = 100 * delta / elapsed
        if cpu is not None:
            self.last_cpu_sample = now, cpu
        values['sampled_cpu_percent_one_core'] = percent
        values['disk_free_bytes'] = {}
        for key, item in self.metadata['host']['disks'].items():
            try:
                path = Path(item['path'])
                while not path.exists():
                    path = path.parent
                values['disk_free_bytes'][key] = shutil.disk_usage(path).free
            except OSError:
                values['disk_free_bytes'][key] = None
        rss = values['tree_rss_bytes']
        if rss is not None:
            self.peak_rss = max(self.peak_rss or 0, rss)
        # Keep step attribution and its observed peak consistent with begin/end.
        with self.lock:
            if active is not self.active:
                active = None
            if active is not None and rss is not None:
                active['sampled_peak_tree_rss_bytes'] = max(active['sampled_peak_tree_rss_bytes'] or 0, rss)
            step = active['name'] if active else None
        self.emit('sample', step=step, **values)
        memory = f'{rss / 2**20:.1f} MiB' if rss is not None else 'non disponibile'
        cpu_label = f'{percent:.1f}%' if percent is not None else 'non disponibile'
        log_line(self.log, f'RISORSE {step or "orchestrazione"}: RSS albero {memory}; CPU {cpu_label} (100%=1 core); carico host {values["host_load_average"][0]:.2f}')

    def monitor(self):
        try:
            while not self.stop.wait(self.args.metrics_interval):
                self.sample()
        except Exception as error:
            self.monitor_error = str(error)
            log_line(self.log, f'MONITORAGGIO FALLITO: {error}')

    def begin_step(self, name, command):
        before = resource.getrusage(resource.RUSAGE_CHILDREN)
        with self.lock:
            self.active = {'name': name, 'command': [str(value) for value in command],
                           'sampled_peak_tree_rss_bytes': None}
        self.emit('step_start', **self.active)
        return time.monotonic(), before

    def end_step(self, baseline, status):
        elapsed = time.monotonic() - baseline[0]
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        user, system = after.ru_utime - baseline[1].ru_utime, after.ru_stime - baseline[1].ru_stime
        with self.lock:
            step, self.active = self.active, None
        step.update(wall_seconds=elapsed, user_cpu_seconds=user, system_cpu_seconds=system,
                    cpu_percent_one_core=100 * (user + system) / elapsed if elapsed else None,
                    returncode=status,
                    filesystem_input_operations=after.ru_inblock - baseline[1].ru_inblock,
                    filesystem_output_operations=after.ru_oublock - baseline[1].ru_oublock)
        self.steps.append(step)
        self.emit('step_end', **step)
        log_line(self.log, f'BENCHMARK {step["name"]}: {elapsed:.2f}s; CPU {step["cpu_percent_one_core"]:.1f}% (100%=1 core)')

    def finish(self, status):
        self.stop.set()
        if self.thread.ident is not None:
            self.thread.join()
        try:
            self.sample()
            result = {**self.metadata, 'status': status, 'wall_seconds': time.monotonic() - self.started,
                      'sampled_peak_tree_rss_bytes': self.peak_rss,
                      'monitor_error': self.monitor_error, 'steps': self.steps}
            self.emit('run_end', **result)
            prepared = self.path.with_suffix('.json.tmp')
            prepared.write_text(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + '\n', encoding='utf-8')
            prepared.replace(self.path)
            log_line(self.log, f'BENCHMARK salvato: {self.path}')
        finally:
            self.events.close()


def run_step(log, name, command, benchmark=None):
    log_line(log, f'INIZIO {name}: {shlex.join(str(value) for value in command)}')
    baseline = benchmark.begin_step(name, command) if benchmark else None
    process = None
    try:
        process = subprocess.Popen(command, cwd=PROJECT_ROOT, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, text=True, bufsize=1,
                                   env={**os.environ, 'PYTHONUNBUFFERED': '1'})
        for line in process.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()
            log.write(line)
            log.flush()
        status = process.wait()
    except BaseException:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        raise
    finally:
        if process is not None:
            process.stdout.close()
        if benchmark:
            benchmark.end_step(baseline, process.returncode if process is not None else None)
    if status:
        log_line(log, f'ERRORE {name}: codice di uscita {status}')
        raise subprocess.CalledProcessError(status, command)
    log_line(log, f'COMPLETATO {name}')


def require_file(path, description):
    if not path.is_file() or path.stat().st_size == 0:
        raise ValueError(f'{description} mancante o vuoto: {path}')


def interrupt_for_shutdown(_signum, _frame):
    raise InterruptedError('SIGTERM')


def run_pipeline(args, log, benchmark=None):
    from orphanwiki.core import configured_languages
    from orphanwiki.dumps import HISTORY_RULE, _read_first_metadata
    configured = configured_languages(project_path(args.languages_file))
    if args.languages:
        if len(args.languages) != len(set(args.languages)) or set(args.languages) - {code for code, _ in configured}:
            raise ValueError('Le lingue richieste devono essere distinte e presenti nella configurazione')
        selected = [pair for pair in configured if pair[0] in args.languages]
    else:
        selected = configured
    languages = [language for language, _ in selected]
    log_line(log, 'LINGUE ' + ' '.join(languages))

    def step(name, command):
        run_step(log, name, command, benchmark)

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
    history_options = ['--history-cache-mib', str(args.history_cache_mib),
                       '--history-commit-rows', str(args.history_commit_rows)]
    state = StepState(dump_root / 'pipeline-state.json') if not args.plan else None
    source = ['--languages-file', str(project_path(args.languages_file))]
    fetch = [*base, 'fetch-dumps', *source, '--sql-release', args.sql_release,
             '--history-release', args.history_release, '--root', str(dump_root)]
    if args.plan:
        step('piano dei download', [*fetch, '--languages', *languages, '--plan'])
        step('piano contenuto corrente', [*base, 'fetch-content', *source, '--languages', *languages,
                 '--content-release', args.content_release, '--root', str(dump_root), '--plan'])
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
        step('download verificato: ' + ' '.join(missing), [*fetch, '--languages', *missing])
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
                         'graph': str(graph), 'history': str(history), 'stage': str(stage), 'history_rule': HISTORY_RULE}
        outputs = [graph, history, stage]
        if not args.force and (state.matches(f'import:{language}', import_inputs, outputs) or imported_files_match(
                graph, history, stage, manifest, metadata)):
            log_line(log, f'SALTA importazione {language}: grafo, cronologia e checkpoint già completi')
            state.save(f'import:{language}', import_inputs, outputs)
        elif not args.force and imported_files_match(graph, history, stage, manifest, metadata, history_required=False):
            step(f'aggiornamento cronologia {language}',
                     [*base, 'import-history', '--manifest', str(manifest), '--output', str(history),
                      '--work-dir', str(work_dir), *history_options])
            state.save(f'import:{language}', import_inputs, outputs)
        else:
            step(f'importazione {language}',
                     [*base, 'import-dumps', '--manifest', str(manifest),
                      '--output', str(graph), '--history-output', str(history),
                      '--work-dir', str(work_dir), *history_options])
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
        step('modello dei 64 temi: ' + ' '.join(missing_topics),
                 [*base, 'topics', '--input-dir', str(import_dir), '--output-dir', str(import_dir),
                  '--languages-file', str(project_path(args.languages_file)),
                  '--languages', *missing_topics, '--model', str(project_path(args.model)),
                  '--model-config', str(project_path(args.model_config)),
                  '--work-dir', str(work_dir),
                  '--time-budget-seconds', str(args.topic_budget_seconds),
                  '--batch-size', str(args.topic_batch_size)])
        for language in missing_topics:
            checkpoint = import_dir / f'{language}-topics.json'
            graph = import_dir / f'{language}-raw.json'
            if not topics_match(checkpoint, graph, checksum(graph), language, topic_spec,
                                census_by_language[language]):
                raise ValueError(f'Inferenza dei temi incompleta per {language}; ripetere lo script')
    content_source = ['--languages-file', str(project_path(args.languages_file)), '--languages', *languages,
                      '--content-release', args.content_release, '--root', str(dump_root)]
    content_outputs = []
    for language, database in selected:
        path = dump_root / database / f'content-{args.content_release}.json'
        if complete_json(path):
            document = json.loads(path.read_text())
            content_outputs.extend([path, *fetched_files(document)])
    content_inputs = {'languages': [list(pair) for pair in selected], 'release': args.content_release}
    if len(content_outputs) < len(selected) or not state.matches('content-fetch', content_inputs, content_outputs):
        step('contenuto corrente verificato', [*base, 'fetch-content', *content_source])
        content_outputs = []
        for _, database in selected:
            path = dump_root / database / f'content-{args.content_release}.json'
            content_outputs.extend([path, *fetched_files(json.loads(path.read_text()))])
        state.save('content-fetch', content_inputs, content_outputs)
    for language, database in selected:
        graph = import_dir / f'{language}-raw.json'
        manifest = dump_root / database / f'content-{args.content_release}.json'
        quality = import_dir / f'{language}-quality.json'
        inputs = {'graph_sha256': checksum(graph), 'manifest_sha256': checksum(manifest),
                  'model_sha256': checksum(PROJECT_ROOT / 'config/quality.json')}
        if not state.matches(f'quality:{language}', inputs, [quality]):
            step(f'qualità {language}', [*base, 'quality', '--languages-file', str(project_path(args.languages_file)),
                '--languages', language, '--content-release', args.content_release, '--root', str(dump_root),
                '--input-dir', str(import_dir), '--output-dir', str(import_dir), '--work-dir', str(work_dir)])
            state.save(f'quality:{language}', inputs, [quality])
    measures = {}
    for language in languages:
        measures[language] = {'history': _read_first_metadata(import_dir / f'{language}-history.json')}
        quality = import_dir / f'{language}-quality.json'
        if quality.is_file():
            measures[language]['quality'] = _read_first_metadata(quality)
    if not args.force and reusable_graph_exports(output_dir, selected, manifest_hashes, topic_spec):
        if len(selected) > 1:
            current = comparison_matches(output_dir, selected, manifest_hashes, topic_spec, measures)
        else:
            current = analysis_matches(output_dir, languages[0], databases[languages[0]], manifest_hashes[languages[0]], topic_spec, measures[languages[0]])
        if current:
            log_line(log, 'SALTA confronto interlingua: osservazioni e misure già aggiornate' if len(selected) > 1
                     else 'SALTA analisi: osservazioni e misure già aggiornate')
            return
        step('aggiornamento misure senza ricalcolo del grafo', [*base, 'refresh-measures',
                 '--languages-file', str(project_path(args.languages_file)), '--languages', *languages,
                 '--input-dir', str(import_dir), '--data-dir', str(output_dir)])
        current = (comparison_matches(output_dir, selected, manifest_hashes, topic_spec, measures) if len(selected) > 1
                   else analysis_matches(output_dir, languages[0], databases[languages[0]], manifest_hashes[languages[0]], topic_spec, measures[languages[0]]))
        if not current:
            raise ValueError('Aggiornamento misure senza pubblicazione completa')
        return
    if len(selected) > 1:
        existing = None if args.force else comparison_matches(
            output_dir, selected, manifest_hashes, topic_spec, measures)
        if existing:
            log_line(log, 'SALTA confronto interlingua: aggregato e candidate già pubblicati')
            return
        step('confronto interlingua: ' + ' '.join(languages),
                 [*base, 'compare', *(str(path) for path in graph_paths),
                  '--histories', *(str(import_dir / f'{language}-history.json') for language in languages),
                  '--output-dir', str(output_dir), '--work-dir', str(work_dir)])
        if not comparison_matches(output_dir, selected, manifest_hashes, topic_spec, measures):
            raise ValueError('Confronto terminato senza pubblicazione completa di tutte le lingue')
        return
    language = languages[0]
    output = output_dir / f'{language}-{args.sql_release}-{args.history_release}.json'
    if not args.force and analysis_matches(
            output_dir, language, databases[language], manifest_hashes[language], topic_spec, measures.get(language) if measures else None):
        log_line(log, f'SALTA analisi {language}: export già pubblicato')
        return
    step(f'analisi {language}',
             [*base, 'analyze', str(graph_paths[0]), '--history', str(import_dir / f'{language}-history.json'),
              '--output', str(output), '--work-dir', str(work_dir)])
    if not analysis_matches(output_dir, language, databases[language], manifest_hashes[language], topic_spec, measures.get(language) if measures else None):
        raise ValueError(f'Analisi {language} terminata senza pubblicazione completa')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--languages-file', default='config/languages.json',
                        help='Unico elenco delle Wikipedie complete da elaborare')
    parser.add_argument('--languages', nargs='+', help='Sottoinsieme della configurazione, senza cambiare gli algoritmi')
    parser.add_argument('--sql-release', required=True, help='Release SQL: YYYYMMDD')
    parser.add_argument('--history-release', required=True, help='Release History: YYYY-MM')
    parser.add_argument('--content-release',
                        help='Release del contenuto corrente per Johnson V2: YYYY-MM-DD; predefinita: data della release SQL')
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
    parser.add_argument('--topic-budget-seconds', type=float, default=86400,
                        help='Budget complessivo dei temi: predefinito 86400 secondi; nessun campionamento')
    parser.add_argument('--topic-batch-size', type=int, default=256,
                        help='Voci per blocco dei temi, da 1 a 10000; non limita il censimento')
    parser.add_argument('--metrics-interval', type=float, default=10,
                        help='Secondi tra campioni risorse e messaggi di attività; predefinito 10')
    parser.add_argument('--history-cache-mib', type=int, default=256,
                        help='Cache SQLite History in MiB, da 1 a 65536; predefinita 256')
    parser.add_argument('--history-commit-rows', type=int, default=250000,
                        help='Record tra commit History, da 1 a 10000000; predefinito 250000')
    parser.add_argument('--force', action='store_true', help='Ricalcola tutte le fasi, mantenendo i download verificati')
    parser.add_argument('--plan', action='store_true', help='Solo piano dei download, senza importare')
    args = parser.parse_args(argv)
    if not math.isfinite(args.topic_budget_seconds) or args.topic_budget_seconds <= 0:
        parser.error('--topic-budget-seconds deve essere positivo e finito')
    if not math.isfinite(args.metrics_interval) or args.metrics_interval < 0.1:
        parser.error('--metrics-interval deve essere finito e almeno 0.1 secondi')
    if not 1 <= args.topic_batch_size <= 10000:
        parser.error('--topic-batch-size deve essere compreso tra 1 e 10000')
    if not 1 <= args.history_cache_mib <= 65536:
        parser.error('--history-cache-mib deve essere compreso tra 1 e 65536')
    if not 1 <= args.history_commit_rows <= 10000000:
        parser.error('--history-commit-rows deve essere compreso tra 1 e 10000000')
    try:
        sql_date = datetime.strptime(args.sql_release, '%Y%m%d')
        if sql_date.strftime('%Y%m%d') != args.sql_release:
            raise ValueError('Release SQL: usare YYYYMMDD')
        if args.content_release is None:
            args.content_release = sql_date.strftime('%Y-%m-%d')
        content_date = datetime.strptime(args.content_release, '%Y-%m-%d')
        if content_date.strftime('%Y-%m-%d') != args.content_release:
            raise ValueError('Release del contenuto: usare YYYY-MM-DD')
    except ValueError as error:
        parser.error(str(error))
    log_dir = project_path(args.log_dir)
    log_dir.mkdir(parents=True, exist_ok=True)
    name = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    log_path = log_dir / f'dumps-{name}-{os.getpid()}.log'
    with log_path.open('x', encoding='utf-8', buffering=1) as log:
        log_line(log, f'LOG {log_path}')
        log_line(log, f'RELEASE SQL {args.sql_release}; History {args.history_release}; contenuto {args.content_release}')
        previous_handler = signal.getsignal(signal.SIGTERM)
        signal.signal(signal.SIGTERM, interrupt_for_shutdown)
        benchmark = None
        status = 'failed'
        try:
            if not args.plan:
                benchmark = PipelineBenchmark(log, log_path.with_suffix('.benchmark.json'), args)
                benchmark.start()
            run_pipeline(args, log, benchmark)
            status = 'completed'
        except KeyboardInterrupt:
            status = 'interrupted'
            log_line(log, 'INTERROTTO dall’utente; i checkpoint già verificati restano disponibili')
            return 130
        except InterruptedError as error:
            status = 'interrupted'
            log_line(log, f'INTERROTTO {error}; i checkpoint già verificati restano disponibili')
            return 143
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            log_line(log, f'PIPELINE FALLITA: {error}')
            return 1
        finally:
            signal.signal(signal.SIGTERM, previous_handler)
            if benchmark is not None:
                if hasattr(benchmark, 'metadata'):
                    benchmark.finish(status)
                else:
                    benchmark.events.close()
        log_line(log, 'PIPELINE COMPLETATA')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
