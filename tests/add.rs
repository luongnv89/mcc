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
    // Logged in, so Claude Code must not rerun its first-run login on start.
    assert_eq!(
        json(sb.read(".claude.json").as_bytes())["hasCompletedOnboarding"],
        true
    );
}

#[test]
fn default_profile_no_login() {
    let sb = Sandbox::new();
    sb.mcc()
        .args(["add", "work", "--no-login", "--yes"])
        .assert()
        .success()
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
        .stderr(predicate::str::contains(
            "[3/8] Copy setup from the default profile",
        ))
        .stderr(predicate::str::contains("no credentials copied"))
        .stderr(predicate::str::contains("[9/8]").not())
        .stderr(predicate::str::contains("fake@example.com"))
        .stderr(predicate::str::contains("start it with `work`"));

    let p = ".mcc/profiles/work";
    // Setup copied.
    for f in [
        "CLAUDE.md",
        "skills/alpha/SKILL.md",
        "skills/beta/SKILL.md",
        "agents/reviewer.md",
        "commands/deploy.md",
        "hooks/notify.sh",
    ] {
        assert!(sb.exists(&format!("{p}/{f}")), "missing {f}");
    }
    // Plugins are shared with the default, not copied (#47).
    assert!(!sb.exists(&format!("{p}/plugins")));
    // State and credentials not copied (the fake login wrote its own).
    assert!(!sb.exists(&format!("{p}/history.jsonl")));
    assert!(!sb.exists(&format!("{p}/projects")));
    assert!(
        !sb.read(&format!("{p}/.credentials.json"))
            .contains("default-token")
    );
    let state = json(sb.read(&format!("{p}/.claude.json")).as_bytes());
    assert!(state.get("oauthAccount").is_none());
    assert!(state.get("userID").is_none());
    assert_eq!(state["mcpServers"]["fs"]["command"], "npx");
    assert_eq!(state["hasCompletedOnboarding"], true);
    assert_eq!(state["theme"], "dark-ansi");

    // Paths rewritten.
    let home = sb.home().display().to_string();
    let settings = sb.read(&format!("{p}/settings.json"));
    assert!(
        settings.contains(&format!("{home}/.mcc/profiles/work/hooks/notify.sh")),
        "{settings}"
    );
    // statusline.sh is not a copied item, so it keeps pointing at the default
    // (rewriting it would leave a dangling reference in the profile).
    assert!(
        settings.contains("\"command\":\"~/.claude/statusline.sh\""),
        "{settings}"
    );

    // Launcher + rc block + registry.
    let wrapper = sb.read(".local/bin/work");
    assert!(wrapper.contains("# managed-by: mcc"));
    assert!(wrapper.contains("export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\""));
    assert!(wrapper.contains("export CLAUDE_CODE_PLUGIN_CACHE_DIR=\"$HOME/.claude/plugins\""));
    let rc = sb.read(".zshrc");
    assert!(rc.starts_with("export EDITOR=vim\n"), "{rc}");
    assert!(rc.contains("# >>> mcc:work >>>\n"));
    // The alias runs the launcher, so `work --mcc-share` reaches it.
    assert!(
        rc.contains("alias work='\"$HOME/.local/bin/work\"'"),
        "{rc}"
    );
    let cfg = sb.read(".mcc/config.toml");
    assert!(
        cfg.contains("[profiles.work]") && cfg.contains("mode = \"clone\""),
        "{cfg}"
    );
    assert!(cfg.contains("link = [\"plugins\"]"), "{cfg}");

    // The launcher actually runs claude with the profile dir.
    let out = std::process::Command::new(sb.path(".local/bin/work"))
        .arg("hello")
        .env("HOME", sb.home())
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", sb.path("fakebin").display()),
        )
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "fake claude hello"
    );

    // …and still works when `claude` is not on PATH (cron, IDE tasks, trimmed PATH).
    let out = std::process::Command::new(sb.path(".local/bin/work"))
        .args(["auth", "status"])
        .env("HOME", sb.home())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"loggedIn\": true"),
        "{out:?}"
    );
}

