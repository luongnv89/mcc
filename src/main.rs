mod claude;
mod cli;
mod commands;
mod config;
mod copy;
mod env_detect;
mod error;
mod fsutil;
mod log;
mod manifest;
mod metrics;
mod paths;
mod pick;
mod plugins;
mod shell;
mod txn;
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
///
/// Without a command (bare `mcc`, or only global flags), mcc shows the
/// status view once it is set up. Before that, clap behaves as it always
/// did: bare `mcc` prints the help on stderr and exits 2, and global flags
/// alone are a usage error.
fn parse_args() -> Cli {
    let no_color = std::env::args_os().skip(1).any(|a| a == "--no-color")
        || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let mut cmd = Cli::command();
    if no_color {
        cmd = cmd.color(ColorChoice::Never);
    }
    // Only argv without a word in it can lack a command; checking the
    // config just then keeps every other command (the statusline shim runs
    // on each refresh) from reading it here.
    let flags_only = std::env::args_os()
        .skip(1)
        .all(|a| a.to_string_lossy().starts_with('-'));
    if flags_only && set_up() {
        cmd = cmd.arg_required_else_help(false);
    } else {
        cmd = cmd.subcommand_required(true);
    }
    let matches = cmd.get_matches_mut();
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.format(&mut cmd).exit())
}

/// Whether mcc is set up: `~/.mcc/config.toml` loads and has its `[env]`
/// (written by `mcc init`, or the first-run setup of any other command).
/// Read-only; any error counts as not set up.
fn set_up() -> bool {
    paths::Paths::from_env()
        .and_then(|p| config::Config::load(&p.config_file))
        .is_ok_and(|c| c.env.is_some())
}

