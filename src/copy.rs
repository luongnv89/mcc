//! Whitelist-based, one-way copy of Claude Code setup from the default profile
//! into another profile. Used by `add --clone` and `sync`.
//!
//! Only setup is copied — never credentials, identity, history, or caches.
//! Absolute `~/.claude` paths in config files are rewritten to the target dir.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use serde_json::{Map, Value};
use walkdir::WalkDir;

use crate::config::SyncConfig;
use crate::error::CliError;
use crate::fsutil;
use crate::manifest::{self, Manifest};
use crate::paths::Paths;

/// One whitelisted path inside a config dir.
#[derive(Debug, Clone)]
pub struct Item {
    pub group: String,
    pub rel: PathBuf,
    /// Rewrite `~/.claude` paths inside this file (config files only).
    pub rewrite: bool,
}

/// Built-in sync groups: (group, relative path, rewrite paths?).
const BUILTIN: &[(&str, &str, bool)] = &[
    ("settings", "settings.json", true),
    ("settings", "settings.local.json", true),
    ("claude-md", "CLAUDE.md", false),
    ("keybindings", "keybindings.json", true),
    ("statusline", "statusline.conf", true),
    ("agents", "agents", false),
    ("skills", "skills", false),
    ("commands", "commands", false),
    ("hooks", "hooks", false),
    ("output-styles", "output-styles", false),
    ("plugins", "plugins/installed_plugins.json", true),
    ("plugins", "plugins/known_marketplaces.json", true),
    ("plugins", "plugins/marketplaces", false),
    ("plugins", "plugins/cache", false),
];

/// Pseudo-group for MCP servers stored in `.claude.json`.
pub const MCP_GROUP: &str = "mcp";

/// Folder inside a profile where `mcc sync` backs up what it overwrites or
/// removes. Never part of the sync set.
pub const BACKUP_DIR: &str = ".mcc-backup";

pub fn group_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = BUILTIN.iter().map(|b| b.0).collect();
    v.dedup();
    v.push(MCP_GROUP);
    v
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub items: Vec<Item>,
    pub mcp: bool,
}

/// Resolve which items to copy from `--only` and the config's include/exclude.
pub fn spec(only: Option<&[String]>, cfg: &SyncConfig) -> Result<Spec> {
    let known = group_names();
    for g in only.unwrap_or_default().iter().chain(cfg.exclude.iter()) {
        if !known.contains(&g.as_str()) {
            return Err(CliError::usage(format!("unknown sync item \"{g}\""))
                .hint(format!("valid items: {}", known.join(", ")))
                .into());
        }
    }
    let wanted = |g: &str| {
        only.is_none_or(|o| o.iter().any(|x| x == g)) && !cfg.exclude.iter().any(|x| x == g)
    };
    let mut items: Vec<Item> = BUILTIN
        .iter()
        .filter(|(g, _, _)| wanted(g))
        .map(|(g, p, rw)| Item {
            group: g.to_string(),
            rel: PathBuf::from(p),
            rewrite: *rw,
        })
        .collect();
    if only.is_none() {
        for extra in &cfg.extra {
            let rel = PathBuf::from(extra);
            // None for "" or ".", which name the config dir itself.
            let first = rel.components().find_map(|c| match c {
                std::path::Component::Normal(name) => Some(name),
                _ => None,
            });
            if rel.is_absolute()
                || first.is_none()
                || rel
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(CliError::input(format!(
                    "sync.extra entry \"{extra}\" must be a relative path inside the config dir"
                ))
                .hint("edit [sync] extra in ~/.mcc/config.toml, e.g. extra = [\"rules\"]")
                .into());
            }
            // Syncing the backups would prune or overwrite them.
            if first.is_some_and(|name| name.eq_ignore_ascii_case(BACKUP_DIR)) {
                return Err(CliError::input(format!(
                    "sync.extra entry \"{extra}\" points into {BACKUP_DIR}, where sync keeps its backups"
                ))
                .hint("edit [sync] extra in ~/.mcc/config.toml, e.g. extra = [\"rules\"]")
                .into());
            }
            // The sync manifest records this profile's own sync state.
            if first.is_some_and(|name| name.eq_ignore_ascii_case(manifest::FILE)) {
                return Err(CliError::input(format!(
                    "sync.extra entry \"{extra}\" is {}, where sync records what it wrote",
                    manifest::FILE
                ))
                .hint("edit [sync] extra in ~/.mcc/config.toml, e.g. extra = [\"rules\"]")
                .into());
            }
            items.push(Item {
                group: "extra".into(),
                rel,
                rewrite: false,
            });
        }
    }
    Ok(Spec {
        items,
        mcp: wanted(MCP_GROUP),
    })
}

/// Rewrites default-profile paths to a target profile dir. Only paths that the
/// profile actually receives (whitelisted items) are rewritten; anything else,
/// e.g. `~/.claude/statusline/x.sh`, keeps pointing at the default profile.
pub struct Rewriter {
    re: Regex,
    copied: Vec<String>,
    abs_to: String,
    home_rel_to: Option<String>,
    from_dir: PathBuf,
    from_dir_real: PathBuf,
    to_dir: PathBuf,
}

impl Rewriter {
    pub fn new(paths: &Paths, to_dir: &Path, sync_cfg: &SyncConfig) -> Self {
        let copied = spec(None, sync_cfg)
            .map(|s| {
                s.items
                    .iter()
                    .map(|i| i.rel.to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let home = regex::escape(&paths.home.to_string_lossy());
        let re = Regex::new(&format!(
            r#"(?P<p>{home}|~|\$HOME|\$\{{HOME\}})/\.claude(?P<b>$|[/"'\s),;:\]}}])"#
        ))
        .expect("static regex");
        let home_rel_to = to_dir
            .strip_prefix(&paths.home)
            .ok()
            .map(|r| r.to_string_lossy().to_string());
        Self {
            re,
            copied,
            abs_to: to_dir.to_string_lossy().to_string(),
            home_rel_to,
            from_dir: paths.default_dir.clone(),
            from_dir_real: std::fs::canonicalize(&paths.default_dir)
                .unwrap_or_else(|_| paths.default_dir.clone()),
            to_dir: to_dir.to_path_buf(),
        }
    }

    pub fn apply(&self, text: &str) -> String {
        self.re
            .replace_all(text, |c: &regex::Captures| {
                let whole = c.get(0).expect("match");
                if &c["b"] == "/" {
                    // `~/.claude/<rest>`: only rewrite when <rest> is copied.
                    let rest: String = text[whole.end()..]
                        .chars()
                        .take_while(|ch| {
                            !matches!(ch, '"' | '\'' | ')' | ',' | ';' | ':' | ']' | '}')
                                && !ch.is_whitespace()
                        })
                        .collect();
                    let copied = self.copied.iter().any(|rel| {
                        rest == *rel
                            || rest
                                .strip_prefix(rel.as_str())
                                .is_some_and(|r| r.starts_with('/'))
                    });
                    if !copied {
                        return whole.as_str().to_string();
                    }
                }
                let p = &c["p"];
                let target = match (&self.home_rel_to, p) {
                    (Some(rel), "~" | "$HOME" | "${HOME}") => format!("{p}/{rel}"),
                    _ => self.abs_to.clone(),
                };
                format!("{target}{}", &c["b"])
            })
            .into_owned()
    }

    /// Where a copy of the symlink `link` (pointing at `target`) should point.
    /// Relative targets are resolved from the link's real location, since the
    /// copy lives at a different depth (`../../.agents/skills/x` would break).
    /// Targets inside the default dir are mapped into the profile.
    fn link_target(&self, link: &Path, target: &Path) -> PathBuf {
        let resolved = if target.is_absolute() {
            target.to_path_buf()
        } else {
            let parent = link.parent().unwrap_or(Path::new("/"));
            let base = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            normalize(&base.join(target))
        };
        for from in [&self.from_dir, &self.from_dir_real] {
            if let Ok(rest) = resolved.strip_prefix(from) {
                return self.to_dir.join(rest);
            }
        }
        resolved
    }
}

/// Lexically resolve `.` and `..` (the target may not exist, so no canonicalize).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Add,
    Update,
    Remove,
    /// Exists only in the target; kept because `--prune` was not given.
    Extra,
    /// Edited in the profile since mcc last wrote it (or deleted there), while
    /// the default still has what mcc wrote: kept as it is.
    LocalEdit,
    /// Changed in the default *and* edited in the profile since mcc last
    /// wrote it: kept as it is (`--force` overwrites it instead).
    Conflict,
}

impl Action {
    /// Kept as it is in the profile: nothing is written for it.
    pub fn kept(self) -> bool {
        matches!(self, Action::Extra | Action::LocalEdit | Action::Conflict)
    }
}

#[derive(Debug, Clone)]
pub struct Change {
    pub action: Action,
    pub rel: PathBuf,
    pub group: String,
    pub(crate) rewrite: bool,
    /// A directory (its contents are listed as their own changes).
    pub dir: bool,
    /// A conflict that `--force` overwrites (`action` is `Add` or `Update`).
    pub forced: bool,
}

#[derive(Debug, Default)]
pub struct McpChange {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    /// Servers edited (or removed) in the profile only; kept.
    pub local: Vec<String>,
    /// Servers changed on both sides; kept unless `--force`.
    pub conflicts: Vec<String>,
    /// Conflicts that `--force` overwrites (also in `added`/`updated`).
    pub forced: Vec<String>,
    merged: Option<Value>,
    /// Hash of every default server, as the profile receives it.
    hashes: BTreeMap<String, String>,
}

impl McpChange {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

/// What three-way sync compares against (see [`plan_with`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct Base<'a> {
    /// What mcc last wrote into the profile; `None` compares two-way only.
    pub manifest: Option<&'a Manifest>,
    /// Overwrite conflicts instead of keeping them.
    pub force: bool,
}

/// How a profile compares with the default, in counts of files, symlinks
/// and MCP servers (directories are not counted).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    /// Paths and servers the next sync would add, update or remove.
    pub behind: usize,
    /// Profile edits a sync keeps.
    pub local_edits: usize,
    /// Changed on both sides; a sync keeps them unless `--force`.
    pub conflicts: usize,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub unchanged: usize,
    pub mcp: Option<McpChange>,
    /// Hash of every default file or symlink in the sync set, as the profile
    /// receives it (what a sync writes, or finds already there).
    hashes: BTreeMap<PathBuf, String>,
}

impl Plan {
    pub fn count(&self, a: Action) -> usize {
        self.changes.iter().filter(|c| c.action == a).count()
    }

