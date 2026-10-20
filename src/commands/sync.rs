//! `mcc sync` — one-way copy of the default profile's setup into profiles,
//! three-way against the profile's sync manifest (`.mcc-sync.json`): changes
//! in the default are copied, edits made in the profile are kept, and paths
//! changed on both sides are conflicts, kept unless `--force`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cli::SyncArgs;
use crate::config::Config;
use crate::copy::{self, Action, Base, Plan, Rewriter, Spec, Status};
use crate::error::CliError;
use crate::manifest::{self, Manifest};
use crate::paths::Paths;
use crate::{fsutil, ui};

/// Profile config files backed up before every sync that writes, even when the
/// plan leaves them as they are. Everything the plan overwrites or removes is
/// backed up as well (see [`backup`]).
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
            return Err(super::unknown_profile(&alias));
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
                let (_, hint) = crate::error::classify(&e);
                ui::error(&format!("{alias}: {e:#}"), hint.as_deref());
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

/// The profile's sync manifest, warning (and comparing two-way) when it
/// can't be read.
fn load_manifest(paths: &Paths, dir: &Path) -> Option<Manifest> {
    match manifest::load(dir) {
        manifest::Loaded::Found(m) => Some(m),
        manifest::Loaded::Missing => None,
        manifest::Loaded::Invalid(why) => {
            ui::warn(&format!(
                "ignoring {} ({why}); every difference is taken from the default this time",
                paths.display(&dir.join(manifest::FILE))
            ));
            None
        }
    }
}

/// Plan a sync of `dir` from the default, three-way against its manifest.
pub fn plan_for(
    paths: &Paths,
    cfg: &Config,
    dir: &Path,
    spec: &Spec,
    manifest: Option<&Manifest>,
    prune: bool,
    force: bool,
) -> Result<Plan> {
    let rw = Rewriter::for_profile(paths, cfg, dir)?;
    copy::plan_with(
        &paths.default_dir,
        &paths.default_json,
        dir,
        spec,
        &rw,
        prune,
        &Base { manifest, force },
    )
}

