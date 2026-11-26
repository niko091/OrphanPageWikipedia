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
        let bind = self
            .args
            .get_one::<String>("bind")
            .ok_or("Indirizzo mancante")?;
        let mut app =
            orphanwiki::editorial::ReviewApplication::new(self.root.join(web_root), database)
                .with_oauth_from_environment()?
                .with_proposals_from_environment(bind)?
                .with_collection_verification(!self.args.get_flag("skip-collection-verification"));
        if let Some(path) = self.args.get_one::<String>("collection-database") {
            orphanwiki::store::Census::open(&self.root.join(path)).map_err(|e| e.to_string())?;
            app = app.with_collection_database(self.root.join(path));
        }
        orphanwiki::editorial::serve(
            app,
            bind,
            *self.args.get_one::<u16>("port").ok_or("Porta mancante")?,
        )
        .map_err(|e| e.to_string())
    }
    pub(super) fn run_dashboard(&self) -> Result<(), String> {
        let folder = self.path("data-dir")?;
        let destination = self.args.get_one::<String>("collection-database");
        if self.args.get_flag("refresh-registration-cohorts") {
            let database = destination.map_or_else(
                || folder.join("collection.sqlite"),
                |path| self.root.join(path),
            );
            match orphanwiki::store::refresh_registration_cohorts(&database)
                .map_err(|e| e.to_string())?
            {
                Some(backup) => println!(
                    "Lustri di registrazione pubblicati in {}. Versione precedente: {}",
                    database.display(),
                    backup.display()
                ),
                None => println!(
                    "Lustri di registrazione già aggiornati in {}",
                    database.display()
                ),
            }
        } else if let Some(path) = destination {
            orphanwiki::dumps::rebuild_dashboard(&folder).map_err(|e| e.to_string())?;
            orphanwiki::store::publish(
                &folder,
                &self.root.join(path),
                &serde_json::json!({"rule":orphanwiki::store::RULE,"source":"approved_exports"}),
            )
            .map_err(|e| e.to_string())?;
        } else {
            let database = folder.join("collection.sqlite");
            return orphanwiki::store::Census::open(&database)
                .and_then(|db| db.verify())
                .map_err(|e| e.to_string());
        }
        Ok(())
    }
}
