//! `mcc doctor`: read-only checks of every profile, each problem with a fix.

mod common;

use std::process::Output;

use common::{HOUR_MS, Sandbox, enable_live_usage, json, now_ms, with_profile};
use predicates::prelude::*;

fn out(o: &[u8]) -> String {
    String::from_utf8_lossy(o).into_owned()
}

/// `mcc doctor ARGS`, with Claude Code reporting `configDirectory`.
fn doctor(sb: &Sandbox, args: &[&str]) -> Output {
    sb.mcc()
        .env("FAKE_CONFIG_DIRECTORY", "1")
        .arg("doctor")
        .args(args)
        .output()
        .unwrap()
}

/// `mcc doctor` fails (exit 1); its stderr.
fn failing(sb: &Sandbox) -> String {
    let o = doctor(sb, &[]);
    let err = out(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    err
}

/// `mcc doctor` succeeds; its stderr.
fn passing(sb: &Sandbox) -> String {
    let o = doctor(sb, &[]);
    let err = out(&o.stderr);
    assert!(o.status.success(), "{err}");
    err
}

/// The `--json` findings.
fn findings(sb: &Sandbox, args: &[&str]) -> Vec<serde_json::Value> {
    let o = doctor(sb, &[&["--json"], args].concat());
    json(&o.stdout).as_array().expect("a bare array").clone()
}

/// The findings of `check` for `profile` (`None`: the setup checks).
fn of<'a>(
    all: &'a [serde_json::Value],
    profile: Option<&str>,
    check: &str,
) -> Vec<&'a serde_json::Value> {
    all.iter()
        .filter(|f| f["check"] == check && f["profile"].as_str() == profile)
        .collect()
}

/// The one finding of `check` for `profile`.
fn one<'a>(
    all: &'a [serde_json::Value],
    profile: Option<&str>,
    check: &str,
) -> &'a serde_json::Value {
    let found = of(all, profile, check);
    assert_eq!(found.len(), 1, "{check} for {profile:?}: {all:#?}");
    found[0]
}

/// Default + `work` (cloned, logged in) + `lab` (fresh, logged in).
fn healthy() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--fresh");
    sb
}

