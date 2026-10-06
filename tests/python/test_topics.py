"""Local inference and exact-graph joins; synthetic PAWS fixtures only."""
import copy
import hashlib
import json
import tempfile
import time
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from fixtures import demo
from orphanwiki.__main__ import main
from orphanwiki.core import analyze, attach_topics, json_sha256, load_topic_model, predict_topics, write_json


SPEC = json.loads(Path('config/topics.json').read_text())


class Model:
    def __init__(self, scores=None):
        self.scores = scores or {SPEC['labels'][0]: 0.75, SPEC['labels'][1]: 0.5}
        self.calls = []

    def get_labels(self):
        return ['__label__' + label.replace(' ', '_') for label in SPEC['labels']]

    def get_word_id(self, item):
        return -1 if item == 'Q99' else 1

    def predict(self, texts, k, threshold):
        self.calls.append(texts)
        return ([self.get_labels() for _ in texts],
                [[self.scores.get(label, 0.1) for label in SPEC['labels']] for _ in texts])


def graph():
    raw = demo()
    raw['pages'] = [
        {'page_id': 1, 'revision_id': 101, 'title': 'Source', 'wikibase_item': 'Q1',
         'links': ['Source', 'Alias', 'Target', 'Alias2', 'Missing', 'Cycle'], 'categories': []},
        {'page_id': 2, 'revision_id': 102, 'title': 'Target', 'wikibase_item': 'Q2', 'links': [], 'categories': []},
        {'page_id': 3, 'revision_id': 103, 'title': 'Other', 'wikibase_item': 'Q3', 'links': ['Outside'], 'categories': []},
        {'page_id': 4, 'revision_id': 104, 'title': 'Outside', 'wikibase_item': 'Q99', 'links': [], 'categories': []},
    ]
    raw['redirects'] = {'Alias': 'Alias2', 'Alias2': 'Target', 'Cycle': 'Cycle2', 'Cycle2': 'Cycle'}
    return raw


