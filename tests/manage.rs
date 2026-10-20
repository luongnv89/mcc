//! `mcc list`, `mcc sync`, `mcc delete`, and cross-cutting error behavior.

mod common;

use std::time::Duration;

use common::{
    HOUR_MS, REFUSED_URL, Sandbox, UsageStub, by_name, enable_live_usage, json, now_ms, rfc3339_in,
    stdout_of, usage_sandbox, usage_stub, with_profile,
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
        .stdout(
            predicate::str::is_match(r"(?m)^NAME\s+COMMAND\s+ACCOUNT\s+ORG\s+5H\s+WEEK\s+SKILLS")
                .unwrap(),
        )
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
    has(r"(?m)^NAME\s+COMMAND\s+ACCOUNT\s+ORG\s+5H\s+WEEK\s+SKILLS");
    has(
        r"(?m)^default ★\s+claude\s+me@example\.com\s+Acme\s+████░░░░░░ 42% \(2h\)\s+██░░░░░░░░ 18% \(3d\)\s+2\s",
    );
    has(
        r"(?m)^work\s+work\s+fake@example\.com\s+—\s+█░░░░░░░░░ 7% \(2h\)\s+██████░░░░ 64% \(3d\)\s",
    );
    has(r"(?m)^lab\s+lab\s+fake@example\.com\s+—\s+unavailable\s+unavailable\s");

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
            predicate::str::is_match(
                r"(?m)^default ★\s+claude\s+me@example\.com\s+Acme\s+unavailable\s+unavailable\s",
            )
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
        .stdout(
            predicate::str::is_match(
                r"(?m)^default ★\s+claude\s+me@example\.com\s+Acme\s+—\s+—\s+2\s",
            )
            .unwrap(),
        )
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
        regex::Regex::new(r"(?m)^default ★\s+claude\s+me@example\.com\s+Acme\s+█████████░ 85%! \(2h\)\s+██████████ 96%!! \(3d\)\s")
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
        regex::Regex::new(
            r"(?m)^default ★\s+claude\s+me@example\.com\s+Acme\s+42% \(2h\)\s+18% \(3d\)\s+2\s"
        )
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
    // From 80 columns the table keeps a compact bar even when it wraps.
    let medium = list("120", &["list"]);
    assert!(
        regex::Regex::new(
            r"(?m)^default ★\s+claude\s+me@example\.com\s+Acme\s+██░░░ 42% \(2h\)\s+█░░░░ 18% \(3d\)\s"
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
    with_profile(&sb, "work", "--clone");
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
    with_profile(&sb, "work", "--clone");
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
        .stderr(predicate::str::contains("ignoring"));
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my rules\n");
    assert_eq!(json(sb.read(&manifest_path()).as_bytes())["version"], 1);
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
            .find(|l| l.starts_with(name))
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
fn settings_overlay_keeps_local_edits_and_conflicts_three_way() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    add_settings_overlay(&sb);
    sb.mcc().args(["sync", "work"]).assert().success();

    // An edit in the profile is a local edit, kept.
    let mut s = work_settings(&sb);
    s["mine"] = true.into();
    sb.write(".mcc/profiles/work/settings.json", &s.to_string());
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "= settings.json (local edit, kept)",
        ));
    // A default change on top of it is a conflict.
    sb.write(".claude/settings.json", r#"{"model":"haiku"}"#);
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("! settings.json (conflict"));
    assert_eq!(work_settings(&sb)["mine"], true);
    // --force takes the merged result.
    sb.mcc()
        .args(["sync", "work", "--force"])
        .assert()
        .success();
    let s = work_settings(&sb);
    assert!(s.get("mine").is_none(), "{s}");
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
