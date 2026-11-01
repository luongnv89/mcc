//! Per-profile metrics for `mcc list` (and the account email for `mcc usage`).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde_json::Value;
use walkdir::WalkDir;

use crate::config::{Account, Profile};
use crate::fsutil;
use crate::paths::Paths;
use crate::shell;
use crate::usage::Usage;

#[derive(Debug, Clone, Serialize)]
pub struct ProfileMetrics {
    pub name: String,
    pub command: String,
    pub is_default: bool,
    pub dir: String,
    pub exists: bool,
    pub mode: Option<String>,
    pub account: Option<String>,
    /// The account's organization: from the profile's state file, else as
    /// `mcc add` recorded it.
    pub organization: Option<String>,
    pub organization_uuid: Option<String>,
    /// The profile's label (`mcc add`); always null for the default profile.
    pub label: Option<String>,
    /// The profile's color (`mcc add --color`); always null for the
    /// default profile.
    pub color: Option<String>,
    pub model: Option<String>,
    pub skills: usize,
    pub agents: usize,
    pub commands: usize,
    pub output_styles: usize,
    pub plugins_installed: usize,
    pub plugins_enabled: usize,
    pub mcp_servers: usize,
    pub hooks: usize,
    pub projects: usize,
    pub sessions: usize,
    pub size_bytes: u64,
    pub last_used: Option<String>,
    pub created_at: Option<String>,
    pub last_sync: Option<String>,
    /// Sessions are shared with the other sharing profiles (`mcc share`);
    /// always false for the default profile.
    pub share_sessions: bool,
    /// The sharing group (`mcc share --group`): `default` for the default
    /// group, null while the profile doesn't share.
    pub share_group: Option<String>,
    /// False for an mcc launcher `mcc refresh-launchers` would rewrite:
    /// written before session sharing, or without a block a setting turns
    /// on (the launch banner, auto sync); true when there is nothing to
    /// refresh, as for the default profile or a profile without a launcher.
    pub launcher_current: bool,
    /// Project directories where `mcc run` starts this profile (`mcc project
    /// set`); filled in by `mcc list` from the config, not by `collect`.
    pub project_defaults: Vec<String>,
    /// How the profile compares with the default (`mcc sync`); filled in by
    /// `mcc list`, and always null for the default profile.
    pub sync: Option<crate::copy::Status>,
    /// Auto sync (`[sync] auto`): whether launches catch the profile up
    /// with the default, and what the last check did; filled in by
    /// `mcc list`, and always null for the default profile.
    pub auto_sync: Option<crate::commands::launch_sync::Info>,
    /// 5-hour/weekly usage; filled in by `mcc list` (network), not by `collect`.
    pub usage: Usage,
    /// The config dir as Claude Code sees it (`CLAUDE_CONFIG_DIR`).
    #[serde(skip)]
    pub config_dir: PathBuf,
    #[serde(skip)]
    pub last_used_time: Option<SystemTime>,
    #[serde(skip)]
    pub warnings: Vec<String>,
}

/// Where Claude Code's state file keeps the logged-in account's email.
const ACCOUNT_EMAIL: &str = "/oauthAccount/emailAddress";

/// The account email in a Claude Code state file (`~/.claude.json`, or
/// `<profile>/.claude.json`); `None` when logged out or unreadable.
pub fn account_email(state_json: &Path) -> Option<String> {
    let state: Value = serde_json::from_slice(&std::fs::read(state_json).ok()?).ok()?;
    state.pointer(ACCOUNT_EMAIL)?.as_str().map(str::to_string)
}

/// The logged-in account in a Claude Code state file (its `oauthAccount`);
/// `None` when logged out or unreadable. `recorded_at` is left unset.
pub fn account_info(state_json: &Path) -> Option<Account> {
    let state: Value = serde_json::from_slice(&std::fs::read(state_json).ok()?).ok()?;
    account_from_state(&state)
}

/// A profile's account: what its state file says now, completed from (or,
/// when the state file has none, replaced by) what `mcc add` recorded.
pub fn profile_account(p: &Profile) -> Account {
    account_info(&p.dir.join(".claude.json"))
        .unwrap_or_default()
        .or_recorded(p.account.clone())
}

