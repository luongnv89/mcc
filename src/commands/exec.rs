//! `mcc exec` — run any command with a profile's `CLAUDE_CONFIG_DIR`, e.g.
//! `mcc exec work -- claude mcp list` or `mcc exec --all -- claude plugin
//! marketplace update`.
//!
//! One profile: mcc replaces itself with the command, so its exit status
//! (and signals) pass through unchanged. `--all`: the command runs once per
//! profile, the default first, and each profile's result is reported. mcc
//! holds no lock meanwhile, so the command may run mcc itself.

use std::ffi::OsString;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitStatus};
use std::sync::atomic::Ordering;

use anyhow::Result;

use crate::cli::ExecArgs;
use crate::config::Config;
use crate::error::{CliError, EXIT_INTERRUPTED};
use crate::paths::Paths;
use crate::{claude, ui};

use super::run::{self, Profile};

/// Exit codes for a command that couldn't be started, as shells use them:
/// not found, or found but not runnable (permissions, bad format).
const EXIT_NOT_FOUND: i32 = 127;
const EXIT_NOT_RUN: i32 = 126;

pub fn run(paths: &Paths, cfg: &Config, args: &ExecArgs) -> Result<()> {
    let all = run::profiles(cfg);
    let shown = shown(&args.command);
    if !args.all {
        let name = args
            .profile
            .as_deref()
            .expect("clap requires PROFILE or --all");
        let p = run::find(&all, name)?;
        run::note_if_not_the_projects(cfg, &p.name);
        ui::detail(&format!("running `{shown}` as `{}`", p.name));
        crate::log::write("INFO", &format!("exec: `{shown}` as profile {}", p.name));
        let err = command(paths, p, &args.command).exec();
        return Err(not_started(&shown, err));
    }

    let mut failed: Vec<(String, i32)> = Vec::new();
    for p in &all {
        ui::header(&format!("{} — {shown}", p.name));
        crate::log::write("INFO", &format!("exec: `{shown}` as profile {}", p.name));
        claude::CHILD_RUNNING.store(true, Ordering::SeqCst);
        let status = command(paths, p, &args.command).status();
        claude::CHILD_RUNNING.store(false, Ordering::SeqCst);
        // Ctrl+C reached the command too (whether it died of it or handled
        // it): stop, don't go on to the next profile.
        let interrupted = claude::INTERRUPTED.swap(false, Ordering::SeqCst)
            || matches!(&status, Ok(s) if s.signal() == Some(libc::SIGINT));
        if interrupted {
            ui::warn(&format!("`{}`: interrupted", p.name));
            std::process::exit(EXIT_INTERRUPTED);
        }
        let code = match status {
            Ok(status) => exit_code(status),
            Err(err) => {
                let err = not_started(&shown, err);
                ui::warn(&format!("`{}`: {err:#}", p.name));
                crate::error::classify(&err).0
            }
        };
        if code == 0 {
            ui::success(&format!("`{}`: exit 0", p.name));
        } else {
            ui::warn(&format!("`{}`: exit {code}", p.name));
            failed.push((p.name.clone(), code));
        }
    }
    report(&shown, all.len(), &failed)
}

/// The command with the profile's environment.
fn command(paths: &Paths, p: &Profile, argv: &[OsString]) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    run::profile_env(&mut cmd, paths, p);
    cmd
}

/// The closing line of `--all`, and its exit status: 0 when the command
/// succeeded everywhere, else the first failing profile's exit code.
fn report(shown: &str, total: usize, failed: &[(String, i32)]) -> Result<()> {
    let Some((_, first)) = failed.first() else {
        ui::result(
            true,
            &format!("`{shown}` succeeded in all {total} profiles"),
        );
        return Ok(());
    };
    let names: Vec<String> = failed
        .iter()
        .map(|(name, code)| format!("{name} (exit {code})"))
        .collect();
    Err(CliError {
        code: *first,
        message: format!(
            "`{shown}` failed in {} of {total} profile{}: {}",
            failed.len(),
            if total == 1 { "" } else { "s" },
            names.join(", ")
        ),
        hint: Some("the output above each result shows what went wrong".into()),
    }
    .into())
}

/// A finished command's exit code; a signal counts as 128 + its number.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

fn not_started(shown: &str, err: std::io::Error) -> anyhow::Error {
    let (code, hint) = match err.kind() {
        std::io::ErrorKind::NotFound => (
            EXIT_NOT_FOUND,
            "check the command name, or that it is on your PATH",
        ),
        _ => (
            EXIT_NOT_RUN,
            "check that the command is executable and that you may run it",
        ),
    };
    CliError {
        code,
        message: format!("couldn't run `{shown}`: {err}"),
        hint: Some(hint.into()),
    }
    .into()
}

/// The command as shown in messages.
fn shown(argv: &[OsString]) -> String {
    argv.iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_reports_the_first_failing_exit_code() {
        assert!(report("true", 3, &[]).is_ok());
        let err = report("x", 3, &[("lab".into(), 3), ("work".into(), 1)]).unwrap_err();
        assert_eq!(crate::error::classify(&err).0, 3);
        assert_eq!(
            err.to_string(),
            "`x` failed in 2 of 3 profiles: lab (exit 3), work (exit 1)"
        );
        assert_eq!(exit_code(ExitStatus::from_raw(9)), 137);
    }
}
