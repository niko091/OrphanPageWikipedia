import hashlib
import json
import os
import runpy
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from orphanwiki.core import operational_span


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts' / 'run_dumps.py'


FAKE_PYTHON = '''#!/usr/bin/env python3
import hashlib
import json
import os
import sqlite3
import sys
import time
from pathlib import Path
sys.path.insert(0, str(Path.cwd()))
from orphanwiki.core import operational_span

args = sys.argv[1:]
assert args[:2] == ['-m', 'orphanwiki']
command = args[2]
options = args[3:]
def option(name):
    return options[options.index(name) + 1]
def sidecars(directory, filename, sources):
    metadata = json.loads((directory / filename).read_text())['metadata']
    if metadata.get('kind') == 'aggregate':
        metadata = {**metadata, 'creators': {'status': 'not_collected'},
                    'history': {'status': 'collected'}}
    fingerprints = [{'language': json.loads((directory / source).read_text())['metadata']['language'],
                     'file': source, 'size': (directory / source).stat().st_size,
                     'mtime_ns': (directory / source).stat().st_mtime_ns} for source in sources]
    stem = filename.removesuffix('.json')
    (directory / (stem + '-summary.json')).write_text(json.dumps({
        'format': 'dashboard_summary_v1', 'histogram_rule': 'shared_p2_p98_tails_v1', 'measures_rule': 'creation_quality_creators_v3',
        'metadata': metadata, 'sources': fingerprints, 'census_page_count': 0,
        'reports': {'overview': {'orphans': 0}}, 'article_preview': []}))
    (directory / (stem + '-creators.json')).write_text(json.dumps({
        'format': 'creator_index_v1', 'metadata': metadata,
        'sources': fingerprints, 'census_page_count': 0}))
with open(os.environ['PIPELINE_CALLS'], 'a', encoding='utf-8') as calls:
    calls.write(command + ':' + (option('--manifest') if '--manifest' in options else '') + '\\n')
print('fake stdout ' + command, flush=True)
with operational_span('mock.command', command=command, rows=1, bytes=4):
    pass
if os.environ.get('PAUSE_STAGE') == command:
    with operational_span('mock.pause'):
        time.sleep(30)
if os.environ.get('WORK_STAGE') == command:
    import subprocess
    subprocess.run([sys.executable, '-c',
        'import time; payload = bytearray(48 * 1024**2); '
        'start = time.process_time(); exec("while time.process_time() - start < 0.4: pass"); '
        'time.sleep(0.5)'], check=True)
if os.environ.get('FAIL_STAGE') == command:
    print('fake stderr ' + command, file=sys.stderr, flush=True)
    sys.exit(7)
if command == 'fetch-dumps' and '--plan' not in options:
    config = json.loads(Path(option('--languages-file')).read_text())
    languages = options[options.index('--languages') + 1:] if '--languages' in options else config['languages']
    databases = config.get('databases', {})
    for language in languages:
        database = databases.get(language, language.replace('-', '_') + 'wiki')
        root = Path(option('--root')) / database
        root.mkdir(parents=True, exist_ok=True)
        filename = 'manifest-' + option('--sql-release') + '-' + option('--history-release') + '.json'
        if not (root / filename).exists():
            (root / filename).write_text(json.dumps({'language': language, 'database': database,
                'sql_release': option('--sql-release'), 'history_release': option('--history-release'),
                'files': {}}))
elif command == 'import-dumps':
    manifest_path = Path(option('--manifest'))
    manifest = json.loads(manifest_path.read_text())
    graph = Path(option('--output'))
    graph.parent.mkdir(parents=True, exist_ok=True)
    graph.write_text(json.dumps({'metadata': {'language': manifest['language'],
        'database': manifest['database'], 'source': 'Wikimedia SQL dumps / ' + manifest['database'],
        'sql_release': manifest['sql_release'], 'complete': True,
        'finished_at': '2026-10-01T00:00:00+00:00',
        'dump_manifest_sha256': hashlib.sha256(manifest_path.read_bytes()).hexdigest()}, 'pages': []}))
    history = Path(option('--history-output'))
    history.write_text(json.dumps({'metadata': {'language': manifest['language'],
        'database': manifest['database'], 'source': 'MediaWiki History dumps / ' + manifest['database'],
        'sql_release': manifest['sql_release'], 'history_release': manifest['history_release'],
        'complete': True, 'creation_rule': 'mediawiki_history_page_creation_v2', 'history_files': {}}, 'pages': []}))
    stage = Path(option('--work-dir')) / (manifest['database'] + '-' + manifest['sql_release'] + '.sqlite')
    stage.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(stage) as database:
        database.execute('CREATE TABLE IF NOT EXISTS pages(id INTEGER PRIMARY KEY)')
        database.execute('INSERT OR IGNORE INTO pages VALUES(1)')
elif command == 'import-history':
    manifest = json.loads(Path(option('--manifest')).read_text())
    Path(option('--output')).write_text(json.dumps({'metadata': {'language': manifest['language'],
        'database': manifest['database'], 'source': 'MediaWiki History dumps / ' + manifest['database'],
        'sql_release': manifest['sql_release'], 'history_release': manifest['history_release'],
        'creation_rule': 'mediawiki_history_page_creation_v2', 'complete': True,
        'history_files': {name:item['sha256_local'] for name,item in manifest['files'].items() if name.endswith('.tsv.bz2')}}, 'pages': []}))
elif command == 'fetch-content' and '--plan' not in options:
    config = json.loads(Path(option('--languages-file')).read_text())
    languages = options[options.index('--languages') + 1:options.index('--content-release')]
    release = option('--content-release')
    for language in languages:
        database = config.get('databases', {}).get(language, language.replace('-', '_') + 'wiki')
        root = Path(option('--root')) / database
        root.mkdir(parents=True, exist_ok=True)
        payload = root / ('content-' + release + '.xml.bz2')
        payload.write_bytes(b'verified synthetic XML')
        (root / ('content-' + release + '.json')).write_text(json.dumps({'language':language,
            'database':database,'source':'mediawiki_content_current','content_release':release,'complete':True,
            'files':{payload.name:{'path':str(payload),'bytes':payload.stat().st_size,
                                  'sha256_local':hashlib.sha256(payload.read_bytes()).hexdigest()}}}))
elif command == 'quality':
    language = options[options.index('--languages') + 1]
    graph = Path(option('--input-dir')) / (language + '-raw.json')
    graph_meta = json.loads(graph.read_text())['metadata']
    release = option('--content-release')
    manifest = Path(option('--root')) / graph_meta['database'] / ('content-' + release + '.json')
    meta = {'language':language,'database':graph_meta['database'],'sql_release':graph_meta['sql_release'],
            'source':'Johnson V2 / mediawiki_content_current','complete':True,'content_release':release,
            'input_file_sha256':hashlib.sha256(graph.read_bytes()).hexdigest(),
            'manifest_sha256':hashlib.sha256(manifest.read_bytes()).hexdigest(),
            'model':json.loads(Path('config/quality.json').read_text()),
            'quality_revision_rule':'selected_official_xml_revision_v1'}
    (Path(option('--output-dir')) / (language + '-quality.json')).write_text(json.dumps({'metadata':meta,'pages':[]}))
elif command == 'topics':
    codes = options[options.index('--languages') + 1:options.index('--model')]
    spec = json.loads(Path(option('--model-config')).read_text())
    for language in codes:
        graph = Path(option('--input-dir')) / (language + '-raw.json')
        graph_meta = json.loads(graph.read_text())['metadata']
        checkpoint = Path(option('--output-dir')) / (language + '-topics.json')
        checkpoint.write_text(json.dumps({'metadata': {'language': language,
            'source': 'local_model_on_verified_SQL_dump_outlinks', 'complete': True,
            'graph_finished_at': graph_meta['finished_at'],
            'input_file_sha256': hashlib.sha256(graph.read_bytes()).hexdigest(),
            'model': spec, 'predicted_pages': 1, 'missing_pages': 0}, 'pages': []}))
elif command == 'analyze':
    output = Path(option('--output'))
    output.parent.mkdir(parents=True, exist_ok=True)
    graph_meta = json.loads(Path(options[0]).read_text())['metadata']
    language = graph_meta['language']
    meta = dict(graph_meta)
    meta['historical_age_rule'] = 'mediawiki_history_first_observed_revision_v1'
    meta['age_rule'] = 'mediawiki_history_page_creation_v2'
    meta['measures_rule'] = 'creation_quality_creators_v3'
    meta['history'] = json.loads(Path(options[0]).with_name(language + '-history.json').read_text())['metadata']
    if (Path(options[0]).with_name(language + '-topics.json')).exists():
        meta['topics'] = {'status': 'predicted', 'model': json.loads(
            Path(options[0]).with_name(language + '-topics.json').read_text())['metadata']['model']}
    if Path(options[0]).with_name(language + '-quality.json').exists():
        meta['quality'] = json.loads(Path(options[0]).with_name(language + '-quality.json').read_text())['metadata']
    output.write_text(json.dumps({'metadata': meta, 'pages': []}))
    sidecars(output.parent, output.name, [output.name])
    manifest = output.parent / 'manifest.json'
    entries = [entry for entry in json.loads(manifest.read_text()) if entry['file'] != output.name] if manifest.exists() else []
    observations = output.stem + '-observations.json'
    cards = output.stem + '-cards.json'
    for name in (observations, cards):
        (output.parent / name).write_text('{}')
    details = output.stem + '-connections-test'
    (output.parent / details).mkdir(exist_ok=True)
    entries.append({'language': language, 'file': output.name,
                    'observations_file': observations, 'cards_file': cards,
                    'connections_dir': details,
                    'summary_file': output.stem + '-summary.json',
                    'creator_index_file': output.stem + '-creators.json'})
    manifest.write_text(json.dumps(entries))
elif command == 'compare':
    graphs = [Path(value) for value in options[:next(i for i, value in enumerate(options) if value.startswith('--'))]]
    histories = [Path(value) for value in options[options.index('--histories') + 1:options.index('--output-dir')]]
    assert histories == [graph.with_name(graph.stem.removesuffix('-raw') + '-history.json') for graph in graphs]
    assert all(history.is_file() for history in histories)
    output = Path(option('--output-dir'))
    output.mkdir(parents=True, exist_ok=True)
    languages = [graph.stem.removesuffix('-raw') for graph in graphs]
    members, entries = [], []
    for graph, language in zip(graphs, languages):
        meta = json.loads(graph.read_text())['metadata']
        meta['historical_age_rule'] = 'mediawiki_history_first_observed_revision_v1'
        meta['age_rule'] = 'mediawiki_history_page_creation_v2'
        meta['measures_rule'] = 'creation_quality_creators_v3'
        meta['history'] = json.loads(graph.with_name(language + '-history.json').read_text())['metadata']
        meta['comparison'] = {'status': 'compared',
                              'reference_languages': [code for code in languages if code != language]}
        if graph.with_name(language + '-topics.json').exists():
            meta['topics'] = {'status': 'predicted', 'model': json.loads(
                graph.with_name(language + '-topics.json').read_text())['metadata']['model']}
        if graph.with_name(language + '-quality.json').exists():
            meta['quality'] = json.loads(graph.with_name(language + '-quality.json').read_text())['metadata']
        (output / (language + '.json')).write_text(json.dumps({'metadata': meta, 'pages': []}))
        sidecars(output, language + '.json', [language + '.json'])
        observations = language + '-observations.json'
        cards = language + '-cards.json'
        for name in (observations, cards):
            (output / name).write_text('{}')
        details = language + '-connections-test'
        (output / details).mkdir(exist_ok=True)
        entries.append({'language': language, 'file': language + '.json',
                        'observations_file': observations, 'cards_file': cards,
                        'connections_dir': details,
                        'summary_file': language + '-summary.json',
                        'creator_index_file': language + '-creators.json'})
        members.append({'language': language, 'file': language + '.json', 'metadata': meta})
    (output / 'all.json').write_text(json.dumps({'metadata': {'kind': 'aggregate', 'languages': sorted(languages)},
                                                'members': members}))
    sidecars(output, 'all.json', [language + '.json' for language in sorted(languages)])
    entries.append({'language': 'all', 'file': 'all.json', 'kind': 'aggregate',
                    'summary_file': 'all-summary.json', 'creator_index_file': 'all-creators.json'})
    (output / 'manifest.json').write_text(json.dumps(entries))
elif command == 'refresh-measures':
    output = Path(option('--data-dir'))
    entries = json.loads((output / 'manifest.json').read_text())
    for entry in entries:
        filename = entry['file']
        document = json.loads((output / filename).read_text())
        if entry.get('kind') == 'aggregate':
            for member in document['members']:
                member['metadata'] = json.loads((output / member['file']).read_text())['metadata']
            (output / filename).write_text(json.dumps(document))
            sidecars(output, filename, [member['file'] for member in sorted(document['members'], key=lambda member: member['language'])])
        else:
            language = entry['language']
            document['metadata']['history'] = json.loads((Path(option('--input-dir')) / (language + '-history.json')).read_text())['metadata']
            quality = Path(option('--input-dir')) / (language + '-quality.json')
            if quality.exists():
                document['metadata']['quality'] = json.loads(quality.read_text())['metadata']
            document['metadata']['age_rule'] = 'mediawiki_history_page_creation_v2'
            document['metadata']['measures_rule'] = 'creation_quality_creators_v3'
            (output / filename).write_text(json.dumps(document))
            sidecars(output, filename, [filename])
            (output / entry['connections_dir']).mkdir(exist_ok=True)
            for key in ('articles_file', 'connections_metadata_file'):
                if key in entry:
                    (output / entry[key]).write_text('{}')
            if 'cards_dir' in entry:
                (output / entry['cards_dir']).mkdir(exist_ok=True)
'''