fn account_from_state(state: &Value) -> Option<Account> {
    let o = state.get("oauthAccount")?;
    let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
    let a = Account {
        email: s("emailAddress"),
        uuid: s("accountUuid"),
        organization: s("organizationName"),
        organization_uuid: s("organizationUuid"),
        organization_type: s("organizationType"),
        recorded_at: None,
    };
    (a != Account::default()).then_some(a)
}

/// `full`: also count skills, agents, commands, sessions… and measure the
/// size on disk, which walks the whole profile directory. Without it (the
/// bare `mcc` status view) those stay 0, so the view stays fast.
pub fn default_profile(paths: &Paths, full: bool) -> ProfileMetrics {
    let mut m = collect(
        "default",
        "claude",
        true,
        &paths.default_dir,
        &paths.default_json,
        None,
        paths,
    );
    if full {
        measure(&mut m, &paths.default_dir, Some(&paths.default_json));
    }
    m
}

/// `blocks` are the launcher blocks the settings turn on for the profile
/// ([`shell::Blocks::for_profile`]): a launcher without one of them is out
/// of date. `full` as for [`default_profile`].
pub fn mcc_profile(
    name: &str,
    p: &Profile,
    paths: &Paths,
    blocks: shell::Blocks,
    full: bool,
) -> ProfileMetrics {
    let mut m = collect(
        name,
        name,
        false,
        &p.dir,
        &p.dir.join(".claude.json"),
        Some(p),
        paths,
    );
    if full {
        measure(&mut m, &p.dir, None);
    }
    m.launcher_current = p
        .wrapper
        .as_deref()
        .is_none_or(|w| !shell::is_mcc_wrapper(w) || !shell::launcher_needs_refresh(w, blocks));
    m
}

fn collect(
    name: &str,
    command: &str,
    is_default: bool,
    dir: &Path,
    state_json: &Path,
    reg: Option<&Profile>,
    paths: &Paths,
) -> ProfileMetrics {
    let mut warnings = Vec::new();
    let exists = dir.is_dir();
    if !exists {
        warnings.push(format!("directory {} is missing", paths.display(dir)));
    }
    let mut read = |p: PathBuf| -> Option<Value> {
        let bytes = std::fs::read(&p).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(v) => Some(v),
            Err(_) => {
                warnings.push(format!("{} is not valid JSON", paths.display(&p)));
                None
            }
        }
    };
    let settings = read(dir.join("settings.json"));
    let state = read(state_json.to_path_buf());
    // A profile sharing the default's plugins has the default's installed.
    let plugins_root = match reg {
        Some(r) if crate::plugins::shared(r) => crate::plugins::shared_root(paths),
        _ => dir.join(crate::plugins::DIR),
    };
    let installed = read(plugins_root.join("installed_plugins.json"));

    let s = |v: &Option<Value>, ptr: &str| {
        v.as_ref()
            .and_then(|v| v.pointer(ptr))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let obj_len = |v: &Option<Value>, ptr: &str| {
        v.as_ref()
            .and_then(|v| v.pointer(ptr))
            .and_then(Value::as_object)
            .map_or(0, |o| o.len())
    };

    // What the state file says now, else what `mcc add` recorded.
    let account = state
        .as_ref()
        .and_then(account_from_state)
        .unwrap_or_default()
        .or_recorded(reg.and_then(|r| r.account.clone()));

    let last_used_time = [dir.join("history.jsonl"), state_json.to_path_buf()]
        .iter()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .max();

    ProfileMetrics {
        name: name.to_string(),
        command: command.to_string(),
        is_default,
        dir: paths.display(dir),
        exists,
        mode: reg.map(|r| r.mode.clone()),
        account: account.email,
        organization: account.organization,
        organization_uuid: account.organization_uuid,
        label: reg.and_then(|r| r.label.clone()),
        color: reg.and_then(|r| r.color.clone()),
        model: s(&settings, "/model"),
        // Set by `measure`: they walk the profile directory.
        skills: 0,
        agents: 0,
        commands: 0,
        output_styles: 0,
        plugins_installed: obj_len(&installed, "/plugins"),
        plugins_enabled: settings
            .as_ref()
            .and_then(|v| v.get("enabledPlugins"))
            .and_then(Value::as_object)
            .map_or(0, |o| {
                o.values().filter(|v| v.as_bool() == Some(true)).count()
            }),
        mcp_servers: obj_len(&state, "/mcpServers"),
        hooks: count_hooks(settings.as_ref()),
        projects: 0,
        sessions: 0,
        size_bytes: 0,
        last_used: last_used_time.map(|t| {
            chrono::DateTime::<chrono::Local>::from(t)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
        }),
        created_at: reg.map(|r| r.created_at.clone()),
        last_sync: reg.and_then(|r| r.last_sync.clone()),
        share_sessions: reg.is_some_and(|r| r.share_sessions),
        share_group: reg
            .filter(|r| r.share_sessions)
            .map(|r| r.share_group.clone().unwrap_or_else(|| "default".into())),
        // Set by `mcc_profile`, which knows which launcher blocks are on.
        launcher_current: true,
        project_defaults: Vec::new(),
        sync: None,
        auto_sync: None,
        usage: Usage::default(),
        config_dir: dir.to_path_buf(),
        last_used_time,
        warnings,
    }
}