    pub fn actionable(&self) -> usize {
        self.changes.iter().filter(|c| !c.action.kept()).count()
            + self.mcp.as_ref().map_or(0, |m| usize::from(!m.is_empty()))
    }

    /// The summary `mcc list` and `mcc diff` show.
    pub fn status(&self) -> Status {
        let files = |f: &dyn Fn(Action) -> bool| {
            self.changes
                .iter()
                .filter(|c| !c.dir && f(c.action))
                .count()
        };
        let mcp = self.mcp.as_ref();
        Status {
            behind: files(&|a| matches!(a, Action::Add | Action::Update | Action::Remove))
                + mcp.map_or(0, |m| m.added.len() + m.updated.len() + m.removed.len()),
            local_edits: files(&|a| a == Action::LocalEdit) + mcp.map_or(0, |m| m.local.len()),
            conflicts: files(&|a| a == Action::Conflict) + mcp.map_or(0, |m| m.conflicts.len()),
        }
    }

    /// Changes per group: (group, add, update, remove, extra, local, conflict).
    #[allow(clippy::type_complexity)]
    pub fn by_group(&self) -> Vec<(String, usize, usize, usize, usize, usize, usize)> {
        let mut groups: Vec<String> = Vec::new();
        for c in &self.changes {
            if !groups.contains(&c.group) {
                groups.push(c.group.clone());
            }
        }
        groups
            .into_iter()
            .map(|g| {
                let n = |a| {
                    self.changes
                        .iter()
                        .filter(|c| c.group == g && c.action == a)
                        .count()
                };
                (
                    g.clone(),
                    n(Action::Add),
                    n(Action::Update),
                    n(Action::Remove),
                    n(Action::Extra),
                    n(Action::LocalEdit),
                    n(Action::Conflict),
                )
            })
            .collect()
    }

    /// The manifest to record once this plan is applied: `previous` (if any)
    /// with every path and server the profile now has as the default does,
    /// and without what was removed. Kept local edits and conflicts keep
    /// their previous entry, so the next sync classifies them the same way.
    pub fn manifest(&self, previous: Option<&Manifest>) -> Manifest {
        let mut m = previous.cloned().unwrap_or_default();
        for c in &self.changes {
            if c.action == Action::Remove {
                m.files.retain(|k, _| !Path::new(k).starts_with(&c.rel));
            }
        }
        let kept: Vec<&PathBuf> = self
            .changes
            .iter()
            .filter(|c| c.action.kept())
            .map(|c| &c.rel)
            .collect();
        for (rel, h) in &self.hashes {
            if !kept.iter().any(|k| rel.starts_with(k)) {
                m.files.insert(manifest::key(rel), h.clone());
            }
        }
        if let Some(mcp) = &self.mcp {
            for k in &mcp.removed {
                m.mcp.remove(k);
            }
            for (k, h) in &mcp.hashes {
                if !mcp.local.contains(k) && !mcp.conflicts.contains(k) {
                    m.mcp.insert(k.clone(), h.clone());
                }
            }
        }
        m
    }

    /// Paths, relative to `dst_dir`, that [`apply`] will overwrite or delete
    /// and that exist now: every update or removal, a file standing where a
    /// copied path needs its parent dir (see `blocks_parent`), and
    /// `.claude.json` when its MCP servers change. Paths only added are never
    /// listed: they are missing, or sit below a listed path (e.g. a symlinked
    /// dir that is replaced), and must not be read through it.
    pub fn backup_paths(&self, dst_dir: &Path) -> Vec<PathBuf> {
        let mut rels = Vec::new();
        for c in &self.changes {
            if matches!(c.action, Action::Update | Action::Remove) {
                rels.push(c.rel.clone());
            }
            if matches!(c.action, Action::Add | Action::Update)
                && let Some(parent) = c.rel.parent()
                && !parent.as_os_str().is_empty()
                && blocks_parent(&dst_dir.join(parent))
            {
                rels.push(parent.to_path_buf());
            }
        }
        if self.mcp.as_ref().is_some_and(|m| !m.is_empty()) {
            rels.push(PathBuf::from(".claude.json"));
        }
        outermost_existing(dst_dir, rels)
    }
}

/// The paths in `rels` that exist under `dir` (symlinks not followed), each
/// once, without any path that lies inside another one of them: copying the
/// outer path already covers it, and nothing may be copied below a symlink.
pub fn outermost_existing(dir: &Path, rels: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut rels: Vec<PathBuf> = rels
        .into_iter()
        .filter(|r| std::fs::symlink_metadata(dir.join(r)).is_ok())
        .collect();
    rels.sort_by_key(|r| r.components().count());
    let mut out: Vec<PathBuf> = Vec::new();
    for r in rels {
        if !out.iter().any(|o| r.starts_with(o)) {
            out.push(r);
        }
    }
    out
}

fn is_noise(name: &str) -> bool {
    name == ".DS_Store"
        || name == ".trash"
        || name.starts_with("temp_git_")
        || name.starts_with("temp_subdir_")
        || name.contains(".mcc-tmp-")
}

#[derive(PartialEq)]
enum Kind {
    File,
    Dir,
    Link(PathBuf),
    Missing,
}

fn kind(p: &Path) -> Kind {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => {
            Kind::Link(std::fs::read_link(p).unwrap_or_default())
        }
        Ok(m) if m.is_dir() => Kind::Dir,
        Ok(_) => Kind::File,
        Err(_) => Kind::Missing,
    }
}

/// The hash of what copying `src` gives the profile ([`manifest`] hashes):
/// its content (with paths rewritten when `rewrite`), or for a symlink, the
/// target the copy points at. `None` for a directory or a missing path.
fn desired_hash(src: &Path, rewrite: bool, rw: &Rewriter) -> Result<Option<String>> {
    Ok(match kind(src) {
        Kind::Link(target) => Some(manifest::hash_link(&rw.link_target(src, &target))),
        Kind::File => Some(manifest::hash_file(&desired_bytes(src, rewrite, rw)?)),
        Kind::Dir | Kind::Missing => None,
    })
}

