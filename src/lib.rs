//! Complete census analysis shared by the command line and WebAssembly dashboard.
#![forbid(unsafe_code)]

pub mod dashboard;
pub mod dumps;
#[cfg(feature = "cli")]
pub mod editorial;
#[cfg(feature = "cli")]
pub mod pipeline;
#[cfg(feature = "cli")]
pub mod runtime;

use std::collections::{HashMap, HashSet};

pub type Counts = (Vec<u64>, Vec<u64>, usize);
pub type GraphDetails = (Vec<u64>, Vec<u64>, usize, Vec<Vec<usize>>);
pub type RankedCategory = (String, f64, u64, f64);
pub type CategoryChoice = (Option<String>, Vec<RankedCategory>, bool);
pub type CategoryRules = (
    Vec<String>,
    HashSet<String>,
    HashMap<String, f64>,
    HashMap<String, String>,
    f64,
);
pub type ReferenceEdge = (String, i64, String, i64, String);
pub type CandidateEdge = (i64, String, u64, String, i64, String, i64, String);
pub type Candidate = (i64, String, u64, Vec<ReferenceEdge>);

/// Resolve only syntactically valid Wikipedia edition and database identities.
pub fn wiki_database(language: &str, database: Option<&str>) -> Result<String, String> {
    let lower = |byte: &u8| byte.is_ascii_lowercase();
    let valid_language = !language.is_empty()
        && language.len() <= 20
        && language.as_bytes().first().is_some_and(lower)
        && language
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && language != "all";
    if !valid_language {
        return Err("Codice lingua non valido".into());
    }
    let database = database
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}wiki", language.replace('-', "_")));
    if database.len() < 5
        || database.len() > 45
        || !database.ends_with("wiki")
        || !database.as_bytes().first().is_some_and(lower)
        || !database
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("Nome database Wikipedia non valido (senza suffisso _p)".into());
    }
    Ok(database)
}

/// Validate the entire sole edition pool, including editions outside a subset.
pub fn configured_languages(config: &serde_json::Value) -> Result<Vec<(String, String)>, String> {
    use serde_json::Value;
    let invalid = || String::from("Configurazione lingue non valida");
    let config = config.as_object().ok_or_else(invalid)?;
    if config
        .keys()
        .any(|key| !["version", "languages", "databases"].contains(&key.as_str()))
        || config.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(invalid());
    }
    let languages = config
        .get("languages")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(invalid)?;
    let mut seen = HashSet::new();
    for code in languages {
        if !seen.insert(code.as_str().ok_or_else(invalid)?) {
            return Err(invalid());
        }
    }
    let empty = serde_json::Map::new();
    let databases = match config.get("databases") {
        Some(value) => value.as_object().ok_or_else(invalid)?,
        None => &empty,
    };
    if databases.keys().any(|code| !seen.contains(code.as_str())) {
        return Err(invalid());
    }
    languages
        .iter()
        .map(|value| {
            let code = value.as_str().ok_or_else(invalid)?;
            let database = match databases.get(code) {
                Some(Value::Null) | None => None,
                Some(Value::String(value)) => Some(value.as_str()),
                Some(_) => {
                    return Err("Nome database Wikipedia non valido (senza suffisso _p)".into())
                }
            };
            Ok((code.to_owned(), wiki_database(code, database)?))
        })
        .collect()
}

/// Select a distinct subset while retaining the edition pool's order.
pub fn select_languages(
    editions: &[(String, String)],
    selected: Option<&[String]>,
) -> Result<Vec<(String, String)>, String> {
    let Some(selected) = selected else {
        return Ok(editions.to_vec());
    };
    let requested = selected.iter().collect::<HashSet<_>>();
    if selected.is_empty()
        || requested.len() != selected.len()
        || selected
            .iter()
            .any(|code| !editions.iter().any(|(language, _)| language == code))
    {
        return Err(
            "Le lingue richieste devono essere distinte e presenti nella configurazione".into(),
        );
    }
    Ok(editions
        .iter()
        .filter(|(code, _)| requested.contains(code))
        .cloned()
        .collect())
}

