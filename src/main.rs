//! Standalone dump workflow with validated arguments and native execution.
#![forbid(unsafe_code)]
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn option(name: &'static str) -> Arg {
    Arg::new(name).long(name).value_name("VALUE")
}

fn path_option(name: &'static str, default: &'static str) -> Arg {
    option(name).default_value(default)
}

fn editions(command: Command) -> Command {
    command
        .arg(path_option("languages-file", "config/languages.json"))
        .arg(option("languages").num_args(1..))
}

fn history_controls(command: Command) -> Command {
    command
        .arg(
            option("history-cache-mib")
                .default_value("256")
                .value_parser(clap::value_parser!(u32).range(1..=65536)),
        )
        .arg(
            option("history-commit-rows")
                .default_value("250000")
                .value_parser(clap::value_parser!(u32).range(1..=10000000)),
        )
}

fn positive_budget(value: &str) -> Result<f64, String> {
    let value = value.parse::<f64>().map_err(|error| error.to_string())?;
    if !value.is_finite() || value <= 0.0 {
        return Err("Il budget deve essere un numero positivo finito".into());
    }
    Ok(value)
}

fn cli() -> Command {
    let content = editions(
        Command::new("fetch-content").about("Scarica i dump ufficiali del solo contenuto corrente"),
    )
    .arg(option("content-release").required(true))
    .arg(path_option("root", "data/dumps"))
    .arg(Arg::new("plan").long("plan").action(ArgAction::SetTrue));
    let quality = editions(
        Command::new("quality").about("Calcola Johnson V2 dai dump XML correnti verificati"),
    )
    .arg(option("content-release").required(true))
    .arg(path_option("root", "data/dumps"))
    .arg(path_option("input-dir", "data/imported"))
    .arg(path_option("output-dir", "data/imported"))
    .arg(path_option("work-dir", "data/dumps/staging"));
    let refresh = editions(
        Command::new("refresh-measures")
            .about("Aggiorna le misure riutilizzando grafo, temi e candidate pubblicati"),
    )
    .arg(path_option("input-dir", "data/imported"))
    .arg(path_option("data-dir", "web/data"));
    let downloads =
        editions(Command::new("fetch-dumps").about("Scarica e verifica dump Wikimedia completi"))
            .arg(option("sql-release").required(true))
            .arg(option("history-release").required(true))
            .arg(path_option("root", "data/dumps"))
            .arg(Arg::new("plan").long("plan").action(ArgAction::SetTrue));
    let imported = history_controls(
        Command::new("import-dumps").about("Importa grafo SQL e cronologia verificata"),
    )
    .arg(option("manifest").required(true))
    .arg(option("output"))
    .arg(option("history-output"))
    .arg(path_option("work-dir", "data/dumps/staging"));
    let history = editions(history_controls(
        Command::new("import-history").about("Riprende la cronologia dallo staging SQL"),
    ))
    .arg(option("manifest").conflicts_with_all(["languages", "sql-release", "history-release"]))
    .arg(option("sql-release").required_unless_present("manifest"))
    .arg(option("history-release").required_unless_present("manifest"))
    .arg(option("output").requires("manifest").conflicts_with_all([
        "sql-release",
        "history-release",
        "languages",
    ]))
    .arg(path_option("output-dir", "data/imported"))
    .arg(path_option("root", "data/dumps"))
    .arg(path_option("work-dir", "data/dumps/staging"));
    let compare = Command::new("compare")
        .about("Confronto interlingua in tutte le direzioni")
        .arg(Arg::new("inputs").num_args(0..))
        .arg(path_option("input-dir", "data"))
        .arg(path_option("languages-file", "config/languages.json"))
        .arg(path_option("output-dir", "web/data"))
        .arg(option("work-dir"))
        .arg(option("topics").num_args(0..))
        .arg(option("qualities").num_args(0..))
        .arg(option("histories").num_args(0..))
        .arg(path_option("rules", "config/categories.json"));
    let topics = editions(
        Command::new("topics").about("Modello locale sui collegamenti dei dump importati"),
    )
    .arg(path_option("input-dir", "data"))
    .arg(path_option("output-dir", "data"))
    .arg(path_option(
        "model",
        "data/models/johnson-outlink-20220727.bin",
    ))
    .arg(path_option("model-config", "config/topics.json"))
    .arg(
        option("batch-size")
            .default_value("256")
            .value_parser(clap::value_parser!(u32).range(1..=10000)),
    )
    .arg(
        option("time-budget-seconds")
            .default_value("3300")
            .value_parser(positive_budget),
    )
    .arg(option("work-dir"));
    let analyze = Command::new("analyze")
        .about("Analisi nativa di un censimento completo")
        .arg(Arg::new("input").required(true))
        .arg(option("topics"))
        .arg(option("quality"))
        .arg(option("history"))
        .arg(option("work-dir"))
        .arg(option("output"))
        .arg(path_option("rules", "config/categories.json"));
    let serve = Command::new("serve")
        .about("Dashboard e valutazioni editoriali condivise")
        .arg(option("web-root"))
        .arg(option("database"))
        .arg(option("bind").default_value("127.0.0.1"))
        .arg(
            option("port")
                .default_value("8000")
                .value_parser(clap::value_parser!(u16)),
        );
    let pipeline = editions(history_controls(
        Command::new("pipeline")
            .about("Raccolta e analisi sequenziali con checkpoint e metriche operative"),
    ))
    .arg(option("sql-release").required(true))
    .arg(option("history-release").required(true))
    .arg(option("content-release"))
    .arg(path_option("dump-root", "data/dumps"))
    .arg(path_option("import-dir", "data/imported"))
    .arg(path_option("output-dir", "web/data"))
    .arg(option("work-dir"))
    .arg(path_option("log-dir", "data/logs"))
    .arg(path_option(
        "model",
        "data/models/johnson-outlink-20220727.bin",
    ))
    .arg(path_option("model-config", "config/topics.json"))
    .arg(
        option("topic-budget-seconds")
            .default_value("86400")
            .value_parser(positive_budget),
    )
    .arg(
        option("topic-batch-size")
            .default_value("256")
            .value_parser(clap::value_parser!(u32).range(1..=10000)),
    )
    .arg(
        option("metrics-interval")
            .default_value("10")
            .value_parser(positive_budget),
    )
    .arg(
        Arg::new("no-phase-spans")
            .long("no-phase-spans")
            .action(ArgAction::SetTrue),
    )
    .arg(Arg::new("force").long("force").action(ArgAction::SetTrue))
    .arg(Arg::new("plan").long("plan").action(ArgAction::SetTrue));
    let benchmark = Command::new("benchmark")
        .about("Confronta comandi su copie private con parità completa degli output")
        .arg(option("spec").required(true))
        .arg(option("output-dir").required(true))
        .arg(
            option("repetitions")
                .default_value("5")
                .value_parser(clap::value_parser!(u32).range(3..)),
        )
        .arg(
            option("warmups")
                .default_value("1")
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            option("metrics-interval")
                .default_value("0.1")
                .value_parser(positive_budget),
        )
        .arg(
            option("min-speedup")
                .default_value("1.10")
                .value_parser(positive_budget),
        )
        .arg(
            option("max-rss-ratio")
                .default_value("1.10")
                .value_parser(positive_budget),
        );
    Command::new("orphanwiki")
        .about("Censimento e analisi delle pagine orfane")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg(option("repo-root").global(true).help("Radice di configurazioni e percorsi relativi; altrimenti cerca dalla directory corrente"))
        .subcommands([
            Command::new("dashboard").about("Prepara i trasporti dagli export approvati").arg(path_option("data-dir", "web/data")),
            content, quality, refresh, downloads, imported, history, compare, topics, analyze, serve, pipeline, benchmark,
        ])
}

