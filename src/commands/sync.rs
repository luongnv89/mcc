//! `mcc sync` — one-way copy of the default profile's setup into profiles.

use std::path::Path;

use anyhow::Result;

use crate::cli::SyncArgs;
use crate::config::Config;
use crate::copy::{self, Action, Plan, Rewriter, Spec};
use crate::error::CliError;
use crate::paths::Paths;
use crate::{fsutil, ui};

/// Files backed up before a sync overwrites them.
const BACKED_UP: &[&str] = &["settings.json", "settings.local.json", ".claude.json"];

pub fn run(paths: &Paths, cfg: &mut Config, args: &SyncArgs) -> Result<()> {
    if !paths.default_dir.is_dir() {
        return Err(CliError::runtime(format!(
            "the default profile {} does not exist, so there is nothing to sync from",
            paths.display(&paths.default_dir)
        ))
        .hint("run `mcc add` to create it first")
        .into());
    }
    let targets: Vec<String> = if args.all {
        cfg.profiles.keys().cloned().collect()
    } else {
        let alias = args.alias.clone().expect("clap requires alias or --all");
        if matches!(alias.as_str(), "default" | "claude") {
            return Err(CliError::usage(
                "the default profile is the sync source; pick another profile",
            )
            .hint("example: mcc sync work")
            .into());
        }
        if !cfg.profiles.contains_key(&alias) {
            return Err(CliError::usage(format!("no profile named \"{alias}\""))
                .hint("run `mcc list` to see your profiles")
                .into());
        }
        vec![alias]
    };
    if targets.is_empty() {
        ui::info("no mcc profiles yet — nothing to sync (add one with `mcc add <alias>`)");
        return Ok(());
    }
    let spec = copy::spec(args.only.as_deref(), &cfg.sync)?;

    let mut failed = Vec::new();
    for (i, alias) in targets.iter().enumerate() {
        let label = if targets.len() > 1 {
            format!(" ({}/{})", i + 1, targets.len())
        } else {
            String::new()
        };
        ui::header(&format!("Sync default → \"{alias}\"{label}"));
        match sync_one(paths, cfg, alias, &spec, args) {
            Ok(()) => {}
            Err(e)
                if targets.len() > 1
                    && crate::error::classify(&e).0 != crate::error::EXIT_USAGE =>
            {
                ui::error(&format!("{alias}: {e:#}"), None);
                failed.push(alias.clone());
            }
            Err(e) => return Err(e),
        }
    }
    if targets.len() > 1 {
        let ok = targets.len() - failed.len();
        ui::result(
            failed.is_empty(),
            &format!(
                "{ok}/{} profiles synced{}",
                targets.len(),
                if failed.is_empty() {
                    String::new()
                } else {
                    format!(" — failed: {}", failed.join(", "))
                }
            ),
        );
        if !failed.is_empty() {
            return Err(
                CliError::runtime(format!("sync failed for {}", failed.join(", ")))
                    .hint("fix the errors shown above, then re-run `mcc sync --all`")
                    .into(),
            );
        }
    }
    Ok(())
}

