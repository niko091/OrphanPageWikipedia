//! Sequential native collection controller and verified reuse decisions.
mod checkpoints;
use checkpoints::{
    current_publication, fetched_outputs, imported_reason, manifest_for, read_json, topics_match,
    PublicationInputs, StepState, Steps,
};
mod execution;
use execution::{host_inventory, write_operational_report, RecordingOptions, Runner};
mod benchmark;
pub use benchmark::{benchmark, BenchmarkOptions};
mod workflow;
use workflow::collection_workflow;

use crate::{dumps, runtime};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};

type PipelineResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone)]
pub struct PipelineOptions {
    pub root: PathBuf,
    pub languages_file: PathBuf,
    pub selected: Vec<(String, String)>,
    pub sql_release: String,
    pub history_release: String,
    pub content_release: String,
    pub dump_root: PathBuf,
    pub import_dir: PathBuf,
    pub output_dir: PathBuf,
    pub collection_database: Option<PathBuf>,
    pub work_dir: PathBuf,
    pub log_dir: PathBuf,
    pub model: PathBuf,
    pub model_config: PathBuf,
    pub topic_budget_seconds: f64,
    pub topic_batch_size: u32,
    pub history_cache_mib: u32,
    pub history_commit_rows: u32,
    pub metrics_interval: f64,
    pub no_phase_spans: bool,
    pub force: bool,
    pub plan: bool,
}

pub fn run(mut options: PipelineOptions) -> PipelineResult<()> {
    options.root = dumps::resolved_path(&options.root)?;
    for path in [
        &mut options.languages_file,
        &mut options.dump_root,
        &mut options.import_dir,
        &mut options.output_dir,
        &mut options.work_dir,
        &mut options.log_dir,
        &mut options.model,
        &mut options.model_config,
    ] {
        *path = dumps::resolved_path(&options.root.join(&*path))?;
    }
    if let Some(path) = &mut options.collection_database {
        *path = dumps::resolved_path(&options.root.join(&*path))?;
    }
    let sql = chrono::NaiveDate::parse_from_str(&options.sql_release, "%Y%m%d")?;
    let content = chrono::NaiveDate::parse_from_str(&options.content_release, "%Y-%m-%d")?;
    let history =
        chrono::NaiveDate::parse_from_str(&format!("{}-01", options.history_release), "%Y-%m-%d")?;
    if sql.format("%Y%m%d").to_string() != options.sql_release
        || content.format("%Y-%m-%d").to_string() != options.content_release
        || history.format("%Y-%m").to_string() != options.history_release
    {
        return Err("Identificatori di release non validi".into());
    }
    if !options.topic_budget_seconds.is_finite()
        || options.topic_budget_seconds <= 0.0
        || !options.metrics_interval.is_finite()
        || options.metrics_interval < 0.1
        || Duration::try_from_secs_f64(options.metrics_interval).is_err()
        || !(1..=10000).contains(&options.topic_batch_size)
        || !(1..=65536).contains(&options.history_cache_mib)
        || !(1..=10000000).contains(&options.history_commit_rows)
    {
        return Err("Controlli operativi non validi".into());
    }
    let configured = crate::configured_languages(&read_json(&options.languages_file)?)?;
    if options.selected.is_empty()
        || options.selected.iter().collect::<BTreeSet<_>>().len() != options.selected.len()
        || options
            .selected
            .iter()
            .any(|edition| !configured.contains(edition))
    {
        return Err("Selezionare solo edizioni configurate senza duplicati".into());
    }
    let execute = || {
        let mut runner = Runner::new(&options)?;
        let settings = runner.trace.clone();
        let result = runtime::with_tracing(settings, || {
            runtime::phase("pipeline", || collection_workflow(&options, &mut runner))
        });
        let report = runner.finish(&result);
        result.and(report)
    };
    if options.plan || options.no_phase_spans {
        runtime::with_tracing(None, execute)
    } else {
        execute()
    }
}

#[cfg(test)]
mod tests;
