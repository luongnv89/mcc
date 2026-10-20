//! `~/.mcc/config.toml`: detected environment, profile registry, sync settings,
//! and the profile each project starts with (`mcc project`, `mcc run`).
//!
//! The file is safe to edit by hand: saving merges the new settings into the
//! file as it is on disk, so comments and formatting survive, and a save that
//! changes nothing leaves the file alone. Commands that write it hold
//! [`lock`] from before they load it until they exit.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::env_detect::EnvInfo;
use crate::error::CliError;
use crate::paths::Paths;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub env: Option<EnvInfo>,
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Project directory (absolute, symlinks resolved) → the profile `mcc run`
    /// starts there first. Left out of the file while empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, Project>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Built-in sync groups to skip (e.g. `["plugins"]`).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Extra paths (relative to the config dir) to copy verbatim.
    #[serde(default)]
    pub extra: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageConfig {
    /// Show each profile's account email in `mcc usage` (hidden by default;
    /// `--show-email` / `--hide-email` override it for one run).
    #[serde(default)]
    pub show_email: bool,
    /// Look usage up live: read each profile's OAuth token and ask
    /// Anthropic's usage endpoint. Off by default, which reads only the
    /// rate limits `mcc statusline` caches. Left out of the file while off,
    /// so existing files stay as they are.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub live: bool,
    /// When the live-lookup disclosure was shown (RFC 3339); it is shown
    /// once. Left out of the file until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_disclosed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub dir: PathBuf,
    /// "clone" or "fresh".
    pub mode: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rc_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<String>,
    /// Share this profile's sessions (`projects/`) with the other sharing
    /// profiles. Off by default and left out of the file while off. Set it
    /// with `mcc share <alias>` (which also links the directory and prints
    /// the terms-of-service note); editing the key by hand does nothing alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub share_sessions: bool,
    /// When the full session-sharing note was first shown for this profile
    /// (RFC 3339). Later `mcc share` / `--mcc-share` runs print a one-line
    /// reminder instead. Left out of the file until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_note_acknowledged_at: Option<String>,
    /// The sharing group whose store this profile's `projects/` links to
    /// (`mcc share <alias> --group <name>`). Unset means the default group,
    /// whose store is the one every sharing profile used before groups
    /// existed (`~/.mcc/shared/projects`). Left out of the file while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_group: Option<String>,
    /// When sharing across organizations was confirmed for this profile
    /// (`mcc share <alias> --allow-cross-org`, RFC 3339). Cleared when the
    /// profile stops sharing. Left out of the file while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_cross_org_allowed_at: Option<String>,
    /// A short label for the account, such as "personal" or the work
    /// organization, asked by `mcc add` (or `--label`). Left out of the file
    /// while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The account `mcc add` saw after logging in, as `[profiles.ALIAS.account]`.
    /// Left out of the file until then (`--no-login`, or a profile added
    /// before mcc recorded it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
}

/// A Claude Code account, as its state file (`oauthAccount` in
/// `.claude.json`) or `claude auth status` describes it. Every field is
/// optional: older Claude Code versions report less, and a field that is
/// unknown is left out of the file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Claude Code's `accountUuid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_uuid: Option<String>,
    /// Claude Code's `organizationType`, such as `claude_max` (a personal
    /// plan) or `claude_team`. Personal plans have an organization too, so
    /// this is what tells them from a work organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_type: Option<String>,
    /// When mcc recorded it (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
}

impl Account {
    /// True when neither an email nor an account UUID is known, so the
    /// account can't be told apart from any other.
    pub fn is_anonymous(&self) -> bool {
        self.email.is_none() && self.uuid.is_none()
    }

    /// True when an organization (name or UUID) is known.
    pub fn has_org(&self) -> bool {
        self.organization.is_some() || self.organization_uuid.is_some()
    }

