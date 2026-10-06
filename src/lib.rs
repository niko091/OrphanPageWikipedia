pub mod dashboard;

#[cfg(feature = "python")]
mod native {
    use pyo3::prelude::*;
    use std::collections::{HashMap, HashSet};

    type Counts = (Vec<u64>, Vec<u64>, usize);
    type GraphDetails = (Vec<u64>, Vec<u64>, usize, Vec<Vec<usize>>);

    // Expected O(V + E + R) hash-table work, plus string hashing/copying.
    // One bulk call releases the GIL; memoization resolves redirect paths once.
    // Input strings are copied by PyO3. Adjacency and redirect caches stay in memory.
    fn count_graph(
        titles: Vec<String>,
        links: Vec<Vec<String>>,
        redirects: HashMap<String, String>,
    ) -> Counts {
        let (incoming, outgoing, unresolved, _) = compute_graph(titles, links, redirects, false);
        (incoming, outgoing, unresolved)
    }

    // Reverse adjacency costs O(E) additional indices only when requested.
    fn compute_graph(
        titles: Vec<String>,
        links: Vec<Vec<String>>,
        redirects: HashMap<String, String>,
        keep_sources: bool,
    ) -> GraphDetails {
        let index: HashMap<&str, usize> = titles
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), i))
            .collect();
        let mut resolved: HashMap<String, Option<usize>> = HashMap::new();
        let mut incoming = vec![0u64; titles.len()];
        let mut outgoing = vec![0u64; titles.len()];
        let mut unresolved = 0;
        let mut sources = if keep_sources {
            vec![Vec::new(); titles.len()]
        } else {
            Vec::new()
        };
        for (source, targets) in links.iter().enumerate() {
            let mut distinct = HashSet::with_capacity(targets.len());
            for original in targets {
                let destination = if let Some(found) = resolved.get(original) {
                    *found
                } else {
                    let mut current = original.as_str();
                    let mut visited = HashSet::new();
                    let mut path = Vec::new();
                    let found = loop {
                        if let Some(value) = resolved.get(current) {
                            break *value;
                        }
                        if !visited.insert(current) {
                            break None;
                        }
                        path.push(current);
                        if let Some(next) = redirects.get(current) {
                            current = next;
                        } else {
                            break index.get(current).copied();
                        }
                    };
                    for item in path {
                        resolved.insert(item.to_owned(), found);
                    }
                    found
                };
                match destination {
                    Some(target) if target != source => {
                        distinct.insert(target);
                    }
                    Some(_) => {}
                    None => unresolved += 1,
                }
            }
            outgoing[source] = distinct.len() as u64;
            for target in distinct {
                incoming[target] += 1;
                if keep_sources {
                    sources[target].push(source);
                }
            }
        }
        (incoming, outgoing, unresolved, sources)
    }

    fn validate_graph(titles: &[String], links: &[Vec<String>]) -> PyResult<()> {
        if titles.len() != links.len() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "titles/links size mismatch",
            ));
        }
        if titles.iter().collect::<HashSet<_>>().len() != titles.len() {
            return Err(pyo3::exceptions::PyValueError::new_err("duplicate titles"));
        }
        Ok(())
    }

    #[pyfunction]
    fn graph_counts(
        py: Python<'_>,
        titles: Vec<String>,
        links: Vec<Vec<String>>,
        redirects: HashMap<String, String>,
    ) -> PyResult<Counts> {
        validate_graph(&titles, &links)?;
        Ok(py.detach(move || count_graph(titles, links, redirects)))
    }

    #[pyfunction]
    fn graph_details(
        py: Python<'_>,
        titles: Vec<String>,
        links: Vec<Vec<String>>,
        redirects: HashMap<String, String>,
    ) -> PyResult<GraphDetails> {
        validate_graph(&titles, &links)?;
        Ok(py.detach(move || compute_graph(titles, links, redirects, true)))
    }

    // Analytical routines use typed bulk inputs. Python handles files/SQL/model I/O;
    // all computation below runs without the GIL. Memory is linear in observations
    // and returned evidence, except category ancestor sets (reachable hierarchy).
    type RankedCategory = (String, f64, u64, f64);
    type CategoryChoice = (Option<String>, Vec<RankedCategory>, bool);

    type CategoryRules = (
        Vec<String>,
        HashSet<String>,
        HashMap<String, f64>,
        HashMap<String, String>,
        f64,
    );

    #[pyfunction]
    fn classify_categories(
        py: Python<'_>,
        pages: Vec<(i64, Vec<String>)>,
        parents: HashMap<String, Vec<String>>,
        rules: CategoryRules,
    ) -> PyResult<Vec<CategoryChoice>> {
        py.detach(move || compute_categories(&pages, &parents, rules))
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }

    fn compute_categories(
        pages: &[(i64, Vec<String>)],
        parents: &HashMap<String, Vec<String>>,
        rules: CategoryRules,
    ) -> Result<Vec<CategoryChoice>, String> {
        let (patterns, excluded, priority, overrides, min_support) = rules;
        if !min_support.is_finite() || min_support <= 0.0 {
            return Err(String::from(
                "min_support deve essere un numero positivo finito",
            ));
        }
        if priority.values().any(|v| !v.is_finite()) {
            return Err(String::from("category priority must be finite"));
        }
        let patterns = patterns
            .iter()
            .map(|pattern| {
                regex::RegexBuilder::new(pattern)
                    .case_insensitive(true)
                    .build()
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;

        let mut frequencies = HashMap::<&str, u64>::new();
        for (_, categories) in pages {
            for category in categories.iter().collect::<HashSet<_>>() {
                *frequencies.entry(category).or_default() += 1;
            }
        }
        let mut ancestors = HashMap::<&str, HashSet<&str>>::new();
        for category in frequencies.keys() {
            let mut seen = HashSet::new();
            let mut pending = parents
                .get(*category)
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect::<Vec<_>>();
            while let Some(item) = pending.pop() {
                if seen.insert(item) {
                    pending.extend(parents.get(item).into_iter().flatten().map(String::as_str));
                }
            }
            ancestors.insert(category, seen);
        }
        pages
            .iter()
            .map(|(id, categories)| {
                let candidates = categories
                    .iter()
                    .map(String::as_str)
                    .filter(|c| !excluded.contains(*c) && !patterns.iter().any(|p| p.is_match(c)))
                    .collect::<HashSet<_>>();
                let mut ranked = Vec::new();
                for c in &candidates {
                    let broader = candidates.iter().any(|d| {
                        c != d
                            && ancestors.get(d).is_some_and(|a| a.contains(c))
                            && !ancestors.get(c).is_some_and(|a| a.contains(d))
                    });
                    if !broader {
                        let support = frequencies.get(c).copied().unwrap_or_default();
                        let score = ((pages.len() as f64 + 1.0) / (support as f64 + 1.0)).ln()
                            * (support as f64 / min_support).min(1.0);
                        ranked.push((
                            (*c).to_owned(),
                            (score * 1e6).round_ties_even() / 1e6,
                            support,
                            priority.get(*c).copied().unwrap_or_default(),
                        ));
                    }
                }
                ranked.sort_by(|a, b| {
                    b.3.total_cmp(&a.3)
                        .then_with(|| b.1.total_cmp(&a.1))
                        .then_with(|| a.0.cmp(&b.0))
                });
                let manual = overrides.get(&id.to_string());
                if manual.is_some_and(|c| !categories.contains(c)) {
                    return Err(format!(
                        "Override {id}: categoria non appartenente alla voce"
                    ));
                }
                let choice = manual
                    .cloned()
                    .or_else(|| ranked.first().map(|c| c.0.clone()));
                Ok((choice, ranked, manual.is_some()))
            })
            .collect()
    }

    fn timestamp(value: &str) -> Result<chrono::DateTime<chrono::FixedOffset>, String> {
        chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|error| format!("Invalid timestamp {value:?}: {error}"))
    }

    #[pyfunction]
    fn article_ages(
        py: Python<'_>,
        reference: String,
        created: Vec<Option<String>>,
    ) -> PyResult<Vec<(Option<f64>, bool)>> {
        py.detach(move || compute_ages(&reference, &created))
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }

    fn compute_ages(
        reference: &str,
        created: &[Option<String>],
    ) -> Result<Vec<(Option<f64>, bool)>, String> {
        let reference = timestamp(reference)?;
        created
            .iter()
            .map(|value| match value {
                None => Ok((None, false)),
                Some(value) => {
                    let date = timestamp(value)?;
                    let duration = reference.signed_duration_since(date);
                    let days = duration.num_seconds() as f64 / 86400.0
                        + duration.subsec_nanos() as f64 / 86_400_000_000_000.0;
                    Ok((if days >= 0.0 { Some(days) } else { None }, days < 0.0))
                }
            })
            .collect()
    }

    type CreatorDates = (Option<String>, Option<String>, bool, Option<String>);
    #[pyfunction]
    fn creator_matches(py: Python<'_>, dates: Vec<CreatorDates>) -> PyResult<Vec<(bool, bool)>> {
        py.detach(move || match_creators(&dates))
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }

    fn match_creators(dates: &[CreatorDates]) -> Result<Vec<(bool, bool)>, String> {
        dates
            .iter()
            .map(|(local, first, has_creation, creation)| {
                let parse = |value: &Option<String>| value.as_deref().map(timestamp).transpose();
                let local = parse(local)?;
                let first = parse(first)?;
                let matches = local.is_some() && local == first;
                let unconfirmed =
                    matches && *has_creation && (creation.is_none() || parse(creation)? != first);
                Ok((matches, unconfirmed))
            })
            .collect()
    }

    type CreatorCount = (Option<u64>, Option<u64>, Option<f64>);
    #[pyfunction]
    fn creator_counts(py: Python<'_>, rows: Vec<(Option<String>, bool)>) -> Vec<CreatorCount> {
        py.detach(move || count_creators(&rows))
    }

    fn count_creators(rows: &[(Option<String>, bool)]) -> Vec<CreatorCount> {
        let mut counts = HashMap::<&str, (u64, u64)>::new();
        for (key, orphan) in rows {
            if let Some(key) = key.as_deref().filter(|k| !k.is_empty()) {
                let pair = counts.entry(key).or_default();
                pair.0 += 1;
                pair.1 += u64::from(*orphan);
            }
        }
        rows.iter()
            .map(|(key, _)| {
                key.as_deref().and_then(|k| counts.get(k)).map_or(
                    (None, None, None),
                    |&(total, orphans)| {
                        (
                            Some(total),
                            Some(orphans),
                            Some(orphans as f64 / total as f64),
                        )
                    },
                )
            })
            .collect()
    }

    fn valid_item(item: &str) -> bool {
        let bytes = item.as_bytes();
        bytes.len() > 1
            && bytes[0] == b'Q'
            && matches!(bytes[1], b'1'..=b'9')
            && bytes[2..].iter().all(u8::is_ascii_digit)
    }
    fn item_index(items: &[Option<String>]) -> (HashMap<String, usize>, HashSet<String>) {
        let mut unique = HashMap::new();
        let mut ambiguous = HashSet::new();
        for (index, item) in items.iter().enumerate() {
            if let Some(item) = item.as_ref().filter(|q| valid_item(q)) {
                if unique.insert(item.clone(), index).is_some() {
                    ambiguous.insert(item.clone());
                }
            }
        }
        unique.retain(|q, _| !ambiguous.contains(q));
        (unique, ambiguous)
    }
    #[pyfunction]
    fn unique_items(
        py: Python<'_>,
        items: Vec<Option<String>>,
    ) -> (HashMap<String, usize>, HashSet<String>) {
        py.detach(move || item_index(&items))
    }
    #[pyfunction]
    fn matchable_edges(
        py: Python<'_>,
        size: usize,
        unique: HashMap<String, usize>,
        incoming: Vec<Vec<usize>>,
    ) -> PyResult<Vec<(usize, usize)>> {
        if incoming.len() != size
            || unique.values().any(|&i| i >= size)
            || incoming.iter().flatten().any(|&i| i >= size)
        {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "invalid adjacency indices",
            ));
        }
        Ok(py.detach(move || {
            let eligible: HashSet<_> = unique.into_values().collect();
            incoming
                .iter()
                .enumerate()
                .filter(|(target, _)| eligible.contains(target))
                .flat_map(|(target, sources)| {
                    sources
                        .iter()
                        .filter(|source| eligible.contains(source))
                        .map(move |&source| (target, source))
                })
                .collect()
        }))
    }
    #[pyfunction]
    fn topic_inputs(
        py: Python<'_>,
        size: usize,
        unique: HashMap<String, usize>,
        known: HashSet<String>,
        incoming: Vec<Vec<usize>>,
    ) -> PyResult<Vec<(String, usize, bool)>> {
        if incoming.len() != size
            || unique.values().any(|&i| i >= size)
            || incoming.iter().flatten().any(|&i| i >= size)
        {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "invalid adjacency indices",
            ));
        }
        Ok(py.detach(move || {
            let mut inputs = vec![Vec::<String>::new(); size];
            let mut mapped = vec![false; size];
            for (item, index) in unique {
                for &source in &incoming[index] {
                    mapped[source] = true;
                    if known.contains(&item) {
                        inputs[source].push(item.clone());
                    }
                }
            }
            inputs
                .into_iter()
                .zip(mapped)
                .map(|(mut tokens, mapped)| {
                    tokens.sort();
                    tokens.dedup();
                    (tokens.join(" "), tokens.len(), mapped)
                })
                .collect()
        }))
    }
    #[pyfunction]
    fn threshold_topics(
        py: Python<'_>,
        taxonomy: HashSet<String>,
        labels: Vec<Vec<String>>,
        probabilities: Vec<Vec<f64>>,
    ) -> PyResult<Vec<Vec<String>>> {
        py.detach(move || {
            if taxonomy.len() != 64 || labels.len() != probabilities.len() {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "Risposta del modello incompleta",
                ));
            }
            labels
                .into_iter()
                .zip(probabilities)
                .map(|(labels, probabilities)| {
                    let labels = labels
                        .into_iter()
                        .map(|s| s.strip_prefix("__label__").unwrap_or(&s).replace('_', " "))
                        .collect::<Vec<_>>();
                    if labels.len() != 64
                        || probabilities.len() != 64
                        || labels.iter().cloned().collect::<HashSet<_>>() != taxonomy
                    {
                        return Err(pyo3::exceptions::PyValueError::new_err(
                            "Predizioni senza tutti i 64 temi",
                        ));
                    }
                    if probabilities
                        .iter()
                        .any(|p| !p.is_finite() || !(0.0..=1.0001).contains(p))
                    {
                        return Err(pyo3::exceptions::PyValueError::new_err(
                            "Probabilità del modello non valide",
                        ));
                    }
                    let mut selected = labels
                        .into_iter()
                        .zip(probabilities)
                        .filter_map(|(label, p)| (p > 0.5).then_some(label))
                        .collect::<Vec<_>>();
                    selected.sort();
                    Ok(selected)
                })
                .collect()
        })
    }

    type ReferenceEdge = (String, i64, String, i64, String);
    type CandidateEdge = (i64, String, u64, String, i64, String, i64, String);
    type Candidate = (i64, String, u64, Vec<ReferenceEdge>);
    #[pyfunction]
    fn rank_candidates(py: Python<'_>, edges: Vec<CandidateEdge>) -> PyResult<Vec<Candidate>> {
        py.detach(move || collect_candidates(edges))
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }

    fn collect_candidates(edges: Vec<CandidateEdge>) -> Result<Vec<Candidate>, String> {
        let mut candidates = HashMap::<i64, Candidate>::new();
        for (id, title, outgoing, language, source, source_title, target, target_title) in edges {
            if id <= 0
                || source <= 0
                || target <= 0
                || title.is_empty()
                || source_title.is_empty()
                || target_title.is_empty()
                || language.is_empty()
            {
                return Err("invalid candidate evidence".into());
            }
            if candidates
                .get(&id)
                .is_some_and(|c| c.1 != title || c.2 != outgoing)
            {
                return Err("inconsistent local candidate".into());
            }
            let candidate = candidates
                .entry(id)
                .or_insert_with(|| (id, title, outgoing, Vec::new()));
            candidate
                .3
                .push((language, source, source_title, target, target_title));
        }
        let mut candidates = candidates.into_values().collect::<Vec<_>>();
        for candidate in &mut candidates {
            candidate.3.sort();
            candidate.3.dedup();
        }
        candidates.sort_by(|a, b| {
            (a.2 != 0)
                .cmp(&(b.2 != 0))
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(candidates)
    }

    #[pyfunction]
    fn collection_interval(
        py: Python<'_>,
        intervals: Vec<(String, String)>,
    ) -> PyResult<(String, String)> {
        py.detach(move || interval_bounds(&intervals))
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }
    fn interval_bounds(intervals: &[(String, String)]) -> Result<(String, String), String> {
        let mut earliest = None;
        let mut latest = None;
        for (start, finish) in intervals {
            let start_date = timestamp(start)?;
            let finish_date = timestamp(finish)?;
            if finish_date < start_date {
                return Err("collection ends before it starts".into());
            }
            if earliest.as_ref().is_none_or(|(date, _)| start_date < *date) {
                earliest = Some((start_date, start));
            }
            if latest.as_ref().is_none_or(|(date, _)| finish_date > *date) {
                latest = Some((finish_date, finish));
            }
        }
        earliest
            .zip(latest)
            .map(|((_, start), (_, finish))| (start.clone(), finish.clone()))
            .ok_or_else(|| "empty collection interval".into())
    }

    #[pymodule]
    fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(wrap_pyfunction!(graph_counts, m)?)?;
        m.add_function(wrap_pyfunction!(graph_details, m)?)?;
        m.add_function(wrap_pyfunction!(classify_categories, m)?)?;
        m.add_function(wrap_pyfunction!(article_ages, m)?)?;
        m.add_function(wrap_pyfunction!(creator_matches, m)?)?;
        m.add_function(wrap_pyfunction!(creator_counts, m)?)?;
        m.add_function(wrap_pyfunction!(unique_items, m)?)?;
        m.add_function(wrap_pyfunction!(matchable_edges, m)?)?;
        m.add_function(wrap_pyfunction!(topic_inputs, m)?)?;
        m.add_function(wrap_pyfunction!(threshold_topics, m)?)?;
        m.add_function(wrap_pyfunction!(rank_candidates, m)?)?;
        m.add_function(wrap_pyfunction!(collection_interval, m)?)?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn collection_bounds_use_instants_and_reject_invalid_intervals() {
            let start = "2026-01-01T01:00:00+02:00";
            let finish = "2026-01-02T00:00:00Z";
            assert_eq!(
                interval_bounds(&[
                    ("2026-01-01T00:00:00Z".into(), finish.into()),
                    (start.into(), "2026-01-01T00:00:00Z".into()),
                ])
                .unwrap(),
                (start.into(), finish.into())
            );
            assert!(interval_bounds(&[]).is_err());
            assert!(interval_bounds(&[(finish.into(), start.into())]).is_err());
            assert!(interval_bounds(&[("invalid".into(), finish.into())]).is_err());
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
            let choices = compute_categories(&pages, &parents, rules).unwrap();
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
            let choices = compute_categories(&pages, &parents, manual).unwrap();
            assert_eq!(choices[0].0.as_deref(), Some("Root"));
            assert!(choices[0].2);
        }

        #[test]
        fn categories_validate_rules_and_ties() {
            let pages = vec![(1, vec!["B".into(), "A".into(), "A".into()]), (2, vec![])];
            let choices = compute_categories(
                &pages,
                &HashMap::new(),
                (vec![], HashSet::new(), HashMap::new(), HashMap::new(), 1.0),
            )
            .unwrap();
            assert_eq!(choices[0].0.as_deref(), Some("A"));
            assert_eq!(choices[0].1[0].2, 1);
            assert_eq!(choices[1].0, None);
            for minimum in [0.0, -1.0, f64::NAN, f64::INFINITY] {
                assert!(compute_categories(
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
            assert!(compute_categories(
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
            assert!(compute_categories(
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
            let ages = compute_ages(
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
            assert!(compute_ages("bad", &[]).is_err());
            assert!(compute_ages(
                "2026-01-02T00:00:00Z",
                &[Some("2026-02-30T00:00:00Z".into())]
            )
            .is_err());
        }

        #[test]
        fn creator_matching_requires_initial_revision_and_confirmed_creation() {
            let first = Some("2026-01-01T00:00:00Z".into());
            let equivalent = Some("2026-01-01T01:00:00+01:00".into());
            let matches = match_creators(&[
                (first.clone(), equivalent.clone(), false, None),
                (first.clone(), equivalent, true, None),
                (None, first.clone(), false, None),
                (first.clone(), first.clone(), true, first),
            ])
            .unwrap();
            assert_eq!(
                matches,
                [(true, false), (true, true), (false, false), (true, false)]
            );
        }

        #[test]
        fn creator_counts_keep_unknown_and_local_identities_separate() {
            let counts = count_creators(&[
                (Some("vec:1".into()), true),
                (Some("vec:1".into()), false),
                (Some("lmo:1".into()), false),
                (None, true),
                (Some(String::new()), false),
            ]);
            assert_eq!(
                counts,
                [
                    (Some(2), Some(1), Some(0.5)),
                    (Some(2), Some(1), Some(0.5)),
                    (Some(1), Some(0), Some(0.0)),
                    (None, None, None),
                    (None, None, None)
                ]
            );
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
            let (unique, ambiguous) = item_index(&items);
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
            let result = collect_candidates(vec![
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
            assert!(collect_candidates(vec![(
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
    }
}
