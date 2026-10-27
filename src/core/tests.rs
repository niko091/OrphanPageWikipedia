use super::graph::{compute_graph, count_graph};
use super::matching::valid_item;
use super::*;

#[test]
fn unresolved_fanout_cannot_accumulate_in_cross_batch_redirect_caches() {
    use super::{GraphAccumulator, TopicInputAccumulator};
    use std::collections::{HashMap, HashSet};
    let titles = vec!["A".into(), "B".into(), "C".into()];
    let redirects: HashMap<String, String> = [
        ("R", "R2"),
        ("R2", "B"),
        ("X", "Y"),
        ("Y", "X"),
        ("D", "missing"),
    ]
    .into_iter()
    .map(|(a, b)| (a.into(), b.into()))
    .collect();
    let mut graph = GraphAccumulator::new(titles.clone(), redirects.clone(), None).unwrap();
    let mut topics = TopicInputAccumulator::new(
        titles,
        vec![Some("Q1".into()), Some("Q2".into()), Some("Q3".into())],
        redirects.clone(),
        HashSet::from(["Q2".into()]),
    )
    .unwrap();
    let mut first = (0..60000)
        .map(|i| format!("Unavailable {i}"))
        .collect::<Vec<_>>();
    first.extend(["R", "R2", "A", "D", "X"].map(str::to_owned));
    graph.add_batch(vec![first.clone()]).unwrap();
    assert_eq!(
        topics.add_batch(vec![first]).unwrap(),
        vec![("Q2".into(), 1, true)]
    );
    let mut second = (0..1000)
        .map(|i| format!("Unavailable {i}"))
        .collect::<Vec<_>>();
    second.extend(["R", "X", "D"].map(str::to_owned));
    graph
        .add_batch(vec![second.clone(), vec!["R".into()]])
        .unwrap();
    assert_eq!(
        topics.add_batch(vec![second, vec!["R".into()]]).unwrap(),
        vec![("".into(), 0, false), ("Q2".into(), 1, true)]
    );
    assert_eq!(
        graph.finish().unwrap(),
        (vec![0, 2, 0], vec![1, 0, 1], 61004)
    );
    topics.finish().unwrap();
    for cache in [&graph.inner.resolved, &topics.graph.resolved] {
        assert!(cache.len() <= redirects.len());
        assert!(cache.keys().all(|key| redirects.contains_key(key)));
    }
}
#[test]
fn collection_bounds_use_instants_and_reject_invalid_intervals() {
    let start = "2026-01-01T01:00:00+02:00";
    let finish = "2026-01-02T00:00:00Z";
    assert_eq!(
        collection_interval(&[
            ("2026-01-01T00:00:00Z".into(), finish.into()),
            (start.into(), "2026-01-01T00:00:00Z".into()),
        ])
        .unwrap(),
        (start.into(), finish.into())
    );
    assert!(collection_interval(&[]).is_err());
    assert!(collection_interval(&[(finish.into(), start.into())]).is_err());
    assert!(collection_interval(&[("invalid".into(), finish.into())]).is_err());
}
#[test]
fn categories_drop_broader_choices_but_keep_cycles_and_manual_overrides() {
    let pages = vec![
        (
            1,
            vec!["Leaf".into(), "Root".into(), "Pagine da correggere".into()],
        ),
        (2, vec!["CycleA".into(), "CycleB".into()]),
    ];
    let parents = [
        ("Leaf".into(), vec!["Root".into()]),
        ("CycleA".into(), vec!["CycleB".into()]),
        ("CycleB".into(), vec!["CycleA".into()]),
    ]
    .into_iter()
    .collect();
    let rules = (
        vec!["^Pagine".into()],
        HashSet::new(),
        HashMap::new(),
        HashMap::new(),
        5.0,
    );
    let choices = classify_categories(&pages, &parents, rules).unwrap();
    assert_eq!(choices[0].0.as_deref(), Some("Leaf"));
    assert_eq!(choices[0].1.len(), 1);
    assert_eq!(choices[1].1.len(), 2);
    assert_eq!(choices[1].0.as_deref(), Some("CycleA"));
    let manual = (
        vec![],
        HashSet::new(),
        HashMap::new(),
        [("1".into(), "Root".into())].into_iter().collect(),
        5.0,
    );
    let choices = classify_categories(&pages, &parents, manual).unwrap();
    assert_eq!(choices[0].0.as_deref(), Some("Root"));
    assert!(choices[0].2);
}

#[test]
fn categories_validate_rules_and_ties() {
    let pages = vec![(1, vec!["B".into(), "A".into(), "A".into()]), (2, vec![])];
    let choices = classify_categories(
        &pages,
        &HashMap::new(),
        (vec![], HashSet::new(), HashMap::new(), HashMap::new(), 1.0),
    )
    .unwrap();
    assert_eq!(choices[0].0.as_deref(), Some("A"));
    assert_eq!(choices[0].1[0].2, 1);
    assert_eq!(choices[1].0, None);
    for minimum in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(classify_categories(
            &pages,
            &HashMap::new(),
            (
                vec![],
                HashSet::new(),
                HashMap::new(),
                HashMap::new(),
                minimum
            )
        )
        .is_err());
    }
    assert!(classify_categories(
        &pages,
        &HashMap::new(),
        (
            vec!["[".into()],
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            1.0
        )
    )
    .is_err());
    assert!(classify_categories(
        &pages,
        &HashMap::new(),
        (
            vec![],
            HashSet::new(),
            HashMap::new(),
            [("1".into(), "Other".into())].into_iter().collect(),
            1.0
        )
    )
    .is_err());
}

