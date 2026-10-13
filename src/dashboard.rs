//! Descriptive dashboard statistics shared by Rust tests and browser WebAssembly.
//! Inputs contain observations only; no network, research-source or edit access.
//! Category/features/counts are O(V + labels), numeric sorting O(V log V).
//! Transient memory is O(V) for the selected observations, never graph edges.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Serialize)]
struct Group {
    count: usize,
    known: usize,
    total: usize,
}
#[derive(Serialize)]
struct FeatureRow {
    label: String,
    groups: [Group; 2],
    total: usize,
    orphans: usize,
    known: usize,
    percent: Option<f64>,
}
#[derive(Serialize)]
struct Summary {
    totals: [usize; 2],
    rows: Vec<FeatureRow>,
}
fn percentage(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| 100.0 * numerator as f64 / denominator as f64)
}
fn median(sorted: &[f64]) -> Option<f64> {
    let middle = sorted.len() / 2;
    if sorted.is_empty() {
        None
    } else if sorted.len() % 2 == 1 {
        Some(sorted[middle])
    } else {
        Some(sorted[middle - 1] / 2.0 + sorted[middle] / 2.0)
    }
}
fn measured(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v >= 0.0)
}
fn summary(
    labels: Vec<String>,
    totals: [usize; 2],
    observations: impl IntoIterator<Item = (usize, Vec<Option<bool>>)>,
) -> Summary {
    let mut rows: Vec<_> = labels
        .into_iter()
        .map(|label| FeatureRow {
            label,
            groups: totals.map(|total| Group {
                total,
                count: 0,
                known: 0,
            }),
            total: 0,
            orphans: 0,
            known: 0,
            percent: None,
        })
        .collect();
    for (group, matches) in observations {
        for (row, matches) in rows.iter_mut().zip(matches) {
            if let Some(matches) = matches {
                row.groups[group].known += 1;
                row.groups[group].count += usize::from(matches);
            }
        }
    }
    for row in &mut rows {
        row.total = row.groups.iter().map(|g| g.count).sum();
        row.known = row.groups.iter().map(|g| g.known).sum();
        row.orphans = row.groups[0].count;
        row.percent = percentage(row.orphans, row.total);
    }
    Summary { totals, rows }
}
fn rank(summary: &mut Summary) {
    summary.rows.sort_by(|a, b| {
        b.orphans
            .cmp(&a.orphans)
            .then_with(|| a.label.cmp(&b.label))
    });
}

type ProfileObservation = (String, bool, Option<Vec<String>>, Option<f64>, Option<f64>);
#[derive(Serialize)]
struct Profiles {
    topics: Summary,
    macrothemes: Summary,
    features: Summary,
    thresholds: BTreeMap<String, [Option<f64>; 2]>,
}
#[derive(Deserialize)]
struct Taxonomy {
    labels: Vec<String>,
}
fn profiles(pages: Vec<ProfileObservation>) -> Result<Profiles, String> {
    let taxonomy: Taxonomy =
        serde_json::from_str(include_str!("../config/topics.json")).map_err(|e| e.to_string())?;
    let macrothemes = ["Culture", "Geography", "History and Society", "STEM"];
    let mut totals = [0, 0];
    let mut samples = BTreeMap::<String, [Vec<f64>; 2]>::new();
    for (language, orphan, _, age, length) in &pages {
        totals[usize::from(!orphan)] += 1;
        let samples = samples.entry(language.clone()).or_default();
        if let Some(age) = measured(*age) {
            samples[0].push(age);
        }
        if let Some(length) = measured(*length) {
            samples[1].push(length);
        }
    }
    let thresholds: BTreeMap<_, _> = samples
        .into_iter()
        .map(|(language, mut samples)| {
            for values in &mut samples {
                values.sort_by(f64::total_cmp);
            }
            (language, samples.map(|values| median(&values)))
        })
        .collect();
    let mut topics = summary(
        taxonomy.labels.clone(),
        totals,
        pages.iter().map(|(_, orphan, topics, ..)| {
            let topics = topics
                .as_ref()
                .map(|labels| labels.iter().collect::<HashSet<_>>());
            (
                usize::from(!orphan),
                taxonomy
                    .labels
                    .iter()
                    .map(|label| topics.as_ref().map(|t| t.contains(label)))
                    .collect(),
            )
        }),
    );
    let mut broad_topics = summary(
        macrothemes.map(str::to_owned).to_vec(),
        totals,
        pages.iter().map(|(_, orphan, topics, ..)| {
            (
                usize::from(!orphan),
                macrothemes
                    .iter()
                    .map(|parent| {
                        topics.as_ref().map(|labels| {
                            labels
                                .iter()
                                .any(|label| label.starts_with(&format!("{parent}.")))
                        })
                    })
                    .collect(),
            )
        }),
    );
    let labels = ["new", "old", "quality_high", "quality_low", "short", "long"];
    let features = summary(
        labels.map(str::to_owned).to_vec(),
        totals,
        pages.iter().map(|(language, orphan, _, age, length)| {
            let split = |value: Option<f64>, index: usize| {
                measured(value)
                    .zip(thresholds.get(language).and_then(|v| v[index]))
                    .map(|(v, m)| v < m)
            };
            let age = split(*age, 0);
            let length = split(*length, 1);
            (
                usize::from(!orphan),
                vec![age, age.map(|v| !v), None, None, length, length.map(|v| !v)],
            )
        }),
    );
    rank(&mut topics);
    rank(&mut broad_topics);
    Ok(Profiles {
        topics,
        macrothemes: broad_topics,
        features,
        thresholds,
    })
}