// Expected O(V + E + R) hash-table work, plus string hashing/copying.
// Memoization resolves each redirect path once. Reverse adjacency is optional.
fn count_graph(
    titles: Vec<String>,
    links: Vec<Vec<String>>,
    redirects: HashMap<String, String>,
) -> Counts {
    let (incoming, outgoing, unresolved, _) = compute_graph(titles, links, redirects, false);
    (incoming, outgoing, unresolved)
}

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

fn validate_graph(titles: &[String], links: &[Vec<String>]) -> Result<(), String> {
    if titles.len() != links.len() {
        return Err(String::from("titles/links size mismatch"));
    }
    if titles.iter().collect::<HashSet<_>>().len() != titles.len() {
        return Err(String::from("duplicate titles"));
    }
    Ok(())
}

/// Count distinct resolved sources and targets, retaining isolated articles.
pub fn graph_counts(
    titles: Vec<String>,
    links: Vec<Vec<String>>,
    redirects: HashMap<String, String>,
) -> Result<Counts, String> {
    validate_graph(&titles, &links)?;
    Ok(count_graph(titles, links, redirects))
}

/// Include complete reverse adjacency in source order.
pub fn graph_details(
    titles: Vec<String>,
    links: Vec<Vec<String>>,
    redirects: HashMap<String, String>,
) -> Result<GraphDetails, String> {
    validate_graph(&titles, &links)?;
    Ok(compute_graph(titles, links, redirects, true))
}

struct IncrementalGraph {
    index: HashMap<String, usize>,
    redirects: HashMap<String, String>,
    resolved: HashMap<String, Option<usize>>,
    incoming: Vec<u64>,
    outgoing: Vec<u64>,
    unresolved: usize,
    next_source: usize,
}

impl IncrementalGraph {
    fn resolve(&mut self, original: &str) -> Option<usize> {
        // Article titles already have an index. Arbitrary dangling targets
        // must not accumulate across batches: only redirect chains need cache.
        if !self.redirects.contains_key(original) {
            return self.index.get(original).copied();
        }
        if let Some(found) = self.resolved.get(original) {
            return *found;
        }
        let mut current = original;
        let mut visited = HashSet::new();
        let mut path = Vec::new();
        let found = loop {
            if let Some(value) = self.resolved.get(current) {
                break *value;
            }
            if !visited.insert(current) {
                break None;
            }
            path.push(current);
            if let Some(next) = self.redirects.get(current) {
                current = next;
            } else {
                break self.index.get(current).copied();
            }
        };
        for item in path {
            if self.redirects.contains_key(item) {
                self.resolved.insert(item.to_owned(), found);
            }
        }
        found
    }

    fn add(
        &mut self,
        batch: Vec<Vec<String>>,
        eligible: Option<&HashSet<usize>>,
    ) -> Result<Vec<(usize, usize)>, String> {
        if self.next_source + batch.len() > self.incoming.len() {
            return Err(String::from("Troppi blocchi di collegamenti"));
        }
        let mut edges = Vec::new();
        for targets in batch {
            #[cfg(feature = "cli")]
            crate::runtime::check_cancelled()?;
            let source = self.next_source;
            self.next_source += 1;
            let mut distinct = HashSet::with_capacity(targets.len());
            for original in targets {
                let destination = self.resolve(&original);
                match destination {
                    Some(target) if target != source => {
                        distinct.insert(target);
                    }
                    Some(_) => {}
                    None => self.unresolved += 1,
                }
            }
            self.outgoing[source] = distinct.len() as u64;
            for target in distinct {
                self.incoming[target] += 1;
                if eligible.is_some_and(|items| items.contains(&source) && items.contains(&target))
                {
                    edges.push((target, source));
                }
            }
        }
        Ok(edges)
    }
}

/// Bounded graph analysis; no Python runtime or synchronization required.
pub struct GraphAccumulator {
    inner: IncrementalGraph,
    eligible: Option<HashSet<usize>>,
}

impl GraphAccumulator {
    pub fn new(
        titles: Vec<String>,
        redirects: HashMap<String, String>,
        eligible_indices: Option<Vec<usize>>,
    ) -> Result<Self, String> {
        let count = titles.len();
        if eligible_indices
            .as_ref()
            .is_some_and(|positions| positions.iter().any(|&i| i >= count))
        {
            return Err(String::from("invalid eligible index"));
        }
        let index: HashMap<String, usize> = titles
            .into_iter()
            .enumerate()
            .map(|(i, t)| (t, i))
            .collect();
        if index.len() != count {
            return Err(String::from("duplicate titles"));
        }
        Ok(Self {
            eligible: eligible_indices.map(|positions| positions.into_iter().collect()),
            inner: IncrementalGraph {
                index,
                redirects,
                resolved: HashMap::new(),
                incoming: vec![0; count],
                outgoing: vec![0; count],
                unresolved: 0,
                next_source: 0,
            },
        })
    }

