//! `mcc statusline`: install/uninstall in settings.json and the shim that
//! caches each profile's rate limits (issue #33).

mod common;

use std::fs::File;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use common::{Sandbox, chmod_x, json, with_profile};
use serde_json::{Value, json};

/// The previous statusLine command: records its stdin and CLAUDE_CONFIG_DIR
/// next to itself and prints one line.
const ORIGINAL: &str = r#"#!/bin/sh
cat > "$HOME/statusline.stdin"
echo "${CLAUDE_CONFIG_DIR:-unset}" > "$HOME/statusline.env"
echo "Opus 5 | ctx 12%"
"#;

const STATUS: &str = r#"{"model":{"id":"claude-opus-5"},"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1792500000},"seven_day":{"used_percentage":41.2,"resets_at":1792900000}}}"#;

fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.write(".claude/statusline.sh", ORIGINAL);
    chmod_x(&sb.path(".claude/statusline.sh"));
    with_profile(&sb, "work", "--clone");
    sb
}

fn settings(sb: &Sandbox, rel: &str) -> Value {
    json(sb.read(rel).as_bytes())
}

fn status_line(sb: &Sandbox, rel: &str) -> Value {
    settings(sb, rel)["statusLine"].clone()
}

fn backups(sb: &Sandbox) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(sb.path(".mcc/backups"))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("settings.json."))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn install(sb: &Sandbox, extra: &[&str]) -> String {
    let out = sb
        .mcc()
        .args(["statusline", "install"])
        .args(extra)
        .assert()
        .success()
        .get_output()
        .clone();
    String::from_utf8(out.stderr).unwrap()
}