#[derive(Deserialize)]
struct NumericInput {
    rows: Vec<(bool, Option<f64>)>,
    logarithmic: bool,
    age: bool,
}
#[derive(Serialize)]
struct NumericGroup {
    total: usize,
    known: usize,
    missing: usize,
    median: Option<f64>,
    counts: Vec<usize>,
    percentages: Vec<Option<f64>>,
    cumulative: Vec<[f64; 2]>,
}
#[derive(Serialize)]
struct NumericReport {
    known: usize,
    available: bool,
    groups: [NumericGroup; 2],
    count_max: usize,
    bin_edges: Vec<f64>,
    ticks: Vec<f64>,
    cumulative_ticks: Vec<f64>,
}
fn combined_rank(left: &[f64], right: &[f64], rank: usize) -> f64 {
    let (mut a, mut b, mut selected) = (0, 0, 0.0);
    for _ in 0..=rank {
        let first = left.get(a).copied().unwrap_or(f64::INFINITY);
        let second = right.get(b).copied().unwrap_or(f64::INFINITY);
        if first <= second {
            selected = first;
            a += 1;
        } else {
            selected = second;
            b += 1;
        }
    }
    selected
}
fn numeric(input: NumericInput) -> NumericReport {
    let mut totals = [0, 0];
    let mut groups = [Vec::new(), Vec::new()];
    for (orphan, value) in input.rows {
        let group = usize::from(!orphan);
        totals[group] += 1;
        if let Some(value) = measured(value) {
            groups[group].push(value);
        }
    }
    for group in &mut groups {
        group.sort_by(f64::total_cmp);
    }
    let transform = |v: f64| {
        if input.logarithmic {
            v.ln_1p()
        } else if input.age {
            v / 365.25
        } else {
            v
        }
    };
    let inverse = |v: f64| if input.logarithmic { v.exp_m1() } else { v };
    let maximum = groups
        .iter()
        .filter_map(|g| g.last())
        .map(|&v| transform(v))
        .fold(1.0, f64::max);
    let available = groups.iter().any(|g| !g.is_empty());
    let [orphans, non_orphans] = groups;
    let count = orphans.len() + non_orphans.len();
    let lower = if count == 0 {
        0.0
    } else {
        combined_rank(
            &orphans,
            &non_orphans,
            ((count - 1) as f64 * 0.02).floor() as usize,
        )
    };
    let upper = if count == 0 {
        1.0
    } else {
        combined_rank(
            &orphans,
            &non_orphans,
            ((count - 1) as f64 * 0.98).ceil() as usize,
        )
    };
    let lower_axis = transform(lower);
    let upper_axis = transform(upper);
    let central_width = upper_axis - lower_axis;
    let groups = [(orphans, totals[0]), (non_orphans, totals[1])].map(|(values, total)| {
        let known = values.len();
        let mut counts = vec![0; 20];
        for &value in &values {
            let bin = if value < lower {
                0
            } else if value > upper {
                19
            } else if central_width == 0.0 {
                10
            } else {
                1 + (((transform(value) - lower_axis) / central_width * 18.0).floor() as usize)
                    .min(17)
            };
            counts[bin] += 1;
        }
        let mut cumulative = Vec::new();
        let step = (known / 600).max(1);
        let mut i = 0;
        let mut previous = 0;
        while i < known {
            let mut j = i;
            while j + 1 < known && values[j + 1] == values[i] {
                j += 1;
            }
            cumulative.push([
                transform(values[i]) / maximum,
                (j + 1) as f64 / known as f64,
            ]);
            previous = j + 1;
            i = j + 1;
            if step > 1 && i + step < known {
                i += step - 1;
            }
        }
        if previous < known {
            if let Some(&last) = values.last() {
                cumulative.push([transform(last) / maximum, 1.0]);
            }
        }
        NumericGroup {
            total,
            known,
            missing: total - known,
            median: median(&values).map(|v| if input.age { v / 365.25 } else { v }),
            percentages: counts.iter().map(|&n| percentage(n, total)).collect(),
            counts,
            cumulative,
        }
    });
    let peak = groups
        .iter()
        .flat_map(|g| &g.counts)
        .copied()
        .max()
        .unwrap_or(1)
        .max(1);
    NumericReport {
        known: groups.iter().map(|g| g.known).sum(),
        available,
        groups,
        count_max: peak.div_ceil(4).max(1) * 4,
        bin_edges: std::iter::once(0.0)
            .chain((0..=18).map(|i| inverse(lower_axis + i as f64 * central_width / 18.0)))
            .chain(std::iter::once(inverse(maximum)))
            .collect(),
        ticks: (0..=4)
            .map(|i| inverse(lower_axis + i as f64 * central_width / 4.0))
            .collect(),
        cumulative_ticks: (0..=4).map(|i| inverse(i as f64 * maximum / 4.0)).collect(),
    }
}

