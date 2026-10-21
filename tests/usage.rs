//! `mcc usage`: detailed 5-hour and weekly usage per profile.

mod common;

use common::{REFUSED_URL, by_name, json, stdout_of, usage_sandbox, usage_stub};
use predicates::prelude::*;

/// "(Sat Oct 10 16:00)": the reset time in local time.
const LOCAL_TIME: &str = r"\([A-Z][a-z]{2} [A-Z][a-z]{2} \d{1,2} \d{2}:\d{2}\)";

fn assert_match(re: &str, text: &str) {
    assert!(
        regex::Regex::new(re).unwrap().is_match(text),
        "{re}\n{text}"
    );
}

fn usage_json(out: &str) -> Vec<serde_json::Value> {
    json(out.as_bytes()).as_array().unwrap().clone()
}

/// Turn `show_email` on in the sandbox's config, under `table`
/// (`[privacy]`, or `[usage]`, its old home).
fn show_email_in_config(sb: &common::Sandbox, table: &str) {
    let cfg = sb.read(".mcc/config.toml");
    assert!(!cfg.contains("show_email"), "{cfg}");
    let head = format!("[{table}]\n");
    let cfg = if cfg.contains(&head) {
        cfg.replacen(&head, &format!("{head}show_email = true\n"), 1)
    } else {
        format!("{cfg}\n{head}show_email = true\n")
    };
    sb.write(".mcc/config.toml", &cfg);
}

#[test]
fn usage_shows_every_profile_as_a_table() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let out = stdout_of(sb.mcc().env("MCC_USAGE_URL", &stub.url).arg("usage"));

    assert_match(
        r"(?m)^PROFILE    5-HOUR          RESETS IN {18}WEEKLY          RESETS IN$",
        &out,
    );
    assert_match(
        &format!(
            r"(?m)^default ★  ████░░░░░░ 42%  2h 14m {LOCAL_TIME}  ██░░░░░░░░ 18%  3d 4h {LOCAL_TIME}$"
        ),
        &out,
    );
    assert_match(r"(?m)^lab        unavailable \(not logged in\)$", &out);
    assert_match(
        &format!(
            r"(?m)^work       █░░░░░░░░░ 7%   2h 14m {LOCAL_TIME}  ██████░░░░ 64%  3d 4h {LOCAL_TIME}$"
        ),
        &out,
    );
    // Header, then the default first and the profiles in name order.
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "{out}");
    assert!(lines[1].starts_with("default") && lines[2].starts_with("lab"));
    // Account emails are hidden by default.
    assert!(
        !out.contains("me@example.com") && !out.contains("ACCOUNT"),
        "{out}"
    );
    assert_eq!(stub.tokens(), ["test-token-default", "test-token-work"]);
}

#[test]
fn usage_shows_emails_only_when_asked() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let run = |args: &[&str]| stdout_of(sb.mcc().env("MCC_USAGE_URL", &stub.url).args(args));
    let shown = |out: &str| {
        assert_match(r"(?m)^PROFILE    ACCOUNT         5-HOUR ", out);
        assert_match(r"(?m)^default ★  me@example\.com  ████░░░░░░ 42%  ", out);
        assert_match(
            r"(?m)^lab        —               unavailable \(not logged in\)$",
            out,
        );
    };
    let hidden = |out: &str| assert!(!out.contains("me@example.com"), "{out}");

    shown(&run(&["usage", "--show-email"]));
    hidden(&run(&["usage", "--hide-email"]));
    sb.mcc()
        .args(["usage", "--show-email", "--hide-email"])
        .assert()
        .code(2);
    let json_account =
        |args: &[&str]| by_name(&usage_json(&run(args)), "default")["account"].clone();
    assert_eq!(json_account(&["usage", "--json"]), serde_json::Value::Null);
    assert_eq!(
        json_account(&["usage", "--json", "--show-email"]),
        "me@example.com"
    );

    // `[privacy] show_email = true` makes it the default, for `mcc list`
    // too (issue #57); --hide-email wins.
    show_email_in_config(&sb, "privacy");
    shown(&run(&["usage"]));
    assert_eq!(json_account(&["usage", "--json"]), "me@example.com");
    hidden(&run(&["usage", "--hide-email"]));
    let list = run(&["list"]);
    assert_match(r"(?m)^default ★\s+me@example\.com \(Acme\)\s", &list);
    assert_eq!(json_account(&["list", "--json"]), "me@example.com");
    hidden(&run(&["list", "--hide-email"]));
    // Other commands keep the setting when they save the config.
    sb.mcc()
        .args(["add", "other", "--fresh", "--no-login"])
        .assert()
        .success();
    assert!(sb.read(".mcc/config.toml").contains("show_email = true"));
}

