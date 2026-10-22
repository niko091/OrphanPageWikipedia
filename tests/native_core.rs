//! Public Rust consumers must work with Python disabled, including bounded paths.
use orphanwiki::{
    classify_categories, graph_counts, graph_details, matchable_edges, threshold_topics,
    topic_inputs, unique_items, CategoryBatchClassifier, GraphAccumulator, TopicInputAccumulator,
};
use std::collections::{HashMap, HashSet};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}

#[test]
fn bounded_graph_and_topics_preserve_all_edges_across_batch_boundaries() {
    let titles = strings(&["Source", "Target", "Duplicate QID", "Unknown", "Isolated"]);
    let items = [Some("Q1"), Some("Q2"), Some("Q2"), Some("Q99"), None]
        .map(|value| value.map(str::to_owned))
        .to_vec();
    let redirects = [
        ("Alias", "Chain"),
        ("Chain", "Target"),
        ("X", "Y"),
        ("Y", "X"),
    ]
    .map(|(source, target)| (source.into(), target.into()))
    .into_iter()
    .collect::<HashMap<_, _>>();
    let links = vec![
        strings(&["Alias", "Target", "Source", "Unknown", "X", "missing"]),
        strings(&["Source", "Isolated"]),
        strings(&["Source", "Alias"]),
        strings(&[]),
        strings(&[]),
    ];
    let (incoming, outgoing, unresolved, sources) =
        graph_details(titles.clone(), links.clone(), redirects.clone()).unwrap();
    let counts = (incoming, outgoing, unresolved);
    assert_eq!(counts, (vec![2, 2, 0, 1, 1], vec![2, 2, 2, 0, 0], 2));
    let (unique, ambiguous) = unique_items(&items);
    assert_eq!(ambiguous, [String::from("Q2")].into_iter().collect());
    let eligible: Vec<_> = unique.values().copied().collect();
    let expected_edges = matchable_edges(titles.len(), unique.clone(), sources.clone()).unwrap();
    assert_eq!(expected_edges, [(3, 0)]);

    for batch_size in 1..=links.len() {
        let mut graph =
            GraphAccumulator::new(titles.clone(), redirects.clone(), Some(eligible.clone()))
                .unwrap();
        let mut edges = Vec::new();
        assert!(graph.finish().is_err());
        graph.add_batch(Vec::new()).unwrap();
        for batch in links.chunks(batch_size) {
            edges.extend(graph.add_batch_edges(batch.to_vec()).unwrap());
        }
        edges.sort_unstable();
        assert_eq!(edges, expected_edges);
        assert_eq!(graph.finish().unwrap(), counts);
        assert!(graph.add_batch(vec![vec![]]).is_err());
        assert_eq!(graph.finish().unwrap(), counts);

        for known in [HashSet::new(), [String::from("Q99")].into_iter().collect()] {
            let mut topics = TopicInputAccumulator::new(
                titles.clone(),
                items.clone(),
                redirects.clone(),
                known.clone(),
            )
            .unwrap();
            assert!(topics.finish().is_err());
            let mut actual = Vec::new();
            for batch in links.chunks(batch_size) {
                actual.extend(topics.add_batch(batch.to_vec()).unwrap());
            }
            topics.finish().unwrap();
            assert_eq!(
                actual,
                topic_inputs(titles.len(), unique.clone(), known, sources.clone()).unwrap()
            );
            assert!(topics.add_batch(vec![vec![]]).is_err());
            topics.finish().unwrap();
        }
    }
}