#[test]
fn clone_shares_plugins_through_the_launcher() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--clone", "--no-login"])
        .assert()
        .success()
        .stderr(predicate::str::contains("plugins shared with the default"))
        .stderr(predicate::str::contains("shared plugins: \"work\""));
    let out = std::process::Command::new(sb.path(".local/bin/work"))
        .arg("plugin-root")
        .env("HOME", sb.home())
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", sb.path("fakebin").display()),
        )
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("plugin root: {}", sb.path(".claude/plugins").display())
    );
}

#[test]
fn clone_with_copy_plugins_copies_them_as_before() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--clone", "--no-login", "--copy-plugins"])
        .assert()
        .success()
        .stderr(predicate::str::contains("shared plugins").not());
    let p = ".mcc/profiles/work";
    assert!(sb.exists(&format!("{p}/plugins/cache/m/p1/1.0.0/plugin.json")));
    let home = sb.home().display().to_string();
    let plugins = sb.read(&format!("{p}/plugins/installed_plugins.json"));
    assert!(
        plugins.contains(&format!("{home}/.mcc/profiles/work/plugins/cache")),
        "{plugins}"
    );
    assert!(
        !sb.read(".local/bin/work")
            .contains("export CLAUDE_CODE_PLUGIN_CACHE_DIR")
    );
    assert!(!sb.read(".mcc/config.toml").contains("plugins"));
    // The launcher clears mcc's own value of the variable, never the user's.
    let launch = |inherited: &str| {
        let out = std::process::Command::new(sb.path(".local/bin/work"))
            .arg("plugin-root")
            .env("HOME", sb.home())
            .env("CLAUDE_CODE_PLUGIN_CACHE_DIR", inherited)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", sb.path("fakebin").display()),
            )
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let root = sb.path(".claude/plugins").display().to_string();
    assert_eq!(launch(&root), "plugin root: unset");
    assert_eq!(launch("/opt/seed"), "plugin root: /opt/seed");
}

#[test]
fn copy_plugins_contradicts_link_plugins() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args([
            "add",
            "work",
            "--clone",
            "--no-login",
            "--copy-plugins",
            "--link",
            "plugins",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("contradict"));
    assert!(!sb.exists(".mcc/profiles/work"));
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
fn fresh_profile_login_skips_first_run_setup() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc().args(["add", "lab", "--fresh"]).assert().success();
    let state = json(sb.read(".mcc/profiles/lab/.claude.json").as_bytes());
    assert_eq!(state["hasCompletedOnboarding"], true);
    assert!(state.get("theme").is_none()); // fresh: nothing taken from the default
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
    reject("doctor", "reserved by mcc");
    reject("export", "shell keyword or builtin");
    reject("ls", "a command named \"ls\" exists");
    reject("cus_luong", "alias \"cus_luong\" is defined in ~/.zshrc");

    sb.mcc()
        .args(["add", "work", "--fresh", "--no-login"])
        .assert()
        .success();
    reject("work", "an mcc profile named \"work\" already exists");
}

#[test]
fn non_interactive_requires_alias_and_mode() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .arg("add")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("mcc add <alias>"));
    sb.mcc()
        .args(["add", "work"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--clone or --fresh"));
    // --yes picks clone.
    sb.mcc()
        .args(["add", "work", "--yes", "--no-login"])
        .assert()
        .success();
    assert!(sb.exists(".mcc/profiles/work/skills/alpha/SKILL.md"));
    sb.mcc()
        .args(["add", "x", "--clone", "--fresh"])
        .assert()
        .code(2);
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
    // Not logged in: leave the first-run setup (with its login) in place.
    assert!(!sb.exists(".mcc/profiles/work/.claude.json"));
}

#[test]
fn failed_login_with_email_or_sso_hints_at_claude_version() {
    const HINT: &str = "--email and --sso need a Claude Code version";
    let sb = Sandbox::new();
    sb.seed_default();
    let out = sb
        .mcc()
        .env("FAKE_LOGIN_FAIL", "1")
        .args(["add", "work", "--fresh", "--email", "me@example.com"])
        .assert()
        .success()
        .stderr(predicate::str::contains(HINT))
        .get_output()
        .clone();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains("me@example.com"), "address echoed: {all}");

    sb.mcc()
        .env("FAKE_LOGIN_FAIL", "1")
        .args(["add", "sso", "--fresh", "--sso"])
        .assert()
        .success()
        .stderr(predicate::str::contains(HINT));

    sb.mcc()
        .env("FAKE_LOGIN_FAIL", "1")
        .args(["add", "plain", "--fresh"])
        .assert()
        .success()
        .stderr(predicate::str::contains(HINT).not());
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

#[test]
fn path_is_reserved_for_the_path_block() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "path", "--fresh", "--no-login"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("reserved by mcc"));
    assert!(!sb.exists(".mcc/profiles/path"));
    assert!(!sb.read(".zshrc").contains("mcc:path"));
}

