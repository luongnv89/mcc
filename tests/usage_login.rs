//! Login expiry (#100): `mcc usage` shows each profile's last activity and
//! when its login runs out, warns before it does, and `mcc login` renews it.

mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{DAY_MS, HOUR_MS, Sandbox, by_name, json, now_ms, oauth_blob_with_deadline};

/// Every token string seeded or written by the fake claude here: none may
/// ever reach mcc's output or its log.
const TOKENS: &[&str] = &[
    "tok-default-a1",
    "tok-work-b2",
    "tok-renewed-c3",
    "tok-refreshed-d4",
    "tok-keychain-e5",
];

/// default (login ends in 25 days), `work` (1 day 4 hours) and `lab` (logged
/// out); the live lookup off, as by default.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    common::with_profile(&sb, "work", "--clone");
    common::with_profile(&sb, "lab", "--fresh");
    sb.seed_login(
        ".claude",
        "tok-default-a1",
        now_ms() + HOUR_MS,
        now_ms() + 25 * DAY_MS,
    );
    sb.seed_login(
        ".mcc/profiles/work",
        "tok-work-b2",
        now_ms() + HOUR_MS,
        now_ms() + DAY_MS + 4 * HOUR_MS,
    );
    sb
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(cmd: &mut assert_cmd::Command) -> Out {
    let o = cmd.output().unwrap();
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8(o.stdout).unwrap(),
        stderr: String::from_utf8(o.stderr).unwrap(),
    }
}

fn usage_json(sb: &Sandbox, extra: &[&str]) -> Vec<serde_json::Value> {
    let o = run(sb.mcc().args(["usage", "--json"]).args(extra));
    assert_eq!(o.code, 0, "{}", o.stderr);
    json(o.stdout.as_bytes()).as_array().unwrap().clone()
}

/// Set `p`'s modification time to `secs_ago` seconds ago.
fn touch_ago(p: &Path, secs_ago: u64) {
    std::fs::File::options()
        .append(true)
        .open(p)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
        .unwrap();
}

fn login_warnings(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|l| l.contains("Login for '"))
        .collect()
}

fn assert_no_token(sb: &Sandbox, outs: &[&Out]) {
    let log = std::fs::read_to_string(sb.path(".mcc/logs/mcc.log")).unwrap_or_default();
    for t in TOKENS {
        for o in outs {
            assert!(!o.stdout.contains(t), "{t} in stdout: {}", o.stdout);
            assert!(!o.stderr.contains(t), "{t} in stderr: {}", o.stderr);
        }
        assert!(!log.contains(t), "{t} in mcc.log");
    }
}

// ---- AC1: last activity ------------------------------------------------------

