//! Whitelist-based, one-way copy of Claude Code setup from the default profile
//! into another profile. Used by `add --clone` and `sync`.
//!
//! Only setup is copied — never credentials, identity, history, or caches.
//! Absolute `~/.claude` paths in config files are rewritten to the target dir.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use serde_json::{Map, Value};
use walkdir::WalkDir;

use crate::config::SyncConfig;
use crate::error::CliError;
use crate::fsutil;
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
        .map(|(g, p, rw)| Item { group: g.to_string(), rel: PathBuf::from(p), rewrite: *rw })
        .collect();
    if only.is_none() {
        for extra in &cfg.extra {
            let rel = PathBuf::from(extra);
            if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                return Err(CliError::input(format!("sync.extra entry \"{extra}\" must be a relative path inside the config dir")).into());
            }
            items.push(Item { group: "extra".into(), rel, rewrite: false });
        }
    }
    Ok(Spec { items, mcp: wanted(MCP_GROUP) })
}

/// Rewrites default-profile paths to a target profile dir.
pub struct Rewriter {
    re: Regex,
    abs_to: String,
    home_rel_to: Option<String>,
    from_dir: PathBuf,
    from_dir_real: PathBuf,
    to_dir: PathBuf,
}

impl Rewriter {
    pub fn new(paths: &Paths, to_dir: &Path) -> Self {
        let home = regex::escape(&paths.home.to_string_lossy());
        let re = Regex::new(&format!(
            r#"(?P<p>{home}|~|\$HOME|\$\{{HOME\}})/\.claude(?P<b>$|[/"'\s),;:\]}}])"#
        ))
        .expect("static regex");
        let home_rel_to =
            to_dir.strip_prefix(&paths.home).ok().map(|r| r.to_string_lossy().to_string());
        Self {
            re,
            abs_to: to_dir.to_string_lossy().to_string(),
            home_rel_to,
            from_dir: paths.default_dir.clone(),
            from_dir_real: std::fs::canonicalize(&paths.default_dir).unwrap_or_else(|_| paths.default_dir.clone()),
            to_dir: to_dir.to_path_buf(),
        }
    }

    pub fn apply(&self, text: &str) -> String {
        self.re
            .replace_all(text, |c: &regex::Captures| {
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
}

#[derive(Debug, Clone)]
pub struct Change {
    pub action: Action,
    pub rel: PathBuf,
    pub group: String,
    rewrite: bool,
}

#[derive(Debug, Default)]
pub struct McpChange {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    merged: Option<Value>,
}

impl McpChange {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub unchanged: usize,
    pub mcp: Option<McpChange>,
}

impl Plan {
    pub fn count(&self, a: Action) -> usize {
        self.changes.iter().filter(|c| c.action == a).count()
    }

    pub fn actionable(&self) -> usize {
        self.changes.iter().filter(|c| c.action != Action::Extra).count()
            + self.mcp.as_ref().map_or(0, |m| usize::from(!m.is_empty()))
    }

    /// Files touched per group: group → (add, update, remove).
    pub fn by_group(&self) -> Vec<(String, usize, usize, usize, usize)> {
        let mut groups: Vec<String> = Vec::new();
        for c in &self.changes {
            if !groups.contains(&c.group) {
                groups.push(c.group.clone());
            }
        }
        groups
            .into_iter()
            .map(|g| {
                let n = |a| self.changes.iter().filter(|c| c.group == g && c.action == a).count();
                let (add, upd, rem, ext) = (n(Action::Add), n(Action::Update), n(Action::Remove), n(Action::Extra));
                (g, add, upd, rem, ext)
            })
            .collect()
    }
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
        Ok(m) if m.file_type().is_symlink() => Kind::Link(std::fs::read_link(p).unwrap_or_default()),
        Ok(m) if m.is_dir() => Kind::Dir,
        Ok(_) => Kind::File,
        Err(_) => Kind::Missing,
    }
}

/// Does `dst` already match what copying `src` would produce?
fn same(src: &Path, dst: &Path, rewrite: bool, rw: &Rewriter) -> Result<bool> {
    match (kind(src), kind(dst)) {
        (Kind::Link(a), Kind::Link(b)) => Ok(rw.link_target(src, &a) == b),
        (Kind::File, Kind::File) => {
            if !rewrite {
                let (a, b) = (std::fs::metadata(src)?.len(), std::fs::metadata(dst)?.len());
                if a != b {
                    return Ok(false);
                }
            }
            let a = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
            let b = std::fs::read(dst).with_context(|| format!("reading {}", dst.display()))?;
            Ok(if rewrite { rewritten(&a, rw) == b } else { a == b })
        }
        _ => Ok(false),
    }
}

fn rewritten(bytes: &[u8], rw: &Rewriter) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(s) => rw.apply(s).into_bytes(),
        Err(_) => bytes.to_vec(),
    }
}

