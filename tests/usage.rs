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

/// Turn `[usage] show_email` on in the sandbox's config.
fn show_email_in_config(sb: &common::Sandbox) {
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("show_email = false"), "{cfg}");
    sb.write(
        ".mcc/config.toml",
        &cfg.replace("show_email = false", "show_email = true"),
    );
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

    // `[usage] show_email = true` makes it the default; --hide-email wins.
    show_email_in_config(&sb);
    shown(&run(&["usage"]));
    assert_eq!(json_account(&["usage", "--json"]), "me@example.com");
    hidden(&run(&["usage", "--hide-email"]));
    // Other commands keep the setting when they save the config.
    sb.mcc()
        .args(["add", "other", "--fresh", "--no-login"])
        .assert()
        .success();
    assert!(sb.read(".mcc/config.toml").contains("show_email = true"));
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
