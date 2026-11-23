//! `mcc list`, `mcc sync`, `mcc delete`, and cross-cutting error behavior.

mod common;

use std::time::Duration;

use common::{
    HOUR_MS, REFUSED_URL, Sandbox, UsageStub, by_name, enable_live_usage, json, now_ms, rfc3339_in,
    seed_usage_cache, stdout_of, usage_sandbox, usage_stub, with_profile,
};
use predicates::prelude::*;

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
    // Emails are hidden by default (issue #57).
    assert!(d["account"].is_null(), "{d}");
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
        .stdout(
            predicate::str::is_match(r"(?m)^# NAME\s+ACCOUNT\s+5H\s+WEEK\s+LAST USED\s+SYNC$")
                .unwrap(),
        )
        .stdout(predicate::str::is_match(r"(?m)^\d+ default ★\s+Acme\s").unwrap())
        .stdout(predicate::str::contains("me@example.com").not())
        .stdout(predicate::str::is_match(r"(?m)^\d+ work\s+").unwrap())
        .stderr(predicate::str::contains("2 profiles"));
    sb.mcc()
        .args(["list", "--long"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Command:         claude"))
        .stdout(predicate::str::is_match(r"Skills:\s+2").unwrap())
        .stdout(predicate::str::is_match(r"Agents:\s+1").unwrap())
        .stdout(predicate::str::is_match(r"Commands:\s+1").unwrap())
        .stdout(predicate::str::is_match(r"MCP servers:\s+1").unwrap())
        .stdout(predicate::str::is_match(r"Hooks:\s+1").unwrap())
        .stdout(predicate::str::contains("Size:"))
        .stdout(predicate::str::is_match(r"Plugins:\s+1 enabled / 2 installed").unwrap())
        .stdout(predicate::str::is_match(r"Projects:\s+1 \(1 sessions\)").unwrap())
        .stdout(predicate::str::contains("Last sync:"));
    sb.mcc().args(["list", "--json", "--long"]).assert().code(2);
}

/// Issue #57: `mcc list` hides account emails by default, like `mcc usage`,
/// in every output; `--show-email` shows them for one run. An organization
/// named after the email (as personal plans' are) is hidden with it.
#[test]
fn list_hides_emails_unless_asked() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "solo", "--fresh");
    sb.write(
        ".mcc/profiles/solo/.claude.json",
        r#"{"oauthAccount":{"emailAddress":"solo@example.com","organizationName":"solo@example.com's Organization"}}"#,
    );
    // `mcc add` offers the organization as the label.
    sb.mcc()
        .args([
            "config",
            "set",
            "profiles.solo.label",
            "solo@example.com's Organization",
        ])
        .assert()
        .success();
    let list = |args: &[&str]| stdout_of(sb.mcc().arg("list").arg("--no-usage").args(args));

    let table = list(&[]);
    assert!(!table.contains('@'), "{table}");
    assert!(
        regex::Regex::new(r"(?m)^\d+ default ★\s+Acme\s")
            .unwrap()
            .is_match(&table),
        "{table}"
    );
    assert!(
        regex::Regex::new(r"(?m)^\d+ solo\s+—\s")
            .unwrap()
            .is_match(&table),
        "{table}"
    );
    let long = list(&["--long"]);
    assert!(!long.contains('@'), "{long}");
    assert!(
        long.contains("Account:         Acme (email hidden; --show-email shows it)"),
        "{long}"
    );
    assert!(
        long.contains("Account:         email hidden (--show-email shows it)"),
        "{long}"
    );
    for args in [&["--json"][..], &["--json-v2"]] {
        let out = list(args);
        assert!(!out.contains('@'), "{out}");
        let v = json(out.as_bytes());
        let arr = v.get("profiles").unwrap_or(&v).as_array().unwrap();
        // The fields stay, null (the schema only ever adds fields).
        let d = by_name(arr, "default");
        assert!(d["account"].is_null(), "{d}");
        assert_eq!(d["organization"], "Acme");
        let s = by_name(arr, "solo");
        assert!(s.get("account").is_some_and(|a| a.is_null()), "{s}");
        assert!(s.get("organization").is_some_and(|o| o.is_null()), "{s}");
        assert!(s.get("label").is_some_and(|l| l.is_null()), "{s}");
    }

    let shown = list(&["--show-email"]);
    assert!(shown.contains("me@example.com (Acme)"), "{shown}");
    // Long cells are cut to keep the table narrow.
    assert!(
        shown.contains("solo@example.com (solo@example.…"),
        "{shown}"
    );
    let v = json(list(&["--json-v2", "--show-email"]).as_bytes());
    assert_eq!(
        by_name(v["profiles"].as_array().unwrap(), "default")["account"],
        "me@example.com"
    );
    assert!(list(&["--long", "--show-email"]).contains("Account:         me@example.com (Acme)"));
}

