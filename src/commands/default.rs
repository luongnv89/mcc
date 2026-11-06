//! `mcc default` — choose the profile plain `claude` and `mcc run` start.
//!
//! The default profile stays ~/.claude on disk: nothing moves, no login and
//! no keychain item changes. mcc keeps a pointer instead, `default_profile`
//! in `~/.mcc/config.toml`, which `mcc run`, `mcc env` and the bare `mcc`
//! status view honor outside any project (a project default still wins),
//! and a managed block in the shell's rc file (`# >>> mcc:default >>>`)
//! defining a `claude` function that starts the chosen profile's launcher in
//! new interactive shells. `command claude`, IDEs and scripts still start
//! ~/.claude, which also stays the setup `mcc sync` and `mcc add --clone`
//! copy from. `mcc default default` goes back: the pointer and the block go.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::cli::DefaultArgs;
use crate::config::Config;
use crate::paths::Paths;
use crate::txn::{self, Transaction};
use crate::{shell, ui};

/// A default-profile block replaced or removed: its rc file and previous
/// body (`None`: there was none). Restored in reverse order on failure.
pub(crate) struct BlockUndo(PathBuf, Option<String>);

/// Put the recorded blocks back as they were.
pub(crate) fn rollback_blocks(paths: &Paths, undo: &mut Transaction<BlockUndo>) {
    undo.rollback(&txn::RESTORE, |BlockUndo(rc, previous)| {
        let what = format!("the default-profile block in {}", paths.display(&rc));
        let done = match previous {
            Some(body) => shell::upsert_block(paths, &rc, shell::DEFAULT_BLOCK, &body),
            None => shell::remove_block(paths, &rc, shell::DEFAULT_BLOCK).map(|_| ()),
        };
        (what, done)
    });
}

pub fn run(paths: &Paths, cfg: &mut Config, args: &DefaultArgs) -> Result<()> {
    match args.profile.as_deref() {
        None => show(paths, cfg),
        Some(name) => set(paths, cfg, name),
    }
}

/// `mcc default`: the global default's name on stdout, what it means on
/// stderr. Writes nothing.
fn show(paths: &Paths, cfg: &Config) -> Result<()> {
    let name = cfg.global_default();
    // A name hand-written in the config file could hold control characters.
    let shown = if super::has_control(name) { "?" } else { name };
    println!("{shown}");
    if name == "default" {
        ui::info(&format!(
            "the default profile is ~/.claude ({}): plain `claude` and `mcc run` (outside projects) start it",
            paths.display(&paths.default_dir)
        ));
    } else if let Some(p) = cfg.profiles.get(name) {
        ui::info(&format!(
            "the default profile is `{name}` ({}): plain `claude` (in new shells) and `mcc run` (outside projects) start it",
            paths.display(&p.dir)
        ));
        ui::info(
            "`command claude` still starts ~/.claude, which stays the setup `mcc sync` copies from",
        );
    } else {
        ui::warn(&format!(
            "the global default is \"{shown}\", which is not a profile — set another with `mcc default <profile>`, or go back to ~/.claude with `mcc default default`"
        ));
    }
    if let Ok(cwd) = super::project::current_dir()
        && let (here, super::run::Origin::Project(dir, _)) = super::run::applies_in(cfg, &cwd)
    {
        let here = if super::has_control(here) { "?" } else { here };
        ui::info(&format!(
            "here, the project default of {} applies instead: `mcc run` starts `{here}`",
            paths.display(Path::new(dir))
        ));
    }
    if !stale_blocks(paths, cfg).is_empty() {
        ui::hint(&format!(
            "the shell's `claude` function doesn't match — run `mcc default {shown}` to update it"
        ));
    }
    Ok(())
}

