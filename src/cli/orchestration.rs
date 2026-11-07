use super::*;

impl Invocation {
    /// Reexec before starting threads, tracing or collection writes. The child
    /// verifies the real kernel ceiling again; the marker only prevents loops.
    pub(crate) fn limit_pipeline_memory(&self, original_directory: &Path) -> Result<(), String> {
        if self.name != "pipeline" || self.args.get_flag("plan") {
            return Ok(());
        }
        let maximum = *self
            .args
            .get_one::<u64>("memory-limit-bytes")
            .ok_or("Limite RAM mancante")?;
        let (group, current) = orphanwiki::runtime::memory_ceiling()?;
        if current.is_some_and(|value| value <= maximum) {
            eprintln!(
                "Limite RAM complessivo verificato: {} byte; cgroup {}",
                current.unwrap_or(maximum),
                group.display()
            );
            return Ok(());
        }
        if std::env::var_os("ORPHANWIKI_MEMORY_SCOPE").is_some() {
            return orphanwiki::runtime::require_memory_ceiling(maximum);
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::{fs::MetadataExt, process::CommandExt};
            let page = rustix::param::page_size() as u64;
            let kernel_maximum = maximum - maximum % page;
            let mut command = std::process::Command::new("systemd-run");
            if std::fs::metadata("/proc/self")
                .map_err(|e| e.to_string())?
                .uid()
                != 0
            {
                command.arg("--user");
            }
            command
                .args(["--scope", "--quiet", "--property=MemoryAccounting=yes"])
                .arg(format!("--property=MemoryMax={kernel_maximum}"))
                .args([
                    "--property=MemorySwapMax=0",
                    "--property=OOMPolicy=kill",
                    "--",
                ])
                .arg(std::env::current_exe().map_err(|e| e.to_string())?)
                .args(std::env::args_os().skip(1))
                .env("ORPHANWIKI_MEMORY_SCOPE", "1")
                .current_dir(original_directory);
            eprintln!("Avvio pipeline in un gruppo con RAM massima {maximum} byte, inclusi tutti i worker; swap disabilitata");
            Err(format!("Impossibile applicare il limite RAM tramite systemd-run: {}. Avviare in un servizio/cgroup con MemoryMax={maximum}; nessuna raccolta avviata", command.exec()))
        }
        #[cfg(not(target_os = "linux"))]
        Err("Il limite RAM complessivo richiede Linux con cgroup v2".into())
    }

    pub(super) fn run_pipeline(&self) -> Result<(), String> {
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
        orphanwiki::pipeline::run(orphanwiki::pipeline::PipelineOptions {
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
            dump_mirror: self
                .args
                .get_one::<String>("dump-mirror")
                .map(|p| self.root.join(p)),
            import_dir: self.path("import-dir")?,
            output_dir: self.path("output-dir")?,
            collection_database: Some(
                self.args
                    .get_one::<String>("collection-database")
                    .map(|p| self.root.join(p))
                    .unwrap_or(self.path("output-dir")?.join("collection.sqlite")),
            ),
            work_dir,
            log_dir: self.path("log-dir")?,
            model: self.path("model")?,
            model_config: self.path("model-config")?,
            topic_budget_seconds: decimal("topic-budget-seconds")?,
            topic_batch_size: integer("topic-batch-size")?,
            history_cache_mib: integer("history-cache-mib")?,
            history_commit_rows: integer("history-commit-rows")?,
            memory_limit_bytes: *self
                .args
                .get_one::<u64>("memory-limit-bytes")
                .ok_or("Limite RAM mancante")?,
            metrics_interval: decimal("metrics-interval")?,
            no_phase_spans: self.args.get_flag("no-phase-spans"),
            force: self.args.get_flag("force"),
            plan: self.args.get_flag("plan"),
        })
        .map_err(|e| e.to_string())
    }
    pub(crate) fn benchmark(&self) -> Result<u8, String> {
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
}