#[test]
fn ac1_last_activity_from_history_and_transcripts() {
    let sb = sandbox();
    // `work` was last used 3 days ago (its history), `lab` never; touching
    // lab's .claude.json and credentials doesn't count as using it.
    sb.write(".mcc/profiles/work/history.jsonl", "{}\n");
    touch_ago(&sb.path(".mcc/profiles/work/history.jsonl"), 3 * 86400 + 60);
    let _ = std::fs::remove_dir_all(sb.path(".mcc/profiles/work/projects"));
    sb.write(".mcc/profiles/lab/.claude.json", "{}");
    sb.write(".mcc/profiles/lab/.credentials.json", "{}");
    let o = run(sb.mcc().arg("usage"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    let header = o.stdout.lines().next().unwrap();
    assert!(
        header.starts_with("PROFILE    LAST ACTIVE  LOGIN ENDS  5-HOUR"),
        "{}",
        o.stdout
    );
    let row = |name: &str| {
        o.stdout
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("no {name} row\n{}", o.stdout))
            .to_string()
    };
    assert!(
        row("work").starts_with("work       3d ago "),
        "{}",
        o.stdout
    );
    assert!(
        row("lab").starts_with("lab        unknown      unknown "),
        "{}",
        o.stdout
    );
    // The default's seeded history and transcript are from just now.
    assert!(
        row("default").starts_with("default ★  just now "),
        "{}",
        o.stdout
    );

    let arr = usage_json(&sb, &[]);
    assert!(
        by_name(&arr, "work")["last_activity"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(
        by_name(&arr, "lab")["last_activity"],
        serde_json::Value::Null
    );

    // A newer transcript is newer activity.
    sb.write(".mcc/profiles/work/projects/-p/s1.jsonl", "{}\n");
    touch_ago(
        &sb.path(".mcc/profiles/work/projects/-p/s1.jsonl"),
        5 * 3600 + 60,
    );
    let o = run(sb.mcc().arg("usage"));
    assert!(o.stdout.contains("\nwork       5h ago "), "{}", o.stdout);
}

#[test]
fn ac1_shared_transcripts_are_not_the_profiles_activity() {
    let sb = sandbox();
    // `lab`'s projects/ links to a shared store with a fresh transcript of
    // another profile: lab itself was never used.
    sb.write(".mcc/shared/projects/-p/other.jsonl", "{}\n");
    let projects = sb.path(".mcc/profiles/lab/projects");
    let _ = std::fs::remove_dir_all(&projects);
    std::os::unix::fs::symlink(sb.path(".mcc/shared/projects"), &projects).unwrap();
    let arr = usage_json(&sb, &[]);
    assert_eq!(
        by_name(&arr, "lab")["last_activity"],
        serde_json::Value::Null
    );
    let o = run(sb.mcc().arg("usage"));
    assert!(o.stdout.contains("\nlab        unknown "), "{}", o.stdout);
}

// ---- AC2 / AC3: the warning --------------------------------------------------

#[test]
fn ac2_warning_names_the_profile_inside_the_window() {
    let sb = sandbox();
    let o = run(sb.mcc().arg("usage"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    let warnings = login_warnings(&o.stderr);
    assert_eq!(warnings.len(), 1, "{}", o.stderr);
    assert!(
        warnings[0].contains("Login for 'work' expires in 1d 4h (")
            || warnings[0].contains("Login for 'work' expires in 1d 3h ("),
        "{}",
        o.stderr
    );
    assert!(
        warnings[0].ends_with("— run `mcc login work` to renew it"),
        "{}",
        o.stderr
    );
    // The warning is on stderr, never in the table or the JSON.
    assert!(!o.stdout.contains("Login for"), "{}", o.stdout);
    let arr = usage_json(&sb, &[]);
    assert_eq!(by_name(&arr, "work")["login_warning"], true);
    assert_eq!(
        by_name(&arr, "work")["login_expires_source"],
        "credentials_file"
    );
    // A login that already ran out says so.
    sb.seed_login(
        ".mcc/profiles/work",
        "tok-work-b2",
        now_ms() - HOUR_MS,
        now_ms() - HOUR_MS,
    );
    let o = run(sb.mcc().arg("usage"));
    assert_eq!(
        login_warnings(&o.stderr),
        ["  ⚠ Login for 'work' has expired — run `mcc login work` to sign in again"],
        "{}",
        o.stderr
    );
    assert!(o.stdout.contains("\nwork       "), "{}", o.stdout);
    assert!(
        o.stdout
            .lines()
            .any(|l| l.starts_with("work") && l.contains(" expired ")),
        "{}",
        o.stdout
    );
}

#[test]
fn ac3_no_warning_outside_the_window() {
    let sb = sandbox();
    let o = run(sb.mcc().arg("usage"));
    assert!(!o.stderr.contains("Login for 'default'"), "{}", o.stderr);
    assert!(!o.stderr.contains("Login for 'lab'"), "{}", o.stderr);
    let arr = usage_json(&sb, &[]);
    assert_eq!(by_name(&arr, "default")["login_warning"], false);
    assert_eq!(by_name(&arr, "lab")["login_warning"], false);
    assert_eq!(
        by_name(&arr, "lab")["login_expires_at"],
        serde_json::Value::Null
    );
    // Only `default`, 25 days out.
    let o = run(sb.mcc().args(["usage", "default"]));
    assert!(login_warnings(&o.stderr).is_empty(), "{}", o.stderr);
    let row = o.stdout.lines().nth(1).unwrap();
    assert!(
        row.contains(" 24d 23h ") || row.contains(" 25d 0h "),
        "{}",
        o.stdout
    );
}

#[test]
fn login_warn_days_widens_the_window() {
    let sb = sandbox();
    sb.seed_login(
        ".claude",
        "tok-default-a1",
        now_ms() + HOUR_MS,
        now_ms() + 10 * DAY_MS,
    );
    let o = run(sb.mcc().arg("usage"));
    assert!(!o.stderr.contains("Login for 'default'"), "{}", o.stderr);
    sb.mcc()
        .args(["config", "set", "usage.login_warn_days", "14"])
        .assert()
        .success();
    let o = run(sb.mcc().arg("usage"));
    assert!(
        o.stderr.contains("Login for 'default' expires in 9d 23h")
            || o.stderr.contains("Login for 'default' expires in 10d 0h"),
        "{}",
        o.stderr
    );
    assert_eq!(login_warnings(&o.stderr).len(), 2, "{}", o.stderr);
    // Out of range: refused, the file unchanged.
    sb.mcc()
        .args(["config", "set", "usage.login_warn_days", "30"])
        .assert()
        .code(2);
    let o = run(sb.mcc().args(["config", "get", "usage.login_warn_days"]));
    assert_eq!(o.stdout, "14\n");
}

// ---- what plain `mcc usage` may read ----------------------------------------

#[test]
fn keychain_not_touched_by_default_or_with_mcc_no_usage() {
    let sb = sandbox();
    sb.seed_keychain(
        "Claude Code-credentials",
        "tok-keychain-e5",
        now_ms() + HOUR_MS,
    );
    // Plain `mcc usage`: the credentials file's timestamps only.
    let o = run(sb.mcc().arg("usage"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    usage_json(&sb, &[]);
    assert!(!sb.exists("security.log"), "the keychain was read");
    // With the live lookup on, MCC_NO_USAGE still reads nothing at all.
    common::enable_live_usage(&sb);
    let o = run(sb.mcc().env("MCC_NO_USAGE", "1").arg("usage"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    let arr = json(
        run(sb.mcc().env("MCC_NO_USAGE", "1").args(["usage", "--json"]))
            .stdout
            .as_bytes(),
    );
    assert!(!sb.exists("security.log"), "the keychain was read");
    for p in arr.as_array().unwrap() {
        assert_eq!(p["login_expires_at"], serde_json::Value::Null, "{p}");
    }
    assert!(login_warnings(&o.stderr).is_empty(), "{}", o.stderr);
}

#[test]
fn the_keychain_is_read_with_the_live_lookup_and_its_newer_deadline_wins() {
    let sb = sandbox();
    common::enable_live_usage(&sb);
    // The default's Keychain item holds a newer login than its file.
    sb.write(
        "fake-keychain/Claude Code-credentials",
        &oauth_blob_with_deadline(
            "tok-keychain-e5",
            now_ms() + HOUR_MS,
            Some(now_ms() + 29 * DAY_MS),
        ),
    );
    let arr = usage_json(&sb, &[]);
    assert!(sb.exists("security.log"));
    let d = by_name(&arr, "default");
    assert_eq!(d["login_expires_source"], "keychain");
    assert_eq!(
        by_name(&arr, "work")["login_expires_source"],
        "credentials_file"
    );
}

#[test]
fn no_token_leak() {
    let sb = sandbox();
    sb.seed_keychain(
        "Claude Code-credentials",
        "tok-keychain-e5",
        now_ms() + HOUR_MS,
    );
    let mut outs = vec![
        run(sb.mcc().arg("usage")),
        run(sb.mcc().args(["usage", "--json"])),
        run(sb.mcc().args(["usage", "--all", "--json-v2"])),
        run(sb.mcc().args(["usage", "--all"])),
    ];
    let renewed = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 30 * DAY_MS),
    );
    outs.push(run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &renewed)
        .args(["login", "work"])));
    outs.push(run(sb.mcc().args(["login", "work", "--refresh-only"])));
    outs.push(run(sb
        .mcc()
        .env("FAKE_LOGIN_FAIL", "1")
        .args(["login", "default"])));
    assert_eq!(outs[4].code, 0, "{}", outs[4].stderr);
    let refs: Vec<&Out> = outs.iter().collect();
    assert_no_token(&sb, &refs);
}

// ---- AC4 / AC5: mcc login ----------------------------------------------------

#[test]
fn ac4_ac5_renew_extends_the_login_and_clears_the_warning() {
    let sb = sandbox();
    let o = run(sb.mcc().arg("usage"));
    assert!(o.stderr.contains("Login for 'work'"), "{}", o.stderr);

    let renewed = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 30 * DAY_MS),
    );
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &renewed)
        .args(["login", "work"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stderr
            .contains("Login for `work` renewed: it now runs out in 29d 23h")
            || o.stderr
                .contains("Login for `work` renewed: it now runs out in 30d 0h"),
        "{}",
        o.stderr
    );
    // It ran `claude auth login` as the profile, and nothing else.
    assert_eq!(sb.read("login-args"), "\n");
    let o = run(sb.mcc().arg("usage"));
    assert!(login_warnings(&o.stderr).is_empty(), "{}", o.stderr);
    let arr = usage_json(&sb, &[]);
    assert_eq!(by_name(&arr, "work")["login_warning"], false);

    // `mcc renew` is the same command, here for the default profile.
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &renewed)
        .args(["renew", "claude"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stderr.contains("Login for `default` renewed"),
        "{}",
        o.stderr
    );
}

#[test]
fn renew_unchanged_deadline_fails() {
    let sb = sandbox();
    // Claude Code rewrote the file (a new access token) but kept the deadline.
    let deadline = now_ms() + DAY_MS + 4 * HOUR_MS;
    sb.seed_login(
        ".mcc/profiles/work",
        "tok-work-b2",
        now_ms() + HOUR_MS,
        deadline,
    );
    let same = oauth_blob_with_deadline("tok-renewed-c3", now_ms() + 8 * HOUR_MS, Some(deadline));
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &same)
        .args(["login", "work"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr
            .contains("`work` signed in, but its login deadline did not move"),
        "{}",
        o.stderr
    );
    assert!(!o.stderr.contains("renewed"), "{}", o.stderr);
}

#[test]
fn renew_that_still_ends_inside_the_window_fails() {
    let sb = sandbox();
    let later_but_soon = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 36 * HOUR_MS),
    );
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &later_but_soon)
        .args(["login", "work"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(o.stderr.contains("still runs out in"), "{}", o.stderr);
}

#[test]
fn renew_sign_in_failure_is_reported() {
    let sb = sandbox();
    let o = run(sb.mcc().env("FAKE_LOGIN_FAIL", "1").args(["login", "work"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr
            .contains("signing `work` in failed: `claude auth login` exited with 1"),
        "{}",
        o.stderr
    );
}

#[test]
fn renew_with_an_unknown_deadline_says_so_and_succeeds() {
    let sb = sandbox();
    // A Claude Code that records no deadline, before and after the sign-in.
    sb.write(
        ".mcc/profiles/work/.credentials.json",
        &oauth_blob_with_deadline("tok-work-b2", now_ms() + HOUR_MS, None),
    );
    let no_deadline = oauth_blob_with_deadline("tok-renewed-c3", now_ms() + 8 * HOUR_MS, None);
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &no_deadline)
        .args(["login", "work"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stderr
            .contains("`work` is signed in; its login expiry is unknown"),
        "{}",
        o.stderr
    );
    assert_no_token(&sb, &[&o]);
}

#[test]
fn renew_that_loses_a_known_deadline_fails() {
    let sb = sandbox();
    // `work` had a deadline; after the sign-in the credentials record none.
    let no_deadline = oauth_blob_with_deadline("tok-renewed-c3", now_ms() + 8 * HOUR_MS, None);
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &no_deadline)
        .args(["login", "work"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr
            .contains("`work` signed in, but mcc could not read its login deadline afterwards"),
        "{}",
        o.stderr
    );
    assert!(o.stderr.contains("`mcc login work`"), "{}", o.stderr);
    assert!(o.stderr.contains("`mcc usage --all`"), "{}", o.stderr);
    assert!(!o.stderr.contains("expiry is unknown"), "{}", o.stderr);
    assert!(!o.stderr.contains("renewed"), "{}", o.stderr);
    assert_no_token(&sb, &[&o]);
}

#[test]
fn renew_unknown_profile_is_a_usage_error() {
    let sb = sandbox();
    let _ = std::fs::remove_file(sb.path("login-args"));
    let o = run(sb.mcc().args(["login", "nope"]));
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(
        o.stderr.contains("no profile named \"nope\""),
        "{}",
        o.stderr
    );
    assert!(!sb.exists("login-args"));
}

#[test]
fn renew_ctrl_c_after_the_child_exits_interrupted() {
    let sb = sandbox();
    // The fake writes a fresh login, then Ctrl+C reaches mcc while it runs.
    let renewed = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 30 * DAY_MS),
    );
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &renewed)
        .env("FAKE_LOGIN_INTERRUPT", "1")
        .args(["login", "work"]));
    assert_eq!(o.code, 130, "{}", o.stderr);
    assert!(o.stderr.contains("interrupted"), "{}", o.stderr);
    assert!(!o.stderr.contains("renewed"), "{}", o.stderr);
}