fn repository_root(explicit: Option<&String>, current: &Path) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        let path = current.join(path);
        if !path.is_dir() {
            return Err(format!(
                "Radice del repository non disponibile: {}",
                path.display()
            ));
        }
        return path.canonicalize().map_err(|error| error.to_string());
    }
    Ok(current
        .ancestors()
        .find(|path| path.join("config/languages.json").is_file())
        .unwrap_or(current)
        .to_owned())
}

struct Invocation {
    name: String,
    args: ArgMatches,
    root: PathBuf,
    editions: Vec<(String, String)>,
}

impl Invocation {
    fn path(&self, argument: &str) -> Result<PathBuf, String> {
        self.args
            .get_one::<String>(argument)
            .map(|value| self.root.join(value))
            .ok_or_else(|| format!("Percorso mancante: {argument}"))
    }

    fn load_editions(&mut self) -> Result<(), String> {
        let uses_pool = matches!(
            self.name.as_str(),
            "fetch-dumps"
                | "fetch-content"
                | "quality"
                | "refresh-measures"
                | "topics"
                | "compare"
                | "pipeline"
        ) || (self.name == "import-history"
            && self.args.get_one::<String>("manifest").is_none());
        if !uses_pool {
            return Ok(());
        }
        let path = self.path("languages-file")?;
        let config = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let config = serde_json::from_str(&config).map_err(|error| error.to_string())?;
        let pool = orphanwiki::configured_languages(&config)?;
        let selected = if self.name == "compare" {
            None
        } else {
            self.args
                .get_many::<String>("languages")
                .map(|values| values.cloned().collect::<Vec<_>>())
        };
        self.editions = orphanwiki::select_languages(&pool, selected.as_deref())?;
        // The CLI's explicit acquisition order is independent of the pool's order.
        if let Some(selected) = selected {
            self.editions
                .sort_by_key(|(language, _)| selected.iter().position(|code| code == language));
        }
        Ok(())
    }