/// Hint for failures that carry no specific one (I/O errors, failed spawns).
fn fallback_hint() -> String {
    let log = paths::Paths::from_env()
        .map(|p| p.display(&p.log_file))
        .unwrap_or_else(|_| "~/.mcc/logs/mcc.log".into());
    format!("re-run with --verbose for more detail; the full log is in {log}")
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let Some(command) = cli.command else {
        // Bare `mcc` once set up (`parse_args` lets nothing else through):
        // read-only, like `mcc doctor` but without even a log line — no
        // lock, no first-run setup, no live usage lookup, nothing written.
        let paths = paths::Paths::from_env()?;
        let cfg = config::Config::load(&paths.config_file)?;
        return commands::status::run(&paths, &cfg);
    };
    if let Command::Completions(args) = &command {
        // Log only into an existing ~/.mcc/logs: completions never create
        // state, and any logging failure stays silent.
        if let Ok(paths) = paths::Paths::from_env() {
            log::init_if_present(paths.log_file);
            log_invocation();
        }
        return commands::completions::run(args);
    }
    if let Command::Statusline(args) = &command
        && args.action.is_none()
    {
        // The shim runs on every status-line refresh: no logging, no config
        // lock, no first-run setup, and it never writes config.toml.
        return commands::statusline::render(&paths::Paths::from_env()?, args.prefix);
    }
    if let Command::LaunchBanner(args) = &command {
        // Run by a launcher on every start: no logging, no lock, no
        // first-run setup, nothing written, and never an error.
        commands::banner::run(&args.alias);
        return Ok(());
    }
    let paths = paths::Paths::from_env()?;
    if let Command::Doctor(args) = &command {
        // Read-only, even on first use: no lock, no first-run setup, and
        // logging only into an existing ~/.mcc/logs.
        log::init_if_present(paths.log_file.clone());
        log_invocation();
        let cfg = config::Config::load(&paths.config_file)?;
        return commands::doctor::run(&paths, &cfg, args);
    }
    log::init(paths.log_file.clone());
    log_invocation();

    if let Command::Config(args) = &command {
        // No first-run setup, and the config isn't loaded here: `mcc config
        // edit` must be able to fix a file that doesn't parse, and `get` and
        // `path` only read. `set` holds the lock for its whole run; `edit`
        // takes it only to save (see commands/config.rs).
        let _lock = if writes_config(&command) {
            Some(config::lock(&paths)?)
        } else {
            None
        };
        return commands::config::run(&paths, args);
    }

    let explicit_init = matches!(command, Command::Init(_));
    // `<alias> --mcc-share`, `mcc run` and `mcc env` (direnv) run on every
    // launch, and `mcc exec` must not hold the lock its command may need:
    // keep them free of setup.
    let launch = matches!(&command, Command::Share(a) if a.from_launch)
        || matches!(
            command,
            Command::Run(_) | Command::Env(_) | Command::Exec(_)
        );
    // Held from before the config is loaded until mcc exits, so concurrent
    // runs never lose each other's changes.
    let mut lock = if writes_config(&command) {
        Some(config::lock(&paths)?)
    } else {
        None
    };
    let mut cfg = config::Config::load(&paths.config_file)?;
    if cfg.env.is_none() && !explicit_init && !launch {
        if lock.is_none() {
            lock = Some(config::lock(&paths)?);
            // Another command may have finished the first run meanwhile.
            cfg = config::Config::load(&paths.config_file)?;
        }
        if cfg.env.is_none() {
            ui::info(
                "First run — detecting your environment (re-run anytime with `mcc init --force`)",
            );
            commands::init::detect_and_save(&paths, &mut cfg, None)?;
        }
    }
    if needs_live_disclosure(&command, &cfg) {
        // Recording the acknowledgement writes config.toml, so it takes the
        // lock like any other write (only for this, when the command doesn't
        // hold it already) and re-reads the config under it.
        let _ack_lock = match lock {
            Some(_) => None,
            None => {
                let l = config::lock(&paths)?;
                cfg = config::Config::load(&paths.config_file)?;
                Some(l)
            }
        };
        if needs_live_disclosure(&command, &cfg) {
            eprintln!("{}", usage::LIVE_DISCLOSURE);
            cfg.usage.live_disclosed_at =
                Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            cfg.save(&paths.config_file)?;
            log::write("INFO", "live usage lookup disclosure shown");
        }
    }
    let _lock = lock;

    match command {
        Command::Init(args) => commands::init::run(&paths, &mut cfg, &args),
        Command::Add(args) => commands::add::run(&paths, &mut cfg, &args),
        Command::List(args) => commands::list::run(&paths, &cfg, &args),
        Command::Usage(args) => commands::usage::run(&paths, &cfg, &args),
        Command::Delete(args) => commands::delete::run(&paths, &mut cfg, &args),
        Command::Sync(args) => commands::sync::run(&paths, &mut cfg, &args),
        Command::Diff(args) => commands::diff::run(&paths, &cfg, &args),
        Command::Rename(args) => commands::rename::run(&paths, &mut cfg, &args),
        Command::Share(args) => commands::share::run(&paths, &mut cfg, &args),
        Command::Link(args) => commands::link::run(&paths, &mut cfg, &args),
        Command::Project(args) => commands::project::run(&paths, &mut cfg, &args),
        Command::Run(args) => commands::run::run(&paths, &cfg, &args),
        Command::Env(args) => commands::env::run(&paths, &cfg, &args),
        Command::Exec(args) => commands::exec::run(&paths, &cfg, &args),
        Command::RefreshLaunchers(args) => commands::refresh_launchers::run(&paths, &cfg, &args),
        Command::Statusline(args) => commands::statusline::run(&paths, &cfg, &args),
        Command::Completions(_)
        | Command::Config(_)
        | Command::Doctor(_)
        | Command::LaunchBanner(_) => {
            unreachable!("handled above")
        }
    }
}

/// Whether this command is about to look usage up live for the first time:
/// `[usage] live = true` without `live_disclosed_at`, and the command reads
/// usage (`mcc list` without `--no-usage`, `mcc usage`, `mcc run` without
/// `--profile`) with `MCC_NO_USAGE` unset.
fn needs_live_disclosure(command: &Command, cfg: &config::Config) -> bool {
    let reads_usage = match command {
        Command::List(args) => !args.no_usage,
        Command::Usage(_) => true,
        Command::Run(args) => args.profile.is_none(),
        _ => false,
    };
    reads_usage
        && cfg.usage.live
        && cfg.usage.live_disclosed_at.is_none()
        && !usage::disabled_by_env()
}

