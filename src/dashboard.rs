//! Descriptive dashboard statistics shared by Rust tests and browser WebAssembly.
//! Inputs contain observations only; no network, research-source or edit access.
//! Category/features/counts are O(V + labels), numeric sorting O(V log V).
//! Transient memory is O(V) for the selected observations, never graph edges.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Johnson V2 transformations in documented feature order. Empty wikitext is
/// a measured zero; ratios with a zero denominator contribute zero.
pub fn quality_transform(features: [u64; 6]) -> [f64; 6] {
    let [bytes, references, headings, links, media, categories] = features;
    let root = (bytes as f64).sqrt();
    let ratio = |value| if root > 0.0 { value / root } else { 0.0 };
    [
        root,
        ratio(references as f64),
        ratio(headings as f64),
        ratio((links as f64).sqrt()),
        media as f64,
        categories as f64,
    ]
}

pub fn quality_scores(rows: &[[u64; 6]], maxima: [f64; 6]) -> Result<Vec<f64>, String> {
    if maxima
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err("invalid quality normalizers".into());
    }
    let weights = [0.395, 0.181, 0.123, 0.115, 0.114, 0.070];
    Ok(rows
        .iter()
        .map(|row| {
            quality_transform(*row)
                .iter()
                .zip(maxima)
                .zip(weights)
                .map(|((value, maximum), weight)| weight * value.min(maximum) / maximum)
                .sum()
        })
        .collect())
}

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

