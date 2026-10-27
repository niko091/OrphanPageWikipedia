use super::*;

impl Invocation {
    pub(super) fn run_refresh(&self) -> Result<(), String> {
        let input = self.path("input-dir")?;
        let output = self.path("data-dir")?;
        for (language, _) in &self.editions {
            let quality = input.join(format!("{language}-quality.json"));
            orphanwiki::dumps::refresh_measures(
                &input.join(format!("{language}-raw.json")),
                &input.join(format!("{language}-history.json")),
                &output,
                quality.as_path(),
            )
            .map_err(|e| e.to_string())?;
        }
        orphanwiki::dumps::publish_available_aggregate(&output).map_err(|e| e.to_string())?;
        return Ok(());
    }
    pub(super) fn run_compare(&self) -> Result<(), String> {
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
        let rules: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.path("rules")?).map_err(|e| e.to_string())?)
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
    pub(super) fn run_analyze(&self) -> Result<(), String> {
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
        let rules: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.path("rules")?).map_err(|e| e.to_string())?)
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
    pub(super) fn run_topics(&self) -> Result<(), String> {
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
}
