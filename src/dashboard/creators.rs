use super::*;

#[derive(Serialize)]
pub(super) struct CreatorRow {
    pub(super) key: String,
    #[serde(rename = "type")]
    pub(super) account_type: Option<String>,
    pub(super) articles: Option<u64>,
    pub(super) total: usize,
    pub(super) orphans: usize,
    pub(super) percent: Option<f64>,
    pub(super) languages: BTreeSet<String>,
}
pub(super) type CreatorObservation = (String, Option<String>, Option<u64>, bool, Vec<String>);
pub(super) fn creator_rows(rows: Vec<CreatorObservation>) -> Vec<CreatorRow> {
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
pub(super) struct Origins {
    pub(super) origins: Vec<CategoryRow>,
    pub(super) bots: Vec<CategoryRow>,
    pub(super) known_bots: usize,
}
pub(super) fn origins(rows: Vec<(String, Option<bool>, bool)>) -> Origins {
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
pub(super) struct Overview {
    pub(super) total: usize,
    pub(super) orphans: usize,
    pub(super) percent: Option<f64>,
    pub(super) dead_ends: usize,
    pub(super) attributed_orphans: usize,
    pub(super) missing_creators: usize,
    pub(super) distinct_creators: usize,
}
pub(super) fn overview(rows: Vec<(bool, Option<u64>, Option<String>)>) -> Overview {
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
