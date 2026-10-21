//! `mcc link` — opt a profile's read-mostly items in to (or out of) link mode.
//!
//! In link mode a profile's item (`skills`, `agents`, `commands`,
//! `output-styles` or `CLAUDE.md`) is a symlink to the default profile's
//! copy instead of a copy of its own: nothing is duplicated and nothing
//! drifts, and `mcc sync` leaves it alone. The cost is write-through: an
//! edit made through the profile changes the default's real files (and so
//! every profile linking them). Off for every item by default.
//!
//! Turning it on backs up the profile's own copy (it may hold local edits)
//! to `.mcc-backup/` first. Turning it off removes only the symlink, never
//! what it points at, and copies the default's item in its place. Both are
//! undone as a whole when a step fails.
//!
//! `plugins` is linked differently: Claude Code keeps every plugin file under
//! one plugins root, which `CLAUDE_CODE_PLUGIN_CACHE_DIR` can point anywhere
//! (see `crate::plugins`). Linking it moves the profile's own `plugins/` into
//! `.mcc-backup/` (a rename: nothing is deleted, and removing that backup is
//! what frees the space) and makes the profile's launcher export the
//! variable, set to the default's `~/.claude/plugins`. No symlink is made.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::cli::LinkArgs;
use crate::config::Config;
use crate::copy::{self, Rewriter, Spec};
use crate::error::CliError;
use crate::manifest::{self, Manifest};
use crate::paths::Paths;
use crate::txn::{self, Transaction};
use crate::{fsutil, plugins, shell, ui};

/// Printed on stderr whenever link mode is turned on (by `mcc link` or
/// `mcc add --clone --link`).
pub fn write_through_note(alias: &str, items: &[String]) -> String {
    format!(
        "write-through: \"{alias}\" now uses the default profile's own {list}. Editing {them} in \"{alias}\" (by hand, or by Claude Code, e.g. installing a skill) changes the default's real files, and every other profile that links {them}. `mcc sync` leaves linked items alone; `mcc link {alias} {first} --off` gives \"{alias}\" its own copy again.",
        list = items.join(", "),
        them = if items.len() == 1 { "it" } else { "them" },
        first = items[0],
    )
}

/// The items linked in the profile whose dir is `dir` (none for a dir no
/// profile has, such as the default's). An entry that can't be linked (a
/// hand edit, e.g. `settings`) is left out, with a warning: sync, diff and
/// list treat that item as a copied one.
pub fn linked(cfg: &Config, dir: &Path) -> Vec<String> {
    cfg.profiles
        .iter()
        .find(|(_, p)| p.dir == dir)
        .map_or_else(Vec::new, |(alias, p)| usable(alias, &p.link))
}

/// Whether `item` can be in a profile's `link`: a [`copy::LINKABLE`] item
/// or the plugins.
pub fn can_link(item: &str) -> bool {
    copy::LINKABLE.contains(&item) || item == plugins::ITEM
}

/// A profile's `link` split into the entries that can be linked and those
/// that can't, each in order. Entries are taken as written (untrimmed), as
/// sync matches them.
fn split_link(link: &[String]) -> (Vec<String>, Vec<String>) {
    link.iter().cloned().partition(|l| can_link(l))
}

/// The (profile, entry) pairs [`usable`] has warned about, so a command
/// that reads a profile's links more than once warns once.
static WARNED: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// The entries of `alias`'s `link` that can be linked. Each other one is
/// ignored, with a warning naming it.
fn usable(alias: &str, link: &[String]) -> Vec<String> {
    let (keep, ignored) = split_link(link);
    for item in ignored {
        let first = WARNED
            .lock()
            .map_or(true, |mut w| w.insert((alias.to_string(), item.clone())));
        if first {
            ui::warn(&format!(
                "\"{alias}\": link entry \"{item}\" can't be linked, so it is ignored (sync treats the item as not linked); remove it with `mcc config edit` (items that can be linked: {}, {})",
                copy::LINKABLE.join(", "),
                plugins::ITEM
            ));
        }
    }
    keep
}