fn set_mode(sb: &Sandbox, rel: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(sb.path(rel), std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_healthy_setup_passes_and_nothing_is_written() {
    let sb = healthy();
    let config = sb.read(".mcc/config.toml");
    let zshrc = sb.read(".zshrc");
    let err = passing(&sb);
    assert!(err.contains("no problems found"), "{err}");
    for header in ["Profile \"default\"", "Profile \"work\"", "Profile \"lab\""] {
        assert!(err.contains(header), "{err}");
    }
    // The seeded `alias cus_luong='CLAUDE_CONFIG_DIR=… claude'` is not global.
    assert!(
        err.contains("no rc file sets CLAUDE_CONFIG_DIR globally"),
        "{err}"
    );
    assert!(
        err.contains(
            "Claude Code uses ~/.mcc/profiles/work (through its launcher ~/.local/bin/work)"
        ),
        "{err}"
    );
    assert_eq!(sb.read(".mcc/config.toml"), config);
    assert_eq!(sb.read(".zshrc"), zshrc);

    let all = findings(&sb, &[]);
    assert!(
        all.iter()
            .all(|f| f["status"] != "problem" && f["status"] != "warn"),
        "{all:#?}"
    );
    assert_eq!(one(&all, Some("lab"), "drift")["status"], "skipped");
    assert_eq!(one(&all, Some("work"), "drift")["status"], "ok");
}

#[test]
fn first_use_is_reported_not_set_up() {
    let sb = Sandbox::new();
    sb.seed_default();
    let err = failing(&sb);
    assert!(err.contains("mcc is not set up yet"), "{err}");
    assert!(err.contains("fix: `mcc init`"), "{err}");
    assert!(!err.contains("First run"), "{err}");
    // Read-only: no config, no log, no ~/.mcc at all.
    assert!(!sb.exists(".mcc"), "doctor created ~/.mcc");
}

#[test]
fn a_missing_launcher_is_fixed_by_refresh_launchers() {
    let sb = healthy();
    std::fs::remove_file(sb.path(".local/bin/work")).unwrap();
    let err = failing(&sb);
    assert!(
        err.contains("the launcher ~/.local/bin/work is missing"),
        "{err}"
    );
    assert!(err.contains("fix: `mcc refresh-launchers work`"), "{err}");
    sb.mcc()
        .args(["refresh-launchers", "work"])
        .assert()
        .success();
    passing(&sb);
}

#[test]
fn launchers_mcc_did_not_write_or_cannot_run_are_problems() {
    let sb = healthy();
    sb.write(".local/bin/work", "#!/bin/sh\necho mine\n");
    set_mode(&sb, ".local/bin/lab", 0o644);
    let err = failing(&sb);
    assert!(
        err.contains("~/.local/bin/work is not a launcher written by mcc"),
        "{err}"
    );
    assert!(
        err.contains(
            "fix: move ~/.local/bin/work out of the way, then run `mcc refresh-launchers work`"
        ),
        "{err}"
    );
    assert!(
        err.contains("the launcher ~/.local/bin/lab is not executable"),
        "{err}"
    );
    assert!(err.contains("fix: `chmod +x ~/.local/bin/lab`"), "{err}");
}

#[test]
fn a_launcher_pointing_elsewhere_and_its_config_directory_mismatch() {
    let sb = healthy();
    let launcher = sb.read(".local/bin/work");
    sb.write(
        ".local/bin/work",
        &launcher.replace(
            "export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\"",
            "export CLAUDE_CONFIG_DIR=\"$HOME/.claude-old\"",
        ),
    );
    let err = failing(&sb);
    assert!(
        err.contains("doesn't start the profile's directory ~/.mcc/profiles/work"),
        "{err}"
    );
    assert!(
        err.contains("Claude Code uses ~/.claude-old through its launcher ~/.local/bin/work, but the profile's directory is ~/.mcc/profiles/work"),
        "{err}"
    );
    let all = findings(&sb, &[]);
    let cd = one(&all, Some("work"), "config_dir");
    assert_eq!(cd["status"], "problem");
    assert_eq!(cd["fix"], "mcc refresh-launchers work");
    sb.mcc()
        .args(["refresh-launchers", "work"])
        .assert()
        .success();
    passing(&sb);
}

#[test]
fn config_directory_is_skipped_on_older_claude_code() {
    let sb = healthy();
    // Without FAKE_CONFIG_DIRECTORY, like Claude Code before 2.1.268.
    let o = sb.mcc().args(["doctor", "--json"]).output().unwrap();
    assert!(o.status.success(), "{}", out(&o.stderr));
    let all = json(&o.stdout);
    let cd = one(all.as_array().unwrap(), Some("work"), "config_dir");
    assert_eq!(cd["status"], "skipped");
    let msg = cd["message"].as_str().unwrap();
    assert!(msg.contains("9.9.9 (Claude Code)"), "{msg}");
    assert!(msg.contains("2.1.268"), "{msg}");
}

#[test]
fn a_missing_alias_block_prints_the_block_to_add() {
    let sb = healthy();
    let zshrc = sb.read(".zshrc");
    let start = zshrc.find("# >>> mcc:lab >>>").unwrap();
    let end = zshrc.find("# <<< mcc:lab <<<").unwrap() + "# <<< mcc:lab <<<".len();
    sb.write(".zshrc", &format!("{}{}", &zshrc[..start], &zshrc[end..]));
    let err = failing(&sb);
    assert!(
        err.contains("the alias block for `lab` is missing from ~/.zshrc"),
        "{err}"
    );
    assert!(err.contains("fix: add these lines to ~/.zshrc"), "{err}");
    assert!(err.contains("        # >>> mcc:lab >>>"), "{err}");
    assert!(
        err.contains("        alias lab='\"$HOME/.local/bin/lab\"'"),
        "{err}"
    );
}

#[test]
fn local_bin_off_path_is_a_problem() {
    let sb = healthy();
    let path = format!("{}:/usr/bin:/bin", sb.path("fakebin").display());
    let o = sb.mcc().env("PATH", path).arg("doctor").output().unwrap();
    let err = out(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("~/.local/bin (where profile launchers live) is not on PATH"),
        "{err}"
    );
    assert!(err.contains("run `mcc init --force`"), "{err}");
    assert!(
        err.contains("export PATH=\"$HOME/.local/bin:$PATH\""),
        "{err}"
    );
}

#[test]
fn a_logged_out_profile_is_fixed_by_logging_in() {
    let sb = healthy();
    sb.mcc()
        .args(["add", "side", "--fresh", "--no-login"])
        .assert()
        .success();
    let err = failing(&sb);
    assert!(err.contains("not logged in"), "{err}");
    assert!(
        err.contains("fix: `mcc exec side -- claude auth login`"),
        "{err}"
    );
    sb.mcc()
        .args(["exec", "side", "--", "claude", "auth", "login"])
        .assert()
        .success();
    passing(&sb);
}

#[test]
fn profile_dir_mode_must_be_private() {
    let sb = healthy();
    set_mode(&sb, ".mcc/profiles/work", 0o755);
    let err = failing(&sb);
    assert!(err.contains("~/.mcc/profiles/work has mode 0755"), "{err}");
    assert!(
        err.contains("fix: `chmod 700 ~/.mcc/profiles/work`"),
        "{err}"
    );
}

#[test]
fn a_missing_profile_dir_is_fixed_by_delete() {
    let sb = healthy();
    std::fs::remove_dir_all(sb.path(".mcc/profiles/lab")).unwrap();
    let err = failing(&sb);
    assert!(
        err.contains("the profile's directory ~/.mcc/profiles/lab is missing"),
        "{err}"
    );
    assert!(err.contains("fix: `mcc delete lab`"), "{err}");
}

fn share(sb: &Sandbox, args: &[&str]) {
    sb.mcc().arg("share").args(args).assert().success();
}

#[test]
fn shared_profiles_and_groups_are_healthy() {
    let sb = healthy();
    share(&sb, &["work"]);
    share(&sb, &["lab", "--group", "acme"]);
    let all = findings(&sb, &[]);
    for p in ["work", "lab"] {
        assert_eq!(one(&all, Some(p), "share")["status"], "ok", "{all:#?}");
    }
    assert!(
        one(&all, Some("lab"), "share")["message"]
            .as_str()
            .unwrap()
            .contains("group \"acme\"")
    );
    passing(&sb);
}

#[test]
fn a_dangling_share_link_is_fixed_by_share() {
    let sb = healthy();
    share(&sb, &["work"]);
    std::fs::remove_dir_all(sb.path(".mcc/shared/projects")).unwrap();
    let err = failing(&sb);
    assert!(err.contains("which is missing"), "{err}");
    assert!(err.contains("fix: `mcc share work`"), "{err}");
    share(&sb, &["work"]);
    passing(&sb);
}

#[test]
fn share_flag_and_link_disagreeing_is_a_problem() {
    let sb = healthy();
    share(&sb, &["work"]);
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("share_sessions = true"), "{cfg}");
    sb.write(
        ".mcc/config.toml",
        &cfg.replacen("share_sessions = true", "share_sessions = false", 1),
    );
    let err = failing(&sb);
    assert!(err.contains("but sharing is off"), "{err}");
    assert!(
        err.contains("run `mcc share work --off` to give the profile its own copy, or `mcc share work` to keep sharing"),
        "{err}"
    );
    share(&sb, &["work", "--off"]);
    passing(&sb);

    // The other way round: the flag says sharing, the link is gone.
    share(&sb, &["lab"]);
    std::fs::remove_file(sb.path(".mcc/profiles/lab/projects")).unwrap();
    let err = failing(&sb);
    assert!(
        err.contains(
            "sharing is on in ~/.mcc/config.toml, but ~/.mcc/profiles/lab/projects doesn't link"
        ),
        "{err}"
    );
    share(&sb, &["lab"]);
    passing(&sb);
}

#[test]
fn share_leftovers_are_reported_with_their_fix() {
    let sb = healthy();
    sb.write(
        ".mcc/profiles/work/.projects.mcc-share-4242/-tmp-p/s.jsonl",
        "{}\n",
    );
    let err = failing(&sb);
    assert!(
        err.contains(
            "~/.mcc/profiles/work/.projects.mcc-share-4242 was left by an interrupted `mcc share`"
        ),
        "{err}"
    );
    assert!(
        err.contains("fix: move its contents into ~/.mcc/profiles/work/projects, then remove it"),
        "{err}"
    );

    // A sharing profile: `mcc share` merges it into the store.
    share(&sb, &["lab"]);
    sb.write(
        ".mcc/profiles/lab/.projects.mcc-share-77/-tmp-p/s.jsonl",
        "{}\n",
    );
    let err = failing(&sb);
    assert!(err.contains("fix: `mcc share lab`"), "{err}");
    share(&sb, &["lab"]);
    assert!(!sb.exists(".mcc/profiles/lab/.projects.mcc-share-77"));

    sb.write(".mcc/profiles/lab/.projects.mcc-unshare-9/x", "x");
    let err = failing(&sb);
    assert!(err.contains("interrupted `mcc share --off`"), "{err}");
    assert!(
        err.contains("fix: `rm -r ~/.mcc/profiles/lab/.projects.mcc-unshare-9`"),
        "{err}"
    );
}

#[test]
fn drift_is_a_warning_with_the_sync_command() {
    let sb = healthy();
    sb.write(".claude/skills/gamma/SKILL.md", "gamma");
    let err = passing(&sb);
    assert!(err.contains("1 behind the default"), "{err}");
    assert!(err.contains("fix: `mcc sync work`"), "{err}");
    assert!(err.contains("no problems; 1 warning"), "{err}");

    // Changed on both sides: a conflict, which only --force overwrites.
    sb.mcc().args(["sync", "work", "--yes"]).assert().success();
    sb.write(".claude/skills/gamma/SKILL.md", "gamma 2");
    sb.write(".mcc/profiles/work/skills/gamma/SKILL.md", "mine");
    let err = passing(&sb);
    assert!(err.contains("1 conflict"), "{err}");
    assert!(err.contains("fix: `mcc sync work --force`"), "{err}");

    // Local edits alone are kept by design: no warning.
    sb.mcc()
        .args(["sync", "work", "--force", "--yes"])
        .assert()
        .success();
    sb.write(".mcc/profiles/work/skills/gamma/SKILL.md", "mine again");
    let all = findings(&sb, &[]);
    let drift = one(&all, Some("work"), "drift");
    assert_eq!(drift["status"], "ok", "{drift}");
    assert!(drift["message"].as_str().unwrap().contains("1 local edit"));
}

#[test]
fn overlays_links_and_shared_plugins_are_not_drift() {
    let sb = Sandbox::new();
    sb.seed_default();
    // Clones share the default's plugins (#47); link skills (#46) too.
    sb.mcc()
        .args(["add", "work", "--clone", "--link", "skills"])
        .assert()
        .success();
    sb.write(
        ".mcc/profiles/work/settings.mcc.json",
        r#"{"model":"sonnet"}"#,
    );
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &format!("{cfg}\n[profiles.work.settings]\ntheme = \"light\"\n"),
    );
    sb.mcc().args(["sync", "work", "--yes"]).assert().success();
    sb.write(".claude/skills/gamma/SKILL.md", "gamma");
    let all = findings(&sb, &[]);
    assert_eq!(one(&all, Some("work"), "drift")["status"], "ok", "{all:#?}");
    for f in of(&all, Some("work"), "launcher") {
        assert_eq!(f["status"], "ok", "{f}");
    }
    passing(&sb);
}

