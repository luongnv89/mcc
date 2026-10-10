//! Per-profile metrics for `mcc list`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde_json::Value;
use walkdir::WalkDir;

use crate::config::Profile;
use crate::fsutil;
use crate::paths::Paths;
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
    pub organization: Option<String>,
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

pub fn default_profile(paths: &Paths) -> ProfileMetrics {
    collect(
        "default",
        "claude",
        true,
        &paths.default_dir,
        &paths.default_json,
        None,
        paths,
    )
}

pub fn mcc_profile(name: &str, p: &Profile, paths: &Paths) -> ProfileMetrics {
    collect(
        name,
        name,
        false,
        &p.dir,
        &p.dir.join(".claude.json"),
        Some(p),
        paths,
    )
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
    let installed = read(dir.join("plugins/installed_plugins.json"));

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
        account: s(&state, "/oauthAccount/emailAddress"),
        organization: s(&state, "/oauthAccount/organizationName"),
        model: s(&settings, "/model"),
        skills: count_skills(&dir.join("skills")),
        agents: count_md(&dir.join("agents")),
        commands: count_md(&dir.join("commands")),
        output_styles: count_md(&dir.join("output-styles")),
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
        projects: count_entries(&dir.join("projects"), |p| p.is_dir()),
        sessions: count_sessions(&dir.join("projects")),
        size_bytes: if exists { fsutil::dir_size(dir) } else { 0 }
            + if is_default {
                std::fs::metadata(state_json).map_or(0, |m| m.len())
            } else {
                0
            },
        last_used: last_used_time.map(|t| {
            chrono::DateTime::<chrono::Local>::from(t)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
        }),
        created_at: reg.map(|r| r.created_at.clone()),
        last_sync: reg.and_then(|r| r.last_sync.clone()),
        usage: Usage::default(),
        config_dir: dir.to_path_buf(),
        last_used_time,
        warnings,
    }
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

        let m = default_profile(&paths);
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
    fn missing_and_corrupt() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let m = default_profile(&paths);
        assert!(!m.exists);
        assert_eq!(m.warnings.len(), 1);
        std::fs::create_dir_all(&paths.default_dir).unwrap();
        std::fs::write(paths.default_dir.join("settings.json"), "{nope").unwrap();
        let m = default_profile(&paths);
        assert!(m.exists);
        assert!(m.warnings[0].contains("not valid JSON"));
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
