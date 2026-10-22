//! Operational tracing, cancellation and Linux resource sampling.
//! None of these values participates in scientific records or checkpoints.
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

static SIGNAL: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn signal() -> &'static Arc<AtomicUsize> {
    SIGNAL.get_or_init(|| Arc::new(AtomicUsize::new(0)))
}

/// Signal handlers only set an atomic; bounded work checks it on its own thread.
pub struct Signals(Vec<signal_hook::SigId>);

impl Signals {
    pub fn install() -> std::io::Result<Self> {
        let mut handlers = Self(Vec::new());
        for number in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            handlers.0.push(signal_hook::flag::register_usize(
                number,
                signal().clone(),
                number as usize,
            )?);
        }
        Ok(handlers)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

pub fn received_signal() -> Option<u8> {
    u8::try_from(signal().load(Ordering::Relaxed))
        .ok()
        .filter(|n| *n > 0)
}

pub fn check_cancelled() -> Result<(), String> {
    if received_signal().is_some() {
        Err("Esecuzione interrotta".into())
    } else {
        Ok(())
    }
}

pub fn unique_id() -> String {
    use sha2::Digest;
    let value = format!(
        "{}:{}:{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    );
    format!("{:x}", sha2::Sha256::digest(value.as_bytes()))[..32].into()
}

pub fn monotonic_ns() -> u64 {
    #[cfg(unix)]
    {
        let value = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        (value.tv_sec as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(value.tv_nsec as u64)
    }
    #[cfg(not(unix))]
    {
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

#[derive(Clone)]
pub struct TraceSettings {
    pub file: PathBuf,
    pub run_id: String,
    pub parent_id: Option<String>,
}

impl TraceSettings {
    pub fn from_environment() -> Option<Self> {
        let file = std::env::var_os("ORPHANWIKI_SPANS_FILE").filter(|v| !v.is_empty())?;
        static LOCAL_RUN: OnceLock<String> = OnceLock::new();
        Some(Self {
            file: file.into(),
            run_id: std::env::var("ORPHANWIKI_RUN_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| LOCAL_RUN.get_or_init(unique_id).clone()),
            parent_id: std::env::var("ORPHANWIKI_PARENT_SPAN").ok(),
        })
    }
}

struct ActiveSpan {
    settings: TraceSettings,
    record: Value,
}
thread_local! {
    static SETTINGS: RefCell<Option<Option<TraceSettings>>> = const { RefCell::new(None) };
    static SPANS: RefCell<Vec<Rc<RefCell<ActiveSpan>>>> = const { RefCell::new(Vec::new()) };
}

/// An explicit `None` also suppresses inherited tracing for read-only plans.
pub fn with_tracing<T>(settings: Option<TraceSettings>, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<Option<TraceSettings>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SETTINGS.with(|s| *s.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(SETTINGS.with(|s| s.replace(Some(settings))));
    work()
}

fn emit(path: &Path, value: &Value) {
    let Ok(mut bytes) = serde_json::to_vec(value) else {
        return;
    };
    bytes.push(b'\n');
    // Diagnostic errors never change collection, reuse or publication decisions.
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(&bytes);
    }
}

struct Span(Option<Rc<RefCell<ActiveSpan>>>);

impl Span {
    fn start(name: &str) -> Self {
        let settings = SETTINGS
            .with(|s| s.borrow().clone())
            .unwrap_or_else(TraceSettings::from_environment);
        let Some(settings) = settings else {
            return Self(None);
        };
        let parent = SPANS
            .with(|s| s.borrow().last().cloned())
            .and_then(|s| {
                let s = s.borrow();
                (s.settings.run_id == settings.run_id && s.settings.file == settings.file)
                    .then(|| s.record["span_id"].as_str().map(str::to_owned))
                    .flatten()
            })
            .or_else(|| settings.parent_id.clone());
        let now = monotonic_ns();
        let record = json!({"run_id":settings.run_id,"span_id":unique_id(),"parent_id":parent,
            "pid":std::process::id(),"name":name,"rows":null,"bytes":null,
            "event":"span_start","monotonic_ns":now,"started_ns":now});
        emit(&settings.file, &record);
        let span = Rc::new(RefCell::new(ActiveSpan { settings, record }));
        SPANS.with(|s| s.borrow_mut().push(span.clone()));
        Self(Some(span))
    }

    fn finish(&mut self, status: &str) {
        let Some(span) = self.0.take() else {
            return;
        };
        SPANS.with(|s| {
            s.borrow_mut().retain(|value| !Rc::ptr_eq(value, &span));
        });
        let mut span = span.borrow_mut();
        let now = monotonic_ns();
        let started = span.record["started_ns"].as_u64().unwrap_or(now);
        span.record["event"] = json!("span_end");
        span.record["monotonic_ns"] = json!(now);
        span.record["finished_ns"] = json!(now);
        span.record["wall_seconds"] = json!(now.saturating_sub(started) as f64 / 1e9);
        span.record["status"] = json!(status);
        span.record["error"] = if status == "completed" {
            Value::Null
        } else {
            json!(status)
        };
        emit(&span.settings.file, &span.record);
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        self.finish(if std::thread::panicking() {
            "failed"
        } else {
            "interrupted"
        });
    }
}

pub fn phase<T, E>(name: &str, work: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let mut span = Span::start(name);
    let result = work();
    span.finish(if result.is_ok() {
        "completed"
    } else if received_signal().is_some() {
        "interrupted"
    } else {
        "failed"
    });
    result
}

pub fn phase_counts(rows: Option<u64>, bytes: Option<u64>) {
    SPANS.with(|s| {
        if let Some(span) = s.borrow().last() {
            let mut span = span.borrow_mut();
            span.record["rows"] = json!(rows);
            span.record["bytes"] = json!(bytes);
        }
    });
}

pub(crate) fn phase_tag(key: &str, value: Value) {
    SPANS.with(|spans| {
        if let Some(span) = spans.borrow().last() {
            span.borrow_mut().record[key] = value;
        }
    });
}

/// Copy these keys onto child commands; the process environment stays untouched.
pub fn child_environment() -> BTreeMap<&'static str, String> {
    let active = SPANS.with(|s| s.borrow().last().cloned());
    let settings = SETTINGS.with(|s| s.borrow().clone()).unwrap_or_else(|| {
        active
            .as_ref()
            .map(|s| s.borrow().settings.clone())
            .or_else(TraceSettings::from_environment)
    });
    let Some(settings) = settings else {
        return BTreeMap::new();
    };
    let active = active.filter(|span| {
        let span = span.borrow();
        span.settings.file == settings.file && span.settings.run_id == settings.run_id
    });
    let mut values = BTreeMap::from([
        (
            "ORPHANWIKI_SPANS_FILE",
            settings.file.to_string_lossy().into_owned(),
        ),
        ("ORPHANWIKI_RUN_ID", settings.run_id),
    ]);
    if let Some(parent) = active
        .and_then(|s| s.borrow().record["span_id"].as_str().map(str::to_owned))
        .or(settings.parent_id)
    {
        values.insert("ORPHANWIKI_PARENT_SPAN", parent);
    }
    values
}

pub fn configure_child_tracing(command: &mut std::process::Command) {
    for key in [
        "ORPHANWIKI_SPANS_FILE",
        "ORPHANWIKI_RUN_ID",
        "ORPHANWIKI_PARENT_SPAN",
    ] {
        command.env_remove(key);
    }
    command.envs(child_environment());
}

/// Close unmatched worker intervals after their process group has exited.
/// The end is a detection bound capped by completed ancestors, never a guessed death time.
pub fn reconcile_spans(path: &Path, offset: u64, returncode: i32) -> std::io::Result<usize> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut pending = BTreeMap::<String, Value>::new();
    for line in BufReader::new(file).lines() {
        let Ok(record) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let Some(id) = record["span_id"].as_str() else {
            continue;
        };
        if record["event"] == "span_start" {
            pending.insert(id.into(), record);
        } else if record["event"] == "span_end" {
            pending.remove(id);
            for child in pending.values_mut().filter(|c| c["parent_id"] == id) {
                child["_end_bound_ns"] = record["finished_ns"].clone();
            }
        }
    }
    let detected = monotonic_ns();
    if !pending.is_empty() {
        OpenOptions::new()
            .append(true)
            .open(path)?
            .write_all(b"\n")?;
    }
    for start in pending.values() {
        let mut bound = detected;
        let mut ancestor = Some(start);
        let mut seen = BTreeSet::new();
        while let Some(record) = ancestor {
            bound = bound.min(record["_end_bound_ns"].as_u64().unwrap_or(detected));
            let Some(parent) = record["parent_id"].as_str().filter(|id| seen.insert(*id)) else {
                break;
            };
            ancestor = pending.get(parent);
        }
        let mut record = start.clone();
        if let Some(fields) = record.as_object_mut() {
            fields.remove("_end_bound_ns");
        }
        let began = record["started_ns"].as_u64().unwrap_or(bound);
        let bound = bound.max(began);
        for (key, value) in [
            ("event", json!("span_end")),
            ("monotonic_ns", json!(detected)),
            ("finished_ns", json!(bound)),
            ("detected_ns", json!(detected)),
            (
                "wall_seconds",
                json!(bound.saturating_sub(began) as f64 / 1e9),
            ),
            ("status", json!("interrupted")),
            ("error", json!("worker_exit")),
            ("returncode", json!(returncode)),
            ("end_is_detection_bound", json!(true)),
        ] {
            record[key] = value;
        }
        emit(path, &record);
    }
    Ok(pending.len())
}

fn proc_values(path: &Path) -> Option<BTreeMap<String, String>> {
    Some(
        std::fs::read_to_string(path)
            .ok()?
            .lines()
            .filter_map(|s| s.split_once(':'))
            .map(|(key, value)| (key.into(), value.trim().into()))
            .collect(),
    )
}

fn kib(values: &BTreeMap<String, String>, key: &str) -> Option<u64> {
    values
        .get(key)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

/// Sample process-tree totals. Missing Linux measurements stay null.
pub fn resources(pid: u32, include_root_memory: bool) -> Value {
    let mut pending = vec![pid];
    let mut seen = BTreeSet::new();
    let (mut rss, mut swap, mut cpu): (Option<u64>, Option<u64>, Option<f64>) = (None, None, None);
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        let root = PathBuf::from(format!("/proc/{current}"));
        if let Some(status) = proc_values(&root.join("status")) {
            if include_root_memory || current != pid {
                if let Some(n) = kib(&status, "VmRSS") {
                    rss = Some(rss.unwrap_or(0).saturating_add(n));
                }
                if let Some(n) = kib(&status, "VmSwap") {
                    swap = Some(swap.unwrap_or(0).saturating_add(n));
                }
            }
        }
        #[cfg(unix)]
        if let Ok(stat) = std::fs::read_to_string(root.join("stat")) {
            if let Some((_, values)) = stat.rsplit_once(')') {
                let values = values
                    .split_whitespace()
                    .skip(11)
                    .take(4)
                    .map(str::parse::<u64>)
                    .collect::<Result<Vec<_>, _>>();
                if let Ok(values) = values {
                    if values.len() == 4 {
                        cpu = Some(
                            cpu.unwrap_or(0.0)
                                + values.into_iter().sum::<u64>() as f64
                                    / rustix::param::clock_ticks_per_second() as f64,
                        );
                    }
                }
            }
        }
        if let Ok(tasks) = std::fs::read_dir(root.join("task")) {
            for task in tasks.flatten() {
                if let Ok(children) = std::fs::read_to_string(task.path().join("children")) {
                    pending.extend(
                        children
                            .split_whitespace()
                            .filter_map(|s| s.parse::<u32>().ok()),
                    );
                }
            }
        }
    }
    let memory = proc_values(Path::new("/proc/meminfo"));
    let io = proc_values(Path::new(&format!("/proc/{pid}/io")));
    let counter = |name| {
        io.as_ref()
            .and_then(|v| v.get(name))
            .and_then(|s| s.parse::<u64>().ok())
    };
    let load = std::fs::read_to_string("/proc/loadavg").ok().and_then(|s| {
        s.split_whitespace()
            .take(3)
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
    });
    let pressure = ["cpu", "memory", "io"].map(|kind| {
        (
            kind,
            std::fs::read_to_string(format!("/proc/pressure/{kind}"))
                .ok()
                .map(|s| s.trim().to_owned()),
        )
    });
    json!({"tree_rss_bytes":rss,"tree_swap_bytes":swap,"tree_cpu_seconds":cpu,
        "host_available_memory_bytes":memory.as_ref().and_then(|v|kib(v,"MemAvailable")),
        "controller_reaped_read_bytes":counter("read_bytes"),"controller_reaped_write_bytes":counter("write_bytes"),
        "host_pressure":pressure.into_iter().collect::<BTreeMap<_,_>>(),"host_load_average":load})
}

/// Completed child CPU and filesystem counters, including their waited workers.
pub fn child_usage() -> Value {
    #[cfg(unix)]
    {
        use nix::sys::{
            resource::{getrusage, UsageWho},
            time::TimeValLike,
        };
        if let Ok(usage) = getrusage(UsageWho::RUSAGE_CHILDREN) {
            return json!({"user_cpu_seconds":usage.user_time().num_microseconds() as f64/1e6,
                "system_cpu_seconds":usage.system_time().num_microseconds() as f64/1e6,
                "filesystem_input_operations":usage.block_reads(),"filesystem_output_operations":usage.block_writes()});
        }
    }
    json!({"user_cpu_seconds":null,"system_cpu_seconds":null,"filesystem_input_operations":null,"filesystem_output_operations":null})
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Log(PathBuf);
    impl Log {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("orphanwiki-spans-{}.jsonl", unique_id())))
        }
        fn settings(&self) -> TraceSettings {
            TraceSettings {
                file: self.0.clone(),
                run_id: "isolated-test".into(),
                parent_id: Some("external-parent".into()),
            }
        }
        fn records(&self) -> Vec<Value> {
            std::fs::read_to_string(&self.0)
                .unwrap()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        }
    }
    impl Drop for Log {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn spans_keep_hierarchy_counters_inclusive_intervals_and_error_status() {
        let log = Log::new();
        with_tracing(Some(log.settings()), || {
            phase("parent", || {
                phase_counts(Some(9), Some(200));
                let environment = child_environment();
                assert_eq!(environment["ORPHANWIKI_RUN_ID"], "isolated-test");
                phase::<_, ()>("child", || {
                    phase_counts(Some(3), None);
                    Ok(())
                })
                .unwrap();
                phase::<(), _>("failed", || Err("test failure")).unwrap_err();
                Ok::<_, ()>(())
            })
            .unwrap();
        });
        let records = log.records();
        assert_eq!(records.len(), 6);
        let parent = &records[0];
        assert_eq!(parent["parent_id"], "external-parent");
        for child in [&records[1], &records[3]] {
            assert_eq!(child["parent_id"], parent["span_id"]);
        }
        assert_eq!(records[2]["rows"], 3);
        assert!(records[2]["bytes"].is_null());
        assert_eq!(records[4]["status"], "failed");
        assert_eq!(records[5]["status"], "completed");
        assert_eq!(records[5]["rows"], 9);
        for record in [&records[2], &records[4]] {
            assert!(
                record["started_ns"].as_u64().unwrap() >= parent["started_ns"].as_u64().unwrap()
            );
            assert!(
                record["finished_ns"].as_u64().unwrap()
                    <= records[5]["finished_ns"].as_u64().unwrap()
            );
        }
    }

    #[test]
    fn read_only_plans_suppress_inherited_tracing_without_touching_the_environment() {
        let log = Log::new();
        with_tracing(Some(log.settings()), || {
            phase::<_, ()>("parent", || {
                let before = log.records().len();
                with_tracing(None, || {
                    phase::<_, ()>("suppressed", || Ok(())).unwrap();
                    assert!(child_environment().is_empty());
                    let mut command = std::process::Command::new("unused-test-command");
                    command.env("ORPHANWIKI_PARENT_SPAN", "stale");
                    configure_child_tracing(&mut command);
                    assert!(command.get_envs().all(|(_, value)| value.is_none()));
                });
                assert_eq!(log.records().len(), before);
                Ok(())
            })
            .unwrap();
        });
        assert_eq!(log.records().len(), 2);
    }

    #[test]
    fn killed_workers_use_completed_ancestor_bounds_and_keep_detection_times() {
        let log = Log::new();
        let now = monotonic_ns();
        for (id, parent, start) in [
            ("a", Value::Null, now - 9000),
            ("b", json!("a"), now - 8000),
            ("c", json!("b"), now - 7000),
        ] {
            emit(
                &log.0,
                &json!({"event":"span_start","span_id":id,"parent_id":parent,"started_ns":start,"rows":7}),
            );
        }
        emit(
            &log.0,
            &json!({"event":"span_end","span_id":"a","finished_ns":now-5000}),
        );
        OpenOptions::new()
            .append(true)
            .open(&log.0)
            .unwrap()
            .write_all(b"{\"truncated\"")
            .unwrap();
        assert_eq!(reconcile_spans(&log.0, 0, -9).unwrap(), 2);
        let records = log.records();
        for record in records
            .iter()
            .filter(|r| r["end_is_detection_bound"] == true)
        {
            assert_eq!(record["finished_ns"], now - 5000);
            assert!(record["detected_ns"].as_u64().unwrap() >= now);
            assert_eq!(record["rows"], 7);
            assert_eq!(record["returncode"], -9);
        }
        assert_eq!(reconcile_spans(&log.0, 0, -9).unwrap(), 0);
    }

    #[test]
    fn missing_resources_stay_null_and_linux_samples_use_the_live_process_tree() {
        let absent = resources(u32::MAX, true);
        for key in [
            "tree_rss_bytes",
            "tree_swap_bytes",
            "tree_cpu_seconds",
            "controller_reaped_read_bytes",
            "controller_reaped_write_bytes",
        ] {
            assert!(absent[key].is_null(), "{key}");
        }
        #[cfg(target_os = "linux")]
        {
            let current = resources(std::process::id(), true);
            assert!(current["tree_rss_bytes"].as_u64().unwrap() > 0);
            assert!(current["host_available_memory_bytes"].as_u64().unwrap() > 0);
            assert!(current["tree_cpu_seconds"].as_f64().unwrap() >= 0.0);
            assert_eq!(current["host_load_average"].as_array().unwrap().len(), 3);
        }
    }
}