fn sync_one(
    paths: &Paths,
    cfg: &mut Config,
    alias: &str,
    spec: &Spec,
    args: &SyncArgs,
) -> Result<()> {
    let dir = cfg.profiles[alias].dir.clone();
    if !dir.is_dir() {
        return Err(CliError::runtime(format!(
            "profile directory {} is missing",
            paths.display(&dir)
        ))
        .hint(format!(
            "remove the stale entry with `mcc delete {alias} --yes`"
        ))
        .into());
    }
    let rw = Rewriter::new(paths, &dir, &cfg.sync);
    let mut steps = ui::Steps::new(if args.dry_run { 1 } else { 3 });

    let step = steps.start("Compare with the default profile");
    let plan = copy::plan(
        &paths.default_dir,
        &paths.default_json,
        &dir,
        spec,
        &rw,
        args.prune,
    )?;
    step.ok(&format!(
        "{} to add, {} to update, {} to remove, {} unchanged",
        plan.count(Action::Add),
        plan.count(Action::Update),
        plan.count(Action::Remove),
        plan.unchanged
    ));
    print_plan(&plan, args.dry_run);

    if plan.actionable() == 0 {
        ui::result(true, &format!("\"{alias}\" is already up to date"));
        return Ok(());
    }
    if args.dry_run {
        ui::result(
            true,
            &format!(
                "dry run — {} change(s) would be applied to \"{alias}\"; nothing was written",
                plan.actionable()
            ),
        );
        return Ok(());
    }
    let removals = plan.count(Action::Remove) + plan.mcp.as_ref().map_or(0, |m| m.removed.len());
    if removals > 0 && !args.yes {
        ui::require_interactive(
            "confirmation to remove files",
            "pass --yes to allow --prune removals",
        )?;
        if !ui::confirm(
            &format!("Remove {removals} item(s) from \"{alias}\" that the default doesn't have?"),
            false,
            "",
        )? {
            return Err(CliError::runtime("cancelled — nothing was written")
                .hint(format!(
                    "re-run `mcc sync {alias}` and confirm, or pass --yes to allow the removals"
                ))
                .into());
        }
    }

    let step = steps.start("Back up profile config");
    match backup(&dir)? {
        Some(b) => step.ok(&paths.display(&b)),
        None => step.skip("nothing to back up"),
    }

    let step = steps.start("Apply changes");
    copy::apply(
        &plan,
        &paths.default_dir,
        &dir,
        &rw,
        ui::is_interactive() && !ui::settings().quiet,
    )?;
    step.ok(&format!("{} change(s)", plan.actionable()));

    if let Some(p) = cfg.profiles.get_mut(alias) {
        p.last_sync =
            Some(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
    }
    cfg.save(&paths.config_file)?;
    ui::result(
        true,
        &format!("\"{alias}\" now matches the default profile's setup"),
    );
    Ok(())
}

fn print_plan(plan: &Plan, dry_run: bool) {
    for (group, add, upd, rem, extra) in plan.by_group() {
        let mut parts = Vec::new();
        if add > 0 {
            parts.push(format!("+{add} new"));
        }
        if upd > 0 {
            parts.push(format!("~{upd} changed"));
        }
        if rem > 0 {
            parts.push(format!("-{rem} removed"));
        }
        if extra > 0 {
            parts.push(format!("{extra} only in profile (kept; --prune removes)"));
        }
        ui::kv(&group, &parts.join(", "));
    }
    if let Some(m) = &plan.mcp
        && !m.is_empty()
    {
        let mut parts = Vec::new();
        if !m.added.is_empty() {
            parts.push(format!("+{} ({})", m.added.len(), m.added.join(", ")));
        }
        if !m.updated.is_empty() {
            parts.push(format!("~{} ({})", m.updated.len(), m.updated.join(", ")));
        }
        if !m.removed.is_empty() {
            parts.push(format!("-{} ({})", m.removed.len(), m.removed.join(", ")));
        }
        ui::kv("mcp", &parts.join(", "));
    }
    // List individual paths in dry-run (all with --verbose, else the first 15).
    if dry_run {
        let limit = if ui::settings().verbose {
            usize::MAX
        } else {
            15
        };
        let listed: Vec<_> = plan
            .changes
            .iter()
            .filter(|c| c.action != Action::Extra)
            .collect();
        for c in listed.iter().take(limit) {
            let sign = match c.action {
                Action::Add => "+",
                Action::Update => "~",
                _ => "-",
            };
            ui::info(&format!("{sign} {}", c.rel.display()));
        }
        if listed.len() > limit {
            ui::hint(&format!(
                "…and {} more (use --verbose to list all)",
                listed.len() - limit
            ));
        }
    }
}

/// Copy the profile's config files to `<profile>/.mcc-backup/<timestamp>/`.
/// Never reuses an earlier backup folder (two syncs can land in one millisecond).
fn backup(dir: &Path) -> Result<Option<std::path::PathBuf>> {
    let existing: Vec<&&str> = BACKED_UP.iter().filter(|f| dir.join(f).is_file()).collect();
    if existing.is_empty() {
        return Ok(None);
    }
    let root = dir.join(".mcc-backup");
    let base = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f").to_string();
    let mut dest = root.join(&base);
    let mut n = 1;
    while dest.exists() {
        dest = root.join(format!("{base}.{n}"));
        n += 1;
    }
    for f in existing {
        fsutil::write_atomic(&dest.join(f), &std::fs::read(dir.join(f))?)?;
    }
    Ok(Some(dest))
}
