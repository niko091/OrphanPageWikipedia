import copy
import hashlib
import json
import random
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

from orphanwiki._native import graph_counts
from fixtures import demo, graph_reference
from orphanwiki.__main__ import export, export_result, rebuild_dashboard, main
from orphanwiki.categories import classify
from orphanwiki.core import analyze, write_json, json_sha256, write_observations, iter_export_sections, write_dashboard_exports, OBSERVATION_COLUMNS


class GraphTests(unittest.TestCase):
    def test_native_empty_and_invalid(self):
        self.assertEqual(graph_counts([], [], {}), ([], [], 0))
        with self.assertRaises(ValueError):
            graph_counts(['A'], [], {})
        with self.assertRaises(ValueError):
            graph_counts(['A', 'A'], [[], []], {})

    def test_graph_semantics(self):
        counts = graph_counts(['A', 'B', 'C'], [['A', 'B', 'R', 'R2', 'missing', 'X'], [], ['A']],
                              {'R': 'R2', 'R2': 'B', 'X': 'Y', 'Y': 'X'})
        self.assertEqual(counts, ([1, 1, 0], [1, 0, 1], 2))

    def test_long_chain(self):
        redirects = {f'R{i}': f'R{i+1}' for i in range(10000)}
        redirects['R10000'] = 'B'
        self.assertEqual(graph_counts(['A', 'B'], [['R0', 'R5000'], []], redirects), ([0, 1], [1, 0], 0))

    def test_randomized_against_python(self):
        rng = random.Random(731)
        for run in range(120):
            raw = demo()
            count = rng.randrange(0, 60)
            raw['pages'] = [{'page_id': i, 'title': f'P{i}', 'links': [f'P{rng.randrange(0, 90)}' for _ in range(rng.randrange(30))]} for i in range(count)]
            raw['redirects'] = {f'P{i}': f'P{rng.randrange(90)}' for i in range(count, 85)}
            native = analyze(raw)
            incoming, outgoing, unresolved, _ = graph_reference(raw)
            self.assertEqual([p['in_degree'] for p in native['pages']], incoming, f'run {run}')
            self.assertEqual([p['out_degree'] for p in native['pages']], outgoing, f'run {run}')
            self.assertEqual(native['metadata']['unresolved_target_occurrences'], unresolved)

    def test_external_enrichment_rejected(self):
        for location in ('metadata', 'page'):
            raw = demo()
            if location == 'metadata':
                raw['metadata']['biographies'] = {'source': 'external'}
            else:
                raw['pages'][0]['biography'] = {'gender_group': 'women'}
            with self.subTest(location=location), self.assertRaisesRegex(ValueError, 'fonte esterna'):
                analyze(raw)

    def test_no_biography_in_plain_export(self):
        result = analyze(demo())
        self.assertNotIn('biographies', result)
        self.assertTrue(all('gender_group' not in page for page in result['pages']))

    def test_partial_census_rejected(self):
        raw = demo()
        raw['metadata']['complete'] = False
        with self.assertRaises(ValueError):
            analyze(raw)

    def test_missing_and_continuous_features(self):
        raw = demo()
        raw['pages'] = [{'page_id': 1, 'title': 'A'}, {'page_id': 2, 'title': 'B', 'page_created_at': '2026-09-28T12:00:00Z', 'length_bytes': 0}]
        rows = analyze(raw)['pages']
        self.assertIsNone(rows[0]['age_days'])
        self.assertIsNone(rows[0]['length_bytes'])
        self.assertEqual(rows[1]['age_days'], 0.5)
        self.assertEqual(rows[1]['length_bytes'], 0)
        self.assertTrue(all(r['orphan'] for r in rows))

    def test_creation_event_age_and_legacy_data(self):
        raw = demo()
        page = raw['pages'][0]
        page.update(page_created_at='2026-09-28T12:00:00Z', created_at='2001-01-01T00:00:00Z',
                    first_public_revision_at='2001-01-01T00:00:00Z')
        result = analyze(raw)
        self.assertEqual(result['pages'][0]['age_days'], 0.5)
        self.assertEqual(result['metadata']['age_rule'], 'page_creation_event')
        page['page_created_at'] = None
        self.assertIsNone(analyze(raw)['pages'][0]['age_days'])  # no fallback to first revision
        page['page_created_at'] = '2030-01-01T00:00:00Z'
        self.assertIsNone(analyze(raw)['pages'][0]['age_days'])
        del raw['metadata']['creation_date_rule']
        result = analyze(raw)
        self.assertEqual(result['metadata']['age_rule'], 'first_public_revision_legacy')
        self.assertGreater(result['pages'][0]['age_days'], 9000)

    def test_duplicate_ids(self):
        raw = demo()
        raw['pages'][1]['page_id'] = raw['pages'][0]['page_id']
        with self.assertRaises(ValueError):
            analyze(raw)

    def test_duplicate_titles_rejected(self):
        raw = demo()
        raw['pages'][1]['title'] = raw['pages'][0]['title']
        with self.assertRaises(ValueError):
            analyze(raw)