/// The sync items named in `items`, each once, or a usage error naming the
/// items that can be linked.
pub fn check_items(items: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item == "settings" {
            return Err(CliError::usage(
                "settings can't be linked: each profile's settings.json is the default's merged with its own overlay and rewritten paths",
            )
            .hint(format!(
                "link one of {}; a profile's own settings go in its overlay ([profiles.ALIAS.settings])",
                copy::LINKABLE.join(", ")
            ))
            .into());
        }
        if !can_link(item) {
            let why = if copy::group_names().contains(&item) {
                format!("\"{item}\" can't be linked")
            } else {
                format!("unknown item \"{item}\"")
            };
            return Err(CliError::usage(why)
                .hint(format!(
                    "items that can be linked: {}, {}",
                    copy::LINKABLE.join(", "),
                    plugins::ITEM
                ))
                .into());
        }
        if !out.iter().any(|o| o == item) {
            out.push(item.to_string());
        }
    }
    Ok(out)
}

/// The profile path of a linkable item.
fn rel(item: &str) -> PathBuf {
    copy::group_path(item).expect("every linkable item is one path")
}

/// What a profile's item currently is.
#[derive(Debug, PartialEq)]
enum State {
    /// A symlink to the default's item: linked.
    Linked,
    /// A symlink somewhere else.
    OtherLink,
    /// The profile's own copy (a directory or a file).
    Copy,
    Missing,
}

fn state(dst: &Path, target: &Path) -> Result<State> {
    match std::fs::symlink_metadata(dst) {
        Ok(m) if m.file_type().is_symlink() => {
            let to =
                std::fs::read_link(dst).with_context(|| format!("reading {}", dst.display()))?;
            Ok(if to == target {
                State::Linked
            } else {
                State::OtherLink
            })
        }
        Ok(_) => Ok(State::Copy),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::Missing),
        Err(e) => Err(e).with_context(|| format!("reading {}", dst.display())),
    }
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// A change made while turning link mode on or off, undone in reverse order
/// when a later step fails.
enum Undo {
    /// The profile's `from` was renamed to `to`; renamed back.
    Moved { from: PathBuf, to: PathBuf },
    /// A symlink or a copy mcc created where nothing was (the old item was
    /// moved aside first); removed. A symlink is removed, never followed.
    Created(PathBuf),
    /// The profile's sync manifest before the change (`None`: there was none).
    Manifest(PathBuf, Option<Manifest>),
    /// The launcher before mcc rewrote it; written back.
    Launcher(PathBuf, Vec<u8>),
    /// A backup folder mcc created; removed only while empty, so a backup
    /// whose contents could not be moved back is kept.
    BackupDir(PathBuf),
}

/// Rollback: quiet unless a step fails.
const MESSAGES: txn::Messages = txn::Messages {
    header: None,
    done: None,
    failed: "rollback: could not",
};

fn rollback(paths: &Paths, undo: &mut Transaction<Undo>) {
    undo.rollback(&MESSAGES, |u| match u {
        Undo::Moved { from, to } => (
            format!("move {} back", paths.display(&to)),
            std::fs::rename(&to, &from).map_err(anyhow::Error::from),
        ),
        Undo::Created(p) => (
            format!("remove {}", paths.display(&p)),
            fsutil::remove_any(&p),
        ),
        Undo::Manifest(dir, previous) => (
            format!("restore {}", paths.display(&dir.join(manifest::FILE))),
            match previous {
                Some(m) => m.save(&dir),
                None => fsutil::remove_any(&dir.join(manifest::FILE)),
            },
        ),
        Undo::BackupDir(dir) => (
            format!("remove the empty {}", paths.display(&dir)),
            std::fs::remove_dir(&dir)
                .map_err(|e| anyhow::Error::from(e).context(format!("{} is kept", dir.display()))),
        ),
        Undo::Launcher(path, bytes) => (
            format!("restore {}", paths.display(&path)),
            fsutil::write_atomic(&path, &bytes).and_then(|()| fsutil::set_executable(&path)),
        ),
    });
}

/// Where the profile's `dst` waits while the change is made: next to it,
/// with a name sync and the metrics skip (see `copy::is_noise`).
fn aside(dst: &Path) -> PathBuf {
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    dst.with_file_name(format!(".{name}.mcc-tmp-link-{}", std::process::id()))
}

/// Move `dst` aside, recording how to move it back.
fn move_aside(dst: &Path, undo: &mut Transaction<Undo>) -> Result<PathBuf> {
    let to = aside(dst);
    if exists(&to) {
        return Err(CliError::runtime(format!(
            "{} is in the way (left by an interrupted `mcc link`?)",
            to.display()
        ))
        .hint("move or remove it, then retry")
        .into());
    }
    std::fs::rename(dst, &to)
        .with_context(|| format!("moving {} to {}", dst.display(), to.display()))?;
    undo.record(Undo::Moved {
        from: dst.to_path_buf(),
        to: to.clone(),
    });
    Ok(to)
}