#[test]
fn ages_preserve_missing_offset_fraction_and_future_dates() {
    let ages = article_ages(
        "2026-01-02T00:00:00Z",
        &[
            None,
            Some("2026-01-01T14:00:00+02:00".into()),
            Some("2026-01-02T00:00:00.000001Z".into()),
            Some("2026-01-01T23:59:59.500000Z".into()),
        ],
    )
    .unwrap();
    assert_eq!(ages[0], (None, false));
    assert_eq!(ages[1], (Some(0.5), false));
    assert_eq!(ages[2], (None, true));
    assert!((ages[3].0.unwrap() - 0.5 / 86400.0).abs() < 1e-12);
    assert!(article_ages("bad", &[]).is_err());
    assert!(article_ages(
        "2026-01-02T00:00:00Z",
        &[Some("2026-02-30T00:00:00Z".into())]
    )
    .is_err());
}

#[test]
fn item_matching_requires_unique_valid_ascii_identifiers() {
    let items = [
        Some("Q1".into()),
        Some("Q2".into()),
        Some("Q2".into()),
        Some("Q0".into()),
        Some("Q01".into()),
        Some("Q٣".into()),
        None,
        Some("Q1234567890123456789012345".into()),
    ];
    let (unique, ambiguous) = unique_items(&items);
    assert_eq!(unique.len(), 2);
    assert_eq!(unique.get("Q1"), Some(&0));
    assert_eq!(ambiguous, ["Q2".into()].into_iter().collect());
    for item in ["", "Q", "q1", "Q1x", " Q1", "Q1\n"] {
        assert!(!valid_item(item));
    }
}

#[test]
fn candidate_union_deduplicates_evidence_and_prioritizes_local_dead_ends() {
    let first = (
        1,
        "Z".into(),
        0,
        "lmo".into(),
        11,
        "S".into(),
        12,
        "T".into(),
    );
    let third = (
        1,
        "Z".into(),
        0,
        "fur".into(),
        21,
        "S2".into(),
        22,
        "T2".into(),
    );
    let result = rank_candidates(vec![
        first.clone(),
        first,
        third,
        (
            2,
            "A".into(),
            1,
            "lmo".into(),
            31,
            "S3".into(),
            12,
            "T".into(),
        ),
    ])
    .unwrap();
    assert_eq!(result[0].0, 1);
    assert_eq!(result[0].3.len(), 2);
    assert_eq!(result[1].0, 2);
    assert!(rank_candidates(vec![(
        0,
        "Z".into(),
        0,
        "lmo".into(),
        11,
        "S".into(),
        12,
        "T".into()
    )])
    .is_err());
}

#[test]
fn reverse_adjacency_matches_counts_and_deduplicates_redirects() {
    let (incoming, outgoing, unresolved, sources) = compute_graph(
        vec!["A".into(), "B".into(), "C".into()],
        vec![
            vec!["R".into(), "B".into(), "A".into()],
            vec![],
            vec!["B".into()],
        ],
        [("R".into(), "B".into())].into_iter().collect(),
        true,
    );
    assert_eq!(sources, vec![vec![], vec![0, 2], vec![]]);
    assert_eq!(incoming, vec![0, 2, 0]);
    assert_eq!(outgoing, vec![1, 0, 1]);
    assert_eq!(unresolved, 0);
}
#[test]
fn resolves_chains_deduplicates_and_ignores_self_and_cycles() {
    let redirects = [("R", "R2"), ("R2", "B"), ("X", "Y"), ("Y", "X")]
        .into_iter()
        .map(|(a, b)| (a.into(), b.into()))
        .collect();
    let result = count_graph(
        vec!["A".into(), "B".into(), "C".into()],
        vec![
            vec!["B".into(), "R".into(), "A".into(), "X".into()],
            vec![],
            vec![],
        ],
        redirects,
    );
    assert_eq!(result, (vec![0, 1, 0], vec![1, 0, 0], 1));
}
#[test]
fn empty_graph() {
    assert_eq!(
        count_graph(vec![], vec![], HashMap::new()),
        (vec![], vec![], 0)
    );
}

#[test]
fn isolated_and_self_linked_nodes_are_orphans() {
    assert_eq!(
        count_graph(
            vec!["A".into(), "B".into()],
            vec![vec!["A".into()], vec![]],
            HashMap::new()
        ),
        (vec![0, 0], vec![0, 0], 0)
    );
}

#[test]
fn counts_distinct_sources_not_occurrences() {
    assert_eq!(
        count_graph(
            vec!["A".into(), "B".into(), "C".into()],
            vec![vec!["C".into(), "C".into()], vec!["C".into()], vec![]],
            HashMap::new()
        ),
        (vec![0, 0, 2], vec![1, 1, 0], 0)
    );
}

#[test]
fn dangling_redirect_does_not_create_an_edge() {
    let redirects = [("R".into(), "missing".into())].into_iter().collect();
    assert_eq!(
        count_graph(vec!["A".into()], vec![vec!["R".into()]], redirects),
        (vec![0], vec![0], 1)
    );
}