    /// What kind of account this is, for the session-sharing guard.
    pub fn kind(&self) -> AccountKind {
        if self.is_anonymous() && !self.has_org() {
            return AccountKind::Unknown;
        }
        if !self.has_org() {
            return AccountKind::Personal;
        }
        match self
            .organization_type
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("claude_free" | "claude_pro" | "claude_max" | "free" | "pro" | "max") => {
                AccountKind::Personal
            }
            Some("claude_team" | "claude_enterprise" | "team" | "enterprise") => {
                AccountKind::Organization
            }
            _ => AccountKind::Unclassified,
        }
    }

    /// How this account's organization compares with `other`'s: equal
    /// organization UUIDs when both are known, else equal names (ignoring
    /// case). Two accounts without any organization are both personal, so
    /// the same; one with and one without are different. Anything that
    /// can't be compared (a UUID on one side, only a name on the other) is
    /// [`OrgMatch::Unknown`].
    pub fn org_match(&self, other: &Account) -> OrgMatch {
        if let (Some(a), Some(b)) = (&self.organization_uuid, &other.organization_uuid) {
            return if a == b {
                OrgMatch::Same
            } else {
                OrgMatch::Different
            };
        }
        if let (Some(a), Some(b)) = (&self.organization, &other.organization) {
            return if a.eq_ignore_ascii_case(b) {
                OrgMatch::Same
            } else {
                OrgMatch::Different
            };
        }
        match (self.has_org(), other.has_org()) {
            (false, false) => OrgMatch::Same,
            (true, false) | (false, true) => OrgMatch::Different,
            (true, true) => OrgMatch::Unknown,
        }
    }

    /// The same login in the same organization: what makes two profiles
    /// share one set of usage limits. The same login in another
    /// organization (a personal plan and a work seat) is not a duplicate.
    pub fn same_seat(&self, other: &Account) -> bool {
        self.same_login(other) && self.org_match(other) != OrgMatch::Different
    }

    /// The same login: equal account UUIDs when both are known, else equal
    /// emails (ignoring case). Unknown never matches. The organization is
    /// not compared; see [`Account::same_seat`].
    pub fn same_login(&self, other: &Account) -> bool {
        match (&self.uuid, &other.uuid) {
            (Some(a), Some(b)) => a == b,
            _ => match (&self.email, &other.email) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => false,
            },
        }
    }

    /// Fill the fields this one lacks from `recorded` when it describes the
    /// same login, or when this one can't be told apart (no email, no UUID).
    pub fn or_recorded(self, recorded: Option<Account>) -> Account {
        match recorded {
            Some(r) if self.is_anonymous() || self.same_login(&r) => self.or(r),
            _ => self,
        }
    }

    /// Fill the fields this one lacks from `other`.
    pub fn or(self, other: Account) -> Account {
        Account {
            email: self.email.or(other.email),
            uuid: self.uuid.or(other.uuid),
            organization: self.organization.or(other.organization),
            organization_uuid: self.organization_uuid.or(other.organization_uuid),
            organization_type: self.organization_type.or(other.organization_type),
            recorded_at: self.recorded_at.or(other.recorded_at),
        }
    }
}

/// What kind of account a profile is logged in to ([`Account::kind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    /// A work organization: Team or Enterprise.
    Organization,
    /// A personal plan (Free, Pro, Max), or an account without any
    /// organization.
    Personal,
    /// In an organization whose type is unknown (not recorded, or a type
    /// mcc doesn't know): it may be a work organization.
    Unclassified,
    /// No account known: never logged in, or `--no-login`.
    Unknown,
}

/// How two accounts' organizations compare ([`Account::org_match`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgMatch {
    Same,
    Different,
    /// Not enough is known to tell.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// An mcc profile's name, or `default` for ~/.claude.
    pub profile: String,
    /// The other profiles `mcc run` may offer when `profile` is at its limit
    /// (`mcc project set --allow`), by name. Left out of the file while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

impl Project {
    /// A project that starts `profile` and also allows `allow`: each name
    /// once, in order, and never `profile` itself.
    pub fn new(profile: &str, allow: impl IntoIterator<Item = String>) -> Self {
        let mut list: Vec<String> = Vec::new();
        for name in allow {
            if name != profile && !list.contains(&name) {
                list.push(name);
            }
        }
        Self {
            profile: profile.to_string(),
            allow: list,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).map_err(|e| {
            CliError::input(format!(
                "{} is not valid mcc config: {}",
                path.display(),
                e.message()
            ))
            .hint("fix the file by hand, or move it away and run `mcc init --force`".to_string())
            .into()
        })
    }