/// Issue #56: the default table has NAME · ACCOUNT · 5H · WEEK · LAST USED
/// · SYNC, and fits 120 columns even with long values.
#[test]
fn list_default_table_fits_120_columns() {
    let sb = Sandbox::new();
    sb.seed_default();
    let alias = "acme-enterprise-prod";
    with_profile(&sb, alias, "--clone");
    sb.write(
        &format!(".mcc/profiles/{alias}/.claude.json"),
        r#"{"oauthAccount":{"emailAddress":"firstname.lastname@acme-enterprise.example.com","organizationName":"Acme Enterprise Holdings International"}}"#,
    );
    sb.mcc()
        .args([
            "config",
            "set",
            &format!("profiles.{alias}.label"),
            "Acme Enterprise (production)",
        ])
        .assert()
        .success();
    for name in ["default", alias] {
        seed_usage_cache(
            &sb,
            name,
            Some((96.0, 4 * 3600)),
            Some((99.5, 6 * 86400)),
            60,
        );
    }
    // Drift: 3 items behind, and local edits.
    sb.write(".claude/agents/reviewer.md", "agent v2");
    sb.write(".claude/commands/deploy.md", "cmd v2");
    sb.write(".claude/skills/gamma/SKILL.md", "new");
    sb.write(&format!(".mcc/profiles/{alias}/CLAUDE.md"), "# mine\n");
    sb.write(&format!(".mcc/profiles/{alias}/hooks/notify.sh"), "mine");

    for email in ["--hide-email", "--show-email"] {
        let table = stdout_of(sb.mcc().env("COLUMNS", "120").args(["list", email]));
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(
            lines[0].split_whitespace().collect::<Vec<_>>(),
            ["#", "NAME", "ACCOUNT", "5H", "WEEK", "LAST", "USED", "SYNC"],
            "{table}"
        );
        let row = lines
            .iter()
            .find(|l| common::unnumbered(l).starts_with(alias))
            .expect(&table);
        assert!(
            row.contains("96%!!") && row.contains("3 behind, "),
            "{table}"
        );
        for l in &lines {
            let w = console::measure_text_width(l);
            assert!(w <= 120, "{w} columns: {l}\n{table}");
        }
    }
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

// ---- list: 5-hour / weekly usage ---------------------------------------------

fn list_json(sb: &Sandbox, url: &str) -> Vec<serde_json::Value> {
    let out = sb
        .mcc()
        .env("MCC_USAGE_URL", url)
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    json(&out).as_array().unwrap().clone()
}

#[test]
fn list_shows_usage_for_every_profile() {
    let sb = usage_sandbox();
    let stub = usage_stub();

    let arr = list_json(&sb, &stub.url);
    let d = &by_name(&arr, "default")["usage"];
    assert_eq!(d["status"], "ok");
    assert_eq!(d["reason"], serde_json::Value::Null);
    assert_eq!(d["five_hour"]["percent"], 42.4);
    assert!(d["five_hour"]["resets_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(d["weekly"]["percent"], 18.0);
    let w = &by_name(&arr, "work")["usage"];
    assert_eq!(w["status"], "ok");
    assert_eq!(w["five_hour"]["percent"], 7.0);
    assert_eq!(w["weekly"]["percent"], 64.0);
    let l = &by_name(&arr, "lab")["usage"];
    assert_eq!(l["status"], "unavailable");
    assert_eq!(l["reason"], "not_logged_in");
    assert_eq!(l["five_hour"], serde_json::Value::Null);
    assert_eq!(l["weekly"], serde_json::Value::Null);

    // Each profile's own token, exactly once, with the endpoint's headers.
    assert_eq!(stub.tokens(), ["test-token-default", "test-token-work"]);
    for r in stub.requests() {
        assert_eq!(r.request_line, "GET /api/oauth/usage HTTP/1.1");
        assert_eq!(r.beta.as_deref(), Some("oauth-2025-04-20"));
    }

    let table = stdout_of(sb.mcc().env("MCC_USAGE_URL", &stub.url).arg("list"));
    let has = |re: &str| {
        assert!(
            regex::Regex::new(re).unwrap().is_match(&table),
            "{re}\n{table}"
        )
    };
    has(r"(?m)^# NAME\s+ACCOUNT\s+5H\s+WEEK\s+LAST USED\s+SYNC$");
    has(r"(?m)^\d+ default ★\s+Acme\s+████░░░░░░ 42% \(2h\)\s+██░░░░░░░░ 18% \(3d\)\s");
    has(r"(?m)^\d+ work\s+—\s+█░░░░░░░░░ 7% \(2h\)\s+██████░░░░ 64% \(3d\)\s");
    has(r"(?m)^\d+ lab\s+—\s+unavailable\s+unavailable\s");

    let long = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .args(["list", "--long"]),
    );
    assert!(
        long.contains("5h usage:        ████████░░░░░░░░░░░░ 42% (resets in 2h 14m)"),
        "{long}"
    );
    assert!(
        long.contains("Weekly usage:    ████░░░░░░░░░░░░░░░░ 18% (resets in 3d 4h)"),
        "{long}"
    );
    assert!(
        long.contains("5h usage:        █░░░░░░░░░░░░░░░░░░░ 7% (resets in 2h 14m)"),
        "{long}"
    );
    assert!(
        long.contains("Usage:           unavailable (not logged in)"),
        "{long}"
    );
}

#[test]
fn list_usage_unavailable_offline_still_succeeds() {
    let sb = usage_sandbox();
    let arr = list_json(&sb, REFUSED_URL);
    assert_eq!(arr.len(), 3);
    for name in ["default", "work"] {
        let u = &by_name(&arr, name)["usage"];
        assert_eq!(u["status"], "unavailable", "{name}");
        assert_eq!(u["reason"], "network", "{name}");
    }
    assert_eq!(by_name(&arr, "lab")["usage"]["reason"], "not_logged_in");

    sb.mcc()
        .arg("list")
        .assert()
        .success()
        .stdout(
            predicate::str::is_match(r"(?m)^\d+ default ★\s+Acme\s+unavailable\s+unavailable\s")
                .unwrap(),
        )
        .stderr(predicate::str::contains("unavailable").not())
        .stderr(predicate::str::contains("3 profiles"));
    sb.mcc()
        .args(["list", "--long"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Usage:           unavailable (offline or the usage API timed out)",
        ));
}

#[test]
fn list_never_sends_expired_tokens() {
    let sb = usage_sandbox();
    sb.seed_oauth(".claude", "test-token-default", now_ms() - 1000);
    sb.seed_oauth(".mcc/profiles/work", "test-token-work", now_ms() - HOUR_MS);
    let stub = usage_stub();
    let arr = list_json(&sb, &stub.url);
    assert!(stub.requests().is_empty(), "{:?}", stub.tokens());
    for name in ["default", "work"] {
        assert_eq!(
            by_name(&arr, name)["usage"]["reason"],
            "token_expired",
            "{name}"
        );
    }
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["list", "--long"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "unavailable (token expired; start the profile in Claude Code to refresh it)",
        ));
    assert!(stub.requests().is_empty(), "{:?}", stub.requests());
}

#[test]
fn list_maps_usage_api_errors_to_reasons() {
    let sb = Sandbox::new();
    sb.seed_default();
    let cases = [
        ("a401", "token_expired"),
        ("a403", "unauthorized"),
        ("a429", "rate_limited"),
        ("a500", "http_error"),
        ("abad", "parse_error"),
    ];
    for (alias, _) in cases {
        sb.mcc()
            .args(["add", alias, "--fresh", "--no-login"])
            .assert()
            .success();
        sb.seed_oauth(
            &format!(".mcc/profiles/{alias}"),
            &format!("test-token-{alias}"),
            now_ms() + HOUR_MS,
        );
    }
    enable_live_usage(&sb);
    let stub = UsageStub::start(|req| {
        let auth = req.authorization.clone().unwrap_or_default();
        // Error bodies echo the token: none of it may reach the output.
        match auth.trim_start_matches("Bearer test-token-") {
            "a401" => (401, format!(r#"{{"error":"{auth}"}}"#)),
            "a403" => (403, format!(r#"{{"error":"{auth}"}}"#)),
            "a429" => (429, "{}".into()),
            "a500" => (500, format!("boom {auth}")),
            _ => (200, format!("not json {auth}")),
        }
    });
    let arr = list_json(&sb, &stub.url);
    for (alias, reason) in cases {
        let u = &by_name(&arr, alias)["usage"];
        assert_eq!(u["status"], "unavailable", "{alias}");
        assert_eq!(u["reason"], reason, "{alias}");
    }
    assert_eq!(by_name(&arr, "default")["usage"]["reason"], "not_logged_in");
    assert_eq!(stub.requests().len(), cases.len());
}

#[test]
fn list_no_usage_skips_the_fetch() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let arr = {
        let out = sb
            .mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .args(["list", "--json", "--no-usage"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        json(&out).as_array().unwrap().clone()
    };
    for p in &arr {
        assert_eq!(p["usage"]["status"], "unavailable");
        assert_eq!(p["usage"]["reason"], "disabled");
    }
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .env("MCC_NO_USAGE", "1")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"(?m)^\d+ default ★\s+Acme\s+—\s+—\s").unwrap())
        .stdout(predicate::str::contains("unavailable").not());
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["list", "--long", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Usage:           — (not fetched: --no-usage or MCC_NO_USAGE)",
        ));
    assert!(stub.requests().is_empty());
    // MCC_NO_USAGE=0 does not disable it.
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .env("MCC_NO_USAGE", "0")
        .arg("list")
        .assert()
        .success();
    assert_eq!(stub.requests().len(), 2);
}

#[test]
fn list_reads_tokens_from_the_keychain_per_profile() {
    use sha2::{Digest, Sha256};
    let sb = usage_sandbox();
    // Logged in through the keychain only (as Claude Code does on macOS).
    std::fs::remove_file(sb.path(".claude/.credentials.json")).unwrap();
    std::fs::remove_file(sb.path(".mcc/profiles/work/.credentials.json")).unwrap();
    let work_dir = sb.path(".mcc/profiles/work").display().to_string();
    let hash: String = Sha256::digest(work_dir.as_bytes())[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let work_service = format!("Claude Code-credentials-{hash}");
    sb.seed_keychain(
        "Claude Code-credentials",
        "test-token-default",
        now_ms() + HOUR_MS,
    );
    sb.seed_keychain(&work_service, "test-token-work", now_ms() + HOUR_MS);
    let stub = usage_stub();

    let arr = list_json(&sb, &stub.url);
    assert_eq!(
        by_name(&arr, "default")["usage"]["five_hour"]["percent"],
        42.4
    );
    assert_eq!(by_name(&arr, "work")["usage"]["weekly"]["percent"], 64.0);
    assert_eq!(by_name(&arr, "lab")["usage"]["reason"], "not_logged_in");
    assert_eq!(stub.tokens(), ["test-token-default", "test-token-work"]);
    let calls = sb.read("security.log");
    assert!(
        calls.contains(&format!(
            "find-generic-password -s {work_service} -a mcc-test -w"
        )),
        "{calls}"
    );
    assert!(
        calls.contains("-s Claude Code-credentials -a mcc-test -w"),
        "{calls}"
    );
}

/// Tokens never appear in stdout, stderr, or the log, whatever the outcome.
#[test]
fn list_never_leaks_tokens() {
    let sb = usage_sandbox();
    sb.mcc()
        .args(["add", "gone", "--fresh", "--no-login"])
        .assert()
        .success();
    sb.mcc()
        .args(["add", "denied", "--fresh", "--no-login"])
        .assert()
        .success();
    sb.seed_oauth(".mcc/profiles/gone", "test-token-gone", now_ms() - HOUR_MS);
    sb.seed_oauth(
        ".mcc/profiles/denied",
        "test-token-denied",
        now_ms() + HOUR_MS,
    );
    let ok = usage_stub();
    // Echo the Authorization header back in every error body.
    let echo = UsageStub::start(|req| {
        let auth = req.authorization.clone().unwrap_or_default();
        if auth.ends_with("denied") {
            (401, format!(r#"{{"error":"bad {auth}"}}"#))
        } else {
            (500, format!("oops {auth}"))
        }
    });
    let mut seen = String::new();
    for url in [
        ok.url.as_str(),
        echo.url.as_str(),
        REFUSED_URL,
        "http://example.invalid/",
    ] {
        for args in [
            &["list"][..],
            &["list", "--long"],
            &["list", "--json"],
            &["-v", "list"],
            &["-v", "list", "--long"],
        ] {
            let out = sb
                .mcc()
                .env("MCC_USAGE_URL", url)
                .args(args)
                .assert()
                .success();
            let o = out.get_output();
            seen.push_str(&String::from_utf8_lossy(&o.stdout));
            seen.push_str(&String::from_utf8_lossy(&o.stderr));
        }
    }
    seen.push_str(&sb.read(".mcc/logs/mcc.log"));
    assert!(seen.contains("42%"), "the ok path ran");
    assert!(!ok.requests().is_empty() && !echo.requests().is_empty());
    for t in ["default", "work", "gone", "denied"] {
        assert!(
            !seen.contains(&format!("test-token-{t}")),
            "test-token-{t} leaked"
        );
    }
    assert!(!seen.contains("refresh-test-token"), "refresh token leaked");
    assert!(!seen.contains("Bearer"), "authorization header leaked");
    // A non-loopback http override is refused: nothing is sent anywhere.
    assert!(seen.contains("MCC_USAGE_URL refused"));
}

// ---- list: usage meters -----------------------------------------------------

/// Only the default profile, logged in; the stub reports 85% / 96% used.
fn near_limit() -> (Sandbox, UsageStub) {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.seed_oauth(".claude", "test-token-default", now_ms() + HOUR_MS);
    enable_live_usage(&sb);
    let (five, week) = (
        rfc3339_in(2 * 3600 + 14 * 60),
        rfc3339_in(3 * 86400 + 4 * 3600),
    );
    let stub = UsageStub::start(move |_| {
        (
            200,
            format!(
                r#"{{"five_hour":{{"utilization":85,"resets_at":"{five}"}},"seven_day":{{"utilization":96,"resets_at":"{week}"}}}}"#
            ),
        )
    });
    (sb, stub)
}

#[test]
fn list_marks_near_limit_usage() {
    let (sb, stub) = near_limit();
    let table = stdout_of(sb.mcc().env("MCC_USAGE_URL", &stub.url).arg("list"));
    assert!(
        regex::Regex::new(
            r"(?m)^\d+ default ★\s+Acme\s+█████████░ 85%! \(2h\)\s+██████████ 96%!! \(3d\)\s"
        )
        .unwrap()
        .is_match(&table),
        "{table}"
    );
    let long = stdout_of(
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .args(["list", "--long"]),
    );
    assert!(
        long.contains("5h usage:        █████████████████░░░ 85%! (resets in 2h 14m)"),
        "{long}"
    );
    assert!(
        long.contains("Weekly usage:    ███████████████████░ 96%!! (resets in 3d 4h)"),
        "{long}"
    );
}

#[test]
fn list_drops_bars_in_narrow_terminals() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let list = |columns: &str, args: &[&str]| {
        stdout_of(
            sb.mcc()
                .env("MCC_USAGE_URL", &stub.url)
                .env("COLUMNS", columns)
                .args(args),
        )
    };
    let narrow = list("40", &["list"]);
    assert!(
        regex::Regex::new(r"(?m)^\d+ default ★\s+Acme\s+42% \(2h\)\s+18% \(3d\)\s")
            .unwrap()
            .is_match(&narrow),
        "{narrow}"
    );
    let long = list("40", &["list", "--long"]);
    assert!(
        long.contains("5h usage:        42% (resets in 2h 14m)"),
        "{long}"
    );
    for out in [&narrow, &long] {
        assert!(!out.contains('█') && !out.contains('░'), "{out}");
    }
    // 120 columns fit the full bars (issue #56).
    let medium = list("120", &["list"]);
    assert!(
        regex::Regex::new(
            r"(?m)^\d+ default ★\s+Acme\s+████░░░░░░ 42% \(2h\)\s+██░░░░░░░░ 18% \(3d\)\s"
        )
        .unwrap()
        .is_match(&medium),
        "{medium}"
    );
    // A wide terminal gets the full bars.
    assert!(list("500", &["list"]).contains("████░░░░░░ 42% (2h)"));
    assert!(list("500", &["list", "--long"]).contains("████████░░░░░░░░░░░░ 42% (resets"));
}

#[test]
fn list_meters_are_plain_text_without_colour() {
    let (sb, stub) = near_limit();
    for args in [&["list"][..], &["list", "--long"], &["--no-color", "list"]] {
        // Piped (no TTY), even without NO_COLOR: no escape codes, the bar
        // and the `!` markers carry the meaning.
        let out = stdout_of(
            sb.mcc()
                .env_remove("NO_COLOR")
                .env("CLICOLOR_FORCE", "1")
                .env("MCC_USAGE_URL", &stub.url)
                .args(args),
        );
        assert!(out.contains('█') && out.contains("96%!!"), "{out}");
        assert!(!out.contains('\x1b'), "{out:?}");
    }
}

#[test]
fn list_json_is_unchanged_by_meters() {
    let (sb, stub) = near_limit();
    let out = stdout_of(
        sb.mcc()
            .env_remove("NO_COLOR")
            .env("MCC_USAGE_URL", &stub.url)
            .args(["list", "--json"]),
    );
    for glyph in ['█', '░', '!', '\x1b'] {
        assert!(!out.contains(glyph), "{glyph:?} in {out}");
    }
    let v = json(out.as_bytes());
    let u = &v[0]["usage"];
    let keys: Vec<&str> = u.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "status",
            "reason",
            "five_hour",
            "weekly",
            "source",
            "updated_at"
        ]
    );
    assert_eq!(u["source"], "live");
    assert_eq!(u["updated_at"], serde_json::Value::Null);
    assert_eq!(u["five_hour"]["percent"], 85.0);
    assert_eq!(u["weekly"]["percent"], 96.0);
}

// ---- sync -------------------------------------------------------------------

#[test]
fn sync_dry_run_then_apply() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");

    // No manifest yet, but only additions: nothing local is replaced (#87).
    sb.mcc()
        .args(["sync", "work", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("dry run"))
        .stderr(predicate::str::contains("+ skills/alpha/SKILL.md"))
        .stderr(predicate::str::contains("would be replaced").not());
    assert!(!sb.exists(".mcc/profiles/work/skills"));

    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("now matches the default"))
        .stderr(predicate::str::contains("will be replaced").not());
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

/// rc files symlinked into a dotfiles repo (GNU stow, yadm) must stay links:
/// the alias block is written to the link's target, never over the link.
#[cfg(unix)]
#[test]
fn add_and_delete_keep_symlinked_rc_file() {
    let sb = Sandbox::new();
    sb.seed_default();
    let original = sb.read(".zshrc");
    std::fs::remove_file(sb.path(".zshrc")).unwrap();
    sb.write("dotfiles/zsh/.zshrc", &original);
    // Relative link, as stow creates them.
    std::os::unix::fs::symlink("dotfiles/zsh/.zshrc", sb.path(".zshrc")).unwrap();
    let is_link = || {
        std::fs::symlink_metadata(sb.path(".zshrc"))
            .unwrap()
            .file_type()
            .is_symlink()
    };

    sb.mcc()
        .args(["add", "work", "--fresh", "--no-login"])
        .assert()
        .success();
    assert!(is_link(), ".zshrc was replaced by a regular file");
    let target = sb.read("dotfiles/zsh/.zshrc");
    assert!(target.contains("# >>> mcc:work >>>"), "{target}");
    assert!(target.starts_with(&original), "{target}");

    sb.mcc()
        .args(["delete", "work", "--yes"])
        .assert()
        .success();
    assert!(is_link(), ".zshrc was replaced by a regular file");
    let target = sb.read("dotfiles/zsh/.zshrc");
    assert!(!target.contains("mcc:work"), "{target}");
    assert!(target.contains("alias cus_luong="), "{target}");
    // No temp files left behind next to the link or the target.
    for dir in [sb.home().to_path_buf(), sb.path("dotfiles/zsh")] {
        for e in std::fs::read_dir(dir).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.contains("mcc-tmp"), "leftover {name}");
        }
    }
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
        .env("MCC_NO_USAGE", "1")
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

/// The backup folders under `<profile>/.mcc-backup/`, oldest first.
fn backup_dirs(sb: &Sandbox, alias: &str) -> Vec<std::path::PathBuf> {
    let root = sb.path(&format!(".mcc/profiles/{alias}/.mcc-backup"));
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
    dirs.sort();
    dirs
}

/// Regression for #30: profile-local edits a sync overwrites are recoverable.
#[test]
fn sync_backs_up_every_file_it_overwrites() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.write(".claude/skills/one/SKILL.md", "one");
    with_profile(&sb, "work", "--clone");
    sb.write(".mcc/profiles/work/CLAUDE.md", "# my edited rules\n");
    sb.write(".mcc/profiles/work/skills/one/SKILL.md", "my edited one");
    // Changed in the default too: conflicts, which --force overwrites.
    sb.write(".claude/CLAUDE.md", "# new rules\n");
    sb.write(".claude/skills/one/SKILL.md", "one v2");

    sb.mcc()
        .args(["sync", "work", "--force"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Back up profile config"));
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# new rules\n");
    assert_eq!(sb.read(".mcc/profiles/work/skills/one/SKILL.md"), "one v2");

    let backups = backup_dirs(&sb, "work");
    assert_eq!(backups.len(), 1, "{backups:?}");
    let b = &backups[0];
    assert!(
        b.join("CLAUDE.md").exists(),
        "edited CLAUDE.md missing from backup {}",
        b.display()
    );
    assert_eq!(
        std::fs::read_to_string(b.join("CLAUDE.md")).unwrap(),
        "# my edited rules\n"
    );
    assert!(
        b.join("skills/one/SKILL.md").exists(),
        "edited skills/one/SKILL.md missing from backup {}",
        b.display()
    );
    assert_eq!(
        std::fs::read_to_string(b.join("skills/one/SKILL.md")).unwrap(),
        "my edited one"
    );
    // Untouched files are not copied.
    assert!(!b.join("skills/alpha").exists());
}

#[test]
fn sync_prune_backs_up_removed_dirs() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.write(".mcc/profiles/work/skills/mine/SKILL.md", "mine");
    sb.write(".mcc/profiles/work/skills/mine/notes/todo.md", "todo");

    sb.mcc()
        .args(["sync", "work", "--prune", "--yes"])
        .assert()
        .success();
    assert!(!sb.exists(".mcc/profiles/work/skills/mine"));

    let backups = backup_dirs(&sb, "work");
    assert_eq!(backups.len(), 1, "{backups:?}");
    let mine = backups[0].join("skills/mine");
    assert_eq!(
        std::fs::read_to_string(mine.join("SKILL.md")).unwrap(),
        "mine"
    );
    assert_eq!(
        std::fs::read_to_string(mine.join("notes/todo.md")).unwrap(),
        "todo"
    );
}

#[test]
fn sync_backs_up_a_file_standing_where_a_dir_goes() {
    let sb = Sandbox::new();
    sb.seed_default();
    // The old full copy of the plugins (`--copy-plugins`, #47).
    sb.mcc()
        .args(["add", "work", "--clone", "--copy-plugins"])
        .assert()
        .success();
    std::fs::remove_dir_all(sb.path(".mcc/profiles/work/plugins")).unwrap();
    sb.write(".mcc/profiles/work/plugins", "my plugins notes");
    // The default changes a plugins file the profile deleted: a conflict.
    sb.write(
        ".claude/plugins/installed_plugins.json",
        r#"{"version":2,"plugins":{}}"#,
    );

    sb.mcc()
        .args(["sync", "work", "--force"])
        .assert()
        .success();
    assert!(sb.exists(".mcc/profiles/work/plugins/installed_plugins.json"));

    let backups = backup_dirs(&sb, "work");
    assert_eq!(backups.len(), 1, "{backups:?}");
    let plugins = backups[0].join("plugins");
    assert!(std::fs::symlink_metadata(&plugins).unwrap().is_file());
    assert_eq!(
        std::fs::read_to_string(&plugins).unwrap(),
        "my plugins notes"
    );
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

/// Every file under `dir` with its bytes, symlinks not followed.
#[cfg(unix)]
fn snapshot(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                stack.push(p);
            } else {
                let bytes = std::fs::read(&p).unwrap_or_default();
                out.push((p.strip_prefix(dir).unwrap().to_path_buf(), bytes));
            }
        }
    }
    out.sort();
    out
}