class TopicTests(unittest.TestCase):
    def test_native_inputs_resolve_redirects_deduplicate_and_exclude_self_links(self):
        raw, model = graph(), Model()
        before = json.dumps(raw, sort_keys=True)
        result = predict_topics(raw, model, SPEC, batch_size=1)
        self.assertEqual(model.calls, [['Q2']])
        rows = result['pages']
        self.assertEqual(rows[0]['topic_labels'], [SPEC['labels'][0]])
        self.assertEqual(rows[0]['topic_input_count'], 1)
        self.assertEqual(rows[1]['topic_status'], 'no_mapped_outlinks')
        self.assertEqual(rows[2]['topic_status'], 'no_model_vocabulary')
        self.assertIsNone(rows[2]['topic_labels'])
        self.assertEqual(result['metadata']['predicted_pages'], 1)
        self.assertEqual(result['metadata']['input_sha256'], json_sha256(raw))
        self.assertEqual(json.dumps(raw, sort_keys=True), before)
        self.assertNotIn('url', json.dumps(rows))

    def test_empty_positive_set_is_known_and_distinct_from_missing(self):
        result = predict_topics(graph(), Model({SPEC['labels'][0]: 0.5}), SPEC)
        self.assertEqual(result['pages'][0]['topic_labels'], [])
        self.assertEqual(result['pages'][0]['topic_status'], 'predicted')
        self.assertIsNone(result['pages'][1]['topic_labels'])
        analyzed = analyze(graph(), topics=result)
        self.assertEqual(analyzed['pages'][0]['topic_labels'], [])
        self.assertTrue(analyzed['metadata']['topics']['input_checksum_verified'])

    def test_ambiguous_missing_and_malformed_identifiers_are_not_model_tokens(self):
        raw = graph()
        for item in [None, 'Q0', 'Q1', [], 'invalid']:
            with self.subTest(item=item):
                raw['pages'][1]['wikibase_item'] = item
                model = Model()
                result = predict_topics(raw, model, SPEC)
                self.assertEqual(model.calls, [])
                self.assertEqual(result['pages'][0]['topic_status'], 'no_mapped_outlinks')

    def test_empty_graph_and_bounded_batches(self):
        raw = demo()
        model = Model()
        result = predict_topics(raw, model, SPEC, batch_size=7)
        self.assertEqual(len(result['pages']), len(raw['pages']))
        self.assertTrue(all(len(batch) <= 7 for batch in model.calls))
        raw['pages'] = []
        self.assertEqual(predict_topics(raw, Model(), SPEC)['pages'], [])

    def test_reject_partial_non_paws_and_invalid_batch_inputs(self):
        for change in [lambda r: r['metadata'].update(complete=False),
                       lambda r: r['metadata']['item_mapping'].update(complete=False),
                       lambda r: r['metadata'].update(demo=False, source='external'),
                       lambda r: r['pages'].append(copy.deepcopy(r['pages'][0]))]:
            raw = graph()
            change(raw)
            with self.assertRaises(ValueError):
                predict_topics(raw, Model(), SPEC)
        for size in [0, 10001, True, 2.5]:
            with self.assertRaises(ValueError):
                predict_topics(graph(), Model(), SPEC, size)

    def test_invalid_scores_labels_and_batch_responses_are_rejected(self):
        for score in [float('nan'), float('inf'), -0.1, 1.1]:
            with self.assertRaises(ValueError):
                predict_topics(graph(), Model({SPEC['labels'][0]: score}), SPEC)
        for response in [([], []), ([['__label__bad']], [[0.8]])]:
            model = Model()
            model.predict = lambda *args, **kwargs: response
            with self.assertRaises(ValueError):
                predict_topics(graph(), model, SPEC)

    def test_deadline_refuses_incomplete_results(self):
        with self.assertRaises(TimeoutError):
            predict_topics(graph(), Model(), SPEC, deadline=time.monotonic() - 1)
        model = Model()
        original = model.predict
        def expired(*args, **kwargs):
            answer = original(*args, **kwargs)
            clock[0] = 100
            return answer
        clock = [0]
        model.predict = expired
        with patch('orphanwiki.core.time.monotonic', side_effect=lambda: clock[0]), self.assertRaises(TimeoutError):
            predict_topics(graph(), model, SPEC, deadline=10)

    def test_load_verifies_hash_taxonomy_and_invalid_configurations(self):
        with tempfile.TemporaryDirectory() as folder:
            weights, config = Path(folder) / 'weights.bin', Path(folder) / 'topics.json'
            weights.write_bytes(b'local synthetic model')
            spec = {**SPEC, 'model_sha512': hashlib.sha512(weights.read_bytes()).hexdigest()}
            write_json(config, spec)
            with patch.dict('sys.modules', {'fasttext': types.SimpleNamespace(load_model=lambda path: Model())}):
                model, actual = load_topic_model(weights, config)
                self.assertEqual(actual, spec)
                self.assertEqual(len(model.get_labels()), 64)
                with patch.object(model, 'get_labels', return_value=['__label__bad']), \
                        patch.dict('sys.modules', {'fasttext': types.SimpleNamespace(load_model=lambda path: model)}), \
                        self.assertRaises(ValueError):
                    load_topic_model(weights, config)
            for change in [{'labels': [{}] * 64}, {'labels': ['bad'] * 64}, {'threshold': 0.6},
                           {'model_sha512': None}, {'model_sha512': 'a' * 128}, {'version': 2}]:
                write_json(config, {**spec, **change})
                with self.assertRaises(ValueError):
                    load_topic_model(weights, config)

    def test_exact_graph_join_rejects_stale_and_incomplete_supplements(self):
        raw = graph()
        supplement = predict_topics(raw, Model(), SPEC)
        mutations = [lambda s: s['metadata'].update(language='lmo'),
                     lambda s: s['metadata'].update(complete=False),
                     lambda s: s['metadata'].update(source='external'),
                     lambda s: s['metadata'].update(input_sha256='stale'),
                     lambda s: s['metadata'].update(demo=False),
                     lambda s: s['pages'].pop(),
                     lambda s: s['pages'].append(copy.deepcopy(s['pages'][0])),
                     lambda s: s['pages'][0].update(revision_id=9),
                     lambda s: s['pages'][0].update(topic_labels=['invalid']),
                     lambda s: s['pages'][0].update(topic_labels=[SPEC['labels'][0]] * 2),
                     lambda s: s['pages'][0].update(topic_labels=None),
                     lambda s: s['pages'][0].update(topic_input_count=0),
                     lambda s: s['pages'][1].update(topic_status='unknown_status'),
                     lambda s: s['metadata']['model'].update(labels=[{}] * 64)]
        for change in mutations:
            with self.subTest(change=change):
                bad = copy.deepcopy(supplement)
                change(bad)
                with self.assertRaises(ValueError):
                    attach_topics(analyze(raw)['pages'], raw, bad)

    def test_cli_configured_languages_and_automatic_analyze_compare_joins(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for language in ['vec', 'lmo']:
                raw = graph()
                raw['metadata']['language'] = language
                write_json(root / f'{language}-raw.json', raw)
            config = root / 'languages.json'
            write_json(config, {'version': 1, 'languages': ['it', 'vec', 'lmo'], 'topic_languages': ['vec', 'lmo']})
            with patch('orphanwiki.__main__.load_topic_model', return_value=(Model(), SPEC)), \
                    patch('sys.argv', ['orphanwiki', 'topics', '--languages-file', str(config),
                                       '--input-dir', folder, '--output-dir', folder]):
                main()
            report = json.loads((root / 'topic-run.json').read_text())
            self.assertEqual(report['completed_languages'], ['vec', 'lmo'])
            self.assertEqual(report['deferred_languages'], [])
            with patch('sys.argv', ['orphanwiki', 'analyze', str(root / 'vec-raw.json'), '--output', str(root / 'analysis.json')]):
                main()
            self.assertEqual(json.loads((root / 'analysis.json').read_text())['pages'][0]['topic_status'], 'predicted')
            with patch('sys.argv', ['orphanwiki', 'compare', '--input-dir', folder, '--output-dir', str(root / 'web')]):
                main()
            for language in ['vec', 'lmo']:
                result = json.loads((root / 'web' / f'{language}-demo.json').read_text())
                self.assertEqual(result['pages'][0]['topic_labels'], [SPEC['labels'][0]])
                self.assertTrue(result['metadata']['topics']['input_checksum_verified'])
            self.assertIn('topic_labels', (root / 'web' / 'vec-demo.csv').read_text().splitlines()[0])

    def test_cli_timeout_preserves_existing_complete_checkpoint(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            write_json(root / 'vec-raw.json', graph())
            checkpoint = root / 'vec-topics.json'
            checkpoint.write_text('existing complete checkpoint')
            with patch('orphanwiki.__main__.load_topic_model', return_value=(Model(), SPEC)), \
                    patch('orphanwiki.__main__.predict_topics', side_effect=TimeoutError), \
                    patch('sys.argv', ['orphanwiki', 'topics', '--languages', 'vec', '--input-dir', folder, '--output-dir', folder]):
                main()
            self.assertEqual(checkpoint.read_text(), 'existing complete checkpoint')
            report = json.loads((root / 'topic-run.json').read_text())
            self.assertEqual(report['completed_languages'], [])
            self.assertEqual(report['deferred_languages'], ['vec'])

    def test_missing_explicit_topic_file_is_an_error(self):
        with tempfile.TemporaryDirectory() as folder:
            raw = Path(folder) / 'vec-raw.json'
            write_json(raw, graph())
            with patch('sys.argv', ['orphanwiki', 'analyze', str(raw), '--topics', str(Path(folder) / 'missing.json')]), \
                    self.assertRaises(ValueError):
                main()