    /// Write the config to `path`, merged into the file already there so
    /// its comments and formatting are kept. Nothing is written when the
    /// result is the file as it is.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let body = toml::to_string_pretty(self).context("serializing config")?;
        let current = match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        // A file that doesn't parse (or is empty) is written afresh.
        let merged = current
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
            .map(|mut doc| -> Result<String> {
                let new: toml_edit::DocumentMut =
                    body.parse().context("re-reading the serialized config")?;
                merge_table(doc.as_table_mut(), new.as_table());
                Ok(doc.to_string())
            })
            .transpose()?;
        let text = merged.unwrap_or_else(|| {
            format!("# mcc configuration — managed by `mcc`; safe to edit by hand.\n\n{body}")
        });
        if current.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        crate::fsutil::write_atomic(path, text.as_bytes())
    }

    /// Point the project defaults naming `old` at `new` (`mcc rename`).
    /// Returns how many changed.
    pub fn rename_project_defaults(&mut self, old: &str, new: &str) -> usize {
        let mut n = 0;
        for p in self.projects.values_mut().filter(|p| p.profile == old) {
            p.profile = new.to_string();
            n += 1;
        }
        n
    }

    /// Rename `old` to `new` in the projects' allow lists (`mcc rename`).
    /// Returns how many changed.
    pub fn rename_in_allow_lists(&mut self, old: &str, new: &str) -> usize {
        let mut n = 0;
        for p in self.projects.values_mut() {
            if p.allow.iter().any(|a| a == old) {
                let allow = std::mem::take(&mut p.allow)
                    .into_iter()
                    .map(|a| if a == old { new.to_string() } else { a });
                *p = Project::new(&p.profile, allow);
                n += 1;
            }
        }
        n
    }

    /// Remove `alias` from the projects' allow lists (`mcc delete`); the
    /// projects stay. Returns the directories of those that changed.
    pub fn remove_from_allow_lists(&mut self, alias: &str) -> Vec<String> {
        let mut changed = Vec::new();
        for (dir, p) in &mut self.projects {
            if p.allow.iter().any(|a| a == alias) {
                p.allow.retain(|a| a != alias);
                changed.push(dir.clone());
            }
        }
        changed
    }

    /// Remove the project defaults naming `alias` (`mcc delete`). Returns
    /// their project directories.
    pub fn remove_project_defaults(&mut self, alias: &str) -> Vec<String> {
        let gone: Vec<String> = self
            .projects
            .iter()
            .filter(|(_, p)| p.profile == alias)
            .map(|(dir, _)| dir.clone())
            .collect();
        for dir in &gone {
            self.projects.remove(dir);
        }
        gone
    }
}

/// Make `old` hold what `new` holds, changing only what differs: kept keys
/// keep their comments, formatting and order, removed keys go, new keys are
/// appended. A hand-written `key = false` that serde leaves out (a
/// `skip_serializing_if = "Not::not"` bool) is kept: it loads the same as
/// the key being absent, and the user may have written it on purpose.
fn merge_table(old: &mut toml_edit::Table, new: &toml_edit::Table) {
    use toml_edit::Item;
    old.retain(|k, v| {
        new.contains_key(k) || v.as_value().and_then(toml_edit::Value::as_bool) == Some(false)
    });
    for (key, new_item) in new.iter() {
        match (old.get_mut(key), new_item) {
            (Some(Item::Table(o)), Item::Table(n)) => merge_table(o, n),
            (Some(Item::Value(o)), Item::Value(n)) => merge_value(o, n.clone()),
            // A table written inline by hand stays inline.
            (Some(Item::Value(o @ toml_edit::Value::InlineTable(_))), Item::Table(n)) => {
                merge_value(
                    o,
                    toml_edit::Value::InlineTable(n.clone().into_inline_table()),
                )
            }
            (Some(slot), _) => *slot = unpositioned(new_item.clone()),
            (None, _) => {
                old.insert(key, unpositioned(new_item.clone()));
            }
        }
    }
}

/// Replace `old` with `new` unless they are equal, keeping `old`'s
/// surrounding whitespace and trailing comment.
fn merge_value(old: &mut toml_edit::Value, mut new: toml_edit::Value) {
    if same_value(old, &new) {
        return;
    }
    *new.decor_mut() = old.decor().clone();
    *old = new;
}