#[cfg(unix)]
#[test]
fn sync_dry_run_writes_nothing_to_the_profile() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.write(".mcc/profiles/work/CLAUDE.md", "# my edited rules\n");
    sb.write(".mcc/profiles/work/skills/mine/SKILL.md", "mine");
    let profile = sb.path(".mcc/profiles/work");
    let before = snapshot(&profile);

    sb.mcc()
        .args(["sync", "work", "--prune", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing was written"));
    assert_eq!(snapshot(&profile), before);
    assert!(!sb.exists(".mcc/profiles/work/.mcc-backup"));
}

/// A profile dir that is a symlink into the default (`plugins -> ~/.claude/plugins`)
/// must never be written or pruned through: the default is only ever a source.
#[cfg(unix)]
#[test]
fn sync_never_writes_through_symlinked_profile_dir() {
    let sb = Sandbox::new();
    sb.seed_default();
    // The old full copy of the plugins (`--copy-plugins`, #47).
    sb.mcc()
        .args(["add", "work", "--clone", "--copy-plugins"])
        .assert()
        .success();
    let link = sb.path(".mcc/profiles/work/plugins");
    let relink = || {
        std::fs::remove_dir_all(&link).unwrap();
        std::os::unix::fs::symlink(sb.path(".claude/plugins"), &link).unwrap();
    };
    relink();
    let default_before = snapshot(&sb.path(".claude/plugins"));
    assert!(!default_before.is_empty());
    // A backup keeps the link itself; nothing is copied through it.
    let link_backed_up = |backup: &std::path::Path| {
        let saved = backup.join("plugins");
        assert!(
            std::fs::symlink_metadata(&saved)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{}",
            saved.display()
        );
        assert_eq!(
            std::fs::read_link(&saved).unwrap(),
            sb.path(".claude/plugins")
        );
        let below: Vec<_> = snapshot(backup)
            .into_iter()
            .filter(|(rel, _)| rel.starts_with("plugins") && rel != std::path::Path::new("plugins"))
            .collect();
        assert!(below.is_empty(), "{below:?}");
    };

    // Replacing the link is a removal: needs confirmation.
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--yes"));
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(snapshot(&sb.path(".claude/plugins")), default_before);
    assert!(backup_dirs(&sb, "work").is_empty());

    sb.mcc().args(["sync", "work", "--yes"]).assert().success();
    assert_eq!(snapshot(&sb.path(".claude/plugins")), default_before);
    let backups = backup_dirs(&sb, "work");
    assert_eq!(backups.len(), 1, "{backups:?}");
    link_backed_up(&backups[0]);
    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(meta.is_dir() && !meta.file_type().is_symlink());
    let installed = sb.read(".mcc/profiles/work/plugins/installed_plugins.json");
    assert!(
        installed.contains(".mcc/profiles/work/plugins/cache/m/p1"),
        "{installed}"
    );
    assert!(sb.exists(".mcc/profiles/work/plugins/cache/m/p1/1.0.0/plugin.json"));

    relink();
    sb.mcc()
        .args(["sync", "work", "--prune", "--yes"])
        .assert()
        .success();
    assert_eq!(snapshot(&sb.path(".claude/plugins")), default_before);
    assert!(
        !std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let backups = backup_dirs(&sb, "work");
    assert_eq!(backups.len(), 2, "{backups:?}");
    for b in &backups {
        link_backed_up(b);
    }
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

// ---- three-way sync (#41) ----------------------------------------------------

fn manifest_path() -> String {
    ".mcc/profiles/work/.mcc-sync.json".into()
}

#[test]
fn clone_records_a_sync_manifest() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    let m = json(sb.read(&manifest_path()).as_bytes());
    assert_eq!(m["version"], 1);
    assert!(m["files"]["CLAUDE.md"].is_string(), "{m}");
    assert!(m["files"]["skills/alpha/SKILL.md"].is_string(), "{m}");
    assert!(m["mcp"]["fs"].is_string(), "{m}");
    // Never anything outside the sync set.
    let files = m["files"].as_object().unwrap();
    assert!(
        files.keys().all(|k| !k.starts_with("projects/")
            && k != ".credentials.json"
            && k != ".mcc-sync.json")
    );
}

#[test]
fn sync_is_three_way() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    // Upstream change, local edit, and a conflict.
    sb.write(".claude/agents/reviewer.md", "agent v2");
    sb.write(".mcc/profiles/work/CLAUDE.md", "# my work rules\n");
    sb.write(".claude/skills/alpha/SKILL.md", "alpha v2");
    sb.write(".mcc/profiles/work/skills/alpha/SKILL.md", "my alpha");
    let before = sb.read(&manifest_path());

    // --dry-run shows the classification and writes nothing.
    sb.mcc()
        .args(["sync", "work", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("~ agents/reviewer.md"))
        .stderr(predicate::str::contains("= CLAUDE.md (local edit, kept)"))
        .stderr(predicate::str::contains(
            "! skills/alpha/SKILL.md (conflict",
        ))
        .stderr(predicate::str::contains("1 local edit, 1 conflict"))
        .stderr(predicate::str::contains("nothing was written"));
    assert_eq!(sb.read(&manifest_path()), before);
    assert_eq!(sb.read(".mcc/profiles/work/agents/reviewer.md"), "agent");
    assert!(backup_dirs(&sb, "work").is_empty());

    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("= CLAUDE.md (local edit, kept)"))
        .stderr(predicate::str::contains("--force"))
        .stderr(predicate::str::contains(
            "except 1 local edit (kept) and 1 conflict (skipped)",
        ));
    assert_eq!(sb.read(".mcc/profiles/work/agents/reviewer.md"), "agent v2");
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my work rules\n");
    assert_eq!(
        sb.read(".mcc/profiles/work/skills/alpha/SKILL.md"),
        "my alpha"
    );
    // The backup holds only what was overwritten.
    let backups = backup_dirs(&sb, "work");
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(backups[0].join("agents/reviewer.md")).unwrap(),
        "agent"
    );
    assert!(!backups[0].join("CLAUDE.md").exists());

    // Still classified the same; nothing else to copy.
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already up to date, except"));

    // --force overwrites the conflict (backed up), never the local edit.
    sb.mcc()
        .args(["sync", "work", "--force"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "~ skills/alpha/SKILL.md (conflict, overwritten with --force)",
        ));
    assert_eq!(
        sb.read(".mcc/profiles/work/skills/alpha/SKILL.md"),
        "alpha v2"
    );
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my work rules\n");
    let last = backup_dirs(&sb, "work").pop().unwrap();
    assert_eq!(
        std::fs::read_to_string(last.join("skills/alpha/SKILL.md")).unwrap(),
        "my alpha"
    );
}