/// What copying the file `src` writes into the profile.
pub fn desired_bytes(src: &Path, rewrite: bool, rw: &Rewriter) -> Result<Vec<u8>> {
    let bytes = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
    Ok(if rewrite {
        rewritten(&bytes, rw)
    } else {
        bytes
    })
}

/// The hash of the file or symlink at `p` in the profile, as [`desired_hash`].
fn current_hash(p: &Path) -> Result<Option<String>> {
    Ok(match kind(p) {
        Kind::Link(target) => Some(manifest::hash_link(&target)),
        Kind::File => Some(manifest::hash_file(
            &std::fs::read(p).with_context(|| format!("reading {}", p.display()))?,
        )),
        Kind::Dir | Kind::Missing => None,
    })
}

/// Three-way classification of a default file or symlink (`want`) that the
/// profile has differently (`have`, `None` when missing): what a two-way
/// copy would do (`two_way`: `Add` or `Update`) without a manifest, when the
/// profile lacks a path mcc never wrote, or when only the default changed
/// since mcc wrote it; otherwise a local edit (only the profile changed) or a
/// conflict (both did, including both adding a path mcc never wrote). The `bool` marks a
/// conflict `--force` overwrites.
fn three_way(
    base: &Base,
    rel: &Path,
    want: &str,
    have: Option<&str>,
    two_way: Action,
) -> (Action, bool) {
    let Some(m) = base.manifest else {
        return (two_way, false);
    };
    let Some(last) = m.files.get(&manifest::key(rel)) else {
        // mcc never wrote it: missing in the profile, it is simply new; there,
        // the profile created its own (both sides added it).
        return match have {
            None => (two_way, false),
            Some(_) if base.force => (two_way, true),
            Some(_) => (Action::Conflict, false),
        };
    };
    if have == Some(last.as_str()) {
        (two_way, false)
    } else if want == last {
        (Action::LocalEdit, false)
    } else if base.force {
        (two_way, true)
    } else {
        (Action::Conflict, false)
    }
}

fn rewritten(bytes: &[u8], rw: &Rewriter) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(s) => rw.apply(s).into_bytes(),
        Err(_) => bytes.to_vec(),
    }
}

/// A real (non-symlink) directory with at least one entry.
fn is_nonempty_dir(p: &Path) -> bool {
    kind(p) == Kind::Dir
        && std::fs::read_dir(p)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
}

/// The source has a file or symlink where the profile has its own non-empty
/// directory. Replacing it would delete profile-only content, so that only
/// happens with `--prune` (counted as a removal, which needs confirmation);
/// otherwise the directory is kept and reported as profile-only.
fn replace_dir(push: &mut impl FnMut(Action, PathBuf, bool), rel: PathBuf, prune: bool) {
    if prune {
        push(Action::Remove, rel.clone(), false);
        push(Action::Add, rel, false);
    } else {
        push(Action::Extra, rel, false);
    }
}

/// The first directory strictly between `dst_dir` and `dst_dir/rel` that is a
/// symlink (dangling or not), as a path relative to `dst_dir`. Nothing may be
/// read, written or pruned through it: it can point into the default profile
/// (`plugins -> ~/.claude/plugins`), which must only ever be a source.
fn linked_ancestor(dst_dir: &Path, rel: &Path) -> Option<PathBuf> {
    let mut comps = rel.components().peekable();
    let mut acc = PathBuf::new();
    while let Some(c) = comps.next() {
        if comps.peek().is_none() {
            break;
        }
        acc.push(c);
        if std::fs::symlink_metadata(dst_dir.join(&acc)).is_ok_and(|m| m.file_type().is_symlink()) {
            return Some(acc);
        }
    }
    None
}

/// Compute what a copy from `src` to `dst` would change (two-way: every
/// difference is taken from the source).
pub fn plan(
    src_dir: &Path,
    src_json: &Path,
    dst_dir: &Path,
    spec: &Spec,
    rw: &Rewriter,
    prune: bool,
) -> Result<Plan> {
    plan_with(
        src_dir,
        src_json,
        dst_dir,
        spec,
        rw,
        prune,
        &Base::default(),
    )
}

/// [`plan`], three-way against `base.manifest` (what mcc last wrote into the
/// profile): a file, symlink or MCP server the profile has differently is
/// taken from the source only when the profile still has what mcc wrote, or
/// mcc never wrote it. Otherwise it is a [`Action::LocalEdit`] when the
/// source still has what mcc wrote, else a [`Action::Conflict`] (taken anyway
/// with `base.force`). Deleting such a path in the profile counts as an edit.
pub fn plan_with(
    src_dir: &Path,
    src_json: &Path,
    dst_dir: &Path,
    spec: &Spec,
    rw: &Rewriter,
    prune: bool,
    base: &Base,
) -> Result<Plan> {
    let mut plan = Plan::default();
    let extra_action = if prune { Action::Remove } else { Action::Extra };
    // Symlinked parent dirs in the profile already planned for replacement.
    let mut relinked: BTreeSet<PathBuf> = BTreeSet::new();
    for item in &spec.items {
        let src = src_dir.join(&item.rel);
        let dst = dst_dir.join(&item.rel);
        let hashes = &mut plan.hashes;
        let changes = &mut plan.changes;
        let mut push = |action, rel: PathBuf, forced: bool| {
            changes.push(Change {
                action,
                rel,
                group: item.group.clone(),
                rewrite: item.rewrite,
                dir: false,
                forced,
            })
        };
        // A nested item (`plugins/cache`) under a symlinked profile dir: like
        // a symlink in place of a top-level dir, the link is removed first
        // (only the link, never its target) and the item is copied fresh into
        // a real directory. Whatever lies behind the link is not the
        // profile's, so it is never compared, updated or pruned.
        let detached = match linked_ancestor(dst_dir, &item.rel) {
            Some(link) => {
                if kind(&src) != Kind::Missing && relinked.insert(link.clone()) {
                    push(Action::Remove, link, false);
                }
                true
            }
            None => false,
        };
        match kind(&src) {
            Kind::Missing => {
                if !detached && kind(&dst) != Kind::Missing {
                    push(extra_action, item.rel.clone(), false);
                }
            }
            Kind::File | Kind::Link(_) => {
                let want = desired_hash(&src, item.rewrite, rw)?.expect("file or symlink");
                hashes.insert(item.rel.clone(), want.clone());
                if detached {
                    push(Action::Add, item.rel.clone(), false);
                } else if kind(&dst) == Kind::Missing {
                    let (a, f) = three_way(base, &item.rel, &want, None, Action::Add);
                    push(a, item.rel.clone(), f);
                } else if is_nonempty_dir(&dst) {
                    replace_dir(&mut push, item.rel.clone(), prune);
                } else {
                    let have = current_hash(&dst)?;
                    if have.as_deref() == Some(want.as_str()) {
                        plan.unchanged += 1;
                    } else {
                        let (a, f) =
                            three_way(base, &item.rel, &want, have.as_deref(), Action::Update);
                        push(a, item.rel.clone(), f);
                    }
                }
            }
            Kind::Dir => {
                let dst_kind = if detached { Kind::Missing } else { kind(&dst) };
                // A file or symlink where the directory belongs is removed
                // *before* anything is written, so nothing lands through a
                // symlink; the whole subtree is then copied fresh.
                let fresh = !matches!(dst_kind, Kind::Dir | Kind::Missing);
                if fresh {
                    push(Action::Remove, item.rel.clone(), false);
                }
                if fresh || dst_kind == Kind::Missing {
                    push(Action::Add, item.rel.clone(), false);
                }
                // Nested dirs replaced the same way; their contents are fresh.
                let mut replaced: Vec<PathBuf> = Vec::new();
                // Profile dirs where the source has a file or symlink; their
                // contents are covered by replace_dir and must not be walked.
                let mut dir_conflicts = BTreeSet::new();
                let mut seen = BTreeSet::new();
                let walker = WalkDir::new(&src)
                    .min_depth(1)
                    .follow_links(false)
                    .sort_by_file_name();
                for entry in walker
                    .into_iter()
                    .filter_entry(|e| !is_noise(&e.file_name().to_string_lossy()))
                {
                    let entry = entry.with_context(|| format!("walking {}", src.display()))?;
                    let rel_in = entry
                        .path()
                        .strip_prefix(&src)
                        .expect("walk prefix")
                        .to_path_buf();
                    seen.insert(rel_in.clone());
                    let rel = item.rel.join(&rel_in);
                    let want = desired_hash(entry.path(), false, rw)?;
                    if let Some(h) = &want {
                        hashes.insert(rel.clone(), h.clone());
                    }
                    if fresh || detached || replaced.iter().any(|r| rel_in.starts_with(r)) {
                        push(Action::Add, rel, false);
                        continue;
                    }
                    let d = dst_dir.join(&rel);
                    let Some(want) = want else {
                        // A directory.
                        match kind(&d) {
                            Kind::Dir => {}
                            Kind::Missing => push(Action::Add, rel, false),
                            _ => {
                                replaced.push(rel_in);
                                push(Action::Update, rel, false);
                            }
                        }
                        continue;
                    };
                    if matches!(kind(&d), Kind::Missing) {
                        let (a, f) = three_way(base, &rel, &want, None, Action::Add);
                        push(a, rel, f);
                    } else if is_nonempty_dir(&d) {
                        dir_conflicts.insert(rel_in);
                        replace_dir(&mut push, rel, prune);
                    } else {
                        let have = current_hash(&d)?;
                        if have.as_deref() == Some(want.as_str()) {
                            plan.unchanged += 1;
                        } else {
                            let (a, f) =
                                three_way(base, &rel, &want, have.as_deref(), Action::Update);
                            push(a, rel, f);
                        }
                    }
                }
                if dst_kind == Kind::Dir {
                    let mut walker = WalkDir::new(&dst)
                        .min_depth(1)
                        .follow_links(false)
                        .sort_by_file_name()
                        .into_iter();
                    while let Some(entry) = walker.next() {
                        let entry = entry.with_context(|| format!("walking {}", dst.display()))?;
                        let rel_in = entry
                            .path()
                            .strip_prefix(&dst)
                            .expect("walk prefix")
                            .to_path_buf();
                        if seen.contains(&rel_in) {
                            if dir_conflicts.contains(&rel_in) {
                                walker.skip_current_dir();
                            }
                            continue;
                        }
                        if entry.file_type().is_dir() {
                            walker.skip_current_dir();
                        }
                        push(extra_action, item.rel.join(rel_in), false);
                    }
                }
            }
        }
    }
    for c in &mut plan.changes {
        let side = if matches!(c.action, Action::Remove | Action::Extra) {
            dst_dir
        } else {
            src_dir
        };
        c.dir = kind(&side.join(&c.rel)) == Kind::Dir;
    }
    drop_unneeded_dir_adds(&mut plan);
    if spec.mcp {
        plan.mcp = plan_mcp(src_json, &dst_dir.join(".claude.json"), rw, prune, base)?;
    }
    Ok(plan)
}