class CategoryTests(unittest.TestCase):
    def page(self, categories, pid=1):
        return {'page_id': pid, 'categories': categories}

    def test_invalid_support(self):
        for support in [0, -1, float('nan')]:
            with self.assertRaises(ValueError):
                classify([self.page(['A'])], {}, {'min_support': support})

    def test_narrower_category_preferred(self):
        result = classify([self.page(['Mondo', 'Comuni'])], {'Comuni': ['Geografia'], 'Geografia': ['Mondo']}, {})
        self.assertEqual(result[1]['category'], 'Comuni')

    def test_cycles_terminate_and_ties_deterministic(self):
        result = classify([self.page(['B', 'A'])], {'A': ['B'], 'B': ['A']}, {})
        self.assertEqual(result[1]['category'], 'A')
        self.assertEqual(len(result[1]['category_candidates']), 2)

    def test_exclusion_missing_and_duplicates(self):
        result = classify([self.page(['Wikipedia:Test', 'Maintenance', 'Topic', 'Topic'])], {},
                          {'exclude_patterns': ['^Wikipedia:'], 'exclude_categories': ['Maintenance']})
        self.assertEqual(result[1]['category'], 'Topic')
        self.assertEqual(result[1]['category_candidates'][0]['support'], 1)
        result = classify([self.page([])], {}, {})
        self.assertIsNone(result[1]['category'])

    def test_overrides_and_priority(self):
        p = [self.page(['A', 'B'])]
        self.assertEqual(classify(p, {}, {'category_priority': {'B': 10}})[1]['category'], 'B')
        result = classify(p, {}, {'page_overrides': {'1': 'B'}})[1]
        self.assertEqual(result['category_status'], 'reviewed')
        self.assertEqual(result['category'], 'B')
        with self.assertRaises(ValueError):
            classify(p, {}, {'page_overrides': {'1': 'Absent'}})

    def test_rare_category_penalty(self):
        pages = [self.page(['Singleton', 'Useful'])]
        pages += [self.page(['Useful'], i) for i in range(2, 6)]
        pages += [self.page(['Other'], i) for i in range(6, 101)]
        self.assertEqual(classify(pages, {}, {'min_support': 5})[1]['category'], 'Useful')

    def test_does_not_use_orphan_status(self):
        pages = [self.page(['A', 'B'])]
        a = classify(pages, {}, {})
        pages[0]['orphan'] = True
        self.assertEqual(a, classify(pages, {}, {}))