/// Remove what was moved aside, once everything else succeeded. Only a
/// symlink is removed when `links_only` (turning link mode off): never what
/// it points at, and never a real directory.
fn remove_asides(paths: &Paths, asides: &[PathBuf], links_only: bool) {
    for a in asides {
        let is_link = std::fs::symlink_metadata(a).is_ok_and(|m| m.file_type().is_symlink());
        if links_only && !is_link {
            ui::warn(&format!("left {} in place", paths.display(a)));
            continue;
        }
        if let Err(e) = fsutil::remove_any(a) {
            ui::warn(&format!(
                "could not remove {}: {e:#}; remove it by hand",
                paths.display(a)
            ));
        }
    }
}

/// The profile's sync manifest as `(found, previous)`; `found` is false when
/// it can't be read (it is then left as it is).
fn load_manifest(paths: &Paths, dir: &Path) -> (bool, Option<Manifest>) {
    match manifest::load(dir) {
        manifest::Loaded::Found(m) => (true, Some(m)),
        manifest::Loaded::Missing => (true, None),
        manifest::Loaded::Invalid(why) => {
            ui::warn(&format!(
                "leaving {} as it is ({why})",
                paths.display(&dir.join(manifest::FILE))
            ));
            (false, None)
        }
    }
}

/// `m` without what it recorded under the item paths `rels`.
fn forget(m: &mut Manifest, rels: &[PathBuf]) {
    m.files
        .retain(|k, _| !rels.iter().any(|r| Path::new(k).starts_with(r)));
}

/// Whether `dir` is the default profile's directory, or one of the two lies
/// inside the other (after resolving symlinks), so linking would touch the
/// default's real files.
fn overlaps_default(paths: &Paths, dir: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (dir, default) = (canon(dir), canon(&paths.default_dir));
    dir.starts_with(&default) || default.starts_with(&dir)
}

pub fn run(paths: &Paths, cfg: &mut Config, args: &LinkArgs) -> Result<()> {
    let alias = args.alias.as_str();
    if matches!(alias, "default" | "claude") {
        return Err(CliError::usage(
            "the default profile is what other profiles link to; pick another profile",
        )
        .hint("example: mcc link work skills")
        .into());
    }
    let Some(profile) = cfg.profiles.get(alias) else {
        return Err(super::unknown_profile(alias));
    };
    let dir = profile.dir.clone();
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
    if overlaps_default(paths, &dir) {
        return Err(CliError::usage(format!(
            "profile \"{alias}\" uses {}, which overlaps the default profile's {}; link mode would change the default's real files",
            paths.display(&dir),
            paths.display(&paths.default_dir)
        ))
        .hint(format!(
            "point \"{alias}\" at its own directory in `mcc config edit`, or pick another profile"
        ))
        .into());
    }
    let items = check_items(&args.items)?;
    if items.is_empty() {
        if args.off {
            return Err(CliError::usage("name the items to stop linking")
                .hint(format!("example: mcc link {alias} skills --off"))
                .into());
        }
        return show(paths, cfg, alias, &dir);
    }
    let (with_plugins, items): (Vec<String>, Vec<String>) =
        items.into_iter().partition(|i| i == plugins::ITEM);
    if !items.is_empty() {
        if args.off {
            disable(paths, cfg, alias, &dir, &items)?;
        } else {
            enable(paths, cfg, alias, &dir, &items)?;
        }
    }
    if with_plugins.is_empty() {
        Ok(())
    } else if args.off {
        disable_plugins(paths, cfg, alias, &dir)
    } else {
        enable_plugins(paths, cfg, alias, &dir)
    }
}

/// `mcc link <alias>`: each linkable item and whether it is linked.
fn show(paths: &Paths, cfg: &Config, alias: &str, dir: &Path) -> Result<()> {
    ui::header(&format!("Link mode for \"{alias}\""));
    let linked = &cfg.profiles[alias].link;
    for item in copy::LINKABLE {
        let rel = rel(item);
        let target = paths.default_dir.join(&rel);
        let on = linked.iter().any(|l| l == item);
        let text = match (on, state(&dir.join(&rel), &target)?) {
            (true, State::Linked) => format!("linked → {}", paths.display(&target)),
            (true, _) => format!(
                "link mode, but not a link to {} (run `mcc link {alias} {item}` to link it again)",
                paths.display(&target)
            ),
            (false, State::Linked) => format!(
                "a link to the default's, but not in link mode (sync replaces it with a copy; `mcc link {alias} {item}` keeps it)"
            ),
            (false, _) => "copied (synced by `mcc sync`)".into(),
        };
        ui::kv(item, &text);
    }
    let root = plugins::shared_root(paths);
    let text = if plugins::in_link(linked) {
        format!(
            "shared → {} ({} in the launcher, `mcc run`, `mcc exec`)",
            paths.display(&root),
            plugins::ENV
        )
    } else {
        "copied (synced by `mcc sync`)".into()
    };
    ui::kv(plugins::ITEM, &text);
    ui::hint(&format!(
        "link an item with `mcc link {alias} ITEM` (off by default); see `mcc help link`"
    ));
    Ok(())
}

