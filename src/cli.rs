//! Validated CLI contracts and responsibility-specific native command adapters.
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::path::{Path, PathBuf};
mod acquisition;
mod analysis;
mod options;
mod orchestration;
mod service;
pub(super) use options::command;
pub(super) fn repository_root(
    explicit: Option<&String>,
    current: &Path,
) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        let path = current.join(path);
        if !path.is_dir() {
            return Err(format!(
                "Radice del repository non disponibile: {}",
                path.display()
            ));
        }
        return path.canonicalize().map_err(|error| error.to_string());
    }
    Ok(current
        .ancestors()
        .find(|path| path.join("config/languages.json").is_file())
        .unwrap_or(current)
        .to_owned())
}

pub(super) struct Invocation {
    pub(super) name: String,
    pub(super) args: ArgMatches,
    pub(super) root: PathBuf,
    pub(super) editions: Vec<(String, String)>,
}

impl Invocation {
    fn path(&self, argument: &str) -> Result<PathBuf, String> {
        self.args
            .get_one::<String>(argument)
            .map(|value| self.root.join(value))
            .ok_or_else(|| format!("Percorso mancante: {argument}"))
    }

    pub(super) fn load_editions(&mut self) -> Result<(), String> {
        let uses_pool = matches!(
            self.name.as_str(),
            "fetch-dumps"
                | "fetch-content"
                | "quality"
                | "refresh-measures"
                | "topics"
                | "compare"
                | "pipeline"
        ) || (self.name == "import-history"
            && self.args.get_one::<String>("manifest").is_none());
        if !uses_pool {
            return Ok(());
        }
        let path = self.path("languages-file")?;
        let config = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let config = serde_json::from_str(&config).map_err(|error| error.to_string())?;
        let pool = orphanwiki::configured_languages(&config)?;
        let selected = if self.name == "compare" {
            None
        } else {
            self.args
                .get_many::<String>("languages")
                .map(|values| values.cloned().collect::<Vec<_>>())
        };
        self.editions = orphanwiki::select_languages(&pool, selected.as_deref())?;
        // The CLI's explicit acquisition order is independent of the pool's order.
        if let Some(selected) = selected {
            self.editions
                .sort_by_key(|(language, _)| selected.iter().position(|code| code == language));
        }
        Ok(())
    }
}
impl Invocation {
    pub(super) fn execute(&self) -> Result<(), String> {
        match self.name.as_str() {
            "pipeline" => self.run_pipeline(),
            "serve" => self.run_serve(),
            "dashboard" => self.run_dashboard(),
            "refresh-measures" => self.run_refresh(),
            "compare" => self.run_compare(),
            "analyze" => self.run_analyze(),
            "topics" => self.run_topics(),
            "fetch-dumps" | "fetch-content" => self.run_download(),
            "import-dumps" | "import-history" => self.run_import(),
            "quality" => self.run_quality(),
            _ => Err(format!("Comando non riconosciuto: {}", self.name)),
        }
    }
}
#[cfg(test)]
mod tests;
