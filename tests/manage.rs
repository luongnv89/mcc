//! `mcc list`, `mcc sync`, `mcc delete`, and cross-cutting error behavior.

mod common;

use std::time::Duration;

use common::{REFUSED_URL, Sandbox, UsageStub, json, now_ms};
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
        .stdout(
            predicate::str::is_match(r"(?m)^NAME\s+COMMAND\s+ACCOUNT\s+5H\s+WEEK\s+SKILLS")
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

const HOUR_MS: i64 = 3_600_000;

fn rfc3339_in(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn stdout_of(cmd: &mut assert_cmd::Command) -> String {
    String::from_utf8(cmd.assert().success().get_output().stdout.clone()).unwrap()
}

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

fn by_name<'a>(arr: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    arr.iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("no profile {name}"))
}

/// default + `work` logged in (each with its own token), `lab` logged out.
fn usage_sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--fresh");
    sb.seed_oauth(".claude", "test-token-default", now_ms() + HOUR_MS);
    sb.seed_oauth(".mcc/profiles/work", "test-token-work", now_ms() + HOUR_MS);
    sb
}

/// Default answers with the flat keys, work with the `limits[]` shape.
fn usage_stub() -> UsageStub {
    let (five, week) = (
        rfc3339_in(2 * 3600 + 14 * 60),
        rfc3339_in(3 * 86400 + 4 * 3600),
    );
    UsageStub::start(move |req| match req.authorization.as_deref() {
        Some("Bearer test-token-default") => (
            200,
            format!(
                r#"{{"five_hour":{{"utilization":42.4,"resets_at":"{five}"}},"seven_day":{{"utilization":18,"resets_at":"{week}"}},"seven_day_opus":null}}"#
            ),
        ),
        Some("Bearer test-token-work") => (
            200,
            format!(
                r#"{{"five_hour":null,"seven_day":null,"limits":[{{"kind":"session","percent":7,"resets_at":"{five}"}},{{"kind":"weekly_all","percent":64,"resets_at":"{week}"}}]}}"#
            ),
        ),
        _ => (401, r#"{"error":"unknown token"}"#.into()),
    })
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
    has(r"(?m)^NAME\s+COMMAND\s+ACCOUNT\s+5H\s+WEEK\s+SKILLS");
    has(
        r"(?m)^default ★\s+claude\s+me@example\.com\s+████░░░░░░ 42% \(2h\)\s+██░░░░░░░░ 18% \(3d\)\s+2\s",
    );
    has(r"(?m)^work\s+work\s+—\s+█░░░░░░░░░ 7% \(2h\)\s+██████░░░░ 64% \(3d\)\s");
    has(r"(?m)^lab\s+lab\s+—\s+unavailable\s+unavailable\s");

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
                r"(?m)^default ★\s+claude\s+me@example\.com\s+unavailable\s+unavailable\s",
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
            predicate::str::is_match(r"(?m)^default ★\s+claude\s+me@example\.com\s+—\s+—\s+2\s")
                .unwrap(),
        )
        .stdout(predicate::str::contains("unavailable").not());
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["list", "--long", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Usage:           — (not fetched: --no-usage)",
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

    sb.mcc().args(["sync", "work", "--yes"]).assert().success();
    assert_eq!(snapshot(&sb.path(".claude/plugins")), default_before);
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