#[test]
fn login_spawn_failure_rolls_back() {
    let sb = Sandbox::new();
    sb.seed_default();
    let rc_before = sb.read(".zshrc");
    sb.mcc()
        .env("MCC_CLAUDE_BIN", sb.path("fakebin/does-not-exist"))
        .args(["add", "work", "--clone"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Rolling back"))
        .stderr(predicate::str::contains("hint:"));
    assert!(!sb.exists(".mcc/profiles/work"));
    assert!(!sb.exists(".local/bin/work"));
    assert_eq!(sb.read(".zshrc"), rc_before);
    assert!(!sb.read(".mcc/config.toml").contains("[profiles.work]"));
}

// ---- account identity and labels (#39) --------------------------------------

/// The `[profiles.<alias>.account]` table of the config, or "" when absent.
fn account_section(sb: &Sandbox, alias: &str) -> String {
    let cfg = sb.read(".mcc/config.toml");
    let Some(start) = cfg.find(&format!("[profiles.{alias}.account]")) else {
        return String::new();
    };
    let rest = &cfg[start + 1..];
    let end = rest.find("\n[").map_or(cfg.len(), |i| start + 1 + i);
    cfg[start..end].to_string()
}

#[test]
fn records_account_organization_and_label() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--clone", "--label", "Acme (work)"])
        .env("FAKE_EMAIL", "dev@acme.test")
        .env("FAKE_ORG_NAME", "Acme")
        .env("FAKE_ORG_ID", "org-uuid-1")
        .assert()
        .success()
        .stderr(predicate::str::contains("dev@acme.test (Acme)"))
        .stderr(predicate::str::contains("already logged in").not());

    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("label = \"Acme (work)\""), "{cfg}");
    let acct = account_section(&sb, "work");
    assert!(acct.contains("email = \"dev@acme.test\""), "{cfg}");
    assert!(acct.contains("organization = \"Acme\""), "{cfg}");
    assert!(acct.contains("organization_uuid = \"org-uuid-1\""), "{cfg}");
    assert!(acct.contains("recorded_at = "), "{cfg}");

    // `mcc list` shows the recorded organization (the fake login writes no
    // oauthAccount into the profile's state file).
    let out = sb
        .mcc()
        .args(["list", "--json", "--no-usage", "--show-email"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let w = &v.as_array().unwrap()[1];
    assert_eq!(w["name"], "work");
    assert_eq!(w["account"], "dev@acme.test");
    assert_eq!(w["organization"], "Acme");
    assert_eq!(w["organization_uuid"], "org-uuid-1");
    assert_eq!(w["label"], "Acme (work)");
    let d = &v.as_array().unwrap()[0];
    assert_eq!(d["organization"], "Acme");
    assert!(d["organization_uuid"].is_null());
    assert!(d["label"].is_null());

    sb.mcc()
        .args(["list", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"(?m)^work\s+Acme \(work\)\s").unwrap())
        .stdout(predicate::str::contains("dev@acme.test").not());
    sb.mcc()
        .args(["list", "--no-usage", "--show-email"])
        .assert()
        .success()
        .stdout(
            predicate::str::is_match(r"(?m)^work\s+dev@acme\.test \(Acme \(work\)\)\s").unwrap(),
        );
    sb.mcc()
        .args(["list", "--long", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Organization ID: org-uuid-1"))
        .stdout(predicate::str::contains("Label:           Acme (work)"));
}

#[test]
fn state_file_account_wins_over_the_recorded_one() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--fresh"])
        .env("FAKE_ORG_NAME", "Old Org")
        .assert()
        .success();
    // Claude Code later writes the account it is logged in to.
    sb.write(
        ".mcc/profiles/work/.claude.json",
        r#"{"oauthAccount":{"emailAddress":"fake@example.com","accountUuid":"acc-1","organizationName":"New Org","organizationUuid":"org-2"}}"#,
    );
    let out = sb
        .mcc()
        .args(["list", "--json", "--no-usage"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let w = &json(&out)[1];
    assert_eq!(w["organization"], "New Org");
    assert_eq!(w["organization_uuid"], "org-2");

    // Logged in to another account whose state has no organization: the
    // recorded organization (of the old account) is not shown.
    sb.write(
        ".mcc/profiles/work/.claude.json",
        r#"{"oauthAccount":{"emailAddress":"new@example.com"}}"#,
    );
    let out = sb
        .mcc()
        .args(["list", "--json", "--no-usage", "--show-email"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let w = &json(&out)[1];
    assert_eq!(w["account"], "new@example.com");
    assert!(w["organization"].is_null(), "{w}");
}

#[test]
fn warns_when_the_account_is_already_used() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--fresh"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already logged in").not());
    // Same account as `work` (recorded in the config).
    sb.mcc()
        .args(["add", "lab", "--fresh"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "fake@example.com is already logged in to profile \"work\"",
        ))
        .stderr(predicate::str::contains("mcc delete lab"));
    // Same account and organization as the default profile (its
    // ~/.claude.json), any case.
    sb.mcc()
        .args(["add", "side", "--fresh"])
        .env("FAKE_EMAIL", "Me@Example.com")
        .env("FAKE_ORG_NAME", "ACME")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Me@Example.com is already logged in to profile \"default\"",
        ));
    // Another account: no warning.
    sb.mcc()
        .args(["add", "other", "--fresh"])
        .env("FAKE_EMAIL", "other@example.com")
        .assert()
        .success()
        .stderr(predicate::str::contains("already logged in").not());
    // Not logged in: nothing recorded, nothing to compare.
    sb.mcc()
        .args(["add", "later", "--fresh", "--no-login"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already logged in").not());
    assert!(account_section(&sb, "later").is_empty());
}

#[test]
fn the_same_login_in_another_organization_is_not_a_duplicate() {
    let sb = Sandbox::new();
    sb.seed_default();
    // A work seat: the same email as `personal` below, in an organization.
    sb.mcc()
        .args(["add", "work", "--fresh"])
        .env("FAKE_ORG_NAME", "Acme")
        .env("FAKE_ORG_ID", "org-acme")
        .assert()
        .success();
    // The same login without an organization (a personal plan): a note,
    // not the duplicate warning or the `mcc delete` hint.
    sb.mcc()
        .args(["add", "personal", "--fresh"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "fake@example.com is also logged in to profile \"work\" under another organization",
        ))
        .stderr(predicate::str::contains("already logged in").not())
        .stderr(predicate::str::contains("mcc delete").not());
    // The same login in another organization (by UUID): also a note only.
    sb.mcc()
        .args(["add", "contract", "--fresh"])
        .env("FAKE_ORG_NAME", "Acme")
        .env("FAKE_ORG_ID", "org-other")
        .assert()
        .success()
        .stderr(predicate::str::contains("under another organization"))
        .stderr(predicate::str::contains("already logged in").not());
    // The same login in the same organization (same UUID): a duplicate.
    sb.mcc()
        .args(["add", "again", "--fresh"])
        .env("FAKE_ORG_NAME", "Acme Inc")
        .env("FAKE_ORG_ID", "org-acme")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "fake@example.com is already logged in to profile \"work\"",
        ))
        .stderr(predicate::str::contains("mcc delete again"));
}

#[test]
fn warns_on_two_profiles_with_the_same_account() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc().args(["add", "work", "--fresh"]).assert().success();
    sb.mcc().args(["add", "lab", "--fresh"]).assert().success();
    sb.mcc()
        .args(["add", "third", "--fresh"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "already logged in to profiles \"lab\", \"work\"",
        ));
}

#[test]
fn default_profile_warns_about_a_profile_on_the_same_account() {
    let sb = Sandbox::new();
    sb.write(
        ".mcc/config.toml",
        &format!(
            "[profiles.work]\ndir = \"{}\"\nmode = \"fresh\"\ncreated_at = \"2026-10-10T00:00:00+02:00\"\n\n\
             [profiles.work.account]\nemail = \"fake@example.com\"\n",
            sb.path(".mcc/profiles/work").display()
        ),
    );
    sb.mcc()
        .args(["add", "--yes", "--label", "personal"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "--label is not used for the default profile",
        ))
        .stderr(predicate::str::contains(
            "fake@example.com is already logged in to profile \"work\"",
        ))
        .stderr(predicate::str::contains("claude auth login"));
}

#[test]
fn rejects_an_invalid_label() {
    let sb = Sandbox::new();
    sb.seed_default();
    for bad in ["", "   ", "a\tb", &"x".repeat(65)] {
        sb.mcc()
            .args(["add", "work", "--fresh", "--label", bad])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("label"));
    }
    assert!(!sb.exists(".mcc/profiles/work"));
}