/// Compute what a copy from `src` to `dst` would change.
pub fn plan(
    src_dir: &Path,
    src_json: &Path,
    dst_dir: &Path,
    spec: &Spec,
    rw: &Rewriter,
    prune: bool,
) -> Result<Plan> {
    let mut plan = Plan::default();
    let extra_action = if prune { Action::Remove } else { Action::Extra };
    for item in &spec.items {
        let src = src_dir.join(&item.rel);
        let dst = dst_dir.join(&item.rel);
        let mut push = |action, rel: PathBuf| {
            plan.changes.push(Change { action, rel, group: item.group.clone(), rewrite: item.rewrite })
        };
        match kind(&src) {
            Kind::Missing => {
                if kind(&dst) != Kind::Missing {
                    push(extra_action, item.rel.clone());
                }
            }
            Kind::File | Kind::Link(_) => {
                if kind(&dst) == Kind::Missing {
                    push(Action::Add, item.rel.clone());
                } else if same(&src, &dst, item.rewrite, rw)? {
                    plan.unchanged += 1;
                } else {
                    push(Action::Update, item.rel.clone());
                }
            }
            Kind::Dir => {
                if kind(&dst) == Kind::Missing {
                    push(Action::Add, item.rel.clone());
                }
                let mut seen = BTreeSet::new();
                let walker = WalkDir::new(&src).min_depth(1).follow_links(false).sort_by_file_name();
                for entry in walker.into_iter().filter_entry(|e| !is_noise(&e.file_name().to_string_lossy())) {
                    let entry = entry.with_context(|| format!("walking {}", src.display()))?;
                    let rel_in = entry.path().strip_prefix(&src).expect("walk prefix").to_path_buf();
                    seen.insert(rel_in.clone());
                    let rel = item.rel.join(&rel_in);
                    let d = dst_dir.join(&rel);
                    if entry.file_type().is_dir() {
                        match kind(&d) {
                            Kind::Dir => {}
                            Kind::Missing => push(Action::Add, rel),
                            _ => push(Action::Update, rel),
                        }
                        continue;
                    }
                    if matches!(kind(&d), Kind::Missing) {
                        push(Action::Add, rel);
                    } else if same(entry.path(), &d, false, rw)? {
                        plan.unchanged += 1;
                    } else {
                        push(Action::Update, rel);
                    }
                }
                if kind(&dst) == Kind::Dir {
                    let mut walker = WalkDir::new(&dst).min_depth(1).follow_links(false).sort_by_file_name().into_iter();
                    while let Some(entry) = walker.next() {
                        let entry = entry.with_context(|| format!("walking {}", dst.display()))?;
                        let rel_in = entry.path().strip_prefix(&dst).expect("walk prefix").to_path_buf();
                        if seen.contains(&rel_in) {
                            continue;
                        }
                        if entry.file_type().is_dir() {
                            walker.skip_current_dir();
                        }
                        push(extra_action, item.rel.join(rel_in));
                    }
                } else if kind(&dst) != Kind::Missing {
                    // A file where a directory belongs: replace it.
                    push(Action::Remove, item.rel.clone());
                }
            }
        }
    }
    if spec.mcp {
        plan.mcp = plan_mcp(src_json, &dst_dir.join(".claude.json"), rw, prune)?;
    }
    Ok(plan)
}

