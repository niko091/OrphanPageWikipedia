"""Transport adapter for the native structural category classifier."""


def classify(pages, parents, rules):
    from ._native import classify_categories
    choices = classify_categories(
        [(p['page_id'], p.get('categories', [])) for p in pages], parents, native_rules(rules))
    return format_choices(pages, choices)


def native_rules(rules):
    return (rules.get('exclude_patterns', []), set(rules.get('exclude_categories', [])),
            rules.get('category_priority', {}), rules.get('page_overrides', {}),
            rules.get('min_support', 5))


def format_choices(pages, choices):
    """Give bulk and staged classification the same public fields."""
    return {p['page_id']: {
        'category': category,
        'category_candidates': [dict(zip(('category', 'score', 'support', 'priority'), row)) for row in ranked],
        'category_status': 'reviewed' if manual else ('heuristic_review_needed' if category else 'no_eligible_category'),
        'category_reason': 'Scelta manuale verificata' if manual else 'Categorie visibili; esclusioni configurate; rimozione dei sovratemi; specificità e numerosità. Non è una classificazione semantica validata.',
    } for p, (category, ranked, manual) in zip(pages, choices)}
