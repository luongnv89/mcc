//! Devin as a provider beside the Claude Code profiles (#124): `mcc devin`
//! starts the Devin CLI as it is, and `mcc list`, `mcc usage` and bare `mcc`
//! show its usage, looked up with the CLI's own login the way custats-macos
//! does. Every lookup here goes to a loopback stub; the default sandbox has
//! no Devin at all.

mod common;

use common::{
    DEVIN_CREDENTIALS, DEVIN_TOKEN, Sandbox, by_name, devin_stub, devin_stub_except,
    enable_live_usage, json, stdout_of,
};
use predicates::prelude::*;

/// A set-up sandbox (default profile, config written) with the Devin CLI
/// signed in.
fn devin_sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc().arg("init").assert().success();
    sb.seed_devin_login(DEVIN_TOKEN);
    sb
}

/// `mcc ARGS` with `stub` as Devin's API: (stdout, stderr).
fn run(sb: &Sandbox, stub: &common::UsageStub, args: &[&str]) -> (String, String) {
    let o = sb
        .mcc()
        .env("MCC_DEVIN_API_URL", stub.devin_base())
        .args(args)
        .assert()
        .success();
    let out = o.get_output();
    (
        String::from_utf8(out.stdout.clone()).unwrap(),
        String::from_utf8(out.stderr.clone()).unwrap(),
    )
}

fn rows(out: &str) -> Vec<serde_json::Value> {
    json(out.as_bytes()).as_array().unwrap().clone()
}

// ---- launching (AC3, AC4) ----------------------------------------------------

