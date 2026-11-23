//! `mcc sync` — one-way copy of the default profile's setup into profiles,
//! three-way against the profile's sync manifest (`.mcc-sync.json`): changes
//! in the default are copied, edits made in the profile are kept, and paths
//! changed on both sides are conflicts, kept unless `--force`.
//!
//! `settings.json` goes key by key (#129): first the settings changed in
//! any profile are taken into the default ([`gather_settings`]: basic ones
//! as they are, shared secrets once the user agrees, account-bound ones
//! never), then every synced profile gets the default's keys.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::cli::SyncArgs;
use crate::commands::statusline;
use crate::config::Config;
use crate::copy::{self, Action, Base, Plan, Rewriter, Spec, Status};
use crate::error::CliError;
use crate::manifest::{self, Manifest};
use crate::paths::Paths;
use crate::settings_sync::{self, Category, Classifier, Outcome, SettingsView, Verdict};
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
    gather_settings(paths, cfg, &spec, args)?;

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

/// Maps a profile's settings value to the default's ([`Rewriter::to_default`]).
type ToDefault<'a> = Box<dyn Fn(&Value) -> Option<Value> + 'a>;

/// One profile's `settings.json`, as a source for [`gather_settings`].
struct SettingsSource {
    alias: String,
    dir: PathBuf,
    rw: Rewriter,
    view: SettingsView,
    manifest: Option<Manifest>,
}

