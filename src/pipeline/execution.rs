use super::*;

pub(super) fn timestamp() -> String {
    let now: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
    now.to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

pub(super) fn disk_usage(path: &Path) -> Value {
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

pub(super) fn host_inventory(disks: &BTreeMap<&str, PathBuf>) -> Value {
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

pub(super) struct Reporting {
    pub(super) log: File,
    pub(super) events: File,
    pub(super) active: Option<String>,
    pub(super) peak_rss: Option<u64>,
    pub(super) step_peak_rss: Option<u64>,
    pub(super) last_cpu: Option<(Instant, f64)>,
}

#[derive(Clone)]
pub(super) struct Reporter {
    pub(super) shared: std::sync::Arc<std::sync::Mutex<Reporting>>,
    pub(super) run_id: String,
    pub(super) started: Instant,
    pub(super) disks: BTreeMap<&'static str, PathBuf>,
    pub(super) include_controller_memory: bool,
}

impl Reporter {
    pub(super) fn bytes(&self, bytes: &[u8]) -> PipelineResult<()> {
        let mut state = self.shared.lock().map_err(|_| "Logger non disponibile")?;
        state.log.write_all(bytes)?;
        std::io::stdout().lock().write_all(bytes)?;
        Ok(())
    }

    pub(super) fn log(&self, text: &str) -> PipelineResult<()> {
        self.bytes(format!("{} {text}\n", timestamp()).as_bytes())
    }

    pub(super) fn event(&self, event: &str, fields: Value) -> PipelineResult<()> {
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

    pub(super) fn sample(&self) -> PipelineResult<()> {
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

pub(super) struct Monitor {
    pub(super) stop: mpsc::Sender<()>,
    pub(super) thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl Monitor {
    pub(super) fn start(reporter: Reporter, interval: f64) -> Self {
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

    pub(super) fn finish(&mut self) -> Option<String> {
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

pub(super) struct RunningChild {
    pub(super) child: std::process::Child,
    pub(super) output: Option<mpsc::Receiver<std::io::Result<Vec<u8>>>>,
    pub(super) reader: Option<std::thread::JoinHandle<()>>,
}

impl RunningChild {
    pub(super) fn start(mut command: Command) -> PipelineResult<Self> {
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

    pub(super) fn terminate(&mut self, force: bool) {
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

pub(super) struct RecordingOptions {
    pub(super) root: PathBuf,
    pub(super) prefix: PathBuf,
    pub(super) interval: f64,
    pub(super) phase_spans: bool,
    pub(super) include_controller_memory: bool,
    pub(super) disks: BTreeMap<&'static str, PathBuf>,
}

pub(super) struct Runner {
    pub(super) executable: PathBuf,
    pub(super) root: PathBuf,
    pub(super) reporter: Option<Reporter>,
    pub(super) monitor: Option<Monitor>,
    pub(super) report_path: Option<PathBuf>,
    pub(super) trace: Option<runtime::TraceSettings>,
    pub(super) metadata: Value,
    pub(super) steps: Vec<Value>,
}

impl Runner {
    pub(super) fn new(options: &PipelineOptions) -> PipelineResult<Self> {
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

    pub(super) fn record(options: RecordingOptions, mut metadata: Value) -> PipelineResult<Self> {
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

    pub(super) fn finish(&mut self, result: &PipelineResult<()>) -> PipelineResult<()> {
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

pub(super) fn write_operational_report(path: &Path, report: &Value) -> PipelineResult<()> {
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

pub(super) fn numeric_difference(after: &Value, before: &Value, key: &str) -> Value {
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
    pub(super) fn execute_command(
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