    pub fn add_batch(&mut self, batch: Vec<Vec<String>>) -> Result<(), String> {
        self.inner.add(batch, None).map(|_| ())
    }

    pub fn add_batch_edges(
        &mut self,
        batch: Vec<Vec<String>>,
    ) -> Result<Vec<(usize, usize)>, String> {
        self.inner.add(batch, self.eligible.as_ref())
    }

    pub fn finish(&self) -> Result<Counts, String> {
        if self.inner.next_source != self.inner.incoming.len() {
            return Err(String::from("Blocchi di collegamenti incompleti"));
        }
        Ok((
            self.inner.incoming.clone(),
            self.inner.outgoing.clone(),
            self.inner.unresolved,
        ))
    }
}

/// Resolve unique local QIDs with memory bounded by titles and one link batch.
pub struct TopicInputAccumulator {
    graph: IncrementalGraph,
    items: Vec<Option<String>>,
    known: HashSet<String>,
}

impl TopicInputAccumulator {
    pub fn new(
        titles: Vec<String>,
        items: Vec<Option<String>>,
        redirects: HashMap<String, String>,
        known: HashSet<String>,
    ) -> Result<Self, String> {
        if titles.len() != items.len() {
            return Err(String::from("titles/items size mismatch"));
        }
        let count = titles.len();
        let index: HashMap<String, usize> = titles
            .into_iter()
            .enumerate()
            .map(|(i, t)| (t, i))
            .collect();
        if index.len() != count {
            return Err(String::from("duplicate titles"));
        }
        let (unique, _) = unique_items(&items);
        let mut retained = vec![None; count];
        for (item, position) in unique {
            retained[position] = Some(item);
        }
        Ok(Self {
            graph: IncrementalGraph {
                index,
                redirects,
                resolved: HashMap::new(),
                incoming: Vec::new(),
                outgoing: Vec::new(),
                unresolved: 0,
                next_source: 0,
            },
            items: retained,
            known,
        })
    }

    pub fn add_batch(
        &mut self,
        batch: Vec<Vec<String>>,
    ) -> Result<Vec<(String, usize, bool)>, String> {
        let graph = &mut self.graph;
        if graph.next_source + batch.len() > self.items.len() {
            return Err(String::from("Troppi blocchi di collegamenti"));
        }
        let mut output = Vec::with_capacity(batch.len());
        for targets in batch {
            #[cfg(feature = "cli")]
            crate::runtime::check_cancelled()?;
            let source = graph.next_source;
            graph.next_source += 1;
            let mut distinct = HashSet::with_capacity(targets.len());
            for target in targets {
                if let Some(destination) = graph.resolve(&target) {
                    if destination != source {
                        distinct.insert(destination);
                    }
                }
            }
            let mut tokens = Vec::new();
            let mut mapped = false;
            for destination in distinct {
                if let Some(item) = &self.items[destination] {
                    mapped = true;
                    if self.known.contains(item) {
                        tokens.push(item.clone());
                    }
                }
            }
            tokens.sort();
            tokens.dedup();
            output.push((tokens.join(" "), tokens.len(), mapped));
        }
        Ok(output)
    }

    pub fn finish(&self) -> Result<(), String> {
        if self.graph.next_source != self.items.len() {
            return Err(String::from("Blocchi di collegamenti incompleti"));
        }
        Ok(())
    }
}