/// Take the `settings.json` keys changed in any profile into the default's
/// `settings.json`, before the profiles are synced from it: every profile
/// is read, whichever ones this sync updates (see
/// [`settings_sync::decide`]). Basic keys are taken; a shared secret only
/// when the user agrees at a terminal (never with `--yes`, and a declined
/// value is not asked about again); account-bound keys never; a key
/// removed in a profile only with `--yes` or a confirmation. Each key is
/// reported with its category and what happened, never its value. The
/// default's settings.json is backed up before it is first written, and
/// written through a symlink (dotfiles). A dry run reports only.
fn gather_settings(paths: &Paths, cfg: &Config, spec: &Spec, args: &SyncArgs) -> Result<()> {
    let Some(item) = spec
        .items
        .iter()
        .find(|i| i.rel == Path::new(copy::SETTINGS_FILE))
    else {
        return Ok(());
    };
    let src = paths.default_dir.join(copy::SETTINGS_FILE);
    if !src.is_file() {
        return Ok(());
    }
    let Ok(Value::Object(mut default)) = serde_json::from_slice::<Value>(
        &std::fs::read(&src).with_context(|| format!("reading {}", src.display()))?,
    ) else {
        ui::warn(&format!(
            "{} is not a JSON object, so settings changed in profiles are not taken into it",
            paths.display(&src)
        ));
        return Ok(());
    };
    let mut sources = Vec::new();
    for (alias, p) in &cfg.profiles {
        if !p.dir.is_dir() {
            continue;
        }
        let read = || -> Result<Option<SettingsSource>> {
            let rw = Rewriter::for_profile(paths, cfg, &p.dir)?;
            let manifest = manifest::load(&p.dir).manifest().cloned();
            let key = manifest::key(&item.rel);
            let base = Base {
                manifest: manifest.as_ref().filter(|m| m.items.contains(&key)),
                force: false,
            };
            let dst = p.dir.join(copy::SETTINGS_FILE);
            Ok(
                copy::settings_view(&src, &dst, item.rewrite, &rw, &base)?.map(|view| {
                    SettingsSource {
                        alias: alias.clone(),
                        dir: p.dir.clone(),
                        rw,
                        view,
                        manifest,
                    }
                }),
            )
        };
        match read() {
            Ok(Some(s)) => sources.push(s),
            Ok(None) => {}
            Err(e) => ui::warn(&format!(
                "\"{alias}\": its settings are not read for changes to take into the default ({e:#})"
            )),
        }
    }
    let classifier = Classifier::new(&cfg.sync);
    let maps: Vec<ToDefault> = sources
        .iter()
        .map(|s| Box::new(|v: &Value| s.rw.to_default(v)) as ToDefault)
        .collect();
    let views: Vec<settings_sync::Source> = sources
        .iter()
        .zip(&maps)
        .map(|(s, f)| settings_sync::Source {
            alias: &s.alias,
            view: &s.view,
            to_default: f.as_ref(),
        })
        .collect();
    let decisions = settings_sync::decide(&classifier, &settings_sync::flatten(&default), &views);
    if decisions.is_empty() {
        return Ok(());
    }
    ui::header("Settings changed in profiles");
    let (mut set, mut remove) = (Vec::new(), Vec::new());
    for d in decisions {
        let outcome = match d.verdict {
            Verdict::AccountBound => {
                Outcome::Skipped("account-bound: never copied to another profile")
            }
            Verdict::Unmappable => Outcome::Skipped(
                "it holds paths of the profile that can't be mapped to the default's",
            ),
            Verdict::Conflict { with_default } => Outcome::Conflict { with_default },
            Verdict::Set(value) if d.category == Category::Basic => {
                set.push(value);
                Outcome::Synced
            }
            Verdict::Set(value) => {
                // A shared secret: the value each source has, by hash.
                let hashes: Vec<(&SettingsSource, Option<String>)> = sources
                    .iter()
                    .filter(|s| d.sources.contains(&s.alias))
                    .map(|s| (s, settings_sync::hash(s.view.have.get(&d.key))))
                    .collect();
                let declined = hashes.iter().all(|(s, h)| {
                    s.manifest
                        .as_ref()
                        .and_then(|m| m.declined_secrets.get(&d.key))
                        == h.as_ref()
                });
                if declined {
                    Outcome::Declined { earlier: true }
                } else if args.dry_run {
                    Outcome::Asked
                } else if !ui::is_interactive() {
                    Outcome::Skipped("a shared secret is only synced once you agree at a terminal")
                } else if ui::confirm(
                    &format!(
                        "Sync {} ({}) from {} to the default and every profile? Its value is not shown.",
                        d.key,
                        d.category.as_str(),
                        quoted(&d.sources)
                    ),
                    false,
                    "",
                )? {
                    set.push(value);
                    Outcome::Approved
                } else {
                    for (s, h) in hashes {
                        if let (Some(mut m), Some(h)) = (s.manifest.clone(), h) {
                            m.declined_secrets.insert(d.key.clone(), h);
                            m.save(&s.dir)?;
                        }
                    }
                    Outcome::Declined { earlier: false }
                }
            }
            Verdict::Remove(key) => {
                let ok = args.dry_run
                    || args.yes
                    || (ui::is_interactive()
                        && ui::confirm(
                            &format!(
                                "Remove {} ({}) from the default and every profile? It was removed in {}",
                                d.key,
                                d.category.as_str(),
                                quoted(&d.sources)
                            ),
                            false,
                            "",
                        )?);
                if ok {
                    remove.push(key);
                    Outcome::Removed
                } else if ui::is_interactive() {
                    Outcome::Skipped("removal not confirmed")
                } else {
                    Outcome::Skipped("removing it needs --yes or a terminal")
                }
            }
        };
        ui::info(&format!(
            "{}: {} · from {} · {}",
            d.key,
            d.category.as_str(),
            quoted(&d.sources),
            outcome.describe(args.dry_run, args.force)
        ));
    }
    if set.is_empty() && remove.is_empty() {
        return Ok(());
    }
    if args.dry_run {
        ui::hint(
            "a real sync takes these into the default's settings.json first, then into every profile it syncs",
        );
        return Ok(());
    }
    // Through a symlink: the user's own file gets the change, and stays linked.
    let real = std::fs::canonicalize(&src).with_context(|| format!("reading {}", src.display()))?;
    let backup = new_backup_dir(&paths.default_dir)?;
    fsutil::copy_path(&real, &backup.join(copy::SETTINGS_FILE))?;
    settings_sync::apply_to_default(&mut default, &set, &remove);
    let mut text = serde_json::to_vec_pretty(&Value::Object(default))?;
    text.push(b'\n');
    fsutil::write_atomic(&real, &text)?;
    ui::success(&format!(
        "updated the default's settings.json (backed up to {}); profiles get it as they sync",
        paths.display(&backup)
    ));
    Ok(())
}

