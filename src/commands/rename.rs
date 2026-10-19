//! `mcc rename` — give a profile a new name, and with it a new command.
//!
//! Only the command changes: the launcher, the shell alias and the registry
//! key. The profile directory stays where it is, because Claude Code keys the
//! macOS keychain login on the config dir path and cloned settings hold that
//! path too; moving it would log the profile out and break its hooks.

use std::path::PathBuf;

use anyhow::Result;

use crate::claude;
use crate::cli::RenameArgs;
use crate::config::Config;
use crate::error::CliError;
use crate::paths::Paths;
use crate::{fsutil, shell, ui, validate};

/// Something created during `rename`, undone in reverse order on failure.
enum Undo {
    Wrapper(PathBuf),
    RcBlock(PathBuf, String),
}

fn rollback(paths: &Paths, undo: Vec<Undo>) {
    if undo.is_empty() {
        return;
    }
    ui::warn("Rolling back changes…");
    for u in undo.into_iter().rev() {
        let (what, res) = match u {
            Undo::Wrapper(p) => (paths.display(&p), fsutil::remove_any(&p)),
            Undo::RcBlock(rc, id) => (
                format!("alias block in {}", paths.display(&rc)),
                shell::remove_block(paths, &rc, &id).map(|_| ()),
            ),
        };
        match res {
            Ok(()) => ui::info(&format!("removed {what}")),
            Err(e) => ui::warn(&format!("could not remove {what}: {e:#}")),
        }
    }
}

