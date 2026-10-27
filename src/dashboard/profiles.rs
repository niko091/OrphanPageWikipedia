use super::*;

#[derive(Serialize)]
pub(super) struct Group {
    pub(super) count: usize,
    pub(super) known: usize,
    pub(super) total: usize,
}
#[derive(Serialize)]
pub(super) struct FeatureRow {
    pub(super) label: String,
    pub(super) groups: [Group; 2],
    pub(super) total: usize,
    pub(super) orphans: usize,
    pub(super) known: usize,
    pub(super) percent: Option<f64>,
}
#[derive(Serialize)]
pub(super) struct Summary {
    pub(super) totals: [usize; 2],
    pub(super) rows: Vec<FeatureRow>,
}
pub(super) fn percentage(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| 100.0 * numerator as f64 / denominator as f64)
}
pub(super) fn median(sorted: &[f64]) -> Option<f64> {
    let middle = sorted.len() / 2;
    if sorted.is_empty() {
        None
    } else if sorted.len() % 2 == 1 {
        Some(sorted[middle])
    } else {
        Some(sorted[middle - 1] / 2.0 + sorted[middle] / 2.0)
    }
}
pub(super) fn measured(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v >= 0.0)
}
pub(super) fn summary(
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
pub(super) fn rank(summary: &mut Summary) {
    summary.rows.sort_by(|a, b| {
        b.orphans
            .cmp(&a.orphans)
            .then_with(|| a.label.cmp(&b.label))
    });
}

pub(super) type ProfileObservation = (
    String,
    bool,
    Option<Vec<String>>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
);
#[derive(Serialize)]
pub(super) struct Profiles {
    pub(super) topics: Summary,
    pub(super) macrothemes: Summary,
    pub(super) features: Summary,
    pub(super) thresholds: BTreeMap<String, [Option<f64>; 3]>,
}
#[derive(Deserialize)]
pub(super) struct Taxonomy {
    pub(super) labels: Vec<String>,
}
pub(super) fn profiles(pages: Vec<ProfileObservation>) -> Result<Profiles, String> {
    let taxonomy: Taxonomy = serde_json::from_str(include_str!("../../config/topics.json"))
        .map_err(|e| e.to_string())?;
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
