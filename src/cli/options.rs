use super::*;

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

pub(crate) fn command() -> Command {
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
    let serve = Command::new("serve")
        .about("Dashboard e valutazioni editoriali condivise")
        .arg(option("web-root"))
        .arg(option("database"))
        .arg(option("collection-database"))
        .arg(option("bind").default_value("127.0.0.1"))
        .arg(
            option("port")
                .default_value("8000")
                .value_parser(clap::value_parser!(u16)),
        );
    let pipeline = editions(history_controls(
        Command::new("pipeline")
            .about("Raccolta e analisi sequenziali con checkpoint e metriche operative"),
    ))
    .arg(option("sql-release").required(true))
    .arg(option("history-release").required(true))
    .arg(option("content-release"))
    .arg(path_option("dump-root", "data/dumps"))
    .arg(path_option("import-dir", "data/imported"))
    .arg(path_option("output-dir", "web/data"))
    .arg(option("collection-database"))
    .arg(option("work-dir"))
    .arg(path_option("log-dir", "data/logs"))
    .arg(path_option(
        "model",
        "data/models/johnson-outlink-20220727.bin",
    ))
    .arg(path_option("model-config", "config/topics.json"))
    .arg(
        option("topic-budget-seconds")
            .default_value("86400")
            .value_parser(positive_budget),
    )
    .arg(
        option("topic-batch-size")
            .default_value("256")
            .value_parser(clap::value_parser!(u32).range(1..=10000)),
    )
    .arg(
        option("metrics-interval")
            .default_value("10")
            .value_parser(positive_budget),
    )
    .arg(
        Arg::new("no-phase-spans")
            .long("no-phase-spans")
            .action(ArgAction::SetTrue),
    )
    .arg(Arg::new("force").long("force").action(ArgAction::SetTrue))
    .arg(Arg::new("plan").long("plan").action(ArgAction::SetTrue));
    let benchmark = Command::new("benchmark")
        .about("Confronta comandi su copie private con parità completa degli output")
        .arg(option("spec").required(true))
        .arg(option("output-dir").required(true))
        .arg(
            option("repetitions")
                .default_value("5")
                .value_parser(clap::value_parser!(u32).range(3..)),
        )
        .arg(
            option("warmups")
                .default_value("1")
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            option("metrics-interval")
                .default_value("0.1")
                .value_parser(positive_budget),
        )
        .arg(
            option("min-speedup")
                .default_value("1.10")
                .value_parser(positive_budget),
        )
        .arg(
            option("max-rss-ratio")
                .default_value("1.10")
                .value_parser(positive_budget),
        );
    Command::new("orphanwiki")
        .about("Censimento e analisi delle pagine orfane")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg(option("repo-root").global(true).help("Radice di configurazioni e percorsi relativi; altrimenti cerca dalla directory corrente"))
        .subcommands([
            Command::new("dashboard").about("Prepara la dashboard dagli export approvati").arg(path_option("data-dir", "web/data")).arg(option("collection-database")),
            content, quality, refresh, downloads, imported, history, compare, topics, analyze, serve, pipeline, benchmark,
        ])
}
