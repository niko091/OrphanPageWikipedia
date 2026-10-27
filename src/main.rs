//! Standalone dump workflow with validated arguments and native execution.
#![forbid(unsafe_code)]
use std::process::ExitCode;
mod cli;
use cli::{command, repository_root, Invocation};

fn main() -> ExitCode {
    let mut arguments = match command().try_get_matches() {
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
    // Existing dump manifests may store paths relative to the repository.
    if let Err(error) = std::env::set_current_dir(&root) {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
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
    let _signals = if invocation.name == "serve" {
        None
    } else {
        match orphanwiki::runtime::Signals::install() {
            Ok(signals) => Some(signals),
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    };
    let tracing_disabled = invocation.name == "benchmark"
        || ["plan", "no-phase-spans"].iter().any(|name| {
            invocation
                .args
                .try_get_one::<bool>(name)
                .ok()
                .flatten()
                .copied()
                .unwrap_or(false)
        });
    let settings = if tracing_disabled {
        None
    } else {
        orphanwiki::runtime::TraceSettings::from_environment()
    };
    let result = orphanwiki::runtime::with_tracing(settings, || {
        orphanwiki::runtime::phase(&format!("cli.{}", invocation.name), || {
            if invocation.name == "benchmark" {
                invocation.benchmark()
            } else {
                invocation.execute().map(|()| 0)
            }
        })
    });
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(
                orphanwiki::runtime::received_signal()
                    .map_or(1, |number| 128u8.saturating_add(number)),
            )
        }
    }
}
