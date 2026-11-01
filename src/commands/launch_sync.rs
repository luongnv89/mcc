//! Auto sync (`[sync] auto`, on by default): when a profile starts, from its
//! launcher (the hidden `mcc launch-sync <alias>`) or `mcc run`, mcc first
//! catches it up with the default profile's setup, then the other profiles
//! the default changed for, while a small time budget lasts.
//!
//! Catching up is the safe part of `mcc sync` only
//! ([`sync::auto_catch_up`]): three-way against the profile's sync manifest,
//! never pruning or forcing, nothing written when the sync would remove
//! something or the profile has no usable manifest, local edits and
//! conflicts kept, and a backup first. Everything else stays with
//! `mcc sync`, which `mcc list` points at.
//!
//! It must never break or slow down a launch:
//! - a fingerprint of what a catch-up depends on (the default's sync set by
//!   size and mtime, its MCP servers, the sync settings, the profile's
//!   overlay, links and manifest) is kept per profile; while it is unchanged
//!   nothing is compared, locked or written;
//! - the config lock is only tried: a profile is skipped (and nothing is
//!   recorded) while another mcc command holds it;
//! - nothing is printed on stdout Claude Code writes to, nothing on a
//!   stream that isn't a terminal, and it never fails.
//!
//! What each check found is kept in `~/.mcc/state/<alias>/auto-sync.json`
//! ([`State`]); `mcc list --long` and `--json` show it.

use std::io::{IsTerminal, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::sync::{self, AutoOutcome};
use crate::config::{self, AutoSync, Config, Profile};
use crate::copy::{self, Spec};
use crate::paths::Paths;
use crate::{fsutil, log, ui};

/// How long a launch may spend on profiles other than the one starting.
/// The starting profile is always checked; the others wait for a later
/// launch once this is used up.
const BUDGET: Duration = Duration::from_millis(500);

/// Set to anything but "" / "0", it turns auto sync off for that run: what
/// `mcc doctor` sets when it runs a launcher, which must not write anything.
pub const ENV_OFF: &str = "MCC_NO_AUTO_SYNC";

fn disabled_by_env() -> bool {
    std::env::var_os(ENV_OFF).is_some_and(|v| !v.is_empty() && v != "0")
}

/// What the last check of a profile found, as kept in its state file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// What the last completed check depended on ([`fingerprint`]); unset
    /// after a failed one, so the next launch tries again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// When the profile was last compared with the default (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<String>,
    /// When a check last copied something into it (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_applied: Option<String>,
    /// What that copied: profile-relative paths and `MCP server "NAME"`.
    #[serde(default)]
    pub changed: Vec<String>,
    /// Why the last check wrote nothing ([`sync::Held::as_str`]), or
    /// `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// The error of the last check, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Local edits and conflicts the last check kept.
    #[serde(default)]
    pub local_edits: usize,
    #[serde(default)]
    pub conflicts: usize,
}

/// The state file of `alias`; empty when there is none (or it can't be read).
pub fn load_state(paths: &Paths, alias: &str) -> State {
    std::fs::read(paths.auto_sync_state(alias))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_state(paths: &Paths, alias: &str, state: &State) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(state)?;
    bytes.push(b'\n');
    fsutil::write_atomic(&paths.auto_sync_state(alias), &bytes)
}

/// Auto sync as `mcc list` shows it (null for the default profile).
#[derive(Debug, Clone, Serialize)]
pub struct Info {
    /// Whether launches catch this profile up with the default.
    pub enabled: bool,
    /// The setting that turned it off: `sync.auto` or
    /// `profiles.<alias>.auto_sync`; null while on.
    pub off_by: Option<String>,
    pub last_checked: Option<String>,
    pub last_applied: Option<String>,
    /// What the last catch-up copied.
    pub changed: Vec<String>,
    /// Why the last check wrote nothing (`no_manifest`, `invalid_manifest`,
    /// `removals`, `error`); null otherwise.
    pub held: Option<String>,
}

impl Info {
    pub fn of(paths: &Paths, cfg: &Config, alias: &str) -> Info {
        let state = load_state(paths, alias);
        let setting = cfg.auto_sync(alias);
        Info {
            enabled: setting.is_on(),
            off_by: match setting {
                AutoSync::On => None,
                AutoSync::OffForAll => Some("sync.auto".into()),
                AutoSync::OffForProfile => Some(format!("profiles.{alias}.auto_sync")),
            },
            last_checked: state.last_checked,
            last_applied: state.last_applied,
            changed: state.changed,
            held: state.held,
        }
    }
}

