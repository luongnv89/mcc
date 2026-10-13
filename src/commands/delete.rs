//! `mcc delete` — log out and remove everything belonging to a profile.

use std::path::{Component, Path};

use anyhow::Result;

use crate::claude::{self, Claude};
use crate::cli::DeleteArgs;
use crate::config::Config;
use crate::error::CliError;
use crate::paths::Paths;
use crate::{fsutil, shell, ui, validate};

/// Only a direct, named child of `profiles_dir` may be removed. The check is
/// lexical, so `..` or an absolute path in the alias or registry never matches.
fn is_profile_dir(paths: &Paths, dir: &Path) -> bool {
    dir.parent() == Some(paths.profiles_dir.as_path())
        && dir
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
}

pub fn run(paths: &Paths, cfg: &mut Config, args: &DeleteArgs) -> Result<()> {
    let alias = args.alias.as_str();
    if matches!(alias, "default" | "claude") {
        return Err(
            CliError::usage("the default profile (~/.claude) can't be deleted with mcc")
                .hint("mcc only deletes profiles it created; see `mcc list`")
                .into(),
        );
    }
    let registered = cfg.profiles.get(alias).cloned();
    if registered.is_none()
        && let Err(msg) = validate::check_syntax(alias)
    {
        return Err(CliError::usage(msg)
            .hint("run `mcc list` to see your profiles")
            .into());
    }
    let dir = registered
        .as_ref()
        .map_or_else(|| paths.profile_dir(alias), |p| p.dir.clone());
    if !is_profile_dir(paths, &dir) {
        return Err(CliError::input(format!(
            "refusing to delete {}: it is not a profile directory inside {}",
            paths.display(&dir),
            paths.display(&paths.profiles_dir)
        ))
        .hint(format!(
            "fix or remove the [profiles.{alias}] entry in {}",
            paths.display(&paths.config_file)
        ))
        .into());
    }
    if registered.is_none() && !dir.exists() {
        return Err(super::unknown_profile(alias));
    }
    // A renamed profile keeps its directory under the old name: never treat
    // it as a leftover of that name.
    if registered.is_none()
        && let Some((owner, _)) = cfg.profiles.iter().find(|(_, p)| p.dir == dir)
    {
        return Err(CliError::usage(format!(
            "no profile named \"{alias}\": {} belongs to profile \"{owner}\"",
            paths.display(&dir)
        ))
        .hint(format!("to delete that profile, run `mcc delete {owner}`"))
        .into());
    }

    ui::header(&format!("Delete Claude Code profile \"{alias}\""));
    let size = if dir.exists() {
        fsutil::dir_size(&dir)
    } else {
        0
    };
    ui::kv(
        "Directory",
        &format!("{} ({})", paths.display(&dir), ui::human_size(size)),
    );
    ui::kv("Command", alias);
    if registered.is_none() {
        ui::warn("not in the mcc registry — cleaning up the leftover directory");
    }

    if !args.yes {
        ui::require_interactive("confirmation", "pass --yes to delete without a prompt")?;
        let typed = ui::input(
            &format!("This permanently deletes \"{alias}\". Type the alias to confirm"),
            "",
        )?;
        if typed != alias {
            return Err(
                CliError::runtime("confirmation did not match — nothing was deleted")
                    .hint(format!(
                        "type \"{alias}\" exactly, or pass --yes to skip the prompt"
                    ))
                    .into(),
            );
        }
    }

    let mut steps = ui::Steps::new(5);
    let mut warnings = 0;

    let step = steps.start("Log out");
    let bin = claude::find_binary(cfg.env.as_ref().and_then(|e| e.claude_bin.as_deref()));
    match (&bin, args.keep_auth, dir.exists()) {
        (_, true, _) => step.skip("--keep-auth"),
        (_, _, false) => step.skip("directory already gone"),
        (None, ..) => {
            warnings += 1;
            step.warn("`claude` not found — stored credentials may remain in your keychain");
        }
        (Some(bin), ..) => {
            // `claude auth logout` exits 0 even when nothing was logged in,
            // so ask for the status first to report what really happened.
            let c = Claude {
                bin,
                paths,
                config_dir: Some(&dir),
            };
            let logged_in = c.status().map(|s| s.logged_in).unwrap_or(true);
            match if logged_in { c.logout() } else { Ok(false) } {
                Ok(true) => step.ok("credentials removed"),
                Ok(false) if !logged_in => step.skip("was not logged in"),
                Ok(false) => {
                    warnings += 1;
                    step.warn("`claude auth logout` failed — stored credentials may remain");
                }
                Err(e) => {
                    warnings += 1;
                    step.warn(&format!("{e:#}"));
                }
            }
        }
    }

    let step = steps.start("Remove launcher");
    let wrapper = registered
        .as_ref()
        .and_then(|p| p.wrapper.clone())
        .unwrap_or_else(|| paths.wrapper(alias));
    if std::fs::symlink_metadata(&wrapper).is_err() {
        step.skip("not present");
    } else if shell::is_mcc_wrapper(&wrapper) {
        fsutil::remove_any(&wrapper)?;
        step.ok(&paths.display(&wrapper));
    } else {
        warnings += 1;
        step.warn(&format!(
            "{} was not created by mcc — left untouched",
            paths.display(&wrapper)
        ));
    }

    let step = steps.start("Remove shell alias");
    let mut rc_files: Vec<_> = registered
        .as_ref()
        .and_then(|p| p.rc_file.clone())
        .into_iter()
        .collect();
    if let Some(rc) = cfg.env.as_ref().and_then(|e| e.rc_file.clone())
        && !rc_files.contains(&rc)
    {
        rc_files.push(rc);
    }
    let mut removed = Vec::new();
    // The PATH entry shares the block namespace; never strip it as an alias.
    if alias == shell::PATH_BLOCK {
        rc_files.clear();
    }
    for rc in &rc_files {
        if shell::remove_block(paths, rc, alias)? {
            removed.push(paths.display(rc));
        }
    }
    if removed.is_empty() {
        step.skip("no alias block found");
    } else {
        step.ok(&removed.join(", "));
    }

    let step = steps.start(&format!("Remove {}", paths.display(&dir)));
    if dir.exists() {
        fsutil::remove_any(&dir)?;
        step.ok(&format!("freed {}", ui::human_size(size)));
    } else {
        step.skip("not present");
    }

    let step = steps.start("Update registry");
    if cfg.profiles.remove(alias).is_some() {
        cfg.save(&paths.config_file)?;
        step.ok("");
    } else {
        step.skip("not registered");
    }

    ui::result(
        warnings == 0,
        &format!(
            "profile \"{alias}\" deleted{}",
            if warnings > 0 {
                format!(" with {warnings} warning(s) — see above")
            } else {
                String::new()
            }
        ),
    );
    if !removed.is_empty() {
        ui::hint(&format!(
            "open a new terminal (or `unalias {alias}`) so the old alias disappears"
        ));
    }
    Ok(())
}
