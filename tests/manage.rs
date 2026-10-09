//! `mcc list`, `mcc sync`, `mcc delete`, and cross-cutting error behavior.

mod common;

use std::time::Duration;

use common::{Sandbox, json};
use predicates::prelude::*;

fn with_profile(sb: &Sandbox, alias: &str, mode: &str) {
    sb.mcc().args(["add", alias, mode]).assert().success();
}

// ---- list -------------------------------------------------------------------

#[test]
fn list_json_reports_metrics() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--fresh");
    let out = sb
        .mcc()
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    let names: Vec<&str> = arr.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["default", "lab", "work"]);

    let d = &arr[0];
    assert_eq!(d["command"], "claude");
    assert_eq!(d["is_default"], true);
    assert_eq!(d["account"], "me@example.com");
    assert_eq!(d["organization"], "Acme");
    assert_eq!(d["skills"], 2);
    assert_eq!(d["agents"], 1);
    assert_eq!(d["commands"], 1);
    assert_eq!(d["plugins_installed"], 2);
    assert_eq!(d["plugins_enabled"], 1);
    assert_eq!(d["mcp_servers"], 1);
    assert_eq!(d["hooks"], 1);
    assert_eq!(d["projects"], 1);
    assert_eq!(d["sessions"], 1);
    assert_eq!(d["model"], "opus");

    let w = &arr[2];
    assert_eq!(w["command"], "work");
    assert_eq!(w["mode"], "clone");
    assert_eq!(w["skills"], 2);
    assert_eq!(w["mcp_servers"], 1);
    assert_eq!(w["projects"], 0);
    assert!(w["last_sync"].is_string());
    assert_eq!(arr[1]["skills"], 0);
}

#[test]
fn list_table_and_long() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.mcc()
        .arg("ls")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"(?m)^NAME\s+COMMAND\s+ACCOUNT\s+SKILLS").unwrap())
        .stdout(predicate::str::is_match(r"(?m)^default ★\s+claude\s+me@example.com").unwrap())
        .stdout(predicate::str::is_match(r"(?m)^work\s+work\s+").unwrap())
        .stderr(predicate::str::contains("2 profiles"));
    sb.mcc()
        .args(["list", "--long"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Plugins:\s+1 enabled / 2 installed").unwrap())
        .stdout(predicate::str::is_match(r"Projects:\s+1 \(1 sessions\)").unwrap())
        .stdout(predicate::str::contains("Last sync:"));
    sb.mcc().args(["list", "--json", "--long"]).assert().code(2);
}

#[test]
fn list_warns_about_missing_dirs() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    std::fs::remove_dir_all(sb.path(".mcc/profiles/work")).unwrap();
    sb.mcc()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("work (missing)"))
        .stderr(predicate::str::contains("is missing"));
}

// ---- sync -------------------------------------------------------------------

#[test]
fn sync_dry_run_then_apply() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");

    sb.mcc()
        .args(["sync", "work", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("dry run"))
        .stderr(predicate::str::contains("+ skills/alpha/SKILL.md"));
    assert!(!sb.exists(".mcc/profiles/work/skills"));

    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("now matches the default"));
    assert!(sb.exists(".mcc/profiles/work/skills/alpha/SKILL.md"));
    assert!(!sb.exists(".mcc/profiles/work/.credentials.json.default"));
    let state = json(sb.read(".mcc/profiles/work/.claude.json").as_bytes());
    assert_eq!(state["mcpServers"]["fs"]["command"], "npx");
    assert!(state.get("oauthAccount").is_none());

    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already up to date"));
}