pub fn run(paths: &Paths, cfg: &mut Config, args: &RenameArgs) -> Result<()> {
    let (old, new) = (args.alias.as_str(), args.new_alias.as_str());
    if matches!(old, "default" | "claude") {
        return Err(CliError::usage(
            "the default profile (~/.claude) can't be renamed: its command is always `claude`",
        )
        .hint("mcc only renames profiles it created; see `mcc list`")
        .into());
    }
    let Some(profile) = cfg.profiles.get(old).cloned() else {
        return Err(super::unknown_profile(old));
    };
    validate::check_syntax(new)
        .map_err(|m| CliError::usage(m).hint(format!("example: mcc rename {old} {old}2")))?;
    if new == old {
        return Err(
            CliError::usage(format!("\"{old}\" is already called \"{new}\""))
                .hint("pass a different new name")
                .into(),
        );
    }
    let env = cfg.env.clone().expect("init runs before commands");
    let shell_path = (!env.shell_path.is_empty()).then_some(env.shell_path.as_str());

    ui::header(&format!(
        "Rename Claude Code profile \"{old}\" to \"{new}\""
    ));
    let mut steps = ui::Steps::new(5);
    let mut undo: Vec<Undo> = Vec::new();

    // rc files that hold the old alias block: the one recorded for the
    // profile, plus the current shell's (in case it changed since `add`).
    let mut rc_files: Vec<PathBuf> = profile.rc_file.iter().cloned().collect();
    if let Some(rc) = &env.rc_file
        && !rc_files.contains(rc)
    {
        rc_files.push(rc.clone());
    }
    rc_files.retain(|rc| shell::has_block(rc, old));

    let result = (|| -> Result<PathBuf> {
        let step = steps.start("Validate new name");
        let conflicts = validate::conflicts_for(new, paths, cfg, shell_path, Some(&profile.dir));
        if !conflicts.is_empty() {
            step.fail("name is taken");
            let reasons: Vec<&str> = conflicts.iter().map(|c| c.reason.as_str()).collect();
            return Err(CliError::usage(format!(
                "\"{new}\" can't be used: {}",
                reasons.join("; ")
            ))
            .hint(format!(
                "choose another name, e.g. `mcc rename {old} {new}2`"
            ))
            .into());
        }
        step.ok(&format!("\"{new}\" is available"));

        let step = steps.start(&format!(
            "Install launcher {}",
            paths.display(&paths.wrapper(new))
        ));
        let wrapper = shell::write_wrapper(
            paths,
            new,
            &profile.dir,
            claude::find_binary(env.claude_bin.as_deref()).as_deref(),
        )?;
        undo.push(Undo::Wrapper(wrapper.clone()));
        step.ok("");

        let step = steps.start("Add shell alias");
        match env.shell {
            Some(sh) if !rc_files.is_empty() => {
                for rc in &rc_files {
                    shell::upsert_block(
                        paths,
                        rc,
                        new,
                        &shell::alias_block(paths, sh, new, &profile.dir, Some(&wrapper)),
                    )?;
                    undo.push(Undo::RcBlock(rc.clone(), new.to_string()));
                }
                let shown: Vec<String> = rc_files.iter().map(|rc| paths.display(rc)).collect();
                step.ok(&shown.join(", "));
            }
            Some(_) => step.skip("the profile had no alias block — the launcher still works"),
            None => step.skip("unsupported shell — the launcher command still works"),
        }

        let step = steps.start("Update registry");
        let mut renamed = profile.clone();
        renamed.wrapper = Some(wrapper.clone());
        cfg.profiles.remove(old);
        cfg.profiles.insert(new.to_string(), renamed);
        let projects = cfg.projects.clone();
        let repointed = cfg.rename_project_defaults(old, new);
        if let Err(e) = cfg.save(&paths.config_file) {
            cfg.profiles.remove(new);
            cfg.profiles.insert(old.to_string(), profile.clone());
            cfg.projects = projects;
            return Err(e);
        }
        let mut note = paths.display(&paths.config_file);
        if repointed > 0 {
            note.push_str(&format!(
                "; {repointed} project default{} now use{} `{new}`",
                if repointed == 1 { "" } else { "s" },
                if repointed == 1 { "s" } else { "" },
            ));
        }
        step.ok(&note);
        Ok(wrapper)
    })();

    if let Err(e) = result {
        rollback(paths, undo);
        return Err(e);
    }

    // The new command works from here on; leftovers of the old one are
    // warnings, not failures.
    let mut warnings = 0;
    let step = steps.start(&format!("Remove the old command `{old}`"));
    let mut removed = Vec::new();
    let mut problems = Vec::new();
    let old_wrapper = profile
        .wrapper
        .clone()
        .unwrap_or_else(|| paths.wrapper(old));
    if std::fs::symlink_metadata(&old_wrapper).is_ok() {
        if !shell::is_mcc_wrapper(&old_wrapper) {
            problems.push(format!(
                "{} was not created by mcc — left untouched",
                paths.display(&old_wrapper)
            ));
        } else {
            match fsutil::remove_any(&old_wrapper) {
                Ok(()) => removed.push(paths.display(&old_wrapper)),
                Err(e) => problems.push(format!(
                    "could not remove {}: {e:#}",
                    paths.display(&old_wrapper)
                )),
            }
        }
    }
    for rc in &rc_files {
        match shell::remove_block(paths, rc, old) {
            Ok(_) => removed.push(format!("alias in {}", paths.display(rc))),
            Err(e) => problems.push(format!(
                "could not remove the alias from {}: {e:#}",
                paths.display(rc)
            )),
        }
    }
    if !problems.is_empty() {
        warnings += problems.len();
        step.warn(&problems.join("; "));
    } else if removed.is_empty() {
        step.skip("nothing left to remove");
    } else {
        step.ok(&removed.join(", "));
    }

    ui::result(
        warnings == 0,
        &format!(
            "profile \"{old}\" is now \"{new}\" — start it with `{new}`{}",
            if warnings > 0 {
                format!(" ({warnings} warning(s) — see above)")
            } else {
                String::new()
            }
        ),
    );
    ui::kv("Command", new);
    ui::kv(
        "Directory",
        &format!("{} (unchanged)", paths.display(&profile.dir)),
    );
    if !rc_files.is_empty() {
        ui::hint(&format!(
            "open a new terminal (or `unalias {old}` and `source` your rc file) so the shell picks up `{new}`"
        ));
    }
    Ok(())
}
