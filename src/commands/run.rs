//! `mcc run` — start Claude Code in the current project with its profile.
//!
//! The project's default profile (`mcc project set`) starts, or the default
//! profile (~/.claude) outside any project. mcc never switches accounts on
//! its own: when that profile is known to be at its 5-hour or weekly limit,
//! `mcc run` shows every profile's usage and asks which one to start.
//! `--profile` starts a given profile without checking usage. Only the
//! default's usage is fetched unless it is at its limit.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;

use crate::cli::RunArgs;
use crate::config::Config;
use crate::error::CliError;
use crate::paths::Paths;
use crate::pick;
use crate::usage::{self, Usage};
use crate::{claude, ui};

/// A profile `mcc run` can start.
struct Profile {
    name: String,
    /// `CLAUDE_CONFIG_DIR` exactly as registered (the keychain login is keyed
    /// on that string); `None` for ~/.claude, which runs with it unset.
    config_dir: Option<PathBuf>,
}

impl Profile {
    fn target(&self, paths: &Paths) -> usage::Target {
        usage::Target {
            name: self.name.clone(),
            config_dir: self.config_dir.clone().unwrap_or(paths.default_dir.clone()),
            is_default: self.config_dir.is_none(),
        }
    }
}

/// Every profile: the default, then the mcc profiles in name order.
fn profiles(cfg: &Config) -> Vec<Profile> {
    std::iter::once(Profile {
        name: "default".into(),
        config_dir: None,
    })
    .chain(cfg.profiles.iter().map(|(name, p)| Profile {
        name: name.clone(),
        config_dir: Some(p.dir.clone()),
    }))
    .collect()
}

/// Usage of `profiles`, in order; not fetched with `MCC_NO_USAGE`.
fn fetch(paths: &Paths, profiles: &[&Profile]) -> Vec<Usage> {
    if usage::disabled_by_env() {
        crate::log::write("INFO", "usage not fetched: MCC_NO_USAGE is set");
        return profiles.iter().map(|_| Usage::default()).collect();
    }
    let targets: Vec<usage::Target> = profiles.iter().map(|p| p.target(paths)).collect();
    usage::fetch_all(&targets)
}

pub fn run(paths: &Paths, cfg: &Config, args: &RunArgs) -> Result<()> {
    let all = profiles(cfg);
    let verb = if args.dry_run {
        "would start"
    } else {
        "starting"
    };

    if let Some(name) = args.profile.as_deref() {
        let name = if name == "claude" { "default" } else { name };
        let Some(p) = all.iter().find(|p| p.name == name) else {
            return Err(super::unknown_profile(name));
        };
        ui::info(&format!("{verb} `{}` — picked with --profile", p.name));
        return launch(paths, cfg, p, args);
    }

    let cwd = super::project::current_dir()?;
    let project = super::project::lookup(cfg, &cwd);
    let default_name = project.map_or("default", |(_, p)| p.profile.as_str());
    let Some(default) = all.iter().find(|p| p.name == default_name) else {
        let (dir, _) = project.expect("the default profile always exists");
        return Err(CliError::input(format!(
            "the project default of {} is \"{default_name}\", which is not a profile",
            paths.display(Path::new(dir))
        ))
        .hint("set another with `mcc project set <profile>`, or remove it with `mcc project unset`")
        .into());
    };
    let why = match project {
        Some((dir, _)) => format!("the default for {}", paths.display(Path::new(dir))),
        None => "no project default here".to_string(),
    };

    let default_usage = fetch(paths, &[default]).remove(0);
    if !pick::at_limit(&default_usage) {
        ui::info(&format!(
            "{verb} `{}` — {why} ({})",
            default.name,
            pick::summary(&default_usage)
        ));
        return launch(paths, cfg, default, args);
    }

    let limit = pick::limit_note(&default_usage, Utc::now());
    ui::warn(&format!("`{}` ({why}) is at {limit}", default.name));
    // Every other profile's usage, for you to choose from: mcc never
    // switches accounts on its own.
    let others: Vec<&Profile> = all.iter().filter(|p| p.name != default.name).collect();
    let usages = fetch(paths, &others);
    let items = choices(&default.name, &limit, &others, &usages);
    let hint = format!(
        "start a profile with `mcc run --profile <name>`, or wait until `{}` resets",
        default.name
    );
    if args.dry_run || !ui::is_interactive() {
        for item in &items[1..] {
            ui::info(item);
        }
    }
    if args.dry_run {
        ui::info(&format!(
            "`mcc run` would ask which profile to start; {hint}"
        ));
        return launch(paths, cfg, default, args);
    }
    let items: Vec<&str> = items.iter().map(String::as_str).collect();
    let chosen = match ui::select("Which profile should start?", &items, 0, &hint)? {
        0 => default,
        i => {
            let p = others[i - 1];
            ui::info(&format!(
                "starting `{}` — your pick (this project still defaults to `{}`)",
                p.name, default.name
            ));
            p
        }
    };
    launch(paths, cfg, chosen, args)
}

/// The prompt's choices: the default first (kept as is), then every other
/// profile with its usage.
fn choices(default: &str, limit: &str, others: &[&Profile], usages: &[Usage]) -> Vec<String> {
    std::iter::once(format!("{default} — start it anyway (at {limit})"))
        .chain(
            others
                .iter()
                .zip(usages)
                .map(|(p, u)| format!("{} — {}", p.name, pick::summary(u))),
        )
        .collect()
}

/// Print the profile's name (`--dry-run`), or replace this process with
/// claude running as that profile.
fn launch(paths: &Paths, cfg: &Config, p: &Profile, args: &RunArgs) -> Result<()> {
    if args.dry_run {
        println!("{}", p.name);
        return Ok(());
    }
    // Same order as the launchers: PATH first, then the binary found at init.
    let bin = match claude::find_binary(None) {
        Some(bin) => bin,
        None => claude::require_binary(cfg.env.as_ref().and_then(|e| e.claude_bin.as_deref()))?,
    };
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(&args.args)
        // Keep claude consistent with mcc's view of HOME (MCC_HOME sandboxes).
        .env("HOME", &paths.home);
    match &p.config_dir {
        Some(dir) => cmd.env("CLAUDE_CONFIG_DIR", dir),
        None => cmd.env_remove("CLAUDE_CONFIG_DIR"),
    };
    crate::log::write(
        "INFO",
        &format!("run: exec {} as profile {}", bin.display(), p.name),
    );
    let err = cmd.exec();
    Err(anyhow::Error::new(err).context(format!("starting {}", bin.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Reason;

    #[test]
    fn prompt_lists_the_default_first_then_every_profile() {
        let (lab, side) = (
            Profile {
                name: "lab".into(),
                config_dir: Some("/h/.mcc/profiles/lab".into()),
            },
            Profile {
                name: "side".into(),
                config_dir: Some("/h/.mcc/profiles/side".into()),
            },
        );
        let full = Usage {
            status: usage::Status::Ok,
            reason: None,
            five_hour: Some(usage::Window {
                percent: 20.0,
                resets_at: None,
            }),
            weekly: Some(usage::Window {
                percent: 35.0,
                resets_at: None,
            }),
        };
        let items = choices(
            "work",
            "its 5-hour limit, resets in 35m",
            &[&lab, &side],
            &[full, Usage::unavailable(Reason::TokenExpired)],
        );
        assert_eq!(
            items,
            [
                "work — start it anyway (at its 5-hour limit, resets in 35m)",
                "lab — 5h 20%, week 35% used",
                "side — usage unknown: token expired; start the profile in Claude Code to refresh it",
            ]
        );
    }
}
