from fixtures import graph_reference
import copy
import csv
import json
import os
import random
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path
import sqlite3

from orphanwiki._native import graph_details
from orphanwiki.core import analyze, compare_files, compare_languages
from orphanwiki.core import (_comparison_analysis, _comparison_schema, _stage_comparison,
                             _finish_comparison, _StagedPages, _StagedCandidates)
from orphanwiki.__main__ import export_result
from orphanwiki.__main__ import _matching_aggregate_report, publish_comparison, publish_available_aggregate
from orphanwiki.core import write_aggregate_summary


def fixture(language, links):
    titles = [f'{language}-{i}' for i in range(1, 7)]
    return {'metadata': {'language': language, 'source': 'Synthetic complete graph',
                        'complete': True, 'demo': True,
                        'started_at': '2026-01-01T00:00:00Z', 'finished_at': '2026-01-02T00:00:00Z',
                        'item_mapping': {'source': 'page_props.wikibase_item', 'complete': True}},
            'pages': [{'page_id': i, 'title': title, 'wikibase_item': f'Q{i}',
                       'links': [f'{language}-{target}' for target in links.get(i, [])]}
                      for i, title in enumerate(titles, 1)], 'redirects': {}}


def failed_comparison_worker(*args):
    """Synthetic abrupt worker death: no exception/checkpoint can be sent."""
    os._exit(9)


