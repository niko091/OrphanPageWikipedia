use super::*;

pub(super) fn valid_item(item: &str) -> bool {
    let bytes = item.as_bytes();
    bytes.len() > 1
        && bytes[0] == b'Q'
        && matches!(bytes[1], b'1'..=b'9')
        && bytes[2..].iter().all(u8::is_ascii_digit)
}

/// Index only valid, unambiguous local Wikidata identifiers.
pub fn unique_items(items: &[Option<String>]) -> (HashMap<String, usize>, HashSet<String>) {
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

/// Retain every edge whose source and target have unique local identifiers.
pub fn matchable_edges(
    size: usize,
    unique: HashMap<String, usize>,
    incoming: Vec<Vec<usize>>,
) -> Result<Vec<(usize, usize)>, String> {
    if incoming.len() != size
        || unique.values().any(|&i| i >= size)
        || incoming.iter().flatten().any(|&i| i >= size)
    {
        return Err(String::from("invalid adjacency indices"));
    }
    Ok({
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
    })
}

/// Build sorted, distinct model tokens from resolved incoming adjacency.
pub fn topic_inputs(
    size: usize,
    unique: HashMap<String, usize>,
    known: HashSet<String>,
    incoming: Vec<Vec<usize>>,
) -> Result<Vec<(String, usize, bool)>, String> {
    if incoming.len() != size
        || unique.values().any(|&i| i >= size)
        || incoming.iter().flatten().any(|&i| i >= size)
    {
        return Err(String::from("invalid adjacency indices"));
    }
    Ok({
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
    })
}

/// Validate all 64 labels and apply the approved strict probability threshold.
pub fn threshold_topics(
    taxonomy: HashSet<String>,
    labels: Vec<Vec<String>>,
    probabilities: Vec<Vec<f64>>,
) -> Result<Vec<Vec<String>>, String> {
    if taxonomy.len() != 64 || labels.len() != probabilities.len() {
        return Err(String::from("Risposta del modello incompleta"));
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
                return Err(String::from("Predizioni senza tutti i 64 temi"));
            }
            if probabilities
                .iter()
                .any(|p| !p.is_finite() || !(0.0..=1.0001).contains(p))
            {
                return Err(String::from("Probabilità del modello non valide"));
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
}

/// Union complete reference evidence, ranking local dead ends first.
pub fn rank_candidates(edges: Vec<CandidateEdge>) -> Result<Vec<Candidate>, String> {
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
