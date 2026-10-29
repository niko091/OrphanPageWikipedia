//! Descriptive dashboard statistics shared by Rust tests and browser WebAssembly.
//! Inputs contain observations only; no network, research-source or edit access.
//! Category/features/counts are O(V + labels), numeric sorting O(V log V).
//! Transient memory is O(V) for the selected observations, never graph edges.
mod profiles;
use profiles::{measured, median, percentage, profiles, Profiles, Taxonomy};
mod numeric;
#[cfg(feature = "cli")]
pub(crate) use numeric::MonthHistogram;
use numeric::{numeric, NumericInput, NumericReport};
mod categories;
use categories::{category_counts, CategoryRow};
mod interests;
use interests::interest_scores;
mod creators;
use creators::{creator_rows, origins, overview, CreatorRow, Origins, Overview};
mod summary;
pub use summary::SummaryAccumulator;

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

#[cfg(test)]
mod tests;