/// Issue #57: `[usage] show_email = true`, the setting's old name, still
/// shows emails, in `mcc usage` and `mcc list` alike.
#[test]
fn legacy_usage_show_email_still_applies_to_both_commands() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let run = |args: &[&str]| stdout_of(sb.mcc().env("MCC_USAGE_URL", &stub.url).args(args));
    assert!(!run(&["list"]).contains("me@example.com"));
    show_email_in_config(&sb, "usage");
    for args in [&["usage"][..], &["list"], &["list", "--long"]] {
        assert!(run(args).contains("me@example.com"), "{args:?}");
    }
}

#[test]
fn usage_limits_output_to_one_profile() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let work = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .args(["usage", "work"]),
    );
    assert_match(r"(?m)^work     █░░░░░░░░░ 7%  ", &work);
    assert!(work.contains("64%"), "{work}");
    assert!(!work.contains("default") && !work.contains("lab"), "{work}");
    assert_eq!(work.lines().count(), 2, "{work}");
    // Only the selected profile's usage is fetched.
    assert_eq!(stub.tokens(), ["test-token-work"]);

    for alias in ["default", "claude"] {
        let out = stdout_of(
            sb.mcc()
                .env("MCC_USAGE_URL", &stub.url)
                .args(["usage", alias]),
        );
        assert_match(r"(?m)^default ★  ████░░░░░░ 42%  ", &out);
        assert_eq!(out.lines().count(), 2, "{out}");
    }
}

#[test]
fn usage_unknown_profile_is_a_usage_error() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["usage", "nope"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("no profile named \"nope\""))
        .stderr(predicate::str::contains(
            "run `mcc list` to see your profiles",
        ));
    assert!(stub.requests().is_empty());
}

#[test]
fn usage_unavailable_still_succeeds() {
    let sb = usage_sandbox();
    let out = stdout_of(sb.mcc().env("MCC_USAGE_URL", REFUSED_URL).arg("usage"));
    assert_match(
        r"(?m)^default ★  unavailable \(offline or the usage API timed out\)$",
        &out,
    );
    assert_match(
        r"(?m)^work       unavailable \(offline or the usage API timed out\)$",
        &out,
    );
    assert_match(r"(?m)^lab        unavailable \(not logged in\)$", &out);

    let arr = usage_json(&stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", REFUSED_URL)
            .args(["usage", "--json"]),
    ));
    assert_eq!(by_name(&arr, "default")["usage"]["reason"], "network");
    assert_eq!(by_name(&arr, "lab")["usage"]["reason"], "not_logged_in");
}

