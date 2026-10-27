use super::*;

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
pub(super) struct BenchmarkSpec {
    pub(super) version: u32,
    pub(super) inputs: Vec<String>,
    #[serde(default)]
    pub(super) copies: Vec<BenchmarkCopy>,
    pub(super) outputs: Vec<BenchmarkOutput>,
    pub(super) baseline: BenchmarkVariant,
    pub(super) candidate: BenchmarkVariant,
}

#[derive(serde::Deserialize)]
pub(super) struct BenchmarkCopy {
    pub(super) source: String,
    pub(super) destination: String,
}

#[derive(serde::Deserialize)]
pub(super) struct BenchmarkVariant {
    #[serde(default = "default_directory")]
    pub(super) cwd: String,
    pub(super) commands: Vec<Vec<String>>,
}

pub(super) fn default_directory() -> String {
    ".".into()
}

#[derive(serde::Deserialize)]
pub(super) struct BenchmarkOutput {
    pub(super) path: String,
    pub(super) kind: OutputKind,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum OutputKind {
    Canonical,
    Manifest,
    Bytes,
}

pub(super) fn relative_path(value: &str) -> PipelineResult<&Path> {
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

pub(super) fn local_output(folder: &Path, name: &str) -> PipelineResult<PathBuf> {
    let path = folder.join(relative_path(name)?).canonicalize()?;
    if !path.starts_with(folder.canonicalize()?) {
        return Err("Output benchmark esterno alla prova".into());
    }
    Ok(path)
}

#[derive(Default)]
pub(super) struct BenchmarkDigest(sha2::Sha256);

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

pub(super) fn canonical_fingerprint(path: &Path, language: Option<&str>) -> PipelineResult<Value> {
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

pub(super) fn benchmark_outputs(
    folder: &Path,
    outputs: &[BenchmarkOutput],
) -> PipelineResult<Value> {
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

pub(super) fn distribution(values: &[Option<f64>]) -> Value {
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

pub(super) fn benchmark_summary(
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

pub(super) fn git_output(cwd: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|r| r.status.success())
        .map(|r| r.stdout)
}

pub(super) fn benchmark_executable(value: &str, cwd: &Path) -> PipelineResult<PathBuf> {
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

pub(super) fn verify_frozen(frozen: &BTreeMap<PathBuf, String>) -> PipelineResult<()> {
    for (path, checksum) in frozen {
        runtime::check_cancelled()?;
        if dumps::file_sha256(path)? != *checksum {
            return Err(format!("Input benchmark modificato: {}", path.display()).into());
        }
    }
    Ok(())
}

pub(super) fn optional_sum(steps: &[Value], keys: &[&str]) -> Option<f64> {
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