/// The default's items to link to, or an error naming the one it lacks.
fn targets(paths: &Paths, items: &[String]) -> Result<Vec<(String, PathBuf, PathBuf)>> {
    let mut out = Vec::new();
    for item in items {
        let rel = rel(item);
        let target = paths.default_dir.join(&rel);
        if !exists(&target) {
            return Err(CliError::runtime(format!(
                "the default profile has no {} to link to",
                paths.display(&target)
            ))
            .hint(format!("create it first, or leave \"{item}\" copied"))
            .into());
        }
        out.push((item.clone(), rel, target));
    }
    Ok(out)
}

/// Link `items` in the brand-new profile at `dir` (`mcc add --clone
/// --link`), where clone copied everything else. The links are inside
/// `dir`, so removing `dir` undoes them; nothing outside it is touched.
pub fn link_new_profile(paths: &Paths, dir: &Path, items: &[String]) -> Result<()> {
    for (_, rel, target) in targets(paths, &symlinked(items))? {
        let dst = dir.join(&rel);
        std::os::unix::fs::symlink(&target, &dst)
            .with_context(|| format!("linking {} → {}", dst.display(), target.display()))?;
    }
    Ok(())
}

/// Check, before `mcc add --clone --link` creates anything, that the items
/// can be linked and the default has each of them.
pub fn check_new_links(paths: &Paths, items: &[String]) -> Result<Vec<String>> {
    let items = check_items(items)?;
    targets(paths, &symlinked(&items))?;
    Ok(items)
}

/// The items linked with a symlink: all but `plugins`, which is shared
/// through the launcher's environment instead.
fn symlinked(items: &[String]) -> Vec<String> {
    items
        .iter()
        .filter(|i| *i != plugins::ITEM)
        .cloned()
        .collect()
}

fn enable(
    paths: &Paths,
    cfg: &mut Config,
    alias: &str,
    dir: &Path,
    items: &[String],
) -> Result<()> {
    if !paths.default_dir.is_dir() {
        return Err(CliError::runtime(format!(
            "the default profile {} does not exist, so there is nothing to link to",
            paths.display(&paths.default_dir)
        ))
        .hint("run `mcc add` to create it first")
        .into());
    }
    let targets = targets(paths, items)?;
    ui::header(&format!("Link {} in \"{alias}\"", items.join(", ")));
    let before = cfg.profiles[alias].link.clone();
    let mut todo = Vec::new();
    for (item, rel, target) in targets {
        let st = state(&dir.join(&rel), &target)?;
        if st == State::Linked && before.contains(&item) {
            ui::kv(&item, "already linked");
        } else {
            todo.push((item, rel, target, st));
        }
    }
    if todo.is_empty() {
        ui::result(
            true,
            &format!("\"{alias}\" already links {}", items.join(", ")),
        );
        return Ok(());
    }
    let mut steps = ui::Steps::new(2);

    // The profile's own copies may hold local edits.
    let step = steps.start("Back up the profile's copies");
    let own: Vec<PathBuf> = todo
        .iter()
        .filter(|(.., st)| !matches!(st, State::Missing | State::Linked))
        .map(|(_, rel, ..)| rel.clone())
        .collect();
    match super::sync::backup_rels(dir, own)? {
        Some(b) => step.ok(&paths.display(&b)),
        None => step.skip("nothing to back up"),
    }

    let step = steps.start("Link to the default profile");
    let mut undo: Transaction<Undo> = Transaction::new();
    let mut asides = Vec::new();
    let result = (|| -> Result<()> {
        for (_, rel, target, st) in &todo {
            let dst = dir.join(rel);
            if *st == State::Linked {
                continue;
            }
            if *st != State::Missing {
                asides.push(move_aside(&dst, &mut undo)?);
            }
            std::os::unix::fs::symlink(target, &dst)
                .with_context(|| format!("linking {} → {}", dst.display(), target.display()))?;
            undo.record(Undo::Created(dst));
        }
        // What sync wrote there is no longer the profile's: forget it, so a
        // later `--off` starts from a fresh copy.
        let rels: Vec<PathBuf> = todo.iter().map(|(_, rel, ..)| rel.clone()).collect();
        if let (true, Some(previous)) = load_manifest(paths, dir) {
            let mut next = previous.clone();
            forget(&mut next, &rels);
            if next != previous {
                next.save(dir)?;
                undo.record(Undo::Manifest(dir.to_path_buf(), Some(previous)));
            }
        }
        let p = cfg.profiles.get_mut(alias).expect("checked above");
        for (item, ..) in &todo {
            if !p.link.contains(item) {
                p.link.push(item.clone());
            }
        }
        cfg.save(&paths.config_file)
    })();
    if let Err(e) = result {
        step.fail("rolled back");
        rollback(paths, &mut undo);
        if let Some(p) = cfg.profiles.get_mut(alias) {
            p.link = before;
        }
        return Err(e);
    }
    undo.commit();
    step.ok(&format!("{} linked", todo.len()));
    remove_asides(paths, &asides, false);
    let linked: Vec<String> = todo.iter().map(|(item, ..)| item.clone()).collect();
    ui::warn(&write_through_note(alias, &linked));
    ui::result(
        true,
        &format!(
            "\"{alias}\" links {} to the default profile",
            linked.join(", ")
        ),
    );
    Ok(())
}