/// What a launch did for one profile.
#[derive(Debug)]
enum Outcome {
    /// Unchanged since the last check: nothing done.
    Current,
    /// Another mcc command holds the config lock: nothing done or recorded.
    Busy,
    Done(AutoOutcome),
    Failed(String),
}

/// `mcc launch-sync <alias>`, run by a launcher: the catch-up, then one
/// summary line on stdout (the launcher sends it to its stderr) when it is
/// a terminal and something was copied. Never fails, never prints
/// otherwise.
pub fn run(alias: &str) {
    let Ok(paths) = Paths::from_env() else { return };
    log::init_if_present(paths.log_file.clone());
    let results = catch_up(&paths, alias);
    if let Some(line) = summary(&results)
        && std::io::stdout().is_terminal()
    {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// For `mcc run`: the catch-up before claude starts as `name`, with the
/// summary on stderr when it is a terminal and `--quiet` is not set. Never
/// fails and never prints on stdout.
pub fn on_launch(paths: &Paths, name: &str) {
    let results = std::panic::catch_unwind(|| catch_up(paths, name)).unwrap_or_default();
    if let Some(line) = summary(&results)
        && !ui::settings().quiet
        && std::io::stderr().is_terminal()
    {
        eprintln!("{line}");
    }
}

/// Catch the starting profile `launched` up, then the other profiles while
/// [`BUDGET`] lasts. Nothing happens with `[sync] auto = false`, nor when
/// `launched` has turned it off (its launcher then has no catch-up block),
/// nor with [`ENV_OFF`] set. `launched` may name the default profile, which
/// only the others follow.
fn catch_up(paths: &Paths, launched: &str) -> Vec<(String, Outcome)> {
    let start = Instant::now();
    if disabled_by_env() {
        return Vec::new();
    }
    let Ok(cfg) = Config::load(&paths.config_file) else {
        return Vec::new();
    };
    if !cfg.auto_sync(launched).is_on() || !paths.default_dir.is_dir() {
        return Vec::new();
    }
    let Ok(spec) = copy::spec(None, &cfg.sync) else {
        return Vec::new();
    };
    let base = default_fingerprint(paths, &spec);
    let mut order: Vec<&String> = cfg.profiles.keys().filter(|a| *a == launched).collect();
    order.extend(cfg.profiles.keys().filter(|a| *a != launched));
    let mut results = Vec::new();
    for alias in order {
        if alias != launched && start.elapsed() > BUDGET {
            log::write(
                "INFO",
                "auto sync: time budget used up; the other profiles are checked on a later launch",
            );
            break;
        }
        let p = &cfg.profiles[alias];
        if !cfg.auto_sync(alias).is_on() || !p.dir.is_dir() {
            continue;
        }
        let outcome = one(paths, &cfg, alias, &base);
        if !matches!(outcome, Outcome::Current) {
            log::write(
                "INFO",
                &format!("auto sync: {alias}: {}", describe(&outcome)),
            );
        }
        results.push((alias.clone(), outcome));
    }
    results
}

/// Check one profile: nothing while its fingerprint is the one recorded,
/// else under the config lock (skipped while another command holds it),
/// the catch-up and its record.
fn one(paths: &Paths, cfg: &Config, alias: &str, base: &str) -> Outcome {
    let fp = fingerprint(cfg, &cfg.profiles[alias], base);
    if load_state(paths, alias).fingerprint.as_deref() == Some(fp.as_str()) {
        return Outcome::Current;
    }
    let _lock = match config::try_lock(paths) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Outcome::Busy,
        Err(e) => return Outcome::Failed(format!("{e:#}")),
    };
    // Under the lock: the config as it is now (another command may have
    // changed it), and the record another launch may have just written.
    let Ok(mut cfg) = Config::load(&paths.config_file) else {
        return Outcome::Failed("the config can't be read".into());
    };
    let Some(p) = cfg.profiles.get(alias) else {
        return Outcome::Current;
    };
    if !cfg.auto_sync(alias).is_on() || !p.dir.is_dir() {
        return Outcome::Current;
    }
    let fp = fingerprint(&cfg, p, base);
    let previous = load_state(paths, alias);
    if previous.fingerprint.as_deref() == Some(fp.as_str()) {
        return Outcome::Current;
    }
    let now = || chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
    let mut state = State {
        last_checked: Some(now()),
        last_applied: previous.last_applied.clone(),
        changed: previous.changed.clone(),
        ..State::default()
    };
    let outcome = match sync::auto_catch_up(paths, &mut cfg, alias) {
        Ok(outcome) => {
            match &outcome {
                AutoOutcome::Applied { changed, status } => {
                    state.last_applied = state.last_checked.clone();
                    state.changed = changed.clone();
                    (state.local_edits, state.conflicts) = (status.local_edits, status.conflicts);
                }
                AutoOutcome::UpToDate { status } => {
                    (state.local_edits, state.conflicts) = (status.local_edits, status.conflicts);
                }
                AutoOutcome::Held { reason } => state.held = Some(reason.as_str().into()),
            }
            // What the profile depends on now: the catch-up rewrote its
            // manifest (and maybe the config).
            state.fingerprint = cfg.profiles.get(alias).map(|p| fingerprint(&cfg, p, base));
            Outcome::Done(outcome)
        }
        Err(e) => {
            state.held = Some("error".into());
            state.error = Some(format!("{e:#}"));
            Outcome::Failed(format!("{e:#}"))
        }
    };
    if let Err(e) = save_state(paths, alias, &state) {
        log::write(
            "WARN",
            &format!("auto sync: {alias}: recording the check failed: {e:#}"),
        );
    }
    outcome
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Current => "unchanged since the last check".into(),
        Outcome::Busy => "skipped: another mcc command holds the config lock".into(),
        Outcome::Done(AutoOutcome::Applied { changed, status }) => format!(
            "copied {} from the default ({}); kept {} local edit(s), {} conflict(s)",
            changed.len(),
            changed.join(", "),
            status.local_edits,
            status.conflicts
        ),
        Outcome::Done(AutoOutcome::UpToDate { .. }) => "already up to date".into(),
        Outcome::Done(AutoOutcome::Held { reason }) => format!(
            "nothing written: {} — `mcc sync` handles it",
            reason.describe()
        ),
        Outcome::Failed(e) => format!("failed: {e}"),
    }
}

