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

    pub(super) fn cache_ancestors(&mut self, category: &str) {
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