#[test]
fn sync_updates_backs_up_and_keeps_identity() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    let creds_before = sb.read(".mcc/profiles/work/.credentials.json");
    // Profile's own identity + extra file; default gains a skill and a new setting.
    let mut state = json(sb.read(".mcc/profiles/work/.claude.json").as_bytes());
    state["oauthAccount"] = serde_json::json!({"emailAddress": "work@corp.com"});
    sb.write(".mcc/profiles/work/.claude.json", &state.to_string());
    sb.write(".mcc/profiles/work/agents/mine.md", "only in work");
    sb.write(".claude/skills/gamma/SKILL.md", "new");
    sb.write(".claude/settings.json", r#"{"model":"sonnet"}"#);

    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "only in profile (kept; --prune removes)",
        ))
        .stderr(predicate::str::contains("Back up profile config"));
    assert!(sb.exists(".mcc/profiles/work/skills/gamma/SKILL.md"));
    assert_eq!(
        sb.read(".mcc/profiles/work/settings.json"),
        r#"{"model":"sonnet"}"#
    );
    assert!(sb.exists(".mcc/profiles/work/agents/mine.md"));
    let state = json(sb.read(".mcc/profiles/work/.claude.json").as_bytes());
    assert_eq!(state["oauthAccount"]["emailAddress"], "work@corp.com");
    assert_eq!(
        sb.read(".mcc/profiles/work/.credentials.json"),
        creds_before
    );
    let backups = std::fs::read_dir(sb.path(".mcc/profiles/work/.mcc-backup"))
        .unwrap()
        .count();
    assert_eq!(backups, 1);

    // Prune needs confirmation without a TTY…
    sb.mcc()
        .args(["sync", "work", "--prune"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--yes"));
    assert!(sb.exists(".mcc/profiles/work/agents/mine.md"));
    // …and removes extras with --yes.
    sb.mcc()
        .args(["sync", "work", "--prune", "--yes"])
        .assert()
        .success();
    assert!(!sb.exists(".mcc/profiles/work/agents/mine.md"));
    assert!(sb.exists(".mcc/profiles/work/agents/reviewer.md"));
}

#[test]
fn sync_only_and_all() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "a1", "--fresh");
    with_profile(&sb, "a2", "--fresh");
    sb.mcc()
        .args(["sync", "--all", "--only", "skills,claude-md"])
        .assert()
        .success()
        .stderr(predicate::str::contains("(2/2)"))
        .stderr(predicate::str::contains("2/2 profiles synced"));
    for a in ["a1", "a2"] {
        assert!(sb.exists(&format!(".mcc/profiles/{a}/skills/beta/SKILL.md")));
        assert!(sb.exists(&format!(".mcc/profiles/{a}/CLAUDE.md")));
        assert!(!sb.exists(&format!(".mcc/profiles/{a}/agents")));
        assert!(!sb.exists(&format!(".mcc/profiles/{a}/settings.json")));
    }
}

#[test]
fn sync_errors() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc().arg("sync").assert().code(2);
    sb.mcc()
        .args(["sync", "ghost"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"ghost\""));
    sb.mcc()
        .args(["sync", "default"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("sync source"));
    sb.mcc()
        .args(["sync", "--all"])
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing to sync"));
    with_profile(&sb, "work", "--fresh");
    sb.mcc()
        .args(["sync", "work", "--only", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("valid items:"));
    sb.mcc().args(["sync", "work", "--all"]).assert().code(2);
}

// ---- delete -----------------------------------------------------------------

#[test]
fn delete_removes_everything() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "keep", "--fresh");
    assert!(sb.exists(".mcc/profiles/work/.credentials.json"));

    sb.mcc()
        .args(["delete", "work", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("credentials removed"))
        .stderr(predicate::str::contains("profile \"work\" deleted"));
    assert!(!sb.exists(".mcc/profiles/work"));
    assert!(!sb.exists(".local/bin/work"));
    let rc = sb.read(".zshrc");
    assert!(!rc.contains("mcc:work"), "{rc}");
    assert!(rc.contains("mcc:keep"));
    assert!(rc.contains("alias cus_luong="));
    let cfg = sb.read(".mcc/config.toml");
    assert!(!cfg.contains("[profiles.work]") && cfg.contains("[profiles.keep]"));
    // The default profile is untouched.
    assert!(sb.exists(".claude/.credentials.json"));
    assert!(sb.exists(".claude/skills/alpha/SKILL.md"));
}

