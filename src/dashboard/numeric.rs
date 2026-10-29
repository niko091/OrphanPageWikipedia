use super::*;

#[derive(Deserialize)]
pub(super) struct NumericInput {
    pub(super) rows: Vec<(bool, Option<f64>)>,
    pub(super) logarithmic: bool,
    pub(super) age: bool,
}
#[derive(Serialize)]
pub(super) struct NumericGroup {
    pub(super) total: usize,
    pub(super) known: usize,
    pub(super) missing: usize,
    pub(super) median: Option<f64>,
    pub(super) counts: Vec<usize>,
    pub(super) percentages: Vec<Option<f64>>,
    pub(super) cumulative: Vec<[f64; 2]>,
}
#[derive(Serialize)]
pub(super) struct NumericReport {
    pub(super) known: usize,
    pub(super) available: bool,
    pub(super) groups: [NumericGroup; 2],
    pub(super) count_max: usize,
    pub(super) bin_edges: Vec<f64>,
    pub(super) ticks: Vec<f64>,
    pub(super) cumulative_ticks: Vec<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) month_histogram: Option<MonthHistogram>,
}
#[derive(Serialize)]
pub(crate) struct MonthHistogram {
    pub(super) rule: &'static str,
    pub(super) through: usize,
    pub(super) count_max: usize,
    pub(super) groups: [MonthGroup; 2],
}
impl MonthHistogram {
    pub(crate) fn from_counts(counts: [Vec<usize>; 2], totals: [usize; 2]) -> Self {
        let count_max = counts
            .iter()
            .flatten()
            .copied()
            .max()
            .unwrap_or(0)
            .div_ceil(4)
            .max(1)
            * 4;
        let [first, second] = counts;
        let group = |counts: Vec<usize>, total| MonthGroup {
            percentages: counts
                .iter()
                .map(|&count| percentage(count, total))
                .collect(),
            counts,
        };
        Self {
            rule: "months_0_12_overflow_v1",
            through: 12,
            count_max,
            groups: [group(first, totals[0]), group(second, totals[1])],
        }
    }
}
#[derive(Serialize)]
pub(super) struct MonthGroup {
    pub(super) counts: Vec<usize>,
    pub(super) percentages: Vec<Option<f64>>,
}

// Continuity is a discrete count of completed months. Keep short runs separate
// and retain every longer run in an explicit overflow category. The original
// full-range cumulative report and all denominators are unchanged.
pub(super) fn continuity(input: NumericInput) -> Result<NumericReport, String> {
    let through = 12;
    let mut counts = [vec![0; through + 2], vec![0; through + 2]];
    for &(orphan, value) in &input.rows {
        if let Some(value) = measured(value) {
            if value.fract() != 0.0 {
                return Err("continuity must be a whole number of completed months".into());
            }
            counts[usize::from(!orphan)][value.min((through + 1) as f64) as usize] += 1;
        }
    }
    let mut report = numeric(input);
    report.month_histogram = Some(MonthHistogram::from_counts(
        counts,
        report.groups.each_ref().map(|group| group.total),
    ));
    Ok(report)
}
pub(super) fn combined_rank(left: &[f64], right: &[f64], rank: usize) -> f64 {
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
pub(super) fn numeric(input: NumericInput) -> NumericReport {
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
        month_histogram: None,
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