#[test]
fn profile_deletions_are_local_edits() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    std::fs::remove_dir_all(sb.path(".mcc/profiles/work/skills/beta")).unwrap();
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "skills/beta/SKILL.md (local edit, kept)",
        ));
    assert!(!sb.exists(".mcc/profiles/work/skills/beta"));
}

#[test]
fn dry_run_never_writes_a_manifest() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    sb.mcc()
        .args(["sync", "work", "--dry-run"])
        .assert()
        .success();
    assert!(!sb.exists(&manifest_path()));
    sb.mcc().args(["sync", "work"]).assert().success();
    assert!(sb.exists(&manifest_path()));
}

#[test]
fn profile_without_manifest_gets_one_even_when_up_to_date() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    std::fs::remove_file(sb.path(&manifest_path())).unwrap();
    // Up to date: --dry-run still writes nothing, a real sync records it.
    sb.mcc()
        .args(["sync", "work", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already up to date"));
    assert!(!sb.exists(&manifest_path()));
    sb.mcc().args(["sync", "work"]).assert().success();
    assert!(sb.exists(&manifest_path()));
}

#[test]
fn invalid_manifest_falls_back_to_two_way() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.write(&manifest_path(), "{oops");
    sb.write(".mcc/profiles/work/CLAUDE.md", "# mine\n");
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("ignoring"))
        .stderr(predicate::str::contains("1 path will be replaced"))
        .stderr(predicate::str::contains("~ CLAUDE.md"));
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my rules\n");
    assert_eq!(json(sb.read(&manifest_path()).as_bytes())["version"], 1);
}

/// #87: without a manifest a sync is two-way and replaces local edits with
/// the default's version, so it lists each one and where it is backed up —
/// even with --quiet, which hides the plan (so the paths below can only come
/// from the warning).
#[test]
fn sync_without_manifest_lists_the_local_edits_it_replaces() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    std::fs::remove_file(sb.path(&manifest_path())).unwrap();
    sb.write(".mcc/profiles/work/CLAUDE.md", "# mine\n");
    #[cfg(unix)]
    let before = snapshot(&sb.path(".mcc/profiles/work"));

    sb.mcc()
        .args(["sync", "work", "--dry-run", "-q"])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 path would be replaced"))
        .stderr(predicate::str::contains("~ CLAUDE.md"))
        .stderr(predicate::str::contains(".mcc-backup"));
    #[cfg(unix)]
    assert_eq!(snapshot(&sb.path(".mcc/profiles/work")), before);
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# mine\n");
    assert!(!sb.exists(&manifest_path()));
    assert!(backup_dirs(&sb, "work").is_empty());

    sb.mcc()
        .args(["sync", "work", "-q"])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 path will be replaced"))
        .stderr(predicate::str::contains("~ CLAUDE.md"))
        .stderr(predicate::str::contains(".mcc-backup"));
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my rules\n");
    let backups = backup_dirs(&sb, "work");
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(
        std::fs::read_to_string(backups[0].join("CLAUDE.md")).unwrap(),
        "# mine\n"
    );
    assert!(sb.exists(&manifest_path()));

    // Recorded now: the next sync is three-way, keeps the edit and takes the
    // default's change without the warning.
    sb.write(".mcc/profiles/work/CLAUDE.md", "# mine again\n");
    sb.write(".claude/agents/reviewer.md", "agent v2");
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("= CLAUDE.md (local edit, kept)"))
        .stderr(predicate::str::contains("will be replaced").not())
        .stderr(predicate::str::contains("would be replaced").not());
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# mine again\n");
    assert_eq!(sb.read(".mcc/profiles/work/agents/reviewer.md"), "agent v2");
}

/// #87: an MCP server edited in the profile is listed as well.
#[test]
fn sync_without_manifest_lists_the_mcp_servers_it_replaces() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    std::fs::remove_file(sb.path(&manifest_path())).unwrap();
    let mut state = json(sb.read(".mcc/profiles/work/.claude.json").as_bytes());
    state["mcpServers"]["fs"]["args"] = serde_json::json!(["mine"]);
    sb.write(".mcc/profiles/work/.claude.json", &state.to_string());

    sb.mcc()
        .args(["sync", "work", "--dry-run", "-q"])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 path would be replaced"))
        .stderr(predicate::str::contains(
            "~ MCP server \"fs\" (.claude.json)",
        ));
}