/// `mcc default PROFILE`: make it the global default.
fn set(paths: &Paths, cfg: &mut Config, name: &str) -> Result<()> {
    // `default`/`claude` mean ~/.claude; anything else must be a profile
    // (exit 2 before anything is written).
    let new = super::project::profile_name(cfg, name)?.to_string();
    let old = cfg.global_default().to_string();
    if new == old {
        return already(paths, cfg, &new);
    }

    ui::header(&if new == "default" {
        "Make ~/.claude the default profile again".to_string()
    } else {
        format!("Make \"{new}\" the default profile")
    });
    let mut steps = ui::Steps::new(2);
    let previous = cfg.default_profile.clone();
    let mut undo: Transaction<BlockUndo> = Transaction::new();
    let result = (|| -> Result<()> {
        cfg.default_profile = (new != "default").then(|| new.clone());
        let step = steps.start("Update the shell's `claude` command");
        let changes = sync_blocks(paths, cfg, &mut undo)?;
        report(paths, cfg, step, &changes);
        let step = steps.start("Update registry");
        cfg.save(&paths.config_file)?;
        step.ok(&paths.display(&paths.config_file));
        Ok(())
    })();
    if let Err(e) = result {
        cfg.default_profile = previous;
        rollback_blocks(paths, &mut undo);
        return Err(e);
    }
    undo.commit();

    ui::result(
        true,
        &format!(
            "the default profile is now {} (was {})",
            describe(paths, &new),
            describe(paths, &old)
        ),
    );
    if new == "default" {
        ui::kv("Plain `claude`", "starts ~/.claude (in new shells)");
    } else {
        ui::kv(
            "Plain `claude`",
            &format!("starts `{new}` in new interactive shells"),
        );
        ui::kv("`command claude`", "still starts ~/.claude");
    }
    ui::kv(
        "`mcc run`",
        &if new == "default" {
            "starts ~/.claude outside projects".to_string()
        } else {
            format!("starts `{new}` outside projects")
        },
    );
    ui::hint(
        "open a new terminal (or run `exec $SHELL`) so plain `claude` follows the change; IDEs and scripts that run `claude` are not affected",
    );
    if new != "default" {
        ui::hint(
            "no data moved: ~/.claude keeps its login and stays the setup `mcc sync` and `mcc add --clone` copy from; `mcc default default` goes back",
        );
    }
    if let Ok(cwd) = super::project::current_dir()
        && let (here, super::run::Origin::Project(dir, _)) = super::run::applies_in(cfg, &cwd)
    {
        ui::info(&format!(
            "here, the project default of {} still applies: `mcc run` starts `{here}`",
            paths.display(Path::new(dir))
        ));
    }
    Ok(())
}

/// `new` is already the global default: nothing to change, unless the
/// shell's block disagrees with it (removed by hand, or the shell changed),
/// which is put right. The config is never written.
fn already(paths: &Paths, cfg: &Config, new: &str) -> Result<()> {
    let what = describe(paths, new);
    if stale_blocks(paths, cfg).is_empty() {
        ui::success(&format!(
            "{what} is already the default profile — nothing changed"
        ));
        return Ok(());
    }
    let mut undo: Transaction<BlockUndo> = Transaction::new();
    match sync_blocks(paths, cfg, &mut undo) {
        Ok(changes) => {
            undo.commit();
            ui::success(&format!(
                "{what} is already the default profile — repaired the shell's `claude` command ({})",
                changes.summary(paths)
            ));
            Ok(())
        }
        Err(e) => {
            rollback_blocks(paths, &mut undo);
            Err(e)
        }
    }
}

/// `` `work` `` or `` `default` (~/.claude) ``.
fn describe(paths: &Paths, name: &str) -> String {
    if name == "default" {
        format!("`default` ({})", paths.display(&paths.default_dir))
    } else if super::has_control(name) {
        "`?`".to_string()
    } else {
        format!("`{name}`")
    }
}

/// How the shell step went.
fn report(paths: &Paths, cfg: &Config, step: ui::Step, changes: &Changes) {
    let rc = cfg.env.as_ref().and_then(|e| e.rc_file.as_deref());
    match (rc, cfg.default_profile.as_deref()) {
        (None, Some(_)) => step.warn(
            "unsupported shell — plain `claude` still starts ~/.claude; `mcc run` follows the new default",
        ),
        _ if changes.is_empty() => step.skip("nothing to change"),
        _ => step.ok(&changes.summary(paths)),
    }
}