#[test]
fn mcc_devin_starts_the_cli_as_is() {
    let sb = devin_sandbox();
    sb.install_fake_devin();
    let creds = sb.read(DEVIN_CREDENTIALS);
    sb.mcc()
        .env("XDG_CONFIG_HOME", sb.path("xdg-config"))
        .args(["devin", "--", "--prompt", "hi there", "-v"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fake devin --prompt hi there -v"));
    // The arguments verbatim: no --config, nothing added.
    assert_eq!(sb.read("devin-args"), "--prompt\nhi there\n-v\n");
    // The environment as it was: HOME and XDG left alone, no Claude Code
    // profile variables.
    let env = sb.read("devin-env");
    for line in [
        format!("HOME={}", sb.home().display()),
        format!("XDG_CONFIG_HOME={}", sb.path("xdg-config").display()),
        "XDG_DATA_HOME unset".into(),
        "CLAUDE_CONFIG_DIR unset".into(),
        "MCC_PROFILE unset".into(),
        "DEVIN_CONFIG unset".into(),
    ] {
        assert!(env.lines().any(|l| l == line), "{line:?} in {env}");
    }
    // Devin's login is untouched, and mcc writes no launcher for it.
    assert_eq!(sb.read(DEVIN_CREDENTIALS), creds);
    assert!(!sb.exists(".local/bin/devin"));
}

#[test]
fn mcc_devin_runs_without_setup_and_passes_the_exit_status() {
    // Not set up: no first-run setup, no config written.
    let sb = Sandbox::new();
    sb.install_fake_devin();
    sb.mcc()
        .env("FAKE_DEVIN_EXIT", "3")
        .args(["devin", "--", "auth", "status"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("First run").not())
        // Signing in is how one gets a login: no hint about it then.
        .stderr(predicate::str::contains("no Devin login").not());
    assert_eq!(sb.read("devin-args"), "auth\nstatus\n");
    assert!(!sb.exists(".mcc/config.toml"));

    // Without a login, other commands still start, with a hint.
    sb.mcc()
        .arg("devin")
        .assert()
        .success()
        .stderr(predicate::str::contains("devin auth login"));
}

#[test]
fn mcc_devin_finds_the_binary_like_devin_installs_it() {
    let sb = devin_sandbox();
    // Not on PATH: ~/.local/bin/devin (where Devin's installer links it).
    let fake = sb.install_fake_devin();
    let local = sb.path(".local/bin/devin");
    std::fs::rename(&fake, &local).unwrap();
    let shown = stdout_of(sb.mcc().args(["devin", "--dry-run", "--", "a b"]));
    assert_eq!(shown, format!("'{}' 'a b'\n", local.display()));
    assert!(!sb.exists("devin-args"), "dry run started devin");
    // MCC_DEVIN_BIN wins.
    let other = sb.path("elsewhere/devin");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::copy(&local, &other).unwrap();
    let shown = stdout_of(
        sb.mcc()
            .env("MCC_DEVIN_BIN", &other)
            .args(["devin", "--dry-run"]),
    );
    assert_eq!(shown, format!("'{}'\n", other.display()));
}

#[test]
fn mcc_devin_without_the_cli_says_how_to_get_it() {
    let sb = devin_sandbox();
    sb.mcc()
        .arg("devin")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("`devin` command was not found"))
        .stderr(predicate::str::contains("install the Devin CLI"));
    assert!(!sb.exists(".local/bin/devin"));
}

// ---- detection and the default (cached) view (AC1, AC2) -----------------------

#[test]
fn no_devin_login_no_devin_row() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.install_fake_devin();
    let stub = devin_stub();
    let list = rows(&run(&sb, &stub, &["list", "--json"]).0);
    let names: Vec<&str> = list.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["default"]);
    let (out, _) = run(&sb, &stub, &["usage"]);
    assert!(!out.contains("PROVIDER") && !out.contains("devin"), "{out}");
    sb.mcc()
        .args(["usage", "devin"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Devin is not set up"))
        .stderr(predicate::str::contains("devin auth login"));
    assert!(stub.requests().is_empty());
}

#[test]
fn devin_shows_without_a_lookup_until_one_is_made() {
    let sb = devin_sandbox();
    let stub = devin_stub();
    let (out, err) = run(&sb, &stub, &["usage"]);
    assert!(
        out.contains("PROVIDER  DAILY  RESETS IN  WEEKLY  RESETS IN"),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|l| l == "devin     — (not looked up; `mcc usage --all` looks it up)"),
        "{out}"
    );
    assert!(!err.contains("Devin"), "{err}");

    let (out, _) = run(&sb, &stub, &["list"]);
    let line = out.lines().find(|l| l.starts_with("devin")).expect(&out);
    assert!(line.contains("n/a"), "{line}");
    // The footer counts Claude Code profiles only.
    let (_, err) = run(&sb, &stub, &["list"]);
    assert!(err.contains("1 profile ·"), "{err}");
    // A hint says how to look Devin up: `mcc list` would with live usage on,
    // bare `mcc` never does.
    assert!(
        err.contains("→ Devin not looked up yet: run `mcc usage --all` or set [usage] live = true"),
        "{err}"
    );
    let (_, err) = run(&sb, &stub, &[]);
    assert!(
        err.contains("→ Devin not looked up yet: run `mcc usage --all` to see its usage"),
        "{err}"
    );
    let (_, err) = run(&sb, &stub, &["list", "--json"]);
    assert!(!err.contains("not looked up"), "{err}");

    let list = rows(&run(&sb, &stub, &["list", "--json"]).0);
    let d = by_name(&list, "devin");
    assert_eq!(d["provider"], "devin");
    assert_eq!(d["usage"]["reason"], "not_looked_up");
    assert_eq!(d["skills"], serde_json::Value::Null);
    assert_eq!(d["size_bytes"], serde_json::Value::Null);
    assert_eq!(by_name(&list, "default")["provider"], "claude");
    let keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys.last(), Some(&"provider"));

    let usage = rows(&run(&sb, &stub, &["usage", "--json"]).0);
    let d = by_name(&usage, "devin");
    assert_eq!(d["provider"], "devin");
    assert_eq!(d["login_warning"], false);
    assert_eq!(d["account"], serde_json::Value::Null);

    // Cached view only: no token read, nothing sent.
    assert!(stub.requests().is_empty());
}

