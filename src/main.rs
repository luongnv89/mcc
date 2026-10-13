mod claude;
mod cli;
mod commands;
mod config;
mod copy;
mod env_detect;
mod error;
mod fsutil;
mod log;
mod metrics;
mod paths;
mod shell;
mod ui;
mod usage;
mod validate;

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use clap::{ColorChoice, CommandFactory, FromArgMatches};

use crate::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = parse_args();
    ui::init(cli.global.verbose, cli.global.quiet, cli.global.no_color);
    install_ctrlc_handler();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let (code, hint) = error::classify(&err);
            if code == error::EXIT_INTERRUPTED {
                ui::error("interrupted", None);
            } else {
                let mut msg = err.to_string();
                for cause in err.chain().skip(1) {
                    msg.push_str(&format!(": {cause}"));
                }
                let hint = hint.unwrap_or_else(fallback_hint);
                ui::error(&msg, Some(&hint));
            }
            ExitCode::from(code as u8)
        }
    }
}

/// Parse argv, turning off clap's own colors (help, usage errors) when
/// `--no-color` or `NO_COLOR` is set. The flag has to be spotted before
/// parsing because clap renders its output while it parses.
fn parse_args() -> Cli {
    let no_color = std::env::args_os().skip(1).any(|a| a == "--no-color")
        || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let mut cmd = Cli::command();
    if no_color {
        cmd = cmd.color(ColorChoice::Never);
    }
    let matches = cmd.get_matches_mut();
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.format(&mut cmd).exit())
}

/// Hint for failures that carry no specific one (I/O errors, failed spawns).
fn fallback_hint() -> String {
    let log = paths::Paths::from_env()
        .map(|p| p.display(&p.log_file))
        .unwrap_or_else(|_| "~/.mcc/logs/mcc.log".into());
    format!("re-run with --verbose for more detail; the full log is in {log}")
}

fn run(cli: Cli) -> anyhow::Result<()> {
    if let Command::Completions(args) = &cli.command {
        // Log only into an existing ~/.mcc/logs: completions never create
        // state, and any logging failure stays silent.
        if let Ok(paths) = paths::Paths::from_env() {
            log::init_if_present(paths.log_file);
            log_invocation();
        }
        return commands::completions::run(args);
    }
    let paths = paths::Paths::from_env()?;
    log::init(paths.log_file.clone());
    log_invocation();

    let explicit_init = matches!(cli.command, Command::Init(_));
    // `<alias> --mcc-share` runs on every such launch: keep it free of setup.
    let launch = matches!(&cli.command, Command::Share(a) if a.from_launch);
    let mut cfg = config::Config::load(&paths.config_file)?;
    if cfg.env.is_none() && !explicit_init && !launch {
        ui::info("First run — detecting your environment (re-run anytime with `mcc init --force`)");
        commands::init::detect_and_save(&paths, &mut cfg, None)?;
    }

    match cli.command {
        Command::Init(args) => commands::init::run(&paths, &mut cfg, &args),
        Command::Add(args) => commands::add::run(&paths, &mut cfg, &args),
        Command::List(args) => commands::list::run(&paths, &cfg, &args),
        Command::Usage(args) => commands::usage::run(&paths, &cfg, &args),
        Command::Delete(args) => commands::delete::run(&paths, &mut cfg, &args),
        Command::Sync(args) => commands::sync::run(&paths, &mut cfg, &args),
        Command::Rename(args) => commands::rename::run(&paths, &mut cfg, &args),
        Command::Share(args) => commands::share::run(&paths, &mut cfg, &args),
        Command::RefreshLaunchers(args) => commands::refresh_launchers::run(&paths, &cfg, &args),
        Command::Completions(_) => unreachable!("handled above"),
    }
}

fn log_invocation() {
    log::write(
        "INFO",
        &format!(
            "mcc {} — {:?}",
            env!("CARGO_PKG_VERSION"),
            std::env::args().skip(1).collect::<Vec<_>>()
        ),
    );
}

/// Ctrl+C exits with 130, except while `claude auth login` owns the terminal
/// (the child handles it, and mcc then rolls back what it created).
fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(|| {
        if claude::CHILD_RUNNING.load(Ordering::SeqCst) {
            return;
        }
        let term = console::Term::stderr();
        let _ = term.show_cursor();
        log::write("WARN", "interrupted by Ctrl+C");
        eprintln!("\n✗ interrupted");
        std::process::exit(error::EXIT_INTERRUPTED);
    });
}
