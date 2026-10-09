//! `mcc add`: default creation, clone/fresh, validation, rollback, login.

mod common;

use common::{Sandbox, json};
use predicates::prelude::*;

#[test]
fn creates_default_profile_when_missing() {
    let sb = Sandbox::new();
    sb.mcc()
        .args(["add", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("becomes the default profile"))
        .stderr(predicate::str::contains("fake@example.com"))
        .stderr(predicate::str::contains("start it with `claude`"));
    assert!(sb.exists(".claude/.credentials.json"));
    assert!(!sb.exists(".mcc/profiles"));
}

#[test]
fn default_profile_no_login() {
    let sb = Sandbox::new();
    sb.mcc().args(["add", "work", "--no-login", "--yes"]).assert().success()
        .stderr(predicate::str::contains("alias \"work\" is not used"));
    assert!(sb.path(".claude").is_dir());
    assert!(!sb.exists(".claude/.credentials.json"));
}

#[test]
fn clone_profile_end_to_end() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--clone"])
        .assert()
        .success()
        // Only the (inherited) login child writes to stdout.
        .stdout(predicate::str::contains("Login successful."))
        .stderr(predicate::str::contains("[1/8] Validate alias"))
        .stderr(predicate::str::contains("[3/8] Copy setup from the default profile"))
        .stderr(predicate::str::contains("no credentials copied"))
        .stderr(predicate::str::contains("[9/8]").not())
        .stderr(predicate::str::contains("fake@example.com"))
        .stderr(predicate::str::contains("start it with `work`"));

    let p = ".mcc/profiles/work";
    // Setup copied.
    for f in ["CLAUDE.md", "skills/alpha/SKILL.md", "skills/beta/SKILL.md", "agents/reviewer.md", "commands/deploy.md", "hooks/notify.sh", "plugins/cache/m/p1/1.0.0/plugin.json"] {
        assert!(sb.exists(&format!("{p}/{f}")), "missing {f}");
    }
    // State and credentials not copied (the fake login wrote its own).
    assert!(!sb.exists(&format!("{p}/history.jsonl")));
    assert!(!sb.exists(&format!("{p}/projects")));
    assert!(!sb.read(&format!("{p}/.credentials.json")).contains("default-token"));
    let state = json(sb.read(&format!("{p}/.claude.json")).as_bytes());
    assert!(state.get("oauthAccount").is_none());
    assert!(state.get("userID").is_none());
    assert_eq!(state["mcpServers"]["fs"]["command"], "npx");

    // Paths rewritten.
    let home = sb.home().display().to_string();
    let settings = sb.read(&format!("{p}/settings.json"));
    assert!(settings.contains(&format!("{home}/.mcc/profiles/work/hooks/notify.sh")), "{settings}");
    // statusline.sh is not a copied item, so it keeps pointing at the default
    // (rewriting it would leave a dangling reference in the profile).
    assert!(settings.contains("\"command\":\"~/.claude/statusline.sh\""), "{settings}");
    let plugins = sb.read(&format!("{p}/plugins/installed_plugins.json"));
    assert!(plugins.contains(&format!("{home}/.mcc/profiles/work/plugins/cache")), "{plugins}");

    // Launcher + rc block + registry.
    let wrapper = sb.read(".local/bin/work");
    assert!(wrapper.contains("# managed-by: mcc"));
    assert!(wrapper.contains("export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\""));
    let rc = sb.read(".zshrc");
    assert!(rc.starts_with("export EDITOR=vim\n"), "{rc}");
    assert!(rc.contains("# >>> mcc:work >>>\n"));
    assert!(rc.contains("alias work='CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" claude'"));
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("[profiles.work]") && cfg.contains("mode = \"clone\""), "{cfg}");

    // The launcher actually runs claude with the profile dir.
    let out = std::process::Command::new(sb.path(".local/bin/work"))
        .arg("hello")
        .env("HOME", sb.home())
        .env("PATH", format!("{}:/usr/bin:/bin", sb.path("fakebin").display()))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "fake claude hello");

    // …and still works when `claude` is not on PATH (cron, IDE tasks, trimmed PATH).
    let out = std::process::Command::new(sb.path(".local/bin/work"))
        .args(["auth", "status"])
        .env("HOME", sb.home())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"loggedIn\": true"), "{out:?}");
}

#[test]
fn fresh_profile_no_login() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "lab", "--fresh", "--no-login"])
        .assert()
        .success()
        .stderr(predicate::str::contains("skipped — fresh profile"))
        .stderr(predicate::str::contains("run `lab` to log in"));
    let dir = sb.path(".mcc/profiles/lab");
    assert!(dir.is_dir());
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
    assert!(sb.exists(".local/bin/lab"));
}

#[test]
fn rejects_invalid_and_conflicting_aliases() {
    let sb = Sandbox::new();
    sb.seed_default();
    let reject = |alias: &str, why: &str| {
        sb.mcc()
            .args(["add", alias, "--fresh", "--no-login"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(why))
            .stderr(predicate::str::contains("hint:"));
    };
    reject("Work", "not a valid alias");
    reject("1abc", "not a valid alias");
    reject("claude", "reserved by mcc");
    reject("export", "shell keyword or builtin");
    reject("ls", "a command named \"ls\" exists");
    reject("cus_luong", "alias \"cus_luong\" is defined in ~/.zshrc");

    sb.mcc().args(["add", "work", "--fresh", "--no-login"]).assert().success();
    reject("work", "an mcc profile named \"work\" already exists");
}

#[test]
fn non_interactive_requires_alias_and_mode() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc().arg("add").assert().code(2).stderr(predicate::str::contains("mcc add <alias>"));
    sb.mcc().args(["add", "work"]).assert().code(2).stderr(predicate::str::contains("--clone or --fresh"));
    // --yes picks clone.
    sb.mcc().args(["add", "work", "--yes", "--no-login"]).assert().success();
    assert!(sb.exists(".mcc/profiles/work/skills/alpha/SKILL.md"));
    sb.mcc().args(["add", "x", "--clone", "--fresh"]).assert().code(2);
}

#[test]
fn rolls_back_on_failure() {
    let sb = Sandbox::new();
    sb.seed_default();
    // Make the rc file unwritable as a file: a directory in its place.
    std::fs::remove_file(sb.path(".zshrc")).unwrap();
    std::fs::create_dir_all(sb.path(".zshrc")).unwrap();
    sb.mcc()
        .args(["add", "work", "--clone", "--no-login"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Rolling back"))
        .stderr(predicate::str::contains("[5/8] Add shell alias"));
    assert!(!sb.exists(".mcc/profiles/work"));
    assert!(!sb.exists(".local/bin/work"));
    assert!(!sb.read(".mcc/config.toml").contains("[profiles.work]"));
}

#[test]
fn failed_login_keeps_profile_non_interactively() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .env("FAKE_LOGIN_FAIL", "1")
        .args(["add", "work", "--fresh"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not logged in"))
        .stderr(predicate::str::contains("run `work` to log in"));
    assert!(sb.exists(".mcc/profiles/work"));
}

#[test]
fn missing_claude_binary_blocks_login() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc().arg("init").assert().success();
    std::fs::remove_file(sb.path("fakebin/claude")).unwrap();
    sb.mcc()
        .env("MCC_CLAUDE_BIN", "")
        .args(["add", "work", "--fresh"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("`claude` command was not found"))
        .stderr(predicate::str::contains("hint:"));
    assert!(!sb.exists(".mcc/profiles/work"));
}
