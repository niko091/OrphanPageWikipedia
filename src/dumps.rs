//! Bounded MySQL decoding and optional standalone verified dump acquisition,
//! SQL/History imports, XML structural quality and local fastText topic inference.

mod sql;
#[cfg(feature = "cli")]
use sql::next_relation;
pub use sql::parse_insert;
#[cfg(feature = "cli")]
pub use sql::{import_dumps, import_graph};
mod history;
pub use history::history_activity;
#[cfg(feature = "cli")]
pub(crate) use history::HISTORY_RULE;
#[cfg(feature = "cli")]
use history::{history_decimal, history_files, history_timestamp};
#[cfg(feature = "cli")]
pub use history::{import_history, HistoryOptions};
mod quality;
#[cfg(feature = "cli")]
pub use quality::infer_quality;
pub use quality::QualityExtractor;
#[cfg(feature = "cli")]
mod download;
#[cfg(feature = "cli")]
pub use download::{DumpClient, DumpHttp, DumpResponse, WikimediaHttp};
mod canonical;
pub(crate) use canonical::python_float;
#[cfg(feature = "cli")]
pub use canonical::{
    canonical_json_sha256, export_csv, first_metadata, write_canonical_json, CSV_COLUMNS,
};
#[cfg(feature = "cli")]
pub(crate) use canonical::{
    file_signature, stage_export, visit_ordered_json, write_csv_row, CanonicalExport,
};
#[cfg(feature = "cli")]
use canonical::{python_json, write_atomic, write_sorted_json, CanonicalDigest};
#[cfg(feature = "cli")]
mod publication;
#[cfg(feature = "cli")]
pub(crate) use publication::dataset_filename;
#[cfg(feature = "cli")]
pub use publication::{analyze_file, rebuild_dashboard};
#[cfg(feature = "cli")]
use publication::{
    export_directory, manifest_entry, prepare_member, publish_prepared, reuse_export,
    PreparedDirectory, PublicationLock, ReusedExport, TRANSPORT_RULE,
};
#[cfg(feature = "cli")]
mod transports;
#[cfg(feature = "cli")]
pub(crate) use transports::editorial_shortlist;
#[cfg(feature = "cli")]
pub use transports::prepare_dashboard_exports;
#[cfg(feature = "cli")]
use transports::{add_article_preview, write_summary_files};
#[cfg(feature = "cli")]
mod supplements;
#[cfg(feature = "cli")]
use supplements::supplement_metadata;
#[cfg(feature = "cli")]
pub use supplements::{stage_supplement, SupplementKind};
#[cfg(feature = "cli")]
mod topics;
#[cfg(feature = "cli")]
use topics::topic_time;
#[cfg(feature = "cli")]
pub use topics::{
    predict_imported_topics, predict_topics, run_topics, NativeTopicModel, TopicModel,
    TopicPrediction, TopicRunOptions,
};
#[cfg(feature = "cli")]
mod imported_graph;
#[cfg(feature = "cli")]
pub(crate) use imported_graph::file_sha256;
#[cfg(feature = "cli")]
use imported_graph::{
    sql_category_parents, sql_redirects, stage_page_batches, ImportedGraph, RecordSpool,
    StagedRecords,
};
#[cfg(feature = "cli")]
mod analysis;
#[cfg(feature = "cli")]
pub use analysis::{analyze_imported_graph, analyze_memory_graph, AnalysisOptions};
#[cfg(feature = "cli")]
mod comparison;
#[cfg(feature = "cli")]
use comparison::approved_graph;
#[cfg(feature = "cli")]
pub use comparison::{
    comparison_schema, export_comparison_member, match_comparison, stage_comparison_graph,
};
#[cfg(feature = "cli")]
mod comparison_cache;
#[cfg(feature = "cli")]
use comparison_cache::comparison_signature;
#[cfg(feature = "cli")]
pub use comparison_cache::{compare_files, seed_comparison_pairs, ComparisonOptions};
#[cfg(feature = "cli")]
mod aggregate;
#[cfg(feature = "cli")]
use aggregate::available_scientific_name;
#[cfg(feature = "cli")]
pub use aggregate::{
    aggregate_index, comparison_collections, comparison_filename, publish_available_aggregate,
    publish_comparison, write_aggregate_summary,
};
#[cfg(feature = "cli")]
pub(crate) use aggregate::{aggregate_summary_metadata, prepared_summaries};
#[cfg(feature = "cli")]
mod refresh;
#[cfg(feature = "cli")]
pub use refresh::refresh_measures;

pub const DUMP_BASE: &str = "https://dumps.wikimedia.org";
pub const SQL_TABLES: [&str; 7] = [
    "page",
    "pagelinks",
    "linktarget",
    "redirect",
    "page_props",
    "categorylinks",
    "user_groups",
];

pub(crate) type DumpResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(feature = "cli")]
pub(crate) fn resolved_path(path: &std::path::Path) -> DumpResult<std::path::PathBuf> {
    use std::path::Component;
    let path = std::env::current_dir()?.join(path);
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
        if normalized.exists() {
            normalized = normalized.canonicalize()?;
        }
    }
    Ok(normalized)
}

/// Complete display projection; column order is the observations_v1 contract.
#[cfg(feature = "cli")]
pub const OBSERVATION_COLUMNS: &[&str] = &[
    "page_id",
    "title",
    "category",
    "category_status",
    "orphan",
    "in_degree",
    "out_degree",
    "created_at",
    "age_days",
    "history_first_revision_age_days",
    "length_bytes",
    "origin",
    "creator_key",
    "creator_data_status",
    "creator_account_type",
    "creator_tenure_days",
    "creator_prior_edits_main",
    "creator_prior_edits_other",
    "creator_prior_articles",
    "creator_articles_created_total",
    "creator_articles_in_census",
    "creator_orphans_current",
    "creator_orphans_current_share",
    "creator_orphans_at_30_days",
    "creator_attribution",
    "creator_registered_at",
    "creator_tenure_status",
    "creator_experience_status",
    "topic_labels",
    "topic_status",
    "topic_input_count",
    "wikibase_item",
    "page_created_at",
    "first_public_revision_at",
    "history_first_revision_at",
    "creation_date_source",
    "creation_date_status",
    "creator_account_type_at_creation",
    "creator_is_bot_now",
    "creator_languages_created",
    "creator_languages_before",
    "creation_tags",
    "import_log_observed",
    "move_log_observed",
    "creator_orphans_at_30_days_status",
    "history_creator_key",
    "history_creator_status",
    "history_data_status",
    "creator_groups_at_creation_history",
    "creator_continuity_months_before_creation",
    "creator_continuity_bucket_before_creation",
    "creator_prior_edits_main_history",
    "creator_prior_edits_other_history",
    "quality_score",
    "quality_status",
    "content_revision_id",
    "quality_revision_relation",
];

#[cfg(feature = "cli")]
pub const ARTICLE_INDEX_COLUMNS: &[&str] = &[
    "page_id",
    "title",
    "orphan",
    "category",
    "topic_status",
    "topic_labels",
    "length_bytes",
    "created_at",
    "in_degree",
];

#[cfg(test)]
mod tests;