#[test]
fn json_v2_carries_the_devin_row_in_the_same_envelope() {
    let sb = devin_sandbox();
    let stub = devin_stub();
    for cmd in ["list", "usage"] {
        let bare = json(run(&sb, &stub, &[cmd, "--json"]).0.as_bytes());
        let v2 = json(run(&sb, &stub, &[cmd, "--json-v2"]).0.as_bytes());
        let obj = v2.as_object().unwrap();
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(keys, ["schema", "profiles"], "{cmd}: {v2}");
        assert_eq!(v2["schema"], 2);
        assert_eq!(v2["profiles"], bare, "{cmd}");
        let rows = bare.as_array().unwrap();
        assert_eq!(by_name(rows, "devin")["provider"], "devin", "{cmd}");
    }
}

#[test]
fn usage_devin_alone_shows_no_claude_disclosure() {
    // Live on, neither note shown yet: `mcc usage devin` looks up Devin
    // only, so only Devin's note is shown and recorded.
    let sb = devin_sandbox();
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &cfg.replacen("[usage]\n", "[usage]\nlive = true\n", 1),
    );
    let stub = devin_stub();
    let (_, err) = run(&sb, &stub, &["usage", "devin"]);
    assert!(
        err.contains("Live usage lookup is on for Devin too"),
        "{err}"
    );
    assert!(!err.contains("Live usage lookup is on ("), "{err}");
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("devin_disclosed_at"), "{cfg}");
    assert!(!cfg.contains("live_disclosed_at"), "{cfg}");
}

// ---- live lookups (AC2, AC4, AC5) ---------------------------------------------

#[test]
fn usage_all_looks_devin_up_with_its_cli_login_and_saves_it() {
    let sb = devin_sandbox();
    let stub = devin_stub();
    let creds = sb.read(DEVIN_CREDENTIALS);
    let (out, err) = run(&sb, &stub, &["usage", "--all"]);

    // The CLI's own token, the primary organization, then its quota.
    let reqs = stub.requests();
    let paths: Vec<&str> = reqs.iter().map(|r| r.path()).collect();
    assert_eq!(
        paths,
        ["/api/organizations", "/api/org-main/billing/quota/usage"]
    );
    let ua = format!("mcc/{}", env!("CARGO_PKG_VERSION"));
    for r in &reqs {
        assert_eq!(
            r.authorization.as_deref(),
            Some(format!("Bearer {DEVIN_TOKEN}").as_str())
        );
        assert_eq!(r.user_agent.as_deref(), Some(ua.as_str()));
    }
    assert_eq!(reqs[0].org, None);
    assert_eq!(reqs[1].org.as_deref(), Some("org-main"));

    // Daily and weekly, never a 5-hour window; the overage in dollars.
    let devin_line = out.lines().find(|l| l.starts_with("devin ")).expect(&out);
    assert!(
        devin_line.contains("12%") && devin_line.contains("54%"),
        "{devin_line}"
    );
    assert!(devin_line.contains("$3.50"), "{devin_line}");
    let head = out.lines().find(|l| l.starts_with("PROVIDER")).expect(&out);
    assert!(
        head.contains("DAILY") && head.contains("WEEKLY") && head.contains("OVERAGE"),
        "{head}"
    );
    assert!(!head.contains("5-HOUR"), "{head}");
    assert!(err.contains("https://app.devin.ai/api"), "{err}");

    // Saved for the next look, without the token; the login file untouched.
    let cache = sb.read(".mcc/providers/devin/quota.json");
    assert!(!cache.contains(DEVIN_TOKEN), "{cache}");
    assert!(cache.contains("org-main"), "{cache}");
    assert_eq!(sb.read(DEVIN_CREDENTIALS), creds);

    // Plain `mcc usage` shows it from the cache, with its age, and sends
    // nothing.
    let (out, _) = run(&sb, &stub, &["usage", "devin"]);
    let devin_line = out.lines().find(|l| l.starts_with("devin ")).expect(&out);
    assert!(
        devin_line.contains("54%") && devin_line.ends_with("just now"),
        "{out}"
    );
    assert!(!out.contains("default"), "{out}");
    let one = rows(&run(&sb, &stub, &["usage", "devin", "--json"]).0);
    assert_eq!(one.len(), 1);
    let u = &one[0]["usage"];
    assert_eq!(u["source"], "cache");
    assert_eq!(u["daily"]["percent"], 12.0);
    assert_eq!(u["weekly"]["percent"], 54.0);
    assert_eq!(u["five_hour"], serde_json::Value::Null);
    assert_eq!(u["overage_usd"], 3.5);
    let list = rows(&run(&sb, &stub, &["list", "--json"]).0);
    let d = by_name(&list, "devin");
    assert_eq!(d["organization"], "Main team");
    assert_eq!(d["usage"]["weekly"]["percent"], 54.0);
    // A Claude profile's usage has no Devin fields.
    let u = &by_name(&list, "default")["usage"];
    assert!(
        u.get("daily").is_none() && u.get("overage_usd").is_none(),
        "{u}"
    );
    assert_eq!(stub.requests().len(), 2);
    // Looked up: no more hint.
    for args in [&["list"][..], &[]] {
        let (_, err) = run(&sb, &stub, args);
        assert!(!err.contains("not looked up"), "{args:?}: {err}");
    }

    // The next live lookup reuses the organization it found.
    run(&sb, &stub, &["usage", "--all"]);
    let reqs = stub.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[2].path(), "/api/org-main/billing/quota/usage");
}

