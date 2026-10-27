use super::*;

pub(super) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(super) fn arguments(name: &str, pairs: &[(&str, String)]) -> Vec<String> {
    let mut args = vec![name.into()];
    append_options(&mut args, pairs);
    args
}

pub(super) fn append_options(args: &mut Vec<String>, pairs: &[(&str, String)]) {
    for (name, value) in pairs {
        args.push(format!("--{name}"));
        args.push(value.clone());
    }
}

pub(super) fn with_languages(mut arguments: Vec<String>, languages: &[String]) -> Vec<String> {
    arguments.push("--languages".into());
    arguments.extend_from_slice(languages);
    arguments
}

/// Delete only verified downloads inside the pipeline's owned work area. Check
/// every path before removing any; never accept a manifest pointing elsewhere.
pub(super) fn remove_downloads(
    manifest: &Path,
    metadata: &Value,
    root: &Path,
) -> PipelineResult<()> {
    let root = root.canonicalize()?;
    let mut files = Vec::new();
    for path in fetched_outputs(manifest, metadata)?.into_iter().skip(1) {
        if !path.exists() {
            continue;
        }
        let resolved = path.canonicalize()?;
        if !resolved.starts_with(&root)
            || std::fs::symlink_metadata(&path)?.file_type().is_symlink()
        {
            return Err("Il dump da eliminare non appartiene all'area di lavoro".into());
        }
        files.push(path);
    }
    for path in files {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub(super) fn collection_workflow(
    options: &PipelineOptions,
    steps: &mut impl Steps,
) -> PipelineResult<()> {
    let Some(destination) = &options.collection_database else {
        return workflow(options, steps);
    };
    if options.plan {
        steps.log(&format!("PUBBLICAZIONE database {}; una raccolta completa; dump eliminati dopo importazione, intermedi dopo pubblicazione",destination.display()))?;
        return workflow(options, steps);
    }
    let identity = json!({"rule":crate::store::RULE,"languages":options.selected,"sql_release":options.sql_release,
        "history_release":options.history_release,"content_release":options.content_release,
        "languages_sha256":dumps::file_sha256(&options.languages_file)?,"topics_sha256":dumps::file_sha256(&options.model_config)?,
        "categories_sha256":dumps::file_sha256(&options.root.join("config/categories.json"))?,
        "quality_sha256":dumps::file_sha256(&options.root.join("config/quality.json"))?});
    let hash = dumps::canonical_json_sha256(&identity)?;
    let parent = destination
        .parent()
        .ok_or("Directory del database mancante")?;
    std::fs::create_dir_all(parent)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(destination.with_extension("lock"))?;
    lock.try_lock()
        .map_err(|_| "Un'altra pipeline sta pubblicando la raccolta")?;
    let mut private = options.clone();
    let directories = [
        options
            .dump_root
            .join(format!(".collection-{}-downloads", &hash[..16])),
        options
            .import_dir
            .join(format!(".collection-{}-imports", &hash[..16])),
        options
            .work_dir
            .join(format!(".collection-{}-staging", &hash[..16])),
        options
            .output_dir
            .join(format!(".collection-{}-exports", &hash[..16])),
    ];
    let cleanup = || -> PipelineResult<()> {
        for directory in &directories {
            if directory.exists() {
                if read_json(&directory.join(".owner.json"))? != identity {
                    return Err("Area temporanea non appartenente alla raccolta".into());
                }
                std::fs::remove_dir_all(directory)?;
            }
        }
        Ok(())
    };
    if !options.force
        && crate::store::Census::open(destination)
            .and_then(|db| {
                db.verify()?;
                db.identity()
            })
            .ok()
            .as_ref()
            == Some(&identity)
    {
        steps.log(
            "SALTA raccolta: database completo già pubblicato per queste release e configurazioni",
        )?;
        cleanup()?;
        return Ok(());
    }
    for directory in &directories {
        if directory.exists() {
            if read_json(&directory.join(".owner.json"))? != identity {
                return Err("Area temporanea non appartenente alla raccolta".into());
            }
        } else {
            std::fs::create_dir_all(directory)?;
            dumps::DumpClient::<dumps::WikimediaHttp>::write_json(
                &directory.join(".owner.json"),
                &identity,
            )?;
        }
    }
    private.dump_root = directories[0].clone();
    private.import_dir = directories[1].clone();
    private.work_dir = directories[2].clone();
    private.output_dir = directories[3].clone();
    workflow(&private, steps)?;
    steps.log("Pubblicazione atomica del database relazionale completo")?;
    crate::store::publish(&private.output_dir, destination, &identity)?;
    cleanup()?;
    steps.log("Raccolta pubblicata; dump e intermedi temporanei eliminati")?;
    Ok(())
}

pub(super) fn workflow(options: &PipelineOptions, steps: &mut impl Steps) -> PipelineResult<()> {
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
    if options.collection_database.is_none() {
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
                &with_languages(fetch.clone(), &missing),
            )?;
        }
    }
    let mut manifests = BTreeMap::new();
    let mut graph_paths = Vec::new();
    for (language, database) in &options.selected {
        if options.collection_database.is_some() {
            let imported = manifest_for(options, language, database).is_some_and(|(path, meta)| {
                imported_reason(
                    &options.import_dir.join(format!("{language}-raw.json")),
                    &options.import_dir.join(format!("{language}-history.json")),
                    &options
                        .work_dir
                        .join(format!("{database}-{}.sqlite", options.sql_release)),
                    &path,
                    &meta,
                    true,
                ) == "verified_reuse"
            });
            if options.force || !imported {
                steps.run(
                    &format!("download verificato {language}"),
                    &with_languages(fetch.clone(), std::slice::from_ref(language)),
                )?;
            }
        }
        let (manifest, metadata) = manifest_for(options, language, database)
            .ok_or("Download terminato senza tutti i manifest richiesti")?;
        let fetch_inputs = json!({"language":language,"database":database,"sql_release":options.sql_release,"history_release":options.history_release,"dump_root":path_text(&options.dump_root)});
        let fetched = fetched_outputs(&manifest, &metadata)?;
        if fetched.iter().all(|p| p.is_file()) {
            state.save(&format!("fetch:{language}"), fetch_inputs, &fetched)?;
        }
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
        if options.collection_database.is_some() {
            remove_downloads(&manifest, &metadata, &options.dump_root)?;
        }
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
    if options.collection_database.is_none() {
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
                &with_languages(content.clone(), &missing_content),
            )?;
        }
    }
    for (language, database) in &options.selected {
        let manifest = options
            .dump_root
            .join(database)
            .join(format!("content-{}.json", options.content_release));
        if options.collection_database.is_some() {
            let quality = options.import_dir.join(format!("{language}-quality.json"));
            let graph = options.import_dir.join(format!("{language}-raw.json"));
            let reusable=(||->PipelineResult<bool>{
                let inputs=json!({"graph_sha256":dumps::file_sha256(&graph)?,"manifest_sha256":dumps::file_sha256(&manifest)?,
                    "model_sha256":dumps::file_sha256(&options.root.join("config/quality.json"))?,"quality_revision_rule":"selected_official_xml_revision_v1"});
                Ok(state.reason(&format!("quality:{language}"),&inputs,std::slice::from_ref(&quality))=="verified_reuse")
            })().unwrap_or(false);
            if options.force || !reusable {
                steps.run(
                    &format!("contenuto corrente verificato {language}"),
                    &with_languages(content.clone(), std::slice::from_ref(language)),
                )?;
            }
        }
        let metadata = read_json(&manifest)?;
        if metadata["language"] != *language
            || metadata["database"] != *database
            || metadata["content_release"] != options.content_release
            || metadata["complete"] != true
        {
            return Err("Manifest del contenuto corrente non corrispondente".into());
        }
        let fetched = fetched_outputs(&manifest, &metadata)?;
        if fetched.iter().all(|p| p.is_file()) {
            state.save(&format!("content-fetch:{language}"),json!({"language":language,"database":database,"release":options.content_release,"dump_root":path_text(&options.dump_root)}),&fetched)?;
        }
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
        if options.collection_database.is_some() {
            remove_downloads(&manifest, &metadata, &options.dump_root)?;
        }
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