#[derive(Serialize)]
struct CategoryRow {
    label: String,
    total: usize,
    orphans: usize,
    percent: Option<f64>,
}
fn category_counts(rows: Vec<(String, bool)>) -> Vec<CategoryRow> {
    let mut counts = BTreeMap::<String, [usize; 2]>::new();
    for (category, orphan) in rows {
        let counts = counts.entry(category).or_default();
        counts[0] += 1;
        counts[1] += usize::from(orphan);
    }
    let mut rows: Vec<_> = counts
        .into_iter()
        .map(|(label, [total, orphans])| CategoryRow {
            label,
            total,
            orphans,
            percent: percentage(orphans, total),
        })
        .collect();
    rows.sort_by(|a, b| {
        b.orphans
            .cmp(&a.orphans)
            .then_with(|| a.label.cmp(&b.label))
    });
    rows
}

#[derive(Deserialize)]
struct InterestEdit {
    language: String,
    topic_labels: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct InterestCandidate {
    key: String,
    language: String,
    topic_labels: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct InterestInput {
    edits: Vec<InterestEdit>,
    candidates: Vec<InterestCandidate>,
    aggregate: bool,
}

#[derive(Serialize)]
struct InterestScore {
    key: String,
    topic: f64,
    language: f64,
}

#[derive(Serialize)]
struct InterestReport {
    total_edits: usize,
    known_topic_edits: usize,
    scores: Vec<InterestScore>,
}

fn interest_scores(input: InterestInput) -> InterestReport {
    let total_edits = input.edits.len();
    let mut known_topic_edits = 0;
    let mut topics = BTreeMap::<String, f64>::new();
    let mut languages = BTreeMap::<String, usize>::new();
    for edit in &input.edits {
        *languages.entry(edit.language.clone()).or_default() += 1;
        if let Some(labels) = &edit.topic_labels {
            let unique: BTreeSet<_> = labels.iter().filter(|label| !label.is_empty()).collect();
            if !unique.is_empty() {
                known_topic_edits += 1;
                let share = 1.0 / unique.len() as f64;
                for label in unique {
                    *topics.entry(label.clone()).or_default() += share;
                }
            }
        }
    }
    let scores = input
        .candidates
        .into_iter()
        .map(|candidate| {
            let unique: BTreeSet<_> = candidate
                .topic_labels
                .as_ref()
                .into_iter()
                .flatten()
                .filter(|label| !label.is_empty())
                .collect();
            let topic = if known_topic_edits == 0 {
                0.0
            } else {
                unique
                    .into_iter()
                    .map(|label| topics.get(label).copied().unwrap_or(0.0))
                    .sum::<f64>()
                    / known_topic_edits as f64
            };
            let language = if input.aggregate && total_edits > 0 {
                languages.get(&candidate.language).copied().unwrap_or(0) as f64 / total_edits as f64
            } else {
                0.0
            };
            InterestScore {
                key: candidate.key,
                topic,
                language,
            }
        })
        .collect();
    InterestReport {
        total_edits,
        known_topic_edits,
        scores,
    }
}
#[derive(Serialize)]
struct CreatorRow {
    key: String,
    #[serde(rename = "type")]
    account_type: Option<String>,
    articles: Option<u64>,
    total: usize,
    orphans: usize,
    percent: Option<f64>,
    languages: BTreeSet<String>,
}
type CreatorObservation = (String, Option<String>, Option<u64>, bool, Vec<String>);
fn creator_rows(rows: Vec<CreatorObservation>) -> Vec<CreatorRow> {
    let mut counts = BTreeMap::<String, CreatorRow>::new();
    for (key, account_type, articles, orphan, languages) in rows {
        let row = counts.entry(key.clone()).or_insert_with(|| CreatorRow {
            key,
            account_type,
            articles,
            total: 0,
            orphans: 0,
            percent: None,
            languages: BTreeSet::new(),
        });
        row.total += 1;
        row.orphans += usize::from(orphan);
        row.languages.extend(languages);
    }
    for row in counts.values_mut() {
        row.percent = percentage(row.orphans, row.total);
    }
    counts.into_values().collect()
}
#[derive(Serialize)]
struct Origins {
    origins: Vec<CategoryRow>,
    bots: Vec<CategoryRow>,
    known_bots: usize,
}
fn origins(rows: Vec<(String, Option<bool>, bool)>) -> Origins {
    let mut bots = [[0, 0]; 3];
    let categories = rows
        .into_iter()
        .map(|(origin, bot, orphan)| {
            let group = match bot {
                Some(true) => 0,
                Some(false) => 1,
                None => 2,
            };
            bots[group][0] += 1;
            bots[group][1] += usize::from(orphan);
            (origin, orphan)
        })
        .collect();
    Origins {
        known_bots: bots[0][0] + bots[1][0],
        origins: category_counts(categories),
        bots: bots
            .into_iter()
            .enumerate()
            .map(|(i, [total, orphans])| CategoryRow {
                label: i.to_string(),
                total,
                orphans,
                percent: percentage(orphans, total),
            })
            .collect(),
    }
}
#[derive(Serialize)]
struct Overview {
    total: usize,
    orphans: usize,
    percent: Option<f64>,
    dead_ends: usize,
    attributed_orphans: usize,
    missing_creators: usize,
    distinct_creators: usize,
}
fn overview(rows: Vec<(bool, Option<u64>, Option<String>)>) -> Overview {
    let total = rows.len();
    let mut orphans = 0;
    let mut dead_ends = 0;
    let mut attributed_orphans = 0;
    let mut creators = BTreeSet::new();
    for (orphan, outgoing, key) in rows {
        dead_ends += usize::from(outgoing == Some(0));
        if orphan {
            orphans += 1;
            if let Some(key) = key.filter(|s| !s.is_empty()) {
                attributed_orphans += 1;
                creators.insert(key);
            }
        }
    }
    Overview {
        total,
        orphans,
        percent: percentage(orphans, total),
        dead_ends,
        attributed_orphans,
        missing_creators: orphans - attributed_orphans,
        distinct_creators: creators.len(),
    }
}

fn decode<T: for<'a> Deserialize<'a>>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|e| e.to_string())
}
fn encode(value: impl Serialize) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|e| e.to_string())
}
#[cfg_attr(feature = "dashboard", wasm_bindgen::prelude::wasm_bindgen)]
pub fn calculate(kind: &str, input: &str) -> Result<String, String> {
    match kind {
        "taxonomy" => encode(decode::<Taxonomy>(include_str!("../config/topics.json"))?.labels),
        "profiles" => encode(profiles(decode(input)?)?),
        "numeric" => encode(numeric(decode(input)?)),
        "categories" => encode(category_counts(decode(input)?)),
        "interests" => encode(interest_scores(decode(input)?)),
        "creators" => encode(creator_rows(decode(input)?)),
        "origins" => encode(origins(decode(input)?)),
        "overview" => encode(overview(decode(input)?)),
        _ => Err("unknown dashboard calculation".into()),
    }
}