/// `"a"`, or `"a", "b"`.
fn quoted(aliases: &[String]) -> String {
    aliases
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(", ")
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
/// The profile's linked items (see `mcc link`) are left out.
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
    // Linked items are the default's own files: never compared, written,
    // pruned or recorded in the manifest.
    let spec = spec.without(&super::link::linked(cfg, dir));
    copy::plan_with(
        &paths.default_dir,
        &paths.default_json,
        dir,
        &spec,
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

/// Whether the profile's settings overlay replaces the `mcc statusline` shim
/// the default's `settings.json` has: the default's `statusLine.command` is
/// the shim, and the merged (overlaid) one is not. `false` without an overlay,
/// when settings aren't synced, or when either file can't be read (the plan
/// reports that).
fn overlay_drops_shim(paths: &Paths, spec: &Spec, rw: &Rewriter) -> bool {
    let src = paths.default_dir.join(copy::SETTINGS_FILE);
    let Some(item) = spec
        .items
        .iter()
        .find(|i| i.rel == Path::new(copy::SETTINGS_FILE))
    else {
        return false;
    };
    if !rw.overlays(&src) {
        return false;
    }
    let command = |bytes: &[u8]| {
        serde_json::from_slice::<serde_json::Value>(bytes)
            .ok()
            .and_then(|v| {
                v.get("statusLine")?
                    .get("command")?
                    .as_str()
                    .map(str::to_owned)
            })
    };
    let shim = |c: &Option<String>| c.as_deref().is_some_and(statusline::is_shim);
    let Ok(base) = std::fs::read(&src) else {
        return false;
    };
    if !shim(&command(&base)) {
        return false;
    }
    copy::desired_bytes(&src, item.rewrite, rw).is_ok_and(|merged| !shim(&command(&merged)))
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
    if rw.has_overlay()
        && spec
            .items
            .iter()
            .any(|i| i.rel == Path::new(copy::SETTINGS_FILE))
        && !paths.default_dir.join(copy::SETTINGS_FILE).is_file()
    {
        ui::warn(&format!(
            "\"{alias}\" has a settings overlay, but the default profile has no settings.json to merge it onto, so it is not applied"
        ));
    }
    if overlay_drops_shim(paths, spec, &rw) {
        ui::warn(&format!(
            "\"{alias}\": the settings overlay sets statusLine.command, which replaces the mcc statusline shim, so usage for this profile is no longer cached; leave statusLine.command out of the overlay to keep it"
        ));
    }
    super::link::report_in_sync(paths, cfg, alias, spec);
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
    if previous.is_none() {
        warn_replaced(paths, alias, &dir, &plan, args.dry_run);
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

/// What a launch-time catch-up ([`auto_catch_up`]) did for one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoOutcome {
    /// Copied `changed` from the default: profile-relative paths, then
    /// `MCP server "NAME"` for each server added or updated. `status` is
    /// what the profile still differs in (the kept local edits and
    /// conflicts).
    Applied {
        changed: Vec<String>,
        status: Status,
    },
    /// Nothing to copy; `status` as in `Applied`.
    UpToDate { status: Status },
    /// Nothing written: catching up needs what only `mcc sync` does.
    Held { reason: Held },
}

/// Why a launch-time catch-up wrote nothing ([`AutoOutcome::Held`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// No sync manifest (a `--fresh` profile never synced, or one from
    /// before manifests): every difference would be taken from the default,
    /// local edits included, so only `mcc sync` (which warns) does that.
    NoManifest,
    /// The manifest can't be read (or is from a newer mcc).
    InvalidManifest,
    /// The sync would remove something from the profile (a symlink or file
    /// where the default has a directory, say); removals need `mcc sync`'s
    /// confirmation.
    Removals,
    /// The sync would overwrite what the profile has in an item its manifest
    /// doesn't record (a new `[sync] extra` path, say), where local edits
    /// can't be told apart from what mcc wrote; only `mcc sync` (which
    /// warns) does that.
    Untracked,
}

impl Held {
    /// Its name in the auto-sync state file and in `mcc list --json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Held::NoManifest => "no_manifest",
            Held::InvalidManifest => "invalid_manifest",
            Held::Removals => "removals",
            Held::Untracked => "untracked",
        }
    }

    /// The reason named `name` ([`Held::as_str`]).
    pub fn from_name(name: &str) -> Option<Held> {
        [
            Held::NoManifest,
            Held::InvalidManifest,
            Held::Removals,
            Held::Untracked,
        ]
        .into_iter()
        .find(|h| h.as_str() == name)
    }

    /// Why, for people: "it has no sync record yet".
    pub fn describe(self) -> &'static str {
        match self {
            Held::NoManifest => "it has no sync record yet",
            Held::InvalidManifest => "its sync record can't be read",
            Held::Removals => "the sync would remove files",
            Held::Untracked => "it would overwrite files its sync record doesn't cover",
        }
    }
}