#[test]
fn delete_guardrails() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["delete", "default", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("can't be deleted"));
    sb.mcc().args(["rm", "claude", "--yes"]).assert().code(2);
    sb.mcc()
        .args(["delete", "ghost", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("mcc list"));
    sb.mcc().arg("delete").assert().code(2);

    with_profile(&sb, "work", "--fresh");
    // No TTY and no --yes: refuse, delete nothing.
    sb.mcc()
        .args(["delete", "work"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--yes"));
    assert!(sb.exists(".mcc/profiles/work"));

    // A launcher not written by mcc is left alone.
    sb.write(".local/bin/work", "#!/bin/sh\necho mine\n");
    sb.mcc()
        .args(["delete", "work", "--yes", "--keep-auth"])
        .assert()
        .success()
        .stderr(predicate::str::contains("was not created by mcc"))
        .stderr(predicate::str::contains("skipped — --keep-auth"));
    assert!(sb.exists(".local/bin/work"));
    assert!(!sb.exists(".mcc/profiles/work"));
}

#[test]
fn delete_reports_not_logged_in_truthfully() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "lab", "--fresh", "--no-login"])
        .assert()
        .success();
    sb.mcc()
        .args(["delete", "lab", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("skipped — was not logged in"))
        .stderr(predicate::str::contains("credentials removed").not());
}

#[test]
fn delete_cleans_unregistered_leftover_dir() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.write(".mcc/profiles/old/settings.json", "{}");
    sb.mcc()
        .args(["delete", "old", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not in the mcc registry"));
    assert!(!sb.exists(".mcc/profiles/old"));
}

// ---- cross-cutting ----------------------------------------------------------

#[test]
fn full_lifecycle_round_trip() {
    let sb = Sandbox::new();
    sb.seed_default();
    let rc_before = sb.read(".zshrc");
    with_profile(&sb, "work", "--clone");
    sb.mcc().args(["sync", "work"]).assert().success();
    sb.mcc()
        .args(["delete", "work", "--yes"])
        .assert()
        .success();
    assert_eq!(
        sb.read(".zshrc"),
        rc_before,
        "rc file restored byte-for-byte"
    );
    let out = sb
        .mcc()
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out).as_array().unwrap().len(), 1);
}

#[test]
fn errors_go_to_stderr_with_hints_and_are_logged() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["sync", "ghost"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("✗ error:"))
        .stderr(predicate::str::contains("hint:"));
    let log = sb.read(".mcc/logs/mcc.log");
    assert!(log.contains("[ERROR] no profile named \"ghost\""), "{log}");
}

#[test]
fn ctrl_c_exits_130() {
    let sb = Sandbox::new();
    // A claude whose --version hangs keeps mcc busy in first-run detection.
    sb.write("fakebin/slow-claude", "#!/bin/sh\nsleep 5\n");
    common::chmod_x(&sb.path("fakebin/slow-claude"));
    let bin = assert_cmd::cargo::cargo_bin("mcc");
    let mut child = std::process::Command::new(bin)
        .arg("list")
        .env("HOME", sb.home())
        .env("MCC_HOME", sb.home())
        .env("MCC_CLAUDE_BIN", sb.path("fakebin/slow-claude"))
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
}

#[test]
fn delete_refuses_paths_outside_profiles_dir() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    sb.write("elsewhere/keep.txt", "keep");
    let abs = sb.path("elsewhere").display().to_string();
    for alias in ["..", "../..", "../x", ".", abs.as_str(), "work/../.."] {
        sb.mcc()
            .args(["delete", alias, "--yes"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("not a valid alias"));
    }
    assert!(sb.exists(".mcc/config.toml"));
    assert!(sb.exists(".mcc/profiles/work"));
    assert!(sb.exists(".claude/settings.json"));
    assert!(sb.exists("elsewhere/keep.txt"));
    assert!(sb.read(".zshrc").contains("mcc:work"));
}

