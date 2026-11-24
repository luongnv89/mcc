//! API-key profiles (#132): `mcc add --api-key-…`, what they never leak,
//! how they start, the commands that refuse them, their spend estimate, and
//! how sync, rename and delete treat them.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::{Sandbox, json, stdout_of, with_profile};
use predicates::prelude::*;

/// A dummy key: shaped enough for mcc's checks, never a real one.
const KEY: &str = "sk-ant-test-fixture";
const OTHER_KEY: &str = "sk-ant-test-second";

fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    sb
}

/// `mcc add ALIAS MODE --api-key-stdin -y EXTRA…` with `key` piped in.
fn add_managed(
    sb: &Sandbox,
    alias: &str,
    mode: &str,
    key: &str,
    extra: &[&str],
) -> assert_cmd::assert::Assert {
    sb.mcc()
        .args(["add", alias, mode, "--api-key-stdin", "-y"])
        .args(extra)
        .write_stdin(format!("{key}\n"))
        .assert()
}

fn settings(sb: &Sandbox, dir: &str) -> serde_json::Value {
    json(sb.read(&format!("{dir}/settings.json")).as_bytes())
}

/// The one key file under ~/.mcc/keys.
fn key_file(sb: &Sandbox) -> std::path::PathBuf {
    let files: Vec<_> = std::fs::read_dir(sb.path(".mcc/keys"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    files[0].clone()
}

fn mode(p: &std::path::Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// The `claude` runs so far (first two arguments each).
fn claude_calls(sb: &Sandbox) -> String {
    std::fs::read_to_string(sb.path("claude-calls.log")).unwrap_or_default()
}

fn log(sb: &Sandbox) -> String {
    std::fs::read_to_string(sb.path(".mcc/logs/mcc.log")).unwrap_or_default()
}

/// AC1 + T1: the key lands in one 0600 file in a 0700 directory, named
/// after the profile's directory, and nowhere else: not in the output, the
/// log or config.toml. The profile is registered as an API-key profile
/// with its budget, no sign-in runs, and Claude Code's first-run setup is
/// marked done.
#[test]
fn a_managed_key_is_stored_once_and_never_shown() {
    let sb = sandbox();
    let out = add_managed(&sb, "api", "--fresh", KEY, &["--budget", "50"]).success();
    let out = out.get_output();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        stderr.contains("API-key profile \"api\" is ready"),
        "{stderr}"
    );
    let key = key_file(&sb);
    assert_eq!(std::fs::read_to_string(&key).unwrap(), KEY);
    assert_eq!(mode(&key), 0o600);
    assert_eq!(mode(&sb.path(".mcc/keys")), 0o700);
    let name = key.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("api-") && name.ends_with(".key"), "{name}");
    let s = settings(&sb, ".mcc/profiles/api");
    assert_eq!(s["apiKeyHelper"], format!("cat '{}'", key.display()), "{s}");
    let config = sb.read(".mcc/config.toml");
    assert!(config.contains("auth = \"api_key\""), "{config}");
    assert!(config.contains("api_budget_usd = 50.0"), "{config}");
    for (what, text) in [
        ("stdout", stdout.as_ref()),
        ("stderr", stderr.as_ref()),
        ("mcc.log", log(&sb).as_str()),
        ("config.toml", config.as_str()),
        ("settings.json", &s.to_string()),
    ] {
        assert!(!text.contains(KEY), "the key is in {what}: {text}");
    }
    assert!(
        !claude_calls(&sb).contains("auth login"),
        "{}",
        claude_calls(&sb)
    );
    assert_eq!(
        json(sb.read(".mcc/profiles/api/.claude.json").as_bytes())["hasCompletedOnboarding"],
        true
    );
    // Claude Code sees an apiKeyHelper login there (P5).
    let status = sb
        .command(sb.path("fakebin/claude"))
        .env("CLAUDE_CONFIG_DIR", sb.path(".mcc/profiles/api"))
        .args(["auth", "status"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("\"api_key_helper\""),
        "{status:?}"
    );
}

/// Helper mode: the user's command becomes apiKeyHelper, mcc stores no key
/// and never prints or logs the command.
#[test]
fn a_helper_command_is_set_and_never_shown() {
    let sb = sandbox();
    let helper = "op read op://Private/anthropic/key";
    let out = sb
        .mcc()
        .args(["add", "api", "--clone", "--api-key-helper", helper, "-y"])
        .assert()
        .success();
    let out = out.get_output();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("op read"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("apiKeyHelper set"), "{stderr}");
    assert!(!stderr.contains("op read"), "{stderr}");
    assert!(!log(&sb).contains("op read"), "{}", log(&sb));
    assert_eq!(settings(&sb, ".mcc/profiles/api")["apiKeyHelper"], helper);
    assert!(!sb.exists(".mcc/keys"));
}

/// B3/B18: a key on the command line, after a flag that takes no value
/// (space or `=`), or as the alias, is refused without being repeated on
/// stderr or in mcc.log, and nothing is created.
#[test]
fn a_key_on_the_command_line_is_refused_without_an_echo() {
    let sb = sandbox();
    for args in [
        vec!["add", "api", "--api-key-prompt", KEY],
        vec!["add", "api", &format!("--api-key-stdin={KEY}")],
        vec!["add", KEY, "--api-key-stdin"],
        vec!["add", "api", "--api-key-helper", &format!("echo {KEY}")],
    ] {
        let out = sb.mcc().args(&args).write_stdin("").assert().code(2);
        let out = out.get_output();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains(KEY), "{args:?}: {stderr}");
        assert!(!String::from_utf8_lossy(&out.stdout).contains(KEY));
        assert!(!log(&sb).contains(KEY), "{args:?}: {}", log(&sb));
    }
    let out = sb
        .mcc()
        .args(["add", KEY, "--api-key-stdin"])
        .write_stdin(format!("{KEY}\n"))
        .assert()
        .code(2);
    assert!(String::from_utf8_lossy(&out.get_output().stderr).contains(
        "that looks like an API key; pass keys with --api-key-stdin or --api-key-prompt"
    ));
    assert!(!sb.exists(".mcc/profiles"));
    assert!(!sb.exists(".mcc/keys"));

    // Without ~/.claude, `mcc add` would create the default profile: the
    // key-shaped name is refused before that, never printed as an alias.
    let fresh = Sandbox::new();
    let out = fresh.mcc().args(["add", KEY, "--yes"]).assert().code(2);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(stderr.contains("that looks like an API key"), "{stderr}");
    assert!(!stderr.contains(KEY), "{stderr}");
    assert!(!fresh.exists(".claude"));
}

/// The hidden prompt needs a terminal; stdin needs a key; neither is a
/// way to create the default profile; --budget needs an API key and a
/// number of dollars.
#[test]
fn api_key_flags_are_checked_before_anything_is_created() {
    let sb = sandbox();
    sb.mcc()
        .args(["add", "api", "--fresh", "--api-key-prompt"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--api-key-prompt needs a terminal",
        ))
        .stderr(predicate::str::contains("--api-key-stdin"));
    add_managed(&sb, "api", "--fresh", "", &[])
        .code(2)
        .stderr(predicate::str::contains("the API key is empty"));
    sb.mcc()
        .args(["add", "api", "--fresh", "--budget", "5", "-y"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--budget is for API-key profiles"));
    for bad in ["-1", "nan", "inf", "lots"] {
        sb.mcc()
            .args([
                "add",
                "api",
                "--fresh",
                "--api-key-stdin",
                &format!("--budget={bad}"),
            ])
            .write_stdin(format!("{KEY}\n"))
            .assert()
            .code(2)
            .stderr(predicate::str::contains("number of US dollars, 0 or more"));
    }
    assert!(!sb.exists(".mcc/profiles/api"));
    assert!(!sb.exists(".mcc/keys"));

    // One way to give the key, and none of the sign-in flags with it.
    for args in [
        &["--api-key-stdin", "--api-key-prompt"][..],
        &["--api-key-stdin", "--api-key-helper", "op read x"][..],
        &["--api-key-prompt", "--email", "me@example.com"][..],
        &["--api-key-helper", "op read x", "--sso"][..],
        &["--api-key-stdin", "--no-login"][..],
    ] {
        sb.mcc()
            .args(["add", "api", "--fresh"])
            .args(args)
            .write_stdin(format!("{KEY}\n"))
            .assert()
            .code(2)
            .stderr(predicate::str::contains("cannot be used with"));
    }
    assert!(!sb.exists(".mcc/profiles/api"));

    let fresh = Sandbox::new();
    fresh
        .mcc()
        .args(["add", "--api-key-stdin"])
        .write_stdin(format!("{KEY}\n"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "create the default profile first (`mcc add` with no API-key flag)",
        ));
    assert!(!fresh.exists(".claude"));
}

/// B14: the launcher, `mcc run`, `mcc exec` and `mcc env` keep the shell's
/// ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN from an API-key profile;
/// subscription profiles' launchers and `mcc env` are as before.
#[test]
fn api_key_profiles_start_without_the_shells_key() {
    let sb = sandbox();
    with_profile(&sb, "work", "--clone");
    add_managed(&sb, "api", "--fresh", KEY, &[]).success();
    let launcher = sb.read(".local/bin/api");
    assert!(
        launcher.contains("\nunset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN\n"),
        "{launcher}"
    );
    assert!(!sb.read(".local/bin/work").contains("ANTHROPIC_API_KEY"));

    // The fake claude's `-p ok` records which credential variables it saw.
    let shell_env = [
        ("ANTHROPIC_API_KEY", "shell-key"),
        ("ANTHROPIC_AUTH_TOKEN", "shell-token"),
    ];
    let seen =
        |sb: &Sandbox| std::fs::read_to_string(sb.path("claude-env.log")).unwrap_or_default();
    let status = sb
        .launcher("api")
        .envs(shell_env)
        .args(["-p", "ok"])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(sb.exists("p-args"));
    assert!(!seen(&sb).contains("ANTHROPIC_"), "{}", seen(&sb));
    sb.mcc()
        .envs(shell_env)
        .args(["run", "--profile", "api", "--", "-p", "ok"])
        .assert()
        .success();
    assert!(!seen(&sb).contains("ANTHROPIC_"), "{}", seen(&sb));
    let out = stdout_of(sb.mcc().envs(shell_env).args([
        "exec",
        "api",
        "--",
        "sh",
        "-c",
        "echo key=${ANTHROPIC_API_KEY:-unset} token=${ANTHROPIC_AUTH_TOKEN:-unset}",
    ]));
    assert_eq!(out.trim(), "key=unset token=unset");
    // A subscription profile keeps the variables, as before.
    let out = stdout_of(sb.mcc().envs(shell_env).args([
        "exec",
        "work",
        "--",
        "sh",
        "-c",
        "echo key=${ANTHROPIC_API_KEY:-unset}",
    ]));
    assert_eq!(out.trim(), "key=shell-key");

    let env = stdout_of(sb.mcc().args(["env", "api"]));
    assert!(
        env.lines()
            .any(|l| l == "unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN"),
        "{env}"
    );
    let fish = stdout_of(sb.mcc().args(["env", "api", "--shell", "fish"]));
    assert!(
        fish.lines()
            .any(|l| l == "set -e ANTHROPIC_API_KEY; set -e ANTHROPIC_AUTH_TOKEN"),
        "{fish}"
    );
    assert!(!stdout_of(sb.mcc().args(["env", "work"])).contains("ANTHROPIC"));
}

/// T3 and the trade: `mcc login` (both modes) and `mcc default` refuse an
/// API-key profile before running `claude` at all.
#[test]
fn login_and_default_refuse_api_key_profiles_without_running_claude() {
    let sb = sandbox();
    add_managed(&sb, "api", "--fresh", KEY, &[]).success();
    let before = claude_calls(&sb);
    for args in [
        &["login", "api"][..],
        &["login", "api", "--refresh-only"][..],
        &["renew", "api"][..],
    ] {
        sb.mcc()
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "`api` uses an API key: it has no Claude login to renew",
            ));
    }
    sb.mcc()
        .args(["default", "api", "-y"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "`api` is an API-key profile: it has no Claude account to trade",
        ));
    assert_eq!(claude_calls(&sb), before);

    // The same when ~/.claude's own settings give Claude Code a key.
    with_profile(&sb, "work", "--clone");
    let mut d = settings(&sb, ".claude");
    d["apiKeyHelper"] = "op read x".into();
    sb.write(".claude/settings.json", &d.to_string());
    let before = claude_calls(&sb);
    sb.mcc()
        .args(["login", "default"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "`default` uses an API key: it has no Claude login to renew",
        ));
    sb.mcc()
        .args(["default", "work", "-y"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "~/.claude uses an API key (apiKeyHelper or env.ANTHROPIC_API_KEY",
        ));
    assert_eq!(claude_calls(&sb), before);
}

/// AC3, AC4, T5: an API-key profile is listed apart, with no 5-hour or
/// weekly window and no "run /usage" hint, its spend this month as the
/// status line recorded it, against its budget. JSON rows carry only
/// additive fields and the status and reason codes older mcc versions know.
#[test]
fn api_key_profiles_show_their_estimated_spend_apart() {
    let sb = sandbox();
    with_profile(&sb, "work", "--clone");
    add_managed(&sb, "api", "--fresh", KEY, &["--budget", "50"]).success();

    // Nothing recorded yet.
    let usage = sb.mcc().arg("usage").assert().success();
    let usage = usage.get_output();
    let stdout = String::from_utf8_lossy(&usage.stdout);
    assert!(stdout.contains("# API KEY"), "{stdout}");
    let api_line = stdout.lines().find(|l| l.contains(" api ")).unwrap();
    assert!(
        api_line.contains("— (no cost recorded yet; $50.00 budget)"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&usage.stderr);
    assert!(
        stderr.contains(
            "no cost recorded yet for `api` — start a Claude Code session in this profile"
        ),
        "{stderr}"
    );
    assert!(
        !stderr.contains("`api`: run /usage") && !stderr.contains("`work`, `api`"),
        "{stderr}"
    );

    // The status line reports a new session (cost 0), then its growth.
    for total in ["0", "1.5"] {
        sb.mcc()
            .env("CLAUDE_CONFIG_DIR", sb.path(".mcc/profiles/api"))
            .arg("statusline")
            .write_stdin(format!(
                r#"{{"session_id":"s-1","cost":{{"total_cost_usd":{total}}}}}"#
            ))
            .assert()
            .success();
    }
    let shown = "est. $1.50 this month of $50.00 budget (3%)";
    for args in [&["usage"][..], &["list"][..], &[][..]] {
        let out = stdout_of(sb.mcc().args(args));
        let (table, apis) = out
            .split_once("API KEY")
            .unwrap_or_else(|| panic!("{args:?}: {out}"));
        assert!(!table.contains(" api "), "{args:?}: {out}");
        assert!(apis.contains(shown), "{args:?}: {out}");
        let line = apis.lines().find(|l| l.contains(" api ")).unwrap();
        assert!(!line.contains('%') || line.contains("(3%)"), "{line}");
    }

    let rows = json(stdout_of(sb.mcc().args(["usage", "--json"])).as_bytes());
    let rows = rows.as_array().unwrap();
    let api = common::by_name(rows, "api");
    assert_eq!(api["auth"], "api_key");
    assert_eq!(api["usage"]["status"], "unavailable");
    assert_eq!(api["usage"]["reason"], "no_data");
    assert_eq!(api["usage"]["est_cost_usd"], 1.5);
    assert_eq!(api["usage"]["budget_usd"], 50.0);
    assert!(api["login_expires_at"].is_null());
    for name in ["default", "work"] {
        let row = common::by_name(rows, name);
        assert!(row.get("auth").is_none(), "{row}");
        assert!(row["usage"].get("est_cost_usd").is_none(), "{row}");
        assert!(row["usage"].get("budget_usd").is_none(), "{row}");
    }
    let list = json(stdout_of(sb.mcc().args(["list", "--json"])).as_bytes());
    let api = common::by_name(list.as_array().unwrap(), "api");
    assert_eq!(api["auth"], "api_key");
    assert_eq!(api["usage"]["est_cost_usd"], 1.5);
    assert!(
        common::by_name(list.as_array().unwrap(), "work")
            .get("auth")
            .is_none()
    );

    // The budget can change later; a bad one is refused.
    sb.mcc()
        .args(["config", "set", "profiles.api.api_budget_usd", "75"])
        .assert()
        .success();
    assert_eq!(
        stdout_of(
            sb.mcc()
                .args(["config", "get", "profiles.api.api_budget_usd"])
        )
        .trim(),
        "75.0"
    );
    for bad in ["-3", "nan", "inf"] {
        sb.mcc()
            .args(["config", "set", "profiles.api.api_budget_usd", bad])
            .assert()
            .code(2);
    }
    sb.mcc()
        .args(["config", "set", "profiles.work.api_budget_usd", "5"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not an API-key profile"));
    sb.mcc()
        .args(["config", "set", "profiles.api.api_budget_usd", ""])
        .assert()
        .success();
    assert!(!sb.read(".mcc/config.toml").contains("api_budget_usd"));
}

/// AC5: the API KEY block never changes the layout of the subscription
/// table above it. An API-key row wider than the terminal (it has no layout
/// to shrink into) leaves the 5-hour and weekly bars as wide as they are
/// without it.
#[test]
fn a_wide_api_key_row_leaves_the_subscription_table_layout_alone() {
    const ALIAS: &str = "api-key-shared-research-platform";
    let sb = common::usage_sandbox();
    let stub = common::usage_stub();
    add_managed(&sb, ALIAS, "--fresh", KEY, &["--budget", "50"]).success();
    // A new session (cost 0), then its growth, as the status line reports.
    for total in ["0", "74.96"] {
        sb.mcc()
            .env(
                "CLAUDE_CONFIG_DIR",
                sb.path(&format!(".mcc/profiles/{ALIAS}")),
            )
            .arg("statusline")
            .write_stdin(format!(
                r#"{{"session_id":"s-1","cost":{{"total_cost_usd":{total}}}}}"#
            ))
            .assert()
            .success();
    }
    let out = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("COLUMNS", "90")
            .arg("usage"),
    );
    let (table, apis) = out.split_once("API KEY").unwrap_or_else(|| panic!("{out}"));
    let api_line = apis.lines().find(|l| l.contains(ALIAS)).unwrap();
    assert!(api_line.contains("est. $74.96 this month"), "{out}");
    // The precondition: the API-key row itself overflows the terminal.
    assert!(console::measure_text_width(api_line) > 90, "{api_line}");
    // The table keeps the 10-cell bars it has at 90 columns on its own
    // (see usage_keeps_bars_at_80_columns), and fits.
    let default = table.lines().find(|l| l.contains(" default ")).unwrap();
    assert!(
        default.contains("████░░░░░░ 42%  2h 14m") && default.contains("██░░░░░░░░ 18%  3d 4h"),
        "{out}"
    );
    for line in table.lines() {
        assert!(console::measure_text_width(line) <= 90, "{line}\n{out}");
    }
}

/// A hand-edited budget must be a number of dollars: an integer is read,
/// `nan` or a negative one makes the file invalid.
#[test]
fn a_hand_written_budget_is_checked() {
    let sb = sandbox();
    add_managed(&sb, "api", "--fresh", KEY, &[]).success();
    let config = sb.read(".mcc/config.toml");
    let with =
        |line: &str| config.replacen("[profiles.api]\n", &format!("[profiles.api]\n{line}\n"), 1);
    sb.write(".mcc/config.toml", &with("api_budget_usd = 20"));
    let rows = json(stdout_of(sb.mcc().args(["usage", "--json"])).as_bytes());
    assert_eq!(
        common::by_name(rows.as_array().unwrap(), "api")["usage"]["budget_usd"],
        20.0
    );
    for bad in [
        "api_budget_usd = nan",
        "api_budget_usd = -1.5",
        "api_budget_usd = inf",
    ] {
        sb.write(".mcc/config.toml", &with(bad));
        sb.mcc()
            .args(["usage", "--json"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("number of US dollars, 0 or more"));
    }
}

/// B1/B19 + T8: the shim lives in the API-key profile's settings overlay,
/// so `mcc sync` and the launch-time catch-up keep it there and never take
/// it into the default or another profile; apiKeyHelper stays too. Both a
/// clone (whose default has a statusLine of its own, wrapped) and a fresh
/// profile.
#[test]
fn the_shim_stays_in_api_key_profiles_through_every_sync() {
    let sb = sandbox();
    with_profile(&sb, "work", "--clone");
    let default_line = settings(&sb, ".claude")["statusLine"].clone();
    let work_line = settings(&sb, ".mcc/profiles/work")["statusLine"].clone();
    add_managed(&sb, "api", "--clone", KEY, &[]).success();
    sb.mcc()
        .args([
            "add",
            "lab",
            "--fresh",
            "--api-key-helper",
            "op read x",
            "-y",
        ])
        .assert()
        .success();
    let is_shim = |v: &serde_json::Value| {
        v["statusLine"]["command"]
            .as_str()
            .is_some_and(|c| c.ends_with("/mcc statusline"))
    };
    for p in ["api", "lab"] {
        let dir = format!(".mcc/profiles/{p}");
        assert!(
            is_shim(&settings(&sb, &dir)),
            "{p}: {}",
            settings(&sb, &dir)
        );
        let overlay = json(sb.read(&format!("{dir}/settings.mcc.json")).as_bytes());
        assert!(is_shim(&overlay), "{p}: {overlay}");
    }
    // The clone's own statusLine is wrapped: the shim runs it.
    let record = json(sb.read(".mcc/state/api/statusline.json").as_bytes());
    assert!(
        record["original"]["command"]
            .as_str()
            .is_some_and(|c| c.contains("statusline.sh")),
        "{record}"
    );
    // A sync right away has nothing to change in the clone's statusLine.
    let dry = sb
        .mcc()
        .args(["sync", "api", "--dry-run"])
        .assert()
        .success();
    assert!(
        !String::from_utf8_lossy(&dry.get_output().stderr).contains("statusLine"),
        "{dry:?}"
    );

    let check = |sb: &Sandbox, when: &str| {
        for p in ["api", "lab"] {
            let s = settings(sb, &format!(".mcc/profiles/{p}"));
            assert!(is_shim(&s), "{when}, {p}: {s}");
            assert!(s.get("apiKeyHelper").is_some(), "{when}, {p}: {s}");
        }
        let d = settings(sb, ".claude");
        assert_eq!(d["statusLine"], default_line, "{when}: {d}");
        assert!(d.get("apiKeyHelper").is_none(), "{when}: {d}");
        let w = settings(sb, ".mcc/profiles/work");
        assert_eq!(w["statusLine"], work_line, "{when}: {w}");
        assert!(w.get("apiKeyHelper").is_none(), "{when}: {w}");
    };
    sb.mcc().args(["sync", "api", "-y"]).assert().success();
    sb.mcc().args(["sync", "lab", "-y"]).assert().success();
    check(&sb, "mcc sync ALIAS");
    sb.mcc().args(["sync", "--all", "-y"]).assert().success();
    check(&sb, "mcc sync --all");
    // The default changes; each profile catches up when it starts.
    let mut d = settings(&sb, ".claude");
    d["model"] = "haiku".into();
    sb.write(".claude/settings.json", &d.to_string());
    for p in ["api", "lab", "work"] {
        sb.mcc().args(["launch-sync", p]).assert().success();
    }
    assert_eq!(settings(&sb, ".mcc/profiles/api")["model"], "haiku");
    check(&sb, "launch-time catch-up");
}

/// T7, B5, B20: delete removes the key file mcc wrote, and only that; with
/// --keep-auth it stays (and is named), and adding the profile again
/// replaces it, putting the old one back when the add fails. A rename
/// keeps the key where the helper looks.
#[test]
fn delete_and_rename_keep_track_of_the_managed_key() {
    let sb = sandbox();
    add_managed(&sb, "api", "--fresh", KEY, &[]).success();
    let key = key_file(&sb);

    // A rename moves the command, never the directory or the key, and
    // the new launcher still keeps the shell's key out.
    sb.mcc().args(["rename", "api", "job"]).assert().success();
    assert!(
        sb.read(".local/bin/job")
            .contains("\nunset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN\n")
    );
    assert_eq!(
        settings(&sb, ".mcc/profiles/api")["apiKeyHelper"],
        format!("cat '{}'", key.display())
    );
    assert!(key.exists());

    // --keep-auth keeps the key file and names it.
    sb.mcc()
        .args(["delete", "job", "--yes", "--keep-auth"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "--keep-auth — kept ~/.mcc/keys/api-",
        ));
    assert_eq!(std::fs::read_to_string(&key).unwrap(), KEY);

    // Adding it again fails at a later step: the old key comes back.
    std::fs::create_dir_all(sb.path(".mcc/state/api/statusline.json")).unwrap();
    add_managed(&sb, "api", "--fresh", OTHER_KEY, &[]).failure();
    assert_eq!(std::fs::read_to_string(&key).unwrap(), KEY);
    assert_eq!(key_file(&sb), key, "nothing left beside it");
    assert!(!sb.exists(".mcc/profiles/api"));
    std::fs::remove_dir_all(sb.path(".mcc/state/api")).unwrap();

    // Then it succeeds and replaces the key.
    add_managed(&sb, "api", "--fresh", OTHER_KEY, &[]).success();
    assert_eq!(std::fs::read_to_string(&key).unwrap(), OTHER_KEY);
    assert_eq!(mode(&key), 0o600);
    assert_eq!(key_file(&sb), key);

    // A plain delete logs out as for any profile, and removes the key.
    let calls = claude_calls(&sb).lines().count();
    sb.mcc()
        .args(["delete", "api", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Remove managed API key"));
    assert!(!key.exists());
    assert!(
        claude_calls(&sb)
            .lines()
            .skip(calls)
            .any(|l| l == "auth logout")
    );

    // A helper of the user's that reads a file: the file is left alone.
    sb.write("my-key", KEY);
    let helper = format!("cat '{}'", sb.path("my-key").display());
    sb.mcc()
        .args(["add", "own", "--fresh", "--api-key-helper", &helper, "-y"])
        .assert()
        .success();
    sb.mcc()
        .args(["delete", "own", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("your own command — left alone"));
    assert_eq!(sb.read("my-key"), KEY);
}

/// Delete with no helper to confirm the key file: when settings.json is
/// gone nothing reads it, so it goes (or is kept, named, with
/// --keep-auth); when the helper was edited by hand it stays, named.
#[test]
fn delete_names_or_removes_a_key_file_no_helper_confirms() {
    let sb = sandbox();

    // settings.json gone: the key mcc wrote is removed all the same.
    add_managed(&sb, "api", "--fresh", KEY, &[]).success();
    let key = key_file(&sb);
    std::fs::remove_file(sb.path(".mcc/profiles/api/settings.json")).unwrap();
    sb.mcc()
        .args(["delete", "api", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Remove managed API key"))
        .stderr(predicate::str::contains("your own command").not());
    assert!(!key.exists());

    // ...unless --keep-auth, which keeps it and says where.
    add_managed(&sb, "api", "--fresh", KEY, &[]).success();
    let key = key_file(&sb);
    std::fs::remove_file(sb.path(".mcc/profiles/api/settings.json")).unwrap();
    sb.mcc()
        .args(["delete", "api", "--yes", "--keep-auth"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "--keep-auth — kept ~/.mcc/keys/api-",
        ));
    assert_eq!(std::fs::read_to_string(&key).unwrap(), KEY);
    std::fs::remove_file(&key).unwrap();

    // A hand-edited helper: the file stays, and the warning names it.
    add_managed(&sb, "api", "--fresh", KEY, &[]).success();
    let key = key_file(&sb);
    let mut s = settings(&sb, ".mcc/profiles/api");
    s["apiKeyHelper"] = serde_json::json!("cat /dev/null");
    sb.write(".mcc/profiles/api/settings.json", &s.to_string());
    sb.mcc()
        .args(["delete", "api", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "its apiKeyHelper is not the one mcc wrote — left ~/.mcc/keys/api-",
        ));
    assert_eq!(std::fs::read_to_string(&key).unwrap(), KEY);
}

/// The sharing guard names API-key profiles when one has no account to
/// compare, instead of asking to log them in.
#[test]
fn sharing_with_an_api_key_profile_says_why_it_is_unknown() {
    let sb = sandbox();
    with_profile(&sb, "work", "--clone");
    add_managed(&sb, "api", "--fresh", KEY, &[]).success();
    sb.mcc().args(["share", "work"]).assert().success();
    sb.mcc()
        .args(["share", "api"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "API-key profiles have no Claude account for mcc to compare",
        ))
        .stderr(predicate::str::contains("claude auth login").not());
}