#[test]
fn usage_json_has_the_same_information() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let run = |args: &[&str]| {
        stdout_of(
            sb.mcc()
                .env_remove("NO_COLOR")
                .env("MCC_USAGE_URL", &stub.url)
                .args(args),
        )
    };
    let out = run(&["usage", "--json", "--show-email"]);
    for glyph in ['█', '░', '◆', '\x1b'] {
        assert!(!out.contains(glyph), "{glyph:?} in {out}");
    }
    let arr = usage_json(&out);
    let names: Vec<&str> = arr.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["default", "lab", "work"]);
    let keys: Vec<&str> = arr[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["name", "command", "is_default", "account", "usage"]);
    let d = by_name(&arr, "default");
    assert_eq!(d["command"], "claude");
    assert_eq!(d["is_default"], true);
    assert_eq!(d["account"], "me@example.com");
    assert_eq!(d["usage"]["status"], "ok");
    assert_eq!(d["usage"]["five_hour"]["percent"], 42.4);
    assert!(
        d["usage"]["five_hour"]["resets_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(d["usage"]["weekly"]["percent"], 18.0);
    let lab = by_name(&arr, "lab");
    assert_eq!(lab["is_default"], false);
    assert_eq!(lab["account"], serde_json::Value::Null);
    assert_eq!(lab["usage"]["status"], "unavailable");
    assert_eq!(lab["usage"]["reason"], "not_logged_in");

    // The usage objects are exactly those of `mcc list --json`.
    let list = usage_json(&run(&["list", "--json"]));
    for name in ["default", "lab", "work"] {
        assert_eq!(
            by_name(&arr, name)["usage"],
            by_name(&list, name)["usage"],
            "{name}"
        );
    }

    let one = usage_json(&run(&["usage", "work", "--json"]));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0]["name"], "work");
    assert_eq!(one[0]["usage"]["weekly"]["percent"], 64.0);
}

#[test]
fn usage_honors_mcc_no_usage() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let out = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("MCC_NO_USAGE", "1")
            .arg("usage"),
    );
    assert_eq!(
        out.matches("  — (not fetched: --no-usage or MCC_NO_USAGE)\n")
            .count(),
        3,
        "{out}"
    );
    assert!(!out.contains("unavailable"), "{out}");
    let arr = usage_json(&stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("MCC_NO_USAGE", "1")
            .args(["usage", "--json"]),
    ));
    for p in &arr {
        assert_eq!(p["usage"]["reason"], "disabled");
    }
    assert!(stub.requests().is_empty(), "{:?}", stub.requests());
}

#[test]
fn usage_drops_bars_in_narrow_terminals() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let out = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("COLUMNS", "40")
            .arg("usage"),
    );
    // No bar and no absolute reset time; the time until the reset stays.
    assert_match(r"(?m)^default ★  42%     2h 14m     18%     3d 4h$", &out);
    assert!(!out.contains('█') && !out.contains('░'), "{out}");
    let wide = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("COLUMNS", "500")
            .arg("usage"),
    );
    assert_match(&format!(r"████░░░░░░ 42%  2h 14m {LOCAL_TIME}"), &wide);
}

#[test]
fn usage_keeps_bars_at_80_columns() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let out = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("COLUMNS", "80")
            .arg("usage"),
    );
    // The absolute reset time goes before the bar shrinks, so the full
    // 10-cell bar still fits.
    assert_match(
        r"(?m)^default ★  ████░░░░░░ 42%  2h 14m     ██░░░░░░░░ 18%  3d 4h$",
        &out,
    );
    assert_match(
        r"(?m)^work       █░░░░░░░░░ 7%   2h 14m     ██████░░░░ 64%  3d 4h$",
        &out,
    );
    for line in out.lines() {
        assert!(console::measure_text_width(line) <= 80, "{line}");
    }
    // Also with the account column.
    let out = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("COLUMNS", "80")
            .args(["usage", "--show-email"]),
    );
    assert!(out.contains("me@example.com  ████░░░░░░ 42%"), "{out}");
    for line in out.lines() {
        assert!(console::measure_text_width(line) <= 80, "{line}");
    }
}

#[test]
fn usage_output_has_no_colour_codes_when_piped() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    for args in [&["usage"][..], &["--no-color", "usage"]] {
        let out = stdout_of(
            sb.mcc()
                .env_remove("NO_COLOR")
                .env("CLICOLOR_FORCE", "1")
                .env("MCC_USAGE_URL", &stub.url)
                .args(args),
        );
        assert!(out.contains('█') && out.contains("42%"), "{out}");
        assert!(!out.contains('\x1b'), "{out:?}");
    }
}

// ---- the statusline cache (the default) and the opt-in live lookup ----------