/// The safe part of `mcc sync ALIAS`, run when a profile starts (`[sync]
/// auto`): with the profile's sync manifest only (three-way), never pruning,
/// never forcing, and only when nothing would be removed and nothing outside
/// what the manifest records would be overwritten. Adds and updates
/// are copied (backed up first, as `mcc sync` does); local edits and
/// conflicts are kept. Records the manifest and, when it copied something,
/// `last_sync` in `cfg`, which it saves.
///
/// `settings.json` is caught up key by key, its account-bound keys kept;
/// settings changed in profiles are never taken into the default here
/// (only `mcc sync` does that, asking about secrets): the default is never
/// written.
///
/// Silent: no output, no prompt. The caller holds the config lock and
/// loaded `cfg` under it.
pub fn auto_catch_up(paths: &Paths, cfg: &mut Config, alias: &str) -> Result<AutoOutcome> {
    let dir = cfg
        .profiles
        .get(alias)
        .map(|p| p.dir.clone())
        .ok_or_else(|| super::unknown_profile(alias))?;
    if !dir.is_dir() || !paths.default_dir.is_dir() {
        return Err(CliError::runtime(format!(
            "{} or the default profile is missing",
            paths.display(&dir)
        ))
        .into());
    }
    let previous = match manifest::load(&dir) {
        manifest::Loaded::Found(m) => m,
        manifest::Loaded::Missing => {
            return Ok(AutoOutcome::Held {
                reason: Held::NoManifest,
            });
        }
        manifest::Loaded::Invalid(_) => {
            return Ok(AutoOutcome::Held {
                reason: Held::InvalidManifest,
            });
        }
    };
    let spec = copy::spec(None, &cfg.sync)?;
    let rw = Rewriter::for_profile(paths, cfg, &dir)?;
    let plan = plan_for(paths, cfg, &dir, &spec, Some(&previous), false, false)?;
    // Anything the profile has that applying the plan deletes: a removal,
    // a file or symlink of the profile replaced by the default's directory,
    // or one standing where a copied path needs its parent dir.
    let removals = plan.deletes(&dir) + plan.mcp.as_ref().map_or(0, |m| m.removed.len());
    if removals > 0 {
        return Ok(AutoOutcome::Held {
            reason: Held::Removals,
        });
    }
    // Items the manifest doesn't record (a new `[sync] extra` path, one no
    // longer excluded, a manifest written by `mcc sync --only`) are compared
    // two-way: what the profile has there differently would be overwritten
    // as if mcc had written it, its own edits included. Adding what the
    // profile lacks is safe; replacing what it has is left to `mcc sync`.
    let untracked: Vec<&Path> = spec
        .items
        .iter()
        .map(|i| i.rel.as_path())
        .filter(|rel| !previous.items.contains(&manifest::key(rel)))
        .collect();
    let overwrites_untracked = plan
        .changes
        .iter()
        .any(|c| c.action == Action::Update && untracked.iter().any(|u| c.rel.starts_with(u)))
        || (!previous.items.contains(manifest::MCP_ITEM)
            && plan.mcp.as_ref().is_some_and(|m| !m.updated.is_empty()));
    if overwrites_untracked {
        return Ok(AutoOutcome::Held {
            reason: Held::Untracked,
        });
    }
    let status = plan.status();
    let kept = Status {
        behind: 0,
        ..status
    };
    let next = plan.manifest(Some(&previous));
    if plan.actionable() == 0 {
        // As `mcc sync` does: record what the profile matches.
        if next != previous {
            next.save(&dir)?;
        }
        return Ok(AutoOutcome::UpToDate { status: kept });
    }
    backup_as(&dir, &plan, AUTO_BACKUP_SUFFIX)?;
    copy::apply(&plan, &paths.default_dir, &dir, &rw, false)?;
    next.save(&dir)?;
    // Catch-ups run unasked on every change of the default: only their
    // latest backups are kept. The profile is caught up either way.
    if let Err(e) = prune_auto_backups(&dir, AUTO_BACKUPS_KEPT) {
        crate::log::write(
            "WARN",
            &format!("auto sync: {alias}: pruning old auto-sync backups failed: {e:#}"),
        );
    }
    let mut changed: Vec<String> = plan
        .changes
        .iter()
        .filter(|c| !c.action.kept() && !c.dir)
        .map(|c| c.rel.display().to_string())
        .collect();
    if let Some(m) = &plan.mcp {
        changed.extend(
            m.added
                .iter()
                .chain(&m.updated)
                .map(|k| format!("MCP server \"{k}\"")),
        );
    }
    if let Some(p) = cfg.profiles.get_mut(alias) {
        p.last_sync =
            Some(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
    }
    cfg.save(&paths.config_file)?;
    Ok(AutoOutcome::Applied {
        changed,
        status: kept,
    })
}

/// Without a usable sync manifest the plan is two-way, so every path that
/// differs from the default is taken from it, local edits included: list
/// each path (and MCP server) whose profile content a sync replaces, and
/// where it is backed up. Warnings, so `--quiet` still shows them; nothing
/// when the sync only adds.
fn warn_replaced(paths: &Paths, alias: &str, dir: &Path, plan: &Plan, dry_run: bool) {
    let mut replaced: Vec<String> = plan
        .changes
        .iter()
        .filter(|c| c.action == Action::Update)
        .map(|c| c.rel.display().to_string())
        .collect();
    if let Some(m) = &plan.mcp {
        replaced.extend(
            m.updated
                .iter()
                .map(|k| format!("MCP server \"{k}\" (.claude.json)")),
        );
    }
    if replaced.is_empty() {
        return;
    }
    let backup = paths.display(&dir.join(copy::BACKUP_DIR));
    ui::warn(&format!(
        "\"{alias}\" has no usable sync record ({}), so this sync can't tell local edits from the default's changes: {} {}",
        paths.display(&dir.join(manifest::FILE)),
        plural(replaced.len(), "path", "paths"),
        if dry_run {
            format!(
                "would be replaced by the default's version (a real sync backs up the profile's copy first, to {backup}/):"
            )
        } else {
            format!("will be replaced by the default's version (backed up first, to {backup}/):")
        }
    ));
    for r in &replaced {
        ui::warn(&format!("  ~ {r}"));
    }
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
    if let Some(s) = &plan.settings
        && s.differs()
    {
        let mut parts = Vec::new();
        let list =
            |sign: &str, keys: &[String]| format!("{sign}{} ({})", keys.len(), keys.join(", "));
        if !s.added.is_empty() {
            parts.push(list("+", &s.added));
        }
        if !s.updated.is_empty() {
            parts.push(list("~", &s.updated));
        }
        if !s.removed.is_empty() {
            parts.push(list("-", &s.removed));
        }
        if !s.local.is_empty() {
            parts.push(format!(
                "{} kept ({})",
                plural(s.local.len(), "local edit", "local edits"),
                s.local.join(", ")
            ));
        }
        if !s.conflicts.is_empty() {
            parts.push(format!(
                "{} skipped ({})",
                plural(s.conflicts.len(), "conflict", "conflicts"),
                s.conflicts.join(", ")
            ));
        }
        if !s.forced.is_empty() {
            parts.push(format!("--force overwrites {}", s.forced.join(", ")));
        }
        ui::kv(copy::SETTINGS_FILE, &parts.join(", "));
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
    backup_as(dir, plan, "")
}

/// [`backup`], to `<timestamp><suffix>/`.
fn backup_as(dir: &Path, plan: &Plan, suffix: &str) -> Result<Option<PathBuf>> {
    backup_rels_as(
        dir,
        BACKED_UP
            .iter()
            .map(PathBuf::from)
            .chain(plan.backup_paths(dir)),
        suffix,
    )
}

/// Copy the paths `rels` (relative to the profile `dir`; missing ones and
/// ones inside another are skipped, symlinks are copied as symlinks, never
/// followed) to a new `<dir>/.mcc-backup/<timestamp>/`. `None` when none of
/// them exists.
pub fn backup_rels(dir: &Path, rels: impl IntoIterator<Item = PathBuf>) -> Result<Option<PathBuf>> {
    backup_rels_as(dir, rels, "")
}

/// [`backup_rels`], to `<timestamp><suffix>/`.
fn backup_rels_as(
    dir: &Path,
    rels: impl IntoIterator<Item = PathBuf>,
    suffix: &str,
) -> Result<Option<PathBuf>> {
    let rels = copy::outermost_existing(dir, rels);
    if rels.is_empty() {
        return Ok(None);
    }
    let dest = new_backup_dir_as(dir, suffix)?;
    for rel in rels {
        fsutil::copy_path(&dir.join(&rel), &dest.join(&rel))?;
    }
    Ok(Some(dest))
}

/// The format of a backup folder's timestamp.
const BACKUP_TIME: &str = "%Y%m%d-%H%M%S%.3f";

/// What ends the name of a backup folder auto sync made
/// (`<timestamp>-auto`, or `<timestamp>-auto.N` on a clash).
const AUTO_BACKUP_SUFFIX: &str = "-auto";

/// How many of its backups auto sync keeps per profile
/// ([`prune_auto_backups`]). Backups of `mcc sync` (and of `mcc link`) are
/// never pruned.
pub const AUTO_BACKUPS_KEPT: usize = 10;

/// When the backup folder `name` was made by auto sync: its timestamp and
/// clash number (`<timestamp>-auto` is 0, `<timestamp>-auto.N` is N).
fn auto_backup_key(name: &str) -> Option<(chrono::NaiveDateTime, u64)> {
    let (stamp, n) = match name.strip_suffix(AUTO_BACKUP_SUFFIX) {
        Some(stamp) => (stamp, 0),
        None => {
            let (head, n) = name.rsplit_once('.')?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (head.strip_suffix(AUTO_BACKUP_SUFFIX)?, n.parse().ok()?)
        }
    };
    let time = chrono::NaiveDateTime::parse_from_str(stamp, BACKUP_TIME).ok()?;
    // Exactly as mcc names them: nothing else is ever touched.
    (time.format(BACKUP_TIME).to_string() == stamp).then_some((time, n))
}

/// Remove all but the newest `keep` backup folders auto sync made in
/// `<dir>/.mcc-backup/` (real directories named as [`auto_backup_key`]
/// reads them; never a symlink, nor any other backup). Returns how many
/// it removed.
pub fn prune_auto_backups(dir: &Path, keep: usize) -> Result<usize> {
    let root = dir.join(copy::BACKUP_DIR);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("reading {}", root.display())),
    };
    let mut auto = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", root.display()))?;
        let Some(key) = entry.file_name().to_str().and_then(auto_backup_key) else {
            continue;
        };
        if std::fs::symlink_metadata(entry.path()).is_ok_and(|m| m.file_type().is_dir()) {
            auto.push((key, entry.path()));
        }
    }
    auto.sort();
    let stale = auto.len().saturating_sub(keep);
    for (_, path) in &auto[..stale] {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(stale)
}