fn disable(
    paths: &Paths,
    cfg: &mut Config,
    alias: &str,
    dir: &Path,
    items: &[String],
) -> Result<()> {
    ui::header(&format!("Stop linking {} in \"{alias}\"", items.join(", ")));
    let before = cfg.profiles[alias].link.clone();
    // Items to give a copy: those whose profile path is a symlink (only the
    // link is removed) or missing. A real copy is already the profile's own.
    let mut copy_in: Vec<(String, PathBuf, State)> = Vec::new();
    let mut changed = false;
    for item in items {
        let rel = rel(item);
        let target = paths.default_dir.join(&rel);
        let st = state(&dir.join(&rel), &target)?;
        let on = before.contains(item);
        match st {
            State::Copy => {
                ui::kv(item, "already a copy of its own");
                changed |= on;
            }
            State::Missing | State::OtherLink if !on => {
                ui::kv(item, "not linked");
            }
            _ => {
                copy_in.push((item.clone(), rel, st));
                changed = true;
            }
        }
    }
    if !changed {
        ui::result(
            true,
            &format!("\"{alias}\" links none of {}", items.join(", ")),
        );
        return Ok(());
    }
    let step = ui::Steps::new(1).start("Copy from the default profile");
    let mut undo: Transaction<Undo> = Transaction::new();
    let mut asides = Vec::new();
    let mut copied = 0;
    let result = (|| -> Result<()> {
        if !copy_in.is_empty() {
            for (_, rel, st) in &copy_in {
                if *st != State::Missing {
                    asides.push(move_aside(&dir.join(rel), &mut undo)?);
                }
            }
            // The item paths are all missing now: everything is new, and
            // whatever the copy creates is removed by a rollback.
            for (_, rel, _) in &copy_in {
                undo.record(Undo::Created(dir.join(rel)));
            }
            let spec = Spec {
                items: copy_in
                    .iter()
                    .map(|(item, rel, _)| copy::Item {
                        group: item.clone(),
                        rel: rel.clone(),
                        rewrite: false,
                    })
                    .collect(),
                mcp: false,
            };
            let rw = Rewriter::new(paths, dir, &cfg.sync);
            let plan = copy::plan(
                &paths.default_dir,
                &paths.default_json,
                dir,
                &spec,
                &rw,
                false,
            )?;
            copy::apply(&plan, &paths.default_dir, dir, &rw, false)?;
            copied = plan.count(copy::Action::Add);
            // Record what was copied, so the next sync tells the default's
            // changes from edits made in the profile.
            if let (true, previous) = load_manifest(paths, dir) {
                let rels: Vec<PathBuf> = copy_in.iter().map(|(_, rel, _)| rel.clone()).collect();
                let mut base = previous.clone().unwrap_or_default();
                forget(&mut base, &rels);
                plan.manifest(Some(&base)).save(dir)?;
                undo.record(Undo::Manifest(dir.to_path_buf(), previous));
            }
        }
        let p = cfg.profiles.get_mut(alias).expect("checked above");
        p.link.retain(|l| !items.contains(l));
        cfg.save(&paths.config_file)
    })();
    if let Err(e) = result {
        step.fail("rolled back");
        rollback(paths, &mut undo);
        if let Some(p) = cfg.profiles.get_mut(alias) {
            p.link = before;
        }
        return Err(e);
    }
    undo.commit();
    step.ok(&format!("{copied} item(s) copied"));
    // Only the old symlinks: the default's files they point at stay as they are.
    remove_asides(paths, &asides, true);
    ui::result(
        true,
        &format!(
            "\"{alias}\" has its own copy of {}; `mcc sync` keeps it up to date",
            items.join(", ")
        ),
    );
    Ok(())
}