/// default + `work` logged in and `lab` logged out, the live lookup off (the
/// default): usage comes from what `mcc statusline` cached.
fn cache_sandbox() -> common::Sandbox {
    let sb = common::Sandbox::new();
    sb.seed_default();
    common::with_profile(&sb, "work", "--clone");
    common::with_profile(&sb, "lab", "--fresh");
    sb.seed_oauth(
        ".claude",
        "test-token-default",
        common::now_ms() + common::HOUR_MS,
    );
    sb.seed_oauth(
        ".mcc/profiles/work",
        "test-token-work",
        common::now_ms() + common::HOUR_MS,
    );
    sb.seed_keychain(
        "Claude Code-credentials",
        "test-token-default",
        common::now_ms() + common::HOUR_MS,
    );
    // default: both windows, cached 12 minutes ago; work: the weekly window
    // only, an hour ago; lab: nothing cached.
    common::seed_usage_cache(
        &sb,
        "default",
        Some((42.4, 2 * 3600 + 14 * 60)),
        Some((18.0, 3 * 86400 + 4 * 3600)),
        12 * 60 + 5,
    );
    common::seed_usage_cache(
        &sb,
        "work",
        None,
        Some((64.0, 3 * 86400 + 4 * 3600)),
        3600 + 5,
    );
    sb
}

/// Nothing may read a token or reach the network: the stub stays unasked and
/// the fake `security` is never run.
fn assert_no_token_lookup(sb: &common::Sandbox, stub: &common::UsageStub) {
    assert!(stub.requests().is_empty(), "{:?}", stub.requests());
    assert!(!sb.exists("security.log"), "the keychain was read");
}

#[test]
fn usage_reads_the_statusline_cache_by_default() {
    let sb = cache_sandbox();
    let stub = usage_stub();
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .arg("usage")
        .assert()
        .success();
    let out = String::from_utf8(o.get_output().stdout.clone()).unwrap();
    let err = String::from_utf8(o.get_output().stderr.clone()).unwrap();
    assert_match(r"(?m)^PROFILE .* RESETS IN\s+UPDATED$", &out);
    assert_match(
        &format!(
            r"(?m)^default ★  ████░░░░░░ 42%  2h 14m {LOCAL_TIME}  ██░░░░░░░░ 18%  3d 4h {LOCAL_TIME}  12m ago$"
        ),
        &out,
    );
    assert_match(
        &format!(r"(?m)^work       —\s+██████░░░░ 64%  3d 4h {LOCAL_TIME}  1h ago$"),
        &out,
    );
    // No cached data: a dash, and a hint to use /usage.
    assert_match(
        r"(?m)^lab        — \(no recent usage cached; run /usage in Claude Code\)$",
        &out,
    );
    assert!(err.contains("no recent usage cached for `lab`"), "{err}");
    assert!(err.contains("/usage"), "{err}");
    assert!(!err.contains("Live usage lookup"), "{err}");
    assert_no_token_lookup(&sb, &stub);

    let arr = usage_json(&stdout_of(sb.mcc().args(["usage", "--json"])));
    let d = &by_name(&arr, "default")["usage"];
    assert_eq!(d["status"], "ok");
    assert_eq!(d["source"], "cache");
    assert_eq!(d["five_hour"]["percent"], 42.4);
    assert!(d["updated_at"].as_str().unwrap().ends_with('Z'));
    let lab = &by_name(&arr, "lab")["usage"];
    assert_eq!(lab["status"], "unavailable");
    assert_eq!(lab["reason"], "no_data");
    assert_eq!(lab["source"], "cache");
}

#[test]
fn cached_usage_past_its_reset_is_obsolete() {
    let sb = cache_sandbox();
    // default's 5-hour window reset a minute ago; work's only window too.
    common::seed_usage_cache(
        &sb,
        "default",
        Some((99.0, -60)),
        Some((18.0, 3 * 86400)),
        6 * 3600,
    );
    common::seed_usage_cache(&sb, "work", None, Some((64.0, -60)), 8 * 86400);
    let arr = usage_json(&stdout_of(sb.mcc().args(["usage", "--json"])));
    let d = &by_name(&arr, "default")["usage"];
    assert_eq!(d["status"], "ok");
    assert_eq!(d["five_hour"], serde_json::Value::Null);
    assert_eq!(d["weekly"]["percent"], 18.0);
    assert_eq!(by_name(&arr, "work")["usage"]["reason"], "no_data");
    let out = stdout_of(sb.mcc().arg("usage"));
    assert_match(
        r"(?m)^work       — \(no recent usage cached; run /usage in Claude Code\)$",
        &out,
    );
}