// The same Rust calculations can be prepared once while publishing an export.
// Retain only the fields needed by charts and the creator index; never graph edges.
#[derive(Deserialize)]
struct SummaryPage {
    page_id: u64,
    title: String,
    language: Option<String>,
    orphan: bool,
    category: Option<String>,
    topic_status: Option<String>,
    topic_labels: Option<Vec<String>>,
    page_created_at: Option<String>,
    age_days: Option<f64>,
    history_first_revision_age_days: Option<f64>,
    length_bytes: Option<f64>,
    out_degree: Option<u64>,
    origin: Option<String>,
    creator_key: Option<String>,
    creator_account_type: Option<String>,
    creator_articles_created_total: Option<u64>,
    creator_languages_created: Option<Vec<String>>,
    creator_is_bot_now: Option<bool>,
    creator_prior_edits_main: Option<f64>,
    creator_prior_edits_other: Option<f64>,
    creator_prior_articles: Option<f64>,
    creator_tenure_days: Option<f64>,
    creator_orphans_current: Option<f64>,
    history_creator_key: Option<String>,
    creator_prior_edits_main_history: Option<f64>,
    creator_prior_edits_other_history: Option<f64>,
    creator_continuity_months_before_creation: Option<f64>,
    creator_groups_at_creation_history: Option<Vec<String>>,
}

