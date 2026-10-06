"""Synthetic fixtures and independent graph reference; never research data."""
import copy
import random

def demo():
    rng = random.Random(42)
    topics = ['Comuni', 'Scritori', 'Montagne', 'Fiumi', 'Storia', 'Musica']
    pages = []
    for i in range(240):
        pages.append({'page_id': i + 1, 'title': f'Voce dimostrativa {i + 1}',
                      'categories': [topics[i % 6], 'Cultura' if i % 6 in (1, 5) else 'Mondo'],
                      'length_bytes': int(rng.lognormvariate(8, 1.2)),
                      'created_at': f'{2005 + i % 20}-01-01T00:00:00+00:00',
                      'links': [f'Voce dimostrativa {rng.randrange(1, 170)}' for _ in range(rng.randrange(5))]})
    for page in pages:
        page.update(page_created_at=page['created_at'], first_public_revision_at=page['created_at'],
                    wikibase_item=f"Q{page['page_id']}",
                    creation_date_source='synthetic', creation_date_status='observed')
    return {'metadata': {'language': 'vec', 'source': 'Fixture sintetica, nessuna misura reale di Wikipedia',
                         'creation_date_rule': 'creation_event_v1',
                         'item_mapping': {'source': 'page_props.wikibase_item', 'complete': True},
                         'complete': True, 'demo': True, 'started_at': '2026-09-29T00:00:00+00:00', 'finished_at': '2026-09-29T00:00:00+00:00'},
            'pages': pages, 'redirects': {}, 'category_parents': {'Scritori': ['Cultura'], 'Musica': ['Cultura'], 'Montagne': ['Mondo'], 'Fiumi': ['Mondo'], 'Comuni': ['Mondo'], 'Storia': ['Mondo']}}


def demo_creators(raw):
    language = raw['metadata']['language']
    records = []
    for i, page in enumerate(raw['pages']):
        group = [p for j, p in enumerate(raw['pages']) if j % 15 == i % 15]
        prior = sum(p['created_at'] < page['created_at'] for p in group)
        records.append({
            'page_id': page['page_id'], 'article_created_at': page['created_at'],
            'creator_key': f'demo:{language}:creator:{i % 15}', 'creator_attribution': 'synthetic',
            'creator_account_type': ['registered', 'bot', 'temporary'][i % 3],
            'creator_is_bot_now': i % 3 == 1,
            'creator_tenure_days': prior * 100, 'creator_prior_edits_main': prior * 12,
            'creator_prior_edits_other': prior * 3, 'creator_prior_articles': prior,
            'creator_articles_created_total': len(group),
            'creator_languages_created': [language], 'creator_languages_before': [language] if prior else [],
            'origin': 'translation_tagged' if i % 4 == 0 else 'new_page_unclassified',
        })
    return {'metadata': {'language': language, 'source': 'synthetic', 'demo': True, 'complete': True,
                         'started_at': raw['metadata']['started_at'], 'finished_at': raw['metadata']['finished_at'],
                         'languages_scanned': [language]}, 'pages': records}


def graph_reference(raw):
    titles = [p['title'] for p in raw['pages']]
    index = {title: i for i, title in enumerate(titles)}
    redirects = raw.get('redirects', {})
    incoming, outgoing, sources = [0] * len(titles), [], [[] for _ in titles]
    unresolved = 0
    for source, page in enumerate(raw['pages']):
        targets = set()
        for original in page.get('links', []):
            title, seen = original, set()
            while title in redirects and title not in seen:
                seen.add(title)
                title = redirects[title]
            if title not in index:
                unresolved += 1
            elif index[title] != source:
                targets.add(index[title])
        outgoing.append(len(targets))
        for target in targets:
            incoming[target] += 1
            sources[target].append(source)
    return incoming, outgoing, unresolved, sources


if __name__ == '__main__':
    import argparse
    from pathlib import Path
    from orphanwiki.core import compare_languages
    from orphanwiki.__main__ import export_result
    parser = argparse.ArgumentParser(description='Test fixture export, synthetic data only')
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    raw = demo()
    reference = copy.deepcopy(raw)
    reference['metadata']['language'] = 'lmo'
    for i, page in enumerate(reference['pages']):
        page['links'] = [reference['pages'][(i + 1) % len(reference['pages'])]['title']] if i % 2 == 0 else []
    supplements = {r['metadata']['language']: demo_creators(r) for r in [raw, reference]}
    results = compare_languages([raw, reference], creators=supplements)
    export_result(results['vec'], args.output)
    export_result(results['lmo'], Path(args.output).parent / 'lmo-demo.json')