// ---- account switches --------------------------------------------------------

fn state(email: &str, org_uuid: &str, org: &str) -> String {
    format!(
        r#"{{"oauthAccount":{{"emailAddress":"{email}","organizationUuid":"{org_uuid}","organizationName":"{org}","organizationType":"claude_team"}}}}"#
    )
}

#[test]
fn renew_into_another_account_is_a_failure() {
    let sb = sandbox();
    sb.write(
        ".mcc/profiles/work/.claude.json",
        &state("a@acme.test", "org-1", "Acme"),
    );
    let renewed = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 30 * DAY_MS),
    );
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &renewed)
        .env("FAKE_LOGIN_STATE", state("b@other.test", "org-9", "Other"))
        .args(["login", "work"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr.contains(
            "`work` is now signed in to b@other.test (Other), not a@acme.test (Acme) as before"
        ),
        "{}",
        o.stderr
    );
    assert!(!o.stderr.contains("renewed"), "{}", o.stderr);
    // Nothing written to mcc's config.
    assert!(!sb.read(".mcc/config.toml").contains("b@other.test"));
}

/// Turn session sharing on for `aliases` in the config, by hand: the guard
/// reads only the config and the state files.
fn share(sb: &Sandbox, aliases: &[&str]) {
    let mut cfg = sb.read(".mcc/config.toml");
    for a in aliases {
        let head = format!("[profiles.{a}]\n");
        assert!(cfg.contains(&head), "{cfg}");
        cfg = cfg.replacen(&head, &format!("{head}share_sessions = true\n"), 1);
    }
    sb.write(".mcc/config.toml", &cfg);
}