/// Run the shim like Claude Code does: `statusLine.command` through `sh -c`,
/// the status JSON on stdin, CLAUDE_CONFIG_DIR as the session has it.
fn render(sb: &Sandbox, settings_rel: &str, config_dir: Option<&str>, input: &str) -> String {
    let cmd = status_line(sb, settings_rel)["command"]
        .as_str()
        .unwrap()
        .to_string();
    let base = sb.mcc();
    let mut c = std::process::Command::new("sh");
    c.env_clear()
        .envs(base.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))))
        .current_dir(sb.home())
        .args(["-c", &cmd]);
    if let Some(dir) = config_dir {
        c.env("CLAUDE_CONFIG_DIR", sb.path(dir));
    }
    let out = assert_cmd::Command::from_std(c)
        .write_stdin(input)
        .timeout(Duration::from_secs(20))
        .assert()
        .success()
        .get_output()
        .clone();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn install_wraps_once_and_uninstall_restores() {
    let sb = sandbox();
    let before = sb.read(".claude/settings.json");
    let original = status_line(&sb, ".claude/settings.json");

    let err = install(&sb, &[]);
    assert!(
        err.contains("set statusLine in ~/.claude/settings.json"),
        "{err}"
    );
    let sl = status_line(&sb, ".claude/settings.json");
    let cmd = sl["command"].as_str().unwrap();
    assert!(cmd.ends_with("mcc statusline"), "{cmd}");
    assert_eq!(sl["type"], "command");
    // The rest of the settings is untouched, in its order.
    let s = settings(&sb, ".claude/settings.json");
    let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["model", "enabledPlugins", "hooks", "statusLine"]);
    // Backed up first, byte for byte.
    let b = backups(&sb);
    assert_eq!(b.len(), 1, "{b:?}");
    assert!(b[0].starts_with("settings.json.default."), "{b:?}");
    assert_eq!(sb.read(&format!(".mcc/backups/{}", b[0])), before);
    let rec = json(sb.read(".mcc/state/default/statusline.json").as_bytes());
    assert_eq!(rec["original"], original);

    // Again: no double wrap, no new backup, the record keeps the original.
    let err = install(&sb, &[]);
    assert!(err.contains("already installed"), "{err}");
    assert_eq!(status_line(&sb, ".claude/settings.json")["command"], cmd);
    assert_eq!(backups(&sb).len(), 1);
    let rec = json(sb.read(".mcc/state/default/statusline.json").as_bytes());
    assert_eq!(rec["original"], original);

    // --prefix changes the command only; the original is still recorded.
    let err = install(&sb, &["--prefix"]);
    assert!(err.contains("updated the statusline"), "{err}");
    let prefixed = status_line(&sb, ".claude/settings.json");
    assert!(
        prefixed["command"]
            .as_str()
            .unwrap()
            .ends_with("statusline --prefix")
    );
    let rec = json(sb.read(".mcc/state/default/statusline.json").as_bytes());
    assert_eq!(rec["original"], original);

    let out = sb
        .mcc()
        .args(["statusline", "uninstall"])
        .assert()
        .success()
        .get_output()
        .clone();
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("restored the previous statusLine"), "{err}");
    assert_eq!(status_line(&sb, ".claude/settings.json"), original);
    assert_eq!(
        settings(&sb, ".claude/settings.json"),
        json(before.as_bytes())
    );
    assert!(!sb.exists(".mcc/state/default/statusline.json"));

    // Nothing left to undo.
    let out = sb
        .mcc()
        .args(["statusline", "uninstall"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing to do"));
}

#[test]
fn uninstall_removes_a_statusline_that_did_not_exist() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.write(".claude/settings.json", r#"{"model":"opus"}"#);
    install(&sb, &[]);
    assert!(status_line(&sb, ".claude/settings.json")["command"].is_string());
    sb.mcc()
        .args(["statusline", "uninstall"])
        .assert()
        .success();
    assert_eq!(
        settings(&sb, ".claude/settings.json"),
        json!({"model": "opus"})
    );
}

#[test]
fn install_creates_missing_settings() {
    let sb = Sandbox::new();
    sb.seed_default();
    std::fs::remove_file(sb.path(".claude/settings.json")).unwrap();
    install(&sb, &[]);
    assert!(status_line(&sb, ".claude/settings.json")["command"].is_string());
    assert!(backups(&sb).is_empty());
}

#[test]
fn shim_caches_rate_limits_and_chains_the_original() {
    let sb = sandbox();
    install(&sb, &[]);
    let config = sb.read(".mcc/config.toml");
    let log = sb.read(".mcc/logs/mcc.log");

    // The default profile: CLAUDE_CONFIG_DIR unset.
    let out = render(&sb, ".claude/settings.json", None, STATUS);
    assert_eq!(out, "Opus 5 | ctx 12%\n");
    assert_eq!(sb.read("statusline.stdin"), STATUS);
    assert_eq!(sb.read("statusline.env").trim(), "unset");
    let usage = json(sb.read(".mcc/state/default/usage.json").as_bytes());
    assert_eq!(usage["profile"], "default");
    assert_eq!(
        usage["five_hour"],
        json!({"used_percentage": 23.5, "resets_at": 1792500000})
    );
    assert_eq!(
        usage["seven_day"],
        json!({"used_percentage": 41.2, "resets_at": 1792900000})
    );
    assert!(usage["updated_at"].as_str().unwrap().ends_with('Z'));

    // The default profile, named by its directory.
    std::fs::remove_file(sb.path(".mcc/state/default/usage.json")).unwrap();
    render(&sb, ".claude/settings.json", Some(".claude"), STATUS);
    assert!(sb.exists(".mcc/state/default/usage.json"));

    // The hot path never writes the config or the log.
    assert_eq!(sb.read(".mcc/config.toml"), config);
    assert_eq!(sb.read(".mcc/logs/mcc.log"), log);
}

#[test]
fn shim_without_rate_limits_keeps_the_last_cache() {
    let sb = sandbox();
    install(&sb, &[]);
    render(&sb, ".claude/settings.json", None, STATUS);
    let cached = sb.read(".mcc/state/default/usage.json");
    let out = render(&sb, ".claude/settings.json", None, r#"{"model":{}}"#);
    assert_eq!(out, "Opus 5 | ctx 12%\n");
    assert_eq!(sb.read(".mcc/state/default/usage.json"), cached);
    // Not JSON at all: still passed through.
    let out = render(&sb, ".claude/settings.json", None, "garbage");
    assert_eq!(out, "Opus 5 | ctx 12%\n");
    assert_eq!(sb.read("statusline.stdin"), "garbage");
}

#[test]
fn shim_prefix_and_synced_profile() {
    let sb = sandbox();
    install(&sb, &["--prefix"]);
    // The profile receives the shim with its settings.
    sb.mcc()
        .args(["sync", "work", "--only", "settings", "--yes"])
        .assert()
        .success();
    let rel = ".mcc/profiles/work/settings.json";
    assert_eq!(
        status_line(&sb, rel),
        status_line(&sb, ".claude/settings.json")
    );

    let out = render(&sb, rel, Some(".mcc/profiles/work"), STATUS);
    assert_eq!(out, "[work] Opus 5 | ctx 12%\n");
    assert_eq!(
        sb.read("statusline.env").trim(),
        sb.path(".mcc/profiles/work").display().to_string()
    );
    let usage = json(sb.read(".mcc/state/work/usage.json").as_bytes());
    assert_eq!(usage["profile"], "work");
    assert!(!sb.exists(".mcc/state/default/usage.json"));

    let out = render(&sb, ".claude/settings.json", None, STATUS);
    assert_eq!(out, "[default] Opus 5 | ctx 12%\n");

    // Uninstall puts the default's previous statusLine back in the profile.
    sb.mcc()
        .args(["statusline", "uninstall"])
        .assert()
        .success();
    assert_eq!(
        status_line(&sb, rel),
        json!({"command": "~/.claude/statusline.sh"})
    );
    assert_eq!(
        status_line(&sb, ".claude/settings.json"),
        json!({"command": "~/.claude/statusline.sh"})
    );
}

#[test]
fn shim_without_an_original_prints_only_the_prefix() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.write(".claude/settings.json", "{}");
    install(&sb, &["--prefix"]);
    let out = render(&sb, ".claude/settings.json", None, STATUS);
    assert_eq!(out, "[default]");
    // Unknown directories are not cached and get no prefix.
    std::fs::create_dir_all(sb.path("elsewhere")).unwrap();
    let out = render(&sb, ".claude/settings.json", Some("elsewhere"), STATUS);
    assert_eq!(out, "");
    assert!(!sb.exists(".mcc/state/elsewhere"));
}

#[test]
fn shim_never_waits_for_the_config_lock() {
    let sb = sandbox();
    install(&sb, &[]);
    let f = File::create(sb.path(".mcc/.lock")).unwrap();
    // SAFETY: the descriptor stays open for the duration of the call.
    assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
    let start = Instant::now();
    let out = render(&sb, ".claude/settings.json", None, STATUS);
    assert_eq!(out, "Opus 5 | ctx 12%\n");
    assert!(start.elapsed() < Duration::from_secs(10));
    drop(f);
}

#[test]
fn install_offers_profiles_that_do_not_sync_settings() {
    let sb = sandbox();
    let cfg = sb.read(".mcc/config.toml");
    let cfg = cfg.replace("exclude = []", "exclude = [\"settings\"]");
    assert!(cfg.contains("exclude = [\"settings\"]"), "{cfg}");
    sb.write(".mcc/config.toml", &cfg);
    let rel = ".mcc/profiles/work/settings.json";
    sb.write(
        rel,
        r#"{"statusLine":{"type":"command","command":"echo work-own","padding":1}}"#,
    );

    // No terminal and no --yes: the profile is left alone, with a hint.
    let err = install(&sb, &[]);
    assert!(err.contains("not added to work"), "{err}");
    assert_eq!(status_line(&sb, rel)["command"], "echo work-own");

    // --yes adds it, keeping the profile's own statusLine to chain.
    let err = install(&sb, &["--yes"]);
    assert!(err.contains("profile \"work\": set statusLine"), "{err}");
    let sl = status_line(&sb, rel);
    assert!(sl["command"].as_str().unwrap().ends_with("mcc statusline"));
    assert_eq!(sl["padding"], 1);
    let out = render(&sb, rel, Some(".mcc/profiles/work"), STATUS);
    assert_eq!(out, "work-own\n");
    assert!(sb.exists(".mcc/state/work/usage.json"));

    // Again: nothing changes.
    let err = install(&sb, &["--yes"]);
    assert!(err.contains("profile \"work\": already installed"), "{err}");

    sb.mcc()
        .args(["statusline", "uninstall"])
        .assert()
        .success();
    assert_eq!(
        status_line(&sb, rel),
        json!({"type": "command", "command": "echo work-own", "padding": 1})
    );
    assert!(!sb.exists(".mcc/state/work/statusline.json"));
}

#[test]
fn uninstall_leaves_a_changed_statusline_alone() {
    let sb = sandbox();
    install(&sb, &[]);
    sb.write(
        ".claude/settings.json",
        r#"{"statusLine":{"command":"echo mine"}}"#,
    );
    let out = sb
        .mcc()
        .args(["statusline", "uninstall"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&out.stderr).contains("changed since install"));
    assert_eq!(
        status_line(&sb, ".claude/settings.json"),
        json!({"command": "echo mine"})
    );
}

#[test]
fn rename_and_delete_carry_the_state() {
    let sb = sandbox();
    install(&sb, &[]);
    sb.mcc()
        .args(["sync", "work", "--only", "settings", "--yes"])
        .assert()
        .success();
    render(
        &sb,
        ".mcc/profiles/work/settings.json",
        Some(".mcc/profiles/work"),
        STATUS,
    );
    assert!(sb.exists(".mcc/state/work/usage.json"));

    sb.mcc().args(["rename", "work", "job"]).assert().success();
    assert!(!sb.exists(".mcc/state/work"));
    assert!(sb.exists(".mcc/state/job/usage.json"));
    // The directory keeps its name; the shim uses the profile's.
    render(
        &sb,
        ".mcc/profiles/work/settings.json",
        Some(".mcc/profiles/work"),
        STATUS,
    );
    assert!(!sb.exists(".mcc/state/work"));

    sb.mcc()
        .args(["delete", "job", "--yes", "--keep-auth"])
        .assert()
        .success();
    assert!(!sb.exists(".mcc/state/job"));
}

#[test]
fn statusline_is_a_reserved_alias() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "statusline", "--fresh", "--no-login"])
        .assert()
        .failure()
        .code(2);
}