#[test]
fn a_launcher_without_the_shared_plugins_root_is_a_problem() {
    let sb = healthy();
    let launcher = sb.read(".local/bin/work");
    let kept: Vec<&str> = launcher
        .lines()
        .filter(|l| !l.starts_with("export CLAUDE_CODE_PLUGIN_CACHE_DIR="))
        .collect();
    assert!(kept.len() < launcher.lines().count(), "{launcher}");
    sb.write(".local/bin/work", &(kept.join("\n") + "\n"));
    let err = failing(&sb);
    assert!(
        err.contains("doesn't point Claude Code at the default's plugins"),
        "{err}"
    );
    assert!(err.contains("fix: `mcc refresh-launchers work`"), "{err}");
}

#[test]
fn a_global_claude_config_dir_in_an_rc_file_is_found() {
    let sb = healthy();
    let zshrc = sb.read(".zshrc");
    sb.write(
        ".zshrc",
        &format!(
            "{zshrc}# export CLAUDE_CONFIG_DIR=~/.commented\n# >>> mcc:other >>>\nexport CLAUDE_CONFIG_DIR=~/.in-block\n# <<< mcc:other <<<\nexport CLAUDE_CONFIG_DIR=~/.claude-work\n"
        ),
    );
    let line = sb.read(".zshrc").lines().count();
    sb.write(".bashrc", "CLAUDE_CONFIG_DIR=/x claude\n");
    let err = failing(&sb);
    assert!(
        err.contains(&format!(
            "~/.zshrc:{line} sets CLAUDE_CONFIG_DIR for every shell (`export CLAUDE_CONFIG_DIR=~/.claude-work`)"
        )),
        "{err}"
    );
    assert!(
        err.contains(&format!("fix: remove line {line} from ~/.zshrc")),
        "{err}"
    );
    let all = findings(&sb, &[]);
    let hits = of(&all, None, "rc_config_dir");
    assert_eq!(hits.len(), 1, "only the real export: {hits:#?}");
}