#[test]
fn renew_same_email_in_another_org_on_a_sharing_profile_fails_naming_the_group() {
    let sb = sandbox();
    share(&sb, &["work", "lab"]);
    sb.write(
        ".mcc/profiles/work/.claude.json",
        &state("a@acme.test", "org-1", "Acme"),
    );
    sb.write(
        ".mcc/profiles/lab/.claude.json",
        &state("c@acme.test", "org-1", "Acme"),
    );
    let renewed = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 30 * DAY_MS),
    );
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &renewed)
        .env("FAKE_LOGIN_STATE", state("a@acme.test", "org-2", "Globex"))
        .args(["login", "work"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(o.stderr.contains("a@acme.test (Globex)"), "{}", o.stderr);
    assert!(o.stderr.contains("group \"default\""), "{}", o.stderr);
    assert!(o.stderr.contains("mcc share work --off"), "{}", o.stderr);
}

#[test]
fn renew_sharing_profile_without_a_previous_account_is_still_checked() {
    let sb = sandbox();
    share(&sb, &["work", "lab"]);
    // No account recorded anywhere for `work` before the sign-in.
    let cfg = sb.read(".mcc/config.toml");
    let cfg = regex::Regex::new(r"(?s)\[profiles\.work\.account\].*?(\n\[|\z)")
        .unwrap()
        .replace(&cfg, "$1")
        .into_owned();
    sb.write(".mcc/config.toml", &cfg);
    let _ = std::fs::remove_file(sb.path(".mcc/profiles/work/.claude.json"));
    sb.write(
        ".mcc/profiles/lab/.claude.json",
        &state("c@acme.test", "org-1", "Acme"),
    );
    let renewed = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 30 * DAY_MS),
    );
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &renewed)
        .env(
            "FAKE_LOGIN_STATE",
            state("x@globex.test", "org-2", "Globex"),
        )
        .args(["login", "work"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr
            .contains("sessions are now shared across a different organization"),
        "{}",
        o.stderr
    );
    assert!(o.stderr.contains("group \"default\""), "{}", o.stderr);
    // The same sign-in into the group's organization is fine.
    let _ = std::fs::remove_file(sb.path(".mcc/profiles/work/.claude.json"));
    let later = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 31 * DAY_MS),
    );
    let o = run(sb
        .mcc()
        .env("FAKE_LOGIN_CREDS", &later)
        .env("FAKE_LOGIN_STATE", state("x@acme.test", "org-1", "Acme"))
        .args(["login", "work"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
}

// ---- --refresh-only ----------------------------------------------------------

/// `work`'s access token expired an hour ago; its login ends in 1d 4h.
fn expired_access(sb: &Sandbox) -> i64 {
    let deadline = now_ms() + DAY_MS + 4 * HOUR_MS;
    sb.seed_login(
        ".mcc/profiles/work",
        "tok-work-b2",
        now_ms() - HOUR_MS,
        deadline,
    );
    deadline
}

const STRIPPED: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "ANTHROPIC_BASE_URL",
];

#[test]
fn refresh_only_isolated() {
    let sb = sandbox();
    expired_access(&sb);
    let refreshed = oauth_blob_with_deadline(
        "tok-refreshed-d4",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 30 * DAY_MS),
    );
    let mut cmd = sb.mcc();
    for v in STRIPPED {
        cmd.env(v, "set-in-mccs-environment");
    }
    let o =
        run(cmd
            .env("FAKE_REFRESH_CREDS", &refreshed)
            .args(["login", "work", "--refresh-only"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stderr.contains("Login for `work` extended"),
        "{}",
        o.stderr
    );
    let args: Vec<String> = sb.read("p-args").lines().map(str::to_string).collect();
    assert_eq!(
        args,
        [
            "-p",
            "ok",
            "--model",
            "haiku",
            "--max-turns",
            "1",
            "--tools",
            "",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--setting-sources",
            "user",
            "--settings",
            r#"{"disableAllHooks":true}"#,
        ]
    );
    // No credential variable reached Claude Code.
    let seen = std::fs::read_to_string(sb.path("claude-env.log")).unwrap_or_default();
    assert!(seen.is_empty(), "{seen}");
    // It ran in an empty scratch directory, outside every profile and
    // project, removed afterwards; no transcript was written.
    let cwd = sb.read("p-cwd");
    let cwd = Path::new(cwd.trim());
    assert!(!cwd.starts_with(sb.home()), "{}", cwd.display());
    assert!(!cwd.exists(), "{} left behind", cwd.display());
    assert!(!sb.exists(".mcc/profiles/work/projects/-p"));
    // A sign-in strips them too.
    let renewed = oauth_blob_with_deadline(
        "tok-renewed-c3",
        now_ms() + 8 * HOUR_MS,
        Some(now_ms() + 30 * DAY_MS),
    );
    let mut cmd = sb.mcc();
    for v in STRIPPED {
        cmd.env(v, "set-in-mccs-environment");
    }
    let o = run(cmd
        .env("FAKE_LOGIN_CREDS", &renewed)
        .args(["login", "work"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
    let seen = std::fs::read_to_string(sb.path("claude-env.log")).unwrap_or_default();
    assert!(seen.is_empty(), "{seen}");
}

#[test]
fn refresh_only_with_a_valid_access_token_sends_nothing() {
    let sb = sandbox();
    let o = run(sb.mcc().args(["login", "work", "--refresh-only"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr
            .contains("the access token of `work` is still valid"),
        "{}",
        o.stderr
    );
    assert!(
        o.stderr.contains("run `mcc login work` to renew the login"),
        "{}",
        o.stderr
    );
    assert!(!sb.exists("p-args"), "a request was made");
}

#[test]
fn refresh_only_unchanged_deadline_fails() {
    let sb = sandbox();
    let deadline = expired_access(&sb);
    let refreshed =
        oauth_blob_with_deadline("tok-refreshed-d4", now_ms() + 8 * HOUR_MS, Some(deadline));
    let o = run(sb.mcc().env("FAKE_REFRESH_CREDS", &refreshed).args([
        "login",
        "work",
        "--refresh-only",
    ]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr
            .contains("access token refreshed but login deadline unchanged"),
        "{}",
        o.stderr
    );
    assert!(
        o.stderr.contains("run `mcc login work` to sign in again"),
        "{}",
        o.stderr
    );
    // A failed request is reported too.
    expired_access(&sb);
    let o =
        run(sb
            .mcc()
            .env("FAKE_REFRESH_MODE", "fail")
            .args(["login", "work", "--refresh-only"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(o.stderr.contains("could not refresh"), "{}", o.stderr);
}

#[test]
fn refresh_only_without_a_login_sends_nothing() {
    let sb = sandbox();
    let o = run(sb.mcc().args(["login", "lab", "--refresh-only"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr.contains("no Claude Code login found for `lab`"),
        "{}",
        o.stderr
    );
    assert!(!sb.exists("p-args"));
}
