//! `mcc run` — start Claude Code in the current project with its profile.
//!
//! The project's default profile (`mcc project set`) starts, or the default
//! profile (~/.claude) outside any project. mcc never switches accounts on
//! its own: when that profile is known to be at its 5-hour or weekly limit,
//! `mcc run` asks what to do, and waiting (starting nothing) is the default.
//! The next choice starts the profile anyway; other profiles are offered
//! only when the project allows them (`mcc project set --allow`), by name.
//! Only that profile's usage is ever fetched. `--profile` starts a given
//! profile without checking usage. Starting a profile other than the
//! project's prints the terms-of-service note.

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

/// Printed on stderr whenever a profile other than the project's starts.
pub const RUN_TOS_NOTE: &str = crate::cli::run_tos_note!();

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
        // The project only decides the note: --profile starts even where the
        // project default is broken.
        let here = super::project::current_dir()
            .ok()
            .and_then(|cwd| super::project::lookup(cfg, &cwd).map(|(_, p)| p.profile.clone()));
        if needs_tos_note(here.as_deref(), &p.name) {
            eprintln!("{RUN_TOS_NOTE}");
        }
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

    let now = Utc::now();
    let limit = pick::limit_note(&default_usage, now);
    ui::warn(&format!("`{}` ({why}) is at {limit}", default.name));
    // mcc never switches accounts on its own, and never fetches or shows
    // another account's usage: waiting is the default, and only the
    // profiles the project allows are offered, by name.
    let mut allowed: Vec<String> = Vec::new();
    for name in project.map_or(&[][..], |(_, p)| &p.allow) {
        if *name != default.name && !allowed.contains(name) && all.iter().any(|p| p.name == *name) {
            allowed.push(name.clone());
        }
    }
    let reset = pick::reset_time(&default_usage, now);
    let items = choices(&default.name, reset.as_deref(), &allowed);
    let mut hint = format!(
        "wait until `{0}` resets, or start it anyway with `mcc run --profile {0}`",
        default.name
    );
    if !allowed.is_empty() {
        hint.push_str(&format!(
            " (this project also allows {})",
            super::project::ticked(&allowed)
        ));
    }
    if args.dry_run {
        ui::info("`mcc run` would ask what to do; the default is to wait");
        ui::hint(&hint);
        return launch(paths, cfg, default, args);
    }
    ui::require_interactive("a choice", &hint)?;
    let labels: Vec<&str> = items.iter().map(|(label, _)| label.as_str()).collect();
    let i = ui::select("Wait, or start a profile?", &labels, DEFAULT_CHOICE, &hint)?;
    let chosen = match &items[i].1 {
        Choice::Wait => {
            ui::info(&format!(
                "not starting anything — {}",
                pick::reset_note(&default.name, reset.as_deref())
            ));
            return Ok(());
        }
        Choice::StartAnyway => default,
        Choice::Other(name) => {
            let p = all
                .iter()
                .find(|p| p.name == *name)
                .expect("allowed profiles exist");
            if needs_tos_note(project.map(|(_, p)| p.profile.as_str()), &p.name) {
                eprintln!("{RUN_TOS_NOTE}");
            }
            ui::info(&format!(
                "starting `{}` — your pick (this project still defaults to `{}`)",
                p.name, default.name
            ));
            p
        }
    };
    launch(paths, cfg, chosen, args)
}

/// What the at-limit prompt does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Choice {
    /// Start nothing, and exit 0.
    Wait,
    /// Start the project's profile although it is at its limit.
    StartAnyway,
    /// Start another profile the project allows.
    Other(String),
}

/// The prompt's preselected choice: waiting.
const DEFAULT_CHOICE: usize = 0;

/// The prompt's choices: wait for `profile` to reset (the default), start it
/// anyway, then each profile the project allows, by name only.
fn choices(profile: &str, reset: Option<&str>, allowed: &[String]) -> Vec<(String, Choice)> {
    [
        (pick::wait_label(profile, reset), Choice::Wait),
        (format!("Start `{profile}` anyway"), Choice::StartAnyway),
    ]
    .into_iter()
    .chain(allowed.iter().map(|name| {
        (
            format!("Start `{name}` instead"),
            Choice::Other(name.clone()),
        )
    }))
    .collect()
}

/// Whether starting `chosen` prints the terms-of-service note: inside a
/// project (`project` is its profile), for any profile but the project's.
fn needs_tos_note(project: Option<&str>, chosen: &str) -> bool {
    project.is_some_and(|p| p != chosen)
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

    #[test]
    fn prompt_waits_by_default_then_offers_only_allowed_profiles_by_name() {
        let allowed = ["lab".to_string(), "side".to_string()];
        let items = choices("work", Some("35m"), &allowed);
        let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Wait — `work` resets in 35m",
                "Start `work` anyway",
                "Start `lab` instead",
                "Start `side` instead",
            ]
        );
        // Waiting is preselected; starting anyway comes next.
        assert_eq!(items[DEFAULT_CHOICE].1, Choice::Wait);
        assert_eq!(items[DEFAULT_CHOICE + 1].1, Choice::StartAnyway);
        let others: Vec<&Choice> = items[2..].iter().map(|(_, c)| c).collect();
        assert_eq!(
            others,
            [&Choice::Other("lab".into()), &Choice::Other("side".into())]
        );
        // No other account's usage.
        assert!(labels.iter().all(|l| !l.contains('%')), "{labels:?}");

        // Nothing allowed: only waiting or starting it anyway.
        let items = choices("work", None, &[]);
        assert_eq!(
            items,
            [
                (
                    "Wait — `work` (reset time unknown)".to_string(),
                    Choice::Wait
                ),
                ("Start `work` anyway".to_string(), Choice::StartAnyway),
            ]
        );
    }

    #[test]
    fn the_note_is_for_another_profile_than_the_projects() {
        assert!(needs_tos_note(Some("work"), "lab"));
        assert!(needs_tos_note(Some("work"), "default"));
        assert!(needs_tos_note(Some("default"), "work"));
        assert!(!needs_tos_note(Some("work"), "work"));
        assert!(!needs_tos_note(Some("default"), "default"));
        // Outside every project, any profile starts without it.
        assert!(!needs_tos_note(None, "lab"));
        assert!(!needs_tos_note(None, "default"));
    }
}