#[test]
fn list_has_a_sync_column() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--fresh");
    let table = stdout_of(sb.mcc().args(["list", "--no-usage"]));
    assert!(table.lines().next().unwrap().contains("SYNC"), "{table}");
    let row = |name: &str| {
        table
            .lines()
            .find(|l| common::unnumbered(l).starts_with(name))
            .unwrap()
            .to_string()
    };
    assert!(row("work").contains('✓'), "{table}");

    sb.write(".claude/agents/reviewer.md", "agent v2");
    sb.write(".claude/commands/deploy.md", "cmd v2");
    sb.write(".claude/skills/gamma/SKILL.md", "new");
    sb.write(".mcc/profiles/work/CLAUDE.md", "# mine\n");
    sb.write(".mcc/profiles/work/hooks/notify.sh", "mine");
    let table = stdout_of(sb.mcc().args(["list", "--no-usage"]));
    assert!(table.contains("3 behind, 2 local edits"), "{table}");

    let arr = json(
        &sb.mcc()
            .args(["list", "--json", "--no-usage"])
            .output()
            .unwrap()
            .stdout,
    );
    let arr = arr.as_array().unwrap();
    let w = by_name(arr, "work");
    assert_eq!(w["sync"]["behind"], 3);
    assert_eq!(w["sync"]["local_edits"], 2);
    assert_eq!(w["sync"]["conflicts"], 0);
    assert!(by_name(arr, "default")["sync"].is_null());
    // A fresh profile has no manifest: every difference is behind.
    assert!(by_name(arr, "lab")["sync"]["behind"].as_u64().unwrap() > 0);
}

#[test]
fn diff_shows_the_differences() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.mcc()
        .args(["diff", "work"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("matches the default"));

    sb.write(".claude/CLAUDE.md", "# my rules\n# more\n");
    sb.write(".mcc/profiles/work/agents/reviewer.md", "my agent");
    sb.write(".mcc/profiles/work/agents/mine.md", "only here");
    let mut state = json(sb.read(".mcc/profiles/work/.claude.json").as_bytes());
    state["mcpServers"]["solo"] = serde_json::json!({"command": "solo-server"});
    sb.write(".mcc/profiles/work/.claude.json", &state.to_string());
    let before = sb.read(&manifest_path());
    sb.mcc()
        .args(["diff", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("== changed in default: CLAUDE.md"))
        .stdout(predicate::str::contains("--- default/CLAUDE.md"))
        .stdout(predicate::str::contains("+++ work/CLAUDE.md"))
        .stdout(predicate::str::contains("-# more"))
        .stdout(predicate::str::contains(
            "== local edit: agents/reviewer.md",
        ))
        .stdout(predicate::str::contains("+my agent"))
        .stdout(predicate::str::contains(
            "== only in profile: agents/mine.md",
        ))
        .stdout(predicate::str::contains(
            "== only in profile: MCP server \"solo\"",
        ))
        .stdout(predicate::str::contains(
            "+++ work/.claude.json mcpServers.solo",
        ))
        .stdout(predicate::str::contains("solo-server"))
        .stderr(predicate::str::contains("1 behind, 1 local edit"));
    // Read-only.
    assert_eq!(sb.read(&manifest_path()), before);
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my rules\n");

    sb.mcc()
        .args(["diff", "work", "--only", "skills"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    sb.mcc().args(["diff", "ghost"]).assert().code(2);
    sb.mcc()
        .args(["diff", "default"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("pick another profile"));
}

/// A manifest from `sync --only` covers only those items: the others are
/// still compared two-way, not reported as conflicts.
#[test]
fn partial_manifest_covers_only_its_items() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    sb.mcc()
        .args(["sync", "work", "--only", "skills"])
        .assert()
        .success();
    sb.write(".mcc/profiles/work/CLAUDE.md", "# old copy\n");
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("conflict").not());
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my rules\n");
    // Now recorded: a later profile edit is a local edit.
    sb.write(".mcc/profiles/work/CLAUDE.md", "# mine\n");
    sb.mcc()
        .args(["sync", "work", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("= CLAUDE.md (local edit, kept)"));
}

// ---- settings overlays --------------------------------------------------------

/// Give "work" an overlay in both places: `[profiles.work.settings]` in
/// config.toml and `settings.mcc.json` in the profile (which wins).
fn add_settings_overlay(sb: &Sandbox) {
    let mut cfg = sb.read(".mcc/config.toml");
    cfg.push_str(
        "\n[profiles.work.settings]\nmodel = \"sonnet\"\ncleanupPeriodDays = 90\n\n[profiles.work.settings.env]\nWORK = \"1\"\n",
    );
    sb.write(".mcc/config.toml", &cfg);
    sb.write(
        ".mcc/profiles/work/settings.mcc.json",
        r#"{"cleanupPeriodDays": 7, "statusLine": null, "enabledPlugins": {"p2@m": true}, "permissions": {"allow": ["Bash(ls)"]}}"#,
    );
}

fn work_settings(sb: &Sandbox) -> serde_json::Value {
    json(sb.read(".mcc/profiles/work/settings.json").as_bytes())
}

#[test]
fn sync_merges_settings_overlays() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    add_settings_overlay(&sb);

    sb.mcc()
        .args(["sync", "work", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("~ settings.json"));
    sb.mcc().args(["sync", "work"]).assert().success();
    let s = work_settings(&sb);
    let profile = sb.path(".mcc/profiles/work").display().to_string();
    // Overlay keys: config.toml, then settings.mcc.json on top.
    assert_eq!(s["model"], "sonnet", "{s}");
    assert_eq!(s["env"]["WORK"], "1", "{s}");
    assert_eq!(s["cleanupPeriodDays"], 7, "{s}");
    assert_eq!(s["permissions"]["allow"], serde_json::json!(["Bash(ls)"]));
    // Objects merge key by key; null removes the key.
    assert_eq!(s["enabledPlugins"]["p1@m"], true, "{s}");
    assert_eq!(s["enabledPlugins"]["p2@m"], true, "{s}");
    assert!(s.get("statusLine").is_none(), "{s}");
    // The default's keys are still there, paths rewritten.
    assert!(
        s["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(&format!("{profile}/hooks/notify.sh")),
        "{s}"
    );
    // The previous settings.json was backed up.
    let backups = backup_dirs(&sb, "work");
    let old = std::fs::read_to_string(backups[0].join("settings.json")).unwrap();
    assert!(old.contains("\"opus\""), "{old}");
    // config.toml keeps the overlay table when mcc saves it.
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("[profiles.work.settings]"), "{cfg}");
    assert!(cfg.contains("model = \"sonnet\""), "{cfg}");

    // The merged result is what sync records: no local edit, nothing to do.
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already up to date"))
        .stderr(predicate::str::contains("local edit").not());
    sb.mcc()
        .args(["diff", "work"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("matches the default"));

    // A change in the default is taken, and the overlay re-applied on top.
    let mut d = json(sb.read(".claude/settings.json").as_bytes());
    d["model"] = "haiku".into();
    d["alwaysThinkingEnabled"] = true.into();
    sb.write(".claude/settings.json", &d.to_string());
    sb.mcc()
        .args(["diff", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "--- default/settings.json + work overlay",
        ))
        .stdout(predicate::str::contains("alwaysThinkingEnabled"))
        .stdout(predicate::str::contains("haiku").not());
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("conflict").not());
    let s = work_settings(&sb);
    assert_eq!(s["model"], "sonnet", "{s}");
    assert_eq!(s["alwaysThinkingEnabled"], true, "{s}");

    // Editing the overlay is a change sync applies (not a conflict).
    sb.write(
        ".mcc/profiles/work/settings.mcc.json",
        r#"{"model": "opus"}"#,
    );
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("conflict").not());
    let s = work_settings(&sb);
    assert_eq!(s["model"], "opus", "{s}");
    assert_eq!(s["cleanupPeriodDays"], 90, "{s}");
    assert_eq!(s["statusLine"]["command"], "~/.claude/statusline.sh");

    // Removing the overlay entirely goes back to the default's settings.
    std::fs::remove_file(sb.path(".mcc/profiles/work/settings.mcc.json")).unwrap();
    let cfg = sb.read(".mcc/config.toml");
    let cut = cfg.find("\n[profiles.work.settings]").unwrap();
    sb.write(".mcc/config.toml", &cfg[..cut]);
    sb.mcc().args(["sync", "work"]).assert().success();
    assert_eq!(work_settings(&sb)["model"], "haiku");
}

#[test]
fn sync_warns_when_overlay_replaces_statusline_shim() {
    let sb = Sandbox::new();
    sb.seed_default();
    let mut d = json(sb.read(".claude/settings.json").as_bytes());
    d["statusLine"] = serde_json::json!({"type": "command", "command": "mcc statusline"});
    sb.write(".claude/settings.json", &d.to_string());
    with_profile(&sb, "work", "--clone");
    const WARNING: &str = "replaces the mcc statusline shim";

    // No overlay: no warning.
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(WARNING).not());

    // An overlay that only sets another statusLine field keeps the shim.
    sb.write(
        ".mcc/profiles/work/settings.mcc.json",
        r#"{"statusLine": {"padding": 1}}"#,
    );
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(WARNING).not());
    let s = work_settings(&sb);
    assert_eq!(s["statusLine"]["command"], "mcc statusline", "{s}");
    assert_eq!(s["statusLine"]["padding"], 1, "{s}");

    // An overlay statusLine.command replaces the shim: warned, still written.
    sb.write(
        ".mcc/profiles/work/settings.mcc.json",
        r#"{"statusLine": {"command": "~/bin/work-status.sh"}}"#,
    );
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "\"work\": the settings overlay sets statusLine.command, which {WARNING}"
        )));
    let s = work_settings(&sb);
    assert_eq!(s["statusLine"]["command"], "~/bin/work-status.sh", "{s}");

    // Without the shim in the default, the same overlay is not warned about.
    let mut d = json(sb.read(".claude/settings.json").as_bytes());
    d["statusLine"] = serde_json::json!({"command": "~/.claude/statusline.sh"});
    sb.write(".claude/settings.json", &d.to_string());
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(WARNING).not());
}

