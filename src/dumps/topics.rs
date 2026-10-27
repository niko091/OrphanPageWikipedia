use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    export_directory, file_sha256, file_signature, first_metadata, next_relation, python_json,
    resolved_path, sql_category_parents, sql_redirects, DumpClient, ImportedGraph,
    PreparedDirectory, RecordSpool, WikimediaHttp,
};

/// Complete native model response, before the strict scientific threshold.
#[cfg(feature = "cli")]
pub struct TopicPrediction {
    pub labels: Vec<String>,
    pub probabilities: Vec<f64>,
}

/// The inference boundary also permits deterministic synthetic test engines.
#[cfg(feature = "cli")]
pub trait TopicModel {
    fn labels(&self) -> Result<Vec<String>, String>;
    fn word_ids(&mut self, words: &[&str]) -> Result<Vec<i32>, String>;
    fn predict(&mut self, inputs: &[String]) -> Result<Vec<TopicPrediction>, String>;
}

/// An owned local fastText worker. Its executable is embedded at build time,
/// extracted into a private owned directory and removed after the child exits.
#[cfg(feature = "cli")]
pub(super) struct TopicWorker {
    child: std::process::Child,
    input: std::io::BufWriter<std::process::ChildStdin>,
    output: std::io::BufReader<std::process::ChildStdout>,
    _directory: PreparedDirectory,
}