#[test]
fn list_reads_the_statusline_cache_and_shows_its_age() {
    let sb = cache_sandbox();
    let stub = usage_stub();
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .arg("list")
        .assert()
        .success();
    let out = String::from_utf8(o.get_output().stdout.clone()).unwrap();
    let err = String::from_utf8(o.get_output().stderr.clone()).unwrap();
    // The table keeps to 120 columns; the age is in --long (issue #56).
    assert_match(r"(?m)^NAME\s+ACCOUNT\s+5H\s+WEEK\s+LAST USED\s+SYNC$", &out);
    assert_match(
        r"(?m)^default ★\s+Acme\s+████░░░░░░ 42% \(2h\)\s+██░░░░░░░░ 18% \(3d\)\s",
        &out,
    );
    assert_match(r"(?m)^lab\s+—\s+—\s+—\s", &out);
    assert!(err.contains("no recent usage cached for `lab`"), "{err}");

    let long = stdout_of(sb.mcc().args(["list", "--long"]));
    assert!(
        long.contains("Usage updated:   12m ago (statusline cache)"),
        "{long}"
    );
    assert!(
        long.contains("Usage:           — (no recent usage cached; run /usage in Claude Code)"),
        "{long}"
    );
    let arr = json(
        &sb.mcc()
            .args(["list", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let arr = arr.as_array().unwrap();
    assert_eq!(by_name(arr, "work")["usage"]["source"], "cache");
    assert_no_token_lookup(&sb, &stub);
}

#[test]
fn run_reads_the_statusline_cache_and_shows_its_age() {
    let sb = cache_sandbox();
    let stub = usage_stub();
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["run", "--dry-run"])
        .assert()
        .success();
    let err = String::from_utf8(o.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains(
            "would start `default` — no project default here (5h 42%, week 18% used · 12m ago)"
        ),
        "{err}"
    );
    // A cached window at its limit stops `mcc run` like a live one.
    common::seed_usage_cache(&sb, "default", Some((100.0, 35 * 60)), None, 60);
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["run", "--dry-run"])
        .assert()
        .stderr(predicate::str::contains(
            "`default` (no project default here) is at its 5-hour limit, resets in 35m",
        ));
    assert_no_token_lookup(&sb, &stub);
}

#[test]
fn live_lookup_shows_its_disclosure_once_and_records_it() {
    let sb = cache_sandbox();
    let stub = usage_stub();
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &cfg.replacen("[usage]\n", "[usage]\n# opted in by hand\nlive = true\n", 1),
    );
    // Commands that don't look usage up neither show nor record it.
    for args in [
        &["list", "--no-usage"][..],
        &["run", "--dry-run", "--profile", "work"],
    ] {
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .args(args)
            .assert()
            .success()
            .stderr(predicate::str::contains("Live usage lookup").not());
    }
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .env("MCC_NO_USAGE", "1")
        .arg("usage")
        .assert()
        .success()
        .stderr(predicate::str::contains("Live usage lookup").not());
    assert!(!sb.read(".mcc/config.toml").contains("live_disclosed_at"));
    assert!(stub.requests().is_empty());

    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .arg("usage")
        .assert()
        .success();
    let err = String::from_utf8(o.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("⚠ Live usage lookup is on ([usage] live = true"),
        "{err}"
    );
    assert!(err.contains("OAuth access token"), "{err}");
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("# opted in by hand\nlive = true\n"), "{cfg}");
    assert!(
        regex::Regex::new(r#"(?m)^live_disclosed_at = "\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ"$"#)
            .unwrap()
            .is_match(&cfg),
        "{cfg}"
    );
    // Live numbers, no UPDATED column.
    let out = String::from_utf8(o.get_output().stdout.clone()).unwrap();
    assert!(!out.contains("UPDATED"), "{out}");
    assert_eq!(stub.tokens(), ["test-token-default", "test-token-work"]);

    // Shown once: later commands look usage up quietly.
    for args in [&["usage"][..], &["list"]] {
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .args(args)
            .assert()
            .success()
            .stderr(predicate::str::contains("Live usage lookup").not());
    }
    let arr = usage_json(&stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .args(["usage", "--json"]),
    ));
    assert_eq!(by_name(&arr, "default")["usage"]["source"], "live");
}

