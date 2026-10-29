use super::*;

// The same Rust calculations can be prepared once while publishing an export.
// Retain only the fields needed by charts and the creator index; never graph edges.
#[derive(Deserialize)]
pub(super) struct SummaryPage {
    pub(super) page_id: u64,
    pub(super) title: String,
    pub(super) language: Option<String>,
    pub(super) orphan: bool,
    pub(super) category: Option<String>,
    pub(super) topic_status: Option<String>,
    pub(super) topic_labels: Option<Vec<String>>,
    pub(super) page_created_at: Option<String>,
    pub(super) age_days: Option<f64>,
    pub(super) length_bytes: Option<f64>,
    pub(super) quality_score: Option<f64>,
    pub(super) quality_status: Option<String>,
    pub(super) out_degree: Option<u64>,
    pub(super) origin: Option<String>,
    pub(super) creator_key: Option<String>,
    pub(super) creator_account_type: Option<String>,
    pub(super) creator_articles_created_total: Option<u64>,
    pub(super) creator_languages_created: Option<Vec<String>>,
    pub(super) creator_is_bot_now: Option<bool>,
    pub(super) creator_prior_edits_main: Option<f64>,
    pub(super) creator_prior_edits_other: Option<f64>,
    pub(super) creator_prior_articles: Option<f64>,
    pub(super) creator_tenure_days: Option<f64>,
    pub(super) creator_orphans_current: Option<f64>,
    pub(super) history_creator_key: Option<String>,
    pub(super) creator_prior_edits_main_history: Option<f64>,
    pub(super) creator_prior_edits_other_history: Option<f64>,
    pub(super) creator_continuity_months_before_creation: Option<f64>,
    pub(super) creator_groups_at_creation_history: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(super) struct DashboardSummary {
    pub(super) overview: Overview,
    pub(super) categories: Vec<CategoryRow>,
    pub(super) profiles: Profiles,
    pub(super) numeric: BTreeMap<String, NumericReport>,
    pub(super) origins: Origins,
    pub(super) creator_groups: Vec<CategoryRow>,
    pub(super) creators: Vec<CreatorRow>,
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
pub(super) fn summary_bridge_float(number: &serde_json::Number) -> Result<f64, String> {
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
    pub(super) fn page_language<'a>(&'a self, page: &'a SummaryPage) -> &'a str {
        page.language.as_deref().unwrap_or(&self.language)
    }

    pub(super) fn page_creator<'a>(&self, page: &'a SummaryPage) -> Option<&'a str> {
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

    pub(super) fn push_page(&mut self, page: SummaryPage) -> Result<(), String> {
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
            let input = NumericInput {
                rows,
                logarithmic: field == "length_bytes"
                    || (field.starts_with("creator_")
                        && field != "creator_continuity_months_before_creation"),
                age: field == "age_days",
            };
            let report = if field == "creator_continuity_months_before_creation" {
                numeric::continuity(input)?
            } else {
                numeric(input)
            };
            numeric_reports.insert(field.to_owned(), report);
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
