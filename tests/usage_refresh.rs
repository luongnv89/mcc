//! `mcc usage --all` refreshes an expired account token through Claude Code
//! (#106): one tiny `claude -p` request as the profile, in a folder where
//! `mcc run` starts it, announced on stderr, then a second live lookup.

mod common;

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use common::{
    DAY_MS, HOUR_MS, Sandbox, UsageStub, by_name, json, now_ms, oauth_blob_with_deadline,
};

/// Every token string seeded here or written by the fake claude: none may
/// ever reach mcc's output or its log.
const TOKENS: &[&str] = &[
    "tok-old-work-a1",
    "tok-new-work-b2",
    "tok-old-default-c3",
    "tok-new-default-d4",
];

/// The request, exactly: one isolated turn.
const REFRESH_ARGS: &[&str] = &[
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
];

/// default (no OAuth login), `work` (access token expired an hour ago, login
/// good for 20 days) and `lab` (logged out). The live lookup is off.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    common::with_profile(&sb, "work", "--clone");
    common::with_profile(&sb, "lab", "--fresh");
    sb.seed_login(
        ".mcc/profiles/work",
        "tok-old-work-a1",
        now_ms() - HOUR_MS,
        now_ms() + 20 * DAY_MS,
    );
    sb
}

/// The usage endpoint: 33% for a refreshed token, 401 for any other.
fn stub() -> UsageStub {
    let reset = common::rfc3339_in(3600);
    UsageStub::start(move |req| match req.authorization.as_deref() {
        Some("Bearer tok-new-work-b2" | "Bearer tok-new-default-d4") => (
            200,
            format!(
                r#"{{"five_hour":{{"utilization":33,"resets_at":"{reset}"}},"seven_day":{{"utilization":12,"resets_at":"{reset}"}}}}"#
            ),
        ),
        _ => (401, r#"{"error":"invalid token"}"#.into()),
    })
}

/// What Claude Code writes when it refreshes `token`: valid for 8 hours, the
/// login deadline as it was.
fn refreshed(token: &str) -> String {
    oauth_blob_with_deadline(token, now_ms() + 8 * HOUR_MS, Some(now_ms() + 20 * DAY_MS))
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

/// `mcc usage --all` (plus `extra`) against `stub`, refreshing to `creds`.
fn usage_all(sb: &Sandbox, stub: &UsageStub, creds: &str, extra: &[&str]) -> Out {
    run(sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .env("FAKE_REFRESH_CREDS", creds)
        .args(["usage", "--all"])
        .args(extra))
}

/// A folder in the sandbox, symlinks resolved (on macOS the temp dir is
/// behind /var -> /private/var), as `mcc project set` stores it.
fn real_dir(sb: &Sandbox, rel: &str) -> PathBuf {
    std::fs::create_dir_all(sb.path(rel)).unwrap();
    std::fs::canonicalize(sb.path(rel)).unwrap()
}

fn project_set(sb: &Sandbox, profile: &str, dir: &Path) {
    sb.mcc()
        .args(["project", "set", profile, "--path"])
        .arg(dir)
        .assert()
        .success();
}

/// The folders the fake claude ran its request in, one per request.
fn requests(sb: &Sandbox) -> Vec<String> {
    std::fs::read_to_string(sb.path("p-runs"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
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

/// No message ever points to `mcc login --refresh-only`.
fn assert_no_refresh_only_hint(o: &Out) {
    assert!(!o.stderr.contains("--refresh-only"), "{}", o.stderr);
}

fn row<'a>(stdout: &'a str, name: &str) -> &'a str {
    stdout
        .lines()
        .find(|l| l.starts_with(name))
        .unwrap_or_else(|| panic!("no {name} row\n{stdout}"))
}

// ---- refreshed in the project mapped to the account ---------------------------

#[test]
fn an_expired_token_is_refreshed_in_the_project_of_its_account() {
    let sb = sandbox();
    let proj = real_dir(&sb, "code/app");
    sb.write("code/app/keep.txt", "mine");
    project_set(&sb, "work", &proj);
    let stub = stub();
    let o = usage_all(&sb, &stub, &refreshed("tok-new-work-b2"), &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);

    // One request, in the project, exactly as isolated as --refresh-only.
    assert_eq!(requests(&sb), [proj.display().to_string()], "{}", o.stderr);
    assert_eq!(sb.read("p-cwd").trim(), proj.display().to_string());
    let args: Vec<String> = sb.read("p-args").lines().map(str::to_string).collect();
    assert_eq!(args, REFRESH_ARGS);
    // The project is left as it was.
    assert_eq!(sb.read("code/app/keep.txt"), "mine");

    // Announced on stderr, naming the account and the folder, before it ran.
    let notice = o
        .stderr
        .lines()
        .position(|l| l.contains("refreshing the token of `work`"))
        .unwrap_or_else(|| panic!("no notice\n{}", o.stderr));
    assert!(
        o.stderr.lines().nth(notice).unwrap().contains("app"),
        "{}",
        o.stderr
    );
    let done = o
        .stderr
        .lines()
        .position(|l| l.contains("refreshed `work`"))
        .unwrap_or_else(|| panic!("no success\n{}", o.stderr));
    assert!(notice < done, "{}", o.stderr);

    // The table shows the fresh usage, and the cache has it for next time.
    let work = row(&o.stdout, "work");
    assert!(work.contains("33%"), "{}", o.stdout);
    assert!(!work.contains("token expired"), "{}", o.stdout);
    let cache = sb.read(".mcc/state/work/usage.json");
    assert!(cache.contains("33"), "{cache}");
    assert!(!sb.exists(".mcc/state/work/refresh-failed-at"));
    assert_eq!(stub.tokens(), ["tok-new-work-b2"]);
    assert_no_token(&sb, &[&o]);
    assert_no_refresh_only_hint(&o);
}

#[test]
fn accounts_no_project_maps_to_are_refreshed_in_new_empty_folders() {
    let sb = sandbox();
    sb.seed_login(
        ".claude",
        "tok-old-default-c3",
        now_ms() - HOUR_MS,
        now_ms() + 20 * DAY_MS,
    );
    // Neither `default` nor `work` is mapped to a project, and `work` is
    // not the global default: each is refreshed in a new empty folder,
    // started by name the way `mcc run --profile` starts it.
    let stub = stub();
    let mut cmd = sb.mcc();
    for v in ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"] {
        cmd.env(v, "set-in-mccs-environment");
    }
    let o = run(cmd
        .env("MCC_USAGE_URL", &stub.url)
        .env("FAKE_REFRESH_CREDS", refreshed("tok-new-default-d4"))
        .args(["usage", "--all"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
    let runs = requests(&sb);
    assert_eq!(runs.len(), 2, "{}", o.stderr);
    // New empty folders outside the sandbox home, removed afterwards.
    let home = std::fs::canonicalize(sb.home()).unwrap();
    for run in &runs {
        let cwd = Path::new(run);
        assert!(!cwd.starts_with(&home), "{}", cwd.display());
        assert!(!cwd.starts_with(sb.home()), "{}", cwd.display());
        assert!(!cwd.exists(), "{} left behind", cwd.display());
    }
    assert_ne!(runs[0], runs[1], "each request gets its own folder");
    assert_eq!(sb.read("p-ls"), "");
    // No credential variable reached Claude Code.
    let seen = std::fs::read_to_string(sb.path("claude-env.log")).unwrap_or_default();
    assert!(seen.is_empty(), "{seen}");

    for name in ["default", "work"] {
        assert!(
            o.stderr
                .contains(&format!("refreshing the token of `{name}`")),
            "{}",
            o.stderr
        );
        assert!(
            o.stderr.contains(&format!("refreshed `{name}`")),
            "{}",
            o.stderr
        );
        // The fresh login was written to this profile's own folder, so
        // its row shows the usage the refreshed token reads.
        assert!(row(&o.stdout, name).contains("33%"), "{}", o.stdout);
    }
    assert!(!o.stderr.contains("no project starts"), "{}", o.stderr);
    assert_no_token(&sb, &[&o]);
    assert_no_refresh_only_hint(&o);
}

#[test]
fn a_new_folder_inside_a_project_is_not_used() {
    let sb = sandbox();
    sb.seed_login(
        ".claude",
        "tok-old-default-c3",
        now_ms() - HOUR_MS,
        now_ms() + 20 * DAY_MS,
    );
    // The temp folder sits inside a project of `lab`: a new folder there
    // is not outside every project, so nothing is refreshed in it.
    let tmp = real_dir(&sb, "tmpdir");
    project_set(&sb, "lab", &tmp);
    let stub = stub();
    let o = run(sb
        .mcc()
        .env("TMPDIR", &tmp)
        .env("MCC_USAGE_URL", &stub.url)
        .env("FAKE_REFRESH_CREDS", refreshed("tok-new-default-d4"))
        .args(["usage", "--all"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(requests(&sb).is_empty(), "a request was made");
    for name in ["default", "work"] {
        assert!(
            o.stderr.contains(&format!(
                "is inside a project, so mcc did not refresh `{name}` there"
            )),
            "{}",
            o.stderr
        );
        assert!(
            o.stderr.contains(&format!("`mcc run --profile {name}`")),
            "{}",
            o.stderr
        );
    }
    // The folder mcc made is gone again.
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
}

// ---- when nothing is sent -----------------------------------------------------

#[test]
fn a_profile_that_is_not_logged_in_is_never_refreshed() {
    let sb = sandbox();
    // `work` is fine: only default and lab, both without an OAuth login.
    sb.seed_login(
        ".mcc/profiles/work",
        "tok-new-work-b2",
        now_ms() + HOUR_MS,
        now_ms() + 20 * DAY_MS,
    );
    project_set(&sb, "lab", &real_dir(&sb, "code/lab"));
    let stub = stub();
    let o = usage_all(&sb, &stub, &refreshed("tok-new-work-b2"), &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!sb.exists("p-args"), "a request was made");
    assert!(!o.stderr.contains("refreshing"), "{}", o.stderr);
    // Not even considered: nothing is said about refreshing them.
    assert!(!o.stderr.contains("login found"), "{}", o.stderr);
    assert!(
        row(&o.stdout, "lab").contains("not logged in"),
        "{}",
        o.stdout
    );
}

#[test]
fn a_rejected_token_that_has_not_expired_is_not_refreshed() {
    let sb = sandbox();
    project_set(&sb, "work", &real_dir(&sb, "code/app"));
    // Valid for another hour, but the API says 401.
    sb.seed_login(
        ".mcc/profiles/work",
        "tok-old-work-a1",
        now_ms() + HOUR_MS,
        now_ms() + 20 * DAY_MS,
    );
    let stub = stub();
    let o = usage_all(&sb, &stub, &refreshed("tok-new-work-b2"), &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(stub.tokens(), ["tok-old-work-a1"]);
    assert!(!sb.exists("p-args"), "a request was made");
    assert!(
        o.stderr
            .contains("the usage API rejected `work`'s access token"),
        "{}",
        o.stderr
    );
    assert!(o.stderr.contains("`mcc login work`"), "{}", o.stderr);
    assert_no_token(&sb, &[&o]);
    assert_no_refresh_only_hint(&o);
}

#[test]
fn an_expired_login_is_not_refreshed_and_points_to_a_new_sign_in() {
    let sb = sandbox();
    project_set(&sb, "work", &real_dir(&sb, "code/app"));
    sb.seed_login(
        ".mcc/profiles/work",
        "tok-old-work-a1",
        now_ms() - 2 * HOUR_MS,
        now_ms() - HOUR_MS,
    );
    let stub = stub();
    let o = usage_all(&sb, &stub, &refreshed("tok-new-work-b2"), &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!sb.exists("p-args"), "a request was made");
    assert!(
        o.stderr.contains("the login of `work` has expired"),
        "{}",
        o.stderr
    );
    assert!(
        o.stderr.contains("sign in again with `mcc login work`"),
        "{}",
        o.stderr
    );
    assert_no_refresh_only_hint(&o);
}

#[test]
fn only_usage_all_refreshes() {
    let sb = sandbox();
    project_set(&sb, "work", &real_dir(&sb, "code/app"));
    let stub = stub();
    // The cache, and the live lookup of `[usage] live = true`: no request.
    for live in [false, true] {
        if live {
            common::enable_live_usage(&sb);
        }
        let o = run(sb
            .mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("FAKE_REFRESH_CREDS", refreshed("tok-new-work-b2"))
            .arg("usage"));
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(!sb.exists("p-args"), "a request was made (live = {live})");
        if live {
            assert!(
                row(&o.stdout, "work").contains("token expired"),
                "{}",
                o.stdout
            );
        }
    }
    // MCC_NO_USAGE: nothing is looked up, so nothing is refreshed.
    let o = run(sb
        .mcc()
        .env("MCC_NO_USAGE", "1")
        .env("MCC_USAGE_URL", &stub.url)
        .args(["usage", "--all"]));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!sb.exists("p-args"), "a request was made");
}

#[test]
fn a_refresh_already_running_is_not_started_twice() {
    let sb = sandbox();
    project_set(&sb, "work", &real_dir(&sb, "code/app"));
    // Another mcc command holds `work`'s refresh lock.
    std::fs::create_dir_all(sb.path(".mcc/state/work")).unwrap();
    let held = std::fs::File::create(sb.path(".mcc/state/work/refresh.lock")).unwrap();
    // SAFETY: flock only reads the descriptor, which `held` keeps open.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let stub = stub();
    let o = usage_all(&sb, &stub, &refreshed("tok-new-work-b2"), &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!sb.exists("p-args"), "a request was made");
    assert!(
        o.stderr.contains("a refresh of `work` is already running"),
        "{}",
        o.stderr
    );
    // `mcc login --refresh-only` waits for it too.
    let o = run(sb
        .mcc()
        .env("FAKE_REFRESH_CREDS", refreshed("tok-new-work-b2"))
        .args(["login", "work", "--refresh-only"]));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr.contains("a refresh of `work` is already running"),
        "{}",
        o.stderr
    );
    assert!(!sb.exists("p-args"), "a request was made");
    drop(held);
}

#[test]
fn a_failed_refresh_is_reported_and_not_tried_again_for_thirty_minutes() {
    let sb = sandbox();
    project_set(&sb, "work", &real_dir(&sb, "code/app"));
    let stub = stub();
    let o = run(sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .env("FAKE_REFRESH_MODE", "fail")
        .args(["usage", "--all"]));
    // Usage lookups never fail the command.
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(requests(&sb).len(), 1);
    assert!(
        o.stderr
            .contains("Claude Code could not refresh `work`'s token (exit 1)"),
        "{}",
        o.stderr
    );
    assert!(
        o.stderr
            .contains("start `work` in Claude Code once (`mcc run --profile work`) to refresh it"),
        "{}",
        o.stderr
    );
    assert!(
        row(&o.stdout, "work").contains("token expired"),
        "{}",
        o.stdout
    );
    assert!(sb.exists(".mcc/state/work/refresh-failed-at"));
    assert_no_refresh_only_hint(&o);

    // Right after: not tried again, even though it would work now.
    std::fs::remove_file(sb.path("p-args")).unwrap();
    let o = usage_all(&sb, &stub, &refreshed("tok-new-work-b2"), &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!sb.exists("p-args"), "a request was made");
    assert!(
        o.stderr
            .contains("refreshing `work` failed less than 30 minutes ago"),
        "{}",
        o.stderr
    );
    assert!(
        o.stderr.contains("`mcc run --profile work`"),
        "{}",
        o.stderr
    );
    assert_no_refresh_only_hint(&o);

    // Once that is over, it is.
    sb.write(
        ".mcc/state/work/refresh-failed-at",
        &common::rfc3339_in(-31 * 60),
    );
    let o = usage_all(&sb, &stub, &refreshed("tok-new-work-b2"), &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(sb.exists("p-args"), "{}", o.stderr);
    assert!(o.stderr.contains("refreshed `work`"), "{}", o.stderr);
    assert!(!sb.exists(".mcc/state/work/refresh-failed-at"));
}

#[test]
fn json_stays_clean_while_refreshing() {
    let sb = sandbox();
    project_set(&sb, "work", &real_dir(&sb, "code/app"));
    let stub = stub();
    for flag in ["--json", "--json-v2"] {
        let _ = std::fs::remove_file(sb.path("p-runs"));
        // Expired again, so each run refreshes it.
        sb.seed_login(
            ".mcc/profiles/work",
            "tok-old-work-a1",
            now_ms() - HOUR_MS,
            now_ms() + 20 * DAY_MS,
        );
        let o = usage_all(&sb, &stub, &refreshed("tok-new-work-b2"), &[flag]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(requests(&sb).len(), 1, "{}", o.stderr);
        assert!(
            o.stderr.contains("refreshing the token of `work`"),
            "{}",
            o.stderr
        );
        let v = json(o.stdout.as_bytes());
        let arr = if flag == "--json" {
            v.as_array().unwrap().clone()
        } else {
            v["profiles"].as_array().unwrap().clone()
        };
        let work = by_name(&arr, "work");
        assert_eq!(work["usage"]["status"], "ok", "{work}");
        assert_eq!(work["usage"]["five_hour"]["percent"], 33.0, "{work}");
        for text in ["refresh", "⚠", "Claude Code"] {
            assert!(!o.stdout.contains(text), "{text} on stdout: {}", o.stdout);
        }
        assert_no_token(&sb, &[&o]);
    }
}

#[test]
fn ctrl_c_during_a_refresh_stops_the_others_and_still_shows_the_table() {
    let sb = sandbox();
    sb.seed_login(
        ".claude",
        "tok-old-default-c3",
        now_ms() - HOUR_MS,
        now_ms() + 20 * DAY_MS,
    );
    project_set(&sb, "work", &real_dir(&sb, "code/app"));
    let stub = stub();
    let o = run(sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .env("FAKE_REFRESH_INTERRUPT", "1")
        .args(["usage", "--all"]));
    assert_eq!(o.code, 130, "{}", o.stderr);
    // default went first; work was never started.
    assert_eq!(requests(&sb).len(), 1, "{}", o.stderr);
    assert!(
        !o.stderr.contains("refreshing the token of `work`"),
        "{}",
        o.stderr
    );
    assert!(o.stdout.starts_with("PROFILE"), "{}", o.stdout);
    assert!(
        row(&o.stdout, "work").contains("token expired"),
        "{}",
        o.stdout
    );
    // An interruption is not a failure to wait out.
    assert!(!sb.exists(".mcc/state/default/refresh-failed-at"));
}
