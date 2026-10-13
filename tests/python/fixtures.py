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


def add_synthetic_creators(result):
    """Give DOM tests varied creator values without a research supplement."""
    pages = result['pages']
    language = result['metadata']['language']
    for index, page in enumerate(pages):
        group = [other for position, other in enumerate(pages) if position % 15 == index % 15]
        prior = sum(other['created_at'] < page['created_at'] for other in group)
        orphans = sum(other['orphan'] for other in group)
        page.update(creator_key=f'demo:{language}:creator:{index % 15}',
                    creator_attribution='synthetic',
                    creator_account_type=['registered', 'bot', 'temporary'][index % 3],
                    creator_is_bot_now=index % 3 == 1,
                    creator_tenure_days=prior * 100,
                    creator_prior_edits_main=prior * 12,
                    creator_prior_edits_other=prior * 3,
                    creator_prior_articles=prior,
                    creator_articles_created_total=len(group),
                    creator_articles_in_census=len(group),
                    creator_orphans_current=orphans,
                    creator_orphans_current_share=orphans / len(group),
                    creator_languages_created=[language],
                    creator_languages_before=[language] if prior else [],
                    creator_data_status='matched',
                    origin='translation_tagged' if index % 4 == 0 else 'new_page_unclassified')
    result['metadata']['creators'] = {
        'status': 'collected', 'language': language, 'languages_scanned': [language],
        'matched_pages': len(pages), 'unmatched_pages': 0,
        'started_at': result['metadata']['started_at'],
        'finished_at': result['metadata']['finished_at'],
    }


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
    results = compare_languages([raw, reference])
    for result in results.values():
        add_synthetic_creators(result)
    export_result(results['vec'], args.output)
    export_result(results['lmo'], Path(args.output).parent / 'lmo-demo.json')