#[test]
fn settings_overlay_keeps_local_edits_and_conflicts_three_way() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    add_settings_overlay(&sb);
    sb.mcc().args(["sync", "work"]).assert().success();

    // A basic key added in the profile is taken into the default (#129):
    // no longer a local edit kept in the profile.
    let mut s = work_settings(&sb);
    s["mine"] = true.into();
    sb.write(".mcc/profiles/work/settings.json", &s.to_string());
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "mine: basic · from \"work\" · synced",
        ))
        .stderr(predicate::str::contains("local edit").not());
    assert_eq!(
        json(sb.read(".claude/settings.json").as_bytes())["mine"],
        true
    );
    // Overlay keys are the profile's own: never taken into the default.
    assert_eq!(
        json(sb.read(".claude/settings.json").as_bytes())["model"],
        "opus"
    );

    // The same key changed both in the default and in the profile: a
    // conflict, kept.
    let mut d = json(sb.read(".claude/settings.json").as_bytes());
    d["mine"] = "theirs".into();
    sb.write(".claude/settings.json", &d.to_string());
    let mut s = work_settings(&sb);
    s["mine"] = "ours".into();
    sb.write(".mcc/profiles/work/settings.json", &s.to_string());
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "mine: basic · from \"work\" · conflict",
        ))
        .stderr(predicate::str::contains("1 conflict skipped (mine)"));
    assert_eq!(work_settings(&sb)["mine"], "ours");
    assert_eq!(
        json(sb.read(".claude/settings.json").as_bytes())["mine"],
        "theirs"
    );
    // --force takes the default's value, the overlay still on top.
    sb.mcc()
        .args(["sync", "work", "--force"])
        .assert()
        .success();
    let s = work_settings(&sb);
    assert_eq!(s["mine"], "theirs", "{s}");
    assert_eq!(s["model"], "sonnet", "{s}");
}

#[test]
fn settings_overlay_errors_and_excluded_settings() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    let before = sb.read(".mcc/profiles/work/settings.json");

    sb.write(".mcc/profiles/work/settings.mcc.json", "{not json");
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "settings.mcc.json is not valid JSON",
        ));
    sb.write(".mcc/profiles/work/settings.mcc.json", "[1]");
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must hold a JSON object"));
    assert_eq!(sb.read(".mcc/profiles/work/settings.json"), before);

    // Without a default settings.json there is nothing to merge onto.
    sb.write(".mcc/profiles/work/settings.mcc.json", r#"{"model": "x"}"#);
    let default_settings = sb.read(".claude/settings.json");
    std::fs::remove_file(sb.path(".claude/settings.json")).unwrap();
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "no settings.json to merge it onto",
        ));
    assert_eq!(sb.read(".mcc/profiles/work/settings.json"), before);
    sb.write(".claude/settings.json", &default_settings);

    // With settings excluded the profile owns its settings.json: the
    // overlay is not applied, and sync says so.
    sb.write(".mcc/profiles/work/settings.mcc.json", r#"{"model": "x"}"#);
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &cfg.replace("exclude = []", "exclude = [\"settings\"]"),
    );
    assert!(
        sb.read(".mcc/config.toml")
            .contains("exclude = [\"settings\"]")
    );
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("overlay is not applied"));
    assert_eq!(sb.read(".mcc/profiles/work/settings.json"), before);
}

// ---- settings synced key by key (#129) ---------------------------------------

const CODE_MODE: &str = "CLAUDE_CODE_EXPERIMENTAL_CODE_MODE";

/// The settings.json at `rel` (a profile dir, or `.claude`).
fn settings_of(sb: &Sandbox, dir: &str) -> serde_json::Value {
    json(sb.read(&format!("{dir}/settings.json")).as_bytes())
}

/// Set `env.NAME` in the settings.json of `dir`.
fn set_env(sb: &Sandbox, dir: &str, name: &str, value: &str) {
    let mut s = settings_of(sb, dir);
    if s.get("env").is_none() {
        s["env"] = serde_json::json!({});
    }
    s["env"][name] = value.into();
    sb.write(&format!("{dir}/settings.json"), &s.to_string());
}

/// default + `work`, `lab` and `csdaisy`, all cloned from it.
fn three_profiles() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    for p in ["work", "lab", "csdaisy"] {
        with_profile(&sb, p, "--clone");
    }
    sb
}

/// `mcc sync ARGS…`'s stderr; it must succeed.
fn sync_stderr(sb: &Sandbox, args: &[&str]) -> String {
    let o = sb.mcc().arg("sync").args(args).output().unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(o.status.success(), "{stderr}");
    stderr
}

#[test]
fn a_basic_setting_changed_in_any_profile_reaches_every_profile() {
    let sb = three_profiles();
    set_env(&sb, ".mcc/profiles/csdaisy", CODE_MODE, "1");

    // Syncing another profile still reads csdaisy's settings: the key goes
    // into the default, and into the profile synced.
    let err = sync_stderr(&sb, &["work"]);
    assert!(
        err.contains(&format!(
            "env.{CODE_MODE}: basic · from \"csdaisy\" · synced"
        )),
        "{err}"
    );
    assert_eq!(settings_of(&sb, ".claude")["env"][CODE_MODE], "1");
    assert_eq!(
        settings_of(&sb, ".mcc/profiles/work")["env"][CODE_MODE],
        "1"
    );
    // The default's settings.json was backed up first.
    let backups: Vec<_> = std::fs::read_dir(sb.path(".claude/.mcc-backup"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
    // lab gets it on its own sync; then nothing is left to report.
    assert!(
        settings_of(&sb, ".mcc/profiles/lab")["env"]
            .get(CODE_MODE)
            .is_none()
    );
    let err = sync_stderr(&sb, &["--all"]);
    assert!(!err.contains("Settings changed in profiles"), "{err}");
    for p in ["work", "lab", "csdaisy"] {
        let dir = format!(".mcc/profiles/{p}");
        assert_eq!(settings_of(&sb, &dir)["env"][CODE_MODE], "1", "{p}");
        // The rest of the default's settings, paths rewritten, still there.
        let hook = settings_of(&sb, &dir)["hooks"]["Stop"][0]["hooks"][0]["command"].clone();
        assert!(
            hook.as_str()
                .unwrap()
                .contains(&format!("{dir}/hooks/notify.sh")),
            "{hook}"
        );
    }
    let list = sb
        .mcc()
        .args(["list", "--json", "--no-usage"])
        .output()
        .unwrap();
    let all: Vec<serde_json::Value> = serde_json::from_slice(&list.stdout).unwrap();
    for p in ["work", "lab", "csdaisy"] {
        assert_eq!(by_name(&all, p)["sync"]["behind"], 0, "{p}");
        assert_eq!(by_name(&all, p)["sync"]["local_edits"], 0, "{p}");
    }
}

#[test]
fn a_key_changed_differently_in_two_profiles_is_a_conflict() {
    let sb = three_profiles();
    for (p, model) in [("work", "sonnet"), ("lab", "haiku")] {
        let dir = format!(".mcc/profiles/{p}");
        let mut s = settings_of(&sb, &dir);
        s["model"] = model.into();
        sb.write(&format!("{dir}/settings.json"), &s.to_string());
    }
    let err = sync_stderr(&sb, &["--all"]);
    assert!(
        err.contains("model: basic · from \"lab\", \"work\" · conflict: changed differently in several profiles; skipped"),
        "{err}"
    );
    assert!(!err.contains("hooks"), "{err}");
    assert_eq!(settings_of(&sb, ".claude")["model"], "opus");
    assert_eq!(settings_of(&sb, ".mcc/profiles/work")["model"], "sonnet");
    assert_eq!(settings_of(&sb, ".mcc/profiles/lab")["model"], "haiku");
    assert_eq!(settings_of(&sb, ".mcc/profiles/csdaisy")["model"], "opus");
}

/// #129 review: a key removed in a profile leaves the default only once
/// the user confirms it at a terminal. `--yes` never confirms it (the
/// profile may have lost it), nor does a run without a terminal.
#[test]
fn a_setting_removed_in_a_profile_leaves_the_default_only_once_confirmed() {
    let sb = three_profiles();
    let mut s = settings_of(&sb, ".mcc/profiles/lab");
    s.as_object_mut().unwrap().remove("statusLine");
    sb.write(".mcc/profiles/lab/settings.json", &s.to_string());
    let default_before = sb.read(".claude/settings.json");
    let err = sync_stderr(&sb, &["--all", "--dry-run"]);
    assert!(
        err.contains("statusLine: basic · from \"lab\" · would be asked about"),
        "{err}"
    );
    for args in [
        &["work"][..],
        &["--all", "--yes"],
        &["--all", "--prune", "--yes"],
    ] {
        let err = sync_stderr(&sb, args);
        assert!(
            err.contains("statusLine: basic · from \"lab\" · skipped (needs confirmation)"),
            "{args:?}: {err}"
        );
        assert!(err.contains("without --yes"), "{args:?}: {err}");
        assert_eq!(sb.read(".claude/settings.json"), default_before, "{args:?}");
    }
    assert!(!sb.exists(".claude/.mcc-backup"));
    // The other profiles keep it, also after a launch catches them up; lab
    // keeps its removal as a local edit.
    let o = sb.launcher("work").args(["-p", "hi"]).output().unwrap();
    assert!(o.status.success(), "{o:?}");
    assert_eq!(sb.read(".claude/settings.json"), default_before);
    for p in ["work", "csdaisy"] {
        let dir = format!(".mcc/profiles/{p}");
        assert!(settings_of(&sb, &dir).get("statusLine").is_some(), "{p}");
    }
    assert!(
        settings_of(&sb, ".mcc/profiles/lab")
            .get("statusLine")
            .is_none()
    );
}

/// #129 review: a profile whose settings.json is missing or emptied is not
/// one that removed every key: `--yes` (with or without `--prune`) leaves
/// the default's settings.json byte for byte as it is.
#[test]
fn a_missing_or_emptied_profile_settings_never_empties_the_default() {
    let sb = three_profiles();
    let default_before = sb.read(".claude/settings.json");
    std::fs::remove_file(sb.path(".mcc/profiles/lab/settings.json")).unwrap();
    sb.write(".mcc/profiles/csdaisy/settings.json", "{}");
    for args in [&["--all", "--yes"][..], &["--all", "--prune", "--yes"]] {
        let err = sync_stderr(&sb, args);
        assert!(!err.contains("· removed"), "{args:?}: {err}");
        assert!(!err.contains("from \"lab\""), "{args:?}: {err}");
        assert_eq!(sb.read(".claude/settings.json"), default_before, "{args:?}");
        let work = settings_of(&sb, ".mcc/profiles/work");
        for key in ["hooks", "enabledPlugins", "statusLine", "model"] {
            assert!(work.get(key).is_some(), "{args:?}: {key}: {work}");
        }
    }
}

#[test]
fn account_bound_settings_are_never_copied_to_another_profile() {
    let sb = Sandbox::new();
    sb.seed_default();
    // The default's own provider never reaches a clone.
    set_env(&sb, ".claude", "ANTHROPIC_API_KEY", "default-account-key");
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "csdaisy", "--clone");
    assert!(
        settings_of(&sb, ".mcc/profiles/work")["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none()
    );
    set_env(
        &sb,
        ".mcc/profiles/csdaisy",
        "ANTHROPIC_API_KEY",
        "csdaisy-account-key",
    );
    set_env(
        &sb,
        ".mcc/profiles/csdaisy",
        "ANTHROPIC_BASE_URL",
        "https://gw.example.invalid",
    );
    // A basic key next to them still travels.
    set_env(&sb, ".mcc/profiles/csdaisy", CODE_MODE, "1");

    let err = sync_stderr(&sb, &["--all"]);
    assert!(
        err.contains("env.ANTHROPIC_BASE_URL: account-bound · from \"csdaisy\" · skipped (account-bound: never copied to another profile)"),
        "{err}"
    );
    let default = settings_of(&sb, ".claude");
    assert_eq!(default["env"]["ANTHROPIC_API_KEY"], "default-account-key");
    assert!(
        default["env"].get("ANTHROPIC_BASE_URL").is_none(),
        "{default}"
    );
    let work = settings_of(&sb, ".mcc/profiles/work");
    assert!(work["env"].get("ANTHROPIC_API_KEY").is_none(), "{work}");
    assert!(work["env"].get("ANTHROPIC_BASE_URL").is_none(), "{work}");
    assert_eq!(work["env"][CODE_MODE], "1");
    // csdaisy keeps its own, and they are not a local edit.
    let cs = settings_of(&sb, ".mcc/profiles/csdaisy");
    assert_eq!(cs["env"]["ANTHROPIC_API_KEY"], "csdaisy-account-key");
    assert_eq!(
        cs["env"]["ANTHROPIC_BASE_URL"],
        "https://gw.example.invalid"
    );
    let err = sync_stderr(&sb, &["--all"]);
    assert!(!err.contains("account-bound"), "{err}");
    assert!(!err.contains("local edit"), "{err}");
}

/// What mcc records for a declined value: the hash `manifest::hash_json`
/// gives a JSON string.
fn value_hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"json\0");
    h.update(serde_json::Value::from(value).to_string().as_bytes());
    format!("{:x}", h.finalize())
}

