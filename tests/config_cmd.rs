//! `mcc config get|set|edit|path` (issue #48): validated reads and writes of
//! `~/.mcc/config.toml`; a rejected value or edit leaves the file unchanged.

mod common;

use std::fs::File;
use std::os::fd::AsRawFd;
use std::time::Duration;

use common::{Sandbox, stdout_of, with_profile};
use predicates::str::contains;

fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb
}

const CONFIG: &str = ".mcc/config.toml";

/// An `$EDITOR` that runs `script` with the file to edit as `$1`.
fn editor(sb: &Sandbox, script: &str) -> String {
    sb.write("editor.sh", script);
    format!("sh {}", sb.path("editor.sh").display())
}

/// No edit copy left behind in ~/.mcc.
fn no_edit_copies(sb: &Sandbox) {
    let left: Vec<_> = std::fs::read_dir(sb.path(".mcc"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("config.edit-"))
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

#[test]
fn path_prints_the_config_file() {
    let sb = sandbox();
    let out = stdout_of(sb.mcc().args(["config", "path"]));
    assert_eq!(out.trim(), sb.path(CONFIG).display().to_string());
}

#[test]
fn get_reads_values_and_defaults() {
    let sb = sandbox();
    assert_eq!(
        stdout_of(sb.mcc().args(["config", "get", "usage.live"])),
        "false\n"
    );
    assert_eq!(
        stdout_of(sb.mcc().args(["config", "get", "profiles.work.mode"])),
        "clone\n"
    );
    sb.mcc()
        .args(["config", "get", "nope.nothing"])
        .assert()
        .code(1)
        .stderr(contains("is not set"));
    sb.mcc()
        .args(["config", "get", "profiles.work.label"])
        .assert()
        .code(1)
        .stderr(contains("not set"));
    sb.mcc()
        .args(["config", "get", "profiles.ghost.label"])
        .assert()
        .code(2)
        .stderr(contains("no profile named \"ghost\""));
}

#[test]
fn set_writes_valid_values_and_keeps_comments() {
    let sb = sandbox();
    let commented = sb
        .read(CONFIG)
        .replace("[usage]\n", "# usage settings\n[usage]\n");
    sb.write(CONFIG, &commented);

    sb.mcc()
        .args(["config", "set", "usage.live", "true"])
        .assert()
        .success();
    sb.mcc()
        .args(["config", "set", "sync.exclude", "plugins, mcp"])
        .assert()
        .success();
    sb.mcc()
        .args(["config", "set", "profiles.work.label", "Acme (work)"])
        .assert()
        .success();

    let text = sb.read(CONFIG);
    assert!(text.contains("# usage settings\n[usage]\n"), "{text}");
    assert_eq!(
        stdout_of(sb.mcc().args(["config", "get", "usage.live"])),
        "true\n"
    );
    assert_eq!(
        stdout_of(sb.mcc().args(["config", "get", "sync.exclude"])),
        "plugins\nmcp\n"
    );
    assert_eq!(
        stdout_of(sb.mcc().args(["config", "get", "profiles.work.label"])),
        "Acme (work)\n"
    );

    // "" clears a list and removes a label.
    sb.mcc()
        .args(["config", "set", "sync.exclude", ""])
        .assert()
        .success();
    sb.mcc()
        .args(["config", "set", "profiles.work.label", ""])
        .assert()
        .success();
    assert_eq!(
        stdout_of(sb.mcc().args(["config", "get", "sync.exclude"])),
        ""
    );
    assert!(!sb.read(CONFIG).contains("label"), "{}", sb.read(CONFIG));
}

#[test]
fn invalid_values_leave_the_file_unchanged() {
    let sb = sandbox();
    let before = sb.read(CONFIG);
    for (args, code, msg) in [
        (["usage.live", "maybe"], 2, "expected true or false"),
        (
            ["sync.exclude", "plugins,nope"],
            2,
            "unknown sync item \"nope\"",
        ),
        (["sync.extra", "../outside"], 3, "relative path inside"),
        (["profiles.work.label", "a\u{7}b"], 2, "control characters"),
        (["profiles.ghost.label", "x"], 2, "no profile named"),
        (["profiles.work.share_group", "acme"], 2, "mcc share work"),
        (
            ["profiles.work.settings.model", "sonnet"],
            2,
            "[profiles.work.settings] with `mcc config edit`",
        ),
        (["projects.\"/x\".profile", "work"], 2, "mcc project set"),
        (["usage.nope", "1"], 2, "unknown config key"),
    ] {
        sb.mcc()
            .arg("config")
            .arg("set")
            .args(args)
            .assert()
            .code(code)
            .stderr(contains(msg));
        assert_eq!(sb.read(CONFIG), before, "changed by {args:?}");
    }
}

/// Issue #57: the email setting lives in `[privacy]`; `usage.show_email`,
/// its old name, still reads and sets it.
#[test]
fn privacy_show_email_is_settable_under_both_names() {
    let sb = sandbox();
    let get = |key: &str| stdout_of(sb.mcc().args(["config", "get", key]));
    assert_eq!(get("privacy.show_email"), "false\n");
    sb.mcc()
        .args(["config", "set", "privacy.show_email", "true"])
        .assert()
        .success();
    assert!(sb.read(CONFIG).contains("[privacy]\nshow_email = true\n"));
    assert_eq!(get("privacy.show_email"), "true\n");
    assert_eq!(get("usage.show_email"), "true\n");
    sb.mcc()
        .args(["config", "set", "usage.show_email", "false"])
        .assert()
        .success();
    assert_eq!(get("privacy.show_email"), "false\n");
}

#[test]
fn edit_saves_a_valid_edit() {
    let sb = sandbox();
    let ed = editor(
        &sb,
        "printf '\\n[privacy]\\nshow_email = true\\n# my note\\n' >> \"$1\"\n",
    );
    sb.mcc()
        .args(["config", "edit"])
        .env("EDITOR", ed)
        .assert()
        .success();
    let text = sb.read(CONFIG);
    assert!(text.contains("show_email = true"), "{text}");
    assert!(text.ends_with("# my note\n"), "{text}");
    no_edit_copies(&sb);
}

#[test]
fn an_invalid_edit_leaves_the_file_unchanged() {
    let sb = sandbox();
    let before = sb.read(CONFIG);
    for (script, msg) in [
        ("printf '[usage\\n' >> \"$1\"\n", "not valid mcc config"),
        (
            "printf '[sync]\\nexclude = [\"nope\"]\\n' > \"$1\"\n",
            "unknown sync item",
        ),
        (
            "printf '[profiles.w]\\ndir = \"/d\"\\nmode = \"clone\"\\ncreated_at = \"t\"\\nshare_group = \"../up\"\\n' > \"$1\"\n",
            "invalid group name",
        ),
        ("exit 3\n", "exited with"),
    ] {
        let ed = editor(&sb, script);
        sb.mcc()
            .args(["config", "edit"])
            .env("EDITOR", ed)
            .assert()
            .failure()
            .stderr(contains(msg));
        assert_eq!(sb.read(CONFIG), before, "changed by {script:?}");
        no_edit_copies(&sb);
    }
}

#[test]
fn edit_can_fix_a_broken_file() {
    let sb = sandbox();
    let good = sb.read(CONFIG);
    sb.write(CONFIG, &format!("{good}\n[oops\n"));
    sb.mcc()
        .args(["config", "get", "usage.live"])
        .assert()
        .failure()
        .stderr(contains("mcc config edit"));
    sb.write("good.toml", &good);
    let ed = editor(
        &sb,
        &format!("cp '{}' \"$1\"\n", sb.path("good.toml").display()),
    );
    sb.mcc()
        .args(["config", "edit"])
        .env("EDITOR", ed)
        .assert()
        .success();
    assert_eq!(sb.read(CONFIG), good);
}

#[test]
fn edit_keeps_changes_made_meanwhile() {
    let sb = sandbox();
    // Another command writes the config while the editor is open.
    let ed = editor(
        &sb,
        &format!(
            "printf '# theirs\\n' >> '{}'\nprintf '# mine\\n' >> \"$1\"\n",
            sb.path(CONFIG).display()
        ),
    );
    sb.mcc()
        .args(["config", "edit"])
        .env("EDITOR", ed)
        .assert()
        .failure()
        .stderr(contains("changed while you were editing"));
    let text = sb.read(CONFIG);
    assert!(
        text.ends_with("# theirs\n") && !text.contains("# mine"),
        "{text}"
    );
    // The edited copy is kept for the user.
    let kept: Vec<String> = std::fs::read_dir(sb.path(".mcc"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("config.edit-"))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(sb.read(&format!(".mcc/{}", kept[0])).ends_with("# mine\n"));
}

#[test]
fn get_and_path_do_not_wait_for_the_lock() {
    let sb = sandbox();
    let f = File::create(sb.path(".mcc/.lock")).unwrap();
    // SAFETY: the descriptor stays open for the duration of the call.
    assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
    for args in [
        ["config", "get", "usage.live"].as_slice(),
        &["config", "path"],
    ] {
        sb.mcc()
            .args(args)
            .timeout(Duration::from_secs(20))
            .assert()
            .success();
    }
    // The editor runs without the lock as well; nothing changed, nothing saved.
    sb.mcc()
        .args(["config", "edit"])
        .env("EDITOR", "true")
        .timeout(Duration::from_secs(20))
        .assert()
        .success()
        .stderr(contains("no changes"));
    // `set` waits for it.
    sb.mcc()
        .args(["config", "set", "usage.live", "true"])
        .timeout(Duration::from_secs(2))
        .assert()
        .failure();
    drop(f);
    assert!(!sb.read(CONFIG).contains("live = true"));
}