/// Equal as data, whatever the formatting.
fn same_value(a: &toml_edit::Value, b: &toml_edit::Value) -> bool {
    use toml_edit::Value as V;
    match (a, b) {
        (V::String(x), V::String(y)) => x.value() == y.value(),
        (V::Integer(x), V::Integer(y)) => x.value() == y.value(),
        (V::Float(x), V::Float(y)) => x.value().to_bits() == y.value().to_bits(),
        (V::Boolean(x), V::Boolean(y)) => x.value() == y.value(),
        (V::Datetime(x), V::Datetime(y)) => x.value() == y.value(),
        (V::Array(x), V::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| same_value(a, b))
        }
        (V::InlineTable(x), V::InlineTable(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_value(v, w)))
        }
        _ => false,
    }
}

/// `item` with its tables' positions cleared, so they print after the
/// tables they are appended to rather than where they were in the
/// serialized config.
fn unpositioned(mut item: toml_edit::Item) -> toml_edit::Item {
    fn clear(t: &mut toml_edit::Table) {
        t.set_position(None);
        for (_, item) in t.iter_mut() {
            if let toml_edit::Item::Table(t) = item {
                clear(t);
            }
        }
    }
    if let toml_edit::Item::Table(t) = &mut item {
        clear(t);
    }
    item
}

/// An exclusive advisory lock on `~/.mcc/.lock`, held until dropped (or the
/// process exits). Every command that writes the config takes it before
/// loading the config, so concurrent runs never lose each other's changes.
#[derive(Debug)]
pub struct ConfigLock {
    _file: File,
}