/// A directory the profile lacks is only added when something is written
/// inside it, or when the default's copy has no files at all. A directory
/// whose files are all kept (deleted in the profile) stays deleted.
fn drop_unneeded_dir_adds(plan: &mut Plan) {
    let written: Vec<PathBuf> = plan
        .changes
        .iter()
        .filter(|c| matches!(c.action, Action::Add | Action::Update) && !c.dir)
        .map(|c| c.rel.clone())
        .collect();
    let hashes = &plan.hashes;
    plan.changes.retain(|c| {
        !(c.action == Action::Add && c.dir)
            || written.iter().any(|w| w.starts_with(&c.rel))
            || !hashes.keys().any(|h| h.starts_with(&c.rel))
    });
}

pub(crate) fn read_json(path: &Path) -> Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            CliError::input(format!("{} is not valid JSON: {e}", path.display()))
                .hint("fix or remove the file, then retry")
                .into()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The default's MCP servers as the profile receives them (paths
/// rewritten), and the profile's own, from the two `.claude.json` files.
pub fn mcp_servers(
    src_json: &Path,
    dst_json: &Path,
    rw: &Rewriter,
) -> Result<(Map<String, Value>, Map<String, Value>)> {
    let src = read_json(src_json)?;
    let src_servers = match src
        .as_ref()
        .and_then(|v| v.get("mcpServers"))
        .and_then(Value::as_object)
    {
        Some(m) => {
            let text = serde_json::to_string(m)?;
            serde_json::from_str::<Map<String, Value>>(&rw.apply(&text))?
        }
        None => Map::new(),
    };
    let dst = read_json(dst_json)?;
    let dst_servers = dst
        .as_ref()
        .and_then(|v| v.get("mcpServers"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    Ok((src_servers, dst_servers))
}

fn plan_mcp(
    src_json: &Path,
    dst_json: &Path,
    rw: &Rewriter,
    prune: bool,
    base: &Base,
) -> Result<Option<McpChange>> {
    let (src_servers, dst_servers) = mcp_servers(src_json, dst_json, rw)?;
    if src_servers.is_empty() && (dst_servers.is_empty() || !prune) {
        return Ok(None);
    }
    let mut change = McpChange::default();
    let mut merged = if prune {
        Map::new()
    } else {
        dst_servers.clone()
    };
    for (k, v) in &src_servers {
        let want = manifest::hash_json(v);
        change.hashes.insert(k.clone(), want.clone());
        let have = dst_servers.get(k);
        if have == Some(v) {
            merged.insert(k.clone(), v.clone());
            continue;
        }
        let have_hash = have.map(manifest::hash_json);
        let last = base.manifest.and_then(|m| m.mcp.get(k));
        let take = match last {
            None if base.manifest.is_none() || have.is_none() => true,
            // Added on both sides: the profile's own server.
            None if base.force => {
                change.forced.push(k.clone());
                true
            }
            None => {
                change.conflicts.push(k.clone());
                false
            }
            Some(l) if have_hash.as_ref() == Some(l) => true,
            Some(l) if *l == want => {
                change.local.push(k.clone());
                false
            }
            Some(_) if base.force => {
                change.forced.push(k.clone());
                true
            }
            Some(_) => {
                change.conflicts.push(k.clone());
                false
            }
        };
        if take {
            if have.is_none() {
                change.added.push(k.clone());
            } else {
                change.updated.push(k.clone());
            }
            merged.insert(k.clone(), v.clone());
        } else if let Some(old) = have {
            merged.insert(k.clone(), old.clone());
        }
    }
    if prune {
        change.removed = dst_servers
            .keys()
            .filter(|k| !src_servers.contains_key(*k))
            .cloned()
            .collect();
    }
    change.merged = Some(Value::Object(merged));
    Ok(Some(change))
}

/// Apply a plan. Shows a progress bar when `progress` is true.
pub fn apply(
    plan: &Plan,
    src_dir: &Path,
    dst_dir: &Path,
    rw: &Rewriter,
    progress: bool,
) -> Result<()> {
    let todo: Vec<&Change> = plan.changes.iter().filter(|c| !c.action.kept()).collect();
    let bar = if progress && !todo.is_empty() {
        let pb = ProgressBar::new(todo.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "        {bar:30.cyan/blue} {pos}/{len} items {wide_msg:.dim}",
            )
            .expect("template")
            .progress_chars("█▉▊▋▌▍▎▏ "),
        );
        pb
    } else {
        ProgressBar::hidden()
    };
    for c in todo {
        bar.set_message(c.rel.display().to_string());
        let src = src_dir.join(&c.rel);
        let dst = dst_dir.join(&c.rel);
        // Defense in depth: plan() removes symlinked parent dirs before
        // anything is written below them; never write or remove through one
        // (e.g. if the profile changed between plan and apply).
        if let Some(link) = linked_ancestor(dst_dir, &c.rel) {
            return Err(CliError::runtime(format!(
                "refusing to write through symlinked directory {}",
                dst_dir.join(link).display()
            ))
            .hint("re-run the command; mcc replaces the symlink with a real directory")
            .into());
        }
        match c.action {
            Action::Remove => fsutil::remove_any(&dst)?,
            Action::Add | Action::Update => copy_one(&src, &dst, c.rewrite, rw)?,
            Action::Extra | Action::LocalEdit | Action::Conflict => {}
        }
        bar.inc(1);
    }
    bar.finish_and_clear();
    if let Some(m) = &plan.mcp
        && let Some(merged) = &m.merged
        && !m.is_empty()
    {
        let dst_json = dst_dir.join(".claude.json");
        let mut doc = read_json(&dst_json)?.unwrap_or_else(|| Value::Object(Map::new()));
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("mcpServers".into(), merged.clone());
        }
        fsutil::write_atomic(&dst_json, serde_json::to_string_pretty(&doc)?.as_bytes())?;
    }
    Ok(())
}