/// A new, empty, private `<dir>/.mcc-backup/<timestamp>/` (with a `.N`
/// suffix when that name is taken).
pub fn new_backup_dir(dir: &Path) -> Result<PathBuf> {
    new_backup_dir_as(dir, "")
}

/// [`new_backup_dir`], named `<timestamp><suffix>` (`.N` after it on a
/// clash).
fn new_backup_dir_as(dir: &Path, suffix: &str) -> Result<PathBuf> {
    let root = dir.join(copy::BACKUP_DIR);
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    fsutil::set_private_dir(&root)?;
    let base = format!("{}{suffix}", chrono::Local::now().format(BACKUP_TIME));
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
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_auto_sync_backups_are_pruned_and_the_latest_kept() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join(copy::BACKUP_DIR);
        let mut made = Vec::new();
        for i in 0..12 {
            let name = format!("20260101-0000{i:02}.000-auto");
            std::fs::create_dir_all(root.join(&name).join("sub")).unwrap();
            std::fs::write(root.join(&name).join("sub/f"), "x").unwrap();
            made.push(name);
        }
        // A clash in one millisecond, after the plain name.
        std::fs::create_dir(root.join("20260101-000011.000-auto.1")).unwrap();
        made.push("20260101-000011.000-auto.1".into());
        // Never touched: `mcc sync` backups, look-alikes, a symlink.
        let manual = "20250101-000000.000";
        let others = [
            manual,
            "20250101-000000.000.1",
            "notes-auto",
            "20250101-000000-auto",
            "20250101-000000.000-auto.x",
            "x20250101-000000.000-auto",
        ];
        for name in others {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let elsewhere = d.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("20240101-000000.000-auto")).unwrap();

        assert_eq!(prune_auto_backups(d.path(), 10).unwrap(), 3);
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut want: Vec<String> = made[3..].to_vec();
        want.extend(others.iter().map(|s| s.to_string()));
        want.push("20240101-000000.000-auto".into());
        want.sort();
        assert_eq!(left, want);
        assert!(elsewhere.is_dir());
        // Nothing more to prune; none at all is fine too.
        assert_eq!(prune_auto_backups(d.path(), 10).unwrap(), 0);
        assert_eq!(prune_auto_backups(&d.path().join("none"), 10).unwrap(), 0);
    }

    #[test]
    fn an_auto_sync_backup_is_named_apart_from_the_others() {
        let d = tempfile::tempdir().unwrap();
        let auto = new_backup_dir_as(d.path(), AUTO_BACKUP_SUFFIX).unwrap();
        let again = new_backup_dir_as(d.path(), AUTO_BACKUP_SUFFIX).unwrap();
        let manual = new_backup_dir(d.path()).unwrap();
        for dir in [&auto, &again] {
            let name = dir.file_name().unwrap().to_str().unwrap();
            assert!(auto_backup_key(name).is_some(), "{name}");
        }
        let name = manual.file_name().unwrap().to_str().unwrap();
        assert_eq!(auto_backup_key(name), None, "{name}");
    }
}