/// Rewrite the profile's launcher for `plugin_root`, recording how to put
/// the old one back. Returns a warning when there is no launcher of mcc's to
/// rewrite (the profile then has to get the variable some other way).
fn rewrite_launcher(
    paths: &Paths,
    cfg: &Config,
    alias: &str,
    plugin_root: Option<&Path>,
    undo: &mut Transaction<Undo>,
) -> Result<Option<String>> {
    let p = &cfg.profiles[alias];
    let Some(wrapper) = &p.wrapper else {
        return Ok(Some(format!(
            "\"{alias}\" has no launcher; start it with `mcc run --profile {alias}` or `mcc exec {alias} -- claude`"
        )));
    };
    if !wrapper.exists() || !shell::is_mcc_wrapper(wrapper) {
        return Ok(Some(format!(
            "{} is missing or was not written by mcc, so it was left alone; run `mcc refresh-launchers {alias}`",
            paths.display(wrapper)
        )));
    }
    let old = std::fs::read(wrapper).with_context(|| format!("reading {}", wrapper.display()))?;
    let env = cfg.env.as_ref();
    let claude_bin = crate::claude::find_binary(env.and_then(|e| e.claude_bin.as_deref()));
    let script = shell::launcher_script(
        paths,
        alias,
        &p.dir,
        claude_bin.as_deref(),
        plugin_root,
        cfg.launch.banner,
    );
    if old != script.as_bytes() {
        undo.record(Undo::Launcher(wrapper.clone(), old));
        shell::install_launcher(wrapper, &script)?;
    }
    Ok(None)
}

/// Move the profile's own `plugins/` (if any) into a new backup folder,
/// recording how to move it back. Returns the backup folder.
fn move_plugins_to_backup(dir: &Path, undo: &mut Transaction<Undo>) -> Result<Option<PathBuf>> {
    let own = dir.join(plugins::DIR);
    if !exists(&own) {
        return Ok(None);
    }
    let dest = super::sync::new_backup_dir(dir)?;
    // Recorded first, so a rollback moves plugins/ back before removing the
    // (then empty) folder; if the move back fails, the folder is kept.
    undo.record(Undo::BackupDir(dest.clone()));
    let to = dest.join(plugins::DIR);
    std::fs::rename(&own, &to)
        .with_context(|| format!("moving {} to {}", own.display(), to.display()))?;
    undo.record(Undo::Moved { from: own, to });
    Ok(Some(dest))
}