/// A file, or a symlink that does not lead to a dir, where `copy_one` needs a
/// parent dir: `copy_one` deletes it, so `Plan::backup_paths` backs it up.
fn blocks_parent(parent: &Path) -> bool {
    parent.is_file() || parent.is_symlink() && !parent.is_dir()
}

fn copy_one(src: &Path, dst: &Path, rewrite: bool, rw: &Rewriter) -> Result<()> {
    if let Some(parent) = dst.parent() {
        // A file where a parent dir should be: replace it.
        if blocks_parent(parent) {
            fsutil::remove_any(parent)?;
        }
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    match kind(src) {
        Kind::Link(target) => {
            fsutil::remove_any(dst)?;
            std::os::unix::fs::symlink(rw.link_target(src, &target), dst)
                .with_context(|| format!("linking {}", dst.display()))?;
        }
        Kind::File => {
            if matches!(kind(dst), Kind::Dir | Kind::Link(_)) {
                fsutil::remove_any(dst)?;
            }
            if rewrite {
                let bytes =
                    std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
                fsutil::write_atomic(dst, &rewritten(&bytes, rw))?;
                if let Ok(meta) = std::fs::metadata(src) {
                    let _ = std::fs::set_permissions(dst, meta.permissions());
                }
            } else {
                std::fs::copy(src, dst)
                    .with_context(|| format!("copying {} → {}", src.display(), dst.display()))?;
            }
        }
        Kind::Dir => {
            if !matches!(kind(dst), Kind::Dir | Kind::Missing) {
                fsutil::remove_any(dst)?;
            }
            std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
        }
        Kind::Missing => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Paths, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let src = &paths.default_dir;
        std::fs::create_dir_all(src.join("skills/one")).unwrap();
        std::fs::create_dir_all(src.join("agents")).unwrap();
        std::fs::create_dir_all(src.join("plugins/cache/temp_git_123")).unwrap();
        std::fs::create_dir_all(src.join("projects/p")).unwrap();
        std::fs::write(src.join("skills/one/SKILL.md"), "skill").unwrap();
        std::fs::write(src.join("agents/a.md"), "agent").unwrap();
        std::fs::write(src.join("plugins/cache/temp_git_123/x"), "junk").unwrap();
        std::fs::write(src.join("projects/p/s.jsonl"), "history").unwrap();
        std::fs::write(src.join(".credentials.json"), "secret").unwrap();
        let home = paths.home.display().to_string();
        std::fs::write(
            src.join("settings.json"),
            format!(r#"{{"hooks":"bash '{home}/.claude/hooks/h.sh'","s":"~/.claude/agents/x","ext":"~/.claude/statusline/run.sh","keep":"~/.claude.json","e":"$HOME/.claude"}}"#),
        )
        .unwrap();
        std::fs::write(
            &paths.default_json,
            r#"{"oauthAccount":{"emailAddress":"me@x"},"userID":"u","mcpServers":{"fs":{"command":"~/.claude/bin/fs"}}}"#,
        )
        .unwrap();
        let dst = paths.profile_dir("work");
        std::fs::create_dir_all(&dst).unwrap();
        (d, paths, dst)
    }

    #[test]
    fn rewriter_boundaries() {
        let paths = Paths::new("/h".into());
        let rw = Rewriter::new(&paths, &paths.profile_dir("w"), &SyncConfig::default());
        assert_eq!(
            rw.apply("/h/.claude/hooks/x.sh"),
            "/h/.mcc/profiles/w/hooks/x.sh"
        );
        assert_eq!(rw.apply("~/.claude/skills"), "~/.mcc/profiles/w/skills");
        assert_eq!(rw.apply("$HOME/.claude"), "$HOME/.mcc/profiles/w");
        assert_eq!(
            rw.apply("${HOME}/.claude/skills/a"),
            "${HOME}/.mcc/profiles/w/skills/a"
        );
        assert_eq!(rw.apply("\"/h/.claude\""), "\"/h/.mcc/profiles/w\"");
        assert_eq!(rw.apply("~/.claude.json"), "~/.claude.json");
        assert_eq!(rw.apply("~/.claude-other/x"), "~/.claude-other/x");
        assert_eq!(rw.apply("/other/.claude/x"), "/other/.claude/x");
        // Not copied into the profile → keep pointing at the default.
        assert_eq!(
            rw.apply("bash ~/.claude/statusline/run.sh"),
            "bash ~/.claude/statusline/run.sh"
        );
        assert_eq!(
            rw.apply("\"/h/.claude/projects\""),
            "\"/h/.claude/projects\""
        );
        assert_eq!(
            rw.apply("~/.claude/plugins/data/x"),
            "~/.claude/plugins/data/x"
        );
        assert_eq!(rw.apply("~/.claude/skillsfoo/x"), "~/.claude/skillsfoo/x");
        // Copied → rewritten, including multi-segment items.
        assert_eq!(
            rw.apply("~/.claude/plugins/cache/m/p"),
            "~/.mcc/profiles/w/plugins/cache/m/p"
        );
        assert_eq!(
            rw.apply("\"~/.claude/statusline.conf\""),
            "\"~/.mcc/profiles/w/statusline.conf\""
        );
        // Excluded groups are not rewritten either.
        let cfg = SyncConfig {
            exclude: vec!["hooks".into()],
            extra: vec![],
        };
        let rw = Rewriter::new(&paths, &paths.profile_dir("w"), &cfg);
        assert_eq!(rw.apply("~/.claude/hooks/h.sh"), "~/.claude/hooks/h.sh");
    }

    #[test]
    fn clone_copies_whitelist_only() {
        let (_d, paths, dst) = setup();
        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        let spec = spec(None, &SyncConfig::default()).unwrap();
        let p = plan(
            &paths.default_dir,
            &paths.default_json,
            &dst,
            &spec,
            &rw,
            false,
        )
        .unwrap();
        // settings.json, skills/, skills/one/, skills/one/SKILL.md, agents/, agents/a.md, plugins/cache/
        assert_eq!(p.count(Action::Add), 7, "{:?}", p.changes);
        apply(&p, &paths.default_dir, &dst, &rw, false).unwrap();

        assert!(dst.join("skills/one/SKILL.md").exists());
        assert!(dst.join("agents/a.md").exists());
        assert!(!dst.join(".credentials.json").exists());
        assert!(!dst.join("projects").exists());
        assert!(!dst.join("plugins/cache/temp_git_123").exists());

        let settings = std::fs::read_to_string(dst.join("settings.json")).unwrap();
        assert!(settings.contains(&format!("{}/hooks/h.sh", dst.display())));
        assert!(settings.contains("~/.mcc/profiles/work/agents/x"));
        assert!(
            settings.contains("\"ext\":\"~/.claude/statusline/run.sh\""),
            "{settings}"
        );
        assert!(settings.contains("\"keep\":\"~/.claude.json\""));
        assert!(settings.contains("$HOME/.mcc/profiles/work"));

        let json: Value =
            serde_json::from_str(&std::fs::read_to_string(dst.join(".claude.json")).unwrap())
                .unwrap();
        assert!(json.get("oauthAccount").is_none());
        assert!(json.get("userID").is_none());
        assert_eq!(json["mcpServers"]["fs"]["command"], "~/.claude/bin/fs"); // bin/ is not copied

        // Second plan is a no-op.
        let p2 = plan(
            &paths.default_dir,
            &paths.default_json,
            &dst,
            &spec,
            &rw,
            false,
        )
        .unwrap();
        assert_eq!(p2.actionable(), 0, "{:?}", p2.changes);
    }

    #[test]
    fn prune_and_extras() {
        let (_d, paths, dst) = setup();
        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        let sp = spec(Some(&["agents".to_string()]), &SyncConfig::default()).unwrap();
        assert!(!sp.mcp);
        std::fs::create_dir_all(dst.join("agents/sub")).unwrap();
        std::fs::write(dst.join("agents/mine.md"), "x").unwrap();
        std::fs::write(dst.join("agents/sub/deep.md"), "x").unwrap();

        let p = plan(
            &paths.default_dir,
            &paths.default_json,
            &dst,
            &sp,
            &rw,
            false,
        )
        .unwrap();
        assert_eq!(p.count(Action::Add), 1);
        assert_eq!(p.count(Action::Extra), 2); // agents/mine.md, agents/sub (collapsed)
        apply(&p, &paths.default_dir, &dst, &rw, false).unwrap();
        assert!(dst.join("agents/mine.md").exists());

        let p = plan(
            &paths.default_dir,
            &paths.default_json,
            &dst,
            &sp,
            &rw,
            true,
        )
        .unwrap();
        assert_eq!(p.count(Action::Remove), 2);
        apply(&p, &paths.default_dir, &dst, &rw, false).unwrap();
        assert!(!dst.join("agents/mine.md").exists());
        assert!(!dst.join("agents/sub").exists());
        assert!(dst.join("agents/a.md").exists());
    }

    #[test]
    fn symlinks_are_preserved_and_retargeted() {
        let (_d, paths, dst) = setup();
        let src = &paths.default_dir;
        std::os::unix::fs::symlink(src.join("skills/one"), src.join("skills/abs-link")).unwrap();
        std::os::unix::fs::symlink("/opt/elsewhere", src.join("skills/ext-link")).unwrap();
        // Relative links: one leaving the config dir, one staying inside it.
        std::fs::create_dir_all(paths.home.join(".agents/skills/shared")).unwrap();
        std::os::unix::fs::symlink("../../.agents/skills/shared", src.join("skills/rel-out"))
            .unwrap();
        std::os::unix::fs::symlink("one", src.join("skills/rel-in")).unwrap();
        std::fs::create_dir_all(src.join("skills/.trash/old")).unwrap();
        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        let sp = spec(Some(&["skills".to_string()]), &SyncConfig::default()).unwrap();
        let p = plan(src, &paths.default_json, &dst, &sp, &rw, false).unwrap();
        apply(&p, src, &dst, &rw, false).unwrap();
        assert_eq!(
            std::fs::read_link(dst.join("skills/abs-link")).unwrap(),
            dst.join("skills/one")
        );
        assert_eq!(
            std::fs::read_link(dst.join("skills/ext-link")).unwrap(),
            PathBuf::from("/opt/elsewhere")
        );
        let shared = std::fs::canonicalize(paths.home.join(".agents/skills/shared")).unwrap();
        assert_eq!(
            std::fs::canonicalize(dst.join("skills/rel-out")).unwrap(),
            shared
        );
        assert_eq!(
            std::fs::read_link(dst.join("skills/rel-in")).unwrap(),
            dst.join("skills/one")
        );
        assert!(!dst.join("skills/.trash").exists());
        let p2 = plan(src, &paths.default_json, &dst, &sp, &rw, false).unwrap();
        assert_eq!(p2.actionable(), 0);
    }

    fn skills_only(paths: &Paths, dst: &Path) -> Plan {
        let rw = Rewriter::new(paths, dst, &SyncConfig::default());
        let sp = spec(Some(&["skills".to_string()]), &SyncConfig::default()).unwrap();
        let p = plan(
            &paths.default_dir,
            &paths.default_json,
            dst,
            &sp,
            &rw,
            false,
        )
        .unwrap();
        apply(&p, &paths.default_dir, dst, &rw, false).unwrap();
        p
    }

    #[test]
    fn symlink_in_place_of_dir_is_replaced_without_writing_through_it() {
        let (_d, paths, dst) = setup();
        let outside = paths.home.join("outside");
        std::fs::create_dir_all(outside.join("one")).unwrap();
        std::fs::write(outside.join("one/SKILL.md"), "skill").unwrap();
        std::os::unix::fs::symlink(&outside, dst.join("skills")).unwrap();

        let p = skills_only(&paths, &dst);
        assert_eq!(p.changes[0].action, Action::Remove, "{:?}", p.changes);
        let skills = std::fs::symlink_metadata(dst.join("skills")).unwrap();
        assert!(skills.is_dir() && !skills.file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(dst.join("skills/one/SKILL.md")).unwrap(),
            "skill"
        );
        // The symlink's target is left exactly as it was.
        assert!(outside.join("one/SKILL.md").exists());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);

        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        let sp = spec(Some(&["skills".to_string()]), &SyncConfig::default()).unwrap();
        let p2 = plan(
            &paths.default_dir,
            &paths.default_json,
            &dst,
            &sp,
            &rw,
            false,
        )
        .unwrap();
        assert_eq!(p2.actionable(), 0, "{:?}", p2.changes);
    }

    #[test]
    fn file_in_place_of_dir_is_replaced() {
        let (_d, paths, dst) = setup();
        std::fs::write(dst.join("skills"), "not a dir").unwrap();
        skills_only(&paths, &dst);
        assert!(dst.join("skills").is_dir());
        assert_eq!(
            std::fs::read_to_string(dst.join("skills/one/SKILL.md")).unwrap(),
            "skill"
        );
    }

    #[test]
    fn nested_symlink_in_place_of_dir_is_replaced() {
        let (_d, paths, dst) = setup();
        let outside = paths.home.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("SKILL.md"), "skill").unwrap();
        std::fs::create_dir_all(dst.join("skills")).unwrap();
        std::os::unix::fs::symlink(&outside, dst.join("skills/one")).unwrap();

        skills_only(&paths, &dst);
        let one = std::fs::symlink_metadata(dst.join("skills/one")).unwrap();
        assert!(one.is_dir() && !one.file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(dst.join("skills/one/SKILL.md")).unwrap(),
            "skill"
        );
        assert!(outside.join("SKILL.md").exists());
    }

    fn plan_skills(paths: &Paths, dst: &Path, prune: bool) -> Plan {
        let rw = Rewriter::new(paths, dst, &SyncConfig::default());
        let sp = spec(Some(&["skills".to_string()]), &SyncConfig::default()).unwrap();
        plan(
            &paths.default_dir,
            &paths.default_json,
            dst,
            &sp,
            &rw,
            prune,
        )
        .unwrap()
    }

    fn apply_plan(paths: &Paths, dst: &Path, p: &Plan) {
        let rw = Rewriter::new(paths, dst, &SyncConfig::default());
        apply(p, &paths.default_dir, dst, &rw, false).unwrap();
    }

    #[test]
    fn symlinked_source_dir_never_deletes_profile_dir_without_prune() {
        let (_d, paths, dst) = setup();
        let dotfiles = paths.home.join("dotfiles/skills");
        std::fs::create_dir_all(dotfiles.join("shared")).unwrap();
        std::fs::write(dotfiles.join("shared/SKILL.md"), "shared").unwrap();
        std::fs::remove_dir_all(paths.default_dir.join("skills")).unwrap();
        std::os::unix::fs::symlink(&dotfiles, paths.default_dir.join("skills")).unwrap();
        std::fs::create_dir_all(dst.join("skills/mine")).unwrap();
        std::fs::write(dst.join("skills/mine/SKILL.md"), "mine").unwrap();

        let p = plan_skills(&paths, &dst, false);
        assert_eq!(p.count(Action::Remove), 0, "{:?}", p.changes);
        assert_eq!(p.count(Action::Update), 0, "{:?}", p.changes);
        assert_eq!(p.count(Action::Extra), 1, "{:?}", p.changes);
        apply_plan(&paths, &dst, &p);
        assert_eq!(
            std::fs::read_to_string(dst.join("skills/mine/SKILL.md")).unwrap(),
            "mine"
        );

        let p = plan_skills(&paths, &dst, true);
        assert_eq!(p.count(Action::Remove), 1, "{:?}", p.changes);
        apply_plan(&paths, &dst, &p);
        assert_eq!(std::fs::read_link(dst.join("skills")).unwrap(), dotfiles);
        assert!(dotfiles.join("shared/SKILL.md").exists());
        assert!(!dotfiles.join("mine").exists());
        assert_eq!(plan_skills(&paths, &dst, true).actionable(), 0);
    }

    #[test]
    fn nested_symlinked_source_dir_never_deletes_profile_dir_without_prune() {
        let (_d, paths, dst) = setup();
        let target = paths.home.join("dotfiles/one");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("SKILL.md"), "skill").unwrap();
        std::fs::remove_dir_all(paths.default_dir.join("skills/one")).unwrap();
        std::os::unix::fs::symlink(&target, paths.default_dir.join("skills/one")).unwrap();
        std::fs::create_dir_all(dst.join("skills/one")).unwrap();
        std::fs::write(dst.join("skills/one/mine.md"), "mine").unwrap();

        let p = plan_skills(&paths, &dst, false);
        assert_eq!(p.count(Action::Remove), 0, "{:?}", p.changes);
        assert_eq!(p.count(Action::Extra), 1, "{:?}", p.changes);
        apply_plan(&paths, &dst, &p);
        assert!(dst.join("skills/one/mine.md").exists());

        // With --prune the dir is replaced once; nothing is removed through the new link.
        let p = plan_skills(&paths, &dst, true);
        assert_eq!(p.count(Action::Remove), 1, "{:?}", p.changes);
        apply_plan(&paths, &dst, &p);
        assert_eq!(std::fs::read_link(dst.join("skills/one")).unwrap(), target);
        assert_eq!(
            std::fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "skill"
        );
    }

    #[test]
    fn nested_items_never_written_or_pruned_through_symlinked_parent() {
        let (_d, paths, dst) = setup();
        let src = &paths.default_dir;
        std::fs::create_dir_all(src.join("plugins/cache/m/p")).unwrap();
        std::fs::write(src.join("plugins/cache/m/p/f"), "plugin").unwrap();
        std::fs::write(
            src.join("plugins/installed_plugins.json"),
            r#"{"installPath":"~/.claude/plugins/cache/m/p"}"#,
        )
        .unwrap();
        // The profile's plugins/ points at a dir with content the default lacks.
        let outside = paths.home.join("dotfiles/plugins");
        std::fs::create_dir_all(outside.join("cache/other")).unwrap();
        std::fs::write(outside.join("cache/other/keep.txt"), "keep").unwrap();
        std::fs::write(outside.join("known_marketplaces.json"), "{}").unwrap();
        std::fs::write(outside.join("installed_plugins.json"), "old").unwrap();
        std::os::unix::fs::symlink(&outside, dst.join("plugins")).unwrap();

        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        let sp = spec(Some(&["plugins".to_string()]), &SyncConfig::default()).unwrap();
        let mk = || {
            plan(
                &paths.default_dir,
                &paths.default_json,
                &dst,
                &sp,
                &rw,
                true,
            )
            .unwrap()
        };
        let p = mk();
        assert_eq!(p.changes[0].action, Action::Remove, "{:?}", p.changes);
        assert_eq!(p.changes[0].rel, PathBuf::from("plugins"));
        assert_eq!(p.count(Action::Remove), 1, "{:?}", p.changes);
        // Only the link is backed up, never what lies behind it.
        assert_eq!(p.backup_paths(&dst), [PathBuf::from("plugins")]);
        apply(&p, &paths.default_dir, &dst, &rw, false).unwrap();

        // The link's target is untouched: nothing written, nothing pruned.
        assert_eq!(
            std::fs::read_to_string(outside.join("installed_plugins.json")).unwrap(),
            "old"
        );
        assert!(outside.join("cache/other/keep.txt").exists());
        assert!(outside.join("known_marketplaces.json").exists());
        // The profile now has a real plugins/ dir with the copied items.
        let meta = std::fs::symlink_metadata(dst.join("plugins")).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(dst.join("plugins/cache/m/p/f")).unwrap(),
            "plugin"
        );
        assert!(
            std::fs::read_to_string(dst.join("plugins/installed_plugins.json"))
                .unwrap()
                .contains("~/.mcc/profiles/work/plugins/cache/m/p")
        );
        assert_eq!(mk().actionable(), 0, "{:?}", mk().changes);
    }

    #[test]
    fn apply_refuses_to_write_through_symlinked_parent() {
        let (_d, paths, dst) = setup();
        let outside = paths.home.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        let sp = spec(Some(&["plugins".to_string()]), &SyncConfig::default()).unwrap();
        let p = plan(
            &paths.default_dir,
            &paths.default_json,
            &dst,
            &sp,
            &rw,
            false,
        )
        .unwrap();
        assert!(p.changes.iter().any(|c| c.rel.starts_with("plugins/cache")));
        // The profile changes between plan and apply.
        std::os::unix::fs::symlink(&outside, dst.join("plugins")).unwrap();
        let err = apply(&p, &paths.default_dir, &dst, &rw, false).unwrap_err();
        assert!(err.to_string().contains("symlinked directory"), "{err}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn backup_paths_lists_what_apply_overwrites_or_removes() {
        let (_d, paths, dst) = setup();
        std::fs::create_dir_all(dst.join("skills/one")).unwrap();
        std::fs::write(dst.join("skills/one/SKILL.md"), "edited").unwrap();
        std::fs::create_dir_all(dst.join("skills/mine/deep")).unwrap();
        std::fs::write(dst.join("skills/mine/deep/x.md"), "mine").unwrap();
        // A file where plugins/cache needs its parent dir.
        std::fs::write(dst.join("plugins"), "not a dir").unwrap();
        std::fs::write(dst.join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();
        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        let groups = ["skills", "agents", "plugins", "mcp"].map(String::from);
        let sp = spec(Some(&groups), &SyncConfig::default()).unwrap();
        let p = plan(
            &paths.default_dir,
            &paths.default_json,
            &dst,
            &sp,
            &rw,
            true,
        )
        .unwrap();
        assert!(p.count(Action::Add) > 0, "{:?}", p.changes);
        let mut got = p.backup_paths(&dst);
        got.sort();
        let want: Vec<PathBuf> = [
            ".claude.json",
            "plugins",
            "skills/mine",
            "skills/one/SKILL.md",
        ]
        .map(PathBuf::from)
        .into();
        assert_eq!(got, want, "{:?}", p.changes);
    }

    fn change(action: Action, rel: &str) -> Change {
        Change {
            action,
            rel: PathBuf::from(rel),
            group: "test".into(),
            rewrite: false,
            dir: false,
            forced: false,
        }
    }

    #[test]
    fn backup_paths_dedupes_and_keeps_outermost() {
        let d = tempfile::tempdir().unwrap();
        let dst = d.path();
        std::fs::create_dir_all(dst.join("skills/mine/deep")).unwrap();
        std::fs::write(dst.join("skills/mine/deep/x.md"), "x").unwrap();
        std::fs::write(dst.join("CLAUDE.md"), "edited").unwrap();
        std::fs::write(dst.join("plugins"), "a file where a dir belongs").unwrap();
        std::fs::write(dst.join(".claude.json"), "{}").unwrap();
        std::fs::create_dir_all(dst.join("agents")).unwrap();
        std::fs::write(dst.join("agents/a.md"), "exists, but only added").unwrap();
        let mut p = Plan {
            changes: vec![
                change(Action::Update, "CLAUDE.md"),
                change(Action::Update, "CLAUDE.md"),
                change(Action::Remove, "skills/mine/deep/x.md"),
                change(Action::Remove, "skills/mine"),
                change(Action::Remove, "skills/gone"),
                change(Action::Add, "agents/a.md"),
                change(Action::Extra, "agents"),
                change(Action::Add, "plugins/installed_plugins.json"),
                change(Action::Add, "plugins/cache"),
            ],
            unchanged: 0,
            hashes: BTreeMap::new(),
            mcp: Some(McpChange {
                added: vec!["fs".into()],
                ..McpChange::default()
            }),
        };
        let mut got = p.backup_paths(dst);
        got.sort();
        let want: Vec<PathBuf> = [".claude.json", "CLAUDE.md", "plugins", "skills/mine"]
            .map(PathBuf::from)
            .into();
        assert_eq!(got, want);

        // No MCP change: .claude.json is not touched.
        p.mcp = Some(McpChange::default());
        assert!(!p.backup_paths(dst).contains(&PathBuf::from(".claude.json")));
    }

    #[test]
    fn extra_entries_must_stay_out_of_the_backup_dir() {
        let extra = |e: &str| SyncConfig {
            exclude: vec![],
            extra: vec![e.to_string()],
        };
        for bad in [
            "",
            ".",
            "./",
            "/abs",
            "../up",
            ".mcc-backup",
            "./.mcc-backup/20260101-000000.000",
            ".MCC-Backup",
            ".mcc-sync.json",
            "./.mcc-sync.json",
        ] {
            let err = spec(None, &extra(bad)).unwrap_err();
            assert_eq!(
                crate::error::classify(&err).0,
                crate::error::EXIT_INPUT,
                "{bad:?}: {err}"
            );
        }
        for ok in ["rules", "./rules", "rules/.mcc-backup"] {
            let sp = spec(None, &extra(ok)).unwrap();
            assert!(sp.items.iter().any(|i| i.group == "extra"), "{ok:?}");
        }
    }

    #[test]
    fn unknown_group_is_usage_error() {
        let err = spec(Some(&["nope".to_string()]), &SyncConfig::default()).unwrap_err();
        assert_eq!(crate::error::classify(&err).0, crate::error::EXIT_USAGE);
    }

    /// Clone, then return the manifest the clone recorded.
    fn cloned(paths: &Paths, dst: &Path) -> Manifest {
        let rw = Rewriter::new(paths, dst, &SyncConfig::default());
        let sp = spec(None, &SyncConfig::default()).unwrap();
        let p = plan(
            &paths.default_dir,
            &paths.default_json,
            dst,
            &sp,
            &rw,
            false,
        )
        .unwrap();
        apply(&p, &paths.default_dir, dst, &rw, false).unwrap();
        p.manifest(None)
    }

    fn plan3(paths: &Paths, dst: &Path, m: &Manifest, force: bool) -> Plan {
        let rw = Rewriter::new(paths, dst, &SyncConfig::default());
        let sp = spec(None, &SyncConfig::default()).unwrap();
        plan_with(
            &paths.default_dir,
            &paths.default_json,
            dst,
            &sp,
            &rw,
            false,
            &Base {
                manifest: Some(m),
                force,
            },
        )
        .unwrap()
    }

    fn action_of(p: &Plan, rel: &str) -> Option<(Action, bool)> {
        p.changes
            .iter()
            .find(|c| c.rel == Path::new(rel))
            .map(|c| (c.action, c.forced))
    }

    #[test]
    fn three_way_classifies_each_path() {
        let (_d, paths, dst) = setup();
        let src = paths.default_dir.clone();
        std::fs::write(src.join("agents/b.md"), "b").unwrap();
        std::fs::write(src.join("agents/c.md"), "c").unwrap();
        std::fs::create_dir_all(src.join("skills/two")).unwrap();
        std::fs::write(src.join("skills/two/SKILL.md"), "two").unwrap();
        let m = cloned(&paths, &dst);
        assert!(m.files.contains_key("agents/a.md"), "{m:?}");
        assert!(m.files.contains_key("settings.json"));
        assert!(m.mcp.contains_key("fs"));
        assert!(!m.files.keys().any(|k| k.contains("temp_git_")));

        std::fs::write(src.join("agents/a.md"), "agent v2").unwrap(); // upstream
        std::fs::write(dst.join("agents/b.md"), "my b").unwrap(); // local edit
        std::fs::write(src.join("agents/c.md"), "c v2").unwrap(); // both
        std::fs::write(dst.join("agents/c.md"), "my c").unwrap();
        std::fs::remove_dir_all(dst.join("skills/two")).unwrap(); // deleted locally
        std::fs::write(src.join("agents/new.md"), "new").unwrap(); // new upstream

        let p = plan3(&paths, &dst, &m, false);
        assert_eq!(action_of(&p, "agents/a.md"), Some((Action::Update, false)));
        assert_eq!(
            action_of(&p, "agents/b.md"),
            Some((Action::LocalEdit, false))
        );
        assert_eq!(
            action_of(&p, "agents/c.md"),
            Some((Action::Conflict, false))
        );
        assert_eq!(
            action_of(&p, "skills/two/SKILL.md"),
            Some((Action::LocalEdit, false))
        );
        // The deleted dir is not re-created empty.
        assert_eq!(action_of(&p, "skills/two"), None, "{:?}", p.changes);
        assert_eq!(action_of(&p, "agents/new.md"), Some((Action::Add, false)));
        assert_eq!(
            p.status(),
            Status {
                behind: 2,
                local_edits: 2,
                conflicts: 1
            }
        );
        // Only what changed upstream is backed up and written.
        assert_eq!(p.backup_paths(&dst), vec![PathBuf::from("agents/a.md")]);
        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        apply(&p, &paths.default_dir, &dst, &rw, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("agents/a.md")).unwrap(),
            "agent v2"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("agents/b.md")).unwrap(),
            "my b"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("agents/c.md")).unwrap(),
            "my c"
        );
        assert!(!dst.join("skills/two").exists());

        // Kept paths keep their old entry, so they classify the same next time.
        let m2 = p.manifest(Some(&m));
        assert_eq!(m2.files["agents/b.md"], m.files["agents/b.md"]);
        assert_eq!(m2.files["agents/c.md"], m.files["agents/c.md"]);
        assert_ne!(m2.files["agents/a.md"], m.files["agents/a.md"]);
        assert!(m2.files.contains_key("agents/new.md"));
        let p = plan3(&paths, &dst, &m2, false);
        assert_eq!(
            action_of(&p, "agents/b.md"),
            Some((Action::LocalEdit, false))
        );
        assert_eq!(
            action_of(&p, "agents/c.md"),
            Some((Action::Conflict, false))
        );
        assert_eq!(action_of(&p, "agents/a.md"), None);

        // Added on both sides (mcc never wrote it): a conflict.
        std::fs::write(src.join("agents/both.md"), "theirs").unwrap();
        std::fs::write(dst.join("agents/both.md"), "mine").unwrap();
        let p = plan3(&paths, &dst, &m2, false);
        assert_eq!(
            action_of(&p, "agents/both.md"),
            Some((Action::Conflict, false))
        );

        // --force overwrites conflicts, never local edits.
        let p = plan3(&paths, &dst, &m2, true);
        assert_eq!(
            action_of(&p, "agents/both.md"),
            Some((Action::Update, true))
        );
        assert_eq!(action_of(&p, "agents/c.md"), Some((Action::Update, true)));
        assert_eq!(
            action_of(&p, "agents/b.md"),
            Some((Action::LocalEdit, false))
        );
    }

    #[test]
    fn three_way_mcp_servers() {
        let (_d, paths, dst) = setup();
        let m = cloned(&paths, &dst);
        // Local edit: the profile changes "fs"; the default adds "gh".
        std::fs::write(
            dst.join(".claude.json"),
            r#"{"mcpServers":{"fs":{"command":"mine"}}}"#,
        )
        .unwrap();
        std::fs::write(
            &paths.default_json,
            r#"{"mcpServers":{"fs":{"command":"~/.claude/bin/fs"},"gh":{"command":"gh"}}}"#,
        )
        .unwrap();
        let p = plan3(&paths, &dst, &m, false);
        let mcp = p.mcp.as_ref().unwrap();
        assert_eq!(mcp.local, vec!["fs".to_string()]);
        assert_eq!(mcp.added, vec!["gh".to_string()]);
        let rw = Rewriter::new(&paths, &dst, &SyncConfig::default());
        apply(&p, &paths.default_dir, &dst, &rw, false).unwrap();
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(dst.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(v["mcpServers"]["fs"]["command"], "mine");
        assert_eq!(v["mcpServers"]["gh"]["command"], "gh");

        // Both sides change "fs": a conflict, kept unless forced.
        let m2 = p.manifest(Some(&m));
        std::fs::write(
            &paths.default_json,
            r#"{"mcpServers":{"fs":{"command":"v2"},"gh":{"command":"gh"}}}"#,
        )
        .unwrap();
        let p = plan3(&paths, &dst, &m2, false);
        assert_eq!(p.mcp.as_ref().unwrap().conflicts, vec!["fs".to_string()]);
        assert!(p.mcp.as_ref().unwrap().is_empty());
        let p = plan3(&paths, &dst, &m2, true);
        let mcp = p.mcp.as_ref().unwrap();
        assert_eq!(mcp.updated, vec!["fs".to_string()]);
        assert_eq!(mcp.forced, vec!["fs".to_string()]);
    }
}
