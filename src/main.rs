//! Standalone command contracts. Each backend stage is enabled as it is ported.
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn option(name: &'static str) -> Arg {
    Arg::new(name).long(name).value_name("VALUE")
}

fn path_option(name: &'static str, default: &'static str) -> Arg {
    option(name).default_value(default)
}

fn editions(command: Command) -> Command {
    command
        .arg(path_option("languages-file", "config/languages.json"))
        .arg(option("languages").num_args(1..))
}

fn history_controls(command: Command) -> Command {
    command
        .arg(
            option("history-cache-mib")
                .default_value("256")
                .value_parser(clap::value_parser!(u32).range(1..=65536)),
        )
        .arg(
            option("history-commit-rows")
                .default_value("250000")
                .value_parser(clap::value_parser!(u32).range(1..=10000000)),
        )
}

fn positive_budget(value: &str) -> Result<f64, String> {
    let value = value.parse::<f64>().map_err(|error| error.to_string())?;
    if !value.is_finite() || value <= 0.0 {
        return Err("Il budget deve essere un numero positivo finito".into());
    }
    Ok(value)
}

fn cli() -> Command {
    let content = editions(
        Command::new("fetch-content").about("Scarica i dump ufficiali del solo contenuto corrente"),
    )
    .arg(option("content-release").required(true))
    .arg(path_option("root", "data/dumps"))
    .arg(Arg::new("plan").long("plan").action(ArgAction::SetTrue));
    let quality = editions(
        Command::new("quality").about("Calcola Johnson V2 dai dump XML correnti verificati"),
    )
    .arg(option("content-release").required(true))
    .arg(path_option("root", "data/dumps"))
    .arg(path_option("input-dir", "data/imported"))
    .arg(path_option("output-dir", "data/imported"))
    .arg(path_option("work-dir", "data/dumps/staging"));
    let refresh = editions(
        Command::new("refresh-measures")
            .about("Aggiorna le misure riutilizzando grafo, temi e candidate pubblicati"),
    )
    .arg(path_option("input-dir", "data/imported"))
    .arg(path_option("data-dir", "web/data"));
    let downloads =
        editions(Command::new("fetch-dumps").about("Scarica e verifica dump Wikimedia completi"))
            .arg(option("sql-release").required(true))
            .arg(option("history-release").required(true))
            .arg(path_option("root", "data/dumps"))
            .arg(Arg::new("plan").long("plan").action(ArgAction::SetTrue));
    let imported = history_controls(
        Command::new("import-dumps").about("Importa grafo SQL e cronologia verificata"),
    )
    .arg(option("manifest").required(true))
    .arg(option("output"))
    .arg(option("history-output"))
    .arg(path_option("work-dir", "data/dumps/staging"));
    let history = editions(history_controls(
        Command::new("import-history").about("Riprende la cronologia dallo staging SQL"),
    ))
    .arg(option("manifest").conflicts_with_all(["languages", "sql-release", "history-release"]))
    .arg(option("sql-release").required_unless_present("manifest"))
    .arg(option("history-release").required_unless_present("manifest"))
    .arg(option("output").requires("manifest").conflicts_with_all([
        "sql-release",
        "history-release",
        "languages",
    ]))
    .arg(path_option("output-dir", "data/imported"))
    .arg(path_option("root", "data/dumps"))
    .arg(path_option("work-dir", "data/dumps/staging"));
    let compare = Command::new("compare")
        .about("Confronto interlingua in tutte le direzioni")
        .arg(Arg::new("inputs").num_args(0..))
        .arg(path_option("input-dir", "data"))
        .arg(path_option("languages-file", "config/languages.json"))
        .arg(path_option("output-dir", "web/data"))
        .arg(option("work-dir"))
        .arg(option("topics").num_args(0..))
        .arg(option("qualities").num_args(0..))
        .arg(option("histories").num_args(0..))
        .arg(path_option("rules", "config/categories.json"));
    let topics = editions(
        Command::new("topics").about("Modello locale sui collegamenti dei dump importati"),
    )
    .arg(path_option("input-dir", "data"))
    .arg(path_option("output-dir", "data"))
    .arg(path_option(
        "model",
        "data/models/johnson-outlink-20220727.bin",
    ))
    .arg(path_option("model-config", "config/topics.json"))
    .arg(
        option("batch-size")
            .default_value("256")
            .value_parser(clap::value_parser!(u32).range(1..=10000)),
    )
    .arg(
        option("time-budget-seconds")
            .default_value("3300")
            .value_parser(positive_budget),
    )
    .arg(option("work-dir"));
    let analyze = Command::new("analyze")
        .about("Analisi nativa di un censimento completo")
        .arg(Arg::new("input").required(true))
        .arg(option("topics"))
        .arg(option("quality"))
        .arg(option("history"))
        .arg(option("work-dir"))
        .arg(option("output"))
        .arg(path_option("rules", "config/categories.json"));
    Command::new("orphanwiki")
        .about("Censimento e analisi delle pagine orfane")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg(option("repo-root").global(true).help("Radice di configurazioni e percorsi relativi; altrimenti cerca dalla directory corrente"))
        .subcommands([
            Command::new("dashboard").about("Prepara i trasporti dagli export approvati").arg(path_option("data-dir", "web/data")),
            content, quality, refresh, downloads, imported, history, compare, topics, analyze,
        ])
}