/// Commands that may write ~/.mcc/config.toml, and so take its lock.
fn writes_config(command: &Command) -> bool {
    match command {
        Command::Init(_)
        | Command::Add(_)
        | Command::Delete(_)
        | Command::Sync(_)
        | Command::Rename(_)
        | Command::Share(_)
        | Command::Link(_) => true,
        Command::Project(args) => !matches!(args.action, cli::ProjectAction::List { .. }),
        // install/uninstall edit settings.json files and mcc's state, one
        // run at a time; the bare shim is routed before the lock is taken.
        Command::Statusline(args) => args.action.is_some(),
        // `set` writes under the lock; `get` and `path` only read. `edit`
        // takes the lock itself, only to save: holding it while the editor
        // is open would block every other mcc command until it closes.
        Command::Config(args) => matches!(args.action, cli::ConfigAction::Set { .. }),
        // These read the config; the one time one of them records the live
        // usage disclosure, it takes the lock for that write alone.
        Command::List(_)
        | Command::Diff(_)
        | Command::Usage(_)
        | Command::Run(_)
        | Command::RefreshLaunchers(_)
        | Command::Completions(_) => false,
        // Read-only, and never locked: it reports, it doesn't fix.
        Command::Doctor(_) => false,
        // Runs on every launch: reads the config, never writes it.
        Command::LaunchBanner(_) => false,
        // Never locked: `mcc exec` runs commands that may run mcc commands
        // which take the lock themselves.
        Command::Env(_) | Command::Exec(_) => false,
    }
}

fn log_invocation() {
    log::write(
        "INFO",
        &format!(
            "mcc {} — {:?}",
            env!("CARGO_PKG_VERSION"),
            redact_args(std::env::args().skip(1))
        ),
    );
}

/// The command line as logged: the value of `--email` (`--email ADDRESS` or
/// `--email=ADDRESS`) becomes `<redacted>`, so the address given to
/// `mcc add --email` never reaches mcc.log (#88). clap accepts only the exact
/// long option (no prefixes, no short form). Arguments after `--` are
/// redacted the same way, which only hides more.
fn redact_args(args: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut redact_next = false;
    args.into_iter()
        .map(|arg| {
            if std::mem::take(&mut redact_next) {
                "<redacted>".to_string()
            } else if arg == "--email" {
                redact_next = true;
                arg
            } else if arg.starts_with("--email=") {
                "--email=<redacted>".to_string()
            } else {
                arg
            }
        })
        .collect()
}

/// Ctrl+C exits with 130, except while `claude auth login` owns the terminal
/// (the child handles it, and mcc then rolls back what it created).
fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(|| {
        if claude::CHILD_RUNNING.load(Ordering::SeqCst) {
            claude::INTERRUPTED.store(true, Ordering::SeqCst);
            return;
        }
        let term = console::Term::stderr();
        let _ = term.show_cursor();
        log::write("WARN", "interrupted by Ctrl+C");
        eprintln!("\n✗ interrupted");
        std::process::exit(error::EXIT_INTERRUPTED);
    });
}

#[cfg(test)]
mod tests {
    use super::redact_args;

    fn redact(args: &[&str]) -> Vec<String> {
        redact_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn redacts_the_email_value_in_both_forms() {
        assert_eq!(
            redact(&["add", "work", "--email", "a@b.c", "--sso"]),
            ["add", "work", "--email", "<redacted>", "--sso"]
        );
        assert_eq!(
            redact(&["add", "--email=a@b.c", "--yes"]),
            ["add", "--email=<redacted>", "--yes"]
        );
        assert_eq!(redact(&["add", "--email="]), ["add", "--email=<redacted>"]);
    }

    #[test]
    fn a_trailing_email_flag_has_nothing_to_redact() {
        assert_eq!(
            redact(&["add", "work", "--email"]),
            ["add", "work", "--email"]
        );
    }

    #[test]
    fn leaves_other_arguments_alone() {
        let args = ["usage", "--show-email", "--emailx", "a@b.c", "--", "x"];
        assert_eq!(redact(&args), args);
        assert!(redact(&[]).is_empty());
    }
}