class ExportTests(unittest.TestCase):
    def test_streamed_pages_and_candidate_objects_preserve_unicode_and_missing_values(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'output.json'
            value = {'metadata': {'complete': True}, 'pages': None, 'link_candidates': None}
            pages = [{'title': 'Venèto 🌙', 'age_days': None}, {'title': 'Second', 'age_days': 0}]
            candidates = {'2': {'candidate_count': None}, '1': {'candidate_count': 0}}
            write_json(path, value, arrays={'pages': iter(pages)}, mappings={'link_candidates': iter(candidates.items())})
            self.assertEqual(json.loads(path.read_text()), {**value, 'pages': pages, 'link_candidates': candidates})
            self.assertIsNone(value['pages'])
            for entries in [[('1', {}), ('1', {})], [(2, {})], [('1', {'bad': float('nan')})]]:
                before = path.read_bytes()
                with self.assertRaises(ValueError):
                    write_json(path, value, mappings={'link_candidates': iter(entries)})
                self.assertEqual(path.read_bytes(), before)
                self.assertFalse(path.with_suffix('.json.tmp').exists())
            with self.assertRaises(ValueError):
                write_json(path, value, arrays={'pages': []}, mappings={'pages': []})

    def test_streaming_json_and_checksum_preserve_existing_encoding(self):
        value = {'unicode': ['Venèto', '🌙', 'a\nb', '"quoted"'],
                 'nested': [{'missing': None, 'count': 0, 'flag': False, 'age': 1.25}] * 10000}
        expected = json.dumps(value, ensure_ascii=False, allow_nan=False)
        checksum = hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()
        with tempfile.TemporaryDirectory() as folder, patch('json.dumps', side_effect=AssertionError('Whole-document serialization prohibited')):
            path = Path(folder) / 'output.json'
            write_json(path, value)
            self.assertEqual(path.read_text(), expected)
            self.assertEqual(json_sha256(value), checksum)
            self.assertFalse(path.with_suffix('.json.tmp').exists())

    def test_failed_streaming_write_preserves_previous_file(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'output.json'
            for bad in [float('nan'), float('inf'), object()]:
                with self.subTest(value=type(bad).__name__):
                    path.write_text('{"previous": true}')
                    with self.assertRaises((TypeError, ValueError)):
                        write_json(path, {'pages': ['x' * 1000] * 100, 'invalid': bad})
                    self.assertEqual(json.loads(path.read_text()), {'previous': True})
                    self.assertFalse(path.with_suffix('.json.tmp').exists())

    def test_failed_atomic_replace_preserves_previous_file(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'output.json'
            path.write_text('{"previous": true}')
            with patch('orphanwiki.core.Path.replace', side_effect=OSError('fixture disk error')):
                with self.assertRaisesRegex(OSError, 'fixture disk error'):
                    write_json(path, {'next': 1})
            self.assertEqual(json.loads(path.read_text()), {'previous': True})
            self.assertFalse(path.with_suffix('.json.tmp').exists())

    def test_real_export_removes_demos_from_manifest(self):
        with tempfile.TemporaryDirectory() as folder:
            manifest = Path(folder) / 'manifest.json'
            manifest.write_text(json.dumps([
                {'file': 'demo.json', 'demo': True},
                {'file': 'lmo.json', 'demo': False},
            ]))
            result = analyze(demo())
            result['metadata']['demo'] = False  # synthetic stand-in for a real export
            export_result(result, Path(folder) / 'vec.json')
            entries = json.loads(manifest.read_text())
            self.assertEqual({entry['file'] for entry in entries}, {'lmo.json', 'vec.json'})
            self.assertTrue(all(not entry['demo'] for entry in entries))

    def test_export_reproducible_and_manifest_upsert(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'demo.json'
            raw = demo()
            original = copy.deepcopy(raw)
            export(raw, path, {})
            first = path.read_bytes()
            export(raw, path, {})
            self.assertEqual(path.read_bytes(), first)
            self.assertEqual(raw, original)
            self.assertEqual(len(json.loads((Path(folder) / 'manifest.json').read_text())), 1)
            self.assertEqual(len(path.with_suffix('.csv').read_text().splitlines()), 241)
            self.assertTrue(json.loads(first)['metadata']['demo'])

    def test_compact_observations_preserve_values_counts_and_original_input(self):
        result = analyze(demo())
        result['pages'][0]['topic_labels'] = ['Culture.Media.Music', 'Culture.Media.Films']
        result['pages'][0]['topic_status'] = 'predicted'
        result['pages'][0]['creator_is_bot_now'] = False
        result['pages'][1]['length_bytes'] = None
        result['link_candidates'] = {'1': {'status': 'compared', 'candidate_count': 0, 'candidates': []},
                                     '2': {'status': 'missing_item', 'candidate_count': None, 'candidates': []}}
        before = copy.deepcopy(result)
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'compact.json'
            write_observations(path, result)
            compact = json.loads(path.read_text())
        self.assertEqual(result, before)
        self.assertEqual(compact['metadata'], result['metadata'])
        self.assertEqual(compact['format'], 'observations_v1')
        self.assertEqual(len(compact['rows']), len(result['pages']))
        for page, row in zip(result['pages'], compact['rows']):
            self.assertEqual(dict(zip(compact['columns'], row)), {key: page.get(key) for key in OBSERVATION_COLUMNS})
        self.assertEqual(compact['link_candidates'], {key: {'status': value['status'], 'candidate_count': value['candidate_count']}
                                                     for key, value in result['link_candidates'].items()})

    def test_existing_dashboard_conversion_streams_and_preserves_complete_files(self):
        result = analyze(demo())
        orphan = next(p for p in result['pages'] if p['orphan'])
        result['link_candidates'] = {str(orphan['page_id']): {'status': 'compared', 'candidate_count': 1,
            'candidates': [{'title': 'Unicode ł \\" ' + 'x' * 180000, 'evidence': []}]}}
        original = copy.deepcopy(result)
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'fixture.json'
            export_result(result, path)
            complete = path.read_bytes()
            csv_bytes = path.with_suffix('.csv').read_bytes()
            compact = path.with_name('fixture-observations.json')
            expected = compact.read_bytes()
            compact.unlink()
            self.assertEqual(list(iter_export_sections(path)), [('metadata', None, result['metadata'])] +
                             [('pages', None, p) for p in result['pages']] +
                             [('link_candidates', str(orphan['page_id']), result['link_candidates'][str(orphan['page_id'])])])
            with patch('sys.argv', ['orphanwiki', 'dashboard', '--data-dir', folder]):
                main()
            self.assertEqual(compact.read_bytes(), expected)
            self.assertEqual(path.read_bytes(), complete)
            self.assertEqual(path.with_suffix('.csv').read_bytes(), csv_bytes)
            self.assertEqual(json.loads((Path(folder) / 'manifest.json').read_text())[0]['observations_file'], compact.name)
            self.assertFalse(list(Path(folder).glob('*.tmp')))
        self.assertEqual(result, original)

    def test_dashboard_conversion_failure_preserves_old_publications(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            result = analyze(demo())
            export_result(result, root / 'first.json')
            manifest = json.loads((root / 'manifest.json').read_text())
            bad = copy.deepcopy(manifest[0]);bad.update(file='second.json', language='lmo')
            write_json(root / 'manifest.json', manifest + [bad])
            (root / 'second.json').write_text('{"metadata":')
            old = {str(p.relative_to(root)): p.read_bytes() for p in root.rglob('*') if p.is_file()}
            with self.assertRaisesRegex(ValueError, 'incompleta'):
                rebuild_dashboard(root)
            self.assertEqual({str(p.relative_to(root)): p.read_bytes() for p in root.rglob('*') if p.is_file()}, old)
        for content in ['{"metadata":{},"pages":[', '{"pages":[],"metadata":{}}', '{"metadata":{},"pages":[]} garbage']:
            with tempfile.TemporaryDirectory() as folder:
                path = Path(folder) / 'bad.json';path.write_text(content)
                with self.assertRaises(ValueError):
                    list(iter_export_sections(path))

    def test_compact_projection_does_not_hide_external_enrichment(self):
        result = analyze(demo())
        result['pages'][0]['gender_group'] = None
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'compact.json';path.write_text('old')
            with self.assertRaisesRegex(ValueError, 'esterno'):
                write_observations(path, result)
            self.assertEqual(path.read_text(), 'old')

    def test_editorial_transports_preserve_counts_and_complete_reference_evidence(self):
        result = analyze(demo())
        orphan = next(page for page in result['pages'] if page['orphan'])
        identifier = str(orphan['page_id'])
        candidates = [
            {'page_id': 100 + i, 'title': f'Candidate {i:02d} ł', 'out_degree': 0 if i < 8 else 2,
             'dead_end': i < 8, 'evidence': [
                 {'language': language, 'source_page_id': 1000 + i, 'source_title': f'Source {i}',
                  'target_page_id': 900, 'target_title': 'Reference target'}
                 for language in ['lmo', 'fur', 'nap', 'co', 'pms', 'scn', 'sc']]}
            for i in range(20)]
        result['link_candidates'] = {identifier: {'status': 'compared', 'candidate_count': 20, 'candidates': candidates}}
        before = copy.deepcopy(result)
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            export_result(result, root / 'wiki.json')
            entry = json.loads((root / 'manifest.json').read_text())[0]
            cards = json.loads((root / entry['cards_file']).read_text())
            self.assertEqual(cards['census_page_count'], len(result['pages']))
            self.assertEqual(cards['pages'], [p for p in result['pages'] if p['orphan']])
            overview = cards['link_candidates'][identifier]
            self.assertEqual(overview['candidate_count'], 20)
            self.assertEqual([c['page_id'] for c in overview['candidates']],
                             [c['page_id'] for c in candidates[:5] + candidates[8:13]])
            self.assertTrue(all(len(c['evidence']) == 1 for c in overview['candidates']))
            detail = json.loads((root / entry['connections_dir'] / (identifier + '.json')).read_text())
            self.assertEqual(detail['pages'], [orphan])
            self.assertEqual(detail['metadata'], result['metadata'])
            self.assertEqual(detail['census_page_count'], len(result['pages']))
            self.assertEqual(detail['link_candidates'], result['link_candidates'])
            # A missing comparison stays missing, and still has a valid detail route.
            other = next((p for p in cards['pages'] if p['page_id'] != orphan['page_id']), None)
            if other:
                missing = json.loads((root / entry['connections_dir'] / f"{other['page_id']}.json").read_text())
                self.assertEqual(missing['link_candidates'][str(other['page_id'])], {})
            canonical = (root / 'wiki.json').read_bytes()
            rebuild_dashboard(root)
            self.assertEqual((root / 'wiki.json').read_bytes(), canonical)
            self.assertEqual(json.loads((root / entry['cards_file']).read_text()), cards)
            self.assertEqual(json.loads((root / entry['connections_dir'] / (identifier + '.json')).read_text()), detail)
        self.assertEqual(result, before)

    def test_editorial_shortlists_skip_invalid_evidence_without_changing_detail(self):
        result = analyze(demo())
        orphan = next(page for page in result['pages'] if page['orphan'])
        identifier = str(orphan['page_id'])
        valid = {'page_id': 100, 'title': 'Valid', 'dead_end': True, 'evidence': [
            {'language': 'lmo', 'source_title': 'Source', 'target_title': 'Target'}]}
        invalid = [{'page_id': orphan['page_id'], 'title': 'Self', 'evidence': valid['evidence']},
                   {'page_id': 9, 'title': 'No evidence', 'evidence': []},
                   {'page_id': 10, 'title': 'Unsafe', 'evidence': [{'language': '../bad', 'source_title': 'X', 'target_title': 'Y'}]}]
        record = {'status': 'compared', 'candidate_count': 4, 'candidates': invalid + [valid]}
        result['link_candidates'] = {identifier: record}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            export_result(result, root / 'wiki.json')
            entry = json.loads((root / 'manifest.json').read_text())[0]
            cards = json.loads((root / entry['cards_file']).read_text())
            self.assertEqual(cards['link_candidates'][identifier]['candidates'], [valid])
            detail = json.loads((root / entry['connections_dir'] / (identifier + '.json')).read_text())
            self.assertEqual(detail['link_candidates'][identifier], record)
            with self.assertRaisesRegex(ValueError, 'Ordine'):
                write_dashboard_exports(root / entry['cards_file'], root)


if __name__ == '__main__':
    unittest.main()