fn repository_root(explicit: Option<&String>, current: &Path) -> Result<PathBuf, String> {
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

struct Invocation {
    name: String,
    args: ArgMatches,
    root: PathBuf,
    editions: Vec<(String, String)>,
}

impl Invocation {
    fn path(&self, argument: &str) -> Result<PathBuf, String> {
        self.args
            .get_one::<String>(argument)
            .map(|value| self.root.join(value))
            .ok_or_else(|| format!("Percorso mancante: {argument}"))
    }

    fn load_editions(&mut self) -> Result<(), String> {
        let uses_pool = matches!(
            self.name.as_str(),
            "fetch-dumps" | "fetch-content" | "quality" | "refresh-measures" | "topics" | "compare"
        ) || (self.name == "import-history"
            && self.args.get_one::<String>("manifest").is_none());
        if !uses_pool {
            return Ok(());
        }
        let path = self.path("languages-file")?;
        let config = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let config = serde_json::from_str(&config).map_err(|error| error.to_string())?;
        let pool = _native::configured_languages(&config)?;
        let selected = if self.name == "compare" {
            None
        } else {
            self.args
                .get_many::<String>("languages")
                .map(|values| values.cloned().collect::<Vec<_>>())
        };
        self.editions = _native::select_languages(&pool, selected.as_deref())?;
        // The CLI's explicit acquisition order is independent of the pool's order.
        if let Some(selected) = selected {
            self.editions
                .sort_by_key(|(language, _)| selected.iter().position(|code| code == language));
        }
        Ok(())
    }

    fn execute(&self) -> Result<(), String> {
        if matches!(self.name.as_str(), "fetch-dumps" | "fetch-content") {
            use _native::dumps::{DumpClient, WikimediaHttp};
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
            return Ok(());
        }
        Err(format!("Il backend Rust del comando {} non è ancora migrato; usare il percorso Python verificato. Nessun output pubblicato.", self.name))
    }
}

fn main() -> ExitCode {
    let mut arguments = match cli().try_get_matches() {
        Ok(arguments) => arguments,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(error.exit_code() as u8);
        }
    };
    let current = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let root = match repository_root(arguments.get_one::<String>("repo-root"), &current) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let Some((name, args)) = arguments.remove_subcommand() else {
        return ExitCode::from(2);
    };
    let mut invocation = Invocation {
        name,
        args,
        root,
        editions: Vec::new(),
    };
    if let Err(error) = invocation.load_editions() {
        eprintln!("{error}");
        return ExitCode::from(2);
    }
    match invocation.execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commands_accept_the_existing_pipeline_arguments() {
        for arguments in [
            vec![
                "fetch-dumps",
                "--sql-release",
                "20261001",
                "--history-release",
                "2026-09",
                "--languages",
                "lmo",
                "vec",
                "--plan",
            ],
            vec!["fetch-content", "--content-release", "2026-10-01", "--plan"],
            vec![
                "import-dumps",
                "--manifest",
                "manifest.json",
                "--history-output",
                "history.json",
            ],
            vec![
                "import-history",
                "--manifest",
                "manifest.json",
                "--history-cache-mib",
                "1024",
                "--history-commit-rows",
                "100000",
            ],
            vec![
                "import-history",
                "--sql-release",
                "20261001",
                "--history-release",
                "2026-09",
            ],
            vec!["quality", "--content-release", "2026-10-01"],
            vec!["refresh-measures", "--languages", "vec", "lmo"],
            vec![
                "topics",
                "--batch-size",
                "10000",
                "--time-budget-seconds",
                "604800",
            ],
            vec![
                "analyze",
                "vec-raw.json",
                "--history",
                "vec-history.json",
                "--quality",
                "vec-quality.json",
            ],
            vec![
                "compare",
                "vec-raw.json",
                "lmo-raw.json",
                "--topics",
                "vec-topics.json",
                "lmo-topics.json",
                "--histories",
            ],
            vec!["dashboard", "--data-dir", "web/data"],
        ] {
            cli()
                .try_get_matches_from(std::iter::once("orphanwiki").chain(arguments))
                .unwrap();
        }
    }

    #[test]
    fn argument_errors_are_distinct_from_backend_errors() {
        for arguments in [
            vec![],
            vec!["unknown"],
            vec!["analyze"],
            vec!["fetch-dumps"],
            vec!["topics", "--batch-size", "0"],
            vec!["topics", "--batch-size", "10001"],
            vec!["topics", "--time-budget-seconds", "NaN"],
            vec!["topics", "--time-budget-seconds", "inf"],
            vec!["topics", "--time-budget-seconds", "0"],
            vec!["import-history", "--manifest", "x", "--languages", "vec"],
            vec![
                "import-history",
                "--manifest",
                "x",
                "--sql-release",
                "20261001",
            ],
            vec![
                "import-history",
                "--sql-release",
                "20261001",
                "--history-release",
                "2026-09",
                "--output",
                "x",
            ],
            vec![
                "import-dumps",
                "--manifest",
                "x",
                "--history-cache-mib",
                "65537",
            ],
            vec![
                "import-dumps",
                "--manifest",
                "x",
                "--history-commit-rows",
                "0",
            ],
        ] {
            let error = cli()
                .try_get_matches_from(std::iter::once("orphanwiki").chain(arguments))
                .unwrap_err();
            assert_eq!(error.exit_code(), 2);
        }
        assert_eq!(
            cli()
                .try_get_matches_from(["orphanwiki", "--help"])
                .unwrap_err()
                .exit_code(),
            0
        );
    }

    #[test]
    fn complete_language_pool_is_validated_before_selecting_a_subset() {
        let pool = _native::configured_languages(&json!({"version":1,"languages":["vec","lmo","be-tarask"],"databases":{"lmo":"customwiki","be-tarask":"be_x_oldwiki"}})).unwrap();
        assert_eq!(pool[2].1, "be_x_oldwiki");
        assert_eq!(
            _native::select_languages(&pool, Some(&["lmo".into(), "vec".into()])).unwrap(),
            pool[..2]
        );
        for selection in [vec![], vec!["en".into()], vec!["vec".into(), "vec".into()]] {
            assert!(_native::select_languages(&pool, Some(&selection)).is_err());
        }
        for config in [
            json!({"version":1,"languages":["vec","lmo"],"databases":{"lmo":"bad"}}),
            json!({"version":1,"languages":["vec"],"databases":{"en":"enwiki"}}),
            json!({"version":1,"languages":["vec","vec"]}),
            json!({"version":1,"languages":["all"]}),
            json!({"version":1,"languages":["vec"],"sampling":10}),
            json!({"version":1,"languages":[]}),
        ] {
            assert!(_native::configured_languages(&config).is_err());
        }
        for language in ["", "ALL", "a/b", "é", "all"] {
            assert!(_native::wiki_database(language, None).is_err());
        }
        assert_eq!(
            _native::wiki_database("roa-tara", None).unwrap(),
            "roa_tarawiki"
        );
        assert!(_native::wiki_database("vec", Some("vecwiki_p")).is_err());
    }
}