class DumpPipelineTests(unittest.TestCase):
    def test_nested_spans_preserve_results_counters_and_error_boundaries(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'spans.jsonl'
            with patch.dict(os.environ, ORPHANWIKI_SPANS_FILE='', ORPHANWIKI_RUN_ID='test'):
                with operational_span('disabled') as span:
                    self.assertEqual(span, {})
            self.assertFalse(path.exists())
            with patch.dict(os.environ, ORPHANWIKI_SPANS_FILE=str(path), ORPHANWIKI_RUN_ID='test'):
                with self.assertRaisesRegex(ValueError, 'synthetic'), operational_span('parent') as parent:
                    with operational_span('child') as child:
                        child.update(rows=7, bytes=21)
                        raise ValueError('synthetic')
            events = [json.loads(line) for line in path.read_text().splitlines()]
            self.assertEqual([event['event'] for event in events],
                             ['span_start', 'span_start', 'span_end', 'span_end'])
            self.assertEqual(child['parent_id'], parent['span_id'])
            self.assertTrue(all(event['run_id'] == 'test' for event in events))
            ends = events[2:]
            self.assertEqual((ends[0]['rows'], ends[0]['bytes']), (7, 21))
            self.assertIsNone(ends[1]['rows'])
            self.assertIsNone(ends[1]['bytes'])
            for event in ends:
                self.assertEqual(event['status'], 'failed')
                self.assertEqual(event['error'], 'ValueError')
                self.assertGreaterEqual(event['finished_ns'], event['started_ns'])
            self.assertGreaterEqual(ends[1]['wall_seconds'], ends[0]['wall_seconds'])
            # An unavailable diagnostic sink cannot change collection results.
            with patch.dict(os.environ, ORPHANWIKI_SPANS_FILE=folder):
                with operational_span('unavailable'):
                    result = 42
            self.assertEqual(result, 42)

    def test_step_state_reports_reuse_and_invalidation_reasons(self):
        state_class = runpy.run_path(str(SCRIPT))['StepState']
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            output = root / 'graph.json'
            state = state_class(root / 'state.json')
            self.assertEqual(state.reason('import:vec', {'release': 'one'}, [output]), 'missing_checkpoint')
            output.write_text('{}')
            state.save('import:vec', {'release': 'one'}, [output])
            self.assertEqual(state.reason('import:vec', {'release': 'one'}, [output]), 'verified_reuse')
            self.assertEqual(state.reason('import:vec', {'release': 'two'}, [output]), 'source_mismatch')
            output.write_text('{"pages":[]}')
            self.assertEqual(state.reason('import:vec', {'release': 'one'}, [output]), 'output_mismatch')
            output.unlink()
            self.assertEqual(state.reason('import:vec', {'release': 'one'}, [output]), 'missing_output')

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.fake = self.root / 'fake-python'
        self.fake.write_text(FAKE_PYTHON)
        self.fake.chmod(0o755)
        self.calls = self.root / 'calls.txt'
        self.weights = self.root / 'model.bin'
        self.weights.write_bytes(b'fake model')

    def invoke(self, *selection, extra=(), fail=None, work=None):
        if selection and selection[0] == '--languages-file':
            language_file = selection[1]
        else:
            language_file = self.root / 'languages.json'
            language_file.write_text(json.dumps({'version': 1, 'languages': list(selection),
                                                 'databases': {}}))
        environment = {**os.environ, 'PIPELINE_CALLS': str(self.calls)}
        if fail:
            environment['FAIL_STAGE'] = fail
        if work:
            environment['WORK_STAGE'] = work
        return subprocess.run(
            [sys.executable, str(SCRIPT), '--languages-file', str(language_file),
             '--sql-release', '20261001', '--history-release', '2026-09',
             '--dump-root', str(self.root / 'dumps'),
             '--import-dir', str(self.root / 'imported'),
             '--output-dir', str(self.root / 'web-data'),
             '--log-dir', str(self.root / 'logs'), '--python', str(self.fake),
             '--model', str(self.weights), *extra],
            cwd=ROOT, env=environment, text=True, capture_output=True, check=False)

    def one_log(self):
        logs = list((self.root / 'logs').glob('dumps-*.log'))
        self.assertEqual(len(logs), 1)
        return logs[0].read_text()

    def one_benchmark(self):
        paths = list((self.root / 'logs').glob('*.benchmark.json'))
        self.assertEqual(len(paths), 1)
        return json.loads(paths[0].read_text())

    def metric_events(self):
        paths = list((self.root / 'logs').glob('*.metrics.jsonl'))
        self.assertEqual(len(paths), 1)
        return [json.loads(line) for line in paths[0].read_text().splitlines()]

    def span_events(self):
        paths = list((self.root / 'logs').glob('*.spans.jsonl'))
        self.assertEqual(len(paths), 1)
        return [json.loads(line) for line in paths[0].read_text().splitlines() if line.strip()]

    def assert_closed_span_tree(self, events):
        starts = {event['span_id']: event for event in events if event['event'] == 'span_start'}
        ends = {event['span_id']: event for event in events if event['event'] == 'span_end'}
        self.assertEqual(starts.keys(), ends.keys())
        self.assertEqual(len(starts) * 2, len(events))
        for key, start in starts.items():
            end = ends[key]
            self.assertEqual(start['run_id'], end['run_id'])
            self.assertGreaterEqual(end['finished_ns'], start['started_ns'])
            if start['parent_id'] is not None:
                self.assertIn(start['parent_id'], starts)
                parent = ends[start['parent_id']]
                self.assertGreaterEqual(start['started_ns'], parent['started_ns'])
                self.assertLessEqual(end['finished_ns'], parent['finished_ns'])

    def test_child_spans_share_run_and_parent_and_can_be_disabled(self):
        result = self.invoke('vec')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        events = self.span_events()
        self.assert_closed_span_tree(events)
        report = self.one_benchmark()
        self.assertTrue(all(event['run_id'] == report['run_id'] for event in events))
        commands = [event for event in events if event['name'] == 'mock.command' and event['event'] == 'span_end']
        self.assertEqual(len(commands), len(report['steps']))
        self.assertTrue(all(event['rows'] == 1 and event['bytes'] == 4 for event in commands))
        enabled_calls = [line.split(':')[0] for line in self.calls.read_text().splitlines()]
        self.calls.write_text('')
        disabled = self.root / 'disabled'
        result = self.invoke('vec', extra=('--no-phase-spans',
            '--dump-root', str(disabled / 'dumps'), '--import-dir', str(disabled / 'imported'),
            '--output-dir', str(disabled / 'web-data'), '--log-dir', str(disabled / 'logs')))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([line.split(':')[0] for line in self.calls.read_text().splitlines()], enabled_calls)
        self.assertEqual(list(disabled.rglob('*.spans.jsonl')), [])
        disabled_report = json.loads(next((disabled / 'logs').glob('*.benchmark.json')).read_text())
        self.assertIsNone(disabled_report['measurement']['spans_file'])

    def test_abrupt_child_exit_closes_abandoned_spans_at_detection_bound(self):
        child = ("import os\nfrom orphanwiki.core import operational_span\n"
                 "with operational_span('mock.abandoned'):\n    os._exit(7)\n")
        replacement = ("    from orphanwiki.core import operational_environment\n"
                       "    import subprocess\n"
                       "    with operational_span('mock.parent'):\n"
                       f"        subprocess.run([sys.executable, '-c', {child!r}], env=operational_environment(), check=False)\n"
                       "    sys.exit(7)")
        self.fake.write_text(FAKE_PYTHON.replace('    sys.exit(7)', replacement))
        result = self.invoke('vec', fail='import-dumps')
        self.assertNotEqual(result.returncode, 0)
        events = self.span_events()
        self.assert_closed_span_tree(events)
        abandoned = next(event for event in events if event['name'] == 'mock.abandoned' and event['event'] == 'span_end')
        self.assertTrue(abandoned['end_is_detection_bound'])
        self.assertEqual(abandoned['status'], 'interrupted')
        self.assertEqual(abandoned['returncode'], 7)
        parent = next(event for event in events if event['name'] == 'mock.parent' and event['event'] == 'span_end')
        self.assertEqual(abandoned['finished_ns'], parent['finished_ns'])
        self.assertGreaterEqual(abandoned['detected_ns'], parent['finished_ns'])
        self.assertEqual(events[-1]['status'], 'failed')

    @unittest.skipUnless(sys.platform.startswith('linux'), 'Linux /proc metrics')
    def test_benchmark_includes_descendant_resources_and_cpu(self):
        result = self.invoke('vec', work='quality', extra=(
            '--metrics-interval', '0.1', '--topic-batch-size', '512',
            '--topic-budget-seconds', '172800'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = self.one_benchmark()
        self.assertEqual(report['status'], 'completed')
        self.assertIsNone(report['monitor_error'])
        self.assertGreater(report['host']['disks']['work_dir']['free_bytes'], 0)
        self.assertEqual(report['arguments']['topic_batch_size'], 512)
        self.assertEqual(report['arguments']['topic_budget_seconds'], 172800)
        self.assertEqual(len(report['code']['runner_sha256']), 64)
        step = next(row for row in report['steps'] if row['name'] == 'qualità vec')
        self.assertEqual(step['returncode'], 0)
        self.assertGreater(step['wall_seconds'], 0.5)
        self.assertGreater(step['user_cpu_seconds'] + step['system_cpu_seconds'], 0.3)
        self.assertGreater(step['sampled_peak_tree_rss_bytes'], 48 * 1024**2)
        events = self.metric_events()
        self.assertEqual(events[0]['event'], 'run_start')
        self.assertEqual(events[-1]['event'], 'run_end')
        samples = [row for row in events if row['event'] == 'sample' and row['step'] == 'qualità vec']
        self.assertGreaterEqual(len(samples), 2)
        self.assertGreater(samples[0]['host_available_memory_bytes'], 0)
        topic = next(row for row in report['steps'] if row['command'][3] == 'topics')['command']
        self.assertEqual(topic[topic.index('--batch-size') + 1], '512')
        self.assertEqual(topic[topic.index('--time-budget-seconds') + 1], '172800.0')

    def test_invalid_benchmark_and_topic_controls_do_not_create_outputs(self):
        for extra in (('--metrics-interval', '0'), ('--metrics-interval', 'nan'),
                      ('--metrics-interval', 'inf'), ('--topic-budget-seconds', '0'),
                      ('--topic-budget-seconds', 'nan'), ('--topic-budget-seconds', 'inf'),
                      ('--topic-batch-size', '0'), ('--topic-batch-size', '10001'),
                      ('--history-cache-mib', '0'), ('--history-cache-mib', '65537'),
                      ('--history-commit-rows', '0'), ('--history-commit-rows', '10000001')):
            with self.subTest(extra=extra):
                result = self.invoke('vec', extra=extra)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.calls.exists())
                self.assertFalse((self.root / 'logs').exists())

    def test_history_controls_reach_each_import_and_do_not_invalidate_completed_steps(self):
        result = self.invoke('vec', 'lmo', extra=('--history-cache-mib', '1024', '--history-commit-rows', '100000'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = self.one_benchmark()
        steps = [row['command'] for row in report['steps'] if row['command'][3] == 'import-dumps']
        self.assertEqual(len(steps), 2)
        for command in steps:
            self.assertEqual(command[command.index('--history-cache-mib') + 1], '1024')
            self.assertEqual(command[command.index('--history-commit-rows') + 1], '100000')
        before = self.calls.read_text()
        rerun = self.invoke('vec', 'lmo', extra=('--history-cache-mib', '2048', '--history-commit-rows', '250000'))
        self.assertEqual(rerun.returncode, 0, rerun.stdout + rerun.stderr)
        self.assertEqual(self.calls.read_text(), before)

    def test_missing_proc_resources_remain_unavailable(self):
        resources = runpy.run_path(str(SCRIPT))['linux_resources']
        with patch.object(Path, 'read_text', side_effect=OSError('proc unavailable')):
            sample = resources(os.getpid())
        for key in ('tree_rss_bytes', 'tree_swap_bytes', 'tree_cpu_seconds',
                    'host_available_memory_bytes', 'controller_reaped_read_bytes',
                    'controller_reaped_write_bytes'):
            self.assertIsNone(sample[key])
        self.assertEqual(sample['host_pressure'], {'cpu': None, 'memory': None, 'io': None})

    def test_cannot_launch_child_preserves_failure_benchmark(self):
        self.fake.write_text('#!/missing-synthetic-interpreter\n')
        result = self.invoke('vec')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.calls.exists())
        report = self.one_benchmark()
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(len(report['steps']), 1)
        self.assertIsNone(report['steps'][0]['returncode'])
        self.assertEqual(self.metric_events()[-1]['event'], 'run_end')

    def test_runs_complete_languages_in_order_and_logs_every_step(self):
        result = self.invoke('vec', 'lmo')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual([line.split(':', 1)[0] for line in calls],
                         ['fetch-dumps', 'import-dumps', 'import-dumps', 'topics',
                          'fetch-content', 'quality', 'quality', 'compare'])
        self.assertIn('vecwiki', calls[1])
        self.assertIn('lmowiki', calls[2])
        self.assertIn('contenuto 2026-10-01', result.stdout)
        for language in ('vec', 'lmo'):
            quality = json.loads((self.root / 'imported' / f'{language}-quality.json').read_text())
            self.assertEqual(quality['metadata']['content_release'], '2026-10-01')
        self.assertTrue((self.root / 'web-data' / 'vec.json').is_file())
        self.assertTrue((self.root / 'web-data' / 'lmo.json').is_file())
        log = self.one_log()
        self.assertIn('PIPELINE COMPLETATA', log)
        self.assertIn('fake stdout compare', log)

    def test_python_symlink_keeps_virtual_environment_invocation_path(self):
        interpreter = self.root / 'venv-python'
        interpreter.symlink_to(self.fake)
        result = self.invoke('vec', extra=('--python', str(interpreter)))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f'INIZIO download verificato: vec: {interpreter} -m orphanwiki',
                      result.stdout)
        self.assertTrue(self.calls.read_text().startswith('fetch-dumps:'))

    def test_rerun_skips_verified_download_import_topics_and_comparison(self):
        first = self.invoke('vec', 'lmo')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        initial = self.calls.read_text()
        second = self.invoke('vec', 'lmo')
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertEqual(self.calls.read_text(), initial)
        self.assertIn('SALTA download', second.stdout)
        self.assertIn('SALTA importazione vec', second.stdout)
        self.assertIn('SALTA temi vec', second.stdout)
        self.assertIn('SALTA confronto interlingua', second.stdout)

    def test_added_language_downloads_only_new_edition(self):
        first = self.invoke('it')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        second = self.invoke('it', 'vec')
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertIn('SALTA download it', second.stdout)
        self.assertIn('download verificato: vec', second.stdout)
        self.assertNotIn('download verificato: it vec', second.stdout)
        calls = [line.split(':', 1)[0] for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls.count('fetch-dumps'), 2)

    def test_prior_combined_download_checkpoint_is_reused_per_language(self):
        first = self.invoke('it')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        state_path = self.root / 'dumps' / 'pipeline-state.json'
        state = json.loads(state_path.read_text())
        prior = state['steps'].pop('fetch:it')
        state['steps']['fetch'] = {'inputs': {
            'languages': [['it', 'itwiki']], 'sql_release': '20261001',
            'history_release': '2026-09', 'dump_root': str(self.root / 'dumps')},
            'outputs': prior['outputs']}
        state_path.write_text(json.dumps(state))
        second = self.invoke('it', 'vec')
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertIn('SALTA download it', second.stdout)
        self.assertIn('download verificato: vec', second.stdout)

    def test_missing_detail_directory_refreshes_without_repeating_comparison(self):
        first = self.invoke('vec', 'lmo')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        (self.root / 'web-data' / 'vec-connections-test').rmdir()
        second = self.invoke('vec', 'lmo')
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        calls = [line.split(':', 1)[0] for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls.count('compare'), 1)
        self.assertEqual(calls.count('refresh-measures'), 1)
        self.assertEqual(calls.count('import-dumps'), 2)

    def test_missing_optional_transports_refresh_only_transports_and_measures(self):
        first = self.invoke('vec', 'lmo')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        output = self.root / 'web-data'
        manifest_path = output / 'manifest.json'
        manifest = json.loads(manifest_path.read_text())
        entry = next(item for item in manifest if item['language'] == 'vec')
        entry.update(articles_file='vec-articles.json', cards_dir='vec-card-records',
                     connections_metadata_file='vec-connections-metadata.json')
        article = output / entry['articles_file']
        metadata = output / entry['connections_metadata_file']
        cards = output / entry['cards_dir']
        article.write_text('{}')
        metadata.write_text('{}')
        cards.mkdir()
        manifest_path.write_text(json.dumps(manifest))
        before = self.calls.read_text()
        rerun = self.invoke('vec', 'lmo')
        self.assertEqual(rerun.returncode, 0, rerun.stdout + rerun.stderr)
        self.assertEqual(self.calls.read_text(), before)
        for missing in (article, metadata, cards):
            if missing.is_dir():
                missing.rmdir()
            else:
                missing.unlink()
            result = self.invoke('vec', 'lmo')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(missing.exists())
        self.assertEqual(self.calls.read_text().count('compare:'), 1)
        self.assertEqual(self.calls.read_text().count('refresh-measures:'), 3)
        self.assertEqual(self.calls.read_text().count('topics:'), 1)

    def test_quality_release_change_reuses_sql_topics_and_matching(self):
        first = self.invoke('vec', 'lmo')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        before = self.calls.read_text()
        second = self.invoke('vec', 'lmo', extra=('--content-release', '2026-10-01'))
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertEqual(self.calls.read_text(), before)
        third = self.invoke('vec', 'lmo', extra=('--content-release', '2026-09-01'))
        self.assertEqual(third.returncode, 0, third.stdout + third.stderr)
        calls = [line.split(':', 1)[0] for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls.count('fetch-dumps'), 1)
        self.assertEqual(calls.count('import-dumps'), 2)
        self.assertEqual(calls.count('topics'), 1)
        self.assertEqual(calls.count('compare'), 1)
        self.assertEqual(calls.count('fetch-content'), 2)
        self.assertEqual(calls.count('quality'), 4)
        self.assertEqual(calls.count('refresh-measures'), 1)

    def test_missing_or_stale_chart_sidecar_refreshes_without_repeating_comparison(self):
        first = self.invoke('vec', 'lmo')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        summary = self.root / 'web-data' / 'vec-summary.json'
        summary.unlink()
        second = self.invoke('vec', 'lmo')
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertEqual(self.calls.read_text().count('compare:'), 1)
        self.assertEqual(self.calls.read_text().count('refresh-measures:'), 1)
        data = json.loads(summary.read_text())
        data['sources'][0]['size'] += 1
        summary.write_text(json.dumps(data))
        third = self.invoke('vec', 'lmo')
        self.assertEqual(third.returncode, 0, third.stdout + third.stderr)
        self.assertEqual(self.calls.read_text().count('compare:'), 1)
        self.assertEqual(self.calls.read_text().count('refresh-measures:'), 2)
        (self.root / 'web-data' / 'all-summary.json').unlink()
        fourth = self.invoke('vec', 'lmo')
        self.assertEqual(fourth.returncode, 0, fourth.stdout + fourth.stderr)
        self.assertEqual(self.calls.read_text().count('compare:'), 1)
        self.assertEqual(self.calls.read_text().count('refresh-measures:'), 3)

    def test_configured_pool_builds_one_aggregate_for_every_language(self):
        config = json.loads((ROOT / 'config' / 'languages.json').read_text())
        result = self.invoke('--languages-file', 'config/languages.json')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        aggregate = json.loads((self.root / 'web-data' / 'all.json').read_text())
        self.assertEqual(aggregate['metadata']['languages'], sorted(config['languages']))
        calls = [line.split(':', 1)[0] for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls.count('import-dumps'), len(config['languages']))
        self.assertEqual(calls.count('fetch-content'), 1)
        self.assertEqual(calls.count('quality'), len(config['languages']))
        self.assertEqual(calls.count('topics'), 1)
        self.assertEqual(calls.count('compare'), 1)

    def test_selected_subset_uses_the_single_configured_pool(self):
        result = self.invoke('--languages-file', 'config/languages.json',
                             extra=('--languages', 'vec', 'lmo'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.root / 'imported' / 'vec-raw.json').exists())
        self.assertTrue((self.root / 'imported' / 'lmo-raw.json').exists())
        self.assertFalse((self.root / 'imported' / 'it-raw.json').exists())
        self.assertEqual(json.loads((self.root / 'web-data' / 'all.json').read_text())['metadata']['languages'],
                         ['lmo', 'vec'])

    def test_adopts_existing_manual_import_and_single_wiki_analysis(self):
        from orphanwiki.dumps import SQL_TABLES
        root = self.root / 'dumps' / 'itwiki'
        root.mkdir(parents=True)
        files = {}
        for name in (*SQL_TABLES, '2026-09.itwiki.all-time.tsv.bz2'):
            path = root / name
            path.write_bytes(name.encode())
            files[name] = {'path': str(path), 'bytes': path.stat().st_size,
                           'sha256_local': hashlib.sha256(path.read_bytes()).hexdigest()}
        manifest = root / 'manifest-20261001-2026-09.json'
        manifest.write_text(json.dumps({'language': 'it', 'database': 'itwiki',
                                        'sql_release': '20261001', 'history_release': '2026-09',
                                        'files': files}))
        manifest_hash = hashlib.sha256(manifest.read_bytes()).hexdigest()
        stage = self.root / 'dumps' / 'staging' / 'itwiki-20261001.sqlite'
        stage.parent.mkdir(parents=True)
        with sqlite3.connect(stage) as database:
            database.execute('CREATE TABLE pages(id INTEGER PRIMARY KEY)')
            database.execute('INSERT INTO pages VALUES(1)')
            database.execute('CREATE TABLE imported(table_name TEXT, sha256 TEXT)')
            database.executemany('INSERT INTO imported VALUES(?,?)',
                                 [(name, files[name]['sha256_local']) for name in SQL_TABLES])
            database.execute('CREATE TABLE history_imported(file TEXT, phase TEXT, sha256 TEXT)')
            database.executemany('INSERT INTO history_imported VALUES(?,?,?)',
                                 [('2026-09.itwiki.all-time.tsv.bz2', phase,
                                   files['2026-09.itwiki.all-time.tsv.bz2']['sha256_local'] + (':mediawiki_history_page_creation_v2' if phase == 'initial' else ':users'))
                                  for phase in ('initial', 'activity')])
        imported = self.root / 'imported'
        imported.mkdir()
        (imported / 'it-raw.json').write_text(json.dumps({'metadata': {
            'language': 'it', 'database': 'itwiki', 'source': 'Wikimedia SQL dumps / itwiki',
            'sql_release': '20261001', 'complete': True,
            'finished_at': '2026-10-01T00:00:00+00:00',
            'dump_manifest_sha256': manifest_hash}, 'pages': []}))
        (imported / 'it-history.json').write_text(json.dumps({'metadata': {
            'language': 'it', 'database': 'itwiki', 'source': 'MediaWiki History dumps / itwiki',
            'sql_release': '20261001', 'history_release': '2026-09', 'complete': True,
            'history_files': {'2026-09.itwiki.all-time.tsv.bz2':
                              files['2026-09.itwiki.all-time.tsv.bz2']['sha256_local']}},
            'pages': []}))
        output = self.root / 'web-data'
        output.mkdir()
        (output / 'it.json').write_text(json.dumps({'metadata': {
            'language': 'it', 'dump_manifest_sha256': manifest_hash}, 'pages': []}))
        for name in ('it-observations.json', 'it-cards.json'):
            (output / name).write_text('{}')
        (output / 'it-connections-test').mkdir()
        (output / 'manifest.json').write_text(json.dumps([{
            'language': 'it', 'file': 'it.json', 'observations_file': 'it-observations.json',
            'cards_file': 'it-cards.json', 'connections_dir': 'it-connections-test'}]))
        result = self.invoke('it')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([line.split(':', 1)[0] for line in self.calls.read_text().splitlines()],
                         ['fetch-dumps', 'import-history', 'topics', 'fetch-content', 'quality', 'analyze'])
        self.assertIn('aggiornamento cronologia it', result.stdout)
        self.assertIn('CHECKPOINT importazione it: definition_mismatch', result.stdout)
        self.assertIn('CHECKPOINT grafo it: verified_reuse', result.stdout)
        history_command = next(step['command'] for step in self.one_benchmark()['steps']
                               if step['command'][3] == 'import-history')
        self.assertEqual(history_command[history_command.index('--history-cache-mib') + 1], '256')
        self.assertEqual(history_command[history_command.index('--history-commit-rows') + 1], '250000')
        self.assertTrue((self.root / 'imported' / 'it-topics.json').exists())

    def test_failure_stops_before_next_language_and_records_stderr(self):
        result = self.invoke('vec', 'lmo', fail='import-dumps')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([line.split(':', 1)[0] for line in self.calls.read_text().splitlines()],
                         ['fetch-dumps', 'import-dumps'])
        self.assertFalse((self.root / 'web-data' / 'vec.json').exists())
        log = self.one_log()
        self.assertIn('fake stderr import-dumps', log)
        self.assertIn('ERRORE importazione vec: codice di uscita 7', log)
        self.assertIn('PIPELINE FALLITA', log)
        report = self.one_benchmark()
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['steps'][-1]['returncode'], 7)
        self.assertEqual(self.metric_events()[-1]['status'], 'failed')

    def test_plan_only_runs_fetch_and_preserves_import_directory(self):
        unexpected = self.root / 'unexpected-spans.jsonl'
        with patch.dict(os.environ, ORPHANWIKI_SPANS_FILE=str(unexpected)):
            result = self.invoke('vec', extra=('--plan',))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.calls.read_text().splitlines(), ['fetch-dumps:', 'fetch-content:'])
        self.assertFalse((self.root / 'dumps').exists())
        self.assertFalse((self.root / 'imported').exists())
        self.assertIn('PIPELINE COMPLETATA', self.one_log())
        self.assertEqual(list((self.root / 'logs').glob('*.benchmark.json')), [])
        self.assertEqual(list((self.root / 'logs').glob('*.metrics.jsonl')), [])
        self.assertEqual(list((self.root / 'logs').glob('*.spans.jsonl')), [])
        self.assertFalse(unexpected.exists())

    def test_plan_applies_language_subset_to_sql_and_content(self):
        result = self.invoke('vec', 'lmo', 'it', extra=('--languages', 'vec', '--plan'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        commands = [line for line in self.one_log().splitlines() if 'INIZIO piano' in line]
        self.assertEqual(len(commands), 2)
        for command in commands:
            self.assertIn('--languages vec ', command)
            self.assertNotIn('--languages vec lmo', command)

    def test_rejects_invalid_release_dates_before_any_download(self):
        for extra in (('--sql-release', '20260230'), ('--sql-release', '2026101'),
                      ('--content-release', '2026-02-30'), ('--content-release', '2026-10-1')):
            with self.subTest(extra=extra):
                result = self.invoke('vec', extra=extra)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.calls.exists())
                self.assertFalse((self.root / 'dumps').exists())
                self.assertFalse((self.root / 'logs').exists())

    def test_quality_failure_stops_before_publishing_analysis(self):
        result = self.invoke('vec', fail='quality')
        self.assertNotEqual(result.returncode, 0)
        calls = [line.split(':', 1)[0] for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls, ['fetch-dumps', 'import-dumps', 'topics', 'fetch-content', 'quality'])
        self.assertFalse((self.root / 'web-data').exists())
        self.assertIn('PIPELINE FALLITA', self.one_log())

    def test_languages_file_supports_explicit_database_mapping(self):
        config = self.root / 'languages.json'
        config.write_text(json.dumps({'version': 1, 'languages': ['vec'],
                                      'databases': {'vec': 'customwiki'}}))
        result = self.invoke('--languages-file', str(config))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('customwiki', self.calls.read_text())

    def test_rejects_old_selection_rules_before_any_download(self):
        for extra in ({'topic_languages': ['vec']},
                      {'size_selection': {'reference_language': 'vec', 'multiplier': 1.5}}):
            with self.subTest(extra=extra):
                config = self.root / 'languages.json'
                config.write_text(json.dumps({'version': 1, 'languages': ['vec'], **extra}))
                result = self.invoke('--languages-file', str(config))
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.calls.exists())

    def test_rejects_duplicate_languages_before_any_download(self):
        config = self.root / 'languages.json'
        config.write_text(json.dumps({'version': 1, 'languages': ['vec', 'vec']}))
        result = self.invoke('--languages-file', str(config))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_selected_languages_all_enter_the_cross_language_comparison(self):
        result = self.invoke('vec', 'lmo', 'it')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([line.split(':', 1)[0] for line in self.calls.read_text().splitlines()],
                         ['fetch-dumps', 'import-dumps', 'import-dumps', 'import-dumps',
                          'topics', 'fetch-content', 'quality', 'quality', 'quality', 'compare'])
        aggregate = json.loads((self.root / 'web-data' / 'all.json').read_text())
        self.assertEqual(aggregate['metadata']['languages'], ['it', 'lmo', 'vec'])
        self.assertTrue((self.root / 'imported' / 'vec-topics.json').exists())
        self.assertTrue((self.root / 'imported' / 'it-topics.json').exists())

    def test_new_subset_extends_suggestions_to_previous_editions_without_reacquiring_them(self):
        config = self.root / 'languages.json'
        config.write_text(json.dumps({'version': 1, 'languages': ['vec', 'lmo', 'it']}))
        first = self.invoke('--languages-file', str(config), extra=('--languages', 'vec'))
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        old_files = {path: path.read_bytes() for path in (self.root / 'imported').glob('vec-*.json')}
        self.calls.unlink()
        second = self.invoke('--languages-file', str(config), extra=('--languages', 'lmo', 'it'))
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual([line.split(':', 1)[0] for line in calls], [
            'fetch-dumps', 'import-dumps', 'import-dumps', 'topics', 'fetch-content', 'quality', 'quality', 'compare'])
        self.assertFalse(any('vecwiki' in line for line in calls))
        self.assertEqual(old_files, {path: path.read_bytes() for path in old_files})
        aggregate = json.loads((self.root / 'web-data' / 'all.json').read_text())
        self.assertEqual(aggregate['metadata']['languages'], ['it', 'lmo', 'vec'])
        self.assertIn('LINGUE confronto incrementale: it lmo vec', second.stdout)
        self.calls.unlink()
        third = self.invoke('--languages-file', str(config), extra=('--languages', 'lmo', 'it'))
        self.assertEqual(third.returncode, 0, third.stdout + third.stderr)
        self.assertFalse(self.calls.exists())
        self.assertIn('SALTA confronto interlingua', third.stdout)

    def test_model_runs_for_every_selected_language(self):
        result = self.invoke('it')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([line.split(':', 1)[0] for line in self.calls.read_text().splitlines()],
                         ['fetch-dumps', 'import-dumps', 'topics', 'fetch-content', 'quality', 'analyze'])
        meta = json.loads((self.root / 'web-data' / 'it-20261001-2026-09.json').read_text())['metadata']
        self.assertEqual(meta['topics']['status'], 'predicted')

    def test_termination_is_logged_and_stops_the_child(self):
        environment = {**os.environ, 'PIPELINE_CALLS': str(self.calls),
                       'PAUSE_STAGE': 'fetch-dumps'}
        language_file = self.root / 'languages.json'
        language_file.write_text(json.dumps({'version': 1, 'languages': ['vec']}))
        command = [sys.executable, str(SCRIPT), '--languages-file', str(language_file),
                   '--sql-release', '20261001', '--history-release', '2026-09',
                   '--dump-root', str(self.root / 'dumps'),
                   '--log-dir', str(self.root / 'logs'), '--python', str(self.fake)]
        with subprocess.Popen(command, cwd=ROOT, env=environment,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              text=True) as process:
            deadline = time.monotonic() + 5
            while not self.calls.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(self.calls.exists())
            process.terminate()
            process.communicate(timeout=12)
            self.assertEqual(process.returncode, 143)
        self.assertIn('INTERROTTO SIGTERM', self.one_log())
        report = self.one_benchmark()
        self.assertEqual(report['status'], 'interrupted')
        self.assertEqual(report['steps'][-1]['returncode'], -15)
        self.assertEqual(self.metric_events()[-1]['status'], 'interrupted')
        spans = self.span_events()
        self.assert_closed_span_tree(spans)
        self.assertEqual(spans[-1]['status'], 'interrupted')
        pause = next(event for event in spans if event['name'] == 'mock.pause' and event['event'] == 'span_end')
        self.assertTrue(pause['end_is_detection_bound'])


if __name__ == '__main__':
    unittest.main()