class ComparisonTests(unittest.TestCase):
    def test_scalar_counterparts_are_looked_up_per_orphan_block(self):
        connection = sqlite3.connect(':memory:')
        try:
            _comparison_schema(connection)
            for language in ('vec', 'lmo'):
                raw = fixture(language, {})
                raw['pages'] = [{'page_id': pid, 'title': f'{language}-{pid}',
                                 'wikibase_item': f'Q{pid}', 'links': []}
                                for pid in range(1, 514)]
                _stage_comparison(connection, _comparison_analysis(raw, {}, None))
            selects = []
            connection.set_trace_callback(lambda statement: selects.append(statement)
                                          if statement.lstrip().upper().startswith('SELECT') else None)
            results = _finish_comparison(connection)
            connection.set_trace_callback(None)
            self.assertEqual(sum(len(result['link_candidates']) for result in results.values()), 1026)
            self.assertLess(len(selects), 1300)
            self.assertEqual(results['vec']['link_candidates']['513']['candidate_count'], 0)
        finally:
            connection.close()

    def test_identical_available_aggregate_reuses_prepared_reports(self):
        results = {}
        for language in ('vec', 'lmo'):
            result = analyze(fixture(language, {2: [1]}))
            result['metadata'].update(demo=False, database=language + 'wiki',
                                      source='Wikimedia SQL dumps / ' + language + 'wiki',
                                      sql_release='20261001')
            results[language] = result
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            with patch('orphanwiki.__main__.write_aggregate_summary', wraps=write_aggregate_summary) as calculate:
                publish_comparison(results, root)
                self.assertEqual(calculate.call_count, 1)
            manifest = json.loads((root / 'manifest.json').read_text())
            aggregates = [entry for entry in manifest if entry.get('kind') == 'aggregate']
            self.assertEqual(len(aggregates), 2)
            full = next(entry for entry in aggregates if '-available-' not in entry['file'])
            available = next(entry for entry in aggregates if '-available-' in entry['file'])
            full_summary = json.loads((root / full['summary_file']).read_text())
            available_summary = json.loads((root / available['summary_file']).read_text())
            self.assertEqual(full_summary['reports'], available_summary['reports'])
            self.assertEqual(full_summary['article_preview'], available_summary['article_preview'])
            self.assertEqual(json.loads((root / full['creator_index_file']).read_text())['creators'],
                             json.loads((root / available['creator_index_file']).read_text())['creators'])
            members = [entry for entry in manifest if entry.get('language') in results]
            sources = [{'language': entry['language'], 'path': root / entry['file'],
                        'page_count': len(results[entry['language']]['pages']),
                        'metadata': results[entry['language']]['metadata']}
                       for entry in sorted(members, key=lambda item: item['language'])]
            oracle = write_aggregate_summary(sources, root / 'oracle-summary.json', full_summary['metadata'])
            self.assertEqual(json.loads(oracle['reports'])['categories'][:20], full_summary['reports']['categories'])
            self.assertEqual(json.loads(oracle['reports'])['creators'],
                             json.loads((root / available['creator_index_file']).read_text())['creators'])
            changed_rule = {**available_summary['metadata'], 'measures_rule': 'different-definition'}
            self.assertIsNone(_matching_aggregate_report(oracle, changed_rule, oracle['sources'], oracle['count']))
            (root / next(entry['file'] for entry in manifest if entry.get('language') == 'vec')).touch()
            with patch('orphanwiki.__main__.write_aggregate_summary', wraps=write_aggregate_summary) as calculate:
                publish_available_aggregate(root)
                self.assertTrue(any(len(call.args[0]) == 2 for call in calculate.call_args_list))

    def test_csv_staging_uses_scalar_candidates_without_decoding_evidence(self):
        with tempfile.TemporaryDirectory() as folder:
            db = sqlite3.connect(':memory:')
            try:
                _comparison_schema(db)
                pages = [{'page_id': 3, 'title': 'Third'}, {'page_id': 1, 'title': 'First'},
                         {'page_id': 2, 'title': 'Second'}]
                db.executemany('INSERT INTO pages VALUES(?,?,?,?,?,?,?,?)',
                               (('vec', p['page_id'], pos, None, p['title'], 0, 0, json.dumps(p))
                                for pos, p in enumerate(pages)))
                records = [(1, 'compared', 0), (3, 'missing_item', None)]
                db.executemany('INSERT INTO candidates VALUES(?,?,?,?,?)',
                               (('vec', pid, status, count, '{invalid evidence JSON}')
                                for pid, status, count in records))
                result = {'metadata': {'language': 'vec'}, 'pages': _StagedPages(db, 'vec', 3),
                          'link_candidates': _StagedCandidates(db, 'vec')}
                output = Path(folder) / 'rows.json'
                with patch('orphanwiki.__main__.write_json'), \
                        patch('orphanwiki.__main__.write_dashboard_exports', return_value={}):
                    export_result(result, output, update_manifest=False, announce=False)
                with output.with_suffix('.csv').open(newline='', encoding='utf-8') as stream:
                    rows = list(csv.DictReader(stream))
                self.assertEqual([(r['page_id'], r['candidate_status'], r['candidate_count']) for r in rows],
                                 [('3', 'missing_item', ''), ('1', 'compared', '0'), ('2', '', '')])
            finally:
                db.close()

    def test_abrupt_worker_death_is_reported_and_staging_is_cleaned(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            paths = []
            for language in ['vec', 'lmo']:
                path = root / f'{language}-raw.json'
                path.write_text(json.dumps(fixture(language, {})))
                paths.append(path)
            with patch('orphanwiki.core._comparison_file_worker', failed_comparison_worker), \
                    self.assertRaisesRegex(RuntimeError, 'exit 9'):
                with compare_files(paths, work_dir=root):
                    self.fail('No completed comparison should be returned')
            self.assertFalse(list(root.glob('orphanwiki-compare-*')))

    def test_staged_files_preserve_all_observations_and_reference_evidence(self):
        raws = [fixture('vec', {2: [4]}), fixture('lmo', {2: [1], 3: [1]}), fixture('en', {2: [1]})]
        raws[0]['pages'][0]['wikibase_item'] = 'Q6'
        raws[1]['pages'][1]['links'] += ['Alias', 'Alias', 'Cycle', 'lmo-2']
        raws[1]['redirects'] = {'Alias': 'lmo-1', 'Cycle': 'Cycle'}
        for raw in raws:
            raw['pages'].reverse()  # Output preserves source order, not numeric IDs.
        expected = compare_languages(raws)
        with tempfile.TemporaryDirectory() as folder:
            work = Path(folder) / 'stage';work.mkdir()
            inputs = []
            originals = []
            for raw in raws:
                path = Path(folder) / (raw['metadata']['language'] + '-raw.json')
                path.write_text(json.dumps(raw));inputs.append(path);originals.append(path.read_bytes())
            with compare_files(inputs, work_dir=work) as results:
                actual = {language: {**result, 'pages': list(result['pages']),
                                     'link_candidates': dict(result['link_candidates'].items())}
                          for language, result in results.items()}
                self.assertEqual(actual, expected)
                self.assertTrue(list(work.glob('orphanwiki-compare-*')))
            self.assertEqual(list(work.iterdir()), [])
            self.assertEqual([p.read_bytes() for p in inputs], originals)

    def test_staged_comparison_preserves_history_creator_and_age_parity(self):
        raws = [fixture('vec', {2: [1]}), fixture('lmo', {2: [1]})]
        histories = {}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            inputs, supplements = [], []
            for raw in raws:
                language = raw['metadata']['language']
                database = language + 'wiki'
                history = {'metadata': {'language': language, 'database': database,
                           'source': 'MediaWiki History dumps / ' + database,
                           'history_release': '2026-09', 'complete': True},
                           'pages': [{'page_id': 1,
                                      'article_created_at': '2025-01-01T00:00:00+00:00',
                                      'creator_attribution': 'history_initial_revision_observed',
                                      'creator_history_status': 'observed_surviving_history',
                                      'creator_key': database + ':history-user:7',
                                      'creator_prior_edits_main_history': 4}]}
                histories[language] = history
                source = root / f'{language}-raw.json'
                supplement = root / f'{language}-history.json'
                source.write_text(json.dumps(raw))
                supplement.write_text(json.dumps(history))
                inputs.append(source)
                supplements.append(supplement)
            expected = compare_languages(raws, histories=histories)
            with compare_files(inputs, work_dir=root, histories=supplements) as results:
                actual = {language: {**result, 'pages': list(result['pages']),
                                     'link_candidates': dict(result['link_candidates'].items())}
                          for language, result in results.items()}
            self.assertEqual(actual, expected)
            first = actual['vec']['pages'][0]
            self.assertIsNone(first['age_days'])
            self.assertGreater(first['history_first_revision_age_days'], 0)
            self.assertEqual(first['history_creator_key'], 'vecwiki:history-user:7')

    def test_worker_failure_preserves_old_exports_and_cleans_staging(self):
        from orphanwiki.__main__ import main
        import sys
        for failure in ['incomplete', 'duplicate', 'mixed_demo']:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as folder:
                root = Path(folder);output = root / 'web';output.mkdir();work = root / 'stage';work.mkdir()
                for file in ['manifest.json', 'vec.json', 'vec.csv']:
                    (output / file).write_text('previous complete export')
                a, b = fixture('vec', {}), fixture('lmo', {})
                if failure == 'incomplete': b['metadata']['complete'] = False
                if failure == 'duplicate': b = copy.deepcopy(a)
                if failure == 'mixed_demo': b['metadata']['demo'] = False
                paths = []
                for name, raw in [('a', a), ('b', b)]:
                    path = root / (name + '.json');path.write_text(json.dumps(raw));paths.append(str(path))
                with patch('sys.argv', ['orphanwiki', 'compare', *paths, '--output-dir', str(output), '--work-dir', str(work)]), \
                        self.assertRaises(ValueError):
                    main()
                self.assertEqual(list(work.iterdir()), [])
                self.assertTrue(all(p.read_text() == 'previous complete export' for p in output.iterdir()))

    def test_cli_rejects_output_overlapping_an_input_census(self):
        from orphanwiki.__main__ import main
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            paths = []
            for language in ['vec', 'lmo']:
                path = root / f'{language}-demo.json'
                path.write_text(json.dumps(fixture(language, {})))
                paths.append(path)
            originals = [p.read_bytes() for p in paths]
            with patch('sys.argv', ['orphanwiki', 'compare', *map(str, paths), '--output-dir', folder]), \
                    self.assertRaisesRegex(ValueError, 'distinti dagli input'):
                main()
            self.assertEqual([p.read_bytes() for p in paths], originals)
            self.assertFalse(list(root.glob('orphanwiki-compare-*')))

    def test_preparation_failure_does_not_publish_a_partial_language_set(self):
        from orphanwiki.__main__ import export_result, publish_comparison
        results = compare_languages([fixture('vec', {}), fixture('lmo', {})])
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'web';output.mkdir()
            (output / 'manifest.json').write_text('[]')
            for name in ['vec-demo.json', 'lmo-demo.csv']:
                (output / name).write_text('old')
            def fail(result, path, **options):
                if path.name == 'vec-demo.json':
                    raise OSError('synthetic disk full')
                return export_result(result, path, **options)
            with patch('orphanwiki.__main__.export_result', side_effect=fail), self.assertRaises(OSError):
                publish_comparison(results, output)
            self.assertEqual((output / 'manifest.json').read_text(), '[]')
            self.assertTrue(all((output / name).read_text() == 'old' for name in ('vec-demo.json', 'lmo-demo.csv')))
            self.assertFalse(list(Path(folder).glob('orphanwiki-publish-*')))

    def test_empty_wikis_missing_items_and_zero_candidates_in_staged_matching(self):
        a, b = fixture('vec', {}), fixture('lmo', {})
        a['pages'] = []
        b['pages'][0]['wikibase_item'] = None
        result = compare_languages([a, b])
        self.assertEqual(result['vec']['pages'], [])
        self.assertEqual(result['lmo']['link_candidates']['1']['status'], 'missing_item')
        self.assertIsNone(result['lmo']['link_candidates']['2']['candidate_count'])
        self.assertEqual(result['lmo']['link_candidates']['2']['status'], 'counterpart_unavailable')

    def test_local_dead_end_sources_rank_first(self):
        a, b = fixture('vec', {2: [4]}), fixture('lmo', {2: [1], 3: [1]})
        candidates = compare_languages([a, b])['vec']['link_candidates']['1']['candidates']
        self.assertEqual([c['page_id'] for c in candidates], [3, 2])
        self.assertTrue(candidates[0]['dead_end'])
        self.assertEqual(candidates[0]['out_degree'], 0)
        self.assertFalse(candidates[1]['dead_end'])
        self.assertEqual(candidates[1]['out_degree'], 1)
        # Reference sources both have outgoing links; ranking is based on A.
        self.assertTrue(all(p['links'] for p in b['pages'][1:3]))

    def test_bidirectional_with_evidence_and_distinct_sources(self):
        a, b = fixture('vec', {1: [2]}), fixture('lmo', {2: [1, 1], 3: [1]})
        b['pages'][1]['links'] += ['Alias', 'Cycle', 'missing', 'lmo-2']
        b['redirects'] = {'Alias': 'Alias2', 'Alias2': 'lmo-1', 'Cycle': 'Cycle'}
        result = compare_languages([a, b])
        to_a = result['vec']['link_candidates']['1']
        self.assertEqual(to_a['candidate_count'], 2)
        self.assertEqual([c['title'] for c in to_a['candidates']], ['vec-2', 'vec-3'])
        self.assertEqual(to_a['candidates'][0]['evidence'][0]['source_title'], 'lmo-2')
        self.assertEqual(to_a['candidates'][0]['evidence'][0]['target_title'], 'lmo-1')
        self.assertEqual(result['lmo']['link_candidates']['2']['candidate_count'], 1)
        self.assertNotIn('2', result['vec']['link_candidates'])  # target is not orphan
        self.assertNotIn('_incoming_sources', result['vec'])
        self.assertEqual(result, compare_languages([b, a]))
        collections = result['vec']['metadata']['comparison']['collections']
        self.assertEqual(len(collections), 2)
        self.assertTrue(all(len(c['input_sha256']) == 64 for c in collections))

    def test_missing_ambiguous_and_zero_are_different(self):
        a, b = fixture('vec', {}), fixture('lmo', {2: [1]})
        a['pages'][0]['wikibase_item'] = None
        a['pages'][3]['wikibase_item'] = 'Q3'
        b['pages'][4]['wikibase_item'] = 'Q6'
        result = compare_languages([a, b])['vec']['link_candidates']
        self.assertIsNone(result['1']['candidate_count'])
        self.assertEqual(result['1']['status'], 'missing_item')
        self.assertEqual(result['3']['status'], 'ambiguous_item')
        self.assertEqual(result['5']['status'], 'counterpart_unavailable')
        self.assertEqual(result['6']['counterparts'][0]['status'], 'ambiguous_item')
        self.assertEqual(result['2']['candidate_count'], 0)
        self.assertEqual(result['2']['counterparts'][0]['status'], 'orphan')

    def test_excludes_sources_without_unambiguous_local_equivalent(self):
        a, b = fixture('vec', {}), fixture('lmo', {2: [1], 3: [1], 4: [1]})
        a['pages'][1]['wikibase_item'] = None
        b['pages'][4]['wikibase_item'] = 'Q3'
        result = compare_languages([a, b])['vec']['link_candidates']['1']
        self.assertEqual(result['candidate_count'], 1)
        self.assertEqual(result['candidates'][0]['page_id'], 4)

    def test_multiple_reference_languages_deduplicate_local_candidates(self):
        raws = [fixture('vec', {}), fixture('lmo', {2: [1]}), fixture('it', {2: [1]})]
        result = compare_languages(raws)['vec']['link_candidates']['1']
        self.assertEqual(result['candidate_count'], 1)
        self.assertEqual(len(result['candidates'][0]['evidence']), 2)

    def test_invalid_collection_contracts(self):
        a, b = fixture('vec', {}), fixture('lmo', {})
        for mutate in [lambda r: r['metadata'].pop('item_mapping'),
                       lambda r: r['metadata'].update(complete=False),
                       lambda r: r['metadata'].update(language='vec'),
                       lambda r: r['metadata'].update(language='../escape'),
                       lambda r: r['metadata'].update(demo=False)]:
            invalid = copy.deepcopy(b)
            mutate(invalid)
            with self.assertRaises(ValueError):
                compare_languages([a, invalid])
        with self.assertRaises(ValueError): compare_languages([a])
        a['metadata']['demo'] = b['metadata']['demo'] = False
        b['metadata']['source'] = 'external'
        with self.assertRaises(ValueError): compare_languages([a, b])

    def test_native_reverse_graph_randomized_against_reference(self):
        rng = random.Random(526)
        for _ in range(80):
            raw = fixture('vec', {})
            raw['pages'] = raw['pages'][:rng.randrange(7)]
            for page in raw['pages']:
                page['links'] = [f'vec-{rng.randrange(1, 12)}' for _ in range(rng.randrange(15))]
            raw['redirects'] = {f'vec-{i}': f'vec-{rng.randrange(1, 12)}' for i in range(7, 12)}
            native = analyze(raw, include_sources=True)
            incoming, outgoing, unresolved, sources = graph_reference(raw)
            self.assertEqual([p['in_degree'] for p in native['pages']], incoming)
            self.assertEqual([p['out_degree'] for p in native['pages']], outgoing)
            self.assertEqual(native['metadata']['unresolved_target_occurrences'], unresolved)
            self.assertEqual(native['_incoming_sources'], sources)
            for page, sources in zip(native['pages'], native['_incoming_sources']):
                self.assertEqual(page['in_degree'], len(sources))
        with self.assertRaises(ValueError): graph_details(['A'], [], {})
        with self.assertRaises(ValueError): graph_details(['A', 'A'], [[], []], {})
        self.assertEqual(graph_details([], [], {}), ([], [], 0, []))

    def test_compare_cli_exports_both_languages_and_manifest(self):
        with tempfile.TemporaryDirectory() as folder:
            paths = []
            for raw in [fixture('vec', {1: [2]}), fixture('lmo', {2: [1]})]:
                path = Path(folder) / (raw['metadata']['language'] + '.json')
                path.write_text(json.dumps(raw))
                paths.append(str(path))
            import sys
            subprocess.run([sys.executable, '-m', 'orphanwiki', 'compare', *paths,
                            '--output-dir', folder], check=True, capture_output=True)
            manifest = json.loads((Path(folder) / 'manifest.json').read_text())
            self.assertEqual({m['language'] for m in manifest}, {'vec', 'lmo', 'all'})
            aggregate = json.loads((Path(folder) / 'all-demo.json').read_text())
            self.assertEqual([m['language'] for m in aggregate['members']], ['lmo', 'vec'])
            self.assertNotIn('pages', aggregate)
            for language in ['vec', 'lmo']:
                result = json.loads((Path(folder) / f'{language}-demo.json').read_text())
                self.assertEqual(result['metadata']['comparison']['status'], 'compared')
                self.assertTrue((Path(folder) / f'{language}-demo.csv').exists())

    def test_cli_stages_each_wiki_without_loading_all_raws_together(self):
        from orphanwiki.__main__ import main
        import sys
        with tempfile.TemporaryDirectory() as folder:
            for raw in [fixture('vec', {1: [2]}), fixture('lmo', {2: [1]}), fixture('en', {2: [1]})]:
                path = Path(folder) / (raw['metadata']['language'] + '-raw.json')
                path.write_text(json.dumps(raw))
            with patch('orphanwiki.core.compare_languages', side_effect=AssertionError('No all-raws call')), \
                    patch('sys.argv', ['orphanwiki', 'compare', '--input-dir', folder, '--output-dir', str(Path(folder) / 'web')]):
                main()
            result = json.loads((Path(folder) / 'web' / 'vec-demo.json').read_text())
            self.assertEqual(result['link_candidates']['1']['candidate_count'], 1)
            self.assertEqual({e['language'] for e in result['link_candidates']['1']['candidates'][0]['evidence']}, {'lmo', 'en'})
