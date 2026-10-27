use super::*;

#[derive(Serialize)]
pub(super) struct CategoryRow {
    pub(super) label: String,
    pub(super) total: usize,
    pub(super) orphans: usize,
    pub(super) percent: Option<f64>,
}
pub(super) fn category_counts(rows: Vec<(String, bool)>) -> Vec<CategoryRow> {
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