#[test]
fn a_shared_secret_is_only_synced_once_agreed_to() {
    let sb = three_profiles();
    set_env(
        &sb,
        ".mcc/profiles/csdaisy",
        "TAVILY_API_KEY",
        "tvly-shared-one",
    );
    // No terminal to ask at: skipped, and the sync goes on; --yes never
    // agrees for the user.
    for args in [&["--all"][..], &["--all", "--yes"]] {
        let err = sync_stderr(&sb, args);
        assert!(
            err.contains("env.TAVILY_API_KEY: shared secret · from \"csdaisy\" · skipped (a shared secret is only synced once you agree at a terminal)"),
            "{err}"
        );
        assert!(err.contains("3/3 profiles synced"), "{err}");
    }
    assert!(settings_of(&sb, ".claude").get("env").is_none());
    assert!(settings_of(&sb, ".mcc/profiles/work").get("env").is_none());
    let err = sync_stderr(&sb, &["--all", "--dry-run"]);
    assert!(err.contains("· would be asked about"), "{err}");

    // A value declined before is not asked about again…
    let path = ".mcc/profiles/csdaisy/.mcc-sync.json";
    let mut m = json(sb.read(path).as_bytes());
    m["declined_secrets"] =
        serde_json::json!({"env.TAVILY_API_KEY": value_hash("tvly-shared-one")});
    sb.write(path, &m.to_string());
    let err = sync_stderr(&sb, &["--all"]);
    assert!(err.contains("· declined earlier"), "{err}");
    // …until it changes.
    set_env(
        &sb,
        ".mcc/profiles/csdaisy",
        "TAVILY_API_KEY",
        "tvly-shared-two",
    );
    let err = sync_stderr(&sb, &["--all"]);
    assert!(err.contains("· skipped (a shared secret"), "{err}");
}

/// As an older mcc recorded `p`'s sync: the whole file's hash only.
fn legacy_record(sb: &Sandbox, p: &str) {
    let path = format!(".mcc/profiles/{p}/.mcc-sync.json");
    let mut m = json(sb.read(&path).as_bytes());
    let m = m.as_object_mut().unwrap();
    m.remove("settings");
    m["items"]
        .as_array_mut()
        .unwrap()
        .retain(|i| i != ":settings");
    sb.write(&path, &serde_json::Value::Object(m.clone()).to_string());
}

/// The first sync after upgrading (#129): records from before per-key
/// settings, a shared secret set differently in each profile, and a basic
/// key in one, while the default changed too. Which side changed a key
/// can't be told, so nothing is taken into the default (a conflict, also
/// on the next sync); the secret is never spread, and no prompt is needed.
#[test]
fn first_sync_after_upgrade_never_spreads_secrets_nor_guesses() {
    let sb = Sandbox::new();
    sb.seed_default();
    set_env(&sb, ".claude", "TAVILY_API_KEY", "tvly-default");
    for p in ["work", "lab", "csdaisy"] {
        with_profile(&sb, p, "--clone");
        legacy_record(&sb, p);
    }
    set_env(&sb, ".mcc/profiles/work", "TAVILY_API_KEY", "tvly-work");
    set_env(&sb, ".mcc/profiles/lab", "TAVILY_API_KEY", "tvly-lab");
    set_env(&sb, ".mcc/profiles/csdaisy", CODE_MODE, "1");
    // The default changed since too.
    let mut d = settings_of(&sb, ".claude");
    d["effortLevel"] = "high".into();
    sb.write(".claude/settings.json", &d.to_string());
    let default_before = sb.read(".claude/settings.json");

    let err = sync_stderr(&sb, &["--all"]);
    assert!(
        err.contains(&format!(
            "env.{CODE_MODE}: basic · from \"csdaisy\" · conflict: changed differently in the default and the profile"
        )),
        "{err}"
    );
    assert!(
        err.contains("env.TAVILY_API_KEY: shared secret · from \"lab\", \"work\" · conflict"),
        "{err}"
    );
    assert!(
        !err.contains("would be asked") && !err.contains("agree at a terminal"),
        "{err}"
    );
    assert_eq!(sb.read(".claude/settings.json"), default_before);
    for (p, tavily, code) in [
        ("work", "tvly-work", None),
        ("lab", "tvly-lab", None),
        ("csdaisy", "tvly-default", Some("1")),
    ] {
        let s = settings_of(&sb, &format!(".mcc/profiles/{p}"));
        assert_eq!(s["env"]["TAVILY_API_KEY"], tavily, "{p}");
        assert_eq!(
            s["env"].get(CODE_MODE).and_then(|v| v.as_str()),
            code,
            "{p}"
        );
        assert_eq!(s["effortLevel"], "high", "{p}");
    }
    // The records are per key now, and the conflict stays one.
    let err = sync_stderr(&sb, &["--all"]);
    assert!(
        err.contains(&format!(
            "env.{CODE_MODE}: basic · from \"csdaisy\" · conflict"
        )),
        "{err}"
    );
    assert_eq!(sb.read(".claude/settings.json"), default_before);
    let m = json(sb.read(".mcc/profiles/csdaisy/.mcc-sync.json").as_bytes());
    assert_eq!(m["settings"][format!("env.{CODE_MODE}")], "unknown", "{m}");
    assert!(m["items"].as_array().unwrap().contains(&":settings".into()));
}

/// #129 review: after upgrading, a profile edited since its last sync
/// still holds the default's older statusLine, and the default changed
/// it since (`mcc statusline install`) and dropped a key. Neither the
/// stale value nor the dropped key goes back into the default.
#[test]
fn first_sync_after_upgrade_never_reverts_the_default() {
    let sb = three_profiles();
    for p in ["work", "lab", "csdaisy"] {
        legacy_record(&sb, p);
    }
    // Some other edit, so the whole-file record no longer matches.
    set_env(&sb, ".mcc/profiles/work", "MINE", "1");
    let mut d = settings_of(&sb, ".claude");
    d["statusLine"] = serde_json::json!({"type": "command", "command": "mcc statusline"});
    d.as_object_mut().unwrap().remove("enabledPlugins");
    sb.write(".claude/settings.json", &d.to_string());
    let default_before = sb.read(".claude/settings.json");
    for _ in 0..2 {
        let err = sync_stderr(&sb, &["--all", "--yes"]);
        for key in ["statusLine", "enabledPlugins"] {
            assert!(
                err.contains(&format!("{key}: basic · from \"work\" · conflict")),
                "{key}: {err}"
            );
        }
        assert_eq!(sb.read(".claude/settings.json"), default_before);
    }
    // The profiles not edited since take the default's new statusLine.
    for p in ["lab", "csdaisy"] {
        let s = settings_of(&sb, &format!(".mcc/profiles/{p}"));
        assert_eq!(s["statusLine"]["command"], "mcc statusline", "{p}: {s}");
        assert!(s.get("enabledPlugins").is_none(), "{p}: {s}");
    }
}

/// No secret value is ever shown or logged: not by a sync, a dry run,
/// nor `mcc diff`.
#[test]
fn secret_setting_values_never_reach_the_output_or_the_log() {
    let sb = three_profiles();
    // Built at run time, so the source holds no token-shaped literal.
    let tavily = ["tvly", "LEAKCHECK", "1234567890abcdef"].join("-");
    let anthropic = ["sk", "ant", "LEAKCHECK", "0987654321fedcba"].join("-");
    set_env(&sb, ".mcc/profiles/csdaisy", "TAVILY_API_KEY", &tavily);
    set_env(
        &sb,
        ".mcc/profiles/csdaisy",
        "ANTHROPIC_API_KEY",
        &anthropic,
    );
    // The default has another TAVILY value, so the profile differs from it.
    set_env(&sb, ".claude", "TAVILY_API_KEY", "tvly-default-value");
    let runs: [&[&str]; 4] = [
        &["sync", "--all", "--dry-run"],
        &["sync", "--all"],
        &["diff", "csdaisy"],
        &["sync", "csdaisy", "--force"],
    ];
    for args in runs {
        let o = sb.mcc().args(args).output().unwrap();
        let (out, err) = (
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr),
        );
        for secret in [&tavily, &anthropic] {
            assert!(!out.contains(secret.as_str()), "{args:?} stdout: {out}");
            assert!(!err.contains(secret.as_str()), "{args:?} stderr: {err}");
        }
        if args[0] == "diff" {
            // The key is shown, its value hidden.
            assert!(out.contains("TAVILY_API_KEY"), "{out}");
            assert!(out.contains("<hidden, differs>"), "{out}");
        }
    }
    let log = sb.read(".mcc/logs/mcc.log");
    for secret in [&tavily, &anthropic] {
        assert!(!log.contains(secret.as_str()), "{log}");
    }
    // The account key stays in csdaisy only.
    assert_eq!(
        settings_of(&sb, ".mcc/profiles/csdaisy")["env"]["ANTHROPIC_API_KEY"],
        anthropic.as_str()
    );
    assert!(!sb.read(".claude/settings.json").contains(&anthropic));
    assert!(
        !sb.read(".mcc/profiles/work/settings.json")
            .contains(&anthropic)
    );
}

