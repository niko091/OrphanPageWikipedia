use super::*;

pub(super) fn read_json(path: &Path) -> PipelineResult<Value> {
    Ok(serde_json::from_reader(std::io::BufReader::new(
        File::open(path)?,
    ))?)
}

pub(super) fn signature(path: &Path) -> Option<Value> {
    dumps::file_signature(path)
        .ok()
        .filter(|(size, _)| *size > 0)
        .map(|s| json!(s))
}

pub(super) fn complete_json(path: &Path) -> bool {
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

pub(super) fn output_signatures(outputs: &[PathBuf]) -> Option<Value> {
    outputs
        .iter()
        .map(|path| Some((path.to_string_lossy().into_owned(), signature(path)?)))
        .collect::<Option<Map<_, _>>>()
        .map(Value::Object)
}

pub(super) struct StepState {
    pub(super) path: PathBuf,
    pub(super) steps: Map<String, Value>,
}

impl StepState {
    pub(super) fn open(path: PathBuf) -> PipelineResult<Self> {
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

    pub(super) fn reason(&self, key: &str, inputs: &Value, outputs: &[PathBuf]) -> &'static str {
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

    pub(super) fn save(
        &mut self,
        key: &str,
        inputs: Value,
        outputs: &[PathBuf],
    ) -> PipelineResult<()> {
        let outputs = output_signatures(outputs).ok_or("Output mancante per il checkpoint")?;
        self.steps
            .insert(key.into(), json!({"inputs":inputs,"outputs":outputs}));
        dumps::DumpClient::<dumps::WikimediaHttp>::write_json(
            &self.path,
            &json!({"version":1,"steps":self.steps}),
        )?;
        Ok(())
    }
}

pub(super) fn fetched_outputs(path: &Path, metadata: &Value) -> PipelineResult<Vec<PathBuf>> {
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

pub(super) fn manifest_for(
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

pub(super) fn imported_reason(
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

pub(super) fn topics_match(
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

pub(super) fn summary_matches(folder: &Path, entry: &Value, names: &[String]) -> bool {
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

pub(super) fn approved_outputs(
    folder: &Path,
    entries: &[Value],
    files: &BTreeMap<String, String>,
) -> bool {
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

pub(super) fn collected_matches(
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

pub(super) struct PublicationInputs<'a> {
    pub(super) selected: &'a [(String, String)],
    pub(super) manifests: &'a BTreeMap<String, String>,
    pub(super) model: &'a Value,
    pub(super) measures: &'a BTreeMap<String, Value>,
    pub(super) rules: &'a Value,
}

pub(super) fn current_publication(
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

pub(super) trait Steps {
    fn log(&mut self, text: &str) -> PipelineResult<()>;
    fn run(&mut self, name: &str, arguments: &[String]) -> PipelineResult<()>;
}