#[test]
fn claude_config_dir_in_this_shell_is_a_warning() {
    let sb = healthy();
    let o = sb
        .mcc()
        .env("CLAUDE_CONFIG_DIR", sb.path(".mcc/profiles/work"))
        .arg("doctor")
        .output()
        .unwrap();
    let err = out(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("CLAUDE_CONFIG_DIR is set in this shell (~/.mcc/profiles/work)"),
        "{err}"
    );
    assert!(err.contains("fix: `unset CLAUDE_CONFIG_DIR`"), "{err}");
}

#[test]
fn claude_config_dir_in_a_fish_shell_is_cleared_with_set_e() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .env("SHELL", "/usr/bin/fish")
        .args(["add", "work", "--clone"])
        .assert()
        .success();
    assert!(
        sb.read(".mcc/config.toml").contains("fish"),
        "set up for fish"
    );
    let o = sb
        .mcc()
        .env("SHELL", "/usr/bin/fish")
        .env("CLAUDE_CONFIG_DIR", sb.path(".mcc/profiles/work"))
        .arg("doctor")
        .output()
        .unwrap();
    let err = out(&o.stderr);
    assert!(
        err.contains("CLAUDE_CONFIG_DIR is set in this shell (~/.mcc/profiles/work)"),
        "{err}"
    );
    assert!(err.contains("after `mcc env | source`"), "{err}");
    assert!(err.contains("fix: `set -e CLAUDE_CONFIG_DIR`"), "{err}");
    assert!(!err.contains("unset CLAUDE_CONFIG_DIR"), "{err}");
}

