//! A label and a color per profile (issue #55): set with `mcc add` or
//! `mcc config set`, and shown by `mcc list`, `mcc run` and the
//! `mcc statusline --prefix` segment.

mod common;

use std::time::Duration;

use common::{Sandbox, json, stdout_of, with_profile};
use predicates::prelude::*;

const CONFIG: &str = ".mcc/config.toml";
const STATUS: &str = r#"{"model":{"id":"claude-opus-5"}}"#;

fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    sb
}

fn set(sb: &Sandbox, key: &str, value: &str) {
    sb.mcc()
        .args(["config", "set", key, value])
        .assert()
        .success();
}

#[test]
fn add_records_a_color_and_rejects_unknown_ones() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--fresh", "--color", " Blue "])
        .assert()
        .success();
    assert!(
        sb.read(CONFIG).contains("color = \"blue\"\n"),
        "{}",
        sb.read(CONFIG)
    );

    for bad in ["", "purple", "#ff0000", "bright-red"] {
        sb.mcc()
            .args(["add", "lab", "--fresh", "--color", bad])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("color"))
            .stderr(predicate::str::contains("black, red, green"));
    }
    assert!(!sb.exists(".mcc/profiles/lab"));
}

#[test]
fn config_set_changes_label_and_color_of_an_existing_profile() {
    let sb = sandbox();
    assert!(!sb.read(CONFIG).contains("color"));
    sb.mcc()
        .args(["config", "get", "profiles.work.color"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("is not set"));

    set(&sb, "profiles.work.label", "Acme (work)");
    set(&sb, "profiles.work.color", "GREEN");
    assert_eq!(
        stdout_of(sb.mcc().args(["config", "get", "profiles.work.color"])),
        "green\n"
    );
    set(&sb, "profiles.work.color", "magenta");
    assert!(sb.read(CONFIG).contains("color = \"magenta\"\n"));

    // An invalid color is refused and leaves the file as it was.
    let before = sb.read(CONFIG);
    sb.mcc()
        .args(["config", "set", "profiles.work.color", "teal"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid color"));
    sb.mcc()
        .args(["config", "set", "profiles.ghost.color", "red"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named"));
    assert_eq!(sb.read(CONFIG), before);

    // "" removes it.
    set(&sb, "profiles.work.color", "");
    assert!(!sb.read(CONFIG).contains("color"), "{}", sb.read(CONFIG));
}

#[test]
fn config_edit_rejects_an_unknown_color() {
    let sb = sandbox();
    let before = sb.read(CONFIG);
    sb.write(
        "editor.sh",
        "printf '[profiles.w]\\ndir = \"/d\"\\nmode = \"clone\"\\ncreated_at = \"t\"\\ncolor = \"teal\"\\n' > \"$1\"\n",
    );
    let editor = format!("sh {}", sb.path("editor.sh").display());
    sb.mcc()
        .env("VISUAL", &editor)
        .env("EDITOR", &editor)
        .args(["config", "edit"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("profiles.w.color"));
    assert_eq!(sb.read(CONFIG), before);
}

#[test]
fn list_shows_the_color_in_json_and_long_cards() {
    let sb = sandbox();
    set(&sb, "profiles.work.color", "blue");
    let v = json(&stdout_of(sb.mcc().args(["list", "--json", "--no-usage"])).into_bytes());
    let arr = v.as_array().unwrap();
    assert!(arr[0]["color"].is_null(), "{v}");
    assert_eq!(arr[1]["color"], "blue");
    let v2 = json(&stdout_of(sb.mcc().args(["list", "--json-v2", "--no-usage"])).into_bytes());
    assert_eq!(v2["profiles"][1]["color"], "blue");

    // Not a terminal (and NO_COLOR): the table has no escapes.
    let table = stdout_of(sb.mcc().args(["list", "--no-usage"]));
    assert!(!table.contains('\u{1b}'), "{table:?}");
    assert!(table.lines().any(|l| l.starts_with("work ")), "{table}");
    assert!(
        stdout_of(sb.mcc().args(["list", "--long", "--no-usage"]))
            .contains("Color:           blue")
    );
}

#[test]
fn run_names_the_profile_with_its_label() {
    let sb = sandbox();
    set(&sb, "profiles.work.label", "Acme (work)");
    set(&sb, "profiles.work.color", "red");
    sb.mcc()
        .env("MCC_NO_USAGE", "1")
        .args(["run", "--dry-run", "--profile", "work"])
        .assert()
        .success()
        .stdout("work\n")
        .stderr(predicate::str::contains("would start `work` (Acme (work))"))
        .stderr(predicate::str::contains('\u{1b}').not());

    // A label with an email stays hidden while emails are.
    set(&sb, "profiles.work.label", "me@example.com's Organization");
    sb.mcc()
        .env("MCC_NO_USAGE", "1")
        .args(["run", "--dry-run", "--profile", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("would start `work` — "))
        .stderr(predicate::str::contains("me@example.com").not());
}

/// `mcc statusline --prefix` as Claude Code runs it for the work profile,
/// with NO_COLOR set or not.
fn render(sb: &Sandbox, no_color: bool) -> String {
    let base = sb.mcc();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_mcc"));
    c.env_clear()
        .envs(
            base.get_envs()
                .filter(|(k, _)| no_color || *k != "NO_COLOR")
                .filter_map(|(k, v)| v.map(|v| (k, v))),
        )
        .env("CLAUDE_CONFIG_DIR", sb.path(".mcc/profiles/work"))
        .current_dir(sb.home())
        .args(["statusline", "--prefix"]);
    let out = assert_cmd::Command::from_std(c)
        .write_stdin(STATUS)
        .timeout(Duration::from_secs(20))
        .assert()
        .success()
        .get_output()
        .clone();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn statusline_prefix_has_the_label_and_the_color() {
    let sb = sandbox();
    assert_eq!(render(&sb, true), "[work]");

    set(&sb, "profiles.work.label", "Acme (work)");
    assert_eq!(render(&sb, true), "[work · Acme (work)]");
    assert_eq!(render(&sb, false), "[work · Acme (work)]");

    // Colored for Claude Code, which renders the escapes, unless NO_COLOR.
    set(&sb, "profiles.work.color", "blue");
    assert_eq!(render(&sb, true), "[work · Acme (work)]");
    let colored = render(&sb, false);
    assert!(colored.contains("\u{1b}[34"), "{colored:?}");
    assert_eq!(console::strip_ansi_codes(&colored), "[work · Acme (work)]");
}