pub fn classify_categories(
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
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
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

/// Preserve census-wide support counts while classifying bounded page batches.
pub struct CategoryBatchClassifier {
    total: usize,
    support_sources: usize,
    frequencies: HashMap<String, u64>,
    parents: HashMap<String, Vec<String>>,
    ancestors: HashMap<String, HashSet<String>>,
    patterns: Vec<regex::Regex>,
    excluded: HashSet<String>,
    priority: HashMap<String, f64>,
    overrides: HashMap<String, String>,
    min_support: f64,
}

impl CategoryBatchClassifier {
    pub fn new(
        total: usize,
        parents: HashMap<String, Vec<String>>,
        rules: CategoryRules,
    ) -> Result<Self, String> {
        let (patterns, excluded, priority, overrides, min_support) = rules;
        if !min_support.is_finite()
            || min_support <= 0.0
            || priority.values().any(|v| !v.is_finite())
        {
            return Err(String::from("Regole delle categorie non valide"));
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
        Ok(Self {
            total,
            support_sources: 0,
            frequencies: HashMap::new(),
            parents,
            ancestors: HashMap::new(),
            patterns,
            excluded,
            priority,
            overrides,
            min_support,
        })
    }

    pub fn add_support_batch(&mut self, batch: Vec<Vec<String>>) -> Result<(), String> {
        if self.support_sources + batch.len() > self.total {
            return Err(String::from("Troppi blocchi di categorie"));
        }
        for categories in batch {
            self.support_sources += 1;
            for category in categories.iter().collect::<HashSet<_>>() {
                *self.frequencies.entry(category.to_owned()).or_default() += 1;
            }
        }
        Ok(())
    }

    fn cache_ancestors(&mut self, category: &str) {
        if self.ancestors.contains_key(category) {
            return;
        }
        let mut seen = HashSet::new();
        let mut pending = self
            .parents
            .get(category)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        while let Some(item) = pending.pop() {
            if seen.insert(item.as_str()) {
                pending.extend(self.parents.get(item).into_iter().flatten());
            }
        }
        self.ancestors.insert(
            category.to_owned(),
            seen.into_iter().map(str::to_owned).collect(),
        );
    }

    pub fn classify_batch(
        &mut self,
        pages: Vec<(i64, Vec<String>)>,
    ) -> Result<Vec<CategoryChoice>, String> {
        if self.support_sources != self.total {
            return Err(String::from("Frequenze delle categorie incomplete"));
        }
        let mut choices = Vec::with_capacity(pages.len());
        for (id, categories) in pages {
            #[cfg(feature = "cli")]
            crate::runtime::check_cancelled()?;
            let candidates = categories
                .iter()
                .map(String::as_str)
                .filter(|category| {
                    !self.excluded.contains(*category)
                        && !self
                            .patterns
                            .iter()
                            .any(|pattern| pattern.is_match(category))
                })
                .collect::<HashSet<_>>();
            for category in &candidates {
                self.cache_ancestors(category);
            }
            let mut ranked = Vec::new();
            for category in &candidates {
                let broader = candidates.iter().any(|other| {
                    category != other
                        && self
                            .ancestors
                            .get(*other)
                            .is_some_and(|ancestors| ancestors.contains(*category))
                        && !self
                            .ancestors
                            .get(*category)
                            .is_some_and(|ancestors| ancestors.contains(*other))
                });
                if !broader {
                    let support = self.frequencies.get(*category).copied().unwrap_or_default();
                    let score = ((self.total as f64 + 1.0) / (support as f64 + 1.0)).ln()
                        * (support as f64 / self.min_support).min(1.0);
                    ranked.push((
                        (*category).to_owned(),
                        (score * 1e6).round_ties_even() / 1e6,
                        support,
                        self.priority.get(*category).copied().unwrap_or_default(),
                    ));
                }
            }
            ranked.sort_by(|a, b| {
                b.3.total_cmp(&a.3)
                    .then_with(|| b.1.total_cmp(&a.1))
                    .then_with(|| a.0.cmp(&b.0))
            });
            let manual = self.overrides.get(&id.to_string());
            if manual.is_some_and(|choice| !categories.contains(choice)) {
                return Err(format!(
                    "Override {id}: categoria non appartenente alla voce"
                ));
            }
            let choice = manual
                .cloned()
                .or_else(|| ranked.first().map(|row| row.0.clone()));
            choices.push((choice, ranked, manual.is_some()));
        }
        Ok(choices)
    }
}

fn timestamp(value: &str) -> Result<chrono::DateTime<chrono::FixedOffset>, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|error| format!("Invalid timestamp {value:?}: {error}"))
}