/// Take the config lock, waiting (and saying so) while another mcc command
/// holds it.
pub fn lock(paths: &Paths) -> Result<ConfigLock> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    let path = &paths.lock_file;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let flock = |op: libc::c_int| -> std::io::Result<()> {
        loop {
            // SAFETY: flock only reads the descriptor, which `file` keeps open.
            if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    };
    if let Err(e) = flock(libc::LOCK_EX | libc::LOCK_NB) {
        if e.kind() != std::io::ErrorKind::WouldBlock {
            return Err(e).with_context(|| format!("locking {}", path.display()));
        }
        crate::ui::info(&format!(
            "waiting for another mcc command to finish (it holds {})",
            paths.display(path)
        ));
        flock(libc::LOCK_EX).with_context(|| format!("locking {}", path.display()))?;
    }
    Ok(ConfigLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.sync.exclude = vec!["plugins".into()];
        cfg.usage.show_email = true;
        cfg.profiles.insert(
            "work".into(),
            Profile {
                dir: "/h/.mcc/profiles/work".into(),
                mode: "clone".into(),
                created_at: "2026-10-10T00:00:00+02:00".into(),
                wrapper: Some("/h/.local/bin/work".into()),
                rc_file: Some("/h/.zshrc".into()),
                last_sync: None,
                share_sessions: true,
                share_note_acknowledged_at: None,
                share_group: None,
                share_cross_org_allowed_at: None,
                label: None,
                account: None,
            },
        );
        cfg.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back.profiles, cfg.profiles);
        assert_eq!(back.sync.exclude, vec!["plugins".to_string()]);
        assert!(back.usage.show_email);
    }

    /// A config written before session sharing existed: loads with sharing
    /// off, and saving it again writes the same bytes (no new key).
    #[test]
    fn sharing_off_by_default_and_legacy_file_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let legacy = "# mcc configuration — managed by `mcc`; safe to edit by hand.\n\n\
            [sync]\nexclude = []\nextra = []\n\n\
            [usage]\nshow_email = false\n\n\
            [profiles.work]\ndir = \"/h/.mcc/profiles/work\"\nmode = \"clone\"\n\
            created_at = \"2026-10-10T00:00:00+02:00\"\nwrapper = \"/h/.local/bin/work\"\n\
            rc_file = \"/h/.zshrc\"\n";
        std::fs::write(&path, legacy).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert!(!cfg.profiles["work"].share_sessions);
        cfg.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);

        let mut cfg = cfg;
        cfg.profiles.get_mut("work").unwrap().share_sessions = true;
        cfg.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("share_sessions = true"), "{text}");
        assert!(Config::load(&path).unwrap().profiles["work"].share_sessions);
    }

    #[test]
    fn project_defaults_round_trip_and_follow_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        for (dir, profile) in [
            ("/src/a", "work"),
            ("/src/b", "default"),
            ("/src/c", "work"),
        ] {
            cfg.projects
                .insert(dir.into(), Project::new(profile, Vec::new()));
        }
        cfg.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[projects.\"/src/a\"]\nprofile = \"work\"\n"),
            "{text}"
        );
        let mut back = Config::load(&path).unwrap();
        assert_eq!(back.projects, cfg.projects);

        assert_eq!(back.rename_project_defaults("work", "job"), 2);
        assert_eq!(back.projects["/src/a"].profile, "job");
        assert_eq!(back.projects["/src/b"].profile, "default");
        assert_eq!(back.remove_project_defaults("job"), ["/src/a", "/src/c"]);
        assert_eq!(back.projects.keys().collect::<Vec<_>>(), ["/src/b"]);
        assert!(back.remove_project_defaults("nope").is_empty());
    }

    /// A config with project defaults written before allow lists existed:
    /// saving it again writes the same bytes (no `allow` key).
    #[test]
    fn legacy_project_defaults_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let legacy = "# mcc configuration — managed by `mcc`; safe to edit by hand.\n\n\
            [sync]\nexclude = []\nextra = []\n\n\
            [usage]\nshow_email = false\n\n\
            [profiles]\n\n\
            [projects.\"/src/a\"]\nprofile = \"work\"\n";
        std::fs::write(&path, legacy).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert!(cfg.projects["/src/a"].allow.is_empty());
        cfg.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);
    }

    #[test]
    fn allow_lists_round_trip_and_follow_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Each name once, never the project's own profile.
        let a = Project::new("work", names(&["lab", "work", "lab"]));
        assert_eq!(a.allow, ["lab"]);
        cfg.projects.insert("/src/a".into(), a);
        cfg.projects.insert(
            "/src/b".into(),
            Project::new("default", names(&["lab", "side"])),
        );
        cfg.projects
            .insert("/src/c".into(), Project::new("lab", Vec::new()));
        cfg.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[projects.\"/src/a\"]\nprofile = \"work\"\nallow = [\"lab\"]\n"),
            "{text}"
        );
        let mut back = Config::load(&path).unwrap();
        assert_eq!(back.projects, cfg.projects);

        // Renamed in every allow list; a project's own profile drops out.
        assert_eq!(back.rename_in_allow_lists("lab", "work"), 2);
        assert!(back.projects["/src/a"].allow.is_empty());
        assert_eq!(back.projects["/src/b"].allow, ["work", "side"]);
        assert_eq!(back.projects["/src/c"].profile, "lab");
        assert_eq!(back.rename_in_allow_lists("nope", "x"), 0);

        // Removed from the allow lists; the projects stay.
        assert_eq!(back.remove_from_allow_lists("work"), ["/src/b"]);
        assert_eq!(back.projects["/src/b"].allow, ["side"]);
        assert_eq!(back.projects.len(), 3);
        assert!(back.remove_from_allow_lists("nope").is_empty());
    }

    fn work_profile() -> Profile {
        Profile {
            dir: "/h/.mcc/profiles/work".into(),
            mode: "clone".into(),
            created_at: "2026-10-10T00:00:00+02:00".into(),
            wrapper: None,
            rc_file: None,
            last_sync: None,
            share_sessions: false,
            share_note_acknowledged_at: None,
            share_group: None,
            share_cross_org_allowed_at: None,
            label: None,
            account: None,
        }
    }

    /// Issue #35: a save changes only what differs and keeps the comments
    /// and formatting around it.
    #[test]
    fn save_keeps_comments_and_formatting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.profiles.insert("work".into(), work_profile());
        cfg.save(&path).unwrap();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("[sync]\n", "# sync settings\n[sync]\n")
            .replace("exclude = []", "exclude = [ ]   # nothing skipped")
            .replace("mode = \"clone\"", "mode = \"clone\"  # copied the default");
        std::fs::write(&path, &text).unwrap();

        let mut cfg = Config::load(&path).unwrap();
        cfg.profiles.get_mut("work").unwrap().mode = "fresh".into();
        let mut lab = work_profile();
        lab.dir = "/h/.mcc/profiles/lab".into();
        cfg.profiles.insert("lab".into(), lab);
        cfg.save(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# sync settings\n[sync]\n"), "{after}");
        assert!(
            after.contains("exclude = [ ]   # nothing skipped"),
            "{after}"
        );
        assert!(
            after.contains("mode = \"fresh\"  # copied the default"),
            "{after}"
        );
        // The new profile is appended after the existing one.
        assert!(after.find("[profiles.work]") < after.find("[profiles.lab]"));
        let back = Config::load(&path).unwrap();
        assert_eq!(back.profiles, cfg.profiles);

        // Removing a profile removes only its table.
        cfg.profiles.remove("lab");
        cfg.save(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("lab"), "{after}");
        assert!(after.contains("# sync settings\n"), "{after}");
    }

    /// A table written inline by hand stays inline.
    #[test]
    fn save_keeps_inline_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "usage = { show_email = false }\n").unwrap();
        let mut cfg = Config::load(&path).unwrap();
        cfg.usage.show_email = true;
        cfg.save(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.starts_with("usage = { show_email = true }\n"),
            "{after}"
        );
        assert!(Config::load(&path).unwrap().usage.show_email);
    }

    /// Issue #34: a hand-written `live = false` and its comment survive a
    /// save, while a `live = true` turned off is removed.
    #[test]
    fn save_keeps_hand_written_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[usage]\nshow_email = false\n# off for privacy\nlive = false\n",
        )
        .unwrap();
        let mut cfg = Config::load(&path).unwrap();
        cfg.usage.show_email = true;
        cfg.save(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("show_email = true\n"), "{after}");
        assert!(
            after.contains("# off for privacy\nlive = false\n"),
            "{after}"
        );
        assert!(!Config::load(&path).unwrap().usage.live);

        std::fs::write(&path, "[usage]\nshow_email = false\nlive = true\n").unwrap();
        let mut cfg = Config::load(&path).unwrap();
        assert!(cfg.usage.live);
        cfg.usage.live = false;
        cfg.save(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("live"), "{after}");
    }

    /// Issue #35: a save that changes nothing does not rewrite the file.
    #[test]
    fn save_without_changes_writes_nothing() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.profiles.insert("work".into(), work_profile());
        cfg.save(&path).unwrap();
        let ino = std::fs::metadata(&path).unwrap().ino();
        Config::load(&path).unwrap().save(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), ino);
    }

    /// Issue #39: the label and recorded account round-trip as
    /// `label = …` and a `[profiles.ALIAS.account]` table.
    #[test]
    fn label_and_account_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        let mut work = work_profile();
        work.label = Some("Acme (work)".into());
        work.account = Some(Account {
            email: Some("dev@acme.test".into()),
            uuid: None,
            organization: Some("Acme".into()),
            organization_uuid: Some("org-1".into()),
            organization_type: Some("claude_team".into()),
            recorded_at: Some("2026-10-20T00:00:00+02:00".into()),
        });
        cfg.profiles.insert("work".into(), work);
        cfg.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("label = \"Acme (work)\"\n"), "{text}");
        assert!(
            text.contains("[profiles.work.account]\nemail = \"dev@acme.test\"\n"),
            "{text}"
        );
        assert!(!text.contains("uuid = \"\""), "{text}");
        assert_eq!(Config::load(&path).unwrap().profiles, cfg.profiles);
    }

    #[test]
    fn same_login_prefers_uuids_then_emails() {
        let a = |email: Option<&str>, uuid: Option<&str>| Account {
            email: email.map(Into::into),
            uuid: uuid.map(Into::into),
            ..Account::default()
        };
        assert!(a(Some("A@x.com"), None).same_login(&a(Some("a@X.com"), None)));
        assert!(!a(Some("a@x.com"), None).same_login(&a(Some("b@x.com"), None)));
        // UUIDs decide when both are known, whatever the emails say.
        assert!(a(Some("a@x.com"), Some("u1")).same_login(&a(Some("b@x.com"), Some("u1"))));
        assert!(!a(Some("a@x.com"), Some("u1")).same_login(&a(Some("a@x.com"), Some("u2"))));
        assert!(a(Some("a@x.com"), Some("u1")).same_login(&a(Some("a@x.com"), None)));
        // Unknown never matches.
        assert!(!a(None, None).same_login(&a(None, None)));
        assert!(a(None, None).is_anonymous());
        assert!(!a(Some("a@x.com"), None).is_anonymous());

        let merged = Account {
            organization: Some("Acme".into()),
            ..a(Some("a@x.com"), None)
        }
        .or(Account {
            organization: Some("Other".into()),
            organization_uuid: Some("org-1".into()),
            ..a(Some("b@x.com"), Some("u1"))
        });
        assert_eq!(merged.email.as_deref(), Some("a@x.com"));
        assert_eq!(merged.uuid.as_deref(), Some("u1"));
        assert_eq!(merged.organization.as_deref(), Some("Acme"));
        assert_eq!(merged.organization_uuid.as_deref(), Some("org-1"));
    }

    #[test]
    fn org_match_prefers_uuids_then_names() {
        let a = |name: Option<&str>, id: Option<&str>| Account {
            email: Some("me@x.com".into()),
            organization: name.map(Into::into),
            organization_uuid: id.map(Into::into),
            ..Account::default()
        };
        use OrgMatch::*;
        assert_eq!(a(None, None).org_match(&a(None, None)), Same);
        assert_eq!(a(Some("Acme"), None).org_match(&a(None, None)), Different);
        assert_eq!(a(None, None).org_match(&a(None, Some("o1"))), Different);
        assert_eq!(
            a(Some("Acme"), None).org_match(&a(Some("ACME"), None)),
            Same
        );
        assert_eq!(
            a(Some("Acme"), None).org_match(&a(Some("Beta"), None)),
            Different
        );
        // UUIDs win over names.
        assert_eq!(
            a(Some("Acme"), Some("o1")).org_match(&a(Some("Acme"), Some("o2"))),
            Different
        );
        assert_eq!(
            a(Some("Old"), Some("o1")).org_match(&a(Some("New"), Some("o1"))),
            Same
        );
        // A UUID on one side and only a name on the other can't be compared.
        assert_eq!(
            a(None, Some("o1")).org_match(&a(Some("Acme"), None)),
            Unknown
        );
    }

    #[test]
    fn kind_tells_personal_plans_from_work_organizations() {
        let a = |org: Option<&str>, ty: Option<&str>| Account {
            email: Some("me@x.com".into()),
            organization_uuid: org.map(Into::into),
            organization_type: ty.map(Into::into),
            ..Account::default()
        };
        use AccountKind::*;
        assert_eq!(Account::default().kind(), Unknown);
        assert_eq!(a(None, None).kind(), Personal);
        // Personal plans have an organization of their own.
        assert_eq!(a(Some("o1"), Some("claude_max")).kind(), Personal);
        assert_eq!(a(Some("o1"), Some("claude_pro")).kind(), Personal);
        assert_eq!(a(Some("o1"), Some("claude_team")).kind(), Organization);
        assert_eq!(
            a(Some("o1"), Some("claude_enterprise")).kind(),
            Organization
        );
        // An organization of unknown type may be a work one.
        assert_eq!(a(Some("o1"), None).kind(), Unclassified);
        assert_eq!(a(Some("o1"), Some("something_new")).kind(), Unclassified);
    }

    #[test]
    fn same_seat_needs_the_same_login_and_organization() {
        let a = |email: &str, org: Option<&str>| Account {
            email: Some(email.into()),
            organization_uuid: org.map(Into::into),
            ..Account::default()
        };
        assert!(a("me@x.com", Some("o1")).same_seat(&a("ME@x.com", Some("o1"))));
        assert!(a("me@x.com", None).same_seat(&a("me@x.com", None)));
        // Personal plan and work seat on one login: not the same seat.
        assert!(!a("me@x.com", None).same_seat(&a("me@x.com", Some("o1"))));
        assert!(!a("me@x.com", Some("o1")).same_seat(&a("me@x.com", Some("o2"))));
        assert!(!a("me@x.com", Some("o1")).same_seat(&a("you@x.com", Some("o1"))));
    }

    #[test]
    fn email_hidden_unless_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[sync]\nexclude = []\n").unwrap();
        assert!(!Config::load(&path).unwrap().usage.show_email);
        std::fs::write(&path, "[usage]\nshow_email = true\n").unwrap();
        assert!(Config::load(&path).unwrap().usage.show_email);
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&dir.path().join("nope.toml")).unwrap();
        assert!(cfg.profiles.is_empty());
        assert!(cfg.env.is_none());
    }

    #[test]
    fn invalid_file_is_input_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "profiles = 3 = 4").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert_eq!(crate::error::classify(&err).0, crate::error::EXIT_INPUT);
    }
}