/// The counts and the size on disk, which walk the profile directory `dir`.
/// `state_json`: the default's `~/.claude.json`, outside its directory,
/// counted in its size.
fn measure(m: &mut ProfileMetrics, dir: &Path, state_json: Option<&Path>) {
    m.skills = count_skills(&dir.join("skills"));
    m.agents = count_md(&dir.join("agents"));
    m.commands = count_md(&dir.join("commands"));
    m.output_styles = count_md(&dir.join("output-styles"));
    m.projects = count_entries(&dir.join("projects"), |p| p.is_dir());
    m.sessions = count_sessions(&dir.join("projects"));
    m.size_bytes = if m.exists { fsutil::dir_size(dir) } else { 0 }
        + state_json.map_or(0, |j| std::fs::metadata(j).map_or(0, |md| md.len()));
}

fn count_entries(dir: &Path, pred: impl Fn(&Path) -> bool) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .filter(|e| pred(&e.path()))
                .count()
        })
        .unwrap_or(0)
}

/// Skills are directories (or symlinks to them) under `skills/`.
fn count_skills(dir: &Path) -> usize {
    count_entries(dir, |p| p.is_dir())
}

/// Markdown files, recursively (agents and commands may be namespaced in subdirs).
fn count_md(dir: &Path) -> usize {
    WalkDir::new(dir)
        .follow_links(true)
        .max_depth(4)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "md"))
        .count()
}

fn count_sessions(dir: &Path) -> usize {
    WalkDir::new(dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl"))
        .count()
}

/// settings.json: `{"hooks": {"Event": [{"matcher": …, "hooks": [cmd, …]}]}}`.
fn count_hooks(settings: Option<&Value>) -> usize {
    let Some(events) = settings
        .and_then(|s| s.get("hooks"))
        .and_then(Value::as_object)
    else {
        return 0;
    };
    events
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .map(|m| {
            m.get("hooks")
                .and_then(Value::as_array)
                .map_or(1, |h| h.len())
        })
        .sum()
}

/// At most this many directory entries are looked at under `projects/` when
/// working out [`last_activity`], so a huge history stays quick to read.
const ACTIVITY_SCAN_LIMIT: usize = 20_000;

/// When the profile in `config_dir` was last used for a conversation: the
/// newest modification time of its prompt history (`history.jsonl`) and its
/// transcripts (`projects/**/*.jsonl`); `None` when it has neither.
///
/// Never `.claude.json` or `.credentials.json`, which change without the
/// account being used (a launch, a token refresh, `mcc` itself). A
/// `projects/` that is a symlink (session sharing) holds the whole group's
/// transcripts, so it is skipped and only the profile's own history counts.
pub fn last_activity(config_dir: &Path) -> Option<SystemTime> {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let history = mtime(&config_dir.join("history.jsonl"));
    let projects = config_dir.join("projects");
    let shared = std::fs::symlink_metadata(&projects).is_ok_and(|m| m.file_type().is_symlink());
    let transcripts = if shared {
        None
    } else {
        WalkDir::new(&projects)
            .follow_links(false)
            .max_depth(4)
            .into_iter()
            .take(ACTIVITY_SCAN_LIMIT)
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl")
            })
            .filter_map(|e| e.metadata().ok()?.modified().ok())
            .max()
    };
    history.into_iter().chain(transcripts).max()
}