#[test]
fn a_global_claude_config_dir_in_config_fish_points_at_mcc_env_source() {
    let sb = healthy();
    sb.write(
        ".config/fish/config.fish",
        "set -gx CLAUDE_CONFIG_DIR ~/.claude-work\n",
    );
    let err = failing(&sb);
    assert!(
        err.contains("fix: remove line 1 from ~/.config/fish/config.fish (start profiles with their alias, `mcc run`, or `mcc env <alias> | source` instead)"),
        "{err}"
    );
}

#[test]
fn token_expiry_is_not_checked_without_live_usage() {
    let sb = healthy();
    sb.seed_oauth(".mcc/profiles/work", "test-token-work", now_ms() - HOUR_MS);
    let o = doctor(&sb, &["--json"]);
    assert!(o.status.success(), "{}", out(&o.stderr));
    let all = json(&o.stdout);
    let token = one(all.as_array().unwrap(), Some("work"), "token");
    assert_eq!(token["status"], "skipped", "{token}");
    assert!(
        token["message"]
            .as_str()
            .unwrap()
            .contains("`[usage] live = true`")
    );
    // Nothing asked the keychain, and no token reached the output.
    assert!(!sb.exists("security.log"), "the keychain was queried");
    assert!(!out(&o.stdout).contains("test-token-work"));
    assert!(!out(&o.stderr).contains("test-token-work"));

    // MCC_NO_USAGE wins over live = true.
    enable_live_usage(&sb);
    let o = sb
        .mcc()
        .env("MCC_NO_USAGE", "1")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let all = json(&o.stdout);
    assert_eq!(
        one(all.as_array().unwrap(), Some("work"), "token")["status"],
        "skipped"
    );
    assert!(!sb.exists("security.log"), "the keychain was queried");
}

#[test]
fn token_expiry_is_reported_with_live_usage() {
    let sb = healthy();
    sb.seed_oauth(".mcc/profiles/work", "test-token-work", now_ms() - HOUR_MS);
    sb.seed_oauth(".mcc/profiles/lab", "test-token-lab", now_ms() + HOUR_MS);
    enable_live_usage(&sb);
    let config = sb.read(".mcc/config.toml");
    let o = doctor(&sb, &[]);
    let err = out(&o.stderr);
    assert!(o.status.success(), "an expired token is a warning: {err}");
    assert!(err.contains("the OAuth access token has expired"), "{err}");
    assert!(
        err.contains("fix: `mcc exec work -- claude auth login`"),
        "{err}"
    );
    assert!(!err.contains("test-token"), "{err}");
    // No disclosure recorded: doctor writes nothing.
    assert_eq!(sb.read(".mcc/config.toml"), config);
    let all = findings(&sb, &[]);
    assert_eq!(one(&all, Some("lab"), "token")["status"], "ok");
}

