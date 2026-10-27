use super::*;

impl Invocation {
    pub(super) fn run_serve(&self) -> Result<(), String> {
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
        let mut app =
            orphanwiki::editorial::ReviewApplication::new(self.root.join(web_root), database)
                .with_oauth_from_environment()?;
        if let Some(path) = self.args.get_one::<String>("collection-database") {
            orphanwiki::store::Census::open(&self.root.join(path)).map_err(|e| e.to_string())?;
            app = app.with_collection_database(self.root.join(path));
        }
        orphanwiki::editorial::serve(
            app,
            self.args
                .get_one::<String>("bind")
                .ok_or("Indirizzo mancante")?,
            *self.args.get_one::<u16>("port").ok_or("Porta mancante")?,
        )
        .map_err(|e| e.to_string())
    }
    pub(super) fn run_dashboard(&self) -> Result<(), String> {
        let database = self.path("data-dir")?.join("collection.sqlite");
        if self.args.get_one::<String>("collection-database").is_none() {
            return orphanwiki::store::Census::open(&database)
                .and_then(|db| db.verify())
                .map_err(|e| e.to_string());
        }
        orphanwiki::dumps::rebuild_dashboard(&self.path("data-dir")?).map_err(|e| e.to_string())?;
        if let Some(path) = self.args.get_one::<String>("collection-database") {
            orphanwiki::store::publish(
                &self.path("data-dir")?,
                &self.root.join(path),
                &serde_json::json!({"rule":orphanwiki::store::RULE,"source":"approved_exports"}),
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