/// One line for the terminal when a catch-up copied something, e.g.
/// `↻ auto sync: work 3 updated from the default; lab 1 updated, 1 conflict
/// kept (see `mcc sync lab`)`; `None` otherwise.
fn summary(results: &[(String, Outcome)]) -> Option<String> {
    let parts: Vec<String> = results
        .iter()
        .filter_map(|(alias, o)| match o {
            Outcome::Done(AutoOutcome::Applied { changed, status }) => {
                let mut part = format!("{alias} {} updated from the default", changed.len());
                if status.conflicts > 0 {
                    part.push_str(&format!(
                        ", {} conflict{} kept (see `mcc sync {alias}`)",
                        status.conflicts,
                        if status.conflicts == 1 { "" } else { "s" }
                    ));
                }
                Some(part)
            }
            _ => None,
        })
        .collect();
    (!parts.is_empty()).then(|| format!("↻ auto sync: {}", parts.join("; ")))
}

/// What a catch-up of `p` depends on: `base` ([`default_fingerprint`]),
/// the sync settings, and the profile's own inputs — its links, its
/// settings overlay (in the config and `settings.mcc.json`) and its sync
/// manifest, by size and mtime.
fn fingerprint(cfg: &Config, p: &Profile, base: &str) -> String {
    let mut h = Sha256::new();
    h.update(base.as_bytes());
    h.update(format!("{:?}|{:?}", cfg.sync.exclude, cfg.sync.extra).as_bytes());
    h.update(p.dir.as_os_str().as_encoded_bytes());
    h.update(format!("|{:?}|{:?}|", p.link, p.settings).as_bytes());
    for name in [copy::OVERLAY_FILE, crate::manifest::FILE] {
        stat(&mut h, &p.dir.join(name), Path::new(name));
    }
    hex(h)
}

