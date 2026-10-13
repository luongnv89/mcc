//! `mcc rename`: a new command for a profile, with the same directory and login.

mod common;

use common::{Sandbox, json, with_profile};
use predicates::prelude::*;

fn names(sb: &Sandbox) -> Vec<String> {
    let out = sb
        .mcc()
        .args(["list", "--json", "--no-usage"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    json(&out)
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn rename_changes_the_command_and_keeps_the_profile() {
    let sb = Sandbox::new();
    sb.seed_default();
    let rc_before = sb.read(".zshrc");
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "keep", "--fresh");
    assert!(sb.exists(".mcc/profiles/work/.credentials.json"));

    sb.mcc()
        .args(["rename", "work", "job"])
        .assert()
        .success()
        .stderr(predicate::str::contains("\"job\" is available"))
        .stderr(predicate::str::contains(
            "profile \"work\" is now \"job\" — start it with `job`",
        ))
        .stderr(predicate::str::contains("~/.mcc/profiles/work (unchanged)"))
        .stderr(predicate::str::contains("unalias work"));

    // New launcher and alias, pointing at the same directory; old ones gone.
    let launcher = sb.read(".local/bin/job");
    assert!(launcher.contains("# managed-by: mcc"), "{launcher}");
    assert!(launcher.contains("mcc delete job"), "{launcher}");
    assert!(
        launcher.contains("CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\""),
        "{launcher}"
    );
    assert!(!sb.exists(".local/bin/work"));
    let rc = sb.read(".zshrc");
    assert!(!rc.contains("mcc:work"), "{rc}");
    assert!(
        rc.contains("alias job='CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" claude'"),
        "{rc}"
    );
    assert!(
        rc.contains("mcc:keep") && rc.contains("alias cus_luong="),
        "{rc}"
    );
    // Same directory, login and setup; only the registry key changed.
    assert!(sb.exists(".mcc/profiles/work/.credentials.json"));
    assert!(sb.exists(".mcc/profiles/work/skills/alpha/SKILL.md"));
    assert!(!sb.exists(".mcc/profiles/job"));
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("[profiles.job]") && !cfg.contains("[profiles.work]"));
    assert_eq!(names(&sb), ["default", "job", "keep"]);
    sb.mcc().args(["sync", "job", "--yes"]).assert().success();

    // The old name no longer refers to anything, and never to job's directory.
    sb.mcc()
        .args(["delete", "work", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("belongs to profile \"job\""));
    assert!(sb.exists(".mcc/profiles/work/.credentials.json"));
    sb.mcc().args(["usage", "work"]).assert().code(2);
    sb.mcc()
        .args(["add", "work", "--fresh", "--no-login"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "~/.mcc/profiles/work is used by profile \"job\"",
        ));

    // The profile can take its original name back.
    sb.mcc().args(["mv", "job", "work"]).assert().success();
    assert!(sb.exists(".local/bin/work") && !sb.exists(".local/bin/job"));
    assert_eq!(names(&sb), ["default", "keep", "work"]);

    for alias in ["work", "keep"] {
        sb.mcc().args(["delete", alias, "--yes"]).assert().success();
    }
    assert_eq!(
        sb.read(".zshrc"),
        rc_before,
        "rc file restored byte-for-byte"
    );
    assert!(!sb.exists(".mcc/profiles/work"));
}

#[test]
fn rename_rejects_names_that_are_taken() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    with_profile(&sb, "lab", "--fresh");
    sb.write(".local/bin/mine", "#!/bin/sh\necho mine\n");
    common::chmod_x(&sb.path(".local/bin/mine"));
    let cfg_before = sb.read(".mcc/config.toml");
    let rc_before = sb.read(".zshrc");

    for (new, why) in [
        ("lab", "an mcc profile named \"lab\" already exists"),
        ("mcc", "is reserved by mcc"),
        ("claude", "is reserved by mcc"),
        ("default", "is reserved by mcc"),
        ("cd", "shell keyword or builtin"),
        ("cus_luong", "alias \"cus_luong\" is defined in ~/.zshrc"),
        ("mine", "~/.local/bin/mine already exists"),
        ("sh", "a command named \"sh\" exists at /bin/sh"),
    ] {
        sb.mcc()
            .args(["rename", "work", new])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(why))
            .stderr(predicate::str::contains("choose another name"));
    }
    for (old, new, why) in [
        ("work", "Work2", "is not a valid alias"),
        ("work", "work", "is already called"),
        ("ghost", "x", "no profile named \"ghost\""),
        ("default", "x", "can't be renamed"),
        ("claude", "x", "can't be renamed"),
    ] {
        sb.mcc()
            .args(["rename", old, new])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(why));
    }
    sb.mcc().args(["rename", "work"]).assert().code(2);

    // Nothing changed.
    assert_eq!(sb.read(".mcc/config.toml"), cfg_before);
    assert_eq!(sb.read(".zshrc"), rc_before);
    assert!(sb.exists(".local/bin/work"));
}

#[cfg(unix)]
#[test]
fn rename_rolls_back_when_the_registry_cannot_be_saved() {
    use std::os::unix::fs::PermissionsExt;

    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    let rc_before = sb.read(".zshrc");
    let cfg_before = sb.read(".mcc/config.toml");
    let mcc_dir = sb.path(".mcc");
    let set_mode =
        |mode| std::fs::set_permissions(&mcc_dir, std::fs::Permissions::from_mode(mode)).unwrap();
    set_mode(0o555);
    // Root ignores the mode (some CI containers): nothing to test there.
    if std::fs::write(mcc_dir.join("probe"), "").is_ok() {
        set_mode(0o755);
        return;
    }
    let assert = sb.mcc().args(["rename", "work", "job"]).assert();
    set_mode(0o755);

    assert
        .failure()
        .stderr(predicate::str::contains("Rolling back"));
    assert!(!sb.exists(".local/bin/job"));
    assert!(sb.exists(".local/bin/work"));
    assert_eq!(sb.read(".zshrc"), rc_before);
    assert_eq!(sb.read(".mcc/config.toml"), cfg_before);
}