#[cfg(feature = "cli")]
impl Drop for TopicWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(feature = "cli")]
impl TopicWorker {
    fn start(model: &std::path::Path) -> DumpResult<Self> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let (directory, _) = export_directory(&std::env::temp_dir())?;
        let path = directory.path.join(if cfg!(windows) {
            "fasttext.exe"
        } else {
            "fasttext"
        });
        let mut file = std::fs::File::create_new(&path)?;
        file.write_all(include_bytes!(env!("ORPHANWIKI_FASTTEXT_WORKER")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        drop(file);
        let mut command = Command::new(&path);
        crate::runtime::configure_child_tracing(&mut command);
        let mut child = command
            .arg(model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Pipe fastText mancanti".into());
        };
        Ok(Self {
            child,
            input: std::io::BufWriter::with_capacity(65536, input),
            output: std::io::BufReader::with_capacity(65536, output),
            _directory: directory,
        })
    }
    fn read_u32(&mut self) -> Result<u32, String> {
        use std::io::Read;
        let mut bytes = [0; 4];
        self.output
            .read_exact(&mut bytes)
            .map_err(|e| format!("Risposta fastText incompleta: {e}"))?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn read_string(&mut self, maximum: usize) -> Result<String, String> {
        use std::io::Read;
        let low = self.read_u32()?;
        let high = self.read_u32()?;
        let count =
            usize::try_from(u64::from(low) | (u64::from(high) << 32)).map_err(|e| e.to_string())?;
        if count > maximum {
            return Err("Risposta fastText fuori limite".into());
        }
        let mut bytes = vec![0; count];
        self.output
            .read_exact(&mut bytes)
            .map_err(|e| e.to_string())?;
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
    fn status(&mut self) -> Result<(), String> {
        match self.read_u32()? {
            0 => Ok(()),
            1 => Err(self.read_string(16384)?),
            _ => Err("Stato fastText non valido".into()),
        }
    }
    fn request(&mut self, operation: u8, inputs: &[&str]) -> Result<(), String> {
        use std::io::Write;
        crate::runtime::check_cancelled()?;
        if inputs.iter().any(|s| s.contains(['\n', '\0'])) {
            return Err("Il modello accetta righe prive di NUL o newline".into());
        }
        let count = u32::try_from(inputs.len()).map_err(|e| e.to_string())?;
        let mut write = || -> std::io::Result<()> {
            self.input.write_all(&[operation])?;
            self.input.write_all(&count.to_le_bytes())?;
            for input in inputs {
                self.input.write_all(&(input.len() as u64).to_le_bytes())?;
                self.input.write_all(input.as_bytes())?;
            }
            self.input.flush()
        };
        write().map_err(|e| format!("Invio a fastText fallito: {e}"))?;
        self.status()?;
        if self.read_u32()? != count {
            return Err("Risposta fastText con dimensione diversa".into());
        }
        Ok(())
    }
}

/// Checksum-verified fastText 0.9.3, isolated behind safe bounded I/O.
#[cfg(feature = "cli")]
pub struct NativeTopicModel {
    worker: TopicWorker,
    labels: Vec<String>,
}

#[cfg(feature = "cli")]
impl NativeTopicModel {
    pub fn load(path: &std::path::Path, spec: &serde_json::Value) -> DumpResult<Self> {
        crate::runtime::phase("topics.model_load", || {
            crate::runtime::check_cancelled()?;
            use sha2::Digest;
            use std::io::Read;
            validate_topic_spec(spec)?;
            let mut digest = sha2::Sha512::new();
            let mut file = std::fs::File::open(path)?;
            let mut block = [0; 1024 * 1024];
            loop {
                crate::runtime::check_cancelled()?;
                let size = file.read(&mut block)?;
                if size == 0 {
                    break;
                }
                digest.update(&block[..size]);
            }
            if spec["model_sha512"] != format!("{:x}", digest.finalize()) {
                return Err("Checksum dei pesi del modello non corrispondente".into());
            }
            let mut worker = TopicWorker::start(path)?;
            worker.status()?;
            if worker.read_u32()? != 64 {
                return Err("Tassonomia fastText incompleta".into());
            }
            let mut labels = Vec::with_capacity(64);
            for _ in 0..64 {
                labels.push(worker.read_string(1048576)?);
            }
            validate_topic_labels(&labels, spec)?;
            Ok(Self { worker, labels })
        })
    }
}

#[cfg(feature = "cli")]
impl TopicModel for NativeTopicModel {
    fn labels(&self) -> Result<Vec<String>, String> {
        Ok(self.labels.clone())
    }
    fn word_ids(&mut self, words: &[&str]) -> Result<Vec<i32>, String> {
        crate::runtime::phase("topics.vocabulary", || {
            crate::runtime::check_cancelled()?;
            let mut ids = Vec::with_capacity(words.len());
            for batch in words.chunks(4096) {
                self.worker.request(1, batch)?;
                for _ in batch {
                    ids.push(i32::from_le_bytes(self.worker.read_u32()?.to_le_bytes()));
                }
            }
            Ok(ids)
        })
    }
    fn predict(&mut self, inputs: &[String]) -> Result<Vec<TopicPrediction>, String> {
        crate::runtime::phase("topics.fasttext_batch", || {
            crate::runtime::check_cancelled()?;
            let mut predictions = Vec::with_capacity(inputs.len());
            for batch in inputs.chunks(10000) {
                self.worker
                    .request(2, &batch.iter().map(String::as_str).collect::<Vec<_>>())?;
                for _ in batch {
                    self.worker.status()?;
                    let count = self.worker.read_u32()?;
                    if count > 64 {
                        return Err("Risposta fastText con troppe etichette".into());
                    }
                    let mut prediction = TopicPrediction {
                        labels: Vec::with_capacity(count as usize),
                        probabilities: Vec::with_capacity(count as usize),
                    };
                    for _ in 0..count {
                        prediction.labels.push(self.worker.read_string(1048576)?);
                        let probability = self
                            .worker
                            .read_string(64)?
                            .parse::<f32>()
                            .map_err(|e| e.to_string())?;
                        if !probability.is_finite() {
                            return Err("Probabilità fastText non finita".into());
                        }
                        prediction.probabilities.push(f64::from(probability));
                    }
                    predictions.push(prediction);
                }
            }
            Ok(predictions)
        })
    }
}

#[cfg(feature = "cli")]
pub(super) fn model_vocabulary(
    model: &mut impl TopicModel,
    items: impl Iterator<Item = String>,
) -> Result<std::collections::HashSet<String>, String> {
    let mut items = items;
    let mut known = std::collections::HashSet::new();
    loop {
        crate::runtime::check_cancelled()?;
        let batch = items.by_ref().take(4096).collect::<Vec<_>>();
        if batch.is_empty() {
            break;
        }
        let ids = model.word_ids(&batch.iter().map(String::as_str).collect::<Vec<_>>())?;
        if ids.len() != batch.len() {
            return Err("Risposta del vocabolario incompleta".into());
        }
        for (item, id) in batch.into_iter().zip(ids) {
            if id >= 0 {
                known.insert(item);
            }
        }
    }
    Ok(known)
}

#[cfg(feature = "cli")]
pub(super) fn validate_topic_spec(spec: &serde_json::Value) -> DumpResult<()> {
    use std::collections::HashSet;
    let labels = spec["labels"]
        .as_array()
        .ok_or("Tassonomia del modello mancante")?;
    if spec["version"] != 1
        || spec["threshold"] != 0.5
        || labels.len() != 64
        || labels
            .iter()
            .any(|label| !label.as_str().is_some_and(|s| s.contains('.')))
        || labels
            .iter()
            .map(|label| label.as_str())
            .collect::<HashSet<_>>()
            .len()
            != 64
        || !spec["model_sha512"].as_str().is_some_and(|s| {
            s.len() == 128
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
    {
        return Err("Configurazione del modello dei temi non valida".into());
    }
    Ok(())
}

#[cfg(feature = "cli")]
pub(super) fn validate_topic_labels(labels: &[String], spec: &serde_json::Value) -> DumpResult<()> {
    let actual = labels
        .iter()
        .map(|label| {
            label
                .strip_prefix("__label__")
                .unwrap_or(label)
                .replace('_', " ")
        })
        .collect::<std::collections::HashSet<_>>();
    let expected = spec["labels"]
        .as_array()
        .ok_or("Tassonomia mancante")?
        .iter()
        .filter_map(|label| label.as_str().map(str::to_owned))
        .collect();
    if labels.len() != 64 || actual != expected {
        return Err("Il modello non contiene la tassonomia completa prevista".into());
    }
    Ok(())
}

#[cfg(feature = "cli")]
pub(super) fn topic_deadline(deadline: Option<std::time::Instant>) -> DumpResult<()> {
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Budget di tempo esaurito; nessun supplemento incompleto pubblicato",
        )
        .into());
    }
    Ok(())
}

#[cfg(feature = "cli")]
pub(super) fn topic_time() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

#[cfg(feature = "cli")]
pub(super) fn predict_topic_batch(
    inputs: &[(String, usize, bool)],
    identities: &[(i64, serde_json::Value)],
    model: &mut impl TopicModel,
    spec: &serde_json::Value,
) -> DumpResult<(Vec<serde_json::Value>, usize)> {
    use serde_json::json;
    let texts = inputs
        .iter()
        .filter(|(text, _, _)| !text.is_empty())
        .map(|(text, _, _)| text.clone())
        .collect::<Vec<_>>();
    let predictions = if texts.is_empty() {
        Vec::new()
    } else {
        model.predict(&texts)?
    };
    if predictions.len() != texts.len() || identities.len() != inputs.len() {
        return Err("Risposta del modello incompleta".into());
    }
    let taxonomy = spec["labels"]
        .as_array()
        .ok_or("Tassonomia mancante")?
        .iter()
        .filter_map(|label| label.as_str().map(str::to_owned))
        .collect();
    let (labels, scores) = predictions
        .into_iter()
        .map(|p| (p.labels, p.probabilities))
        .unzip();
    let selected = crate::threshold_topics(taxonomy, labels, scores)?;
    let mut selected = selected.into_iter();
    let records = inputs.iter().zip(identities).map(|((text, count, mapped), (id, revision))| {
        let (labels, status) = if text.is_empty() {
            (None, if *mapped { "no_model_vocabulary" } else { "no_mapped_outlinks" })
        } else { (selected.next(), "predicted") };
        json!({"page_id":id,"revision_id":revision,"topic_labels":labels,"topic_input_count":count,"topic_status":status})
    }).collect();
    Ok((records, texts.len()))
}

/// Infer a full SQL edition with the verified staging's three indexed streams.
/// Only titles, QIDs, redirects, category hierarchy and one edge batch reside
/// in memory; an expired budget never publishes a partial edition.
#[cfg(feature = "cli")]
pub fn predict_imported_topics(
    raw_path: &std::path::Path,
    output: &std::path::Path,
    model: &mut impl TopicModel,
    spec: &serde_json::Value,
    batch_size: usize,
    deadline: Option<std::time::Instant>,
    work_dir: Option<&std::path::Path>,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("topics.sql", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use sha2::Digest;
        use std::{io::Write, time::Instant};
        validate_topic_spec(spec)?;
        validate_topic_labels(&model.labels()?, spec)?;
        if !(1..=10000).contains(&batch_size) {
            return Err("Dimensione del blocco non valida".into());
        }
        topic_deadline(deadline)?;
        let started = topic_time();
        let begin = Instant::now();
        let imported = ImportedGraph::open(raw_path, work_dir, true)?;
        imported.check_output(raw_path, output, &[])?;
        let ImportedGraph {
            metadata,
            manifest_path,
            source,
            expected,
            raw_signature,
            ..
        } = imported;
        let language = metadata["language"]
            .as_str()
            .ok_or("Lingua del grafo mancante")?;
        let mut identities = Vec::with_capacity(expected);
        let mut titles = Vec::with_capacity(expected);
        let mut items = Vec::with_capacity(expected);
        {
            let mut query = source.prepare("SELECT id,data FROM pages ORDER BY id")?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let page: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
                if page["page_id"] != id {
                    return Err("Identità delle pagine SQL non corrispondenti".into());
                }
                titles.push(
                    page["title"]
                        .as_str()
                        .ok_or("Titolo SQL mancante")?
                        .to_owned(),
                );
                items.push(page["wikibase_item"].as_str().map(str::to_owned));
                identities.push((id, page["revision_id"].clone()));
            }
        }
        if identities.len() != expected || identities.windows(2).any(|rows| rows[0].0 >= rows[1].0)
        {
            return Err("Identità delle pagine SQL incomplete".into());
        }
        let (unique, _) = crate::unique_items(&items);
        let known = model_vocabulary(model, unique.into_keys())?;
        let redirects = sql_redirects(&source)?;
        let parents = sql_category_parents(&source)?;
        let mut inputs =
            crate::TopicInputAccumulator::new(titles, items, redirects.clone(), known)?;
        let graph_seconds = begin.elapsed().as_secs_f64();
        topic_deadline(deadline)?;
        let (spool, file) = RecordSpool::new(output)?;
        let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
        let mut digest = sha2::Sha256::new();
        digest.update(b"{\"category_parents\": ");
        digest.update(python_json(&json!(parents))?.as_bytes());
        digest.update(b", \"metadata\": ");
        digest.update(python_json(&metadata)?.as_bytes());
        digest.update(b", \"pages\": [");
        let mut pages_query = source.prepare("SELECT id,data FROM pages ORDER BY id")?;
        let mut links_query = source.prepare("SELECT id,target FROM links ORDER BY id,rowid")?;
        let mut cats_query =
            source.prepare("SELECT id,target FROM categories ORDER BY id,rowid")?;
        let mut pages = pages_query.query([])?;
        let mut links = links_query.query([])?;
        let mut cats = cats_query.query([])?;
        let mut link = next_relation(&mut links)?;
        let mut cat = next_relation(&mut cats)?;
        let mut processed = 0;
        let mut predicted = 0;
        loop {
            crate::runtime::check_cancelled()?;
            topic_deadline(deadline)?;
            let mut batch = Vec::with_capacity(batch_size);
            let mut outlinks = Vec::with_capacity(batch_size);
            while batch.len() < batch_size {
                let Some(row) = pages.next()? else {
                    break;
                };
                let id: i64 = row.get(0)?;
                if link.as_ref().is_some_and(|(source, _)| *source < id)
                    || cat.as_ref().is_some_and(|(source, _)| *source < id)
                {
                    return Err("Staging con relazione senza la voce sorgente".into());
                }
                let mut targets = Vec::new();
                while link.as_ref().is_some_and(|(source, _)| *source == id) {
                    if let Some((_, title)) = link.take() {
                        targets.push(title);
                    }
                    link = next_relation(&mut links)?;
                }
                let mut categories = Vec::new();
                while cat.as_ref().is_some_and(|(source, _)| *source == id) {
                    if let Some((_, title)) = cat.take() {
                        categories.push(title);
                    }
                    cat = next_relation(&mut cats)?;
                }
                let mut page: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
                page["links"] = json!(targets);
                page["categories"] = json!(categories);
                batch.push((id, page));
                outlinks.push(targets);
            }
            if batch.is_empty() {
                break;
            }
            let results = inputs.add_batch(outlinks)?;
            let end = processed + batch.len();
            let expected_ids = identities
                .get(processed..end)
                .ok_or("Troppi record nello staging")?;
            let (records, count) = predict_topic_batch(&results, expected_ids, model, spec)?;
            for (((id, page), record), (expected_id, _)) in
                batch.into_iter().zip(records).zip(expected_ids)
            {
                if id != *expected_id {
                    return Err("Staging con ordine delle pagine diverso dal dump".into());
                }
                if processed != 0 {
                    digest.update(b", ");
                }
                digest.update(python_json(&page)?.as_bytes());
                serde_json::to_writer(&mut writer, &record)?;
                writer.write_all(b"\n")?;
                processed += 1;
            }
            predicted += count;
            if processed % 100000 < results.len() {
                println!("Temi SQL {language}: {processed}/{expected} voci");
            }
        }
        if link.is_some() || cat.is_some() {
            return Err("Staging con relazioni non associate a voci".into());
        }
        inputs.finish()?;
        if processed != expected {
            return Err("Predizioni incomplete rispetto al censimento SQL".into());
        }
        writer.flush()?;
        drop(writer);
        digest.update(b"], \"redirects\": ");
        digest.update(python_json(&json!(redirects))?.as_bytes());
        digest.update(b"}");
        topic_deadline(deadline)?;
        let raw_checksum = file_sha256(raw_path)?;
        if file_signature(raw_path)? != raw_signature
            || file_sha256(&manifest_path)? != metadata["dump_manifest_sha256"]
        {
            return Err("Input dei temi modificati durante l'inferenza".into());
        }
        topic_deadline(deadline)?;
        let topic_metadata = json!({"language":language,"source":"local_model_on_verified_SQL_dump_outlinks","demo":false,"complete":true,
        "started_at":started,"finished_at":topic_time(),"input_sha256":format!("{:x}",digest.finalize()),"input_file_sha256":raw_checksum,
        "graph_finished_at":metadata["finished_at"],"model":spec,"top_level_rule":"any_child_probability_gt_0.5",
        "scope":"model topic labels, not biographical gender observations","timing":{"native_graph_seconds":graph_seconds,"prediction_seconds":begin.elapsed().as_secs_f64()-graph_seconds},"predicted_pages":predicted,"missing_pages":expected-predicted});
        spool.publish(output, &topic_metadata)?;
        Ok(topic_metadata)
    })
}