/// The default's side of [`fingerprint`]: every path of its sync set by
/// type, size, mtime and inode (symlinks by target), and its MCP servers,
/// plus the mcc version (a newer mcc may sync differently). Cheap: nothing
/// is read but `.claude.json`'s `mcpServers`, which changes far less often
/// than the rest of that file.
fn default_fingerprint(paths: &Paths, spec: &Spec) -> String {
    let mut h = Sha256::new();
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    for item in &spec.items {
        let root = paths.default_dir.join(&item.rel);
        if std::fs::symlink_metadata(&root).is_err() {
            h.update(b"\0missing\0");
            h.update(item.rel.as_os_str().as_encoded_bytes());
            continue;
        }
        for entry in WalkDir::new(&root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
        {
            let rel = entry
                .path()
                .strip_prefix(&paths.default_dir)
                .unwrap_or(entry.path());
            stat(&mut h, entry.path(), rel);
        }
    }
    if spec.mcp {
        #[derive(Deserialize)]
        struct Servers {
            #[serde(rename = "mcpServers")]
            servers: Option<serde_json::Value>,
        }
        match std::fs::read(&paths.default_json)
            .ok()
            .and_then(|b| serde_json::from_slice::<Servers>(&b).ok())
        {
            Some(s) => h.update(serde_json::to_vec(&s.servers).unwrap_or_default()),
            None => h.update(b"\0no mcp servers\0"),
        }
    }
    hex(h)
}

/// Add `path` (shown as `rel`) to `h` by type, size, mtime and inode; a
/// symlink by its target; a missing path as missing.
fn stat(h: &mut Sha256, path: &Path, rel: &Path) {
    h.update(b"\0");
    h.update(rel.as_os_str().as_encoded_bytes());
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            let target = std::fs::read_link(path).unwrap_or_default();
            h.update(b"\0link\0");
            h.update(target.as_os_str().as_encoded_bytes());
        }
        Ok(m) => h.update(
            format!(
                "\0{}|{}|{}.{}|{}",
                if m.is_dir() { "d" } else { "f" },
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ino()
            )
            .as_bytes(),
        ),
        Err(_) => h.update(b"\0missing"),
    }
}

fn hex(h: Sha256) -> String {
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copy::Status;

    #[test]
    fn the_summary_names_what_was_copied_and_the_conflicts_kept() {
        let applied = |n: usize, conflicts| {
            Outcome::Done(AutoOutcome::Applied {
                changed: (0..n).map(|i| format!("f{i}")).collect(),
                status: Status {
                    behind: 0,
                    local_edits: 2,
                    conflicts,
                },
            })
        };
        let results = vec![
            ("work".to_string(), applied(3, 0)),
            ("lab".to_string(), applied(1, 1)),
            ("side".to_string(), Outcome::Busy),
            ("old".to_string(), Outcome::Current),
        ];
        assert_eq!(
            summary(&results).unwrap(),
            "↻ auto sync: work 3 updated from the default; lab 1 updated from the default, 1 conflict kept (see `mcc sync lab`)"
        );
        // Nothing copied: nothing to say.
        let quiet = vec![
            ("work".to_string(), Outcome::Current),
            (
                "lab".to_string(),
                Outcome::Done(AutoOutcome::UpToDate {
                    status: Status::default(),
                }),
            ),
            (
                "fresh".to_string(),
                Outcome::Done(AutoOutcome::Held {
                    reason: sync::Held::NoManifest,
                }),
            ),
        ];
        assert_eq!(summary(&quiet), None);
    }

    #[test]
    fn the_fingerprint_follows_the_default_and_the_profiles_inputs() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        std::fs::create_dir_all(paths.default_dir.join("skills/a")).unwrap();
        std::fs::write(paths.default_dir.join("skills/a/SKILL.md"), "a").unwrap();
        std::fs::write(
            &paths.default_json,
            r#"{"mcpServers":{"fs":{"command":"x"}},"numStartups":1}"#,
        )
        .unwrap();
        let dir = paths.profile_dir("work");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg: Config = toml::from_str(&format!(
            "[profiles.work]\ndir = \"{}\"\nmode = \"clone\"\ncreated_at = \"t\"\n",
            dir.display()
        ))
        .unwrap();
        let spec = copy::spec(None, &cfg.sync).unwrap();
        let p = &cfg.profiles["work"];
        let fp = || fingerprint(&cfg, p, &default_fingerprint(&paths, &spec));
        let first = fp();
        assert_eq!(fp(), first, "stable while nothing changes");

        // The rest of ~/.claude.json changes on every session: ignored.
        std::fs::write(
            &paths.default_json,
            r#"{"mcpServers":{"fs":{"command":"x"}},"numStartups":2}"#,
        )
        .unwrap();
        assert_eq!(fp(), first);
        // Its MCP servers count.
        std::fs::write(&paths.default_json, r#"{"mcpServers":{}}"#).unwrap();
        let no_mcp = fp();
        assert_ne!(no_mcp, first);
        // So do the default's files...
        std::fs::write(paths.default_dir.join("skills/a/SKILL.md"), "a v2").unwrap();
        let edited = fp();
        assert_ne!(edited, no_mcp);
        // ...and the profile's manifest (a `mcc sync` rewrote it).
        std::fs::write(dir.join(crate::manifest::FILE), "{}").unwrap();
        assert_ne!(fp(), edited);
    }
}
