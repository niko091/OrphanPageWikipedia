//! Complete census analysis shared by the command line and WebAssembly dashboard.
#![forbid(unsafe_code)]

#[path = "core/languages.rs"]
mod languages;
pub use languages::{configured_languages, select_languages, wiki_database};
#[path = "core/graph.rs"]
mod graph;
pub use graph::{graph_counts, graph_details, GraphAccumulator, TopicInputAccumulator};
#[path = "core/categories.rs"]
mod categories;
pub use categories::{classify_categories, CategoryBatchClassifier};
#[path = "core/observations.rs"]
mod observations;
#[cfg(feature = "cli")]
pub(crate) use observations::validate_topic_record;
pub use observations::{
    article_ages, collection_interval, initialize_creator_rows, join_history_rows,
    join_quality_rows, join_topic_rows, observation_ages, observation_row,
};
#[path = "core/matching.rs"]
mod matching;
#[cfg(feature = "cli")]
use matching::valid_item;
pub use matching::{
    matchable_edges, rank_candidates, threshold_topics, topic_inputs, unique_items,
};

pub mod dashboard;
pub mod dumps;
#[cfg(feature = "cli")]
pub mod editorial;
#[cfg(feature = "cli")]
pub mod pipeline;
#[cfg(feature = "cli")]
pub mod runtime;
#[cfg(feature = "cli")]
pub mod store;

use std::collections::{HashMap, HashSet};

pub type Counts = (Vec<u64>, Vec<u64>, usize);
pub type GraphDetails = (Vec<u64>, Vec<u64>, usize, Vec<Vec<usize>>);
pub type RankedCategory = (String, f64, u64, f64);
pub type CategoryChoice = (Option<String>, Vec<RankedCategory>, bool);
pub type CategoryRules = (
    Vec<String>,
    HashSet<String>,
    HashMap<String, f64>,
    HashMap<String, String>,
    f64,
);
pub type ReferenceEdge = (String, i64, String, i64, String);
pub type CandidateEdge = (i64, String, u64, String, i64, String, i64, String);
pub type Candidate = (i64, String, u64, Vec<ReferenceEdge>);

#[cfg(test)]
#[path = "core/tests.rs"]
mod tests;