/// What [`sync_blocks`] changed.
#[derive(Debug, Default)]
pub(crate) struct Changes {
    pub(crate) written: Vec<PathBuf>,
    pub(crate) removed: Vec<PathBuf>,
}

impl Changes {
    pub(crate) fn is_empty(&self) -> bool {
        self.written.is_empty() && self.removed.is_empty()
    }

    fn summary(&self, paths: &Paths) -> String {
        let list = |v: &[PathBuf]| {
            v.iter()
                .map(|p| paths.display(p))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut parts = Vec::new();
        if !self.written.is_empty() {
            parts.push(format!("`claude` function in {}", list(&self.written)));
        }
        if !self.removed.is_empty() {
            parts.push(format!(
                "removed the `claude` function from {}",
                list(&self.removed)
            ));
        }
        parts.join("; ")
    }
}

/// The rc files a default-profile block may be in: the current shell's,
/// then every profile's (each once).
pub(crate) fn block_rc_files(cfg: &Config) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let all = cfg
        .env
        .iter()
        .filter_map(|e| e.rc_file.clone())
        .chain(cfg.profiles.values().filter_map(|p| p.rc_file.clone()));
    for rc in all {
        if !files.contains(&rc) {
            files.push(rc);
        }
    }
    files
}

/// The block body `rc` should hold under `cfg`: the `claude` function for
/// the global default in the current shell's rc file, nothing anywhere else
/// (and nothing at all while ~/.claude is the default, or the pointer names
/// no profile).
pub(crate) fn wanted_block(paths: &Paths, cfg: &Config, rc: &Path) -> Option<String> {
    let current = cfg.env.as_ref().and_then(|e| e.rc_file.as_deref())?;
    if current != rc {
        return None;
    }
    let alias = cfg.default_profile.as_deref()?;
    let p = cfg.profiles.get(alias)?;
    Some(shell::default_block(
        paths,
        super::refresh_launchers::rc_shell(rc),
        alias,
        &p.dir,
        p.wrapper.as_deref(),
    ))
}

/// The rc files whose default-profile block differs from what `cfg` wants
/// (missing, out of date, or left over).
pub(crate) fn stale_blocks(paths: &Paths, cfg: &Config) -> Vec<PathBuf> {
    block_rc_files(cfg)
        .into_iter()
        .filter(|rc| {
            let have = shell::block_body(rc, shell::DEFAULT_BLOCK);
            let want = wanted_block(paths, cfg, rc);
            have.as_deref().map(str::trim_end) != want.as_deref().map(str::trim_end)
        })
        .collect()
}

/// Make the rc files agree with `cfg` ([`wanted_block`]), writing only what
/// differs, so running it again changes nothing. Each change is recorded in
/// `undo` as it is made.
pub(crate) fn sync_blocks(
    paths: &Paths,
    cfg: &Config,
    undo: &mut Transaction<BlockUndo>,
) -> Result<Changes> {
    let mut changes = Changes::default();
    for rc in block_rc_files(cfg) {
        let have = shell::block_body(&rc, shell::DEFAULT_BLOCK);
        let want = wanted_block(paths, cfg, &rc);
        if have.as_deref().map(str::trim_end) == want.as_deref().map(str::trim_end) {
            continue;
        }
        match &want {
            Some(body) => {
                shell::upsert_block(paths, &rc, shell::DEFAULT_BLOCK, body)?;
                undo.record(BlockUndo(rc.clone(), have));
                changes.written.push(rc);
            }
            None => {
                if shell::remove_block(paths, &rc, shell::DEFAULT_BLOCK)? {
                    undo.record(BlockUndo(rc.clone(), have));
                    changes.removed.push(rc);
                }
            }
        }
    }
    Ok(changes)
}
