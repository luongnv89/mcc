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
mod validate;

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use clap::Parser;

use crate::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
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
                ui::error(&msg, hint.as_deref());
            }
            ExitCode::from(code as u8)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    if let Command::Completions(args) = &cli.command {
        return commands::completions::run(args);
    }
    let paths = paths::Paths::from_env()?;
    log::init(paths.log_file.clone());
    log::write("INFO", &format!("mcc {} — {:?}", env!("CARGO_PKG_VERSION"), std::env::args().skip(1).collect::<Vec<_>>()));

    let explicit_init = matches!(cli.command, Command::Init(_));
    let mut cfg = config::Config::load(&paths.config_file)?;
    if cfg.env.is_none() && !explicit_init {
        ui::info("First run — detecting your environment (re-run anytime with `mcc init --force`)");
        commands::init::detect_and_save(&paths, &mut cfg, None)?;
    }

    match cli.command {
        Command::Init(args) => commands::init::run(&paths, &mut cfg, &args),
        Command::Add(args) => commands::add::run(&paths, &mut cfg, &args),
        Command::List(args) => commands::list::run(&paths, &cfg, &args),
        Command::Delete(args) => commands::delete::run(&paths, &mut cfg, &args),
        Command::Sync(args) => commands::sync::run(&paths, &mut cfg, &args),
        Command::Completions(_) => unreachable!("handled above"),
    }
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
