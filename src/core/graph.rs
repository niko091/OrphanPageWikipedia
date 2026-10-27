use super::*;

// Expected O(V + E + R) hash-table work, plus string hashing/copying.
// Memoization resolves each redirect path once. Reverse adjacency is optional.
pub(super) fn count_graph(
    titles: Vec<String>,
    links: Vec<Vec<String>>,
    redirects: HashMap<String, String>,
) -> Counts {
    let (incoming, outgoing, unresolved, _) = compute_graph(titles, links, redirects, false);
    (incoming, outgoing, unresolved)
}

pub(super) fn compute_graph(
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

pub(super) fn validate_graph(titles: &[String], links: &[Vec<String>]) -> Result<(), String> {
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

pub(super) struct IncrementalGraph {
    pub(super) index: HashMap<String, usize>,
    pub(super) redirects: HashMap<String, String>,
    pub(super) resolved: HashMap<String, Option<usize>>,
    pub(super) incoming: Vec<u64>,
    pub(super) outgoing: Vec<u64>,
    pub(super) unresolved: usize,
    pub(super) next_source: usize,
}

impl IncrementalGraph {
    pub(super) fn resolve(&mut self, original: &str) -> Option<usize> {
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

    pub(super) fn add(
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
    pub(super) inner: IncrementalGraph,
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
    pub(super) graph: IncrementalGraph,
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
