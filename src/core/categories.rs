use super::*;

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

pub(super) const ANCESTOR_CACHE_BYTES: usize = 16 * 1024 * 1024;
pub(super) const ANCESTOR_CACHE_ENTRIES: usize = 256;

struct CachedAncestors {
    values: HashSet<String>,
    bytes: usize,
    used: u64,
}

/// Preserve census-wide support counts while classifying bounded page batches.
pub struct CategoryBatchClassifier {
    total: usize,
    support_sources: usize,
    frequencies: HashMap<String, u64>,
    parents: HashMap<String, Vec<String>>,
    ancestors: HashMap<String, CachedAncestors>,
    ancestor_cache_bytes: usize,
    ancestor_cache_clock: u64,
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
            ancestor_cache_bytes: 0,
            ancestor_cache_clock: 0,
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

    fn candidate_ancestors<'a>(
        &mut self,
        category: &str,
        candidates: &HashSet<&'a str>,
    ) -> Result<HashSet<&'a str>, String> {
        self.ancestor_cache_clock = self.ancestor_cache_clock.saturating_add(1);
        if let Some(cached) = self.ancestors.get_mut(category) {
            cached.used = self.ancestor_cache_clock;
            return Ok(candidates
                .iter()
                .copied()
                .filter(|candidate| cached.values.contains(*candidate))
                .collect());
        }
        // Traverse the complete hierarchy using borrowed names. Only relations
        // between this page's candidates need to survive the traversal; keeping
        // owned transitive closures for every census category grows quadratically.
        let mut seen = HashSet::new();
        let mut pending = self
            .parents
            .get(category)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        while let Some(item) = pending.pop() {
            #[cfg(feature = "cli")]
            crate::runtime::check_cancelled()?;
            if seen.insert(item.as_str()) {
                pending.extend(self.parents.get(item).into_iter().flatten());
            }
        }
        let relevant = candidates
            .iter()
            .copied()
            .filter(|candidate| seen.contains(*candidate))
            .collect();
        let overhead = category.len() + std::mem::size_of::<(String, CachedAncestors)>();
        let estimated = overhead
            + seen.capacity() * std::mem::size_of::<String>()
            + seen.iter().map(|name| name.len()).sum::<usize>();
        // Oversized closures are still searched exactly, but never copied into
        // the cache. Eviction affects reuse only, not category ranking or cycles.
        if estimated <= ANCESTOR_CACHE_BYTES {
            let values = seen.into_iter().map(str::to_owned).collect::<HashSet<_>>();
            let bytes = overhead
                + values.capacity() * std::mem::size_of::<String>()
                + values.iter().map(String::capacity).sum::<usize>();
            if bytes <= ANCESTOR_CACHE_BYTES {
                while self.ancestors.len() >= ANCESTOR_CACHE_ENTRIES
                    || self.ancestor_cache_bytes + bytes > ANCESTOR_CACHE_BYTES
                {
                    let oldest = self
                        .ancestors
                        .iter()
                        .min_by_key(|(_, cached)| cached.used)
                        .map(|(name, _)| name.clone())
                        .ok_or("Cache degli antenati incoerente")?;
                    if let Some(removed) = self.ancestors.remove(&oldest) {
                        self.ancestor_cache_bytes -= removed.bytes;
                    }
                }
                self.ancestor_cache_bytes += bytes;
                self.ancestors.insert(
                    category.to_owned(),
                    CachedAncestors {
                        values,
                        bytes,
                        used: self.ancestor_cache_clock,
                    },
                );
            }
        }
        Ok(relevant)
    }

    #[cfg(test)]
    pub(super) fn ancestor_cache_usage(&self) -> (usize, usize) {
        (self.ancestors.len(), self.ancestor_cache_bytes)
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
            let mut ancestors = HashMap::new();
            if candidates.len() > 1 {
                for category in &candidates {
                    ancestors.insert(*category, self.candidate_ancestors(category, &candidates)?);
                }
            }
            let mut ranked = Vec::new();
            for category in &candidates {
                let broader = candidates.iter().any(|other| {
                    category != other
                        && ancestors
                            .get(*other)
                            .is_some_and(|ancestors| ancestors.contains(*category))
                        && !ancestors
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