    fn benchmark(&self) -> Result<u8, String> {
        let integer = |name| {
            self.args
                .get_one::<u32>(name)
                .copied()
                .ok_or_else(|| format!("Argomento mancante: {name}"))
        };
        let decimal = |name| {
            self.args
                .get_one::<f64>(name)
                .copied()
                .ok_or_else(|| format!("Argomento mancante: {name}"))
        };
        orphanwiki::pipeline::benchmark(orphanwiki::pipeline::BenchmarkOptions {
            root: self.root.clone(),
            spec: self.path("spec")?,
            output_dir: self.path("output-dir")?,
            repetitions: integer("repetitions")?,
            warmups: integer("warmups")?,
            metrics_interval: decimal("metrics-interval")?,
            min_speedup: decimal("min-speedup")?,
            max_rss_ratio: decimal("max-rss-ratio")?,
        })
        .map_err(|e| e.to_string())
    }

    fn execute(&self) -> Result<(), String> {
        if self.name == "pipeline" {
            let value = |name| {
                self.args
                    .get_one::<String>(name)
                    .cloned()
                    .ok_or_else(|| format!("Argomento mancante: {name}"))
            };
            let integer = |name| {
                self.args
                    .get_one::<u32>(name)
                    .copied()
                    .ok_or_else(|| format!("Argomento mancante: {name}"))
            };
            let decimal = |name| {
                self.args
                    .get_one::<f64>(name)
                    .copied()
                    .ok_or_else(|| format!("Argomento mancante: {name}"))
            };
            let sql_release = value("sql-release")?;
            let date = chrono::NaiveDate::parse_from_str(&sql_release, "%Y%m%d")
                .map_err(|_| "Release SQL: usare YYYYMMDD")?;
            if date.format("%Y%m%d").to_string() != sql_release {
                return Err("Release SQL: usare YYYYMMDD".into());
            }
            let dump_root = self.path("dump-root")?;
            let work_dir = self
                .args
                .get_one::<String>("work-dir")
                .map(|p| self.root.join(p))
                .unwrap_or_else(|| dump_root.join("staging"));
            return orphanwiki::pipeline::run(orphanwiki::pipeline::PipelineOptions {
                root: self.root.clone(),
                languages_file: self.path("languages-file")?,
                selected: self.editions.clone(),
                sql_release,
                history_release: value("history-release")?,
                content_release: self
                    .args
                    .get_one::<String>("content-release")
                    .cloned()
                    .unwrap_or_else(|| date.format("%Y-%m-%d").to_string()),
                dump_root,
                import_dir: self.path("import-dir")?,
                output_dir: self.path("output-dir")?,
                work_dir,
                log_dir: self.path("log-dir")?,
                model: self.path("model")?,
                model_config: self.path("model-config")?,
                topic_budget_seconds: decimal("topic-budget-seconds")?,
                topic_batch_size: integer("topic-batch-size")?,
                history_cache_mib: integer("history-cache-mib")?,
                history_commit_rows: integer("history-commit-rows")?,
                metrics_interval: decimal("metrics-interval")?,
                no_phase_spans: self.args.get_flag("no-phase-spans"),
                force: self.args.get_flag("force"),
                plan: self.args.get_flag("plan"),
            })
            .map_err(|e| e.to_string());
        }
        if self.name == "serve" {
            let database = orphanwiki::editorial::ReviewDatabase::from_environment(
                &self.root,
                self.args.get_one::<String>("database").map(PathBuf::from),
            )?;
            let web_root = self
                .args
                .get_one::<String>("web-root")
                .cloned()
                .or_else(|| std::env::var("ORPHANWIKI_WEB_ROOT").ok())
                .unwrap_or_else(|| "web".into());
            let app =
                orphanwiki::editorial::ReviewApplication::new(self.root.join(web_root), database)
                    .with_oauth_from_environment()?;
            return orphanwiki::editorial::serve(
                app,
                self.args
                    .get_one::<String>("bind")
                    .ok_or("Indirizzo mancante")?,
                *self.args.get_one::<u16>("port").ok_or("Porta mancante")?,
            )
            .map_err(|e| e.to_string());
        }
        if self.name == "refresh-measures" {
            let input = self.path("input-dir")?;
            let output = self.path("data-dir")?;
            for (language, _) in &self.editions {
                let quality = input.join(format!("{language}-quality.json"));
                orphanwiki::dumps::refresh_measures(
                    &input.join(format!("{language}-raw.json")),
                    &input.join(format!("{language}-history.json")),
                    &output,
                    quality.is_file().then_some(quality.as_path()),
                )
                .map_err(|e| e.to_string())?;
            }
            orphanwiki::dumps::publish_available_aggregate(&output).map_err(|e| e.to_string())?;
            return Ok(());
        }
        if self.name == "compare" {
            let mut paths = if let Some(inputs) = self.args.get_many::<String>("inputs") {
                inputs.map(|p| self.root.join(p)).collect::<Vec<_>>()
            } else {
                std::fs::read_dir(self.path("input-dir")?)
                    .map_err(|e| e.to_string())?
                    .map(|entry| entry.map(|e| e.path()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|p| {
                        p.is_file()
                            && p.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.ends_with("-raw.json"))
                    })
                    .collect()
            };
            paths.sort();
            if paths.is_empty() {
                return Err("Nessuna raccolta completa *-raw.json nella cartella dati".into());
            }
            let allowed = self
                .editions
                .iter()
                .map(|(code, _)| code.clone())
                .collect::<Vec<_>>();
            let output = self.path("output-dir")?;
            let (paths, published) =
                orphanwiki::dumps::comparison_collections(&paths, &output, &allowed)
                    .map_err(|e| e.to_string())?;
            let work = self
                .args
                .get_one::<String>("work-dir")
                .map(|p| self.root.join(p))
                .unwrap_or_else(|| paths[0].parent().unwrap_or(&self.root).join("staging"));
            let files = |name: &str| {
                self.args
                    .get_many::<String>(name)
                    .map(|values| values.map(|p| self.root.join(p)).collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            let topics = files("topics");
            let histories = files("histories");
            let qualities = files("qualities");
            let rules: serde_json::Value = serde_json::from_slice(
                &std::fs::read(self.path("rules")?).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let connection = orphanwiki::dumps::compare_files(
                &paths,
                &orphanwiki::dumps::ComparisonOptions {
                    rules: &rules,
                    work_dir: &work,
                    topics: &topics,
                    histories: &histories,
                    qualities: &qualities,
                    allowed_languages: Some(&allowed),
                    published: Some(&published),
                },
            )
            .map_err(|e| e.to_string())?;
            let index = orphanwiki::dumps::publish_comparison(&connection, &output)
                .map_err(|e| e.to_string())?;
            println!(
                "Confronto pubblicato: {} edizioni; {}",
                index["members"].as_array().map_or(0, Vec::len),
                output.display()
            );
            return Ok(());
        }
        if self.name == "analyze" {
            let raw = self.path("input")?;
            let metadata = orphanwiki::dumps::first_metadata(&raw).map_err(|e| e.to_string())?;
            let language = metadata["language"].as_str().ok_or("Lingua mancante")?;
            orphanwiki::wiki_database(language, metadata["database"].as_str())?;
            let sibling = |argument: &str| -> Result<Option<PathBuf>, String> {
                if self.args.get_one::<String>(argument).is_some() {
                    return Ok(Some(self.path(argument)?));
                }
                let stem = raw
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or("Nome input non valido")?;
                let path = raw.with_file_name(format!(
                    "{}-{argument}.json",
                    stem.strip_suffix("-raw").unwrap_or(stem)
                ));
                Ok(path.is_file().then_some(path))
            };
            let topics = sibling("topics")?;
            let quality = sibling("quality")?;
            let history = self
                .args
                .get_one::<String>("history")
                .map(|p| self.root.join(p));
            let work = self
                .args
                .get_one::<String>("work-dir")
                .map(|p| self.root.join(p));
            let rules: serde_json::Value = serde_json::from_slice(
                &std::fs::read(self.path("rules")?).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let output = self
                .args
                .get_one::<String>("output")
                .map(|p| self.root.join(p))
                .unwrap_or_else(|| self.root.join(format!("web/data/{language}.json")));
            orphanwiki::dumps::analyze_file(
                &raw,
                &output,
                &orphanwiki::dumps::AnalysisOptions {
                    rules: &rules,
                    work_dir: work.as_deref(),
                    history: history.as_deref(),
                    quality: quality.as_deref(),
                    topics: topics.as_deref(),
                    batch_size: 512,
                },
            )
            .map_err(|e| e.to_string())?;
            return Ok(());
        }
        if self.name == "dashboard" {
            orphanwiki::dumps::rebuild_dashboard(&self.path("data-dir")?)
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        if self.name == "topics" {
            let work = self
                .args
                .get_one::<String>("work-dir")
                .map(|path| self.root.join(path));
            orphanwiki::dumps::run_topics(
                &self
                    .editions
                    .iter()
                    .map(|(code, _)| code.clone())
                    .collect::<Vec<_>>(),
                orphanwiki::dumps::TopicRunOptions {
                    input_dir: &self.path("input-dir")?,
                    output_dir: &self.path("output-dir")?,
                    model_path: &self.path("model")?,
                    spec_path: &self.path("model-config")?,
                    work_dir: work.as_deref(),
                    batch_size: *self
                        .args
                        .get_one::<u32>("batch-size")
                        .ok_or("Batch mancante")? as usize,
                    budget_seconds: *self
                        .args
                        .get_one::<f64>("time-budget-seconds")
                        .ok_or("Budget mancante")?,
                },
            )
            .map_err(|e| e.to_string())?;
            return Ok(());
        }
        if matches!(self.name.as_str(), "fetch-dumps" | "fetch-content") {
            use orphanwiki::dumps::{DumpClient, WikimediaHttp};
            let mut client = DumpClient::new(WikimediaHttp::new()?);
            let release = |name: &str| {
                self.args
                    .get_one::<String>(name)
                    .map(String::as_str)
                    .ok_or_else(|| format!("Release mancante: {name}"))
            };
            let root = self.path("root")?;
            let plan = self.args.get_flag("plan");
            if self.name == "fetch-dumps" {
                client.fetch_dumps(
                    &self.editions,
                    release("sql-release")?,
                    release("history-release")?,
                    &root,
                    plan,
                )?;
            } else {
                client.fetch_content(&self.editions, release("content-release")?, &root, plan)?;
            }
            return Ok(());
        }
        if matches!(self.name.as_str(), "import-dumps" | "import-history") {
            use orphanwiki::dumps::{import_dumps, import_history, HistoryOptions};
            let options = HistoryOptions {
                cache_mib: *self
                    .args
                    .get_one::<u32>("history-cache-mib")
                    .ok_or("Cache History mancante")?,
                commit_rows: *self
                    .args
                    .get_one::<u32>("history-commit-rows")
                    .ok_or("Commit History mancante")? as usize,
                ..HistoryOptions::default()
            };
            let work = self.path("work-dir")?;
            if self.args.get_one::<String>("manifest").is_some() {
                let manifest_path = self.path("manifest")?;
                let manifest: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(&manifest_path).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let language = manifest["language"]
                    .as_str()
                    .ok_or("Lingua del manifest mancante")?;
                orphanwiki::wiki_database(language, manifest["database"].as_str())?;
                let history_argument = if self.name == "import-dumps" {
                    "history-output"
                } else {
                    "output"
                };
                let output = self
                    .args
                    .get_one::<String>(history_argument)
                    .map(|path| self.root.join(path))
                    .unwrap_or_else(|| {
                        self.root
                            .join(format!("data/imported/{language}-history.json"))
                    });
                if self.name == "import-dumps" {
                    let graph = self
                        .args
                        .get_one::<String>("output")
                        .map(|path| self.root.join(path))
                        .unwrap_or_else(|| {
                            self.root.join(format!("data/imported/{language}-raw.json"))
                        });
                    import_dumps(&manifest_path, &graph, &output, &work, options)
                        .map_err(|e| e.to_string())?;
                } else {
                    import_history(&manifest_path, &output, &work, options)
                        .map_err(|e| e.to_string())?;
                }
            } else {
                let sql = self
                    .args
                    .get_one::<String>("sql-release")
                    .ok_or("Release SQL mancante")?;
                let history = self
                    .args
                    .get_one::<String>("history-release")
                    .ok_or("Release History mancante")?;
                for (language, database) in &self.editions {
                    let path = self
                        .path("root")?
                        .join(database)
                        .join(format!("manifest-{sql}-{history}.json"));
                    let output = self
                        .path("output-dir")?
                        .join(format!("{language}-history.json"));
                    import_history(&path, &output, &work, options).map_err(|e| e.to_string())?;
                }
            }
            return Ok(());
        }
        if self.name == "quality" {
            let release = self
                .args
                .get_one::<String>("content-release")
                .ok_or("Release XML mancante")?;
            for (language, database) in &self.editions {
                orphanwiki::dumps::infer_quality(
                    &self.path("input-dir")?.join(format!("{language}-raw.json")),
                    &self
                        .path("root")?
                        .join(database)
                        .join(format!("content-{release}.json")),
                    &self
                        .path("output-dir")?
                        .join(format!("{language}-quality.json")),
                    &self.path("work-dir")?,
                    &self.root.join("config/quality.json"),
                )
                .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        Err(format!("Comando non riconosciuto: {}", self.name))
    }
}

fn main() -> ExitCode {
    let mut arguments = match cli().try_get_matches() {
        Ok(arguments) => arguments,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(error.exit_code() as u8);
        }
    };
    let current = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let root = match repository_root(arguments.get_one::<String>("repo-root"), &current) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    // Existing dump manifests may store paths relative to the repository.
    if let Err(error) = std::env::set_current_dir(&root) {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    let Some((name, args)) = arguments.remove_subcommand() else {
        return ExitCode::from(2);
    };
    let mut invocation = Invocation {
        name,
        args,
        root,
        editions: Vec::new(),
    };
    if let Err(error) = invocation.load_editions() {
        eprintln!("{error}");
        return ExitCode::from(2);
    }
    let _signals = if invocation.name == "serve" {
        None
    } else {
        match orphanwiki::runtime::Signals::install() {
            Ok(signals) => Some(signals),
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    };
    let tracing_disabled = invocation.name == "benchmark"
        || ["plan", "no-phase-spans"].iter().any(|name| {
            invocation
                .args
                .try_get_one::<bool>(name)
                .ok()
                .flatten()
                .copied()
                .unwrap_or(false)
        });
    let settings = if tracing_disabled {
        None
    } else {
        orphanwiki::runtime::TraceSettings::from_environment()
    };
    let result = orphanwiki::runtime::with_tracing(settings, || {
        orphanwiki::runtime::phase(&format!("cli.{}", invocation.name), || {
            if invocation.name == "benchmark" {
                invocation.benchmark()
            } else {
                invocation.execute().map(|()| 0)
            }
        })
    });
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(
                orphanwiki::runtime::received_signal()
                    .map_or(1, |number| 128u8.saturating_add(number)),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commands_accept_the_existing_pipeline_arguments() {
        for arguments in [
            vec![
                "fetch-dumps",
                "--sql-release",
                "20261001",
                "--history-release",
                "2026-09",
                "--languages",
                "lmo",
                "vec",
                "--plan",
            ],
            vec!["fetch-content", "--content-release", "2026-10-01", "--plan"],
            vec![
                "import-dumps",
                "--manifest",
                "manifest.json",
                "--history-output",
                "history.json",
            ],
            vec![
                "import-history",
                "--manifest",
                "manifest.json",
                "--history-cache-mib",
                "1024",
                "--history-commit-rows",
                "100000",
            ],
            vec![
                "import-history",
                "--sql-release",
                "20261001",
                "--history-release",
                "2026-09",
            ],
            vec!["quality", "--content-release", "2026-10-01"],
            vec!["refresh-measures", "--languages", "vec", "lmo"],
            vec![
                "topics",
                "--batch-size",
                "10000",
                "--time-budget-seconds",
                "604800",
            ],
            vec![
                "analyze",
                "vec-raw.json",
                "--history",
                "vec-history.json",
                "--quality",
                "vec-quality.json",
            ],
            vec![
                "compare",
                "vec-raw.json",
                "lmo-raw.json",
                "--topics",
                "vec-topics.json",
                "lmo-topics.json",
                "--histories",
            ],
            vec!["dashboard", "--data-dir", "web/data"],
        ] {
            cli()
                .try_get_matches_from(std::iter::once("orphanwiki").chain(arguments))
                .unwrap();
        }
    }

    #[test]
    fn argument_errors_are_distinct_from_backend_errors() {
        for arguments in [
            vec![],
            vec!["unknown"],
            vec!["analyze"],
            vec!["fetch-dumps"],
            vec!["topics", "--batch-size", "0"],
            vec!["topics", "--batch-size", "10001"],
            vec!["topics", "--time-budget-seconds", "NaN"],
            vec!["topics", "--time-budget-seconds", "inf"],
            vec!["topics", "--time-budget-seconds", "0"],
            vec!["import-history", "--manifest", "x", "--languages", "vec"],
            vec![
                "import-history",
                "--manifest",
                "x",
                "--sql-release",
                "20261001",
            ],
            vec![
                "import-history",
                "--sql-release",
                "20261001",
                "--history-release",
                "2026-09",
                "--output",
                "x",
            ],
            vec![
                "import-dumps",
                "--manifest",
                "x",
                "--history-cache-mib",
                "65537",
            ],
            vec![
                "import-dumps",
                "--manifest",
                "x",
                "--history-commit-rows",
                "0",
            ],
        ] {
            let error = cli()
                .try_get_matches_from(std::iter::once("orphanwiki").chain(arguments))
                .unwrap_err();
            assert_eq!(error.exit_code(), 2);
        }
        assert_eq!(
            cli()
                .try_get_matches_from(["orphanwiki", "--help"])
                .unwrap_err()
                .exit_code(),
            0
        );
    }

    #[test]
    fn complete_language_pool_is_validated_before_selecting_a_subset() {
        let pool = orphanwiki::configured_languages(&json!({"version":1,"languages":["vec","lmo","be-tarask"],"databases":{"lmo":"customwiki","be-tarask":"be_x_oldwiki"}})).unwrap();
        assert_eq!(pool[2].1, "be_x_oldwiki");
        assert_eq!(
            orphanwiki::select_languages(&pool, Some(&["lmo".into(), "vec".into()])).unwrap(),
            pool[..2]
        );
        for selection in [vec![], vec!["en".into()], vec!["vec".into(), "vec".into()]] {
            assert!(orphanwiki::select_languages(&pool, Some(&selection)).is_err());
        }
        for config in [
            json!({"version":1,"languages":["vec","lmo"],"databases":{"lmo":"bad"}}),
            json!({"version":1,"languages":["vec"],"databases":{"en":"enwiki"}}),
            json!({"version":1,"languages":["vec","vec"]}),
            json!({"version":1,"languages":["all"]}),
            json!({"version":1,"languages":["vec"],"sampling":10}),
            json!({"version":1,"languages":[]}),
        ] {
            assert!(orphanwiki::configured_languages(&config).is_err());
        }
        for language in ["", "ALL", "a/b", "é", "all"] {
            assert!(orphanwiki::wiki_database(language, None).is_err());
        }
        assert_eq!(
            orphanwiki::wiki_database("roa-tara", None).unwrap(),
            "roa_tarawiki"
        );
        assert!(orphanwiki::wiki_database("vec", Some("vecwiki_p")).is_err());
    }
}
