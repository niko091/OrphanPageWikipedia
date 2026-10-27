use super::*;

impl Invocation {
    pub(super) fn run_download(&self) -> Result<(), String> {
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
        Ok(())
    }
    pub(super) fn run_import(&self) -> Result<(), String> {
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
            let manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&manifest_path).map_err(|e| e.to_string())?)
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
        Ok(())
    }
    pub(super) fn run_quality(&self) -> Result<(), String> {
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
        Ok(())
    }
}