#[derive(Serialize)]
struct DashboardSummary {
    overview: Overview,
    categories: Vec<CategoryRow>,
    profiles: Profiles,
    numeric: BTreeMap<String, NumericReport>,
    origins: Origins,
    creator_groups: Vec<CategoryRow>,
    creators: Vec<CreatorRow>,
}

pub struct SummaryAccumulator {
    language: String,
    aggregate: bool,
    historical_creators: bool,
    pages: Vec<SummaryPage>,
    identities: HashSet<(String, u64)>,
}

impl SummaryAccumulator {
    fn page_language<'a>(&'a self, page: &'a SummaryPage) -> &'a str {
        page.language.as_deref().unwrap_or(&self.language)
    }

    fn page_creator<'a>(&self, page: &'a SummaryPage) -> Option<&'a str> {
        if self.historical_creators {
            page.history_creator_key.as_deref()
        } else {
            page.creator_key.as_deref()
        }
    }

    pub fn new(language: String, aggregate: bool, historical_creators: bool) -> Self {
        Self {
            language,
            aggregate,
            historical_creators,
            pages: Vec::new(),
            identities: HashSet::new(),
        }
    }

    pub fn add_json(&mut self, input: &str) -> Result<(), String> {
        let pages: Vec<SummaryPage> = serde_json::from_str(input).map_err(|e| e.to_string())?;
        for page in pages {
            let language = page.language.as_deref().unwrap_or(&self.language);
            if page.page_id == 0 || language.is_empty() || page.title.is_empty() {
                return Err("invalid dashboard article identity".into());
            }
            if !self.identities.insert((language.to_owned(), page.page_id)) {
                return Err("duplicate dashboard article identity".into());
            }
            self.pages.push(page);
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn finish(&self) -> Result<String, String> {
        let creator_id = |page: &SummaryPage| {
            self.page_creator(page)
                .filter(|key| !key.is_empty())
                .map(|key| {
                    if self.aggregate {
                        format!("{}:{key}", self.page_language(page))
                    } else {
                        key.to_owned()
                    }
                })
        };
        let creators = creator_rows(
            self.pages
                .iter()
                .filter_map(|page| {
                    creator_id(page).map(|key| {
                        (
                            key,
                            (!self.historical_creators)
                                .then(|| page.creator_account_type.clone())
                                .flatten(),
                            (!self.historical_creators)
                                .then_some(page.creator_articles_created_total)
                                .flatten(),
                            page.orphan,
                            if self.historical_creators {
                                vec![self.page_language(page).to_owned()]
                            } else {
                                page.creator_languages_created.clone().unwrap_or_default()
                            },
                        )
                    })
                })
                .collect(),
        );
        let by_creator: BTreeMap<_, _> = creators
            .iter()
            .map(|row| (row.key.as_str(), (row.total, row.orphans)))
            .collect();
        let mut numeric_reports = BTreeMap::new();
        for field in [
            "age_days",
            "history_first_revision_age_days",
            "length_bytes",
            "creator_prior_edits_main",
            "creator_prior_edits_other",
            "creator_prior_articles",
            "creator_tenure_days",
            "creator_articles_created_total",
            "creator_orphans_current",
            "creator_continuity_months_before_creation",
        ] {
            let rows = self
                .pages
                .iter()
                .map(|page| {
                    let value = match field {
                        "age_days" => page.age_days,
                        "history_first_revision_age_days" => page.history_first_revision_age_days,
                        "length_bytes" => page.length_bytes,
                        "creator_prior_edits_main" if self.historical_creators => {
                            page.creator_prior_edits_main_history
                        }
                        "creator_prior_edits_main" => page.creator_prior_edits_main,
                        "creator_prior_edits_other" if self.historical_creators => {
                            page.creator_prior_edits_other_history
                        }
                        "creator_prior_edits_other" => page.creator_prior_edits_other,
                        "creator_prior_articles" if self.historical_creators => None,
                        "creator_prior_articles" => page.creator_prior_articles,
                        "creator_tenure_days" if self.historical_creators => None,
                        "creator_tenure_days" => page.creator_tenure_days,
                        "creator_articles_created_total" if self.historical_creators => {
                            creator_id(page)
                                .and_then(|key| by_creator.get(key.as_str()).map(|v| v.0 as f64))
                        }
                        "creator_articles_created_total" => {
                            page.creator_articles_created_total.map(|v| v as f64)
                        }
                        "creator_orphans_current" if self.historical_creators => creator_id(page)
                            .and_then(|key| by_creator.get(key.as_str()).map(|v| v.1 as f64)),
                        "creator_orphans_current" => page.creator_orphans_current,
                        _ => page.creator_continuity_months_before_creation,
                    };
                    (page.orphan, value)
                })
                .collect();
            numeric_reports.insert(
                field.to_owned(),
                numeric(NumericInput {
                    rows,
                    logarithmic: field == "length_bytes"
                        || (field.starts_with("creator_")
                            && field != "creator_continuity_months_before_creation"),
                    age: field == "age_days" || field == "history_first_revision_age_days",
                }),
            );
        }
        let report = DashboardSummary {
            overview: overview(
                self.pages
                    .iter()
                    .map(|page| (page.orphan, page.out_degree, creator_id(page)))
                    .collect(),
            ),
            categories: category_counts(
                self.pages
                    .iter()
                    .map(|page| {
                        let category = page.category.as_deref().unwrap_or("Senza categoria idonea");
                        let label = if self.aggregate {
                            format!("{} · {category}", self.page_language(page).to_uppercase())
                        } else {
                            category.to_owned()
                        };
                        (label, page.orphan)
                    })
                    .collect(),
            ),
            profiles: profiles(
                self.pages
                    .iter()
                    .map(|page| {
                        (
                            self.page_language(page).to_owned(),
                            page.orphan,
                            (page.topic_status.as_deref() == Some("predicted"))
                                .then(|| page.topic_labels.clone())
                                .flatten(),
                            page.page_created_at.as_ref().and(page.age_days),
                            page.length_bytes,
                        )
                    })
                    .collect(),
            )?,
            numeric: numeric_reports,
            origins: origins(
                self.pages
                    .iter()
                    .map(|page| {
                        (
                            page.origin.clone().unwrap_or_else(|| "unknown".into()),
                            page.creator_key.as_ref().and(page.creator_is_bot_now),
                            page.orphan,
                        )
                    })
                    .collect(),
            ),
            creator_groups: category_counts(
                self.pages
                    .iter()
                    .flat_map(|page| {
                        let labels = page
                            .creator_groups_at_creation_history
                            .as_ref()
                            .filter(|labels| !labels.is_empty())
                            .cloned()
                            .unwrap_or_else(|| vec!["__missing__".into()]);
                        labels
                            .into_iter()
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .map(move |label| (label, page.orphan))
                    })
                    .collect(),
            ),
            creators,
        };
        encode(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interest_score_prioritizes_topics_and_uses_language_only_in_aggregate() {
        let report = interest_scores(InterestInput {
            edits: vec![
                InterestEdit {
                    language: "it".into(),
                    topic_labels: Some(vec!["Culture.Media".into()]),
                },
                InterestEdit {
                    language: "lmo".into(),
                    topic_labels: Some(vec!["Culture.Media".into()]),
                },
                InterestEdit {
                    language: "lmo".into(),
                    topic_labels: None,
                },
            ],
            candidates: vec![
                InterestCandidate {
                    key: "vec:1".into(),
                    language: "vec".into(),
                    topic_labels: Some(vec!["Culture.Media".into()]),
                },
                InterestCandidate {
                    key: "lmo:2".into(),
                    language: "lmo".into(),
                    topic_labels: Some(vec!["Geography".into()]),
                },
            ],
            aggregate: true,
        });
        assert_eq!(report.total_edits, 3);
        assert_eq!(report.known_topic_edits, 2);
        assert_eq!(report.scores[0].topic, 1.0);
        assert_eq!(report.scores[0].language, 0.0);
        assert_eq!(report.scores[1].topic, 0.0);
        assert_eq!(report.scores[1].language, 2.0 / 3.0);
    }
    #[test]
    fn histogram_shared_bins_missing_denominators_and_inclusive_endpoint() {
        let report = numeric(NumericInput {
            rows: vec![
                (true, Some(0.0)),
                (true, Some(1.0)),
                (true, None),
                (false, Some(0.5)),
            ],
            logarithmic: false,
            age: false,
        });
        assert_eq!(report.groups[0].counts[1], 1);
        assert_eq!(report.groups[0].counts[18], 1);
        assert_eq!(report.groups[0].missing, 1);
        assert_eq!(report.groups[0].percentages[1], Some(100.0 / 3.0));
        assert_eq!(report.groups[0].cumulative.last(), Some(&[1.0, 1.0]));
        assert_eq!(report.groups[1].median, Some(0.5));
    }
    #[test]
    fn histogram_groups_both_tails_without_losing_observations() {
        let report = numeric(NumericInput {
            rows: (0..=100)
                .map(|value| (value % 2 == 0, Some(value as f64)))
                .collect(),
            logarithmic: false,
            age: false,
        });
        assert_eq!(report.bin_edges[1], 2.0);
        assert_eq!(report.bin_edges[19], 98.0);
        assert_eq!(report.groups[0].counts[0], 1);
        assert_eq!(report.groups[0].counts[19], 1);
        assert_eq!(report.groups[1].counts[0], 1);
        assert_eq!(report.groups[1].counts[19], 1);
        for group in &report.groups {
            assert_eq!(group.counts.iter().sum::<usize>(), group.known);
        }
        assert_eq!(report.cumulative_ticks[4], 100.0);
    }
    #[test]
    fn numeric_empty_groups_have_no_percentages_or_fabricated_values() {
        let report = numeric(NumericInput {
            rows: vec![(true, None)],
            logarithmic: true,
            age: false,
        });
        assert!(!report.available);
        assert_eq!(report.groups[1].total, 0);
        assert!(report.groups[1].percentages.iter().all(Option::is_none));
        assert_eq!(report.groups[0].median, None);
    }
    #[test]
    fn profiles_use_local_medians_multilabel_or_and_known_empty_labels() {
        let report = profiles(vec![
            (
                "vec".into(),
                true,
                Some(vec![
                    "Culture.Media.Music".into(),
                    "Culture.Media.Films".into(),
                ]),
                Some(1.0),
                Some(4.0),
            ),
            ("vec".into(), false, Some(vec![]), Some(3.0), Some(8.0)),
            ("lmo".into(), true, None, Some(100.0), None),
        ])
        .unwrap();
        assert_eq!(report.topics.rows.len(), 64);
        assert_eq!(report.macrothemes.rows.len(), 4);
        let culture = report
            .macrothemes
            .rows
            .iter()
            .find(|r| r.label == "Culture")
            .unwrap();
        assert_eq!(culture.orphans, 1);
        assert_eq!(culture.known, 2);
        assert_eq!(culture.percent, Some(100.0));
        assert_eq!(report.thresholds["vec"], [Some(2.0), Some(6.0)]);
        assert_eq!(report.features.rows[0].orphans, 1);
        assert_eq!(report.features.rows[1].orphans, 1);
        assert_eq!(report.features.rows[2].known, 0);
        assert_eq!(report.features.rows.len(), 6);
        assert_eq!(report.features.rows[4].known, 2);
        assert_eq!(report.features.rows[5].percent, Some(0.0));
    }
    #[test]
    fn aggregate_creators_remain_scoped_and_empty_bot_groups_undefined() {
        let report = creator_rows(vec![
            ("vec:1".into(), None, Some(2), true, vec!["vec".into()]),
            ("lmo:1".into(), None, None, false, vec!["lmo".into()]),
        ]);
        assert_eq!(report.len(), 2);
        assert_eq!(report[0].key, "lmo:1");
        let report = origins(vec![("unknown".into(), None, true)]);
        assert_eq!(report.bots[0].percent, None);
        assert_eq!(report.bots[2].percent, Some(100.0));
    }
    #[test]
    fn bridge_rejects_unknown_operations_and_invalid_json() {
        assert!(calculate("other", "[]").is_err());
        assert!(calculate("numeric", "{}").is_err());
        assert!(calculate("overview", "invalid").is_err());
        let report: serde_json::Value =
            serde_json::from_str(&calculate("overview", "[[true,0,null]]").unwrap()).unwrap();
        assert_eq!(report["orphans"], 1);
        assert_eq!(report["distinct_creators"], 0);
    }

    #[test]
    fn prepared_summary_keeps_historical_attribution_distinct_and_complete() {
        let mut prepared = SummaryAccumulator::new("all".into(), true, true);
        prepared
            .add_json(
                r#"[{"page_id":1,"title":"Alpha","language":"vec","orphan":true,
                      "category":"People","topic_status":"predicted","topic_labels":["Culture.Media.Music"],
                      "length_bytes":100,"out_degree":0,"history_creator_key":"history:7",
                      "creator_prior_edits_main_history":3,"creator_groups_at_creation_history":["sysop"]},
                     {"page_id":1,"title":"Beta","language":"lmo","orphan":false,
                      "category":"People","topic_status":"predicted","topic_labels":[],
                      "length_bytes":200,"out_degree":1,"history_creator_key":"history:7",
                      "creator_prior_edits_main_history":5}]"#,
            )
            .unwrap();
        assert_eq!(prepared.len(), 2);
        let report: serde_json::Value = serde_json::from_str(&prepared.finish().unwrap()).unwrap();
        assert_eq!(report["overview"]["total"], 2);
        assert_eq!(report["overview"]["attributed_orphans"], 1);
        assert_eq!(report["creators"].as_array().unwrap().len(), 2);
        assert_eq!(report["numeric"]["creator_prior_edits_main"]["known"], 2);
        assert_eq!(report["numeric"]["creator_prior_articles"]["known"], 0);
        assert_eq!(
            report["numeric"]["creator_articles_created_total"]["known"],
            2
        );
        assert_eq!(
            report["profiles"]["macrothemes"]["rows"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert!(prepared
            .add_json(r#"[{"page_id":1,"title":"Duplicate","language":"vec","orphan":true}]"#)
            .is_err());
    }
}