#[test]
fn live_usage_looks_devin_up_after_a_one_time_note() {
    let sb = devin_sandbox();
    enable_live_usage(&sb);
    let stub = devin_stub();
    let (out, err) = run(&sb, &stub, &["list"]);
    assert!(
        err.contains("Live usage lookup is on for Devin too"),
        "{err}"
    );
    assert!(err.contains("https://app.devin.ai/api"), "{err}");
    let line = out.lines().find(|l| l.starts_with("devin")).expect(&out);
    assert!(line.contains("Main team") && line.contains("54%"), "{line}");
    let cfg = sb.read(".mcc/config.toml");
    assert!(
        regex::Regex::new(r#"(?m)^devin_disclosed_at = "\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ"$"#)
            .unwrap()
            .is_match(&cfg),
        "{cfg}"
    );
    // Once.
    let (_, err) = run(&sb, &stub, &["usage"]);
    assert!(!err.contains("for Devin too"), "{err}");
    assert_eq!(stub.requests().len(), 3);

    // Bare `mcc` reads the cache only, even with live on.
    let (out, _) = run(&sb, &stub, &[]);
    let line = out.lines().find(|l| l.starts_with("devin")).expect(&out);
    assert!(line.contains("54%"), "{line}");
    assert_eq!(stub.requests().len(), 3);
}

#[test]
fn mcc_no_usage_never_reads_or_sends_the_devin_login() {
    let sb = devin_sandbox();
    enable_live_usage(&sb);
    let stub = devin_stub();
    for args in [&["usage"][..], &["usage", "--all"], &["list"], &[]] {
        let o = sb
            .mcc()
            .env("MCC_DEVIN_API_URL", stub.devin_base())
            .env("MCC_NO_USAGE", "1")
            .args(args)
            .assert()
            .success();
        let err = String::from_utf8(o.get_output().stderr.clone()).unwrap();
        assert!(!err.contains("Devin too"), "{args:?}: {err}");
    }
    assert!(stub.requests().is_empty());
    assert!(!sb.read(".mcc/config.toml").contains("devin_disclosed_at"));
    let usage = rows(&stdout_of(
        sb.mcc().env("MCC_NO_USAGE", "1").args(["usage", "--json"]),
    ));
    assert_eq!(by_name(&usage, "devin")["usage"]["reason"], "disabled");
}

#[test]
fn a_stale_cached_organization_is_looked_up_again() {
    // The cache names an organization the login no longer has (the Devin CLI
    // signed in to another account, say): the live lookup finds the login's
    // organization again and saves that one.
    let sb = devin_sandbox();
    sb.write(
        ".mcc/providers/devin/quota.json",
        r#"{"updated_at":1700000000,"org_id":"org-old","org_name":"Old team"}"#,
    );
    let stub = devin_stub_except(|req| {
        (req.path() == "/api/org-old/billing/quota/usage")
            .then(|| (403, r#"{"detail":"Unauthorized"}"#.into()))
    });
    let usage = rows(&run(&sb, &stub, &["usage", "--all", "--json"]).0);
    let d = by_name(&usage, "devin");
    assert_eq!(d["usage"]["reason"], serde_json::Value::Null, "{d}");
    assert_eq!(d["usage"]["weekly"]["percent"], 54.0);
    let paths: Vec<String> = stub.requests().iter().map(|r| r.path().into()).collect();
    assert_eq!(
        paths,
        [
            "/api/org-old/billing/quota/usage",
            "/api/organizations",
            "/api/org-main/billing/quota/usage",
        ]
    );
    let cache = sb.read(".mcc/providers/devin/quota.json");
    assert!(cache.contains("\"org-main\""), "{cache}");
    assert!(!cache.contains("org-old"), "{cache}");
    assert!(!cache.contains(DEVIN_TOKEN), "{cache}");
    let list = rows(&run(&sb, &stub, &["list", "--json"]).0);
    assert_eq!(by_name(&list, "devin")["organization"], "Main team");

    // The next lookup goes straight to the new organization.
    run(&sb, &stub, &["usage", "--all"]);
    let reqs = stub.requests();
    assert_eq!(reqs.len(), 4);
    assert_eq!(reqs[3].path(), "/api/org-main/billing/quota/usage");
}

#[test]
fn a_rejected_devin_login_says_to_sign_in_again() {
    let sb = devin_sandbox();
    sb.seed_devin_login("test-stale-token");
    let stub = devin_stub();
    let (out, _) = run(&sb, &stub, &["usage", "--all"]);
    assert!(
        out.contains(
            "devin     unavailable (Devin rejected the session token; run `devin auth login`)"
        ),
        "{out}"
    );
    let usage = rows(&run(&sb, &stub, &["usage", "--all", "--json"]).0);
    assert_eq!(by_name(&usage, "devin")["usage"]["reason"], "unauthorized");
    // Nothing cached from a failed lookup.
    assert!(!sb.exists(".mcc/providers/devin/quota.json"));

    // An empty token is never sent.
    sb.write(DEVIN_CREDENTIALS, "windsurf_api_key = \"\"\n");
    let before = stub.requests().len();
    let usage = rows(&run(&sb, &stub, &["usage", "--all", "--json"]).0);
    assert_eq!(by_name(&usage, "devin")["usage"]["reason"], "missing_token");
    assert_eq!(stub.requests().len(), before);
}

#[test]
fn devin_login_follows_xdg_data_home() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.write(
        "xdg-data/devin/credentials.toml",
        &format!("windsurf_api_key = \"{DEVIN_TOKEN}\"\n"),
    );
    let stub = devin_stub();
    let o = sb
        .mcc()
        .env("MCC_DEVIN_API_URL", stub.devin_base())
        .env("XDG_DATA_HOME", sb.path("xdg-data"))
        .args(["usage", "--all", "--json"])
        .assert()
        .success();
    let usage = rows(&String::from_utf8(o.get_output().stdout.clone()).unwrap());
    assert_eq!(by_name(&usage, "devin")["usage"]["weekly"]["percent"], 54.0);
    // Without XDG_DATA_HOME, that file is not Devin's login.
    let list = rows(&stdout_of(sb.mcc().args(["list", "--json"])));
    assert!(list.iter().all(|p| p["name"] != "devin"));
}

#[test]
fn devin_help_documents_the_launch_and_usage() {
    let sb = Sandbox::new();
    let help = stdout_of(sb.mcc().args(["devin", "--help"]));
    for needle in ["default configuration", "--dry-run", "no --config"] {
        assert!(help.contains(needle), "missing {needle:?} in:\n{help}");
    }
    let help = stdout_of(sb.mcc().args(["usage", "--help"]));
    assert!(help.contains("app.devin.ai"), "{help}");
}
