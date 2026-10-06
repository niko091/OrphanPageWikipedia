from fixtures import graph_reference
import copy
import json
import os
import random
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

from orphanwiki._native import graph_details
from orphanwiki.core import analyze, compare_files, compare_languages


def fixture(language, links):
    titles = [f'{language}-{i}' for i in range(1, 7)]
    return {'metadata': {'language': language, 'source': 'Wiki Replicas / ' + language + 'wiki',
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
            for name in ['manifest.json', 'vec-demo.json', 'lmo-demo.csv']:
                (output / name).write_text('old')
            def fail(result, path, **options):
                if path.name == 'vec-demo.json':
                    raise OSError('synthetic disk full')
                return export_result(result, path, **options)
            with patch('orphanwiki.__main__.export_result', side_effect=fail), self.assertRaises(OSError):
                publish_comparison(results, output)
            self.assertTrue(all(p.read_text() == 'old' for p in output.iterdir()))
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