fn read_json(path: &Path) -> Result<Option<Value>> {
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

fn plan_mcp(src_json: &Path, dst_json: &Path, rw: &Rewriter, prune: bool) -> Result<Option<McpChange>> {
    let src = read_json(src_json)?;
    let src_servers = match src.as_ref().and_then(|v| v.get("mcpServers")).and_then(Value::as_object) {
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
    if src_servers.is_empty() && (dst_servers.is_empty() || !prune) {
        return Ok(None);
    }
    let mut change = McpChange::default();
    let mut merged = if prune { Map::new() } else { dst_servers.clone() };
    for (k, v) in &src_servers {
        match dst_servers.get(k) {
            None => change.added.push(k.clone()),
            Some(old) if old != v => change.updated.push(k.clone()),
            _ => {}
        }
        merged.insert(k.clone(), v.clone());
    }
    if prune {
        change.removed = dst_servers.keys().filter(|k| !src_servers.contains_key(*k)).cloned().collect();
    }
    change.merged = Some(Value::Object(merged));
    Ok(Some(change))
}

/// Apply a plan. Shows a progress bar when `progress` is true.
pub fn apply(plan: &Plan, src_dir: &Path, dst_dir: &Path, rw: &Rewriter, progress: bool) -> Result<()> {
    let todo: Vec<&Change> = plan.changes.iter().filter(|c| c.action != Action::Extra).collect();
    let bar = if progress && !todo.is_empty() {
        let pb = ProgressBar::new(todo.len() as u64);
        pb.set_style(
            ProgressStyle::with_template("        {bar:30.cyan/blue} {pos}/{len} items {wide_msg:.dim}")
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
        match c.action {
            Action::Remove => fsutil::remove_any(&dst)?,
            Action::Add | Action::Update => copy_one(&src, &dst, c.rewrite, rw)?,
            Action::Extra => {}
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

fn copy_one(src: &Path, dst: &Path, rewrite: bool, rw: &Rewriter) -> Result<()> {
    if let Some(parent) = dst.parent() {
        // A file where a parent dir should be: replace it.
        if parent.is_file() || parent.is_symlink() && !parent.is_dir() {
            fsutil::remove_any(parent)?;
        }
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
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
                let bytes = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
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
            format!(r#"{{"hooks":"bash '{home}/.claude/hooks/h.sh'","s":"~/.claude/x","keep":"~/.claude.json","e":"$HOME/.claude"}}"#),
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
        let rw = Rewriter::new(&paths, &paths.profile_dir("w"));
        assert_eq!(rw.apply("/h/.claude/hooks/x.sh"), "/h/.mcc/profiles/w/hooks/x.sh");
        assert_eq!(rw.apply("~/.claude/skills"), "~/.mcc/profiles/w/skills");
        assert_eq!(rw.apply("$HOME/.claude"), "$HOME/.mcc/profiles/w");
        assert_eq!(rw.apply("${HOME}/.claude/a"), "${HOME}/.mcc/profiles/w/a");
        assert_eq!(rw.apply("\"/h/.claude\""), "\"/h/.mcc/profiles/w\"");
        assert_eq!(rw.apply("~/.claude.json"), "~/.claude.json");
        assert_eq!(rw.apply("~/.claude-other/x"), "~/.claude-other/x");
        assert_eq!(rw.apply("/other/.claude/x"), "/other/.claude/x");
    }

    #[test]
    fn clone_copies_whitelist_only() {
        let (_d, paths, dst) = setup();
        let rw = Rewriter::new(&paths, &dst);
        let spec = spec(None, &SyncConfig::default()).unwrap();
        let p = plan(&paths.default_dir, &paths.default_json, &dst, &spec, &rw, false).unwrap();
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
        assert!(settings.contains("~/.mcc/profiles/work/x"));
        assert!(settings.contains("\"keep\":\"~/.claude.json\""));
        assert!(settings.contains("$HOME/.mcc/profiles/work"));

        let json: Value = serde_json::from_str(&std::fs::read_to_string(dst.join(".claude.json")).unwrap()).unwrap();
        assert!(json.get("oauthAccount").is_none());
        assert!(json.get("userID").is_none());
        assert_eq!(json["mcpServers"]["fs"]["command"], "~/.mcc/profiles/work/bin/fs");

        // Second plan is a no-op.
        let p2 = plan(&paths.default_dir, &paths.default_json, &dst, &spec, &rw, false).unwrap();
        assert_eq!(p2.actionable(), 0, "{:?}", p2.changes);
    }

    #[test]
    fn prune_and_extras() {
        let (_d, paths, dst) = setup();
        let rw = Rewriter::new(&paths, &dst);
        let sp = spec(Some(&["agents".to_string()]), &SyncConfig::default()).unwrap();
        assert!(!sp.mcp);
        std::fs::create_dir_all(dst.join("agents/sub")).unwrap();
        std::fs::write(dst.join("agents/mine.md"), "x").unwrap();
        std::fs::write(dst.join("agents/sub/deep.md"), "x").unwrap();

        let p = plan(&paths.default_dir, &paths.default_json, &dst, &sp, &rw, false).unwrap();
        assert_eq!(p.count(Action::Add), 1);
        assert_eq!(p.count(Action::Extra), 2); // agents/mine.md, agents/sub (collapsed)
        apply(&p, &paths.default_dir, &dst, &rw, false).unwrap();
        assert!(dst.join("agents/mine.md").exists());

        let p = plan(&paths.default_dir, &paths.default_json, &dst, &sp, &rw, true).unwrap();
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
        std::os::unix::fs::symlink("../../.agents/skills/shared", src.join("skills/rel-out")).unwrap();
        std::os::unix::fs::symlink("one", src.join("skills/rel-in")).unwrap();
        std::fs::create_dir_all(src.join("skills/.trash/old")).unwrap();
        let rw = Rewriter::new(&paths, &dst);
        let sp = spec(Some(&["skills".to_string()]), &SyncConfig::default()).unwrap();
        let p = plan(src, &paths.default_json, &dst, &sp, &rw, false).unwrap();
        apply(&p, src, &dst, &rw, false).unwrap();
        assert_eq!(std::fs::read_link(dst.join("skills/abs-link")).unwrap(), dst.join("skills/one"));
        assert_eq!(std::fs::read_link(dst.join("skills/ext-link")).unwrap(), PathBuf::from("/opt/elsewhere"));
        let shared = std::fs::canonicalize(paths.home.join(".agents/skills/shared")).unwrap();
        assert_eq!(std::fs::canonicalize(dst.join("skills/rel-out")).unwrap(), shared);
        assert_eq!(std::fs::read_link(dst.join("skills/rel-in")).unwrap(), dst.join("skills/one"));
        assert!(!dst.join("skills/.trash").exists());
        let p2 = plan(src, &paths.default_json, &dst, &sp, &rw, false).unwrap();
        assert_eq!(p2.actionable(), 0);
    }

    #[test]
    fn unknown_group_is_usage_error() {
        let err = spec(Some(&["nope".to_string()]), &SyncConfig::default()).unwrap_err();
        assert_eq!(crate::error::classify(&err).0, crate::error::EXIT_USAGE);
    }
}