/// `mcc link <alias> plugins`: use the default's plugins root instead of a
/// copy (see `crate::plugins`).
fn enable_plugins(paths: &Paths, cfg: &mut Config, alias: &str, dir: &Path) -> Result<()> {
    if !paths.default_dir.is_dir() {
        return Err(CliError::runtime(format!(
            "the default profile {} does not exist, so there are no plugins to share",
            paths.display(&paths.default_dir)
        ))
        .hint("run `mcc add` to create it first")
        .into());
    }
    let root = plugins::shared_root(paths);
    ui::header(&format!("Share the default's plugins with \"{alias}\""));
    let before = cfg.profiles[alias].link.clone();
    let was_current = cfg.profiles[alias]
        .wrapper
        .as_deref()
        .is_some_and(shell::launcher_is_current);
    let mut steps = ui::Steps::new(3);
    let mut undo: Transaction<Undo> = Transaction::new();
    let mut backup = None;
    let mut size = 0;
    let mut launcher_warning = None;
    let result = (|| -> Result<()> {
        // The profile's own plugin files, which may hold plugins or plugin
        // data the default lacks: moved aside, never deleted.
        let step = steps.start("Move the profile's plugins to a backup");
        size = fsutil::dir_size(&dir.join(plugins::DIR));
        backup = move_plugins_to_backup(dir, &mut undo)?;
        match &backup {
            Some(b) => step.ok(&format!("{} ({})", paths.display(b), ui::human_size(size))),
            None => step.skip("the profile has no plugins/"),
        }

        let step = steps.start("Point the launcher at the default's plugins");
        launcher_warning = rewrite_launcher(paths, cfg, alias, Some(&root), &mut undo)?;
        match &launcher_warning {
            Some(_) => step.skip("no launcher of mcc's"),
            None => step.ok(&format!("{}={}", plugins::ENV, paths.display(&root))),
        }

        let step = steps.start("Register");
        // What sync wrote under plugins/ is gone with it: forget it, so a
        // later `--off` starts from a fresh copy.
        if let (true, Some(previous)) = load_manifest(paths, dir) {
            let mut next = previous.clone();
            forget(&mut next, &[PathBuf::from(plugins::DIR)]);
            if next != previous {
                next.save(dir)?;
                undo.record(Undo::Manifest(dir.to_path_buf(), Some(previous)));
            }
        }
        let p = cfg.profiles.get_mut(alias).expect("checked above");
        if !plugins::in_link(&p.link) {
            p.link.push(plugins::ITEM.into());
        }
        cfg.save(&paths.config_file)?;
        step.ok(&paths.display(&paths.config_file));
        Ok(())
    })();
    if let Err(e) = result {
        rollback(paths, &mut undo);
        if let Some(p) = cfg.profiles.get_mut(alias) {
            p.link = before;
        }
        return Err(e);
    }
    undo.commit();
    ui::warn(&plugins::shared_note(alias));
    if let Some(w) = launcher_warning {
        ui::warn(&w);
    } else if !was_current {
        ui::hint(&format!(
            "run `mcc refresh-launchers {alias}` so the shell alias `{alias}` runs the launcher"
        ));
    }
    if let Some(b) = &backup {
        ui::hint(&format!(
            "the profile's old plugins ({}) are in {}; remove that folder to free the space",
            ui::human_size(size),
            paths.display(b)
        ));
    }
    ui::result(
        true,
        &format!(
            "\"{alias}\" uses the default's plugins ({}); restart its running sessions",
            paths.display(&root)
        ),
    );
    Ok(())
}

/// `mcc link <alias> plugins --off`: give the profile its own copy of the
/// default's plugins again.
fn disable_plugins(paths: &Paths, cfg: &mut Config, alias: &str, dir: &Path) -> Result<()> {
    ui::header(&format!(
        "Stop sharing the default's plugins with \"{alias}\""
    ));
    let before = cfg.profiles[alias].link.clone();
    if !plugins::in_link(&before) {
        ui::result(true, &format!("\"{alias}\" already has its own plugins"));
        return Ok(());
    }
    let mut steps = ui::Steps::new(3);
    let mut undo: Transaction<Undo> = Transaction::new();
    let mut copied = 0;
    let mut launcher_warning = None;
    let result = (|| -> Result<()> {
        // A plugins/ made while the profile ran without the variable.
        let step = steps.start("Move the profile's plugins to a backup");
        match move_plugins_to_backup(dir, &mut undo)? {
            Some(b) => step.ok(&paths.display(&b)),
            None => step.skip("the profile has no plugins/"),
        }

        let step = steps.start("Copy the default's plugins");
        cfg.profiles
            .get_mut(alias)
            .expect("checked above")
            .link
            .retain(|l| l != plugins::ITEM);
        undo.record(Undo::Created(dir.join(plugins::DIR)));
        let spec = copy::spec(Some(&[plugins::ITEM.to_string()]), &cfg.sync)?;
        let rw = Rewriter::for_profile(paths, cfg, dir)?;
        let plan = copy::plan(
            &paths.default_dir,
            &paths.default_json,
            dir,
            &spec,
            &rw,
            false,
        )?;
        copy::apply(
            &plan,
            &paths.default_dir,
            dir,
            &rw,
            ui::is_interactive() && !ui::settings().quiet,
        )?;
        copied = plan.count(copy::Action::Add);
        if let (true, previous) = load_manifest(paths, dir) {
            let mut base = previous.clone().unwrap_or_default();
            forget(&mut base, &[PathBuf::from(plugins::DIR)]);
            plan.manifest(Some(&base)).save(dir)?;
            undo.record(Undo::Manifest(dir.to_path_buf(), previous));
        }
        step.ok(&format!("{copied} item(s) copied"));

        let step = steps.start("Point the launcher at the profile's own plugins");
        launcher_warning = rewrite_launcher(paths, cfg, alias, None, &mut undo)?;
        match &launcher_warning {
            Some(_) => step.skip("no launcher of mcc's"),
            None => step.ok(""),
        }
        cfg.save(&paths.config_file)
    })();
    if let Err(e) = result {
        rollback(paths, &mut undo);
        if let Some(p) = cfg.profiles.get_mut(alias) {
            p.link = before;
        }
        return Err(e);
    }
    undo.commit();
    if let Some(w) = launcher_warning {
        ui::warn(&w);
    }
    ui::result(
        true,
        &format!("\"{alias}\" has its own copy of the plugins; `mcc sync` keeps it up to date"),
    );
    Ok(())
}

