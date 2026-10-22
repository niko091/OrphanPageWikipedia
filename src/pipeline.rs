//! Sequential native collection controller and verified reuse decisions.
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

fn read_json(path: &Path) -> PipelineResult<Value> {
    Ok(serde_json::from_reader(std::io::BufReader::new(
        File::open(path)?,
    ))?)
}

fn signature(path: &Path) -> Option<Value> {
    dumps::file_signature(path)
        .ok()
        .filter(|(size, _)| *size > 0)
        .map(|s| json!(s))
}

fn complete_json(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(size) = file.metadata().map(|s| s.len()) else {
        return false;
    };
    if size == 0
        || file
            .seek(SeekFrom::Start(size.saturating_sub(4096)))
            .is_err()
    {
        return false;
    }
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).is_ok()
        && tail
            .into_iter()
            .rev()
            .find(|c| !c.is_ascii_whitespace())
            .is_some_and(|c| c == b'}' || c == b']')
}

fn output_signatures(outputs: &[PathBuf]) -> Option<Value> {
    outputs
        .iter()
        .map(|path| Some((path.to_string_lossy().into_owned(), signature(path)?)))
        .collect::<Option<Map<_, _>>>()
        .map(Value::Object)
}

struct StepState {
    path: PathBuf,
    steps: Map<String, Value>,
}

impl StepState {
    fn open(path: PathBuf) -> PipelineResult<Self> {
        let steps = if path.exists() {
            let mut value = read_json(&path)?;
            if value["version"] != 1 || !value["steps"].is_object() {
                return Err("Stato della pipeline non valido".into());
            }
            value["steps"]
                .as_object_mut()
                .ok_or("Checkpoint mancanti")?
                .clone()
        } else {
            Map::new()
        };
        Ok(Self { path, steps })
    }

    fn reason(&self, key: &str, inputs: &Value, outputs: &[PathBuf]) -> &'static str {
        let Some(saved) = self.steps.get(key) else {
            return "missing_checkpoint";
        };
        if saved["inputs"] != *inputs {
            return "source_mismatch";
        }
        let Some(current) = output_signatures(outputs) else {
            return "missing_output";
        };
        if saved["outputs"] != current {
            "output_mismatch"
        } else {
            "verified_reuse"
        }
    }

    fn save(&mut self, key: &str, inputs: Value, outputs: &[PathBuf]) -> PipelineResult<()> {
        let outputs = output_signatures(outputs).ok_or("Output mancante per il checkpoint")?;
        self.steps
            .insert(key.into(), json!({"inputs":inputs,"outputs":outputs}));
        dumps::DumpClient::<dumps::WikimediaHttp>::write_json(
            &self.path,
            &json!({"version":1,"steps":self.steps}),
        )?;
        Ok(())
    }

    fn inherit(
        &self,
        key: &str,
        required: &Value,
        edition: &(String, String),
        outputs: &[PathBuf],
    ) -> bool {
        let Some(saved) = self.steps.get(key) else {
            return false;
        };
        let matches = required.as_object().is_some_and(|fields| {
            fields
                .iter()
                .all(|(key, value)| saved["inputs"][key] == *value)
        });
        matches
            && saved["inputs"]["languages"]
                .as_array()
                .is_some_and(|pairs| pairs.contains(&json!(edition)))
            && !outputs.is_empty()
            && outputs.iter().all(|path| {
                signature(path)
                    .is_some_and(|value| saved["outputs"][path.to_string_lossy().as_ref()] == value)
            })
    }
}

fn fetched_outputs(path: &Path, metadata: &Value) -> PipelineResult<Vec<PathBuf>> {
    let mut outputs = vec![path.to_owned()];
    for item in metadata["files"]
        .as_object()
        .ok_or("File del manifest mancanti")?
        .values()
    {
        outputs.push(
            item["path"]
                .as_str()
                .ok_or("Percorso del dump mancante")?
                .into(),
        );
    }
    Ok(outputs)
}

fn manifest_for(
    options: &PipelineOptions,
    language: &str,
    database: &str,
) -> Option<(PathBuf, Value)> {
    let path = options.dump_root.join(database).join(format!(
        "manifest-{}-{}.json",
        options.sql_release, options.history_release
    ));
    if !complete_json(&path) {
        return None;
    }
    let value = read_json(&path).ok()?;
    (value["language"] == language
        && value["database"] == database
        && value["sql_release"] == options.sql_release
        && value["history_release"] == options.history_release
        && value["files"].is_object())
    .then_some((path, value))
}