/// A profile whose settings.json isn't a JSON object is synced as a whole
/// file, as before; the default's account-bound keys still never reach it,
/// and `mcc diff` shows neither side's text.
#[test]
fn a_profile_with_unreadable_settings_never_gets_account_bound_keys() {
    let sb = Sandbox::new();
    sb.seed_default();
    let key = ["sk", "ant", "LEAKCHECK", "fallback0123456789"].join("-");
    let tavily = ["tvly", "LEAKCHECK", "fallback0123456789"].join("-");
    set_env(&sb, ".claude", "ANTHROPIC_API_KEY", &key);
    set_env(&sb, ".claude", "TAVILY_API_KEY", &tavily);
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "fresh", "--fresh");
    for p in ["work", "fresh"] {
        sb.write(&format!(".mcc/profiles/{p}/settings.json"), "{not json");
    }
    // Changed in the default too: a conflict, which --force overwrites.
    let mut d = settings_of(&sb, ".claude");
    d["model"] = "haiku".into();
    sb.write(".claude/settings.json", &d.to_string());
    let runs: [&[&str]; 4] = [
        &["diff", "work"],
        &["sync", "work", "--force"],
        &["diff", "fresh"],
        &["sync", "fresh"],
    ];
    for args in runs {
        let o = sb.mcc().args(args).output().unwrap();
        assert!(o.status.success(), "{args:?}: {o:?}");
        let out = String::from_utf8_lossy(&o.stdout);
        let err = String::from_utf8_lossy(&o.stderr);
        for secret in [&key, &tavily] {
            assert!(!out.contains(secret.as_str()), "{args:?}: {out}");
            assert!(!err.contains(secret.as_str()), "{args:?}: {err}");
        }
    }
    for p in ["work", "fresh"] {
        let s = settings_of(&sb, &format!(".mcc/profiles/{p}"));
        assert_eq!(s["model"], "haiku", "{p}: {s}");
        assert!(s["env"].get("ANTHROPIC_API_KEY").is_none(), "{p}: {s}");
    }
    let log = sb.read(".mcc/logs/mcc.log");
    assert!(!log.contains(&key) && !log.contains(&tavily));
}

/// The settings.local.json at `dir` (a profile dir, or `.claude`).
fn local_settings_of(sb: &Sandbox, dir: &str) -> serde_json::Value {
    json(sb.read(&format!("{dir}/settings.local.json")).as_bytes())
}

/// #129 review: settings.local.json is synced as a whole file, but the
/// default's account-bound keys never reach a profile (on `add --clone`,
/// `sync` or a launch), a profile keeps its own, and `mcc diff` hides
/// secret values in it.
#[test]
fn account_bound_keys_of_settings_local_json_never_reach_a_profile() {
    let sb = Sandbox::new();
    sb.seed_default();
    // Built at run time, so the source holds no token-shaped literal.
    let key = ["sk", "ant", "LEAKCHECK", "local0123456789"].join("-");
    let token = ["tok", "LEAKCHECK", "local0123456789"].join("-");
    sb.write(
        ".claude/settings.local.json",
        &serde_json::json!({
            "env": {"ANTHROPIC_API_KEY": key, "ANTHROPIC_BASE_URL": "https://default.example.invalid", "LOCAL_FLAG": "1"},
            "apiKeyHelper": "/bin/default-helper",
            "permissions": {"allow": ["Bash(ls)"]}
        })
        .to_string(),
    );
    let no_default_keys = |s: &serde_json::Value, p: &str| {
        assert!(s["env"].get("ANTHROPIC_API_KEY").is_none(), "{p}: {s}");
        assert_ne!(
            s["env"]["ANTHROPIC_BASE_URL"], "https://default.example.invalid",
            "{p}: {s}"
        );
        assert_ne!(s["apiKeyHelper"], "/bin/default-helper", "{p}: {s}");
    };
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--clone");
    for p in ["work", "lab"] {
        let s = local_settings_of(&sb, &format!(".mcc/profiles/{p}"));
        no_default_keys(&s, p);
        assert_eq!(s["env"]["LOCAL_FLAG"], "1", "{p}: {s}");
        assert_eq!(s["permissions"]["allow"][0], "Bash(ls)", "{p}: {s}");
    }

    // lab has its own provider; the default changes a basic key.
    let mut lab = local_settings_of(&sb, ".mcc/profiles/lab");
    lab["env"]["ANTHROPIC_BASE_URL"] = "https://lab.example.invalid".into();
    lab["apiKeyHelper"] = "/bin/lab-helper".into();
    sb.write(".mcc/profiles/lab/settings.local.json", &lab.to_string());
    let mut d = local_settings_of(&sb, ".claude");
    d["permissions"]["allow"] = serde_json::json!(["Bash(ls)", "Bash(pwd)"]);
    sb.write(".claude/settings.local.json", &d.to_string());
    let err = sync_stderr(&sb, &["--all"]);
    assert!(!err.contains(&key), "{err}");
    for p in ["work", "lab"] {
        let s = local_settings_of(&sb, &format!(".mcc/profiles/{p}"));
        no_default_keys(&s, p);
        assert_eq!(s["permissions"]["allow"][1], "Bash(pwd)", "{p}: {s}");
    }
    let lab = local_settings_of(&sb, ".mcc/profiles/lab");
    assert_eq!(
        lab["env"]["ANTHROPIC_BASE_URL"],
        "https://lab.example.invalid"
    );
    assert_eq!(lab["apiKeyHelper"], "/bin/lab-helper");
    // Nothing left to sync: the profile's own keys are not a difference.
    let err = sync_stderr(&sb, &["--all"]);
    assert!(!err.contains("settings.local.json"), "{err}");

    // mcc diff shows a new secret of the default's by name, never by value.
    let mut d = local_settings_of(&sb, ".claude");
    d["env"]["SEARCH_TOKEN"] = token.as_str().into();
    sb.write(".claude/settings.local.json", &d.to_string());
    let o = sb.mcc().args(["diff", "lab"]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("SEARCH_TOKEN") && out.contains("<hidden>"),
        "{out}"
    );
    for secret in [&key, &token] {
        assert!(!out.contains(secret.as_str()), "{out}");
    }
    assert!(!out.contains("https://lab.example.invalid"), "{out}");

    // A launch catches work up the same way.
    let o = sb.launcher("work").args(["-p", "hi"]).output().unwrap();
    assert!(o.status.success(), "{o:?}");
    let s = local_settings_of(&sb, ".mcc/profiles/work");
    assert_eq!(s["env"]["SEARCH_TOKEN"], token.as_str(), "{s}");
    no_default_keys(&s, "work");
    let log = sb.read(".mcc/logs/mcc.log");
    assert!(!log.contains(&key) && !log.contains(&token));
}

/// #129 review: a profile's settings files left by an older mcc as links
/// to the default's own (dotfiles) files are replaced by files without the
/// default's account-bound keys, by `mcc sync` and by a launch; they are
/// never read as the profile's own settings.
#[test]
fn a_profile_settings_link_to_the_default_is_replaced_without_its_account_keys() {
    let sb = Sandbox::new();
    sb.seed_default();
    let key = ["sk", "ant", "LEAKCHECK", "linked0123456789"].join("-");
    set_env(&sb, ".claude", "ANTHROPIC_API_KEY", &key);
    sb.write(
        ".claude/settings.local.json",
        &serde_json::json!({"env": {"ANTHROPIC_API_KEY": key}, "x": 1}).to_string(),
    );
    let files = ["settings.json", "settings.local.json"];
    for f in files {
        sb.write(&format!("dotfiles/{f}"), &sb.read(&format!(".claude/{f}")));
        std::fs::remove_file(sb.path(&format!(".claude/{f}"))).unwrap();
        std::os::unix::fs::symlink(
            sb.path(&format!("dotfiles/{f}")),
            sb.path(&format!(".claude/{f}")),
        )
        .unwrap();
    }
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--clone");
    // As an older mcc left them: links to the default's real files.
    for p in ["work", "lab"] {
        for f in files {
            let rel = format!(".mcc/profiles/{p}/{f}");
            std::fs::remove_file(sb.path(&rel)).unwrap();
            std::os::unix::fs::symlink(sb.path(&format!("dotfiles/{f}")), sb.path(&rel)).unwrap();
        }
    }
    let default_before = sb.read("dotfiles/settings.json");
    let check = |p: &str| {
        for f in files {
            let rel = format!(".mcc/profiles/{p}/{f}");
            assert!(!sb.path(&rel).is_symlink(), "{rel}");
            assert!(!sb.read(&rel).contains(&key), "{rel}");
        }
    };
    let err = sync_stderr(&sb, &["work", "--yes"]);
    assert!(!err.contains(&key), "{err}");
    check("work");
    assert_eq!(sb.read("dotfiles/settings.json"), default_before);
    assert!(sb.read("dotfiles/settings.json").contains(&key));

    // A change in the default, so a launch catches lab up.
    let mut d = settings_of(&sb, ".claude");
    d["alwaysThinkingEnabled"] = true.into();
    sb.write("dotfiles/settings.json", &d.to_string());
    let o = sb.launcher("lab").args(["-p", "hi"]).output().unwrap();
    assert!(o.status.success(), "{o:?}");
    check("lab");
    assert_eq!(
        settings_of(&sb, ".mcc/profiles/lab")["alwaysThinkingEnabled"],
        true
    );
    assert!(sb.read("dotfiles/settings.json").contains(&key));
}
