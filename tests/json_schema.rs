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
fn json_and_json_v2_conflict() {
    let sb = sandbox();
    for args in [
        &["list", "--json", "--json-v2"][..],
        &["list", "--long", "--json-v2"][..],
        &["usage", "--json", "--json-v2"][..],
        &["project", "list", "--json", "--json-v2"][..],
    ] {
        sb.mcc()
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("cannot be used with"));
    }
}