fn imported_reason(
    graph: &Path,
    history: &Path,
    stage: &Path,
    manifest_path: &Path,
    manifest: &Value,
    require_history: bool,
) -> &'static str {
    if !complete_json(graph)
        || (require_history && !complete_json(history))
        || signature(stage).is_none()
    {
        return "missing_output";
    }
    let verify = || -> PipelineResult<&'static str> {
        let graph = dumps::first_metadata(graph)?;
        let database = manifest["database"].as_str().ok_or("Database mancante")?;
        if graph["source"] != format!("Wikimedia SQL dumps / {database}")
            || graph["language"] != manifest["language"]
            || graph["sql_release"] != manifest["sql_release"]
            || graph["complete"] != true
            || graph["dump_manifest_sha256"] != dumps::file_sha256(manifest_path)?
        {
            return Ok("source_mismatch");
        }
        let connection = rusqlite::Connection::open_with_flags(
            stage,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let imported = connection
            .prepare("SELECT table_name,sha256 FROM imported")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        if dumps::SQL_TABLES.iter().any(|name| {
            manifest["files"][name]["sha256_local"]
                .as_str()
                .is_none_or(|digest| imported.get(*name).map(String::as_str) != Some(digest))
        }) {
            return Ok("stage_table_mismatch");
        }
        if !require_history {
            return Ok("verified_reuse");
        }
        let history = dumps::first_metadata(history)?;
        if history["source"] != format!("MediaWiki History dumps / {database}")
            || history["language"] != manifest["language"]
            || history["sql_release"] != manifest["sql_release"]
            || history["history_release"] != manifest["history_release"]
            || history["complete"] != true
        {
            return Ok("source_mismatch");
        }
        if history["creation_rule"] != dumps::HISTORY_RULE {
            return Ok("definition_mismatch");
        }
        let files = manifest["files"]
            .as_object()
            .ok_or("Partizioni History mancanti")?
            .iter()
            .filter(|(name, _)| name.ends_with(".tsv.bz2"))
            .map(|(name, item)| (name.clone(), item["sha256_local"].clone()))
            .collect::<Map<_, _>>();
        if history["history_files"] != Value::Object(files.clone()) {
            return Ok("source_mismatch");
        }
        let scanned = connection
            .prepare("SELECT file,phase,sha256 FROM history_imported")?
            .query_map([], |r| {
                Ok((
                    (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        for (name, digest) in files {
            let digest = digest.as_str().ok_or("Checksum History mancante")?;
            if scanned.get(&(name.clone(), "initial".into()))
                != Some(&format!("{digest}:{}", dumps::HISTORY_RULE))
            {
                return Ok("definition_mismatch");
            }
            if !scanned
                .get(&(name, "activity".into()))
                .is_some_and(|s| s.starts_with(&format!("{digest}:")))
            {
                return Ok("stage_table_mismatch");
            }
        }
        Ok("verified_reuse")
    };
    verify().unwrap_or("stage_table_mismatch")
}

fn topics_match(
    path: &Path,
    graph: &Path,
    graph_sha: &str,
    language: &str,
    model: &Value,
    census: u64,
) -> bool {
    let verify = || -> PipelineResult<bool> {
        if !complete_json(path) {
            return Ok(false);
        }
        let meta = dumps::first_metadata(path)?;
        let graph = dumps::first_metadata(graph)?;
        Ok(meta["language"] == language
            && meta["complete"] == true
            && meta["source"] == "local_model_on_verified_SQL_dump_outlinks"
            && meta["graph_finished_at"] == graph["finished_at"]
            && meta["input_file_sha256"] == graph_sha
            && meta["model"] == *model
            && meta["predicted_pages"]
                .as_u64()
                .zip(meta["missing_pages"].as_u64())
                .and_then(|(a, b)| a.checked_add(b))
                == Some(census))
    };
    verify().unwrap_or(false)
}

fn summary_matches(folder: &Path, entry: &Value, names: &[String]) -> bool {
    let verify = || -> PipelineResult<bool> {
        let source = folder.join(dumps::dataset_filename(&entry["file"])?);
        let metadata = dumps::first_metadata(&source)?;
        let summary = folder.join(dumps::dataset_filename(&entry["summary_file"])?);
        let creators = folder.join(dumps::dataset_filename(&entry["creator_index_file"])?);
        let count = read_json(&summary)?["census_page_count"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("Censimento del riepilogo mancante")?;
        let mut sources = Vec::new();
        for name in names {
            dumps::dataset_filename(&json!(name))?;
            let path = folder.join(name);
            let metadata = dumps::first_metadata(&path)?;
            let signature = dumps::file_signature(&path)?;
            sources.push(json!({"language":metadata["language"],"file":name,"size":signature.0,"mtime_ns":signature.1}));
        }
        Ok(dumps::prepared_summaries(
            &summary,
            &creators,
            &metadata,
            &json!(sources),
            count,
            false,
        )
        .is_some())
    };
    verify().unwrap_or(false)
}

fn approved_outputs(folder: &Path, entries: &[Value], files: &BTreeMap<String, String>) -> bool {
    let identities = entries
        .iter()
        .map(|e| Some((e["language"].as_str()?, e["file"].as_str()?)))
        .collect::<Option<BTreeSet<_>>>();
    if identities.is_none_or(|values| values.len() != entries.len()) {
        return false;
    }
    files.iter().all(|(language, file)| {
        let Some(entry) = entries
            .iter()
            .find(|e| e["language"] == *language && e["file"] == *file && e["demo"] != true)
        else {
            return false;
        };
        if !summary_matches(folder, entry, std::slice::from_ref(file)) {
            return false;
        }
        for key in [
            "file",
            "observations_file",
            "cards_file",
            "articles_file",
            "connections_metadata_file",
        ] {
            if matches!(key, "articles_file" | "connections_metadata_file")
                && entry.get(key).is_none()
            {
                continue;
            }
            let Ok(name) = dumps::dataset_filename(&entry[key]) else {
                return false;
            };
            if !complete_json(&folder.join(name)) {
                return false;
            }
        }
        for key in ["connections_dir", "cards_dir", "profile_topics_dir"] {
            if key != "connections_dir" && entry.get(key).is_none() {
                continue;
            }
            let Some(name) = entry[key].as_str().filter(|s| {
                !s.is_empty()
                    && Path::new(s).file_name().is_some_and(|p| p == *s)
                    && !s.starts_with('.')
            }) else {
                return false;
            };
            if !folder.join(name).is_dir() {
                return false;
            }
        }
        true
    })
}

fn collected_matches(
    meta: &Value,
    language: &str,
    database: &str,
    digest: &str,
    model: &Value,
    measures: Option<&Value>,
) -> bool {
    meta["language"] == language
        && meta["database"] == database
        && meta["source"] == format!("Wikimedia SQL dumps / {database}")
        && meta["complete"] == true
        && meta["demo"] != true
        && meta["dump_manifest_sha256"] == digest
        && meta["age_rule"] == dumps::HISTORY_RULE
        && meta["measures_rule"] == "creation_quality_creators_v3"
        && meta["topics"]["status"] == "predicted"
        && meta["topics"]["model"] == *model
        && measures.is_none_or(|sections| {
            sections.as_object().is_some_and(|sections| {
                sections.iter().all(|(section, expected)| {
                    expected.as_object().is_some_and(|fields| {
                        fields
                            .iter()
                            .all(|(key, value)| meta[section][key] == *value)
                    })
                })
            })
        })
}

struct PublicationInputs<'a> {
    selected: &'a [(String, String)],
    manifests: &'a BTreeMap<String, String>,
    model: &'a Value,
    measures: &'a BTreeMap<String, Value>,
    rules: &'a Value,
}

fn current_publication(
    folder: &Path,
    expected: &PublicationInputs<'_>,
    measures_required: bool,
) -> bool {
    let verify = || -> PipelineResult<bool> {
        let manifest = read_json(&folder.join("manifest.json"))?;
        let entries = manifest.as_array().ok_or("Manifest non valido")?;
        let languages = expected
            .selected
            .iter()
            .map(|(l, _)| l.clone())
            .collect::<BTreeSet<_>>();
        let mut files = BTreeMap::new();
        let mut metadata = BTreeMap::new();
        for (language, database) in expected.selected {
            let mut members = entries
                .iter()
                .filter(|e| {
                    e["language"] == *language && e["kind"] != "aggregate" && e["demo"] != true
                })
                .collect::<Vec<_>>();
            members.sort_by(|a, b| {
                (b["date"].as_str(), b["published_at"].as_str())
                    .cmp(&(a["date"].as_str(), a["published_at"].as_str()))
            });
            let Some(entry) = members.first() else {
                return Ok(false);
            };
            let filename = dumps::dataset_filename(&entry["file"])?;
            let meta = dumps::first_metadata(&folder.join(filename))?;
            let digest = expected
                .manifests
                .get(language)
                .ok_or("Identità del grafo mancante")?;
            if meta["complete"] != true
                || meta["source"] != format!("Wikimedia SQL dumps / {database}")
                || meta["dump_manifest_sha256"] != *digest
                || meta["topics"]["model"] != *expected.model
                || meta["topics"]["status"] != "predicted"
                || meta["category_rules"] != *expected.rules
            {
                return Ok(false);
            }
            if languages.len() > 1 {
                let references = meta["comparison"]["reference_languages"]
                    .as_array()
                    .ok_or("Riferimenti mancanti")?
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned))
                    .collect::<Option<BTreeSet<_>>>()
                    .ok_or("Lingue di riferimento non valide")?;
                if meta["comparison"]["status"] != "compared"
                    || references
                        != languages
                            .iter()
                            .filter(|l| *l != language)
                            .cloned()
                            .collect()
                {
                    return Ok(false);
                }
            }
            if measures_required
                && !collected_matches(
                    &meta,
                    language,
                    database,
                    digest,
                    expected.model,
                    expected.measures.get(language),
                )
            {
                return Ok(false);
            }
            files.insert(language.clone(), filename.into());
            metadata.insert(language.clone(), meta);
        }
        if !measures_required {
            return Ok(true);
        }
        if !approved_outputs(folder, entries, &files) {
            return Ok(false);
        }
        if languages.len() == 1 {
            return Ok(true);
        }
        for entry in entries
            .iter()
            .filter(|e| e["kind"] == "aggregate" && e["demo"] != true)
        {
            let name = dumps::dataset_filename(&entry["file"])?;
            let aggregate = read_json(&folder.join(name))?;
            let Some(members) = aggregate["members"]
                .as_array()
                .filter(|m| m.len() == languages.len())
            else {
                continue;
            };
            if aggregate["metadata"]["languages"] != json!(languages) {
                continue;
            }
            if members
                .iter()
                .map(|m| m["language"].as_str().map(str::to_owned))
                .collect::<Option<BTreeSet<_>>>()
                .as_ref()
                != Some(&languages)
            {
                continue;
            }
            if members.iter().all(|member| {
                member["language"].as_str().is_some_and(|l| {
                    files.get(l).is_some_and(|name| member["file"] == *name)
                        && metadata.get(l) == Some(&member["metadata"])
                        && expected
                            .selected
                            .iter()
                            .find(|(language, _)| language == l)
                            .is_some_and(|(_, database)| {
                                expected.manifests.get(l).is_some_and(|hash| {
                                    collected_matches(
                                        &member["metadata"],
                                        l,
                                        database,
                                        hash,
                                        expected.model,
                                        expected.measures.get(l),
                                    )
                                })
                            })
                })
            }) && summary_matches(folder, entry, &files.values().cloned().collect::<Vec<_>>())
            {
                return Ok(true);
            }
        }
        Ok(false)
    };
    verify().unwrap_or(false)
}

trait Steps {
    fn log(&mut self, text: &str) -> PipelineResult<()>;
    fn run(&mut self, name: &str, arguments: &[String]) -> PipelineResult<()>;
}

fn timestamp() -> String {
    let now: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
    now.to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

fn disk_usage(path: &Path) -> Value {
    #[cfg(unix)]
    {
        if let Some(existing) = path.ancestors().find(|p| p.exists()) {
            if let Ok(usage) = rustix::fs::statvfs(existing) {
                use std::os::unix::fs::MetadataExt;
                return json!({"path":path,"device":std::fs::metadata(existing).ok().map(|s|s.dev()),
                    "total_bytes":usage.f_blocks.saturating_mul(usage.f_frsize),"free_bytes":usage.f_bavail.saturating_mul(usage.f_frsize)});
            }
        }
    }
    json!({"path":path,"device":null,"total_bytes":null,"free_bytes":null})
}

fn host_inventory(disks: &BTreeMap<&str, PathBuf>) -> Value {
    #[cfg(target_os = "linux")]
    let affinity = rustix::thread::sched_getaffinity(None).ok().map(|set| {
        (0..rustix::thread::CpuSet::MAX_CPU)
            .filter(|i| set.is_set(*i))
            .collect::<Vec<_>>()
    });
    #[cfg(not(target_os = "linux"))]
    let affinity: Option<Vec<usize>> = None;
    json!({"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"cpu_count":std::thread::available_parallelism().ok().map(|n|n.get()),
        "cpu_affinity":affinity,"disks":disks.iter().map(|(key,path)|(*key,disk_usage(path))).collect::<BTreeMap<_,_>>(),
        "cpuinfo":std::fs::read_to_string("/proc/cpuinfo").ok(),"memory":std::fs::read_to_string("/proc/meminfo").ok(),
        "cgroup_cpu_max":std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),"cgroup_memory_max":std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()})
}

struct Reporting {
    log: File,
    events: File,
    active: Option<String>,
    peak_rss: Option<u64>,
    step_peak_rss: Option<u64>,
    last_cpu: Option<(Instant, f64)>,
}

#[derive(Clone)]
struct Reporter {
    shared: std::sync::Arc<std::sync::Mutex<Reporting>>,
    run_id: String,
    started: Instant,
    disks: BTreeMap<&'static str, PathBuf>,
    include_controller_memory: bool,
}

impl Reporter {
    fn bytes(&self, bytes: &[u8]) -> PipelineResult<()> {
        let mut state = self.shared.lock().map_err(|_| "Logger non disponibile")?;
        state.log.write_all(bytes)?;
        std::io::stdout().lock().write_all(bytes)?;
        Ok(())
    }

    fn log(&self, text: &str) -> PipelineResult<()> {
        self.bytes(format!("{} {text}\n", timestamp()).as_bytes())
    }

    fn event(&self, event: &str, fields: Value) -> PipelineResult<()> {
        let mut value = fields.as_object().cloned().unwrap_or_default();
        value.extend([
            (String::from("event"), json!(event)),
            ("run_id".into(), json!(self.run_id)),
            ("at".into(), json!(timestamp())),
            (
                "elapsed_seconds".into(),
                json!(self.started.elapsed().as_secs_f64()),
            ),
        ]);
        let mut state = self.shared.lock().map_err(|_| "Metriche non disponibili")?;
        serde_json::to_writer(&mut state.events, &value)?;
        state.events.write_all(b"\n")?;
        Ok(())
    }

    fn sample(&self) -> PipelineResult<()> {
        let active = self
            .shared
            .lock()
            .map_err(|_| "Metriche non disponibili")?
            .active
            .clone();
        let mut resources = runtime::resources(std::process::id(), self.include_controller_memory);
        let now = Instant::now();
        let step = {
            let mut state = self.shared.lock().map_err(|_| "Metriche non disponibili")?;
            let percent = state
                .last_cpu
                .zip(resources["tree_cpu_seconds"].as_f64())
                .and_then(|((at, before), after)| {
                    let elapsed = now.saturating_duration_since(at).as_secs_f64();
                    (elapsed > 0.0 && after >= before).then_some(100.0 * (after - before) / elapsed)
                });
            resources["sampled_cpu_percent_one_core"] = json!(percent);
            if let Some(cpu) = resources["tree_cpu_seconds"].as_f64() {
                state.last_cpu = Some((now, cpu));
            }
            let step = if state.active == active { active } else { None };
            if let Some(rss) = resources["tree_rss_bytes"].as_u64() {
                state.peak_rss = Some(state.peak_rss.unwrap_or(0).max(rss));
                if step.is_some() {
                    state.step_peak_rss = Some(state.step_peak_rss.unwrap_or(0).max(rss));
                }
            }
            step
        };
        resources["disk_free_bytes"] = json!(self
            .disks
            .iter()
            .map(|(key, path)| (*key, disk_usage(path)["free_bytes"].clone()))
            .collect::<BTreeMap<_, _>>());
        resources["step"] = json!(step);
        self.event("sample", resources.clone())?;
        let rss = resources["tree_rss_bytes"]
            .as_u64()
            .map(|n| format!("{:.1} MiB", n as f64 / 1048576.0))
            .unwrap_or_else(|| "non disponibile".into());
        let cpu = resources["sampled_cpu_percent_one_core"]
            .as_f64()
            .map(|n| format!("{n:.1}%"))
            .unwrap_or_else(|| "non disponibile".into());
        self.log(&format!(
            "RISORSE {}: RSS albero {rss}; CPU {cpu} (100%=1 core)",
            step.as_deref().unwrap_or("orchestrazione")
        ))
    }
}

struct Monitor {
    stop: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl Monitor {
    fn start(reporter: Reporter, interval: f64) -> Self {
        let (stop, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                receiver.recv_timeout(Duration::from_secs_f64(interval))
            {
                reporter.sample().map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn finish(&mut self) -> Option<String> {
        let _ = self.stop.send(());
        self.thread.take().and_then(|thread| match thread.join() {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some("Monitor risorse interrotto".into()),
        })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.finish();
    }
}

struct RunningChild {
    child: std::process::Child,
    output: Option<mpsc::Receiver<std::io::Result<Vec<u8>>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl RunningChild {
    fn start(mut command: Command) -> PipelineResult<Self> {
        let (mut input, output) = std::io::pipe()?;
        command
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(output));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.stdin(Stdio::null()).spawn()?;
        let (sender, receiver) = mpsc::sync_channel(8);
        let reader = std::thread::spawn(move || {
            let mut buffer = vec![0; 65536];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(size) => {
                        if sender.send(Ok(buffer[..size].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            output: Some(receiver),
            reader: Some(reader),
        })
    }

    fn terminate(&mut self, force: bool) {
        #[cfg(unix)]
        {
            if let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32) {
                let _ = rustix::process::kill_process_group(
                    pid,
                    if force {
                        rustix::process::Signal::KILL
                    } else {
                        rustix::process::Signal::TERM
                    },
                );
            }
        }
        #[cfg(not(unix))]
        {
            let _ = force;
            let _ = self.child.kill();
        }
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        self.terminate(true);
        let _ = self.child.wait();
        self.output.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct RecordingOptions {
    root: PathBuf,
    prefix: PathBuf,
    interval: f64,
    phase_spans: bool,
    include_controller_memory: bool,
    disks: BTreeMap<&'static str, PathBuf>,
}

struct Runner {
    executable: PathBuf,
    root: PathBuf,
    reporter: Option<Reporter>,
    monitor: Option<Monitor>,
    report_path: Option<PathBuf>,
    trace: Option<runtime::TraceSettings>,
    metadata: Value,
    steps: Vec<Value>,
}

impl Runner {
    fn new(options: &PipelineOptions) -> PipelineResult<Self> {
        let executable = std::env::current_exe()?;
        if options.plan {
            return Ok(Self {
                executable,
                root: options.root.clone(),
                reporter: None,
                monitor: None,
                report_path: None,
                trace: None,
                metadata: Value::Null,
                steps: Vec::new(),
            });
        }
        std::fs::create_dir_all(&options.log_dir)?;
        let run_id = runtime::unique_id();
        let prefix = format!(
            "dumps-{}-{}",
            timestamp().replace(['-', ':', '.'], ""),
            &run_id[..12]
        );
        let prefix = options.log_dir.join(prefix);
        let disks = BTreeMap::from([
            ("dump_root", options.dump_root.clone()),
            ("import_dir", options.import_dir.clone()),
            ("output_dir", options.output_dir.clone()),
            ("work_dir", options.work_dir.clone()),
        ]);
        let configs = [
            &options.languages_file,
            &options.model_config,
            &options.root.join("config/quality.json"),
        ]
        .into_iter()
        .map(|path| {
            Ok((
                path.to_string_lossy().into_owned(),
                json!(dumps::file_sha256(path)?),
            ))
        })
        .collect::<PipelineResult<Map<_, _>>>()?;
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&options.root)
                .output()
                .ok()
                .filter(|r| r.status.success())
                .and_then(|r| String::from_utf8(r.stdout).ok())
                .map(|s| s.trim().to_owned())
        };
        let metadata = json!({"format":"pipeline_benchmark_v1","run_id":run_id,
            "arguments":{"languages":options.selected,"languages_file":options.languages_file,"model":options.model,"model_config":options.model_config,"log_dir":options.log_dir,
                "repo_root":options.root,"sql_release":options.sql_release,"history_release":options.history_release,"content_release":options.content_release,
                "dump_root":options.dump_root,"import_dir":options.import_dir,"output_dir":options.output_dir,"work_dir":options.work_dir,
                "topic_batch_size":options.topic_batch_size,"topic_budget_seconds":options.topic_budget_seconds,"history_cache_mib":options.history_cache_mib,
                "history_commit_rows":options.history_commit_rows,"metrics_interval":options.metrics_interval,"no_phase_spans":options.no_phase_spans,"force":options.force},
            "host":host_inventory(&disks),
            "code":{"git_revision":git(&["rev-parse","HEAD"]),"binary_sha256":dumps::file_sha256(&executable)?},"config_sha256":configs,
            "measurement":{"interval_seconds":options.metrics_interval,"spans_file":(!options.no_phase_spans).then(||prefix.with_extension("benchmark.spans.jsonl").file_name().map(|s|s.to_string_lossy().into_owned())).flatten(),
                "spans":"inclusive monotonic intervals; subtract child interval unions; abrupt ends are detection bounds",
                "rss":"sampled sum of controller and live descendants; shared pages may repeat; not PSS",
                "cpu":"getrusage children deltas per completed step; live /proc samples are estimates; 100 percent = one core",
                "io":"controller plus reaped descendants; physical storage bytes, excludes network and cache hits"}});
        Self::record(
            RecordingOptions {
                root: options.root.clone(),
                prefix,
                interval: options.metrics_interval,
                phase_spans: !options.no_phase_spans,
                include_controller_memory: true,
                disks,
            },
            metadata,
        )
    }

    fn record(options: RecordingOptions, mut metadata: Value) -> PipelineResult<Self> {
        let executable = std::env::current_exe()?;
        let run_id = runtime::unique_id();
        metadata["run_id"] = json!(run_id);
        let log_path = options.prefix.with_extension("log");
        let event_path = options.prefix.with_extension("benchmark.metrics.jsonl");
        let report_path = options.prefix.with_extension("benchmark.json");
        let create = |path: &Path| OpenOptions::new().write(true).create_new(true).open(path);
        let reporter = Reporter {
            shared: std::sync::Arc::new(std::sync::Mutex::new(Reporting {
                log: create(&log_path)?,
                events: create(&event_path)?,
                active: None,
                peak_rss: None,
                step_peak_rss: None,
                last_cpu: None,
            })),
            run_id: run_id.clone(),
            started: Instant::now(),
            disks: options.disks,
            include_controller_memory: options.include_controller_memory,
        };
        let trace = if options.phase_spans {
            let file = options.prefix.with_extension("benchmark.spans.jsonl");
            create(&file)?;
            Some(runtime::TraceSettings {
                file,
                run_id,
                parent_id: None,
            })
        } else {
            None
        };
        reporter.log(&format!("LOG {}", log_path.display()))?;
        reporter.event("run_start", metadata.clone())?;
        reporter.sample()?;
        let monitor = Monitor::start(reporter.clone(), options.interval);
        Ok(Self {
            executable,
            root: options.root,
            reporter: Some(reporter),
            monitor: Some(monitor),
            report_path: Some(report_path),
            trace,
            metadata,
            steps: Vec::new(),
        })
    }

    fn finish(&mut self, result: &PipelineResult<()>) -> PipelineResult<()> {
        let monitor_error = self.monitor.as_mut().and_then(Monitor::finish);
        let Some(reporter) = &self.reporter else {
            return Ok(());
        };
        reporter.sample()?;
        let resources = runtime::resources(std::process::id(), reporter.include_controller_memory);
        let status = if result.is_ok() {
            "completed"
        } else if runtime::received_signal().is_some() {
            "interrupted"
        } else {
            "failed"
        };
        let peak = reporter
            .shared
            .lock()
            .map_err(|_| "Metriche non disponibili")?
            .peak_rss;
        let mut report = self.metadata.clone();
        for (key, value) in [
            ("status", json!(status)),
            ("steps", json!(self.steps)),
            (
                "wall_seconds",
                json!(reporter.started.elapsed().as_secs_f64()),
            ),
            ("sampled_peak_tree_rss_bytes", json!(peak)),
            ("resources_final", resources),
            ("monitor_error", json!(monitor_error)),
            ("error", json!(result.as_ref().err().map(|e| e.to_string()))),
        ] {
            report[key] = value;
        }
        reporter.event("run_end", report.clone())?;
        let path = self
            .report_path
            .as_ref()
            .ok_or("Percorso del report mancante")?;
        write_operational_report(path, &report)
    }
}

fn write_operational_report(path: &Path, report: &Value) -> PipelineResult<()> {
    // Diagnostics must also survive handled cancellation.
    let temporary = path.with_extension(format!("{}.tmp", runtime::unique_id()));
    let written = (|| -> PipelineResult<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, report)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    written
}

fn numeric_difference(after: &Value, before: &Value, key: &str) -> Value {
    if let Some((after, before)) = after[key].as_u64().zip(before[key].as_u64()) {
        return json!(after.saturating_sub(before));
    }
    after[key]
        .as_f64()
        .zip(before[key].as_f64())
        .map(|(a, b)| json!((a - b).max(0.0)))
        .unwrap_or(Value::Null)
}

impl Steps for Runner {
    fn log(&mut self, text: &str) -> PipelineResult<()> {
        if let Some(reporter) = &self.reporter {
            reporter.log(text)
        } else {
            println!("{text}");
            Ok(())
        }
    }

    fn run(&mut self, name: &str, arguments: &[String]) -> PipelineResult<()> {
        let mut command = Command::new(&self.executable);
        command
            .arg("--repo-root")
            .arg(&self.root)
            .args(arguments)
            .current_dir(&self.root);
        self.execute_command(name, arguments, command)
    }
}

impl Runner {
    fn execute_command(
        &mut self,
        name: &str,
        arguments: &[String],
        mut command: Command,
    ) -> PipelineResult<()> {
        runtime::check_cancelled()?;
        runtime::phase("pipeline.step", || {
            runtime::phase_tag("step", json!(name));
            self.log(&format!(
                "INIZIO {name}: {}",
                serde_json::to_string(arguments)?
            ))?;
            if let Some(reporter) = &self.reporter {
                let mut shared = reporter
                    .shared
                    .lock()
                    .map_err(|_| "Metriche non disponibili")?;
                shared.active = Some(name.into());
                shared.step_peak_rss = None;
            }
            if let Some(reporter) = &self.reporter {
                reporter.event("step_start", json!({"name":name,"command":arguments}))?;
                reporter.sample()?;
            }
            let before = runtime::resources(std::process::id(), true);
            let usage_before = runtime::child_usage();
            let started = Instant::now();
            let offset = self
                .trace
                .as_ref()
                .and_then(|s| std::fs::metadata(&s.file).ok())
                .map_or(0, |s| s.len());
            let result = (|| -> PipelineResult<i32> {
                runtime::configure_child_tracing(&mut command);
                let mut child = RunningChild::start(command)?;
                let mut exited = None;
                let mut interrupted = None;
                loop {
                    if runtime::received_signal().is_some() && interrupted.is_none() {
                        child.terminate(false);
                        interrupted = Some(Instant::now());
                    }
                    if interrupted.is_some_and(|at| at.elapsed() >= Duration::from_secs(10)) {
                        child.terminate(true);
                    }
                    let output = child
                        .output
                        .as_ref()
                        .ok_or("Pipe del processo mancante")?
                        .recv_timeout(Duration::from_millis(50));
                    match output {
                        Ok(Ok(bytes)) => {
                            if let Some(reporter) = &self.reporter {
                                reporter.bytes(&bytes)?;
                            } else {
                                std::io::stdout().lock().write_all(&bytes)?;
                            }
                        }
                        Ok(Err(error)) => return Err(error.into()),
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            if let Some(status) = child.child.try_wait()? {
                                exited = Some(status);
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if exited.is_none() {
                        if let Some(status) = child.child.try_wait()? {
                            exited = Some(status);
                            child.terminate(true);
                        }
                    }
                }
                let status = exited.ok_or("Esito del processo mancante")?;
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    Ok(status
                        .code()
                        .unwrap_or_else(|| -status.signal().unwrap_or(1)))
                }
                #[cfg(not(unix))]
                {
                    Ok(status.code().unwrap_or(-1))
                }
            })();
            let returncode = result.as_ref().copied().unwrap_or(-1);
            if let Some(settings) = &self.trace {
                let _ = runtime::reconcile_spans(&settings.file, offset, returncode);
            }
            let after = runtime::resources(std::process::id(), true);
            let usage_after = runtime::child_usage();
            if let Some(reporter) = &self.reporter {
                reporter.sample()?;
                let peak = {
                    let mut shared = reporter
                        .shared
                        .lock()
                        .map_err(|_| "Metriche non disponibili")?;
                    shared.active = None;
                    shared.step_peak_rss.take()
                };
                let wall = started.elapsed().as_secs_f64();
                let user = numeric_difference(&usage_after, &usage_before, "user_cpu_seconds");
                let system = numeric_difference(&usage_after, &usage_before, "system_cpu_seconds");
                let percent = user
                    .as_f64()
                    .zip(system.as_f64())
                    .filter(|_| wall > 0.0)
                    .map(|(user, system)| 100.0 * (user + system) / wall);
                let environment = runtime::child_environment();
                let step = json!({"name":name,"command":arguments,"returncode":returncode,"wall_seconds":wall,
                    "span_id":environment.get("ORPHANWIKI_PARENT_SPAN"),"sampled_peak_tree_rss_bytes":peak,
                    "user_cpu_seconds":user,"system_cpu_seconds":system,"cpu_percent_one_core":percent,
                    "filesystem_input_operations":numeric_difference(&usage_after,&usage_before,"filesystem_input_operations"),
                    "filesystem_output_operations":numeric_difference(&usage_after,&usage_before,"filesystem_output_operations"),
                    "read_bytes":numeric_difference(&after,&before,"controller_reaped_read_bytes"),"write_bytes":numeric_difference(&after,&before,"controller_reaped_write_bytes")});
                reporter.event("step_end", step.clone())?;
                self.steps.push(step);
            }
            let returncode = result?;
            runtime::check_cancelled()?;
            if returncode != 0 {
                self.log(&format!("ERRORE {name}: codice di uscita {returncode}"))?;
                return Err(format!("{name}: codice di uscita {returncode}").into());
            }
            self.log(&format!("COMPLETATO {name}"))?;
            Ok(())
        })
    }
}

/// Paired commands use independent copies of frozen inputs; no controller RSS.
pub struct BenchmarkOptions {
    pub root: PathBuf,
    pub spec: PathBuf,
    pub output_dir: PathBuf,
    pub repetitions: u32,
    pub warmups: u32,
    pub metrics_interval: f64,
    pub min_speedup: f64,
    pub max_rss_ratio: f64,
}

#[derive(serde::Deserialize)]
struct BenchmarkSpec {
    version: u32,
    inputs: Vec<String>,
    #[serde(default)]
    copies: Vec<BenchmarkCopy>,
    outputs: Vec<BenchmarkOutput>,
    baseline: BenchmarkVariant,
    candidate: BenchmarkVariant,
}

#[derive(serde::Deserialize)]
struct BenchmarkCopy {
    source: String,
    destination: String,
}

#[derive(serde::Deserialize)]
struct BenchmarkVariant {
    #[serde(default = "default_directory")]
    cwd: String,
    commands: Vec<Vec<String>>,
}

fn default_directory() -> String {
    ".".into()
}

#[derive(serde::Deserialize)]
struct BenchmarkOutput {
    path: String,
    kind: OutputKind,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutputKind {
    Canonical,
    Manifest,
    Bytes,
}

fn relative_path(value: &str) -> PipelineResult<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || !path
            .components()
            .any(|c| matches!(c, std::path::Component::Normal(_)))
        || path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err("Percorso benchmark relativo non valido".into());
    }
    Ok(path)
}

fn local_output(folder: &Path, name: &str) -> PipelineResult<PathBuf> {
    let path = folder.join(relative_path(name)?).canonicalize()?;
    if !path.starts_with(folder.canonicalize()?) {
        return Err("Output benchmark esterno alla prova".into());
    }
    Ok(path)
}

#[derive(Default)]
struct BenchmarkDigest(sha2::Sha256);

impl Write for BenchmarkDigest {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn canonical_fingerprint(path: &Path, language: Option<&str>) -> PipelineResult<Value> {
    use sha2::Digest;
    let mut digest = BenchmarkDigest::default();
    let mut pages = 0u64;
    let mut targets = 0u64;
    let mut candidates = 0u64;
    let mut reference_edges = 0u64;
    dumps::visit_ordered_json(
        std::io::BufReader::new(File::open(path)?),
        true,
        false,
        &mut |section, identifier, mut value| {
            match section {
                "metadata" => {
                    if value["complete"] != true || !value["language"].is_string() {
                        return Err("Il benchmark richiede un censimento completo".into());
                    }
                    if language.is_some_and(|l| value["language"] != l) {
                        return Err("Lingua del manifest benchmark non corrispondente".into());
                    }
                    let meta = value.as_object_mut().ok_or("Metadati non validi")?;
                    meta.remove("engine");
                    meta.remove("dump_manifest");
                    if matches!(
                        meta.get("source").and_then(Value::as_str),
                        Some(
                            "local_model_on_synthetic_outlinks"
                                | "local_model_on_verified_SQL_dump_outlinks"
                        )
                    ) {
                        for key in ["started_at", "finished_at", "timing"] {
                            meta.remove(key);
                        }
                    }
                    if let Some(topics) = meta.get_mut("topics").and_then(Value::as_object_mut) {
                        for key in ["started_at", "finished_at", "timing"] {
                            topics.remove(key);
                        }
                    }
                }
                "pages" => pages += 1,
                "link_candidates" => {
                    targets += 1;
                    if let Some(rows) = value.get("candidates") {
                        let rows = rows.as_array().ok_or("Candidate non valide")?;
                        candidates += rows.len() as u64;
                        for row in rows {
                            if let Some(edges) = row.get("evidence") {
                                reference_edges +=
                                    edges.as_array().ok_or("Evidenze non valide")?.len() as u64;
                            }
                        }
                    }
                }
                _ => return Err("Sezione benchmark non valida".into()),
            }
            dumps::write_canonical_json(&json!([section, identifier, value]), &mut digest)?;
            digest.write_all(b"\n")?;
            Ok(())
        },
    )?;
    if pages == 0 {
        return Err("Output benchmark senza voci".into());
    }
    Ok(
        json!({"sha256":format!("{:x}",digest.0.finalize()), "pages":pages,"targets":targets,"candidates":candidates,"reference_edges":reference_edges}),
    )
}

fn benchmark_outputs(folder: &Path, outputs: &[BenchmarkOutput]) -> PipelineResult<Value> {
    let mut result = Map::new();
    for output in outputs {
        let path = local_output(folder, &output.path)?;
        match output.kind {
            OutputKind::Canonical => {
                result.insert(output.path.clone(), canonical_fingerprint(&path, None)?);
            }
            OutputKind::Bytes => {
                result.insert(
                    output.path.clone(),
                    json!({"sha256":dumps::file_sha256(&path)?,"bytes":path.metadata()?.len()}),
                );
            }
            OutputKind::Manifest => {
                let entries = read_json(&path)?;
                let entries = entries.as_array().ok_or("Manifest benchmark non valido")?;
                let mut languages = BTreeSet::new();
                for entry in entries {
                    if !entry.is_object() {
                        return Err("Manifest benchmark non valido".into());
                    }
                    if entry.get("kind").and_then(Value::as_str).unwrap_or("wiki") != "wiki" {
                        continue;
                    }
                    let language = entry["language"]
                        .as_str()
                        .filter(|l| !l.is_empty())
                        .ok_or("Lingua benchmark mancante")?;
                    if !languages.insert(language) {
                        return Err("Serve esattamente un export per lingua nel benchmark".into());
                    }
                    let filename = entry["file"]
                        .as_str()
                        .ok_or("Nome export benchmark non valido")?;
                    if relative_path(filename)?.components().count() != 1 {
                        return Err("Nome export benchmark non valido".into());
                    }
                    let parent = Path::new(&output.path)
                        .parent()
                        .ok_or("Percorso manifest non valido")?;
                    let member = local_output(folder, &parent.join(filename).to_string_lossy())?;
                    result.insert(
                        format!("{}:{language}", output.path),
                        canonical_fingerprint(&member, Some(language))?,
                    );
                }
                if languages.is_empty() {
                    return Err("Manifest benchmark senza membri".into());
                }
            }
        }
    }
    Ok(Value::Object(result))
}

fn distribution(values: &[Option<f64>]) -> Value {
    let Some(mut values) = values.iter().copied().collect::<Option<Vec<_>>>() else {
        return Value::Null;
    };
    if values.is_empty() || values.iter().any(|n| !n.is_finite()) {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let median = if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    };
    json!({"median":median,"min":values[0],"max":values[values.len()-1]})
}

fn benchmark_summary(
    samples: &[Value],
    min_speedup: f64,
    max_rss_ratio: f64,
) -> PipelineResult<Value> {
    let mut pairs = BTreeMap::<u64, BTreeMap<&str, &Value>>::new();
    for sample in samples {
        if sample["warmup"] == true {
            continue;
        }
        let pair = sample["pair"]
            .as_u64()
            .ok_or("Coppia benchmark non valida")?;
        let variant = sample["variant"]
            .as_str()
            .ok_or("Variante benchmark mancante")?;
        if !matches!(variant, "baseline" | "candidate")
            || pairs
                .entry(pair)
                .or_default()
                .insert(variant, sample)
                .is_some()
        {
            return Err("Prova benchmark duplicata o non valida".into());
        }
    }
    if pairs.len() < 3 || pairs.values().any(|p| p.len() != 2) {
        return Err("Servono almeno tre coppie complete di misure".into());
    }
    let mut metrics = Map::new();
    for metric in ["wall_seconds", "cpu_seconds", "sampled_peak_tree_rss_bytes"] {
        let before = pairs
            .values()
            .map(|p| p["baseline"][metric].as_f64())
            .collect::<Vec<_>>();
        let after = pairs
            .values()
            .map(|p| p["candidate"][metric].as_f64())
            .collect::<Vec<_>>();
        if metric == "wall_seconds"
            && before
                .iter()
                .chain(&after)
                .any(|v| v.is_none_or(|n| !n.is_finite() || n <= 0.0))
        {
            return Err("Durata benchmark non valida".into());
        }
        let ratios = before
            .iter()
            .zip(&after)
            .map(|(b, a)| b.zip(*a).filter(|(b, _)| *b > 0.0).map(|(b, a)| a / b))
            .collect::<Vec<_>>();
        metrics.insert(metric.into(),json!({"baseline":distribution(&before),"candidate":distribution(&after),"candidate_over_baseline":distribution(&ratios)}));
    }
    let speedup = distribution(
        &pairs
            .values()
            .map(|p| {
                p["baseline"]["wall_seconds"]
                    .as_f64()
                    .zip(p["candidate"]["wall_seconds"].as_f64())
                    .map(|(b, a)| b / a)
            })
            .collect::<Vec<_>>(),
    );
    let memory = &metrics["sampled_peak_tree_rss_bytes"]["candidate_over_baseline"];
    let status = if speedup["median"].as_f64().is_some_and(|n| n < min_speedup)
        || memory["max"].as_f64().is_some_and(|n| n > max_rss_ratio)
    {
        "no_improvement"
    } else if memory.is_null() || speedup["min"].as_f64().is_none_or(|n| n <= 1.0) {
        "inconclusive"
    } else {
        "passed"
    };
    Ok(
        json!({"status":status,"metrics":metrics,"paired_speedup":speedup,
        "thresholds":{"minimum_median_speedup":min_speedup,"maximum_paired_rss_ratio":max_rss_ratio,"every_pair_must_be_faster":true},
        "interpretation":"observed paired repetitions, not a statistical confidence interval"}),
    )
}

fn git_output(cwd: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|r| r.status.success())
        .map(|r| r.stdout)
}

fn benchmark_executable(value: &str, cwd: &Path) -> PipelineResult<PathBuf> {
    // Keep a virtual environment's interpreter path instead of resolving its symlink.
    if value.contains(std::path::MAIN_SEPARATOR) {
        let path = cwd.join(value);
        if path.is_file() {
            return Ok(path);
        }
    } else if let Some(paths) = std::env::var_os("PATH") {
        if let Some(path) = std::env::split_paths(&paths)
            .map(|p| p.join(value))
            .find(|p| p.is_file())
        {
            return Ok(path);
        }
    }
    Err("Eseguibile benchmark mancante".into())
}

fn verify_frozen(frozen: &BTreeMap<PathBuf, String>) -> PipelineResult<()> {
    for (path, checksum) in frozen {
        runtime::check_cancelled()?;
        if dumps::file_sha256(path)? != *checksum {
            return Err(format!("Input benchmark modificato: {}", path.display()).into());
        }
    }
    Ok(())
}

fn optional_sum(steps: &[Value], keys: &[&str]) -> Option<f64> {
    steps.iter().try_fold(0.0, |sum, step| {
        keys.iter()
            .try_fold(sum, |sum, key| step[*key].as_f64().map(|v| sum + v))
    })
}

/// Return 0 for accepted measurements and 2 for an unproven improvement.
/// Execution, input mutation and parity failures return an error and retain diagnostics.
pub fn benchmark(options: BenchmarkOptions) -> PipelineResult<u8> {
    use sha2::Digest;
    if options.repetitions < 3
        || options.warmups.checked_add(options.repetitions).is_none()
        || [
            options.metrics_interval,
            options.min_speedup,
            options.max_rss_ratio,
        ]
        .iter()
        .any(|v| !v.is_finite())
        || options.metrics_interval < 0.1
        || options.metrics_interval > 86400.0
        || options.min_speedup < 1.0
        || options.max_rss_ratio < 1.0
    {
        return Err("Controlli benchmark non validi".into());
    }
    let expand = |s: &str| s.replace("{repo}", &options.root.to_string_lossy());
    let spec_path = options.root.join(&options.spec);
    let spec_value = read_json(&spec_path)?;
    let spec: BenchmarkSpec = serde_json::from_value(spec_value.clone())?;
    if spec.version != 1 || spec.inputs.is_empty() || spec.outputs.is_empty() {
        return Err("Specifica benchmark incompleta".into());
    }
    let mut output_names = BTreeSet::new();
    for output in &spec.outputs {
        relative_path(&output.path)?;
        if !output_names.insert(&output.path) {
            return Err("Output benchmark duplicati".into());
        }
    }
    let mut fingerprints = BTreeMap::new();
    for input in &spec.inputs {
        let path = options.root.join(expand(input)).canonicalize()?;
        let sha = dumps::file_sha256(&path)?;
        if fingerprints.insert(path, sha).is_some() {
            return Err("Input benchmark duplicati".into());
        }
    }
    let mut copies = Vec::new();
    let mut destinations = BTreeSet::new();
    for copy in &spec.copies {
        let source = options.root.join(expand(&copy.source)).canonicalize()?;
        let destination = relative_path(&copy.destination)?.to_path_buf();
        if !fingerprints.contains_key(&source) || !destinations.insert(destination.clone()) {
            return Err("Copie benchmark duplicate o non verificate".into());
        }
        copies.push((source, destination));
    }
    let mut frozen = fingerprints.clone();
    let mut variants = Map::new();
    let mut commands = BTreeMap::new();
    for (name, variant) in [("baseline", &spec.baseline), ("candidate", &spec.candidate)] {
        if variant.commands.is_empty()
            || variant
                .commands
                .iter()
                .any(|c| c.is_empty() || c.iter().any(|t| t.is_empty()))
        {
            return Err("I comandi devono essere liste di argomenti, senza shell".into());
        }
        let cwd = options.root.join(expand(&variant.cwd)).canonicalize()?;
        if !cwd.is_dir() {
            return Err("Directory della variante mancante".into());
        }
        let mut executables = BTreeMap::new();
        let mut resolved = Vec::new();
        for command in &variant.commands {
            let path = benchmark_executable(&expand(&command[0]), &cwd)?;
            let checksum = dumps::file_sha256(&path)?;
            frozen.insert(path.clone(), checksum.clone());
            executables.insert(path.to_string_lossy().into_owned(), checksum);
            let mut command = command.clone();
            command[0] = path.to_string_lossy().into_owned();
            resolved.push(command);
        }
        variants.insert(name.into(),json!({"cwd":cwd,"commands":variant.commands,
            "git_revision":git_output(&cwd,&["rev-parse","HEAD"]).and_then(|v|String::from_utf8(v).ok()).map(|s|s.trim().to_owned()),
            "tracked_diff_sha256":git_output(&cwd,&["diff","HEAD"]).map(|v|format!("{:x}",sha2::Sha256::digest(v))),"executables_sha256":executables}));
        commands.insert(name, (cwd, resolved));
    }
    let output = options.root.join(&options.output_dir);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir(&output)?;
    let mut report = json!({"format":"migration_benchmark_v1","status":"failed","samples":[],
        "arguments":{"spec":spec_path,"output_dir":output,"repetitions":options.repetitions,"warmups":options.warmups,
            "metrics_interval":options.metrics_interval,"min_speedup":options.min_speedup,"max_rss_ratio":options.max_rss_ratio},
        "cache_policy":"input verification and copies before timing; OS cache not flushed",
        "timing_scope":"sum of sequential commands, including startup; excludes copies, hashes and output validation",
        "spec":spec_value,"spec_sha256":dumps::file_sha256(&spec_path)?,"inputs_sha256":fingerprints,"variants":variants,
        "host":host_inventory(&BTreeMap::from([("output_dir",output.clone())]))});
    let mut samples = Vec::new();
    let result = runtime::with_tracing(None, || -> PipelineResult<u8> {
        let mut reference = None;
        for pair in 0..options.warmups + options.repetitions {
            let order = if pair % 2 == 0 {
                ["baseline", "candidate"]
            } else {
                ["candidate", "baseline"]
            };
            for variant in order {
                verify_frozen(&frozen)?;
                let run = output.join(format!("{pair:02}-{variant}"));
                let work = run.join("work");
                std::fs::create_dir_all(&work)?;
                for (source, destination) in &copies {
                    let destination = work.join(destination);
                    if let Some(parent) = destination.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::copy(source, &destination)?;
                    let metadata = source.metadata()?;
                    let mut times = std::fs::FileTimes::new().set_modified(metadata.modified()?);
                    if let Ok(accessed) = metadata.accessed() {
                        times = times.set_accessed(accessed);
                    }
                    File::options()
                        .write(true)
                        .open(destination)?
                        .set_times(times)?;
                }
                let mut runner = Runner::record(
                    RecordingOptions {
                        root: options.root.clone(),
                        prefix: run.join("run"),
                        interval: options.metrics_interval,
                        phase_spans: false,
                        include_controller_memory: false,
                        disks: BTreeMap::from([("work_dir", work.clone())]),
                    },
                    json!({"format":"pipeline_benchmark_v1","variant":variant,"pair":pair,
                    "measurement":{"interval_seconds":options.metrics_interval,"rss":"sampled sum of live descendants only; benchmark controller excluded"}}),
                )?;
                let (cwd, variant_commands) = &commands[variant];
                let executed = (|| -> PipelineResult<()> {
                    for (number, command) in variant_commands.iter().enumerate() {
                        let args = command
                            .iter()
                            .map(|s| expand(s).replace("{run}", &work.to_string_lossy()))
                            .collect::<Vec<_>>();
                        let mut command = Command::new(&args[0]);
                        command.args(&args[1..]).current_dir(cwd);
                        runner.execute_command(&format!("command-{number}"), &args, command)?;
                    }
                    Ok(())
                })();
                runner.finish(&executed)?;
                executed?;
                let measured = read_json(
                    runner
                        .report_path
                        .as_ref()
                        .ok_or("Report benchmark mancante")?,
                )?;
                if !measured["monitor_error"].is_null() {
                    return Err("Monitoraggio benchmark fallito".into());
                }
                let peak = runner
                    .steps
                    .iter()
                    .map(|s| s["sampled_peak_tree_rss_bytes"].as_u64())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|v| v.into_iter().max());
                let sample = json!({"pair":pair,"variant":variant,"warmup":pair<options.warmups,
                    "wall_seconds":optional_sum(&runner.steps,&["wall_seconds"]),
                    "cpu_seconds":optional_sum(&runner.steps,&["user_cpu_seconds","system_cpu_seconds"]),
                    "sampled_peak_tree_rss_bytes":peak,"report":format!("{pair:02}-{variant}/run.benchmark.json")});
                samples.push(sample);
                verify_frozen(&frozen)?;
                let outputs = benchmark_outputs(&work, &spec.outputs)?;
                samples.last_mut().ok_or("Prova benchmark mancante")?["outputs"] = outputs.clone();
                if let Some(before) = &reference {
                    if before != &outputs {
                        return Err(format!(
                            "Output differenti nella prova {pair}, variante {variant}"
                        )
                        .into());
                    }
                } else {
                    reference = Some(outputs);
                }
            }
        }
        let summary = benchmark_summary(&samples, options.min_speedup, options.max_rss_ratio)?;
        let passed = summary["status"] == "passed";
        report
            .as_object_mut()
            .ok_or("Report non valido")?
            .extend(summary.as_object().ok_or("Riepilogo non valido")?.clone());
        report["correctness"] = json!("matched");
        Ok(if passed { 0 } else { 2 })
    });
    report["samples"] = json!(samples);
    if let Err(error) = &result {
        report["error"] = json!(error.to_string());
        report["status"] = json!(if runtime::received_signal().is_some() {
            "interrupted"
        } else {
            "failed"
        });
    }
    write_operational_report(&output.join("comparison.json"), &report)?;
    println!(
        "Benchmark {}: {}",
        report["status"].as_str().unwrap_or("failed"),
        output.join("comparison.json").display()
    );
    result
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn arguments(name: &str, pairs: &[(&str, String)]) -> Vec<String> {
    let mut args = vec![name.into()];
    append_options(&mut args, pairs);
    args
}

fn append_options(args: &mut Vec<String>, pairs: &[(&str, String)]) {
    for (name, value) in pairs {
        args.push(format!("--{name}"));
        args.push(value.clone());
    }
}

fn with_languages(mut arguments: Vec<String>, languages: &[String]) -> Vec<String> {
    arguments.push("--languages".into());
    arguments.extend_from_slice(languages);
    arguments
}

fn workflow(options: &PipelineOptions, steps: &mut impl Steps) -> PipelineResult<()> {
    let languages = options
        .selected
        .iter()
        .map(|(l, _)| l.clone())
        .collect::<Vec<_>>();
    steps.log(&format!("LINGUE {}", languages.join(" ")))?;
    let fetch = arguments(
        "fetch-dumps",
        &[
            ("languages-file", path_text(&options.languages_file)),
            ("sql-release", options.sql_release.clone()),
            ("history-release", options.history_release.clone()),
            ("root", path_text(&options.dump_root)),
        ],
    );
    let content = arguments(
        "fetch-content",
        &[
            ("languages-file", path_text(&options.languages_file)),
            ("content-release", options.content_release.clone()),
            ("root", path_text(&options.dump_root)),
        ],
    );
    if options.plan {
        let mut sql = with_languages(fetch, &languages);
        sql.push("--plan".into());
        let mut xml = with_languages(content, &languages);
        xml.push("--plan".into());
        steps.run("piano dei download", &sql)?;
        steps.run("piano contenuto corrente", &xml)?;
        return Ok(());
    }
    std::fs::create_dir_all(&options.dump_root)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(options.dump_root.join("pipeline.lock"))?;
    lock.try_lock()
        .map_err(|_| "Un'altra pipeline sta usando gli stessi checkpoint")?;
    let _lock = lock;
    let mut state = StepState::open(options.dump_root.join("pipeline-state.json"))?;
    let mut missing = Vec::new();
    for edition in &options.selected {
        let (language, database) = edition;
        let inputs = json!({"language":language,"database":database,"sql_release":options.sql_release,"history_release":options.history_release,"dump_root":path_text(&options.dump_root)});
        let outputs = manifest_for(options, language, database)
            .and_then(|(path, meta)| fetched_outputs(&path, &meta).ok())
            .unwrap_or_default();
        let inherited=state.inherit("fetch",&json!({"sql_release":options.sql_release,"history_release":options.history_release,"dump_root":path_text(&options.dump_root)}),edition,&outputs);
        let key = format!("fetch:{language}");
        let reason = state.reason(&key, &inputs, &outputs);
        steps.log(&format!(
            "CHECKPOINT download {language}: {}",
            if options.force {
                "forced"
            } else if inherited {
                "verified_reuse"
            } else {
                reason
            }
        ))?;
        if !options.force && !outputs.is_empty() && (reason == "verified_reuse" || inherited) {
            steps.log(&format!(
                "SALTA download {language}: file verificati e invariati"
            ))?;
            state.save(&key, inputs, &outputs)?;
        } else {
            missing.push(language.clone());
        }
    }
    if !missing.is_empty() {
        steps.run(
            &format!("download verificato: {}", missing.join(" ")),
            &with_languages(fetch, &missing),
        )?;
    }
    let mut manifests = BTreeMap::new();
    let mut graph_paths = Vec::new();
    for (language, database) in &options.selected {
        let (manifest, metadata) = manifest_for(options, language, database)
            .ok_or("Download terminato senza tutti i manifest richiesti")?;
        let fetch_inputs = json!({"language":language,"database":database,"sql_release":options.sql_release,"history_release":options.history_release,"dump_root":path_text(&options.dump_root)});
        state.save(
            &format!("fetch:{language}"),
            fetch_inputs,
            &fetched_outputs(&manifest, &metadata)?,
        )?;
        let manifest_sha = dumps::file_sha256(&manifest)?;
        manifests.insert(language.clone(), manifest_sha.clone());
        let graph = options.import_dir.join(format!("{language}-raw.json"));
        let history = options.import_dir.join(format!("{language}-history.json"));
        let stage = options
            .work_dir
            .join(format!("{database}-{}.sqlite", options.sql_release));
        let inputs = json!({"manifest_sha256":manifest_sha,"graph":path_text(&graph),"history":path_text(&history),"stage":path_text(&stage),"history_rule":dumps::HISTORY_RULE});
        let outputs = vec![graph.clone(), history.clone(), stage.clone()];
        let key = format!("import:{language}");
        let saved = state.reason(&key, &inputs, &outputs);
        let reason = if options.force {
            "forced"
        } else if saved == "verified_reuse" {
            saved
        } else {
            imported_reason(&graph, &history, &stage, &manifest, &metadata, true)
        };
        steps.log(&format!(
            "CHECKPOINT importazione {language}: {reason}; stato={saved}"
        ))?;
        if reason == "verified_reuse" {
            steps.log(&format!(
                "SALTA importazione {language}: grafo, cronologia e checkpoint già completi"
            ))?;
        } else {
            let graph_reason = if options.force {
                "forced"
            } else {
                imported_reason(&graph, &history, &stage, &manifest, &metadata, false)
            };
            let history_only = graph_reason == "verified_reuse";
            let mut command = arguments(
                if history_only {
                    "import-history"
                } else {
                    "import-dumps"
                },
                &[
                    ("manifest", path_text(&manifest)),
                    (
                        "output",
                        path_text(if history_only { &history } else { &graph }),
                    ),
                    ("work-dir", path_text(&options.work_dir)),
                    ("history-cache-mib", options.history_cache_mib.to_string()),
                    (
                        "history-commit-rows",
                        options.history_commit_rows.to_string(),
                    ),
                ],
            );
            if !history_only {
                command.extend(["--history-output".into(), path_text(&history)]);
            }
            steps.run(
                &format!(
                    "{} {language}",
                    if history_only {
                        "aggiornamento cronologia"
                    } else {
                        "importazione"
                    }
                ),
                &command,
            )?;
            if imported_reason(&graph, &history, &stage, &manifest, &metadata, true)
                != "verified_reuse"
            {
                return Err(format!(
                    "Importazione {language} terminata senza checkpoint completi verificabili"
                )
                .into());
            }
        }
        state.save(&key, inputs, &outputs)?;
        graph_paths.push(graph);
    }
    let model = read_json(&options.model_config)?;
    let mut censuses = BTreeMap::new();
    let mut missing_topics = Vec::new();
    for (language, database) in &options.selected {
        let graph = options.import_dir.join(format!("{language}-raw.json"));
        let hash = dumps::file_sha256(&graph)?;
        let connection = rusqlite::Connection::open_with_flags(
            options
                .work_dir
                .join(format!("{database}-{}.sqlite", options.sql_release)),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let count =
            connection.query_row("SELECT COUNT(*) FROM pages", [], |row| row.get::<_, u64>(0))?;
        let checkpoint = options.import_dir.join(format!("{language}-topics.json"));
        if !options.force && topics_match(&checkpoint, &graph, &hash, language, &model, count) {
            steps.log(&format!(
                "SALTA temi {language}: checkpoint completo del modello e grafo invariato"
            ))?;
        } else {
            missing_topics.push(language.clone());
        }
        censuses.insert(language.clone(), count);
    }
    if !missing_topics.is_empty() {
        let command = arguments(
            "topics",
            &[
                ("input-dir", path_text(&options.import_dir)),
                ("output-dir", path_text(&options.import_dir)),
                ("languages-file", path_text(&options.languages_file)),
                ("model", path_text(&options.model)),
                ("model-config", path_text(&options.model_config)),
                ("work-dir", path_text(&options.work_dir)),
                (
                    "time-budget-seconds",
                    options.topic_budget_seconds.to_string(),
                ),
                ("batch-size", options.topic_batch_size.to_string()),
            ],
        );
        steps.run(
            &format!("modello dei 64 temi: {}", missing_topics.join(" ")),
            &with_languages(command, &missing_topics),
        )?;
        for language in missing_topics {
            let graph = options.import_dir.join(format!("{language}-raw.json"));
            if !topics_match(
                &options.import_dir.join(format!("{language}-topics.json")),
                &graph,
                &dumps::file_sha256(&graph)?,
                &language,
                &model,
                censuses[&language],
            ) {
                return Err(format!(
                    "Inferenza dei temi incompleta per {language}; ripetere la pipeline"
                )
                .into());
            }
        }
    }
    // XML downloads have the same per-edition reuse guarantee as SQL/History.
    let mut missing_content = Vec::new();
    for edition in &options.selected {
        let (language, database) = edition;
        let path = options
            .dump_root
            .join(database)
            .join(format!("content-{}.json", options.content_release));
        let outputs = read_json(&path)
            .ok()
            .and_then(|meta| fetched_outputs(&path, &meta).ok())
            .unwrap_or_default();
        let inputs = json!({"language":language,"database":database,"release":options.content_release,"dump_root":path_text(&options.dump_root)});
        let key = format!("content-fetch:{language}");
        let inherited = state.inherit(
            "content-fetch",
            &json!({"release":options.content_release}),
            edition,
            &outputs,
        );
        let reason = state.reason(&key, &inputs, &outputs);
        if !outputs.is_empty() && (reason == "verified_reuse" || inherited) {
            state.save(&key, inputs, &outputs)?;
            steps.log(&format!(
                "SALTA contenuto XML {language}: file verificati e invariati"
            ))?;
        } else {
            missing_content.push(language.clone());
        }
    }
    if !missing_content.is_empty() {
        steps.run(
            "contenuto corrente verificato",
            &with_languages(content, &missing_content),
        )?;
    }
    for (language, database) in &options.selected {
        let manifest = options
            .dump_root
            .join(database)
            .join(format!("content-{}.json", options.content_release));
        let metadata = read_json(&manifest)?;
        if metadata["language"] != *language
            || metadata["database"] != *database
            || metadata["content_release"] != options.content_release
            || metadata["complete"] != true
        {
            return Err("Manifest del contenuto corrente non corrispondente".into());
        }
        state.save(&format!("content-fetch:{language}"),json!({"language":language,"database":database,"release":options.content_release,"dump_root":path_text(&options.dump_root)}),&fetched_outputs(&manifest,&metadata)?)?;
        let graph = options.import_dir.join(format!("{language}-raw.json"));
        let quality = options.import_dir.join(format!("{language}-quality.json"));
        let inputs = json!({"graph_sha256":dumps::file_sha256(&graph)?,"manifest_sha256":dumps::file_sha256(&manifest)?,
            "model_sha256":dumps::file_sha256(&options.root.join("config/quality.json"))?,"quality_revision_rule":"selected_official_xml_revision_v1"});
        let key = format!("quality:{language}");
        let reason = state.reason(&key, &inputs, std::slice::from_ref(&quality));
        steps.log(&format!("CHECKPOINT qualità {language}: {reason}"))?;
        if !options.force && reason == "verified_reuse" {
            steps.log(&format!(
                "SALTA qualità {language}: supplemento verificato e invariato"
            ))?;
        } else {
            let command = arguments(
                "quality",
                &[
                    ("languages-file", path_text(&options.languages_file)),
                    ("content-release", options.content_release.clone()),
                    ("root", path_text(&options.dump_root)),
                    ("input-dir", path_text(&options.import_dir)),
                    ("output-dir", path_text(&options.import_dir)),
                    ("work-dir", path_text(&options.work_dir)),
                ],
            );
            steps.run(
                &format!("qualità {language}"),
                &with_languages(command, std::slice::from_ref(language)),
            )?;
        }
        state.save(&key, inputs, std::slice::from_ref(&quality))?;
    }
    let configured = crate::configured_languages(&read_json(&options.languages_file)?)?;
    let allowed = configured
        .iter()
        .map(|(l, _)| l.clone())
        .collect::<Vec<_>>();
    let (graphs, _) = dumps::comparison_collections(&graph_paths, &options.output_dir, &allowed)?;
    let mut selected: Vec<(String, String)> = Vec::new();
    let mut measures = BTreeMap::new();
    let mut histories = Vec::new();
    for graph in &graphs {
        let metadata = dumps::first_metadata(graph)?;
        let language = metadata["language"]
            .as_str()
            .ok_or("Lingua del grafo mancante")?;
        let database = crate::wiki_database(language, metadata["database"].as_str())?;
        manifests.insert(
            language.into(),
            metadata["dump_manifest_sha256"]
                .as_str()
                .ok_or("Identità del manifest mancante")?
                .into(),
        );
        let parent = graph.parent().ok_or("Directory del grafo mancante")?;
        let history = parent.join(format!("{language}-history.json"));
        let quality = parent.join(format!("{language}-quality.json"));
        let mut required = json!({"history":dumps::first_metadata(&history)?});
        if quality.is_file() {
            required["quality"] = dumps::first_metadata(&quality)?;
        }
        measures.insert(language.into(), required);
        histories.push(history);
        selected.push((language.into(), database));
    }
    selected.sort();
    steps.log(&format!(
        "LINGUE confronto incrementale: {}",
        selected
            .iter()
            .map(|(l, _)| l.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    ))?;
    let rules = read_json(&options.root.join("config/categories.json"))?;
    let expected = PublicationInputs {
        selected: &selected,
        manifests: &manifests,
        model: &model,
        measures: &measures,
        rules: &rules,
    };
    if !options.force && current_publication(&options.output_dir, &expected, true) {
        steps.log("SALTA analisi/confronto: osservazioni e misure già aggiornate")?;
        return Ok(());
    }
    if !options.force && current_publication(&options.output_dir, &expected, false) {
        for graph in &graphs {
            let meta = dumps::first_metadata(graph)?;
            let language = meta["language"].as_str().ok_or("Lingua mancante")?;
            let command = arguments(
                "refresh-measures",
                &[
                    ("languages-file", path_text(&options.languages_file)),
                    (
                        "input-dir",
                        path_text(graph.parent().ok_or("Directory input mancante")?),
                    ),
                    ("data-dir", path_text(&options.output_dir)),
                ],
            );
            steps.run(
                &format!("aggiornamento misure {language} senza ricalcolo del grafo"),
                &with_languages(command, &[language.into()]),
            )?;
        }
    } else if selected.len() > 1 {
        let mut command = vec!["compare".into()];
        command.extend(graphs.iter().map(|p| path_text(p)));
        append_options(
            &mut command,
            &[
                ("languages-file", path_text(&options.languages_file)),
                ("output-dir", path_text(&options.output_dir)),
                ("work-dir", path_text(&options.work_dir)),
                (
                    "rules",
                    path_text(&options.root.join("config/categories.json")),
                ),
            ],
        );
        command.push("--histories".into());
        command.extend(histories.iter().map(|p| path_text(p)));
        steps.run("confronto interlingua", &command)?;
    } else {
        let language = &selected[0].0;
        let stem = format!(
            "{language}-{}-{}",
            options.sql_release, options.history_release
        );
        let plain = options.output_dir.join(format!("{stem}.json"));
        let output = if plain.exists() {
            let parent = graphs[0].parent().ok_or("Directory input mancante")?;
            let mut identity = json!({"graph":dumps::file_sha256(&graphs[0])?,"rules":rules});
            for suffix in ["history", "quality", "topics"] {
                let file = parent.join(format!("{language}-{suffix}.json"));
                if file.is_file() {
                    identity[suffix] = json!(dumps::file_sha256(&file)?);
                }
            }
            let hash = dumps::canonical_json_sha256(&identity)?;
            options
                .output_dir
                .join(format!("{stem}-{}.json", &hash[..12]))
        } else {
            plain
        };
        let mut command = vec!["analyze".into(), path_text(&graphs[0])];
        append_options(
            &mut command,
            &[
                ("history", path_text(&histories[0])),
                ("output", path_text(&output)),
                ("work-dir", path_text(&options.work_dir)),
                (
                    "rules",
                    path_text(&options.root.join("config/categories.json")),
                ),
            ],
        );
        steps.run(&format!("analisi {language}"), &command)?;
    }
    if !current_publication(&options.output_dir, &expected, true) {
        return Err("Elaborazione terminata senza pubblicazione completa verificabile".into());
    }
    Ok(())
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
            runtime::phase("pipeline", || workflow(&options, &mut runner))
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
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        options: PipelineOptions,
        steps: FakeSteps,
    }

    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("orphanwiki-pipeline-{}", runtime::unique_id()));
            std::fs::create_dir_all(root.join("config")).unwrap();
            write(
                &root.join("config/languages.json"),
                &json!({"version":1,"languages":["vec","lmo"]}),
            );
            write(&root.join("config/categories.json"), &json!({}));
            write(
                &root.join("config/topics.json"),
                &serde_json::from_str(include_str!("../config/topics.json")).unwrap(),
            );
            write(
                &root.join("config/quality.json"),
                &json!({"model":"synthetic controller fixture"}),
            );
            let options = PipelineOptions {
                root: root.clone(),
                languages_file: root.join("config/languages.json"),
                selected: vec![("vec".into(), "vecwiki".into())],
                sql_release: "20261001".into(),
                history_release: "2026-09".into(),
                content_release: "2026-10-01".into(),
                dump_root: root.join("dumps"),
                import_dir: root.join("imported"),
                output_dir: root.join("exports"),
                work_dir: root.join("staging"),
                log_dir: root.join("logs"),
                model: root.join("unused-model.bin"),
                model_config: root.join("config/topics.json"),
                topic_budget_seconds: 86400.0,
                topic_batch_size: 256,
                history_cache_mib: 256,
                history_commit_rows: 250000,
                metrics_interval: 0.1,
                no_phase_spans: false,
                force: false,
                plan: false,
            };
            let steps = FakeSteps {
                root: root.clone(),
                calls: Vec::new(),
                logs: Vec::new(),
                fail: None,
                incomplete_topics: false,
                generation: 0,
            };
            Self {
                root,
                options,
                steps,
            }
        }
        fn run(&mut self) -> PipelineResult<()> {
            workflow(&self.options, &mut self.steps)
        }
        fn reset(&mut self) {
            self.steps.calls.clear();
            self.steps.logs.clear();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn write(path: &Path, value: &Value) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if read_json(path).ok().as_ref() == Some(value) {
            return;
        }
        if value.get("metadata").is_some() {
            let mut file = File::create(path).unwrap();
            file.write_all(b"{").unwrap();
            let mut first = true;
            for key in ["metadata", "pages"].into_iter().chain(
                value
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .filter(|s| !matches!(*s, "metadata" | "pages")),
            ) {
                let Some(value) = value.get(key) else {
                    continue;
                };
                if !first {
                    file.write_all(b",").unwrap();
                }
                first = false;
                serde_json::to_writer(&mut file, key).unwrap();
                file.write_all(b":").unwrap();
                serde_json::to_writer(&mut file, value).unwrap();
            }
            file.write_all(b"}\n").unwrap();
        } else {
            // Native operational documents contain trailing whitespace too.
            dumps::DumpClient::<dumps::WikimediaHttp>::write_json(path, value).unwrap();
        }
    }

    struct FakeSteps {
        root: PathBuf,
        calls: Vec<Vec<String>>,
        logs: Vec<String>,
        fail: Option<String>,
        incomplete_topics: bool,
        generation: usize,
    }

    impl FakeSteps {
        fn publish(&mut self, languages: &[String], folder: &Path) {
            self.generation += 1;
            std::fs::create_dir_all(folder).unwrap();
            let mut manifest = read_json(&folder.join("manifest.json"))
                .unwrap_or(json!([]))
                .as_array()
                .unwrap()
                .clone();
            let mut members = Vec::new();
            for language in languages {
                let graph =
                    read_json(&self.root.join(format!("imported/{language}-raw.json"))).unwrap();
                let mut metadata = graph["metadata"].clone();
                metadata["age_rule"] = json!(dumps::HISTORY_RULE);
                metadata["measures_rule"] = json!("creation_quality_creators_v3");
                metadata["category_rules"] =
                    read_json(&self.root.join("config/categories.json")).unwrap();
                metadata["topics"] = dumps::first_metadata(
                    &self.root.join(format!("imported/{language}-topics.json")),
                )
                .unwrap();
                metadata["topics"]["status"] = json!("predicted");
                metadata["history"] = dumps::first_metadata(
                    &self.root.join(format!("imported/{language}-history.json")),
                )
                .unwrap();
                metadata["history"]["status"] = json!("collected");
                metadata["quality"] = dumps::first_metadata(
                    &self.root.join(format!("imported/{language}-quality.json")),
                )
                .unwrap();
                if languages.len() > 1 {
                    metadata["comparison"] = json!({"status":"compared","reference_languages":languages.iter().filter(|l|*l!=language).collect::<Vec<_>>()});
                }
                let file = format!("{language}-result-{}.json", self.generation);
                write(
                    &folder.join(&file),
                    &json!({"metadata":metadata,"pages":[{"page_id":1,"title":"A","orphan":true},{"page_id":2,"title":"B","orphan":false}],
                    "link_candidates":{"1":{"status":"compared","candidate_count":0,"candidates":[]}}}),
                );
                let private = folder.join(format!("private-{}", runtime::unique_id()));
                let names =
                    dumps::prepare_dashboard_exports(&folder.join(&file), &private, None, None)
                        .unwrap();
                for entry in std::fs::read_dir(&private).unwrap() {
                    let entry = entry.unwrap();
                    std::fs::rename(entry.path(), folder.join(entry.file_name())).unwrap();
                }
                std::fs::remove_dir(private).unwrap();
                let mut entry = names;
                for (key, value) in [
                    ("file", json!(file)),
                    ("language", json!(language)),
                    ("date", metadata["finished_at"].clone()),
                    ("kind", json!("wiki")),
                    ("published_at", json!(format!("{:08}", self.generation))),
                ] {
                    entry[key] = value;
                }
                manifest.push(entry);
                members.push(json!({"language":language,"file":file,"path":folder.join(&file),"page_count":2,"metadata":metadata}));
            }
            if languages.len() > 1 {
                members.sort_by(|a, b| a["language"].as_str().cmp(&b["language"].as_str()));
                let index = dumps::aggregate_index(&members).unwrap();
                let stem = format!("all-{}", self.generation);
                write(&folder.join(format!("{stem}.json")), &index);
                dumps::write_aggregate_summary(
                    &members,
                    &folder.join(format!("{stem}-summary.json")),
                    &index["metadata"],
                    true,
                )
                .unwrap();
                manifest.push(json!({"kind":"aggregate","language":"all","file":format!("{stem}.json"),"summary_file":format!("{stem}-summary.json"),"creator_index_file":format!("{stem}-creators.json")}));
            }
            write(&folder.join("manifest.json"), &json!(manifest));
        }
    }

    impl Steps for FakeSteps {
        fn log(&mut self, text: &str) -> PipelineResult<()> {
            self.logs.push(text.into());
            Ok(())
        }
        fn run(&mut self, _name: &str, args: &[String]) -> PipelineResult<()> {
            self.calls.push(args.to_vec());
            let command = args[0].as_str();
            if self.fail.as_deref() == Some(command) {
                return Err("Synthetic child failure".into());
            }
            if args.iter().any(|s| s == "--plan") {
                return Ok(());
            }
            let value = |key: &str| args[args.iter().position(|v| v == key).unwrap() + 1].clone();
            let languages = || {
                let start = args.iter().position(|v| v == "--languages").unwrap() + 1;
                args[start..]
                    .iter()
                    .take_while(|v| !v.starts_with("--"))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            match command {
                "fetch-dumps" => {
                    for language in languages() {
                        let database = format!("{language}wiki");
                        let root = PathBuf::from(value("--root")).join(&database);
                        let mut files = Map::new();
                        for name in dumps::SQL_TABLES.into_iter().chain(["history.tsv.bz2"]) {
                            let path = root.join(name);
                            std::fs::create_dir_all(&root).unwrap();
                            std::fs::write(&path, name).unwrap();
                            files.insert(name.into(),json!({"path":path,"sha256_local":dumps::file_sha256(&path).unwrap(),"bytes":name.len()}));
                        }
                        write(
                            &root.join("manifest-20261001-2026-09.json"),
                            &json!({"language":language,"database":database,"sql_release":"20261001","history_release":"2026-09","files":files}),
                        );
                    }
                }
                "import-dumps" | "import-history" => {
                    let manifest = PathBuf::from(value("--manifest"));
                    let meta = read_json(&manifest).unwrap();
                    let language = meta["language"].as_str().unwrap();
                    let database = meta["database"].as_str().unwrap();
                    let stage = PathBuf::from(value("--work-dir"))
                        .join(format!("{database}-20261001.sqlite"));
                    std::fs::create_dir_all(stage.parent().unwrap()).unwrap();
                    let db = rusqlite::Connection::open(&stage).unwrap();
                    db.execute_batch("CREATE TABLE IF NOT EXISTS imported(table_name TEXT PRIMARY KEY,sha256 TEXT);CREATE TABLE IF NOT EXISTS pages(id INTEGER PRIMARY KEY,data TEXT);CREATE TABLE IF NOT EXISTS history_imported(file TEXT,phase TEXT,sha256 TEXT,PRIMARY KEY(file,phase));").unwrap();
                    if command == "import-dumps" {
                        for name in dumps::SQL_TABLES {
                            db.execute(
                                "INSERT OR REPLACE INTO imported VALUES (?,?)",
                                rusqlite::params![
                                    name,
                                    meta["files"][name]["sha256_local"].as_str().unwrap()
                                ],
                            )
                            .unwrap();
                        }
                        db.execute_batch("INSERT OR REPLACE INTO pages VALUES(1,'{}'),(2,'{}')")
                            .unwrap();
                        write(
                            Path::new(&value("--output")),
                            &json!({"metadata":{"language":language,"database":database,"source":format!("Wikimedia SQL dumps / {database}"),"complete":true,
                            "started_at":"2026-10-01T00:00:00Z","finished_at":"2026-10-02T00:00:00Z","sql_release":"20261001","dump_manifest":manifest,"dump_manifest_sha256":dumps::file_sha256(&manifest).unwrap()},
                            "pages":[{"page_id":1,"title":"A"},{"page_id":2,"title":"B"}]}),
                        );
                    }
                    let digest = meta["files"]["history.tsv.bz2"]["sha256_local"]
                        .as_str()
                        .unwrap();
                    for (phase, suffix) in [
                        ("initial", dumps::HISTORY_RULE),
                        ("activity", "synthetic-user-set"),
                    ] {
                        db.execute(
                            "INSERT OR REPLACE INTO history_imported VALUES (?,?,?)",
                            rusqlite::params![
                                "history.tsv.bz2",
                                phase,
                                format!("{digest}:{suffix}")
                            ],
                        )
                        .unwrap();
                    }
                    let output = value(if command == "import-dumps" {
                        "--history-output"
                    } else {
                        "--output"
                    });
                    write(
                        Path::new(&output),
                        &json!({"metadata":{"source":format!("MediaWiki History dumps / {database}"),"language":language,"database":database,"complete":true,"sql_release":"20261001","history_release":"2026-09",
                        "creation_rule":dumps::HISTORY_RULE,"history_files":{"history.tsv.bz2":digest}},"pages":[]}),
                    );
                }
                "topics" => {
                    for language in languages() {
                        let graph = PathBuf::from(value("--input-dir"))
                            .join(format!("{language}-raw.json"));
                        let metadata = dumps::first_metadata(&graph).unwrap();
                        write(
                            &PathBuf::from(value("--output-dir"))
                                .join(format!("{language}-topics.json")),
                            &json!({"metadata":{"language":language,"complete":!self.incomplete_topics,
                        "source":"local_model_on_verified_SQL_dump_outlinks","input_file_sha256":dumps::file_sha256(&graph).unwrap(),"graph_finished_at":metadata["finished_at"],
                        "model":read_json(Path::new(&value("--model-config"))).unwrap(),"predicted_pages":2,"missing_pages":0},"pages":[]}),
                        );
                    }
                }
                "fetch-content" => {
                    for language in languages() {
                        let database = format!("{language}wiki");
                        let root = PathBuf::from(value("--root")).join(&database);
                        std::fs::create_dir_all(&root).unwrap();
                        let path = root.join("content.xml");
                        std::fs::write(&path, "synthetic XML archive").unwrap();
                        write(
                            &root.join("content-2026-10-01.json"),
                            &json!({"language":language,"database":database,"complete":true,"content_release":"2026-10-01","files":{"content.xml":{"path":path}}}),
                        );
                    }
                }
                "quality" => {
                    for language in languages() {
                        write(
                            &PathBuf::from(value("--output-dir"))
                                .join(format!("{language}-quality.json")),
                            &json!({"metadata":{"language":language,"complete":true,"quality_revision_rule":"selected_official_xml_revision_v1","definition":"synthetic controller fixture"},"pages":[]}),
                        );
                    }
                }
                "analyze" => {
                    let language = dumps::first_metadata(Path::new(&args[1])).unwrap()["language"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    self.publish(&[language], Path::new(&value("--output")).parent().unwrap());
                }
                "compare" => {
                    let languages = args[1..]
                        .iter()
                        .take_while(|s| !s.starts_with("--"))
                        .map(|p| {
                            dumps::first_metadata(Path::new(p)).unwrap()["language"]
                                .as_str()
                                .unwrap()
                                .to_owned()
                        })
                        .collect::<Vec<_>>();
                    self.publish(&languages, Path::new(&value("--output-dir")));
                }
                "refresh-measures" => {
                    let folder = PathBuf::from(value("--data-dir"));
                    let manifest = read_json(&folder.join("manifest.json")).unwrap();
                    let languages = manifest
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|e| e["language"].as_str())
                        .filter(|l| *l != "all")
                        .map(str::to_owned)
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    self.publish(&languages, &folder);
                }
                _ => panic!("Unexpected test command {command}"),
            }
            Ok(())
        }
    }

    #[test]
    fn complete_sequence_and_verified_resume_use_native_commands_only() {
        let mut f = Fixture::new();
        f.run().unwrap();
        assert_eq!(
            f.steps
                .calls
                .iter()
                .map(|c| c[0].as_str())
                .collect::<Vec<_>>(),
            [
                "fetch-dumps",
                "import-dumps",
                "topics",
                "fetch-content",
                "quality",
                "analyze"
            ]
        );
        let raw = signature(&f.options.import_dir.join("vec-raw.json"));
        f.reset();
        f.run().unwrap();
        assert!(f.steps.calls.is_empty(), "{:?}", f.steps.calls);
        assert_eq!(signature(&f.options.import_dir.join("vec-raw.json")), raw);
    }

    #[test]
    fn every_selected_edition_reaches_all_collection_stages_before_comparison() {
        let mut f = Fixture::new();
        f.options.selected.push(("lmo".into(), "lmowiki".into()));
        f.run().unwrap();
        assert_eq!(
            f.steps
                .calls
                .iter()
                .map(|c| c[0].as_str())
                .collect::<Vec<_>>(),
            [
                "fetch-dumps",
                "import-dumps",
                "import-dumps",
                "topics",
                "fetch-content",
                "quality",
                "quality",
                "compare"
            ]
        );
        for command in f
            .steps
            .calls
            .iter()
            .filter(|c| matches!(c[0].as_str(), "fetch-dumps" | "fetch-content" | "topics"))
        {
            assert!(command.iter().any(|s| s == "vec"));
            assert!(command.iter().any(|s| s == "lmo"));
        }
        f.reset();
        f.run().unwrap();
        assert!(f.steps.calls.is_empty());
    }

    #[test]
    fn plans_write_nothing_and_include_sql_history_and_current_content() {
        let mut f = Fixture::new();
        f.options.plan = true;
        f.run().unwrap();
        assert_eq!(f.steps.calls.len(), 2);
        assert!(f.steps.calls.iter().all(|c| c.contains(&"--plan".into())));
        assert!(!f.options.dump_root.exists());
        assert!(!f.options.import_dir.exists());
        assert!(!f.options.output_dir.exists());
        assert!(!f.options.log_dir.exists());
    }

    #[test]
    fn failed_children_and_deferred_topics_stop_before_later_stages() {
        let mut f = Fixture::new();
        f.steps.fail = Some("import-dumps".into());
        assert!(f.run().is_err());
        assert_eq!(f.steps.calls.len(), 2);
        f.reset();
        f.steps.fail = None;
        f.steps.incomplete_topics = true;
        assert!(f.run().is_err());
        assert_eq!(
            f.steps
                .calls
                .iter()
                .map(|c| c[0].as_str())
                .collect::<Vec<_>>(),
            ["import-dumps", "topics"]
        );
        assert!(!f.options.output_dir.exists());
        f.reset();
        f.steps.incomplete_topics = false;
        f.run().unwrap();
        assert_eq!(f.steps.calls[0][0], "topics");
    }

    #[test]
    fn history_only_refresh_reuses_sql_and_topics_and_preserves_source_archives() {
        let mut f = Fixture::new();
        f.run().unwrap();
        f.reset();
        let stage = f.options.work_dir.join("vecwiki-20261001.sqlite");
        rusqlite::Connection::open(stage)
            .unwrap()
            .execute(
                "UPDATE history_imported SET sha256='stale' WHERE phase='initial'",
                [],
            )
            .unwrap();
        f.run().unwrap();
        assert_eq!(
            f.steps
                .calls
                .iter()
                .map(|c| c[0].as_str())
                .collect::<Vec<_>>(),
            ["import-history"]
        );
        let history = f.options.import_dir.join("vec-history.json");
        let mut value = read_json(&history).unwrap();
        value["metadata"]["new_measure"] = json!("verified supplement update");
        write(&history, &value);
        f.reset();
        f.run().unwrap();
        assert_eq!(
            f.steps
                .calls
                .iter()
                .map(|c| c[0].as_str())
                .collect::<Vec<_>>(),
            ["refresh-measures"]
        );
    }

    #[test]
    fn extending_acquisition_retains_prior_editions_without_rehashing_their_archives() {
        let mut f = Fixture::new();
        f.run().unwrap();
        let archive = f.options.dump_root.join("vecwiki/page");
        let before = signature(&archive);
        let raw_before = signature(&f.options.import_dir.join("vec-raw.json"));
        f.reset();
        f.options.selected = vec![("lmo".into(), "lmowiki".into())];
        f.run().unwrap();
        assert_eq!(signature(&archive), before);
        assert_eq!(
            signature(&f.options.import_dir.join("vec-raw.json")),
            raw_before
        );
        let fetch = &f.steps.calls[0];
        assert_eq!(fetch.last().map(String::as_str), Some("lmo"));
        assert!(!fetch.iter().any(|s| s == "vec"));
        let compare = f.steps.calls.iter().find(|c| c[0] == "compare").unwrap();
        assert!(compare.iter().any(|s| s.ends_with("vec-raw.json")));
        assert!(compare.iter().any(|s| s.ends_with("lmo-raw.json")));
        f.reset();
        f.run().unwrap();
        assert!(f.steps.calls.is_empty());
    }

    #[test]
    fn changed_category_rules_recompute_observations_and_missing_transports_are_repaired() {
        let mut f = Fixture::new();
        f.run().unwrap();
        f.reset();
        write(
            &f.root.join("config/categories.json"),
            &json!({"min_support":10}),
        );
        f.run().unwrap();
        assert_eq!(
            f.steps
                .calls
                .iter()
                .map(|c| c[0].as_str())
                .collect::<Vec<_>>(),
            ["analyze"]
        );
        let manifest = read_json(&f.options.output_dir.join("manifest.json")).unwrap();
        let latest = manifest.as_array().unwrap().last().unwrap();
        std::fs::remove_file(
            f.options
                .output_dir
                .join(latest["articles_file"].as_str().unwrap()),
        )
        .unwrap();
        f.reset();
        f.run().unwrap();
        assert_eq!(
            f.steps
                .calls
                .iter()
                .map(|c| c[0].as_str())
                .collect::<Vec<_>>(),
            ["refresh-measures"]
        );
    }

    #[test]
    fn legacy_combined_download_checkpoints_seed_per_edition_reuse() {
        let mut f = Fixture::new();
        f.run().unwrap();
        let edition = f.options.selected[0].clone();
        let (manifest, meta) = manifest_for(&f.options, &edition.0, &edition.1).unwrap();
        let outputs = fetched_outputs(&manifest, &meta).unwrap();
        let mut state = StepState::open(f.options.dump_root.join("pipeline-state.json")).unwrap();
        state.steps.remove("fetch:vec");
        state.save("fetch",json!({"languages":[edition],"sql_release":f.options.sql_release,"history_release":f.options.history_release,"dump_root":path_text(&f.options.dump_root)}),&outputs).unwrap();
        f.options.selected.push(("lmo".into(), "lmowiki".into()));
        f.reset();
        f.run().unwrap();
        let downloads = f
            .steps
            .calls
            .iter()
            .filter(|c| c[0] == "fetch-dumps")
            .collect::<Vec<_>>();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].last().unwrap(), "lmo");
        let updated = StepState::open(f.options.dump_root.join("pipeline-state.json")).unwrap();
        assert!(updated.steps.contains_key("fetch:vec"));
        assert!(updated.steps.contains_key("fetch:lmo"));
    }

    #[test]
    fn changed_import_checkpoints_are_revalidated_before_reuse() {
        let mut f = Fixture::new();
        f.run().unwrap();
        f.reset();
        let stage = f.options.work_dir.join("vecwiki-20261001.sqlite");
        rusqlite::Connection::open(stage)
            .unwrap()
            .execute(
                "UPDATE imported SET sha256='changed' WHERE table_name='page'",
                [],
            )
            .unwrap();
        f.run().unwrap();
        assert_eq!(
            f.steps
                .calls
                .iter()
                .map(|c| c[0].as_str())
                .collect::<Vec<_>>(),
            ["import-dumps"]
        );
        let mut state = StepState::open(f.options.dump_root.join("pipeline-state.json")).unwrap();
        state.steps["import:vec"]["inputs"]["history_rule"] = json!("old definition");
        write(&state.path, &json!({"version":1,"steps":state.steps}));
        f.reset();
        f.run().unwrap();
        assert!(f.steps.calls.is_empty());
        let manifest = f.options.output_dir.join("manifest.json");
        let before = std::fs::read(&manifest).unwrap();
        write(
            &f.options.dump_root.join("pipeline-state.json"),
            &json!({"version":999,"steps":{}}),
        );
        f.reset();
        assert!(f.run().is_err());
        assert!(f.steps.calls.is_empty());
        assert_eq!(std::fs::read(manifest).unwrap(), before);
    }

    #[test]
    #[cfg(unix)]
    fn runner_streams_logs_samples_resources_and_reconciles_failed_worker_groups() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let executable = f.root.join("mock-native-cli");
        let script = r#"#!/bin/sh
shift
shift
sleep 30 &
printf '%s' "$!" > worker.pid
printf 'child stdout\n'
printf 'child stderr\n' >&2
printf '{"event":"span_start","span_id":"abandoned-worker","parent_id":"%s","run_id":"%s","name":"mock.worker","started_ns":1}\n' "$ORPHANWIKI_PARENT_SPAN" "$ORPHANWIKI_RUN_ID" >> "$ORPHANWIKI_SPANS_FILE"
sleep 0.3
exit 7
"#;
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut runner = Runner::new(&f.options).unwrap();
        runner.executable = executable;
        let settings = runner.trace.clone();
        let result = runtime::with_tracing(settings, || {
            runtime::phase("pipeline", || {
                runner.run("synthetic failure", &["mock".into()])
            })
        });
        assert!(result.is_err());
        runner.finish(&result).unwrap();
        let report = read_json(runner.report_path.as_ref().unwrap()).unwrap();
        assert_eq!(report["status"], "failed");
        assert_eq!(report["steps"][0]["returncode"], 7);
        assert!(
            report["steps"][0]["sampled_peak_tree_rss_bytes"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(report["steps"][0]["user_cpu_seconds"].as_f64().unwrap() >= 0.0);
        assert!(report["monitor_error"].is_null());
        let files = std::fs::read_dir(&f.options.log_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        let log = std::fs::read_to_string(
            files
                .iter()
                .find(|p| p.extension().is_some_and(|s| s == "log"))
                .unwrap(),
        )
        .unwrap();
        assert!(log.contains("child stdout\n"));
        assert!(log.contains("child stderr\n"));
        let metrics = std::fs::read_to_string(
            files
                .iter()
                .find(|p| p.to_string_lossy().ends_with(".metrics.jsonl"))
                .unwrap(),
        )
        .unwrap();
        let events = metrics
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(events.iter().filter(|e| e["event"] == "sample").count() >= 3);
        assert!(events.iter().any(|e| e["event"] == "step_start"));
        assert!(events.iter().any(|e| e["event"] == "step_end"));
        let spans = std::fs::read_to_string(&runner.trace.as_ref().unwrap().file)
            .unwrap()
            .lines()
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .collect::<Vec<_>>();
        let closed = spans
            .iter()
            .find(|s| s["span_id"] == "abandoned-worker" && s["event"] == "span_end")
            .unwrap();
        assert_eq!(closed["end_is_detection_bound"], true);
        assert_eq!(closed["returncode"], 7);
        let worker = std::fs::read_to_string(f.root.join("worker.pid")).unwrap();
        let state = std::fs::read_to_string(format!("/proc/{worker}/stat"))
            .ok()
            .and_then(|s| {
                s.rsplit_once(')')
                    .map(|(_, v)| v.trim_start().starts_with('Z'))
            });
        assert!(
            state.is_none_or(|zombie| zombie),
            "Worker remained alive after its command failed"
        );
    }
    #[test]
    fn benchmark_fingerprints_keep_missing_intervals_models_and_every_edge() {
        let f = Fixture::new();
        let path = f.root.join("result.json");
        let mut record = json!({"metadata":{"complete":true,"language":"vec","engine":"python","started_at":"2026-10-01","dump_manifest":"old","topics":{"model_sha512":"frozen","started_at":"one"}},
            "pages":[{"id":1,"age":null,"quality":0.9859704508633369}],"link_candidates":{"1":{"candidates":[{"id":2,"evidence":[{"language":"lmo","source_id":3},{"language":"it","source_id":4}]}]}}});
        let save = |record: &Value| {
            // The canonical reader intentionally requires metadata first.
            let text = format!(
                "{{\"metadata\":{},\"pages\":{},\"link_candidates\":{}}}",
                record["metadata"], record["pages"], record["link_candidates"]
            );
            std::fs::write(&path, text).unwrap();
        };
        save(&record);
        let expected = canonical_fingerprint(&path, None).unwrap();
        assert_eq!(expected["reference_edges"], 2);
        // Independent frozen Python reference, including its exact float encoding.
        assert_eq!(
            expected["sha256"],
            "b7ca872ba3a0645fa9afcd1f3248e9fe4bcc8dd84dd4daa26b478f646c4e1e72"
        );
        record["metadata"]["engine"] = json!("rust");
        record["metadata"]["dump_manifest"] = json!("new");
        record["metadata"]["topics"]["started_at"] = json!("two");
        save(&record);
        assert_eq!(canonical_fingerprint(&path, None).unwrap(), expected);
        for key in ["missing", "edge", "interval", "model", "float"] {
            let mut changed = record.clone();
            match key {
                "missing" => changed["pages"][0]["age"] = json!(0),
                "edge" => {
                    changed["link_candidates"]["1"]["candidates"][0]["evidence"]
                        .as_array_mut()
                        .unwrap()
                        .pop();
                }
                "interval" => changed["metadata"]["started_at"] = json!("2026-10-02"),
                "model" => changed["metadata"]["topics"]["model_sha512"] = json!("changed"),
                _ => changed["pages"][0]["quality"] = json!(0.985970450863337),
            }
            save(&changed);
            assert_ne!(
                canonical_fingerprint(&path, None).unwrap(),
                expected,
                "{key}"
            );
        }
        record["metadata"]["complete"] = json!(false);
        save(&record);
        assert!(canonical_fingerprint(&path, None).is_err());
        assert!(local_output(&f.root, "../outside").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", f.root.join("escape")).unwrap();
            assert!(local_output(&f.root, "escape").is_err());
        }
    }

    #[test]
    fn benchmark_manifest_requires_one_complete_member_per_language() {
        let f = Fixture::new();
        std::fs::write(
            f.root.join("member.json"),
            r#"{"metadata":{"complete":true,"language":"vec"},"pages":[{"id":1}]}"#,
        )
        .unwrap();
        let outputs: Vec<BenchmarkOutput> =
            serde_json::from_value(json!([{"path":"index.json","kind":"manifest"}])).unwrap();
        write(
            &f.root.join("index.json"),
            &json!([{"file":"member.json","language":"vec"},{"kind":"aggregate","file":"unused.json"}]),
        );
        assert_eq!(
            benchmark_outputs(&f.root, &outputs).unwrap()["index.json:vec"]["pages"],
            1
        );
        for entries in [
            json!([]),
            json!([{"file":"member.json","language":"lmo"}]),
            json!([{"file":"../member.json","language":"vec"}]),
            json!([{"file":"member.json","language":"vec"},{"file":"member.json","language":"vec"}]),
        ] {
            write(&f.root.join("index.json"), &entries);
            assert!(benchmark_outputs(&f.root, &outputs).is_err());
        }
    }

    #[test]
    fn benchmark_verdict_requires_paired_time_and_memory_excluding_warmups() {
        let samples = (0..4).flat_map(|pair|["baseline","candidate"].map(move |variant|json!({"pair":pair,"variant":variant,"warmup":pair==0,"wall_seconds":if variant=="baseline" {10.0} else if pair==0 {1000.0} else {5.0},"cpu_seconds":8.0,"sampled_peak_tree_rss_bytes":100}))).collect::<Vec<_>>();
        let result = benchmark_summary(&samples, 1.1, 1.1).unwrap();
        assert_eq!(result["status"], "passed");
        assert_eq!(result["paired_speedup"]["median"], 2.0);
        for (key, value, status) in [
            ("wall_seconds", json!(12.0), "inconclusive"),
            ("sampled_peak_tree_rss_bytes", json!(120), "no_improvement"),
            ("sampled_peak_tree_rss_bytes", Value::Null, "inconclusive"),
        ] {
            let mut changed = samples.clone();
            changed.last_mut().unwrap()[key] = value;
            assert_eq!(
                benchmark_summary(&changed, 1.1, 1.1).unwrap()["status"],
                status
            );
        }
        assert_eq!(
            benchmark_summary(&samples, 3.0, 1.1).unwrap()["status"],
            "no_improvement"
        );
        assert!(benchmark_summary(&samples[..7], 1.1, 1.1).is_err());
        let mut duplicate = samples.clone();
        duplicate.push(samples[2].clone());
        assert!(benchmark_summary(&duplicate, 1.1, 1.1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn benchmark_runs_fresh_copies_and_rejects_mismatch_mutation_and_failure() {
        let f = Fixture::new();
        let seed = f.root.join("seed.json");
        let original =
            r#"{"metadata":{"complete":true,"language":"vec"},"pages":[{"id":1,"age":null}]}"#;
        std::fs::write(&seed, original).unwrap();
        let script = f.root.join("run.sh");
        std::fs::write(&script,"test ! -e \"$1/result.json\" || exit 9\ncp \"$1/seed.json\" \"$1/result.json\"\nprintf private > \"$1/seed.json\"\nsleep 0.15\n").unwrap();
        let spec_path = f.root.join("spec.json");
        let mut spec = json!({"version":1,"inputs":[seed,script],"copies":[{"source":seed,"destination":"seed.json"}],
            "baseline":{"cwd":"{repo}","commands":[["/bin/sh","{repo}/run.sh","{run}"]]},
            "candidate":{"cwd":"{repo}","commands":[["/bin/sh","{repo}/run.sh","{run}"]]},
            "outputs":[{"path":"result.json","kind":"canonical"}]});
        write(&spec_path, &spec);
        let options = |name: &str| BenchmarkOptions {
            root: f.root.clone(),
            spec: spec_path.clone(),
            output_dir: f.root.join(name),
            repetitions: 3,
            warmups: 1,
            metrics_interval: 0.1,
            min_speedup: 10000.0,
            max_rss_ratio: 1.1,
        };
        assert_eq!(benchmark(options("accepted-parity")).unwrap(), 2);
        let report_path = f.root.join("accepted-parity/comparison.json");
        let report = read_json(&report_path).unwrap();
        assert_eq!(report["correctness"], "matched");
        let variants = report["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["variant"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            variants,
            [
                "baseline",
                "candidate",
                "candidate",
                "baseline",
                "baseline",
                "candidate",
                "candidate",
                "baseline"
            ]
        );
        assert_eq!(std::fs::read_to_string(&seed).unwrap(), original);
        for sample in report["samples"].as_array().unwrap() {
            let measured = read_json(
                &f.root
                    .join("accepted-parity")
                    .join(sample["report"].as_str().unwrap()),
            )
            .unwrap();
            assert_eq!(sample["wall_seconds"], measured["steps"][0]["wall_seconds"]);
            assert!(measured["measurement"]["rss"]
                .as_str()
                .unwrap()
                .contains("controller excluded"));
            assert!(sample["sampled_peak_tree_rss_bytes"].as_u64().unwrap() > 0);
        }
        let saved = std::fs::read(&report_path).unwrap();
        assert!(benchmark(options("accepted-parity")).is_err());
        assert_eq!(std::fs::read(&report_path).unwrap(), saved);
        for (name, code, message) in [
            (
                "different",
                "sed 's/null/0/' \"$1/seed.json\" > \"$1/result.json\"",
                "Output differenti",
            ),
            ("failed", "exit 7", "codice di uscita 7"),
            (
                "mutated",
                "printf changed > \"$2\"",
                "Input benchmark modificato",
            ),
        ] {
            spec["candidate"]["commands"] =
                json!([["/bin/sh", "-c", code, "benchmark", "{run}", seed]]);
            write(&spec_path, &spec);
            let mut opts = options(name);
            opts.warmups = 0;
            let error = benchmark(opts).unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
            let report = read_json(&f.root.join(name).join("comparison.json")).unwrap();
            assert_eq!(report["status"], "failed");
            assert!(report.get("correctness").is_none());
        }
        spec["copies"][0]["destination"] = json!("../escape");
        write(&spec_path, &spec);
        assert!(benchmark(options("invalid")).is_err());
        assert!(!f.root.join("invalid").exists());
    }
}