/// In-memory parity path for explicitly synthetic graphs. Production imports
/// always use predict_imported_topics, regardless of their edition's size.
#[cfg(feature = "cli")]
pub fn predict_topics(
    raw: &serde_json::Value,
    model: &mut impl TopicModel,
    spec: &serde_json::Value,
    batch_size: usize,
    deadline: Option<std::time::Instant>,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("topics.memory", || {
        crate::runtime::check_cancelled()?;
        use serde_json::json;
        use sha2::Digest;
        use std::collections::{HashMap, HashSet};
        validate_topic_spec(spec)?;
        validate_topic_labels(&model.labels()?, spec)?;
        if !(1..=10000).contains(&batch_size) {
            return Err("Dimensione del blocco non valida".into());
        }
        let metadata = &raw["metadata"];
        if metadata.get("finished_at").is_none() {
            return Err("Identità temporale del grafo mancante".into());
        }
        let language = metadata["language"].as_str().ok_or("Lingua mancante")?;
        crate::wiki_database(language, metadata["database"].as_str())?;
        if metadata["complete"] != true
            || metadata["demo"] != true
            || metadata["item_mapping"]
                != json!({"source":"page_props.wikibase_item","complete":true})
        {
            return Err(
                "Il percorso in memoria richiede un grafo sintetico completo con QID".into(),
            );
        }
        topic_deadline(deadline)?;
        let started = topic_time();
        let begin = std::time::Instant::now();
        let pages = raw["pages"].as_array().ok_or("Pagine mancanti")?;
        let mut identities = Vec::with_capacity(pages.len());
        let mut titles = Vec::with_capacity(pages.len());
        let mut links = Vec::with_capacity(pages.len());
        let mut items = Vec::with_capacity(pages.len());
        for page in pages {
            identities.push((
                page["page_id"].as_i64().ok_or("ID di pagina non valido")?,
                page["revision_id"].clone(),
            ));
            titles.push(page["title"].as_str().ok_or("Titolo mancante")?.to_owned());
            links.push(match page.get("links") {
                None => Vec::new(),
                Some(links) => links
                    .as_array()
                    .ok_or("Collegamenti non validi")?
                    .iter()
                    .map(|link| {
                        link.as_str()
                            .map(str::to_owned)
                            .ok_or("Collegamento non valido")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            });
            items.push(page["wikibase_item"].as_str().map(str::to_owned));
        }
        if identities
            .iter()
            .map(|(id, _)| id)
            .collect::<HashSet<_>>()
            .len()
            != identities.len()
        {
            return Err("Identificativi di pagina duplicati".into());
        }
        let redirects: HashMap<String, String> =
            serde_json::from_value(raw.get("redirects").cloned().unwrap_or(json!({})))?;
        let (_, _, _, incoming) = crate::graph_details(titles, links, redirects)?;
        let graph_seconds = begin.elapsed().as_secs_f64();
        topic_deadline(deadline)?;
        let (unique, _) = crate::unique_items(&items);
        let known = model_vocabulary(model, unique.keys().cloned())?;
        let inputs = crate::topic_inputs(pages.len(), unique, known, incoming)?;
        let mapping_seconds = begin.elapsed().as_secs_f64() - graph_seconds;
        let prediction_start = std::time::Instant::now();
        let mut records = Vec::with_capacity(pages.len());
        let mut predicted = 0;
        // Select batches of eligible pages as in the Python synthetic path; retain
        // the original page order, including known negatives and missing records.
        let eligible = inputs
            .iter()
            .enumerate()
            .filter(|(_, (text, _, _))| !text.is_empty())
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let mut selected = HashMap::new();
        for indexes in eligible.chunks(batch_size) {
            topic_deadline(deadline)?;
            let batch = indexes
                .iter()
                .map(|&i| inputs[i].clone())
                .collect::<Vec<_>>();
            let ids = indexes
                .iter()
                .map(|&i| identities[i].clone())
                .collect::<Vec<_>>();
            let (rows, count) = predict_topic_batch(&batch, &ids, model, spec)?;
            predicted += count;
            selected.extend(indexes.iter().copied().zip(rows));
        }
        for (position, ((_, count, mapped), (id, revision))) in
            inputs.iter().zip(&identities).enumerate()
        {
            records.push(selected.remove(&position).unwrap_or_else(|| json!({"page_id":id,"revision_id":revision,"topic_labels":null,"topic_input_count":count,"topic_status":if *mapped {"no_model_vocabulary"} else {"no_mapped_outlinks"}})));
        }
        topic_deadline(deadline)?;
        Ok(
            json!({"metadata":{"language":language,"source":"local_model_on_synthetic_outlinks","demo":true,"complete":true,
        "started_at":started,"finished_at":topic_time(),"input_sha256":format!("{:x}",sha2::Sha256::digest(python_json(raw)?.as_bytes())),
        "graph_finished_at":metadata["finished_at"],"model":spec,"top_level_rule":"any_child_probability_gt_0.5",
        "scope":"model topic labels, not biographical gender observations","timing":{"native_graph_seconds":graph_seconds,"mapping_seconds":mapping_seconds,"prediction_seconds":prediction_start.elapsed().as_secs_f64()},
        "predicted_pages":predicted,"missing_pages":pages.len()-predicted},"pages":records}),
        )
    })
}

#[cfg(feature = "cli")]
pub struct TopicRunOptions<'a> {
    pub input_dir: &'a std::path::Path,
    pub output_dir: &'a std::path::Path,
    pub model_path: &'a std::path::Path,
    pub spec_path: &'a std::path::Path,
    pub work_dir: Option<&'a std::path::Path>,
    pub batch_size: usize,
    pub budget_seconds: f64,
}

/// One verified model, the entire selected pool, and a total inference budget.
#[cfg(feature = "cli")]
pub fn run_topics(
    languages: &[String],
    options: TopicRunOptions<'_>,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("topics.run", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use std::time::{Duration, Instant};
        if !options.budget_seconds.is_finite() || options.budget_seconds <= 0.0 {
            return Err("Il budget deve essere un numero positivo finito".into());
        }
        if !(1..=10000).contains(&options.batch_size) {
            return Err("Dimensione del blocco non valida".into());
        }
        let began = Instant::now();
        let deadline = began
            .checked_add(Duration::try_from_secs_f64(options.budget_seconds)?)
            .ok_or("Budget troppo grande")?;
        let paths = languages
            .iter()
            .map(|code| options.input_dir.join(format!("{code}-raw.json")))
            .collect::<Vec<_>>();
        if paths.iter().any(|path| !path.is_file()) {
            return Err("Manca un censimento completo: importare prima i dump".into());
        }
        let outputs = languages
            .iter()
            .map(|code| options.output_dir.join(format!("{code}-topics.json")))
            .chain(std::iter::once(options.output_dir.join("topic-run.json")))
            .collect::<Vec<_>>();
        for output in &outputs {
            for input in paths
                .iter()
                .map(|p| p.as_path())
                .chain([options.model_path, options.spec_path])
            {
                if resolved_path(output)? == resolved_path(input)? {
                    return Err("Output dei temi sovrapposto agli input".into());
                }
            }
        }
        let spec: Value = serde_json::from_slice(&std::fs::read(options.spec_path)?)?;
        let mut model = NativeTopicModel::load(options.model_path, &spec)?;
        let mut report = json!({"time_budget_seconds":options.budget_seconds,"completed_languages":[],"deferred_languages":[],"wikis":[]});
        let mut completed = Vec::new();
        let mut deferred = Vec::new();
        let mut wikis = Vec::new();
        let mut rate: Option<f64> = None;
        for (position, path) in paths.iter().enumerate() {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64();
            if remaining == 0.0
                || rate.is_some_and(|rate| {
                    path.metadata()
                        .map(|m| m.len() as f64 * rate * 2.0 > remaining)
                        .unwrap_or(false)
                })
            {
                deferred = languages[position..].to_vec();
                break;
            }
            let before = Instant::now();
            let output = &outputs[position];
            let metadata = first_metadata(path)?;
            if metadata["language"] != languages[position] {
                return Err("Lingua del censimento non corrispondente al file selezionato".into());
            }
            let result = if metadata["source"]
                .as_str()
                .is_some_and(|s| s.starts_with("Wikimedia SQL dumps / "))
            {
                predict_imported_topics(
                    path,
                    output,
                    &mut model,
                    &spec,
                    options.batch_size,
                    Some(deadline),
                    options.work_dir,
                )
            } else {
                let raw = serde_json::from_slice(&std::fs::read(path)?)?;
                predict_topics(&raw, &mut model, &spec, options.batch_size, Some(deadline))
                    .and_then(|supplement| {
                        DumpClient::<WikimediaHttp>::write_json(output, &supplement)?;
                        Ok(supplement["metadata"].clone())
                    })
            };
            let metadata = match result {
                Ok(metadata) => metadata,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::TimedOut) =>
                {
                    deferred = languages[position..].to_vec();
                    break;
                }
                Err(error) => return Err(error),
            };
            let elapsed = before.elapsed().as_secs_f64();
            rate = Some(
                rate.unwrap_or(0.0)
                    .max(elapsed / path.metadata()?.len().max(1) as f64),
            );
            completed.push(languages[position].clone());
            wikis.push(json!({"language":languages[position],"seconds":elapsed,
            "census_pages":metadata["predicted_pages"].as_u64().ok_or("Conteggio predetto mancante")?+metadata["missing_pages"].as_u64().ok_or("Conteggio mancante")?,
            "predicted_pages":metadata["predicted_pages"],"timing":metadata["timing"]}));
            println!("Temi salvati: {}; {elapsed:.1}s", output.display());
        }
        report["completed_languages"] = json!(completed);
        report["deferred_languages"] = json!(deferred);
        report["wikis"] = json!(wikis);
        report["elapsed_seconds"] = json!(began.elapsed().as_secs_f64());
        DumpClient::<WikimediaHttp>::write_json(
            &options.output_dir.join("topic-run.json"),
            &report,
        )?;
        if !deferred.is_empty() {
            println!("Rinviate senza campionamento: {}", deferred.join(" "));
        }
        Ok(report)
    })
}