#[test]
fn no_label_without_flag_and_no_account_without_login() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args([
            "add",
            "work",
            "--fresh",
            "--no-login",
            "--label",
            "  personal  ",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Label:"));
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("label = \"personal\""), "{cfg}");
    assert!(!cfg.contains(".account]"), "{cfg}");

    sb.mcc()
        .args(["add", "lab", "--fresh", "--yes"])
        .assert()
        .success();
    let cfg = sb.read(".mcc/config.toml");
    assert_eq!(cfg.matches("label = ").count(), 1, "{cfg}");
    assert!(account_section(&sb, "lab").contains("email = \"fake@example.com\""));
}

/// What the last `claude auth login` was passed, one argument per entry.
fn login_args(sb: &Sandbox) -> Vec<String> {
    sb.read("login-args")
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn email_and_sso_are_passed_to_claude_auth_login() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc().args(["add", "work", "--fresh"]).assert().success();
    assert!(login_args(&sb).is_empty());

    // Each its own argument, as typed; mcc itself doesn't print the address.
    sb.mcc()
        .args([
            "add",
            "lab",
            "--fresh",
            "--email",
            "typed@corp.example",
            "--sso",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("typed@corp.example").not());
    assert_eq!(login_args(&sb), ["--email", "typed@corp.example", "--sso"]);

    sb.mcc()
        .args(["add", "side", "--fresh", "--sso"])
        .assert()
        .success();
    assert_eq!(login_args(&sb), ["--sso"]);
}

#[test]
fn email_is_passed_when_creating_the_default_profile() {
    let sb = Sandbox::new();
    sb.mcc()
        .args(["add", "--yes", "--email=me@example.com"])
        .assert()
        .success();
    assert_eq!(login_args(&sb), ["--email", "me@example.com"]);
}

const LOG: &str = ".mcc/logs/mcc.log";

/// Runs `mcc add ARGS`, expecting exit CODE, and returns what it appended to
/// mcc.log.
fn add_log_lines(sb: &Sandbox, args: &[&str], code: i32) -> String {
    let before = if sb.exists(LOG) {
        sb.read(LOG)
    } else {
        String::new()
    };
    sb.mcc().arg("add").args(args).assert().code(code);
    let after = sb.read(LOG);
    assert!(after.starts_with(&before), "{after}");
    after[before.len()..].to_string()
}

#[test]
fn the_email_value_is_redacted_from_the_log() {
    let sb = Sandbox::new();
    sb.seed_default();
    let added = add_log_lines(&sb, &["work", "--fresh", "--email", "a@b.c"], 0);
    assert!(added.contains(r#""--email", "<redacted>""#), "{added}");
    // claude still gets the address itself.
    assert_eq!(login_args(&sb), ["--email", "a@b.c"]);

    // A value --email refuses is logged before it is checked: redacted too.
    let added = add_log_lines(&sb, &["lab", "--fresh", "--email", "leakmarker@"], 2);
    assert!(added.contains(r#""--email", "<redacted>""#), "{added}");

    let log = sb.read(LOG);
    assert!(!log.contains("a@b.c"), "{log}");
    assert!(!log.contains("leakmarker"), "{log}");
}

#[test]
fn the_email_value_after_an_equals_sign_is_redacted_from_the_log() {
    let sb = Sandbox::new();
    sb.seed_default();
    let added = add_log_lines(&sb, &["work", "--fresh", "--email=a@b.c"], 0);
    assert!(added.contains(r#""--email=<redacted>""#), "{added}");
    assert_eq!(login_args(&sb), ["--email", "a@b.c"]);

    let added = add_log_lines(&sb, &["lab", "--fresh", "--email=leakmarker@"], 2);
    assert!(added.contains(r#""--email=<redacted>""#), "{added}");

    let log = sb.read(LOG);
    assert!(!log.contains("a@b.c"), "{log}");
    assert!(!log.contains("leakmarker"), "{log}");
}

#[test]
fn rejects_an_invalid_email_and_login_flags_without_login() {
    let sb = Sandbox::new();
    sb.seed_default();
    for bad in [
        "",
        "not-an-email",
        "a b@c.d",
        "-x@c.d",
        "a@b@c",
        "@c.d",
        "a@",
    ] {
        sb.mcc()
            .args(["add", "work", "--fresh", &format!("--email={bad}")])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("--email"));
    }
    for flags in [&["--email", "a@b.c"][..], &["--sso"][..]] {
        sb.mcc()
            .args(["add", "work", "--fresh", "--no-login"])
            .args(flags)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("cannot be used with"));
    }
    // Nothing was created, and claude was never asked to log in.
    assert!(!sb.exists(".mcc/profiles/work"));
    assert!(!sb.exists("login-args"));
}

#[test]
fn warns_on_a_console_login_without_blocking() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--fresh"])
        .env("FAKE_AUTH_METHOD", "api_key")
        .env("FAKE_API_KEY_SOURCE", "/login managed key")
        .assert()
        .success()
        .stderr(predicate::str::contains("Anthropic Console account"))
        .stderr(predicate::str::contains("work auth login --claudeai"))
        .stderr(predicate::str::contains("profile \"work\" is ready"));
    assert!(account_section(&sb, "work").contains("fake@example.com"));

    // A Claude subscription, or an API key from the environment: no warning.
    sb.mcc()
        .args(["add", "lab", "--fresh"])
        .env("FAKE_EMAIL", "lab@example.com")
        .assert()
        .success()
        .stderr(predicate::str::contains("Console").not());
    sb.mcc()
        .args(["add", "side", "--fresh"])
        .env("FAKE_EMAIL", "side@example.com")
        .env("FAKE_AUTH_METHOD", "api_key")
        .env("FAKE_API_KEY_SOURCE", "ANTHROPIC_API_KEY")
        .assert()
        .success()
        .stderr(predicate::str::contains("Console").not());
}

#[test]
fn default_profile_warns_on_a_console_login() {
    let sb = Sandbox::new();
    sb.mcc()
        .args(["add", "--yes"])
        .env("FAKE_AUTH_METHOD", "api_key")
        .env("FAKE_API_KEY_SOURCE", "/login managed key")
        .assert()
        .success()
        .stderr(predicate::str::contains("Anthropic Console account"))
        .stderr(predicate::str::contains("`claude auth login --claudeai`"));
}