/// Before a sync of `alias` with `spec`: name its linked items (sync leaves
/// them alone), and warn about one that is no longer a link.
pub fn report_in_sync(paths: &Paths, cfg: &Config, alias: &str, spec: &Spec) {
    report(paths, cfg, alias, spec, "sync leaves them alone");
}

/// Before `mcc diff`, as [`report_in_sync`].
pub fn report_in_diff(paths: &Paths, cfg: &Config, alias: &str, spec: &Spec) {
    report(paths, cfg, alias, spec, "not compared");
}

fn report(paths: &Paths, cfg: &Config, alias: &str, spec: &Spec, what: &str) {
    let Some(profile) = cfg.profiles.get(alias) else {
        return;
    };
    let linked: Vec<String> = usable(alias, &profile.link)
        .into_iter()
        .filter(|l| spec.items.iter().any(|i| &i.group == l))
        .collect();
    if linked.is_empty() {
        return;
    }
    for item in &linked {
        let Some(rel) = copy::group_path(item) else {
            continue;
        };
        let target = paths.default_dir.join(&rel);
        if !matches!(state(&profile.dir.join(&rel), &target), Ok(State::Linked)) {
            ui::warn(&format!(
                "\"{alias}\": {item} is in link mode, but {} is not a link to {}; run `mcc link {alias} {item}` to link it again, or `mcc link {alias} {item} --off` to sync it",
                paths.display(&profile.dir.join(&rel)),
                paths.display(&target)
            ));
        }
    }
    let names: Vec<&str> = linked.iter().map(|s| s.as_str()).collect();
    ui::info(&format!(
        "linked to the default profile: {} ({what})",
        names.join(", ")
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_mostly_items_can_be_linked() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            check_items(&s(&["skills", "agents", "skills", " claude-md"])).unwrap(),
            s(&["skills", "agents", "claude-md"])
        );
        let err = check_items(&s(&["settings"])).unwrap_err().to_string();
        assert!(err.contains("settings can't be linked"), "{err}");
        assert_eq!(check_items(&s(&["plugins"])).unwrap(), s(&["plugins"]));
        let err = check_items(&s(&["hooks"])).unwrap_err().to_string();
        assert!(err.contains("can't be linked"), "{err}");
        let err = check_items(&s(&["nope"])).unwrap_err().to_string();
        assert!(err.contains("unknown item"), "{err}");
        for item in copy::LINKABLE {
            assert!(copy::group_path(item).is_some(), "{item}");
        }
    }

    #[test]
    fn link_entries_that_cant_be_linked_are_split_off() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        for item in copy::LINKABLE.iter().chain([&plugins::ITEM]) {
            assert!(can_link(item), "{item}");
        }
        for item in ["settings", "hooks", "mcp", "nope", " skills", ""] {
            assert!(!can_link(item), "{item:?}");
        }
        assert_eq!(
            split_link(&s(&["plugins", "settings", "skills", "hooks", " skills"])),
            (
                s(&["plugins", "skills"]),
                s(&["settings", "hooks", " skills"])
            )
        );
        assert_eq!(split_link(&[]), (vec![], vec![]));
    }

    #[test]
    fn forget_drops_only_the_items_paths() {
        let mut m = Manifest::default();
        m.files.insert("skills/a/SKILL.md".into(), "1".into());
        m.files.insert("skills-extra/b".into(), "2".into());
        m.files.insert("CLAUDE.md".into(), "3".into());
        forget(
            &mut m,
            &[PathBuf::from("skills"), PathBuf::from("CLAUDE.md")],
        );
        assert_eq!(m.files.keys().collect::<Vec<_>>(), ["skills-extra/b"]);
    }
}