/// How a profile compares with the default (for `mcc list`): `None` when the
/// default or the profile dir is missing, or the comparison fails.
pub fn status(paths: &Paths, cfg: &Config, dir: &Path) -> Option<Status> {
    if !paths.default_dir.is_dir() || !dir.is_dir() {
        return None;
    }
    let spec = copy::spec(None, &cfg.sync).ok()?;
    let loaded = manifest::load(dir);
    plan_for(paths, cfg, dir, &spec, loaded.manifest(), false, false)
        .ok()
        .map(|p| p.status())
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "✓", or e.g. "3 behind, 2 local edits, 1 conflict".
pub fn status_text(s: &Status) -> String {
    let mut parts = Vec::new();
    if s.behind > 0 {
        parts.push(format!("{} behind", s.behind));
    }
    if s.local_edits > 0 {
        parts.push(plural(s.local_edits, "local edit", "local edits"));
    }
    if s.conflicts > 0 {
        parts.push(plural(s.conflicts, "conflict", "conflicts"));
    }
    if parts.is_empty() {
        "✓".into()
    } else {
        parts.join(", ")
    }
}

/// The local edits and conflicts a sync keeps, e.g. ", except 2 local edits
/// (kept)", or "".
fn kept_note(s: &Status) -> String {
    let mut parts = Vec::new();
    if s.local_edits > 0 {
        parts.push(format!(
            "{} (kept)",
            plural(s.local_edits, "local edit", "local edits")
        ));
    }
    if s.conflicts > 0 {
        parts.push(format!(
            "{} (skipped)",
            plural(s.conflicts, "conflict", "conflicts")
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(", except {}", parts.join(" and "))
    }
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
    let rw = Rewriter::for_profile(paths, cfg, &dir)?;
    if rw.has_overlay() && cfg.sync.exclude.iter().any(|g| g == "settings") {
        ui::warn(&format!(
            "\"{alias}\" has a settings overlay, but [sync] exclude has \"settings\", so sync leaves its settings.json alone and the overlay is not applied"
        ));
    }
    let mut steps = ui::Steps::new(if args.dry_run { 1 } else { 3 });

    let step = steps.start("Compare with the default profile");
    let previous = load_manifest(paths, &dir);
    let plan = plan_for(
        paths,
        cfg,
        &dir,
        spec,
        previous.as_ref(),
        args.prune,
        args.force,
    )?;
    let status = plan.status();
    let mut summary = format!(
        "{} to add, {} to update, {} to remove, {} unchanged",
        plan.count(Action::Add),
        plan.count(Action::Update),
        plan.count(Action::Remove),
        plan.unchanged
    );
    if status.local_edits > 0 {
        summary.push_str(&format!(
            ", {}",
            plural(status.local_edits, "local edit", "local edits")
        ));
    }
    if status.conflicts > 0 {
        summary.push_str(&format!(
            ", {}",
            plural(status.conflicts, "conflict", "conflicts")
        ));
    }
    step.ok(&summary);
    print_plan(&plan, args.dry_run);
    if status.conflicts > 0 {
        ui::hint(&format!(
            "conflicts changed in the default and in the profile; see them with `mcc diff {alias}`, or overwrite them with `mcc sync {alias} --force` (backed up first)"
        ));
    }
    let next = plan.manifest(previous.as_ref());

    if plan.actionable() == 0 {
        // Nothing to copy, but record what the profile matches, so the next
        // sync can tell its edits from the default's.
        if !args.dry_run && previous.as_ref() != Some(&next) {
            next.save(&dir)?;
        }
        ui::result(
            true,
            &format!("\"{alias}\" is already up to date{}", kept_note(&status)),
        );
        return Ok(());
    }
    if args.dry_run {
        ui::result(
            true,
            &format!(
                "dry run — {} change(s) would be applied to \"{alias}\"{}; nothing was written",
                plan.actionable(),
                kept_note(&status)
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
    match backup(&dir, &plan)? {
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
    next.save(&dir)?;
    step.ok(&format!("{} change(s)", plan.actionable()));

    if let Some(p) = cfg.profiles.get_mut(alias) {
        p.last_sync =
            Some(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
    }
    cfg.save(&paths.config_file)?;
    ui::result(
        true,
        &format!(
            "\"{alias}\" now matches the default profile's setup{}",
            kept_note(&status)
        ),
    );
    Ok(())
}

fn print_plan(plan: &Plan, dry_run: bool) {
    for (group, add, upd, rem, extra, local, conflict) in plan.by_group() {
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
        if local > 0 {
            parts.push(format!(
                "{} (kept)",
                plural(local, "local edit", "local edits")
            ));
        }
        if conflict > 0 {
            parts.push(format!(
                "{} (skipped; --force overwrites)",
                plural(conflict, "conflict", "conflicts")
            ));
        }
        if extra > 0 {
            parts.push(format!("{extra} only in profile (kept; --prune removes)"));
        }
        ui::kv(&group, &parts.join(", "));
    }
    if let Some(m) = &plan.mcp
        && !(m.is_empty() && m.local.is_empty() && m.conflicts.is_empty())
    {
        let mut parts = Vec::new();
        let list =
            |sign: &str, names: &[String]| format!("{sign}{} ({})", names.len(), names.join(", "));
        if !m.added.is_empty() {
            parts.push(list("+", &m.added));
        }
        if !m.updated.is_empty() {
            parts.push(list("~", &m.updated));
        }
        if !m.removed.is_empty() {
            parts.push(list("-", &m.removed));
        }
        if !m.local.is_empty() {
            parts.push(format!(
                "{} kept ({})",
                plural(m.local.len(), "local edit", "local edits"),
                m.local.join(", ")
            ));
        }
        if !m.conflicts.is_empty() {
            parts.push(format!(
                "{} skipped ({})",
                plural(m.conflicts.len(), "conflict", "conflicts"),
                m.conflicts.join(", ")
            ));
        }
        if !m.forced.is_empty() {
            parts.push(format!("--force overwrites {}", m.forced.join(", ")));
        }
        ui::kv("mcp", &parts.join(", "));
    }
    // List individual paths: in a dry run every change, otherwise the local
    // edits and conflicts kept and the conflicts --force overwrites (all with
    // --verbose, else the first 15).
    let limit = if ui::settings().verbose {
        usize::MAX
    } else {
        15
    };
    let listed: Vec<_> = plan
        .changes
        .iter()
        .filter(|c| {
            if dry_run {
                c.action != Action::Extra
            } else {
                matches!(c.action, Action::LocalEdit | Action::Conflict) || c.forced
            }
        })
        .collect();
    for c in listed.iter().take(limit) {
        let line = match (c.action, c.forced) {
            (_, true) => format!("~ {} (conflict, overwritten with --force)", c.rel.display()),
            (Action::Add, _) => format!("+ {}", c.rel.display()),
            (Action::Update, _) => format!("~ {}", c.rel.display()),
            (Action::LocalEdit, _) => format!("= {} (local edit, kept)", c.rel.display()),
            (Action::Conflict, _) => format!(
                "! {} (conflict: changed in the default and the profile; skipped)",
                c.rel.display()
            ),
            _ => format!("- {}", c.rel.display()),
        };
        ui::info(&line);
    }
    if listed.len() > limit {
        ui::hint(&format!(
            "…and {} more (use --verbose to list all)",
            listed.len() - limit
        ));
    }
}

/// Copy everything the sync is about to overwrite or remove — each path in
/// [`Plan::backup_paths`] (files, whole dirs, symlinks kept as symlinks) plus
/// the [`BACKED_UP`] config files — to `<profile>/.mcc-backup/<timestamp>/`,
/// at the same relative paths. Runs before anything is written, so a failed
/// backup leaves the profile untouched. Never reuses an earlier backup folder
/// (two syncs can land in one millisecond).
fn backup(dir: &Path, plan: &Plan) -> Result<Option<PathBuf>> {
    let rels = copy::outermost_existing(
        dir,
        BACKED_UP
            .iter()
            .map(PathBuf::from)
            .chain(plan.backup_paths(dir)),
    );
    if rels.is_empty() {
        return Ok(None);
    }
    let root = dir.join(copy::BACKUP_DIR);
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    fsutil::set_private_dir(&root)?;
    let base = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f").to_string();
    let mut dest = root.join(&base);
    let mut n = 1;
    loop {
        match std::fs::create_dir(&dest) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                dest = root.join(format!("{base}.{n}"));
                n += 1;
            }
            Err(e) => return Err(e).with_context(|| format!("creating {}", dest.display())),
        }
    }
    fsutil::set_private_dir(&dest)?;
    for rel in rels {
        fsutil::copy_path(&dir.join(&rel), &dest.join(&rel))?;
    }
    Ok(Some(dest))
}
