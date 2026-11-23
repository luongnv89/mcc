//! `mcc sync` — copy of the default profile's setup into profiles,
//! three-way against the profile's sync manifest (`.mcc-sync.json`): changes
//! in the default are copied, edits made in the profile are kept, and paths
//! changed on both sides are conflicts, kept unless `--force`.
//!
//! `settings.json` goes key by key (#129): first the settings changed in
//! any profile are taken into the default ([`gather_settings`]: basic ones
//! as they are, shared secrets once the user agrees, account-bound ones
//! never), then every synced profile gets the default's keys (a shared
//! secret new or changed in the default only once the user agrees,
//! [`ask_secrets`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::cli::SyncArgs;
use crate::commands::statusline;
use crate::config::Config;
use crate::copy::{self, Action, Base, Plan, Rewriter, Secrets, Spec, Status};
use crate::error::CliError;
use crate::manifest::{self, Manifest};
use crate::paths::Paths;
use crate::settings_sync::{self, Category, Classifier, Outcome, SettingsView, Verdict};
use crate::{fsutil, ui};

/// Profile config files backed up before every sync that writes, even when the
/// plan leaves them as they are. Everything the plan overwrites or removes is
/// backed up as well (see [`backup`]).
const BACKED_UP: &[&str] = &["settings.json", "settings.local.json", ".claude.json"];

/// How `mcc sync` asks the user to confirm a setting it takes into the
/// default, or a shared secret it copies from it: at the terminal, or
/// scripted in tests.
pub(crate) trait Ask {
    /// Whether there is someone to ask.
    fn interactive(&self) -> bool;
    /// Ask `prompt`; no by default.
    fn confirm(&mut self, prompt: &str) -> Result<bool>;
}

/// [`Ask`] at the terminal.
pub(crate) struct Terminal;

