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
    let groups = [(orphans, totals[0]), (non_orphans, totals[1])].map(|(values, total)| {
        let known = values.len();
        let mut counts = vec![0; 20];
        for &value in &values {
            let bin = ((transform(value) / maximum * 20.0).floor() as usize).min(19);
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
        bin_edges: (0..=20)
            .map(|i| inverse(i as f64 * maximum / 20.0))
            .collect(),
        ticks: (0..=4).map(|i| inverse(i as f64 * maximum / 4.0)).collect(),
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
        "creators" => encode(creator_rows(decode(input)?)),
        "origins" => encode(origins(decode(input)?)),
        "overview" => encode(overview(decode(input)?)),
        _ => Err("unknown dashboard calculation".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(report.groups[0].counts[0], 1);
        assert_eq!(report.groups[0].counts[19], 1);
        assert_eq!(report.groups[0].missing, 1);
        assert_eq!(report.groups[0].percentages[0], Some(100.0 / 3.0));
        assert_eq!(report.groups[0].cumulative.last(), Some(&[1.0, 1.0]));
        assert_eq!(report.groups[1].median, Some(0.5));
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
}
