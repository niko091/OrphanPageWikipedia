use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    analyze_imported_graph, analyze_memory_graph, canonical_json_sha256, export_csv, file_sha256,
    file_signature, first_metadata, prepare_dashboard_exports, resolved_path, topic_time,
    write_aggregate_summary, write_atomic, AnalysisOptions, DumpClient, WikimediaHttp,
};

#[cfg(feature = "cli")]
pub(super) const TRANSPORT_RULE: &str = "dashboard_transports_v3";

#[cfg(feature = "cli")]
pub(super) struct PreparedDirectory {
    pub(super) path: std::path::PathBuf,
    pub(super) complete: bool,
}

#[cfg(feature = "cli")]
impl PreparedDirectory {
    pub(super) fn new(path: &std::path::Path) -> DumpResult<Self> {
        std::fs::create_dir_all(
            path.parent()
                .ok_or("Directory di preparazione non valida")?,
        )?;
        // Never adopt or remove a preexisting directory, including empty ones.
        std::fs::create_dir(path)?;
        Ok(Self {
            path: path.into(),
            complete: false,
        })
    }
}

#[cfg(feature = "cli")]
impl Drop for PreparedDirectory {
    fn drop(&mut self) {
        if !self.complete {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(feature = "cli")]
pub(super) fn export_directory(
    parent: &std::path::Path,
) -> DumpResult<(PreparedDirectory, String)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    loop {
        crate::runtime::check_cancelled()?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
        let generation = canonical_json_sha256(&serde_json::json!([
            now.as_secs(),
            now.subsec_nanos(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ]))?[..32]
            .to_owned();
        let path = parent.join(format!("orphanwiki-export-{generation}"));
        match PreparedDirectory::new(&path) {
            Ok(directory) => return Ok((directory, generation)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                continue
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(feature = "cli")]
pub(super) struct PublicationLock {
    _file: std::fs::File,
}

#[cfg(feature = "cli")]
impl PublicationLock {
    pub(super) fn acquire(destination: &std::path::Path) -> DumpResult<Self> {
        let identity = canonical_json_sha256(&serde_json::json!(resolved_path(destination)?
            .to_str()
            .ok_or("Percorso non UTF-8")?))?;
        let path = std::env::temp_dir().join(format!("orphanwiki-publish-{identity}.lock"));
        Self::acquire_file(&path)
    }

    pub(super) fn acquire_file(path: &std::path::Path) -> DumpResult<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options.open(path)?;
        file.try_lock()
            .map_err(|error| format!("Un altro writer è attivo: {error}"))?;
        Ok(Self { _file: file })
    }
}

#[cfg(feature = "cli")]
pub(super) fn publication_walk(
    prepared: &std::path::Path,
    destination: &std::path::Path,
    relative: &std::path::Path,
    move_files: bool,
    rename: &mut impl FnMut(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
) -> DumpResult<()> {
    for entry in std::fs::read_dir(prepared.join(relative))? {
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        if relative == std::path::Path::new("manifest.json") {
            continue;
        }
        let target = destination.join(&relative);
        let kind = entry.file_type()?;
        if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
            return Err("Prodotto di pubblicazione non regolare".into());
        }
        if !move_files && target.try_exists()? {
            return Err(format!("La pubblicazione sovrascriverebbe {}", target.display()).into());
        }
        if kind.is_dir() {
            if move_files {
                std::fs::create_dir(&target)?;
            }
            publication_walk(prepared, destination, &relative, move_files, rename)?;
        } else if move_files {
            rename(&entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(feature = "cli")]
pub(super) fn publish_prepared(
    prepared: &std::path::Path,
    destination: &std::path::Path,
    original_manifest: Option<&[u8]>,
    reused: &[ReusedExport],
    mut rename: impl FnMut(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
) -> DumpResult<()> {
    crate::runtime::phase("export.publish", || {
        crate::runtime::check_cancelled()?;
        let _lock = PublicationLock::acquire(destination)?;
        let manifest = destination.join("manifest.json");
        let current = match std::fs::read(&manifest) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if current.as_deref() != original_manifest {
            return Err("Manifest modificato da un altro writer".into());
        }
        for reused in reused {
            if file_signature(&reused.path)? != reused.signature
                || file_sha256(&reused.path)? != reused.checksum
            {
                return Err("Export riutilizzato modificato da un altro writer".into());
            }
        }
        if !prepared.join("manifest.json").is_file() {
            return Err("Manifest preparato mancante".into());
        }
        publication_walk(
            prepared,
            destination,
            std::path::Path::new(""),
            false,
            &mut rename,
        )?;
        std::fs::create_dir_all(destination)?;
        publication_walk(
            prepared,
            destination,
            std::path::Path::new(""),
            true,
            &mut rename,
        )?;
        rename(&prepared.join("manifest.json"), &manifest)?;
        Ok(())
    })
}

#[cfg(feature = "cli")]
pub(crate) fn dataset_filename(value: &serde_json::Value) -> DumpResult<&str> {
    let name = value.as_str().ok_or("Nome dataset non valido")?;
    if !name.ends_with(".json")
        || name.len() <= 5
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    {
        return Err("Nome dataset non valido".into());
    }
    Ok(name)
}

#[cfg(feature = "cli")]
pub(super) struct ReusedExport {
    pub(super) path: std::path::PathBuf,
    pub(super) checksum: String,
    pub(super) signature: (u64, i64),
}

#[cfg(feature = "cli")]
pub(super) fn reuse_export(
    source: &std::path::Path,
    target: &std::path::Path,
    reused: &mut Vec<ReusedExport>,
) -> DumpResult<bool> {
    if !target.try_exists()? {
        return Ok(false);
    }
    let signature = file_signature(target)?;
    let checksum = file_sha256(source)?;
    if file_sha256(target)? != checksum || file_signature(target)? != signature {
        return Err(format!("Export già esistente con contenuto diverso: {}. Usare un nome distinto per conservare la generazione approvata",target.display()).into());
    }
    reused.push(ReusedExport {
        path: target.into(),
        checksum,
        signature,
    });
    std::fs::remove_file(source)?;
    Ok(true)
}

#[cfg(feature = "cli")]
pub(super) fn manifest_entry(
    metadata: &serde_json::Value,
    name: &str,
    transports: Option<&serde_json::Value>,
) -> DumpResult<serde_json::Value> {
    use serde_json::json;
    dataset_filename(&json!(name))?;
    let mut entry = json!({"file":name,"language":metadata["language"],"date":metadata["finished_at"],
        "published_at":topic_time(),"demo":metadata.get("demo").cloned().unwrap_or(json!(false)),
        "kind":metadata.get("kind").cloned().unwrap_or(json!("wiki"))});
    if let Some(transports) = transports {
        entry
            .as_object_mut()
            .ok_or("Voce manifest non valida")?
            .extend(
                transports
                    .as_object()
                    .ok_or("Trasporti non validi")?
                    .clone(),
            );
    }
    Ok(entry)
}

#[cfg(feature = "cli")]
pub(super) struct PreparedMember {
    pub(super) metadata: serde_json::Value,
    pub(super) entry: serde_json::Value,
    pub(super) reused: Vec<ReusedExport>,
}

/// Prepare a member using the final approved source's stat fingerprint when
/// its canonical bytes are reused. A temporary copy has a different mtime and
/// cannot identify the previously approved file in prepared summary checks.
#[cfg(feature = "cli")]
pub(super) fn prepare_member(
    prepared: &PreparedDirectory,
    output: &std::path::Path,
    generation: &str,
) -> DumpResult<PreparedMember> {
    use serde_json::json;
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Nome output non valido")?;
    dataset_filename(&json!(name))?;
    let source = prepared.path.join(name);
    let mut reused = Vec::new();
    let canonical = if reuse_export(&source, output, &mut reused)? {
        output
    } else {
        source.as_path()
    };
    let metadata = first_metadata(canonical)?;
    let csv = source.with_extension("csv");
    export_csv(canonical, &csv)?;
    reuse_export(&csv, &output.with_extension("csv"), &mut reused)?;
    let transports = prepared.path.join("transports");
    let names = prepare_dashboard_exports(canonical, &transports, None, Some(generation))?;
    for name in names.as_object().ok_or("Trasporti non validi")?.values() {
        let name = name.as_str().ok_or("Nome trasporto mancante")?;
        std::fs::rename(transports.join(name), prepared.path.join(name))?;
    }
    std::fs::remove_dir(transports)?;
    Ok(PreparedMember {
        entry: manifest_entry(&metadata, name, Some(&names))?,
        metadata,
        reused,
    })
}

/// Publish a prepared member with its CSV and every dashboard transport. A
/// changed scientific result requires a distinct filename; manifest approval
/// follows all moves and rechecks both bytes and fingerprints of reused files.
#[cfg(feature = "cli")]
pub(super) fn publish_member(
    prepared: &PreparedDirectory,
    output: &std::path::Path,
    generation: &str,
    original_manifest: Option<&[u8]>,
) -> DumpResult<serde_json::Value> {
    use serde_json::{json, Value};
    let mut manifest: Vec<Value> = original_manifest
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_default();
    let mut files = std::collections::HashSet::new();
    for entry in &manifest {
        if !files.insert(dataset_filename(&entry["file"])?.to_owned()) {
            return Err("Dataset duplicato nel manifest".into());
        }
    }
    let member = prepare_member(prepared, output, generation)?;
    manifest.retain(|entry| {
        entry["file"] != member.entry["file"]
            && (member.metadata["demo"] == true || entry["demo"] != true)
    });
    manifest.push(member.entry);
    DumpClient::<WikimediaHttp>::write_json(
        &prepared.path.join("manifest.json"),
        &json!(manifest),
    )?;
    publish_prepared(
        &prepared.path,
        output.parent().ok_or("Directory output mancante")?,
        original_manifest,
        &member.reused,
        |from, to| std::fs::rename(from, to),
    )?;
    Ok(member.metadata)
}

/// Analyze a frozen input and publish one complete member without Python.
/// The 128 MiB switch preserves the prior operational path and checksum rules.
#[cfg(feature = "cli")]
pub fn analyze_file(
    raw: &std::path::Path,
    output: &std::path::Path,
    options: &AnalysisOptions<'_>,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("analysis.single", || {
        crate::runtime::check_cancelled()?;
        use std::io::{BufReader, BufWriter, Write};
        if !(1..=512).contains(&options.batch_size) {
            return Err("Dimensione blocco non valida".into());
        }
        let output = resolved_path(output)?;
        let folder = output.parent().ok_or("Directory output mancante")?;
        let filename = output
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("Nome output non valido")?;
        dataset_filename(&serde_json::json!(filename))?;
        let inputs = [Some(raw), options.history, options.quality, options.topics];
        let signatures = inputs
            .into_iter()
            .flatten()
            .map(|p| Ok((p, file_signature(p)?)))
            .collect::<DumpResult<Vec<_>>>()?;
        for (input, _) in &signatures {
            let input = resolved_path(input)?;
            if input == output
                || input == output.with_extension("csv")
                || input == folder.join("manifest.json")
            {
                return Err("Output sovrapposto a un input".into());
            }
        }
        let manifest = match std::fs::read(folder.join("manifest.json")) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let (prepared, generation) =
            export_directory(folder.parent().ok_or("Directory dati non valida")?)?;
        let source = prepared.path.join(filename);
        let meta = first_metadata(raw)?;
        if file_signature(raw)?.0 >= 128 * 1024 * 1024
            && meta["source"]
                .as_str()
                .is_some_and(|s| s.starts_with("Wikimedia SQL dumps / "))
        {
            analyze_imported_graph(raw, &source, options)?;
        } else {
            let read = |path: &std::path::Path| -> DumpResult<serde_json::Value> {
                Ok(serde_json::from_reader(BufReader::new(
                    std::fs::File::open(path)?,
                ))?)
            };
            let graph = read(raw)?;
            let history = options.history.map(read).transpose()?;
            let quality = options.quality.map(read).transpose()?;
            let topics = options.topics.map(read).transpose()?;
            let result = analyze_memory_graph(
                &graph,
                options.rules,
                history.as_ref(),
                quality.as_ref(),
                topics.as_ref(),
                false,
            )?;
            write_atomic(&source, |file| {
                let mut writer = BufWriter::new(file);
                serde_json::to_writer(&mut writer, &result)?;
                writer.flush()?;
                Ok(())
            })?;
        }
        for (path, signature) in signatures {
            if file_signature(path)? != signature {
                return Err("Input modificato durante l'analisi".into());
            }
        }
        publish_member(&prepared, &output, &generation, manifest.as_deref())
    })
}

/// Regenerate every approved member's transports, retaining aggregate entries
/// and canonical JSON/CSV. No approved file is overwritten; the manifest moves
/// only after every new file and empty transport directory is installed.
#[cfg(feature = "cli")]
pub fn rebuild_dashboard(folder: &std::path::Path) -> DumpResult<()> {
    crate::runtime::phase("dashboard.convert", || {
        crate::runtime::check_cancelled()?;
        use serde_json::Value;
        let original = std::fs::read(folder.join("manifest.json"))?;
        let mut manifest: Vec<Value> = serde_json::from_slice(&original)?;
        if manifest.is_empty() {
            return Err("Manifest non valido".into());
        }
        let mut files = std::collections::HashSet::new();
        for entry in &manifest {
            let name = dataset_filename(&entry["file"])?;
            if !entry.is_object() || !files.insert(name.to_owned()) {
                return Err("Dataset duplicato nel manifest".into());
            }
            if entry["kind"] != "aggregate" {
                crate::wiki_database(entry["language"].as_str().ok_or("Lingua mancante")?, None)?;
            }
        }
        let (prepared, generation) =
            export_directory(folder.parent().ok_or("Directory dati non valida")?)?;
        let mut sources = Vec::new();
        for (position, entry) in manifest.iter_mut().enumerate() {
            if entry["kind"] == "aggregate" {
                continue;
            }
            let source = folder.join(dataset_filename(&entry["file"])?);
            sources.push((source.clone(), file_signature(&source)?));
            let member = prepared.path.join(format!("member-{position}"));
            let names =
                prepare_dashboard_exports(&source, &member, Some(entry), Some(&generation))?;
            for name in names.as_object().ok_or("Trasporti non validi")?.values() {
                let name = name.as_str().ok_or("Nome trasporto non valido")?;
                if files.contains(name) || name == "manifest.json" {
                    return Err("Trasporto sovrapposto a un dataset".into());
                }
                std::fs::rename(member.join(name), prepared.path.join(name))?;
            }
            std::fs::remove_dir(member)?;
            entry
                .as_object_mut()
                .ok_or("Voce del manifest non valida")?
                .extend(names.as_object().ok_or("Trasporti non validi")?.clone());
        }
        for entry in manifest.iter_mut().filter(|e| e["kind"] == "aggregate") {
            let name = dataset_filename(&entry["file"])?;
            let source = folder.join(name);
            let index: Value = serde_json::from_reader(std::fs::File::open(&source)?)?;
            sources.push((source.clone(), file_signature(&source)?));
            let mut members = index["members"]
                .as_array()
                .filter(|members| !members.is_empty())
                .ok_or("Membri aggregati mancanti")?
                .clone();
            for member in &mut members {
                member["path"] = serde_json::json!(folder.join(dataset_filename(&member["file"])?));
            }
            let stem = source
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or("Nome aggregato non valido")?;
            let summary = format!("{stem}-{generation}-summary.json");
            let creators = format!("{stem}-{generation}-creators.json");
            write_aggregate_summary(
                &members,
                &prepared.path.join(&summary),
                &index["metadata"],
                true,
            )?;
            entry["summary_file"] = serde_json::json!(summary);
            entry["creator_index_file"] = serde_json::json!(creators);
        }
        for (source, signature) in sources {
            if file_signature(&source)? != signature {
                return Err("Dataset modificato durante la conversione".into());
            }
        }
        DumpClient::<WikimediaHttp>::write_json(
            &prepared.path.join("manifest.json"),
            &serde_json::json!(manifest),
        )?;
        publish_prepared(&prepared.path, folder, Some(&original), &[], |from, to| {
            std::fs::rename(from, to)
        })
    })
}