#[test]
fn delete_refuses_registered_dir_outside_profiles_dir() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    sb.write("elsewhere/keep.txt", "keep");
    let cfg = sb.read(".mcc/config.toml");
    let tampered = cfg.replace(
        &sb.path(".mcc/profiles/work").display().to_string(),
        &sb.path("elsewhere").display().to_string(),
    );
    assert_ne!(cfg, tampered);
    sb.write(".mcc/config.toml", &tampered);
    sb.mcc()
        .args(["delete", "work", "--yes"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("refusing to delete"))
        .stderr(predicate::str::contains("hint:"));
    assert!(sb.exists("elsewhere/keep.txt"));
    assert!(sb.exists(".local/bin/work"));
}

#[test]
fn back_to_back_syncs_keep_every_backup() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.write(".claude/settings.json", r#"{"model":"one"}"#);
    sb.mcc().args(["sync", "work"]).assert().success();
    sb.write(".claude/settings.json", r#"{"model":"two"}"#);
    sb.mcc().args(["sync", "work"]).assert().success();
    let mut kept: Vec<String> = std::fs::read_dir(sb.path(".mcc/profiles/work/.mcc-backup"))
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path().join("settings.json")).unwrap())
        .collect();
    kept.sort();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert_eq!(kept[0], r#"{"model":"one"}"#);
    assert!(kept[1].contains("opus"), "{kept:?}");
}

#[test]
fn errors_without_a_specific_hint_get_a_fallback_hint() {
    let sb = Sandbox::new();
    sb.seed_default();
    // A directory where the rc file belongs makes the alias step fail with a plain I/O error.
    std::fs::remove_file(sb.path(".zshrc")).unwrap();
    std::fs::create_dir_all(sb.path(".zshrc")).unwrap();
    sb.mcc()
        .args(["add", "work", "--fresh", "--no-login"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("✗ error:"))
        .stderr(predicate::str::contains("hint:"))
        .stderr(predicate::str::contains("~/.mcc/logs/mcc.log"));
}

#[test]
fn sync_keeps_profile_dir_when_default_item_is_a_symlink() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.write(".mcc/profiles/work/skills/mine/SKILL.md", "mine");
    // The default's skills/ becomes a symlink into a dotfiles repo.
    std::fs::create_dir_all(sb.path("dotfiles")).unwrap();
    std::fs::rename(sb.path(".claude/skills"), sb.path("dotfiles/skills")).unwrap();
    std::os::unix::fs::symlink(sb.path("dotfiles/skills"), sb.path(".claude/skills")).unwrap();

    sb.mcc()
        .args(["sync", "work", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "only in profile (kept; --prune removes)",
        ));
    assert_eq!(sb.read(".mcc/profiles/work/skills/mine/SKILL.md"), "mine");

    // Replacing it is a removal: needs --prune and confirmation.
    sb.mcc()
        .args(["sync", "work", "--prune"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--yes"));
    assert_eq!(sb.read(".mcc/profiles/work/skills/mine/SKILL.md"), "mine");
    sb.mcc()
        .args(["sync", "work", "--prune", "--yes"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_link(sb.path(".mcc/profiles/work/skills")).unwrap(),
        sb.path("dotfiles/skills")
    );
    assert!(sb.exists("dotfiles/skills/alpha/SKILL.md"));
    assert!(!sb.exists("dotfiles/skills/mine"));
}

#[test]
fn sync_all_shows_each_failing_profiles_hint() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "a1", "--fresh");
    with_profile(&sb, "a2", "--fresh");
    std::fs::remove_dir_all(sb.path(".mcc/profiles/a1")).unwrap();
    sb.mcc()
        .args(["sync", "--all"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("a1: profile directory"))
        .stderr(predicate::str::contains("mcc delete a1 --yes"))
        .stderr(predicate::str::contains("1/2 profiles synced"));
}