impl Ask for Terminal {
    fn interactive(&self) -> bool {
        ui::is_interactive()
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool> {
        ui::confirm(prompt, false, "")
    }
}

pub fn run(paths: &Paths, cfg: &mut Config, args: &SyncArgs) -> Result<()> {
    run_with(paths, cfg, args, &mut Terminal)
}

/// [`run`], asking through `ask`.
pub(crate) fn run_with(
    paths: &Paths,
    cfg: &mut Config,
    args: &SyncArgs,
    ask: &mut dyn Ask,
) -> Result<()> {
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
    // Shared secrets the user agreed to take into the default: they reach
    // every profile without being asked about again.
    let approved = gather_settings(paths, cfg, &spec, args, ask)?;

    let mut failed = Vec::new();
    for (i, alias) in targets.iter().enumerate() {
        let label = if targets.len() > 1 {
            format!(" ({}/{})", i + 1, targets.len())
        } else {
            String::new()
        };
        ui::header(&format!("Sync default → \"{alias}\"{label}"));
        match sync_one(paths, cfg, alias, &spec, args, &approved, ask) {
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
/// removed in a profile only once the user confirms it at a terminal
/// (never with `--yes`), and never from a profile whose settings.json has
/// no keys of its own (emptied); a key only a profile has, from before per-key
/// records while the default changed since ([`Verdict::Propose`]), the
/// same way (a declined value is not asked about again). A profile without a settings.json of its own (or
/// one that isn't a JSON object, or is the default's own file through a
/// symlink) is never a source. Each key is
/// reported with its category and what happened, never its value. The
/// default's settings.json is backed up before it is first written, and
/// written through a symlink (dotfiles); before that, every profile read
/// by guess gets a per-key record of this reading, so the change can't
/// flip it. A dry run reports only. Returns the shared secrets the user
/// agreed to take into the default.
fn gather_settings(
    paths: &Paths,
    cfg: &Config,
    spec: &Spec,
    args: &SyncArgs,
    ask: &mut dyn Ask,
) -> Result<BTreeSet<String>> {
    let mut approved = BTreeSet::new();
    let Some(item) = spec
        .items
        .iter()
        .find(|i| i.rel == Path::new(copy::SETTINGS_FILE))
    else {
        return Ok(approved);
    };
    let src = paths.default_dir.join(copy::SETTINGS_FILE);
    if !src.is_file() {
        return Ok(approved);
    }
    let Ok(Value::Object(mut default)) = serde_json::from_slice::<Value>(
        &std::fs::read(&src).with_context(|| format!("reading {}", src.display()))?,
    ) else {
        ui::warn(&format!(
            "{} is not a JSON object, so settings changed in profiles are not taken into it",
            paths.display(&src)
        ));
        return Ok(approved);
    };
    let classifier = Classifier::new(&cfg.sync);
    let mut sources = Vec::new();
    // Per-key records for the profiles read by guess, saved before the
    // default changes (see [`settings_sync::per_key_record`]).
    let mut guessed = Vec::new();
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
                ..Base::default()
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
            Ok(Some(s)) => {
                if let Some(record) = settings_sync::per_key_record(&s.view, &classifier) {
                    guessed.push((s.dir.clone(), record));
                }
                // A missing file is not a profile that removed every key.
                if s.view.have_obj.is_some() && !s.view.linked {
                    sources.push(s);
                }
            }
            Ok(None) => {}
            Err(e) => ui::warn(&format!(
                "\"{alias}\": its settings are not read for changes to take into the default ({e:#})"
            )),
        }
    }
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
    let default_flat = settings_sync::flatten(&default);
    let decisions = settings_sync::decide(&classifier, &default_flat, &views);
    if decisions.is_empty() {
        return Ok(approved);
    }
    ui::header("Settings changed in profiles");
    let (mut set, mut remove) = (Vec::new(), Vec::new());
    let mut unconfirmed = false;
    let mut unconfirmed_proposal = false;
    let mut emptied_source = false;
    let (mut conflict_default, mut conflict_profiles) = (false, false);
    for d in decisions {
        let outcome = match d.verdict {
            Verdict::AccountBound => {
                Outcome::Skipped("account-bound: never copied to another profile")
            }
            Verdict::Unmappable => Outcome::Skipped(
                "it holds paths of the profile that can't be mapped to the default's",
            ),
            Verdict::Conflict {
                with_default,
                default_has,
            } => {
                conflict_default |= with_default;
                conflict_profiles |= !with_default;
                Outcome::Conflict {
                    with_default,
                    default_has,
                }
            }
            Verdict::Set(value) if d.category == Category::Basic => {
                set.push(value);
                Outcome::Synced
            }
            // A shared secret, or a key only a profile has that the default
            // may have dropped: taken only once the user agrees.
            Verdict::Set(value) | Verdict::Propose(value) => {
                let secret = d.category != Category::Basic;
                // The value each source has, by hash.
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
                let prompt = if secret {
                    format!(
                        "Sync {} ({}) from {} to the default and every profile? Its value is not shown.",
                        d.key,
                        d.category.as_str(),
                        quoted(&d.sources)
                    )
                } else {
                    format!(
                        "Take {} ({}) from {} into the default? Only {} it: it may be new there, or one the default dropped. Its value is not shown.",
                        d.key,
                        d.category.as_str(),
                        quoted(&d.sources),
                        if d.sources.len() > 1 {
                            "those profiles have"
                        } else {
                            "that profile has"
                        }
                    )
                };
                if declined {
                    Outcome::Declined { earlier: true }
                } else if args.dry_run {
                    Outcome::Asked
                } else if !secret && (args.yes || !ask.interactive()) {
                    unconfirmed_proposal = true;
                    Outcome::Skipped("needs confirmation")
                } else if !ask.interactive() {
                    Outcome::Skipped("a shared secret is only synced once you agree at a terminal")
                } else if ask.confirm(&prompt)? {
                    if secret {
                        approved.insert(d.key.clone());
                    }
                    set.push(value);
                    Outcome::Approved
                } else {
                    for (s, h) in hashes {
                        if let Some(h) = h {
                            save_declined(&s.dir, &d.key, h)?;
                        }
                    }
                    Outcome::Declined { earlier: false }
                }
            }
            // A real sync removes it only once the user confirms it at a
            // terminal: --yes never does (the profile may have lost it).
            // A declined removal is not asked about again while the
            // default keeps the same value.
            Verdict::Remove(key) => {
                let mark = settings_sync::declined_removal(default_flat.get(&d.key));
                let from: Vec<&SettingsSource> = sources
                    .iter()
                    .filter(|s| d.sources.contains(&s.alias))
                    .collect();
                let declined = from.iter().all(|s| {
                    s.manifest
                        .as_ref()
                        .and_then(|m| m.declined_secrets.get(&d.key))
                        == Some(&mark)
                });
                // A settings.json with no keys of its own (`{}`, or only
                // account-bound and overlay keys) was emptied, not edited:
                // never a reason to remove anything from the default.
                let emptied = from.iter().any(|s| {
                    s.view
                        .have
                        .keys()
                        .all(|k| classifier.is_account_bound(k) || s.view.overlaid(k))
                });
                if emptied {
                    emptied_source = true;
                    Outcome::Skipped("its settings.json has no keys of its own")
                } else if declined {
                    Outcome::Declined { earlier: true }
                } else if args.dry_run {
                    Outcome::Asked
                } else if args.yes || !ask.interactive() {
                    unconfirmed = true;
                    Outcome::Skipped("needs confirmation")
                } else if ask.confirm(&format!(
                    "Remove {} ({}) from the default and every profile? It was removed in {}",
                    d.key,
                    d.category.as_str(),
                    quoted(&d.sources)
                ))? {
                    remove.push(key);
                    Outcome::Removed
                } else {
                    for s in from {
                        save_declined(&s.dir, &d.key, mark.clone())?;
                    }
                    Outcome::Declined { earlier: false }
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
    if unconfirmed {
        ui::hint(
            "a key removed in a profile leaves the default only once you confirm it: run `mcc sync` at a terminal, without --yes",
        );
    }
    if emptied_source {
        ui::hint(
            "a profile whose settings.json has no keys of its own is never read for removals, so an emptied file never empties the default; to remove a key everywhere, remove it from ~/.claude/settings.json",
        );
    }
    if unconfirmed_proposal {
        ui::hint(
            "a key only a profile has (recorded by an older mcc, and the default changed since) goes into the default only once you confirm it: run `mcc sync` at a terminal, without --yes, or add it to ~/.claude/settings.json yourself",
        );
    }
    if conflict_default {
        ui::hint(
            "a conflict stays until the profile and the default agree on the key: give it the same value in both (or remove it from both), or run `mcc sync ALIAS --force` to give the profile the default's (backed up first)",
        );
    }
    if conflict_profiles {
        ui::hint(
            "a key changed differently in several profiles stays as it is until they agree: give it the same value in each",
        );
    }
    if set.is_empty() && remove.is_empty() {
        return Ok(approved);
    }
    if args.dry_run {
        ui::hint(
            "a real sync takes these into the default's settings.json first, then into every profile it syncs",
        );
        return Ok(approved);
    }
    // The profiles read by guess compare with the default: give them the
    // per-key record of this reading first, so its change doesn't flip it.
    for (dir, record) in guessed {
        if let Some(mut m) = manifest::load(&dir).manifest().cloned() {
            m.items.insert(manifest::SETTINGS_ITEM.into());
            m.settings = record;
            m.save(&dir)?;
        }
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
    Ok(approved)
}

/// What the user said about the shared secrets the default sets that a
/// plan holds ([`Plan::held`]).
#[derive(Debug, Default)]
pub(crate) struct Consent {
    /// Each held secret, and what became of it.
    pub held: Vec<(copy::Held, Outcome)>,
    /// The [`copy::Held::id`]s agreed to.
    pub agreed: BTreeSet<String>,
    /// The `declined_secrets` entries to record: id and mark.
    pub declined: Vec<(String, String)>,
}

/// Plan with `plan`, which holds every shared secret the default sets
/// except those it is given; ask about each one it holds for `alias`, in
/// the same way as [`gather_settings`] asks about a profile's: a value
/// declined earlier (in `previous`) is not asked about again; a dry run
/// would ask; with no terminal (`--yes` never agrees) it is skipped. Then
/// plan again with those agreed to. `approved` were agreed to already.
pub(crate) fn plan_asking(
    alias: &str,
    approved: &BTreeSet<String>,
    previous: Option<&Manifest>,
    dry_run: bool,
    ask: &mut dyn Ask,
    plan: &mut dyn FnMut(&BTreeSet<String>) -> Result<Plan>,
) -> Result<(Plan, Consent)> {
    let first = plan(approved)?;
    let mut consent = Consent::default();
    for h in first.held() {
        let outcome = if previous.and_then(|m| m.declined_secrets.get(&h.id)) == Some(&h.mark) {
            Outcome::Declined { earlier: true }
        } else if dry_run {
            Outcome::Asked
        } else if !ask.interactive() {
            Outcome::Skipped("a shared secret is only synced once you agree at a terminal")
        } else if ask.confirm(&format!(
            "Sync {} ({}) from the default to \"{alias}\"? Its value is not shown.",
            h.label(),
            Category::SharedSecret.as_str()
        ))? {
            consent.agreed.insert(h.id.clone());
            Outcome::Approved
        } else {
            consent.declined.push((h.id.clone(), h.mark.clone()));
            Outcome::Declined { earlier: false }
        };
        consent.held.push((h, outcome));
    }
    if consent.agreed.is_empty() {
        return Ok((first, consent));
    }
    let all: BTreeSet<String> = approved.union(&consent.agreed).cloned().collect();
    Ok((plan(&all)?, consent))
}

/// Report each settings key the default gives the profile (or holds
/// back) with its category and what happened, as [`gather_settings`]
/// reports the keys taken from profiles; never a value. Keys the profile's
/// overlay sets and conflicts `--force` overwrites are left to the plan.
/// `sync`: the command that offers a skipped secret again.
pub(crate) fn report_settings(
    plan: &Plan,
    consent: &Consent,
    dry_run: bool,
    force: bool,
    sync: &str,
) {
    if let Some(s) = &plan.settings {
        let taken = s
            .added
            .iter()
            .chain(&s.updated)
            .map(|k| {
                (
                    k,
                    if consent.agreed.contains(k) {
                        Outcome::Approved
                    } else {
                        Outcome::Synced
                    },
                )
            })
            .chain(s.removed.iter().map(|k| (k, Outcome::Removed)))
            .filter(|(k, _)| !s.forced.contains(k));
        for (key, outcome) in taken {
            if let Some(category) = s.categories.get(key) {
                report_line(key, *category, outcome, dry_run, force);
            }
        }
    }
    report_held(consent, dry_run, force, sync, true);
}

/// Report what became of each shared secret the default sets that a new
/// profile's clone held (`sync`: the command that offers a skipped one).
pub(crate) fn report_secrets(consent: &Consent, sync: &str) {
    report_held(consent, false, false, sync, false);
}

/// One line of [`report_settings`]: `KEY: CATEGORY · from the default · OUTCOME`.
fn report_line(label: &str, category: Category, outcome: Outcome, dry_run: bool, force: bool) {
    ui::info(&format!(
        "{label}: {} · from the default · {}",
        category.as_str(),
        outcome.describe(dry_run, force)
    ));
}

/// The held secrets' lines of [`report_settings`], and how to be offered
/// a skipped one again. `taken_reported`: one of `settings.json` agreed to
/// is reported with the keys taken.
fn report_held(consent: &Consent, dry_run: bool, force: bool, sync: &str, taken_reported: bool) {
    for (h, outcome) in &consent.held {
        if taken_reported && *outcome == Outcome::Approved && h.file == copy::SETTINGS_FILE {
            continue;
        }
        report_line(&h.label(), Category::SharedSecret, *outcome, dry_run, force);
    }
    if consent
        .held
        .iter()
        .any(|(_, o)| matches!(o, Outcome::Skipped(_)))
    {
        ui::hint(&format!(
            "a shared secret new or changed in the default reaches a profile only once you agree: run `{sync}` at a terminal to be offered it (--yes never agrees)"
        ));
    }
}

/// Remember in the sync record at `dir` that the user declined `key` as
/// `mark` (a value's hash, or [`settings_sync::declined_removal`]). Read
/// afresh, so an earlier decline in this run is kept.
fn save_declined(dir: &Path, key: &str, mark: String) -> Result<()> {
    if let Some(mut m) = manifest::load(dir).manifest().cloned() {
        m.declined_secrets.insert(key.to_string(), mark);
        m.save(dir)?;
    }
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
/// The profile's linked items (see `mcc link`) are left out. Shared
/// secrets the default sets are held ([`Secrets::Held`]).
pub fn plan_for(
    paths: &Paths,
    cfg: &Config,
    dir: &Path,
    spec: &Spec,
    manifest: Option<&Manifest>,
    prune: bool,
    force: bool,
) -> Result<Plan> {
    plan_for_with(
        paths,
        cfg,
        dir,
        spec,
        &Base {
            manifest,
            force,
            secrets: Secrets::Held,
        },
        prune,
    )
}

/// [`plan_for`], against `base`.
fn plan_for_with(
    paths: &Paths,
    cfg: &Config,
    dir: &Path,
    spec: &Spec,
    base: &Base,
    prune: bool,
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
        base,
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
    approved: &BTreeSet<String>,
    ask: &mut dyn Ask,
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
    let (plan, consent) = plan_asking(
        alias,
        approved,
        previous.as_ref(),
        args.dry_run,
        ask,
        &mut |approved| {
            let base = Base {
                manifest: previous.as_ref(),
                force: args.force,
                secrets: Secrets::HeldExcept(approved),
            };
            plan_for_with(paths, cfg, &dir, spec, &base, args.prune)
        },
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
    report_settings(
        &plan,
        &consent,
        args.dry_run,
        args.force,
        &format!("mcc sync {alias}"),
    );
    for rel in &plan.unreadable {
        ui::warn(&format!(
            "{} is not a JSON object (or not valid JSON), so it is not synced to \"{alias}\": the keys tied to one account in it can't be told apart; fix it, then run `mcc sync {alias}` again",
            paths.display(&paths.default_dir.join(rel))
        ));
    }
    if status.conflicts > 0 {
        ui::hint(&format!(
            "conflicts changed in the default and in the profile; see them with `mcc diff {alias}`, or overwrite them with `mcc sync {alias} --force` (backed up first)"
        ));
    }
    if previous.is_none() {
        warn_replaced(paths, alias, &dir, &plan, args.dry_run);
    }
    let mut next = plan.manifest(previous.as_ref());
    next.declined_secrets.extend(consent.declined.clone());

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
/// written. A shared secret new or changed in the default is held, left
/// for `mcc sync` to ask about; a default settings file that isn't a JSON
/// object is left out.
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
        if !s.held.is_empty() {
            parts.push(format!(
                "{} held until you agree ({})",
                plural(s.held.len(), "shared secret", "shared secrets"),
                s.held.join(", ")
            ));
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

    /// [`Ask`] with scripted answers, recording every prompt; asking past
    /// the script fails the test.
    struct Scripted {
        answers: Vec<bool>,
        prompts: Vec<String>,
    }

    impl Scripted {
        fn new(answers: &[bool]) -> Self {
            Self {
                answers: answers.to_vec(),
                prompts: Vec::new(),
            }
        }
    }

    impl Ask for Scripted {
        fn interactive(&self) -> bool {
            true
        }

        fn confirm(&mut self, prompt: &str) -> Result<bool> {
            self.prompts.push(prompt.to_string());
            assert!(!self.answers.is_empty(), "asked once too often: {prompt}");
            Ok(self.answers.remove(0))
        }
    }

    const PROFILES: [&str; 3] = ["work", "lab", "csdaisy"];

    /// A default with settings, and `work`, `lab` and `csdaisy` synced
    /// from it once.
    fn synced() -> (tempfile::TempDir, Paths, Config) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        std::fs::create_dir_all(&paths.default_dir).unwrap();
        write(&paths.default_dir, serde_json::json!({"model": "opus"}));
        let mut cfg = Config::default();
        for a in PROFILES {
            let dir = paths.profile_dir(a);
            std::fs::create_dir_all(&dir).unwrap();
            let p = toml::from_str(&format!(
                "dir = {:?}\nmode = \"clone\"\ncreated_at = \"2026-01-01T00:00:00Z\"",
                dir.display().to_string()
            ))
            .unwrap();
            cfg.profiles.insert(a.into(), p);
        }
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        (d, paths, cfg)
    }

    fn sync(paths: &Paths, cfg: &mut Config, ask: &mut dyn Ask) {
        sync_forced(paths, cfg, ask, false);
    }

    fn sync_forced(paths: &Paths, cfg: &mut Config, ask: &mut dyn Ask, force: bool) {
        let args = SyncArgs {
            alias: None,
            all: true,
            dry_run: false,
            prune: false,
            force,
            only: Some(vec!["settings".into()]),
            yes: false,
        };
        run_with(paths, cfg, &args, ask).unwrap();
    }

    fn write(dir: &Path, v: serde_json::Value) {
        std::fs::write(dir.join(copy::SETTINGS_FILE), v.to_string()).unwrap();
    }

    fn settings(dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join(copy::SETTINGS_FILE)).unwrap()).unwrap()
    }

    fn record(dir: &Path) -> Manifest {
        manifest::load(dir).manifest().cloned().unwrap()
    }

    /// The upgrade case of #129: records from an older mcc (the whole
    /// file's hash only), a key set in csdaisy, and the default changed
    /// since. Which side the key is new on can't be told, so it is asked.
    fn upgraded_with_codemode(value: &str) -> (tempfile::TempDir, Paths, Config) {
        let (d, paths, cfg) = synced();
        legacy_records(&paths);
        let cs = paths.profile_dir("csdaisy");
        let mut s = settings(&cs);
        s["env"] = serde_json::json!({"CODE_MODE": value});
        write(&cs, s);
        let mut s = settings(&paths.default_dir);
        s["effortLevel"] = "high".into();
        write(&paths.default_dir, s);
        (d, paths, cfg)
    }

    /// Every profile's record as an older mcc kept it: the whole
    /// settings.json's hash only.
    fn legacy_records(paths: &Paths) {
        for a in PROFILES {
            let dir = paths.profile_dir(a);
            let mut m = record(&dir);
            m.items.remove(manifest::SETTINGS_ITEM);
            m.settings.clear();
            m.save(&dir).unwrap();
        }
    }

    /// What `mcc list` would say about `alias`'s settings.
    fn status(paths: &Paths, cfg: &Config, alias: &str) -> Status {
        let dir = paths.profile_dir(alias);
        let spec = copy::spec(Some(&["settings".to_string()]), &cfg.sync).unwrap();
        plan_for(paths, cfg, &dir, &spec, Some(&record(&dir)), false, false)
            .unwrap()
            .status()
    }

    /// #129 QA: two profiles on records from an older mcc, each with a
    /// change of its own, the default unchanged. Taking the first change
    /// into the default must not make the second profile's reading flip
    /// (the guess compares with the default): both changes reach every
    /// profile, with no conflict, and the next sync is quiet.
    #[test]
    fn changes_in_several_upgraded_profiles_all_reach_every_profile() {
        let (_d, paths, mut cfg) = synced();
        legacy_records(&paths);
        let edit = |a: &str, k: &str, v: &str| {
            let dir = paths.profile_dir(a);
            let mut s = settings(&dir);
            s[k] = v.into();
            write(&dir, s);
        };
        edit("work", "model", "sonnet");
        edit("lab", "effortLevel", "high");
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        for dir in PROFILES
            .iter()
            .map(|a| paths.profile_dir(a))
            .chain([paths.default_dir.clone()])
        {
            let s = settings(&dir);
            assert_eq!(s["model"], "sonnet", "{}: {s}", dir.display());
            assert_eq!(s["effortLevel"], "high", "{}: {s}", dir.display());
        }
        for a in PROFILES {
            assert_eq!(status(&paths, &cfg, a), Status::default(), "{a}");
        }
        let before = std::fs::read(paths.default_dir.join(copy::SETTINGS_FILE)).unwrap();
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        assert_eq!(
            std::fs::read(paths.default_dir.join(copy::SETTINGS_FILE)).unwrap(),
            before
        );
    }

    /// A default with `effortLevel`, synced to every profile.
    fn synced_with_effort() -> (tempfile::TempDir, Paths, Config) {
        let (d, paths, mut cfg) = synced();
        let mut s = settings(&paths.default_dir);
        s["effortLevel"] = "high".into();
        write(&paths.default_dir, s);
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        (d, paths, cfg)
    }

    /// #129 QA: a key an upgraded profile lacks while the default is
    /// unchanged may have been lost rather than removed: it is never
    /// proposed for removal, on this sync or the next.
    #[test]
    fn a_key_an_upgraded_profile_lacks_is_never_proposed_for_removal() {
        let (_d, paths, mut cfg) = synced_with_effort();
        legacy_records(&paths);
        let cs = paths.profile_dir("csdaisy");
        let mut s = settings(&cs);
        s.as_object_mut().unwrap().remove("effortLevel");
        write(&cs, s);
        for _ in 0..2 {
            // Asking anything fails the test.
            sync(&paths, &mut cfg, &mut Scripted::new(&[]));
            assert_eq!(settings(&paths.default_dir)["effortLevel"], "high");
            assert!(settings(&cs).get("effortLevel").is_none());
            let base = record(&cs).settings["effortLevel"].clone();
            assert!(base.starts_with(settings_sync::LOST_PREFIX), "{base}");
        }
        // A new value in the default reaches it.
        let mut s = settings(&paths.default_dir);
        s["effortLevel"] = "low".into();
        write(&paths.default_dir, s);
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        assert_eq!(settings(&cs)["effortLevel"], "low");
        assert_eq!(
            record(&cs).settings["effortLevel"],
            manifest::hash_json(&serde_json::json!("low"))
        );
    }

    /// #129 QA: a settings.json mcc wrote and the profile deleted stays
    /// deleted (one local edit, never a `{}` whose keys would all read as
    /// removed), so a sync at a terminal asks nothing and the default is
    /// left as it is; `--force` restores it with fresh per-key bases.
    #[test]
    fn a_deleted_settings_file_stays_deleted_until_forced() {
        let (_d, paths, mut cfg) = synced_with_effort();
        let cs = paths.profile_dir("csdaisy");
        let file = cs.join(copy::SETTINGS_FILE);
        std::fs::remove_file(&file).unwrap();
        let recorded = record(&cs);
        let default = paths.default_dir.join(copy::SETTINGS_FILE);
        let before = std::fs::read(&default).unwrap();
        for _ in 0..2 {
            // Asking anything fails the test.
            sync(&paths, &mut cfg, &mut Scripted::new(&[]));
            assert!(!file.exists(), "{:?}", std::fs::read_to_string(&file));
            assert_eq!(std::fs::read(&default).unwrap(), before);
            let st = status(&paths, &cfg, "csdaisy");
            assert_eq!((st.local_edits, st.behind, st.conflicts), (1, 0, 0));
            let m = record(&cs);
            assert_eq!(m.settings, recorded.settings);
            assert_eq!(
                m.files.get("settings.json"),
                recorded.files.get("settings.json")
            );
        }
        sync_forced(&paths, &mut cfg, &mut Scripted::new(&[]), true);
        let s = settings(&cs);
        assert_eq!(s["model"], "opus", "{s}");
        assert_eq!(s["effortLevel"], "high", "{s}");
        assert_eq!(
            record(&cs).settings["effortLevel"],
            manifest::hash_json(&serde_json::json!("high"))
        );
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        assert_eq!(status(&paths, &cfg, "csdaisy"), Status::default());
        assert_eq!(std::fs::read(&default).unwrap(), before);
    }

    /// #129 QA: a settings.json emptied by hand (`{}`, or only keys of its
    /// own account) is never read for removals: a sync at a terminal asks
    /// nothing and the default is left as it is.
    #[test]
    fn an_emptied_settings_file_never_removes_from_the_default() {
        let (_d, paths, mut cfg) = synced_with_effort();
        let cs = paths.profile_dir("csdaisy");
        let default = paths.default_dir.join(copy::SETTINGS_FILE);
        let before = std::fs::read(&default).unwrap();
        for emptied in [
            serde_json::json!({}),
            serde_json::json!({"env": {"ANTHROPIC_BASE_URL": "https://gw.example.invalid"}}),
        ] {
            write(&cs, emptied.clone());
            // Asking anything fails the test.
            sync(&paths, &mut cfg, &mut Scripted::new(&[]));
            assert_eq!(std::fs::read(&default).unwrap(), before, "{emptied}");
            assert_eq!(settings(&cs), emptied);
        }
    }

    /// A removal declined at the prompt is not asked about again while the
    /// default keeps the same value.
    #[test]
    fn a_declined_removal_is_not_asked_again() {
        let (_d, paths, mut cfg) = synced_with_effort();
        let cs = paths.profile_dir("csdaisy");
        let mut s = settings(&cs);
        s.as_object_mut().unwrap().remove("effortLevel");
        write(&cs, s);
        let mut ask = Scripted::new(&[false]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 1);
        assert!(
            ask.prompts[0].starts_with("Remove effortLevel (basic)"),
            "{:?}",
            ask.prompts
        );
        assert_eq!(settings(&paths.default_dir)["effortLevel"], "high");
        assert_eq!(
            record(&cs).declined_secrets["effortLevel"],
            settings_sync::declined_removal(Some(&serde_json::json!("high")))
        );
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        assert_eq!(settings(&paths.default_dir)["effortLevel"], "high");
    }

    #[test]
    fn a_confirmed_proposal_reaches_every_profile_and_is_not_asked_again() {
        let (_d, paths, mut cfg) = upgraded_with_codemode("codemode-on");
        let mut ask = Scripted::new(&[true]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 1, "{:?}", ask.prompts);
        let prompt = &ask.prompts[0];
        assert!(
            prompt.starts_with("Take env.CODE_MODE (basic) from \"csdaisy\" into the default?"),
            "{prompt}"
        );
        assert!(!prompt.contains("codemode-on"), "{prompt}");
        assert_eq!(
            settings(&paths.default_dir)["env"]["CODE_MODE"],
            "codemode-on"
        );
        let real = manifest::hash_json(&serde_json::json!("codemode-on"));
        for a in PROFILES {
            let dir = paths.profile_dir(a);
            assert_eq!(settings(&dir)["env"]["CODE_MODE"], "codemode-on", "{a}");
            assert_eq!(record(&dir).settings["env.CODE_MODE"], real, "{a}");
            let text = std::fs::read_to_string(dir.join(manifest::FILE)).unwrap();
            assert!(!text.contains(settings_sync::PROPOSED_BASE), "{a}: {text}");
        }
        // Quiet from then on: nothing to ask, the default left as it is.
        let before = std::fs::read(paths.default_dir.join(copy::SETTINGS_FILE)).unwrap();
        for _ in 0..2 {
            sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        }
        assert_eq!(
            std::fs::read(paths.default_dir.join(copy::SETTINGS_FILE)).unwrap(),
            before
        );
    }

    #[test]
    fn a_declined_proposal_is_not_asked_again_until_its_value_changes() {
        let (_d, paths, mut cfg) = upgraded_with_codemode("codemode-on");
        // A second key only csdaisy has: both declined in one run are kept.
        let cs = paths.profile_dir("csdaisy");
        let mut s = settings(&cs);
        s["env"]["OTHER_FLAG"] = "1".into();
        write(&cs, s);
        let mut ask = Scripted::new(&[false, false]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 2, "{:?}", ask.prompts);
        assert!(
            settings(&paths.default_dir)["env"]
                .get("CODE_MODE")
                .is_none()
        );
        let m = record(&cs);
        assert_eq!(
            m.declined_secrets["env.OTHER_FLAG"],
            manifest::hash_json(&serde_json::json!("1"))
        );
        assert_eq!(
            m.declined_secrets["env.CODE_MODE"],
            manifest::hash_json(&serde_json::json!("codemode-on"))
        );
        // Kept in csdaisy as a pending proposal, not reached elsewhere.
        assert_eq!(m.settings["env.CODE_MODE"], settings_sync::PROPOSED_BASE);
        assert_eq!(settings(&cs)["env"]["CODE_MODE"], "codemode-on");
        assert!(settings(&paths.profile_dir("work")).get("env").is_none());
        let before = std::fs::read(paths.default_dir.join(copy::SETTINGS_FILE)).unwrap();
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        assert_eq!(
            std::fs::read(paths.default_dir.join(copy::SETTINGS_FILE)).unwrap(),
            before
        );
        // A new value is asked about again.
        let mut s = settings(&cs);
        s["env"]["CODE_MODE"] = "codemode-two".into();
        write(&cs, s);
        let mut ask = Scripted::new(&[false]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 1);
    }

    /// Shared secrets: an approved one reaches the default and every
    /// profile; a declined one is recorded by hash and not asked again.
    #[test]
    fn shared_secrets_are_synced_only_once_approved() {
        let (_d, paths, mut cfg) = synced();
        let cs = paths.profile_dir("csdaisy");
        let mut s = settings(&cs);
        s["env"] =
            serde_json::json!({"TAVILY_API_KEY": "tvly-approved", "OTHER_TOKEN": "tok-declined"});
        write(&cs, s);
        // Keys in order: env.OTHER_TOKEN, then env.TAVILY_API_KEY.
        let mut ask = Scripted::new(&[false, true]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 2, "{:?}", ask.prompts);
        for p in &ask.prompts {
            assert!(p.contains("(shared secret)"), "{p}");
            assert!(
                !p.contains("tvly-approved") && !p.contains("tok-declined"),
                "{p}"
            );
        }
        let d = settings(&paths.default_dir);
        assert_eq!(d["env"]["TAVILY_API_KEY"], "tvly-approved");
        assert!(d["env"].get("OTHER_TOKEN").is_none());
        assert_eq!(
            settings(&paths.profile_dir("work"))["env"]["TAVILY_API_KEY"],
            "tvly-approved"
        );
        assert_eq!(
            record(&cs).declined_secrets["env.OTHER_TOKEN"],
            manifest::hash_json(&serde_json::json!("tok-declined"))
        );
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
    }

    /// AC3 of #129: a shared secret set in the default reaches a profile
    /// only once the user agrees, asked per profile without its value; a
    /// declined one is recorded by hash and not asked again, one already
    /// in sync is never asked about, and a new value is asked about again.
    #[test]
    fn a_shared_secret_from_the_default_is_synced_only_once_approved() {
        let (_d, paths, mut cfg) = synced();
        let mut s = settings(&paths.default_dir);
        s["env"] = serde_json::json!({"TAVILY_API_KEY": "tvly-from-default"});
        write(&paths.default_dir, s);
        // Profiles in order: csdaisy, lab, work.
        let mut ask = Scripted::new(&[true, false, true]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 3, "{:?}", ask.prompts);
        for (p, a) in ask.prompts.iter().zip(["csdaisy", "lab", "work"]) {
            assert_eq!(
                *p,
                format!(
                    "Sync env.TAVILY_API_KEY (shared secret) from the default to \"{a}\"? Its value is not shown."
                )
            );
        }
        for a in ["csdaisy", "work"] {
            let dir = paths.profile_dir(a);
            assert_eq!(
                settings(&dir)["env"]["TAVILY_API_KEY"],
                "tvly-from-default",
                "{a}"
            );
        }
        let lab = paths.profile_dir("lab");
        assert!(settings(&lab).get("env").is_none(), "{}", settings(&lab));
        let value = serde_json::json!("tvly-from-default");
        assert_eq!(
            record(&lab).declined_secrets["env.TAVILY_API_KEY"],
            settings_sync::declined_from_default(Some(&value))
        );
        assert!(!record(&lab).settings.contains_key("env.TAVILY_API_KEY"));
        // Asking anything fails the test: declined in lab, in sync elsewhere.
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        assert!(settings(&lab).get("env").is_none());
        assert_eq!(status(&paths, &cfg, "lab"), Status::default());
        // A new value is asked about again, everywhere; declined, a
        // profile keeps the value it has.
        let mut s = settings(&paths.default_dir);
        s["env"]["TAVILY_API_KEY"] = "tvly-rotated".into();
        write(&paths.default_dir, s);
        let mut ask = Scripted::new(&[false, true, false]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 3, "{:?}", ask.prompts);
        let cs = settings(&paths.profile_dir("csdaisy"));
        assert_eq!(cs["env"]["TAVILY_API_KEY"], "tvly-from-default");
        assert_eq!(settings(&lab)["env"]["TAVILY_API_KEY"], "tvly-rotated");
        // Removing it from the default needs no consent.
        let mut s = settings(&paths.default_dir);
        s.as_object_mut().unwrap().remove("env");
        write(&paths.default_dir, s);
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        assert!(settings(&lab)["env"].get("TAVILY_API_KEY").is_none());
    }

    fn local(dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join(copy::LOCAL_SETTINGS_FILE)).unwrap())
            .unwrap()
    }

    /// AC3 of #129, for settings.local.json (synced as a whole file): a
    /// shared secret the default's sets reaches a profile only once the
    /// user agrees, asked per profile without its value, while the rest of
    /// the file still syncs; a declined one is not asked again, one in sync
    /// is never asked about, and a rotated value is asked about again.
    #[test]
    fn a_shared_secret_in_the_default_local_settings_is_synced_only_once_approved() {
        let (_d, paths, mut cfg) = synced();
        let write_local = |v: serde_json::Value| {
            std::fs::write(
                paths.default_dir.join(copy::LOCAL_SETTINGS_FILE),
                v.to_string(),
            )
            .unwrap()
        };
        write_local(serde_json::json!({"env": {"LOCAL_FLAG": "1"}}));
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        write_local(serde_json::json!({"env": {"LOCAL_FLAG": "2", "SEARCH_TOKEN": "tok-one"}}));
        // Profiles in order: csdaisy, lab, work.
        let mut ask = Scripted::new(&[true, false, true]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 3, "{:?}", ask.prompts);
        for (p, a) in ask.prompts.iter().zip(["csdaisy", "lab", "work"]) {
            assert_eq!(
                *p,
                format!(
                    "Sync env.SEARCH_TOKEN in settings.local.json (shared secret) from the default to \"{a}\"? Its value is not shown."
                )
            );
        }
        for a in ["csdaisy", "work"] {
            let l = local(&paths.profile_dir(a));
            assert_eq!(l["env"]["SEARCH_TOKEN"], "tok-one", "{a}");
            assert_eq!(l["env"]["LOCAL_FLAG"], "2", "{a}");
        }
        let lab = paths.profile_dir("lab");
        assert!(local(&lab)["env"].get("SEARCH_TOKEN").is_none());
        assert_eq!(local(&lab)["env"]["LOCAL_FLAG"], "2");
        let value = serde_json::json!("tok-one");
        assert_eq!(
            record(&lab).declined_secrets["settings.local.json:env.SEARCH_TOKEN"],
            settings_sync::declined_from_default(Some(&value))
        );
        // Asking anything fails the test.
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
        assert!(local(&lab)["env"].get("SEARCH_TOKEN").is_none());
        // Rotated: asked again everywhere; declined, a profile keeps its own.
        write_local(serde_json::json!({"env": {"LOCAL_FLAG": "2", "SEARCH_TOKEN": "tok-two"}}));
        let mut ask = Scripted::new(&[false, true, false]);
        sync(&paths, &mut cfg, &mut ask);
        assert_eq!(ask.prompts.len(), 3, "{:?}", ask.prompts);
        assert_eq!(
            local(&paths.profile_dir("csdaisy"))["env"]["SEARCH_TOKEN"],
            "tok-one"
        );
        assert_eq!(local(&lab)["env"]["SEARCH_TOKEN"], "tok-two");
        sync(&paths, &mut cfg, &mut Scripted::new(&[]));
    }

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