#[test]
fn categories_use_complete_supports_cycles_exclusions_and_overrides_in_every_batch_size() {
    let pages = vec![
        (1, strings(&["Leaf", "Root", "Maintenance"])),
        (2, strings(&["Cycle A", "Cycle B"])),
        (3, strings(&["Leaf", "Leaf"])),
        (4, strings(&[])),
    ];
    let parents = [
        (String::from("Leaf"), strings(&["Root"])),
        (String::from("Cycle A"), strings(&["Cycle B"])),
        (String::from("Cycle B"), strings(&["Cycle A"])),
    ]
    .into_iter()
    .collect::<HashMap<_, _>>();
    let rules = (
        strings(&["^Maintenance$"]),
        HashSet::new(),
        [(String::from("Cycle B"), 1.0)].into_iter().collect(),
        [(String::from("1"), String::from("Root"))]
            .into_iter()
            .collect(),
        5.0,
    );
    let expected = classify_categories(&pages, &parents, rules.clone()).unwrap();
    assert_eq!(expected[0].0.as_deref(), Some("Root"));
    assert!(expected[0].2);
    assert_eq!(expected[0].1.len(), 1);
    assert_eq!(expected[0].1[0].2, 2);
    assert_eq!(expected[1].0.as_deref(), Some("Cycle B"));
    assert_eq!(expected[1].1.len(), 2);
    assert_eq!(expected[3], (None, vec![], false));
    for batch_size in 1..=pages.len() {
        let mut classifier =
            CategoryBatchClassifier::new(pages.len(), parents.clone(), rules.clone()).unwrap();
        assert!(classifier.classify_batch(pages.clone()).is_err());
        for batch in pages.chunks(batch_size) {
            classifier
                .add_support_batch(
                    batch
                        .iter()
                        .map(|(_, categories)| categories.clone())
                        .collect(),
                )
                .unwrap();
        }
        assert!(classifier.add_support_batch(vec![vec![]]).is_err());
        let mut actual = Vec::new();
        for batch in pages.chunks(batch_size) {
            actual.extend(classifier.classify_batch(batch.to_vec()).unwrap());
        }
        assert_eq!(actual, expected);
    }
}

#[test]
fn rust_entry_points_reject_invalid_inputs_without_panics() {
    let titles = strings(&["Duplicate", "Duplicate"]);
    assert!(graph_counts(titles.clone(), vec![vec![], vec![]], HashMap::new()).is_err());
    assert!(graph_details(strings(&["A"]), vec![], HashMap::new()).is_err());
    assert!(GraphAccumulator::new(titles.clone(), HashMap::new(), None).is_err());
    assert!(GraphAccumulator::new(strings(&["A"]), HashMap::new(), Some(vec![1])).is_err());
    assert!(
        TopicInputAccumulator::new(titles, vec![None, None], HashMap::new(), HashSet::new())
            .is_err()
    );
    assert!(
        TopicInputAccumulator::new(vec![], vec![None], HashMap::new(), HashSet::new()).is_err()
    );
    for (unique, sources) in [
        (
            [(String::from("Q1"), 1)].into_iter().collect(),
            vec![vec![]],
        ),
        (HashMap::new(), vec![vec![1]]),
        (HashMap::new(), vec![]),
    ] {
        assert!(matchable_edges(1, unique.clone(), sources.clone()).is_err());
        assert!(topic_inputs(1, unique, HashSet::new(), sources).is_err());
    }
    let mut empty_graph = GraphAccumulator::new(vec![], HashMap::new(), None).unwrap();
    empty_graph.add_batch(vec![]).unwrap();
    assert_eq!(empty_graph.finish().unwrap(), (vec![], vec![], 0));
    let empty_topics =
        TopicInputAccumulator::new(vec![], vec![], HashMap::new(), HashSet::new()).unwrap();
    empty_topics.finish().unwrap();
    let mut empty_categories = CategoryBatchClassifier::new(
        0,
        HashMap::new(),
        (vec![], HashSet::new(), HashMap::new(), HashMap::new(), 5.0),
    )
    .unwrap();
    assert_eq!(empty_categories.classify_batch(vec![]).unwrap(), vec![]);
}

#[test]
fn all_topic_labels_are_required_and_half_is_a_known_negative() {
    let taxonomy = (0..64)
        .map(|index| format!("Topic {index}"))
        .collect::<HashSet<_>>();
    let mut labels = taxonomy
        .iter()
        .map(|label| format!("__label__{}", label.replace(' ', "_")))
        .collect::<Vec<_>>();
    labels.sort();
    let mut probabilities = vec![0.5; 64];
    assert_eq!(
        threshold_topics(
            taxonomy.clone(),
            vec![labels.clone()],
            vec![probabilities.clone()]
        )
        .unwrap(),
        vec![Vec::<String>::new()]
    );
    probabilities[0] = f64::from_bits(0.5f64.to_bits() + 1);
    assert_eq!(
        threshold_topics(
            taxonomy.clone(),
            vec![labels.clone()],
            vec![probabilities.clone()]
        )
        .unwrap(),
        vec![vec![String::from("Topic 0")]]
    );
    for invalid in [f64::NAN, f64::INFINITY, -0.1, 1.01] {
        probabilities[0] = invalid;
        assert!(threshold_topics(
            taxonomy.clone(),
            vec![labels.clone()],
            vec![probabilities.clone()]
        )
        .is_err());
    }
    probabilities[0] = 0.5;
    labels[0] = labels[1].clone();
    assert!(threshold_topics(taxonomy, vec![labels], vec![probabilities]).is_err());
}
