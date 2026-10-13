import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


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
        'format': 'dashboard_summary_v1', 'histogram_rule': 'shared_p2_p98_tails_v1',
        'metadata': metadata, 'sources': fingerprints, 'census_page_count': 0}))
    (directory / (stem + '-creators.json')).write_text(json.dumps({
        'format': 'creator_index_v1', 'metadata': metadata,
        'sources': fingerprints, 'census_page_count': 0}))
with open(os.environ['PIPELINE_CALLS'], 'a', encoding='utf-8') as calls:
    calls.write(command + ':' + (option('--manifest') if '--manifest' in options else '') + '\\n')
print('fake stdout ' + command, flush=True)
if os.environ.get('PAUSE_STAGE') == command:
    time.sleep(30)
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
        'complete': True, 'history_files': {}}, 'pages': []}))
    stage = Path(option('--work-dir')) / (manifest['database'] + '-' + manifest['sql_release'] + '.sqlite')
    stage.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(stage) as database:
        database.execute('CREATE TABLE IF NOT EXISTS pages(id INTEGER PRIMARY KEY)')
        database.execute('INSERT OR IGNORE INTO pages VALUES(1)')
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
    if (Path(options[0]).with_name(language + '-topics.json')).exists():
        meta['topics'] = {'status': 'predicted', 'model': json.loads(
            Path(options[0]).with_name(language + '-topics.json').read_text())['metadata']['model']}
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
    graphs = [Path(value) for value in options[:options.index('--histories')]]
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
        meta['comparison'] = {'status': 'compared',
                              'reference_languages': [code for code in languages if code != language]}
        if graph.with_name(language + '-topics.json').exists():
            meta['topics'] = {'status': 'predicted', 'model': json.loads(
                graph.with_name(language + '-topics.json').read_text())['metadata']['model']}
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
'''


class DumpPipelineTests(unittest.TestCase):
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

    def invoke(self, *selection, extra=(), fail=None):
        if selection and selection[0] == '--languages-file':
            language_file = selection[1]
        else:
            language_file = self.root / 'languages.json'
            language_file.write_text(json.dumps({'version': 1, 'languages': list(selection),
                                                 'databases': {}}))
        environment = {**os.environ, 'PIPELINE_CALLS': str(self.calls)}
        if fail:
            environment['FAIL_STAGE'] = fail
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

    def test_runs_complete_languages_in_order_and_logs_every_step(self):
        result = self.invoke('vec', 'lmo')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual([line.split(':', 1)[0] for line in calls],
                         ['fetch-dumps', 'import-dumps', 'import-dumps', 'topics', 'compare'])
        self.assertIn('vecwiki', calls[1])
        self.assertIn('lmowiki', calls[2])
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

    def test_missing_detail_directory_rebuilds_comparison(self):
        first = self.invoke('vec', 'lmo')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        (self.root / 'web-data' / 'vec-connections-test').rmdir()
        second = self.invoke('vec', 'lmo')
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        calls = [line.split(':', 1)[0] for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls.count('compare'), 2)
        self.assertEqual(calls.count('import-dumps'), 2)

    def test_missing_or_stale_chart_sidecar_rebuilds_comparison(self):
        first = self.invoke('vec', 'lmo')
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        summary = self.root / 'web-data' / 'vec-summary.json'
        summary.unlink()
        second = self.invoke('vec', 'lmo')
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertEqual(self.calls.read_text().count('compare:'), 2)
        data = json.loads(summary.read_text())
        data['sources'][0]['size'] += 1
        summary.write_text(json.dumps(data))
        third = self.invoke('vec', 'lmo')
        self.assertEqual(third.returncode, 0, third.stdout + third.stderr)
        self.assertEqual(self.calls.read_text().count('compare:'), 3)
        (self.root / 'web-data' / 'all-summary.json').unlink()
        fourth = self.invoke('vec', 'lmo')
        self.assertEqual(fourth.returncode, 0, fourth.stdout + fourth.stderr)
        self.assertEqual(self.calls.read_text().count('compare:'), 4)

    def test_configured_pool_builds_one_aggregate_for_every_language(self):
        config = json.loads((ROOT / 'config' / 'languages.json').read_text())
        result = self.invoke('--languages-file', 'config/languages.json')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        aggregate = json.loads((self.root / 'web-data' / 'all.json').read_text())
        self.assertEqual(aggregate['metadata']['languages'], sorted(config['languages']))
        calls = [line.split(':', 1)[0] for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls.count('import-dumps'), len(config['languages']))
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
                                   files['2026-09.itwiki.all-time.tsv.bz2']['sha256_local'])
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
                         ['fetch-dumps', 'topics', 'analyze'])
        self.assertIn('SALTA importazione it', result.stdout)
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

    def test_plan_only_runs_fetch_and_preserves_import_directory(self):
        result = self.invoke('vec', extra=('--plan',))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.calls.read_text().splitlines(), ['fetch-dumps:'])
        self.assertFalse((self.root / 'imported').exists())
        self.assertIn('PIPELINE COMPLETATA', self.one_log())

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
                          'topics', 'compare'])
        aggregate = json.loads((self.root / 'web-data' / 'all.json').read_text())
        self.assertEqual(aggregate['metadata']['languages'], ['it', 'lmo', 'vec'])
        self.assertTrue((self.root / 'imported' / 'vec-topics.json').exists())
        self.assertTrue((self.root / 'imported' / 'it-topics.json').exists())

    def test_model_runs_for_every_selected_language(self):
        result = self.invoke('it')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([line.split(':', 1)[0] for line in self.calls.read_text().splitlines()],
                         ['fetch-dumps', 'import-dumps', 'topics', 'analyze'])
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


if __name__ == '__main__':
    unittest.main()