/// Preserve missing dates and report future dates separately from known ages.
pub fn article_ages(
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

fn valid_item(item: &str) -> bool {
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

/// Compare instants while preserving the original boundary timestamp strings.
pub fn collection_interval(intervals: &[(String, String)]) -> Result<(String, String), String> {
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

/// Build the existing scientific observation from native graph/category results.
/// No measure is inferred from length, current bot flags or missing evidence.
pub fn observation_row(
    page: &serde_json::Value,
    choice: &CategoryChoice,
    incoming: u64,
    outgoing: u64,
    creation_events: bool,
) -> Result<serde_json::Value, String> {
    use serde_json::{json, Value};
    if page["page_id"].as_i64().is_none()
        || !page["title"].is_string()
        || choice
            .1
            .iter()
            .any(|(_, score, _, priority)| !score.is_finite() || !priority.is_finite())
    {
        return Err("Identità della pagina o ranking delle categorie non validi".into());
    }
    let created = &page[if creation_events {
        "page_created_at"
    } else {
        "created_at"
    }];
    let candidates = choice.1.iter().map(|(category,score,support,priority)|json!({"category":category,"score":score,"support":support,"priority":priority})).collect::<Vec<_>>();
    Ok(
        json!({"page_id":page["page_id"],"title":page["title"],"revision_id":page["revision_id"],
        "wikibase_item":page["wikibase_item"],"category":choice.0,"category_candidates":candidates,
        "category_status":if choice.2 {"reviewed"} else if choice.0.is_some() {"heuristic_review_needed"} else {"no_eligible_category"},
        "category_reason":if choice.2 {"Scelta manuale verificata"} else {"Categorie visibili; esclusioni configurate; rimozione dei sovratemi; specificità e numerosità. Non è una classificazione semantica validata."},
        "categories":page.get("categories").cloned().unwrap_or_else(||json!([])),"created_at":created,
        "length_bytes":page["length_bytes"],"first_public_revision_at":page.get("first_public_revision_at").unwrap_or(&page["created_at"]),
        "page_created_at":page["page_created_at"],"creation_date_source":page["creation_date_source"],
        "creation_date_status":page.get("creation_date_status").cloned().unwrap_or(Value::String("not_collected".into())),
        "in_degree":incoming,"out_degree":outgoing,"orphan":incoming==0}),
    )
}

/// Initialize only unsupported creator measures. Historical attribution remains
/// separate from observed creation and current census aggregates.
pub fn initialize_creator_rows(
    rows: &mut [serde_json::Value],
) -> Result<serde_json::Value, String> {
    use serde_json::{json, Value};
    for row in rows.iter_mut() {
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        for field in [
            "creator_key",
            "creator_attribution",
            "creator_account_type",
            "creator_is_bot_now",
            "creator_account_type_at_creation",
            "creator_registered_at",
            "creator_tenure_days",
            "creator_tenure_status",
            "creator_prior_edits_main",
            "creator_prior_edits_other",
            "creator_prior_articles",
            "creator_articles_created_total",
            "creator_languages_created",
            "creator_languages_before",
            "origin",
            "creation_tags",
            "import_log_observed",
            "move_log_observed",
            "creator_experience_status",
            "creator_orphans_at_30_days",
            "creator_articles_in_census",
            "creator_orphans_current",
            "creator_orphans_current_share",
        ] {
            row.insert(field.into(), Value::Null);
        }
        row.insert("origin".into(), json!("unknown"));
        row.insert("creator_data_status".into(), json!("not_collected"));
        row.insert(
            "creator_orphans_at_30_days_status".into(),
            json!("unavailable_no_historical_link_graph"),
        );
    }
    Ok(
        json!({"status":"not_collected","languages_scanned":[],"matched_pages":0,"unmatched_pages":rows.len(),"orphan_30d_status":"unavailable_no_historical_link_graph"}),
    )
}

/// Join a verified History mapping to one observation batch. The caller checks
/// supplement source/release and census identities before supplying records.
pub fn join_history_rows(
    rows: &mut [serde_json::Value],
    records: &HashMap<i64, serde_json::Value>,
    missing_status: &str,
) -> Result<usize, String> {
    use serde_json::{json, Value};
    if records
        .iter()
        .any(|(id, record)| record["page_id"].as_i64() != Some(*id))
    {
        return Err("Identità della cronologia non corrispondente".into());
    }
    let mut matched = 0;
    for row in rows {
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
        let id = row["page_id"]
            .as_i64()
            .ok_or("ID dell'osservazione non valido")?;
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        for field in [
            "history_first_revision_at",
            "history_creator_key",
            "history_creator_status",
            "creator_groups_at_creation_history",
            "creator_continuity_months_before_creation",
            "creator_continuity_bucket_before_creation",
            "creator_prior_edits_main_history",
            "creator_prior_edits_other_history",
        ] {
            row.insert(field.into(), Value::Null);
        }
        row.insert("history_data_status".into(), json!(missing_status));
        let Some(record) = records.get(&id) else {
            continue;
        };
        if let Some(created) = record.get("page_created_at") {
            row.insert("page_created_at".into(), created.clone());
            row.insert("created_at".into(), created.clone());
            row.insert(
                "creation_date_source".into(),
                json!("MediaWiki History / page_creation_timestamp"),
            );
            row.insert(
                "creation_date_status".into(),
                record
                    .get("creation_date_status")
                    .cloned()
                    .unwrap_or(json!("missing_history_creation")),
            );
        }
        let historical_date = &record["article_created_at"];
        if historical_date.is_null() || historical_date == "" {
            row.insert(
                "history_data_status".into(),
                json!("history_initial_revision_unknown"),
            );
            continue;
        }
        if !historical_date.is_string() {
            return Err("Data della prima revisione non valida".into());
        }
        row.insert("history_first_revision_at".into(), historical_date.clone());
        row.insert(
            "history_creator_status".into(),
            record["creator_attribution"].clone(),
        );
        if record["creator_history_status"] != "observed_surviving_history" {
            row.insert("history_data_status".into(), json!("unattributed_history"));
            continue;
        }
        for (target, source) in [
            ("history_creator_key", "creator_key"),
            ("creator_registered_at", "creator_registered_at"),
            ("creator_tenure_days", "creator_tenure_days"),
            ("creator_prior_articles", "creator_prior_articles"),
            ("creator_account_type", "creator_account_type"),
            ("creator_is_bot_now", "creator_is_bot_now"),
            (
                "creator_groups_at_creation_history",
                "creator_groups_at_creation",
            ),
            (
                "creator_continuity_months_before_creation",
                "creator_continuity_months_before_creation",
            ),
            (
                "creator_continuity_bucket_before_creation",
                "creator_continuity_bucket_before_creation",
            ),
            (
                "creator_prior_edits_main_history",
                "creator_prior_edits_main_history",
            ),
            (
                "creator_prior_edits_other_history",
                "creator_prior_edits_other_history",
            ),
        ] {
            row.insert(target.into(), record[source].clone());
        }
        row.insert(
            "creator_tenure_status".into(),
            json!(if record["creator_tenure_days"].is_null() {
                "missing_or_inconsistent_registration"
            } else {
                "local_registration_to_reconstructed_creation"
            }),
        );
        row.insert(
            "history_data_status".into(),
            json!("matched_reconstructed_history"),
        );
        matched += 1;
    }
    Ok(matched)
}

/// Join verified structural-quality records without substituting missing text
/// or rejecting the selected official XML revision when it differs from SQL.
pub fn join_quality_rows(
    rows: &mut [serde_json::Value],
    records: &HashMap<i64, serde_json::Value>,
) -> Result<(), String> {
    for row in rows {
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
        let id = row["page_id"]
            .as_i64()
            .ok_or("ID dell'osservazione non valido")?;
        let record = records.get(&id).ok_or("Qualità mancante per la voce")?;
        if record["page_id"].as_i64() != Some(id) || record["revision_id"] != row["revision_id"] {
            return Err("Qualità mancante o revisione diversa".into());
        }
        let status = record["quality_status"]
            .as_str()
            .ok_or("Stato della qualità mancante")?;
        let relation = record["quality_revision_relation"]
            .as_str()
            .ok_or("Relazione delle revisioni mancante")?;
        let content = &record["content_revision_id"];
        let score = &record["quality_score"];
        if !matches!(
            status,
            "scored" | "missing_current_page" | "missing_wikitext"
        ) || !matches!(relation, "same" | "different" | "unavailable")
            || (relation == "unavailable") != content.is_null()
            || (!content.is_null() && content.as_u64().is_none_or(|id| id == 0))
            || (relation == "same" && content != &row["revision_id"])
            || (relation == "different" && content == &row["revision_id"])
            || (status == "missing_current_page") != (relation == "unavailable")
            || (status == "scored"
                && !score
                    .as_f64()
                    .is_some_and(|s| (0.0..=0.998001).contains(&s)))
            || (status != "scored" && !score.is_null())
        {
            return Err("Punteggio qualità non valido".into());
        }
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        for field in [
            "quality_score",
            "quality_status",
            "content_revision_id",
            "quality_revision_relation",
        ] {
            row.insert(field.into(), record[field].clone());
        }
    }
    Ok(())
}

/// Join complete topic records after the caller verifies the graph fingerprint
/// and approved model. Empty predictions are known negatives, never missing.
pub fn join_topic_rows(
    rows: &mut [serde_json::Value],
    records: &HashMap<i64, serde_json::Value>,
    taxonomy: &HashSet<String>,
) -> Result<usize, String> {
    if taxonomy.len() != 64 || taxonomy.iter().any(|label| !label.contains('.')) {
        return Err("Tassonomia dei temi non valida".into());
    }
    let mut predicted = 0;
    for row in rows {
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
        let id = row["page_id"]
            .as_i64()
            .ok_or("ID dell'osservazione non valido")?;
        let record = records.get(&id).ok_or("Temi mancanti per la voce")?;
        if record["page_id"].as_i64() != Some(id) || record["revision_id"] != row["revision_id"] {
            return Err("Temi o revisione non corrispondenti".into());
        }
        predicted += usize::from(validate_topic_record(record, taxonomy)?);
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        for field in ["topic_labels", "topic_status", "topic_input_count"] {
            row.insert(field.into(), record[field].clone());
        }
    }
    Ok(predicted)
}

pub(crate) fn validate_topic_record(
    record: &serde_json::Value,
    taxonomy: &HashSet<String>,
) -> Result<bool, String> {
    let status = record["topic_status"]
        .as_str()
        .ok_or("Stato dei temi mancante")?;
    let count = record["topic_input_count"]
        .as_u64()
        .ok_or("Numero di collegamenti del modello incoerente")?;
    let labels = &record["topic_labels"];
    if !matches!(
        status,
        "predicted" | "no_model_vocabulary" | "no_mapped_outlinks"
    ) || (status == "predicted") != (count > 0)
        || (status == "predicted") == labels.is_null()
    {
        return Err("Stato di disponibilità dei temi incoerente".into());
    }
    if !labels.is_null() {
        let labels = labels.as_array().ok_or("Etichette dei temi non valide")?;
        let mut distinct = HashSet::with_capacity(labels.len());
        for label in labels {
            let label = label.as_str().ok_or("Etichetta dei temi non valida")?;
            if !taxonomy.contains(label) || !distinct.insert(label) {
                return Err("Temi sconosciuti o duplicati".into());
            }
        }
    }
    Ok(status == "predicted")
}

/// Compute both ages independently. A reconstructed or observed creation event
/// never falls back to the historical first revision when its date is missing.
pub fn observation_ages(
    rows: &mut [serde_json::Value],
    reference: &str,
    use_page_creation: bool,
) -> Result<(), String> {
    let collect_dates = |field: &str| {
        rows.iter()
            .map(|row| match &row[field] {
                serde_json::Value::Null => Ok(None),
                serde_json::Value::String(date) => Ok(Some(date.clone())),
                _ => Err("Data dell'osservazione non valida".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let dates = collect_dates(if use_page_creation {
        "page_created_at"
    } else {
        "created_at"
    })?;
    let history_dates = collect_dates("history_first_revision_at")?;
    let ages = article_ages(reference, &dates)?;
    let historical = article_ages(reference, &history_dates)?;
    for (((row, date), (age, after)), (historical_age, _)) in
        rows.iter_mut().zip(dates).zip(ages).zip(historical)
    {
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        row.insert("created_at".into(), serde_json::json!(date));
        row.insert("age_days".into(), serde_json::json!(age));
        row.insert(
            "history_first_revision_age_days".into(),
            serde_json::json!(historical_age),
        );
        if after {
            row.insert(
                "creation_date_status".into(),
                serde_json::json!("after_graph_collection"),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
