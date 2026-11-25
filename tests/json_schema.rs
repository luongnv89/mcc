//! The `--json` contract: a bare array (schema 1) that existing scripts read
//! with `jq '.[]'`, and the opt-in `--json-v2` envelope `{"schema": 2, ...}`.

mod common;

use common::{Sandbox, json, stdout_of, with_profile};
use predicates::prelude::*;

fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    sb
}

/// `mcc ARGS` with usage off, parsed as JSON.
fn out(sb: &Sandbox, args: &[&str]) -> serde_json::Value {
    json(stdout_of(sb.mcc().env("MCC_NO_USAGE", "1").args(args)).as_bytes())
}

/// The envelope has exactly `schema` (first) and `key`, whose value is the
/// bare `--json` array.
fn assert_envelope(raw: &str, key: &str, bare: &serde_json::Value) {
    let v = json(raw.as_bytes());
    let obj = v
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {raw}"));
    assert_eq!(obj.len(), 2, "{raw}");
    assert_eq!(v["schema"], 2, "{raw}");
    assert_eq!(&v[key], bare, "{raw}");
    assert!(
        raw.trim_start().starts_with("{\n  \"schema\": 2,"),
        "schema comes first: {raw}"
    );
}

#[test]
fn list_json_stays_a_bare_array_and_json_v2_wraps_it() {
    let sb = sandbox();
    let bare = out(&sb, &["list", "--json"]);
    let names: Vec<&str> = bare
        .as_array()
        .expect("--json is still a bare array")
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["default", "work"]);

    let raw = stdout_of(
        sb.mcc()
            .env("MCC_NO_USAGE", "1")
            .args(["list", "--json-v2"]),
    );
    assert!(raw.ends_with("}\n"), "{raw}");
    assert_envelope(&raw, "profiles", &bare);
}

#[test]
fn usage_json_v2_wraps_the_same_profiles() {
    let sb = sandbox();
    for args in [&["usage"][..], &["usage", "work"][..]] {
        let bare = out(&sb, &[args, &["--json"]].concat());
        assert!(bare.is_array());
        let raw = stdout_of(
            sb.mcc()
                .env("MCC_NO_USAGE", "1")
                .args([args, &["--json-v2"]].concat()),
        );
        assert_envelope(&raw, "profiles", &bare);
    }
}

#[test]
fn project_list_json_v2_wraps_the_projects() {
    let sb = sandbox();
    // No projects yet: an empty array, and an envelope around it.
    assert_eq!(
        out(&sb, &["project", "list", "--json"]),
        serde_json::json!([])
    );
    let raw = stdout_of(sb.mcc().args(["project", "list", "--json-v2"]));
    assert_envelope(&raw, "projects", &serde_json::json!([]));

    let dir = sb.path("code/app");
    std::fs::create_dir_all(&dir).unwrap();
    sb.mcc()
        .args(["project", "set", "work", "--path"])
        .arg(&dir)
        .assert()
        .success();
    let bare = out(&sb, &["project", "list", "--json"]);
    assert_eq!(bare.as_array().unwrap().len(), 1);
    assert_eq!(bare[0]["profile"], "work");
    let raw = stdout_of(sb.mcc().args(["project", "list", "--json-v2"]));
    assert_envelope(&raw, "projects", &bare);
}

#[test]
fn doctor_json_v2_wraps_the_checks() {
    let sb = sandbox();
    let bare = out(&sb, &["doctor", "--json"]);
    assert!(!bare.as_array().expect("a bare array").is_empty());
    let raw = stdout_of(
        sb.mcc()
            .env("MCC_NO_USAGE", "1")
            .args(["doctor", "--json-v2"]),
    );
    assert_envelope(&raw, "checks", &bare);
}

#[test]
fn json_and_json_v2_conflict() {
    let sb = sandbox();
    for args in [
        &["list", "--json", "--json-v2"][..],
        &["list", "--long", "--json-v2"][..],
        &["usage", "--json", "--json-v2"][..],
        &["project", "list", "--json", "--json-v2"][..],
        &["doctor", "--json", "--json-v2"][..],
    ] {
        sb.mcc()
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn list_json_only_gains_is_global_default_then_provider_at_the_end() {
    // #104: `is_global_default` appended to each schema-1 object; `is_default`
    // stays where it was and keeps meaning ~/.claude. #124 appends
    // `provider` after it ("claude" for every Claude Code profile).
    let sb = sandbox();
    let bare = out(&sb, &["list", "--json"]);
    for p in bare.as_array().unwrap() {
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys[..3], ["name", "command", "is_default"], "{p}");
        assert_eq!(
            keys[keys.len() - 2..],
            ["is_global_default", "provider"],
            "{p}"
        );
        assert_eq!(p["is_global_default"], p["name"] == "default", "{p}");
        assert_eq!(p["provider"], "claude", "{p}");
    }
}

#[test]
fn usage_json_only_gains_the_login_fields() {
    // #100: four fields appended to each schema-1 object, the old ones
    // unchanged and in place; the v2 envelope carries the same objects.
    // #104 appends `is_global_default` after them, and #124 `provider`.
    let sb = sandbox();
    let bare = out(&sb, &["usage", "--json"]);
    for p in bare.as_array().unwrap() {
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "name",
                "command",
                "is_default",
                "account",
                "usage",
                "last_activity",
                "login_expires_at",
                "login_expires_source",
                "login_warning",
                "is_global_default",
                "provider"
            ],
            "{p}"
        );
        assert!(p["login_warning"].is_boolean(), "{p}");
        assert_eq!(p["provider"], "claude", "{p}");
    }
    let raw = stdout_of(
        sb.mcc()
            .env("MCC_NO_USAGE", "1")
            .args(["usage", "--json-v2"]),
    );
    assert_envelope(&raw, "profiles", &bare);
}

#[test]
fn usage_json_v2_gains_machines_only_when_they_are_set_up() {
    // #130: no [machines] (or no --remote), no `machines` key and no new
    // row field; with one and --remote, this machine's own `updated_at` and
    // `machines` follow `profiles`, and rows carry `account_id`.
    let sb = sandbox();
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    let v = json(stdout_of(sb.mcc().args(["usage", "--json-v2"])).as_bytes());
    assert_eq!(keys(&v), ["schema", "profiles"]);
    let row_keys = keys(&v["profiles"][0]);
    assert!(!row_keys.contains(&"account_id".to_string()), "{v}");

    let lab = Sandbox::new();
    lab.seed_default();
    sb.add_machine("lab", "lab-host", Some(&lab), None);
    let v = json(stdout_of(sb.mcc().args(["usage", "--json-v2"])).as_bytes());
    assert_eq!(keys(&v), ["schema", "profiles"]);
    let v = json(stdout_of(sb.mcc().args(["usage", "--json-v2", "--remote"])).as_bytes());
    assert_eq!(keys(&v), ["schema", "profiles", "updated_at", "machines"]);
    let default = &v["profiles"][0];
    assert_eq!(default["name"], "default");
    assert_eq!(
        default["account_id"].as_str().map(str::len),
        Some(64),
        "{v}"
    );
    assert_eq!(default["also_on"], serde_json::json!(["lab"]), "{v}");
    let m = &v["machines"][0];
    assert_eq!(keys(m), ["name", "host", "updated_at", "error", "profiles"]);
    assert_eq!(m["profiles"][0]["account_id"], default["account_id"]);
    // --json stays the bare array of this machine's profiles.
    let bare = json(stdout_of(sb.mcc().args(["usage", "--json"])).as_bytes());
    assert_eq!(keys(&bare[0]), row_keys);
}