#[test]
fn live_lookup_reads_the_keychain_without_user() {
    use sha2::{Digest, Sha256};
    let sb = usage_sandbox();
    // `work` is logged in through the keychain only.
    std::fs::remove_file(sb.path(".mcc/profiles/work/.credentials.json")).unwrap();
    let work_dir = sb.path(".mcc/profiles/work").display().to_string();
    let hash: String = Sha256::digest(work_dir.as_bytes())[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let service = format!("Claude Code-credentials-{hash}");
    sb.seed_keychain(
        &service,
        "test-token-work",
        common::now_ms() + common::HOUR_MS,
    );
    // The login name, as the system knows it.
    let id = std::process::Command::new("id")
        .arg("-un")
        .output()
        .unwrap();
    let account = String::from_utf8(id.stdout).unwrap().trim().to_string();
    assert!(!account.is_empty());
    let stub = usage_stub();

    // launchd, cron and IDE tasks may run without USER.
    let arr = usage_json(&stdout_of(
        sb.mcc()
            .env_remove("USER")
            .env("FAKE_KEYCHAIN_ACCOUNT", &account)
            .env("MCC_USAGE_URL", &stub.url)
            .args(["usage", "--json"]),
    ));
    let work = &by_name(&arr, "work")["usage"];
    assert_eq!(work["status"], "ok", "{work}");
    assert_eq!(work["weekly"]["percent"], 64.0);
    assert_eq!(stub.tokens(), ["test-token-default", "test-token-work"]);
    let calls = sb.read("security.log");
    assert!(
        calls.contains(&format!(
            "find-generic-password -s {service} -a {account} -w"
        )),
        "{calls}"
    );
}

// ---- `mcc usage --all`: every account looked up live, on demand (#94) ------

/// The risk notice `mcc usage --all` prints each time, verbatim (issue #94).
const ALL_NOTE: &str = "⚠ --all looks usage up live for every account, the default included: mcc reads each profile's Claude Code login (its OAuth access token, from .credentials.json or the macOS Keychain) and sends it to Anthropic's undocumented, rate-limited usage endpoint (https://api.anthropic.com/api/oauth/usage). Anthropic says third-party tools may not collect or intermediate Claude.ai credentials (https://code.claude.com/docs/en/legal-and-compliance), so use this at your own discretion, only with accounts you own, and never to get around usage limits. Without --all, `mcc usage` reads the `mcc statusline` cache (unless `[usage] live = true`).";

/// True when `stderr` carries the `--all` notice as a line of its own.
fn has_all_note(stderr: &str) -> bool {
    stderr.lines().any(|l| l == ALL_NOTE)
}

/// `mcc usage --all` with the stub as the usage endpoint: (stdout, stderr).
fn usage_all(sb: &common::Sandbox, stub: &common::UsageStub, args: &[&str]) -> (String, String) {
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(args)
        .assert()
        .success();
    let out = o.get_output();
    (
        String::from_utf8(out.stdout.clone()).unwrap(),
        String::from_utf8(out.stderr.clone()).unwrap(),
    )
}

#[test]
fn usage_all_looks_every_account_up_live_with_a_risk_notice() {
    // The live lookup is off in the config: --all alone turns it on.
    let sb = cache_sandbox();
    let stub = usage_stub();
    let cfg = sb.read(".mcc/config.toml");
    let (out, err) = usage_all(&sb, &stub, &["usage", "--all"]);

    // Every account, the default included, with its own login.
    assert_eq!(stub.tokens(), ["test-token-default", "test-token-work"]);
    // Live numbers: no UPDATED column, work's 5-hour window (not cached),
    // and the logged-out `lab` is unavailable rather than uncached.
    assert!(!out.contains("UPDATED"), "{out}");
    assert_match(
        &format!(
            r"(?m)^default ★  ████░░░░░░ 42%  2h 14m {LOCAL_TIME}  ██░░░░░░░░ 18%  3d 4h {LOCAL_TIME}$"
        ),
        &out,
    );
    assert_match(
        &format!(
            r"(?m)^work       █░░░░░░░░░ 7%   2h 14m {LOCAL_TIME}  ██████░░░░ 64%  3d 4h {LOCAL_TIME}$"
        ),
        &out,
    );
    assert_match(r"(?m)^lab        unavailable \(not logged in\)$", &out);

    // The risk notice, on a line of its own; not the config's disclosure,
    // and nothing written: no opt-in, no acknowledgement.
    assert!(has_all_note(&err), "{err}");
    assert!(!err.contains("Live usage lookup is on"), "{err}");
    assert_eq!(sb.read(".mcc/config.toml"), cfg);

    // Each time, not once.
    let (_, err) = usage_all(&sb, &stub, &["usage", "--all"]);
    assert!(has_all_note(&err), "{err}");
    assert_eq!(stub.tokens().len(), 4);
}

#[test]
fn usage_all_keeps_the_notice_off_stdout_and_on_with_quiet() {
    let sb = cache_sandbox();
    let stub = usage_stub();
    // --json: the notice goes to stderr only, so stdout stays parseable.
    let (out, err) = usage_all(&sb, &stub, &["usage", "--all", "--json"]);
    let arr = usage_json(&out);
    assert_eq!(arr.len(), 3, "{out}");
    let d = &by_name(&arr, "default")["usage"];
    assert_eq!(d["source"], "live");
    assert_eq!(d["updated_at"], serde_json::Value::Null);
    assert_eq!(by_name(&arr, "work")["usage"]["five_hour"]["percent"], 7.0);
    assert_eq!(by_name(&arr, "lab")["usage"]["reason"], "not_logged_in");
    assert!(!out.contains('⚠'), "{out}");
    assert!(has_all_note(&err), "{err}");

    // --quiet drops hints, never the notice.
    let (out, err) = usage_all(&sb, &stub, &["-q", "usage", "--all"]);
    assert!(out.contains("42%"), "{out}");
    assert!(has_all_note(&err), "{err}");
}

#[test]
fn usage_all_respects_mcc_no_usage_and_takes_no_profile() {
    let sb = cache_sandbox();
    let stub = usage_stub();
    // MCC_NO_USAGE wins: no lookup, so no notice either.
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .env("MCC_NO_USAGE", "1")
        .args(["usage", "--all"])
        .assert()
        .success();
    let out = String::from_utf8(o.get_output().stdout.clone()).unwrap();
    let err = String::from_utf8(o.get_output().stderr.clone()).unwrap();
    assert_eq!(
        out.matches("  — (not fetched: --no-usage or MCC_NO_USAGE)\n")
            .count(),
        3,
        "{out}"
    );
    assert!(!err.contains("⚠ --all"), "{err}");

    // One profile, or every account: not both (a usage error).
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["usage", "work", "--all"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--all").and(predicate::str::contains("⚠").not()));
    assert_no_token_lookup(&sb, &stub);
}

#[test]
fn usage_help_documents_all_and_its_notice() {
    let sb = common::Sandbox::new();
    let help = stdout_of(sb.mcc().args(["usage", "--help"]));
    for needle in [
        "--all",
        "mcc usage --all",
        "risk notice",
        "MCC_NO_USAGE=1 still skips usage",
    ] {
        assert!(help.contains(needle), "missing {needle:?} in:\n{help}");
    }
    // clap wraps long lines; the note's words are all there, in order.
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words(&help).contains(&words(ALL_NOTE)), "{help}");
}