/// "3m ago", "5h ago", "2d ago".
pub fn ago(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "never".into() };
    let secs = SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

pub fn ago_str(rfc3339: Option<&str>) -> String {
    match rfc3339.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) {
        Some(t) => ago(Some(SystemTime::from(t))),
        None => "never".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_from_the_state_file() {
        let state = serde_json::json!({"oauthAccount": {
            "emailAddress": "a@x.com", "accountUuid": "u1",
            "organizationName": "Acme", "organizationUuid": "org-1",
            "organizationRole": "admin", "organizationType": "claude_team"
        }});
        let a = account_from_state(&state).unwrap();
        assert_eq!(a.email.as_deref(), Some("a@x.com"));
        assert_eq!(a.uuid.as_deref(), Some("u1"));
        assert_eq!(a.organization.as_deref(), Some("Acme"));
        assert_eq!(a.organization_uuid.as_deref(), Some("org-1"));
        assert_eq!(a.organization_type.as_deref(), Some("claude_team"));
        assert_eq!(a.recorded_at, None);
        assert!(account_from_state(&serde_json::json!({"oauthAccount": {}})).is_none());
        assert!(account_from_state(&serde_json::json!({"theme": "dark"})).is_none());
    }

    #[test]
    fn counts_fixture() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let c = &paths.default_dir;
        for p in [
            "skills/a",
            "skills/b",
            "skills/.hidden",
            "agents/ns",
            "commands",
            "projects/p1/x",
            "projects/p2",
            "plugins",
        ] {
            std::fs::create_dir_all(c.join(p)).unwrap();
        }
        std::fs::write(c.join("skills/stray.txt"), "").unwrap();
        std::fs::write(c.join("agents/a.md"), "").unwrap();
        std::fs::write(c.join("agents/ns/b.md"), "").unwrap();
        std::fs::write(c.join("commands/c.md"), "").unwrap();
        std::fs::write(c.join("projects/p1/s1.jsonl"), "").unwrap();
        std::fs::write(c.join("projects/p1/s2.jsonl"), "").unwrap();
        std::fs::write(
            c.join("settings.json"),
            r#"{"model":"opus","enabledPlugins":{"a@m":true,"b@m":false},"hooks":{"Stop":[{"hooks":[{"type":"command"},{"type":"command"}]}],"PreToolUse":[{"matcher":"Bash","hooks":[{}]}]}}"#,
        )
        .unwrap();
        std::fs::write(
            c.join("plugins/installed_plugins.json"),
            r#"{"version":2,"plugins":{"a@m":[],"b@m":[]}}"#,
        )
        .unwrap();
        std::fs::write(
            &paths.default_json,
            r#"{"oauthAccount":{"emailAddress":"me@x.io"},"mcpServers":{"one":{},"two":{}}}"#,
        )
        .unwrap();

        let m = default_profile(&paths, true);
        assert_eq!((m.name.as_str(), m.command.as_str()), ("default", "claude"));
        assert_eq!(m.skills, 2);
        assert_eq!(m.agents, 2);
        assert_eq!(m.commands, 1);
        assert_eq!(m.plugins_installed, 2);
        assert_eq!(m.plugins_enabled, 1);
        assert_eq!(m.mcp_servers, 2);
        assert_eq!(m.hooks, 3);
        assert_eq!(m.projects, 2);
        assert_eq!(m.sessions, 2);
        assert_eq!(m.account.as_deref(), Some("me@x.io"));
        assert_eq!(m.model.as_deref(), Some("opus"));
        assert!(m.warnings.is_empty());
    }

    #[test]
    fn a_profile_sharing_plugins_counts_the_defaults_installed_plugins() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let root = paths.default_dir.join("plugins");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("installed_plugins.json"),
            r#"{"version":2,"plugins":{"a@m":[],"b@m":[],"c@m":[]}}"#,
        )
        .unwrap();
        let dir = paths.profile_dir("work");
        std::fs::create_dir_all(&dir).unwrap();
        let mut p: Profile = toml::from_str(&format!(
            "dir = {:?}\nmode = \"clone\"\ncreated_at = \"x\"\nlink = [\"plugins\"]\n",
            dir.to_str().unwrap()
        ))
        .unwrap();
        assert_eq!(
            mcc_profile("work", &p, &paths, shell::Blocks::default(), true).plugins_installed,
            3
        );
        // With its own plugins it counts its own (none here).
        p.link.clear();
        assert_eq!(
            mcc_profile("work", &p, &paths, shell::Blocks::default(), true).plugins_installed,
            0
        );
    }

    #[test]
    fn account_email_reads_the_state_file() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join(".claude.json");
        assert_eq!(account_email(&state), None);
        std::fs::write(&state, "{nope").unwrap();
        assert_eq!(account_email(&state), None);
        std::fs::write(&state, r#"{"oauthAccount":{}}"#).unwrap();
        assert_eq!(account_email(&state), None);
        std::fs::write(&state, r#"{"oauthAccount":{"emailAddress":"me@x.io"}}"#).unwrap();
        assert_eq!(account_email(&state).as_deref(), Some("me@x.io"));
    }

    #[test]
    fn missing_and_corrupt() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let m = default_profile(&paths, true);
        assert!(!m.exists);
        assert_eq!(m.warnings.len(), 1);
        std::fs::create_dir_all(&paths.default_dir).unwrap();
        std::fs::write(paths.default_dir.join("settings.json"), "{nope").unwrap();
        let m = default_profile(&paths, true);
        assert!(m.exists);
        assert!(m.warnings[0].contains("not valid JSON"));
    }

    /// Set `p`'s modification time to `secs_ago` seconds ago.
    fn touch_ago(p: &Path, secs_ago: u64) {
        let t = SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        std::fs::File::options()
            .append(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn last_activity_is_the_newest_history_or_transcript() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        assert_eq!(last_activity(dir), None);
        // Files that change without the account being used never count.
        for f in [".claude.json", ".credentials.json", "settings.json"] {
            std::fs::write(dir.join(f), "{}").unwrap();
        }
        std::fs::create_dir_all(dir.join("projects/-p")).unwrap();
        std::fs::write(dir.join("projects/-p/notes.txt"), "").unwrap();
        assert_eq!(last_activity(dir), None);

        std::fs::write(dir.join("history.jsonl"), "{}\n").unwrap();
        touch_ago(&dir.join("history.jsonl"), 3 * 86400);
        let history = std::fs::metadata(dir.join("history.jsonl"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(last_activity(dir), Some(history));
        // A newer transcript (subagent transcripts sit a level deeper) wins.
        let sub = dir.join("projects/-p/s1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a.jsonl"), "{}\n").unwrap();
        touch_ago(&sub.join("a.jsonl"), 3600);
        let transcript = std::fs::metadata(sub.join("a.jsonl"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(last_activity(dir), Some(transcript));
        // Touching .claude.json changes nothing.
        std::fs::write(dir.join(".claude.json"), r#"{"x":1}"#).unwrap();
        assert_eq!(last_activity(dir), Some(transcript));
    }

    #[test]
    fn last_activity_skips_shared_transcripts() {
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join("store");
        std::fs::create_dir_all(store.join("-p")).unwrap();
        std::fs::write(store.join("-p/other-profile.jsonl"), "{}\n").unwrap();
        let dir = d.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&store, dir.join("projects")).unwrap();
        // Only the group's transcripts: unknown.
        assert_eq!(last_activity(&dir), None);
        std::fs::write(dir.join("history.jsonl"), "{}\n").unwrap();
        touch_ago(&dir.join("history.jsonl"), 86400);
        let history = std::fs::metadata(dir.join("history.jsonl"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(last_activity(&dir), Some(history));
    }

    #[test]
    fn ago_formats() {
        let now = SystemTime::now();
        assert_eq!(ago(None), "never");
        assert_eq!(ago(Some(now)), "just now");
        assert_eq!(
            ago(Some(now - std::time::Duration::from_secs(7200))),
            "2h ago"
        );
        assert_eq!(
            ago(Some(now - std::time::Duration::from_secs(3 * 86400))),
            "3d ago"
        );
    }
}