#[test]
fn the_statusline_shim_is_expected_in_every_profile() {
    let sb = healthy();
    // Not installed: nothing to check.
    assert!(of(&findings(&sb, &[]), Some("work"), "statusline").is_empty());

    sb.write(
        ".claude/settings.json",
        r#"{"model":"opus","statusLine":{"type":"command","command":"/usr/local/bin/mcc statusline"}}"#,
    );
    let err = passing(&sb);
    assert!(
        err.contains("the mcc statusline is installed in the default profile but not in ~/.mcc/profiles/work/settings.json"),
        "{err}"
    );
    assert!(
        err.contains("fix: `mcc sync work --only settings`"),
        "{err}"
    );

    sb.mcc()
        .args(["sync", "work", "--only", "settings", "--yes"])
        .assert()
        .success();
    let all = findings(&sb, &[]);
    assert_eq!(one(&all, Some("work"), "statusline")["status"], "ok");

    // A statusLine of the profile's own overlay is not overridden.
    sb.write(
        ".mcc/profiles/lab/settings.mcc.json",
        r#"{"statusLine":{"type":"command","command":"echo lab"}}"#,
    );
    let all = findings(&sb, &[]);
    let lab = one(&all, Some("lab"), "statusline");
    assert_eq!(lab["status"], "ok", "{lab}");
    assert!(lab["message"].as_str().unwrap().contains("overlay"));
}

#[test]
fn a_statusline_blocked_by_a_settings_conflict_is_fixed_with_force() {
    let sb = healthy();
    // Edited in the profile since its last sync…
    sb.write(".mcc/profiles/work/settings.json", r#"{"model":"sonnet"}"#);
    // …and changed in the default by `mcc statusline install`.
    sb.write(
        ".claude/settings.json",
        r#"{"model":"opus","statusLine":{"type":"command","command":"/usr/local/bin/mcc statusline"}}"#,
    );
    let err = passing(&sb);
    assert!(
        err.contains("fix: `mcc sync work --only settings --force`"),
        "{err}"
    );
    assert!(err.contains("--force backs it up first"), "{err}");

    // The plain sync would skip it; the printed fix clears the warning.
    sb.mcc()
        .args(["sync", "work", "--only", "settings", "--yes"])
        .assert()
        .success();
    let all = findings(&sb, &[]);
    assert_eq!(one(&all, Some("work"), "statusline")["status"], "warn");
    sb.mcc()
        .args(["sync", "work", "--only", "settings", "--force", "--yes"])
        .assert()
        .success();
    let all = findings(&sb, &[]);
    assert_eq!(one(&all, Some("work"), "statusline")["status"], "ok");
}

#[test]
fn json_lists_every_finding_and_json_v2_wraps_it() {
    let sb = healthy();
    std::fs::remove_file(sb.path(".local/bin/work")).unwrap();
    let o = doctor(&sb, &["--json"]);
    assert_eq!(o.status.code(), Some(1), "problems still fail the command");
    let bare = json(&o.stdout);
    let all = bare.as_array().unwrap();
    for f in all {
        let keys: Vec<&str> = f.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 5, "{f}");
        for k in ["profile", "check", "status", "message", "fix"] {
            assert!(keys.contains(&k), "{f}");
        }
        assert!(
            ["ok", "warn", "problem", "skipped"].contains(&f["status"].as_str().unwrap()),
            "{f}"
        );
    }
    let launcher = one(all, Some("work"), "launcher");
    assert_eq!(launcher["status"], "problem");
    assert_eq!(launcher["fix"], "mcc refresh-launchers work");
    assert_eq!(one(all, None, "path")["fix"], serde_json::Value::Null);

    let o = doctor(&sb, &["--json-v2"]);
    assert_eq!(o.status.code(), Some(1));
    let v = json(&o.stdout);
    assert_eq!(v["schema"], 2);
    assert_eq!(v["checks"], bare);
}

#[test]
fn one_profile_only() {
    let sb = healthy();
    let all = findings(&sb, &["work"]);
    assert!(
        all.iter()
            .all(|f| f["profile"].is_null() || f["profile"] == "work")
    );
    assert!(!of(&all, Some("work"), "launcher").is_empty());
    let all = findings(&sb, &["default"]);
    assert!(
        all.iter()
            .all(|f| f["profile"].is_null() || f["profile"] == "default")
    );
    assert_eq!(one(&all, Some("default"), "login")["status"], "ok");

    sb.mcc()
        .args(["doctor", "ghost"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"ghost\""));
}
