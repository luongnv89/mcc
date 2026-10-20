//! `mcc refresh-launchers` — rewrite profiles' launchers and shell aliases
//! from the current templates.
//!
//! mcc never does this on its own: profiles created by an older mcc keep
//! their launcher and alias, byte for byte, until this command runs. The
//! `CLAUDE_CONFIG_DIR` line does not change, so neither does the login.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::claude;
use crate::cli::RefreshLaunchersArgs;
use crate::config::Config;
use crate::env_detect::Shell;
use crate::error::CliError;
use crate::paths::Paths;
use crate::txn::{self, Transaction};
use crate::{fsutil, shell, ui};

/// What a refresh replaced, restored in reverse order on failure. Unlike
/// `add`/`rename`, nothing is removed: the previous launcher and alias come back.
enum Undo {
    /// The launcher's previous bytes (`None`: there was no file).
    Wrapper(PathBuf, Option<Vec<u8>>),
    /// The previous body of alias block `id` in an rc file.
    RcBlock(PathBuf, String, String),
}

fn rollback(paths: &Paths, undo: &mut Transaction<Undo>) {
    undo.rollback(&txn::RESTORE, |u| match u {
        Undo::Wrapper(p, Some(bytes)) => (
            paths.display(&p),
            fsutil::write_atomic(&p, &bytes).and_then(|()| fsutil::set_executable(&p)),
        ),
        Undo::Wrapper(p, None) => (paths.display(&p), fsutil::remove_any(&p)),
        Undo::RcBlock(rc, id, body) => (
            format!("alias block in {}", paths.display(&rc)),
            shell::upsert_block(paths, &rc, &id, &body),
        ),
    });
}

/// How one profile's refresh went.
enum Outcome {
    Refreshed,
    Current,
    Skipped,
}

pub fn run(paths: &Paths, cfg: &Config, args: &RefreshLaunchersArgs) -> Result<()> {
    let names = selected(cfg, &args.aliases)?;
    let env = cfg.env.clone().expect("init runs before commands");
    ui::header("Refresh launchers and shell aliases");
    if names.is_empty() {
        ui::result(true, "no mcc profiles yet — add one with `mcc add <alias>`");
        return Ok(());
    }

    let claude_bin = claude::find_binary(env.claude_bin.as_deref());
    let mut undo: Transaction<Undo> = Transaction::new();
    let mut outcomes = Vec::new();
    let mut rc_changed = false;
    let result = (|| -> Result<()> {
        for name in &names {
            let p = &cfg.profiles[name];
            let Some(wrapper) = &p.wrapper else {
                ui::warn(&format!(
                    "{name}: no launcher recorded — skipped (`mcc share {name}` still works)"
                ));
                outcomes.push(Outcome::Skipped);
                continue;
            };
            let existing = std::fs::read(wrapper).ok();
            if std::fs::symlink_metadata(wrapper).is_ok() && !shell::is_mcc_wrapper(wrapper) {
                ui::warn(&format!(
                    "{name}: {} was not created by mcc — skipped",
                    paths.display(wrapper)
                ));
                outcomes.push(Outcome::Skipped);
                continue;
            }

            let mut changed = false;
            let script = shell::launcher_script(paths, name, &p.dir, claude_bin.as_deref());
            if existing.as_deref() != Some(script.as_bytes()) {
                undo.record(Undo::Wrapper(wrapper.clone(), existing));
                shell::install_launcher(wrapper, &script)?;
                changed = true;
            }

            for rc in rc_files(cfg, name) {
                let body = shell::alias_block(paths, rc_shell(&rc), name, &p.dir, Some(wrapper));
                let old = shell::block_body(&rc, name).unwrap_or_default();
                if old.trim_end() != body.trim_end() {
                    undo.record(Undo::RcBlock(rc.clone(), name.clone(), old));
                    shell::upsert_block(paths, &rc, name, &body)?;
                    changed = true;
                    rc_changed = true;
                }
            }

            if changed {
                ui::success(&format!("{name}: launcher and shell alias refreshed"));
                outcomes.push(Outcome::Refreshed);
            } else {
                ui::info(&format!("{name}: already up to date"));
                outcomes.push(Outcome::Current);
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        rollback(paths, &mut undo);
        return Err(e);
    }
    undo.commit();

    let count = |f: fn(&Outcome) -> bool| outcomes.iter().filter(|o| f(o)).count();
    let skipped = count(|o| matches!(o, Outcome::Skipped));
    ui::result(
        skipped == 0,
        &format!(
            "{} refreshed, {} already up to date, {skipped} skipped",
            count(|o| matches!(o, Outcome::Refreshed)),
            count(|o| matches!(o, Outcome::Current)),
        ),
    );
    if rc_changed {
        ui::hint(
            "open a new terminal (or `source` your rc file) so the shell picks up the new aliases",
        );
    }
    Ok(())
}

/// The profiles to refresh, all checked before anything is written.
fn selected(cfg: &Config, aliases: &[String]) -> Result<Vec<String>> {
    if aliases.is_empty() {
        return Ok(cfg.profiles.keys().cloned().collect());
    }
    let mut names: Vec<String> = Vec::new();
    for a in aliases {
        if matches!(a.as_str(), "default" | "claude") {
            return Err(
                CliError::usage("the default profile (~/.claude) has no mcc launcher")
                    .hint("it is started with `claude`; refresh mcc profiles from `mcc list`")
                    .into(),
            );
        }
        if !cfg.profiles.contains_key(a) {
            return Err(super::unknown_profile(a));
        }
        if !names.contains(a) {
            names.push(a.clone());
        }
    }
    Ok(names)
}

/// rc files holding the profile's alias block: the one recorded at `add`,
/// plus the current shell's (in case it changed since).
fn rc_files(cfg: &Config, name: &str) -> Vec<PathBuf> {
    let mut rcs: Vec<PathBuf> = cfg.profiles[name].rc_file.iter().cloned().collect();
    if let Some(rc) = cfg.env.as_ref().and_then(|e| e.rc_file.clone())
        && !rcs.contains(&rc)
    {
        rcs.push(rc);
    }
    rcs.retain(|rc| shell::has_block(rc, name));
    rcs
}

/// Alias syntax for an rc file: fish's for `config.fish`, else sh's (zsh and
/// bash share it), whatever the current shell is.
fn rc_shell(rc: &Path) -> Shell {
    if rc.extension().is_some_and(|e| e == "fish") {
        Shell::Fish
    } else {
        Shell::Zsh
    }
}