type ProfileObservation = (
    String,
    bool,
    Option<Vec<String>>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
);
#[derive(Serialize)]
struct Profiles {
    topics: Summary,
    macrothemes: Summary,
    features: Summary,
    thresholds: BTreeMap<String, [Option<f64>; 3]>,
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
    let mut samples = BTreeMap::<String, [Vec<f64>; 3]>::new();
    for (language, orphan, _, age, length, quality) in &pages {
        totals[usize::from(!orphan)] += 1;
        let samples = samples.entry(language.clone()).or_default();
        if let Some(age) = measured(*age) {
            samples[0].push(age);
        }
        if let Some(length) = measured(*length) {
            samples[1].push(length);
        }
        if let Some(quality) = measured(*quality).filter(|v| *v <= 0.998001) {
            samples[2].push(quality);
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
        pages
            .iter()
            .map(|(language, orphan, _, age, length, quality)| {
                let split = |value: Option<f64>, index: usize| {
                    measured(value)
                        .zip(thresholds.get(language).and_then(|v| v[index]))
                        .map(|(v, m)| v < m)
                };
                let age = split(*age, 0);
                let length = split(*length, 1);
                let quality = split(*quality, 2);
                (
                    usize::from(!orphan),
                    vec![
                        age,
                        age.map(|v| !v),
                        quality.map(|v| !v),
                        quality,
                        length,
                        length.map(|v| !v),
                    ],
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
    length_bytes: Option<f64>,
    quality_score: Option<f64>,
    quality_status: Option<String>,
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

// Preserve the frozen Python -> JSON -> serde_json 1.0.150 chart-input bridge.
// The CLI enables float_roundtrip for canonical records, but approved chart
// reports historically read Python's shortest decimal via the fast parser's
// significand/power operations. Restrict this behavior to summary inputs; do
// not round canonical observations, CSV, previews or transport projections.
fn summary_bridge_float(number: &serde_json::Number) -> Result<f64, String> {
    static POWERS: std::sync::OnceLock<Result<Vec<f64>, String>> = std::sync::OnceLock::new();
    let powers = POWERS
        .get_or_init(|| {
            (0..=308)
                .map(|i| format!("1e{i}").parse::<f64>().map_err(|e| e.to_string()))
                .collect()
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let encoded = crate::dumps::python_float(number).map_err(|e| e.to_string())?;
    let positive = !encoded.starts_with('-');
    let encoded = encoded.strip_prefix('-').unwrap_or(&encoded);
    let (mantissa, exponent) = encoded.split_once('e').unwrap_or((encoded, "0"));
    let fractional = mantissa
        .split_once('.')
        .map_or(0, |(_, digits)| digits.len());
    let significand = mantissa
        .replace('.', "")
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    let mut exponent = exponent.parse::<i32>().map_err(|e| e.to_string())?
        - i32::try_from(fractional).map_err(|e| e.to_string())?;
    let mut value = significand as f64;
    loop {
        if let Some(power) = powers.get(exponent.unsigned_abs() as usize) {
            if exponent >= 0 {
                value *= power;
            } else {
                value /= power;
            }
            break;
        }
        if value == 0.0 {
            break;
        }
        if exponent >= 0 {
            return Err("chart input number out of range".into());
        }
        value /= 1e308;
        exponent += 308;
    }
    if !value.is_finite() {
        return Err("chart input number out of range".into());
    }
    Ok(if positive { value } else { -value })
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
            self.push_page(page)?;
        }
        Ok(())
    }

    /// Read only chart fields from an already decoded observation, preserving
    /// the frozen Python JSON bridge's numeric ingestion. Member
    /// exports ignore an incidental page.language; aggregate callers supply
    /// the verified member language explicitly, as the Python projection does.
    pub fn add_observation(
        &mut self,
        input: &serde_json::Value,
        language: Option<&str>,
    ) -> Result<(), String> {
        let fields = input.as_object().ok_or("invalid dashboard observation")?;
        let projection = serde::de::value::MapDeserializer::<_, serde_json::Error>::new(
            fields
                .iter()
                .filter(|(key, _)| key.as_str() != "language")
                .map(|(key, value)| (key.as_str(), value)),
        );
        let mut page = SummaryPage::deserialize(projection).map_err(|e| e.to_string())?;
        page.language = language.map(str::to_owned);
        for (key, field) in [
            ("age_days", &mut page.age_days),
            ("length_bytes", &mut page.length_bytes),
            ("quality_score", &mut page.quality_score),
            (
                "creator_prior_edits_main",
                &mut page.creator_prior_edits_main,
            ),
            (
                "creator_prior_edits_other",
                &mut page.creator_prior_edits_other,
            ),
            ("creator_prior_articles", &mut page.creator_prior_articles),
            ("creator_tenure_days", &mut page.creator_tenure_days),
            ("creator_orphans_current", &mut page.creator_orphans_current),
            (
                "creator_prior_edits_main_history",
                &mut page.creator_prior_edits_main_history,
            ),
            (
                "creator_prior_edits_other_history",
                &mut page.creator_prior_edits_other_history,
            ),
            (
                "creator_continuity_months_before_creation",
                &mut page.creator_continuity_months_before_creation,
            ),
        ] {
            if let serde_json::Value::Number(number) = &input[key] {
                if number.is_f64() {
                    *field = Some(summary_bridge_float(number)?);
                }
            }
        }
        self.push_page(page)
    }

    fn push_page(&mut self, page: SummaryPage) -> Result<(), String> {
        let language = page.language.as_deref().unwrap_or(&self.language);
        if page.page_id == 0 || language.is_empty() || page.title.is_empty() {
            return Err("invalid dashboard article identity".into());
        }
        if !self.identities.insert((language.to_owned(), page.page_id)) {
            return Err("duplicate dashboard article identity".into());
        }
        self.pages.push(page);
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
                            page.creator_account_type.clone(),
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
                        "age_days" => page.page_created_at.as_ref().and(page.age_days),
                        "length_bytes" => page.length_bytes,
                        "creator_prior_edits_main" if self.historical_creators => {
                            page.creator_prior_edits_main_history
                        }
                        "creator_prior_edits_main" => page.creator_prior_edits_main,
                        "creator_prior_edits_other" if self.historical_creators => {
                            page.creator_prior_edits_other_history
                        }
                        "creator_prior_edits_other" => page.creator_prior_edits_other,
                        "creator_prior_articles" => page.creator_prior_articles,
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
                    age: field == "age_days",
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
                            (page.quality_status.as_deref() == Some("scored"))
                                .then_some(page.quality_score)
                                .flatten(),
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
                            self.page_creator(page).and(page.creator_is_bot_now),
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
    fn chart_float_ingestion_preserves_the_frozen_bridge_without_mutating_observations() {
        let input = serde_json::json!({"page_id":1,"title":"Exact input","orphan":true,
            "quality_status":"scored","quality_score":0.9859704508633369});
        let before = input.clone();
        let mut summary = SummaryAccumulator::new("vec".into(), false, false);
        summary.add_observation(&input, None).unwrap();
        let report: serde_json::Value = serde_json::from_str(&summary.finish().unwrap()).unwrap();
        assert_eq!(
            report["profiles"]["thresholds"]["vec"][2]
                .as_f64()
                .unwrap()
                .to_bits(),
            0.9859704508633368f64.to_bits()
        );
        assert_eq!(input, before);
        assert_eq!(
            input["quality_score"].as_f64().unwrap().to_bits(),
            0.9859704508633369f64.to_bits()
        );
    }

    #[cfg(not(feature = "cli"))]
    #[test]
    fn chart_bridge_matches_the_original_parser_on_finite_binary64_inputs() {
        let mut bits = 918_271_613u64;
        for _ in 0..100_000 {
            bits = bits.wrapping_mul(6364136223846793005).wrapping_add(1);
            let value = f64::from_bits(bits);
            let Some(number) = serde_json::Number::from_f64(value) else {
                continue;
            };
            let encoded = crate::dumps::python_float(&number).unwrap();
            let original = serde_json::from_str::<f64>(&encoded);
            let bridged = summary_bridge_float(&number);
            match (original, bridged) {
                (Ok(a), Ok(b)) => assert_eq!(a.to_bits(), b.to_bits(), "{encoded}"),
                (Err(_), Err(_)) => {}
                pair => panic!("bridge and reference disagree for {encoded}: {pair:?}"),
            }
        }
    }
    #[test]
    fn decoded_observations_ignore_incidental_languages_and_scope_verified_members() {
        let page = serde_json::json!({"page_id":1,"title":"A","orphan":true,
            "language":{"not":"a member identity"},"history_creator_key":"history:7",
            "creator_prior_edits_main_history":3,"unused":[1,2,3]});
        let mut member = SummaryAccumulator::new("vec".into(), false, true);
        member.add_observation(&page, None).unwrap();
        assert!(member.add_observation(&page, None).is_err());
        assert_eq!(member.len(), 1);
        let report: serde_json::Value = serde_json::from_str(&member.finish().unwrap()).unwrap();
        assert_eq!(report["overview"]["attributed_orphans"], 1);
        assert_eq!(report["creators"][0]["key"], "history:7");
        let mut aggregate = SummaryAccumulator::new("all".into(), true, true);
        aggregate.add_observation(&page, Some("vec")).unwrap();
        aggregate.add_observation(&page, Some("lmo")).unwrap();
        assert_eq!(aggregate.len(), 2);
        let report: serde_json::Value = serde_json::from_str(&aggregate.finish().unwrap()).unwrap();
        assert_eq!(report["creators"].as_array().unwrap().len(), 2);
        assert_eq!(report["overview"]["total"], 2);
        assert!(aggregate
            .add_observation(&serde_json::json!([]), None)
            .is_err());
        assert!(aggregate
            .add_observation(
                &serde_json::json!({"page_id":2,"title":"","orphan":true}),
                None
            )
            .is_err());
        assert_eq!(aggregate.len(), 2);
    }
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
                None,
            ),
            (
                "vec".into(),
                false,
                Some(vec![]),
                Some(3.0),
                Some(8.0),
                None,
            ),
            ("lmo".into(), true, None, Some(100.0), None, None),
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
        assert_eq!(report.thresholds["vec"], [Some(2.0), Some(6.0), None]);
        assert_eq!(report.features.rows[0].orphans, 1);
        assert_eq!(report.features.rows[1].orphans, 1);
        assert_eq!(report.features.rows[2].known, 0);
        assert_eq!(report.features.rows.len(), 6);
        assert_eq!(report.features.rows[4].known, 2);
        assert_eq!(report.features.rows[5].percent, Some(0.0));
    }

    #[test]
    fn quality_features_split_at_local_medians_and_missing_is_unknown() {
        let report = profiles(vec![
            ("vec".into(), true, None, None, None, Some(0.1)),
            ("vec".into(), false, None, None, None, Some(0.3)),
            ("lmo".into(), true, None, None, None, Some(0.8)),
            ("lmo".into(), false, None, None, None, None),
        ])
        .unwrap();
        assert_eq!(report.thresholds["vec"][2], Some(0.2));
        assert_eq!(report.thresholds["lmo"][2], Some(0.8));
        assert_eq!(report.features.rows[2].known, 3);
        assert_eq!(report.features.rows[2].total, 2);
        assert_eq!(report.features.rows[2].orphans, 1);
        assert_eq!(report.features.rows[3].total, 1);
        let scores = quality_scores(
            &[[10000, 15, 10, 100, 2, 5], [0; 6]],
            [100.0, 0.15, 0.1, 0.1, 2.0, 5.0],
        )
        .unwrap();
        assert!((scores[0] - 0.998).abs() < 1e-12);
        assert_eq!(scores[1], 0.0);
        assert!(quality_scores(&[], [0.0; 6]).is_err());
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
    fn prepared_age_distribution_and_features_require_reconstructed_creation() {
        let mut prepared = SummaryAccumulator::new("vec".into(), false, false);
        prepared
            .add_json(
                r#"[{"page_id":1,"title":"New","orphan":true,
                      "page_created_at":"2020-01-01T00:00:00Z","age_days":365.25,
                      "history_first_revision_age_days":9000},
                     {"page_id":2,"title":"Old","orphan":false,
                      "page_created_at":"2019-01-01T00:00:00Z","age_days":730.5,
                      "history_first_revision_age_days":9000},
                     {"page_id":3,"title":"Missing creation","orphan":true,
                      "age_days":9000,"history_first_revision_age_days":9000},
                     {"page_id":4,"title":"Missing age","orphan":false,
                      "page_created_at":"2020-01-01T00:00:00Z",
                      "history_first_revision_age_days":9000}]"#,
            )
            .unwrap();
        let report: serde_json::Value = serde_json::from_str(&prepared.finish().unwrap()).unwrap();
        let ages = &report["numeric"]["age_days"];
        assert_eq!(ages["known"], 2);
        assert_eq!(ages["groups"][0]["median"], 1.0);
        assert_eq!(ages["groups"][1]["median"], 2.0);
        assert_eq!(ages["groups"][0]["missing"], 1);
        assert_eq!(ages["groups"][1]["missing"], 1);
        assert!(report["numeric"]
            .get("history_first_revision_age_days")
            .is_none());
        let features = report["profiles"]["features"]["rows"].as_array().unwrap();
        for label in ["new", "old"] {
            let row = features.iter().find(|row| row["label"] == label).unwrap();
            assert_eq!(row["known"], 2);
            assert_eq!(row["total"], 1);
            assert_eq!(row["orphans"], usize::from(label == "new"));
        }
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
