//! `mcc link`: opt-in link mode for read-mostly items, and how `add --clone`,
//! `sync`, `diff` and `list` treat linked items.

mod common;

use std::path::Path;

use common::{Sandbox, by_name, json, stdout_of, with_profile};
use predicates::prelude::*;

const WORK: &str = ".mcc/profiles/work";

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

fn manifest(sb: &Sandbox) -> serde_json::Value {
    json(sb.read(&format!("{WORK}/.mcc-sync.json")).as_bytes())
}

fn manifest_has(sb: &Sandbox, prefix: &str) -> bool {
    manifest(sb)["files"]
        .as_object()
        .unwrap()
        .keys()
        .any(|k| k.starts_with(prefix))
}

/// `link` of `[profiles.work]` in config.toml.
fn links(sb: &Sandbox) -> Vec<String> {
    let cfg: toml::Value = toml::from_str(&sb.read(".mcc/config.toml")).unwrap();
    cfg["profiles"]["work"]
        .get("link")
        .and_then(|l| l.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn sync_status(sb: &Sandbox) -> serde_json::Value {
    let out = stdout_of(sb.mcc().args(["list", "--json", "--no-usage"]));
    let all: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    by_name(&all, "work")["sync"].clone()
}

#[test]
fn link_backs_up_the_copy_and_warns_about_write_through() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    // A local edit the profile's copy holds.
    sb.write(&format!("{WORK}/skills/alpha/SKILL.md"), "edited in work");
    assert!(manifest_has(&sb, "skills/"));

    sb.mcc()
        .args(["link", "work", "skills"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Back up the profile's copies"))
        .stderr(predicate::str::contains("write-through"))
        .stderr(predicate::str::contains("changes the default's real files"));

    let skills = sb.path(&format!("{WORK}/skills"));
    assert!(is_link(&skills));
    assert_eq!(
        std::fs::read_link(&skills).unwrap(),
        sb.path(".claude/skills")
    );
    // The default is untouched; the profile's edit is in the backup.
    assert_eq!(sb.read(".claude/skills/alpha/SKILL.md"), "alpha");
    let backups: Vec<_> = std::fs::read_dir(sb.path(&format!("{WORK}/.mcc-backup")))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(backups[0].join("skills/alpha/SKILL.md")).unwrap(),
        "edited in work"
    );
    // Nothing is left aside, and the manifest forgot the old copy.
    let leftovers: Vec<_> = std::fs::read_dir(sb.path(WORK))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("mcc-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(!manifest_has(&sb, "skills/"));
    // A clone shares plugins from the start (#47).
    assert_eq!(links(&sb), ["plugins", "skills"]);

    // Linking again is a no-op.
    sb.mcc()
        .args(["link", "work", "skills"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already links skills"));

    sb.mcc()
        .args(["link", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("linked →"))
        .stderr(predicate::str::contains("copied (synced by `mcc sync`)"));
}

#[test]
fn sync_diff_and_list_leave_linked_items_alone() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.mcc()
        .args(["link", "work", "skills,claude-md"])
        .assert()
        .success();

    // Changes on both sides of the link are the same files: no local edits,
    // no conflicts, nothing behind.
    sb.write(".claude/skills/gamma/SKILL.md", "new in default");
    sb.write(
        &format!("{WORK}/skills/delta/SKILL.md"),
        "written through work",
    );
    sb.write(&format!("{WORK}/CLAUDE.md"), "# edited through work\n");
    let status = sync_status(&sb);
    assert_eq!(status["behind"], 0, "{status}");
    assert_eq!(status["local_edits"], 0, "{status}");
    assert_eq!(status["conflicts"], 0, "{status}");

    sb.mcc()
        .args(["diff", "work", "--only", "skills,claude-md"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not compared"))
        .stdout(predicate::str::contains("skills/").not());

    sb.mcc()
        .args(["sync", "work", "--prune", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("sync leaves them alone"));
    assert!(is_link(&sb.path(&format!("{WORK}/skills"))));
    assert!(is_link(&sb.path(&format!("{WORK}/CLAUDE.md"))));
    // --prune never followed the link into the default.
    assert_eq!(
        sb.read(".claude/skills/delta/SKILL.md"),
        "written through work"
    );
    assert_eq!(sb.read(".claude/skills/gamma/SKILL.md"), "new in default");
    assert!(!manifest_has(&sb, "skills/"));
    assert!(!manifest_has(&sb, "CLAUDE.md"));

    // A linked item replaced by hand is warned about, and still left alone.
    std::fs::remove_file(sb.path(&format!("{WORK}/CLAUDE.md"))).unwrap();
    sb.write(&format!("{WORK}/CLAUDE.md"), "# my own\n");
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("is not a link to"));
    assert_eq!(sb.read(&format!("{WORK}/CLAUDE.md")), "# my own\n");
}

#[test]
fn off_replaces_the_link_with_a_copy_and_never_touches_the_default() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.mcc()
        .args(["link", "work", "skills", "agents"])
        .assert()
        .success();
    sb.write(
        &format!("{WORK}/skills/delta/SKILL.md"),
        "written through work",
    );

    sb.mcc()
        .args(["link", "work", "skills", "--off"])
        .assert()
        .success()
        .stderr(predicate::str::contains("has its own copy of skills"));

    let skills = sb.path(&format!("{WORK}/skills"));
    assert!(!is_link(&skills) && skills.is_dir());
    assert_eq!(sb.read(&format!("{WORK}/skills/alpha/SKILL.md")), "alpha");
    assert_eq!(
        sb.read(&format!("{WORK}/skills/delta/SKILL.md")),
        "written through work"
    );
    // The default keeps every file, including the one written through the link.
    for (rel, text) in [
        ("alpha", "alpha"),
        ("beta", "beta"),
        ("delta", "written through work"),
    ] {
        assert_eq!(sb.read(&format!(".claude/skills/{rel}/SKILL.md")), text);
    }
    // agents stays linked; skills is recorded again for three-way sync.
    assert!(is_link(&sb.path(&format!("{WORK}/agents"))));
    assert!(manifest_has(&sb, "skills/"));
    assert_eq!(links(&sb), ["plugins", "agents"]);

    // The copy is in sync: an edit in the profile is now a local edit.
    sb.mcc()
        .args(["sync", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already up to date"));
    sb.write(&format!("{WORK}/skills/alpha/SKILL.md"), "edited");
    assert_eq!(sync_status(&sb)["local_edits"], 1);
    assert_eq!(sb.read(".claude/skills/alpha/SKILL.md"), "alpha");

    // Off for an item that is not linked does nothing.
    sb.mcc()
        .args(["link", "work", "commands", "--off"])
        .assert()
        .success()
        .stderr(predicate::str::contains("links none of commands"));
}

#[test]
fn clone_honors_link_mode() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--clone", "--link", "skills,claude-md"])
        .assert()
        .success()
        .stderr(predicate::str::contains("write-through"))
        .stderr(predicate::str::contains("linked: skills, claude-md"));
    assert!(is_link(&sb.path(&format!("{WORK}/skills"))));
    assert!(is_link(&sb.path(&format!("{WORK}/CLAUDE.md"))));
    assert!(!is_link(&sb.path(&format!("{WORK}/agents"))));
    assert!(sb.exists(&format!("{WORK}/agents/reviewer.md")));
    assert!(!manifest_has(&sb, "skills/"));
    assert!(manifest_has(&sb, "agents/"));
    assert_eq!(links(&sb), ["skills", "claude-md", "plugins"]);
    let status = sync_status(&sb);
    assert_eq!(status["behind"], 0, "{status}");

    // --link needs --clone, and is checked before anything is created.
    sb.mcc()
        .args(["add", "lab", "--fresh", "--link", "skills"])
        .assert()
        .code(2);
    sb.mcc()
        .args(["add", "lab", "--clone", "--link", "settings", "--no-login"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("settings can't be linked"));
    assert!(!sb.exists(".mcc/profiles/lab"));
}

#[test]
fn link_refusals() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    for (args, needle) in [
        (vec!["link", "work", "settings"], "settings can't be linked"),
        (vec!["link", "work", "hooks"], "can't be linked"),
        (vec!["link", "work", "nope"], "unknown item"),
        (vec!["link", "default", "skills"], "pick another profile"),
        (vec!["link", "work", "--off"], "name the items"),
        (vec!["link", "ghost", "skills"], "ghost"),
    ] {
        sb.mcc()
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(needle));
    }
    // The default has no output-styles to link to.
    sb.mcc()
        .args(["link", "work", "output-styles"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no "));
    assert!(!sb.exists(&format!("{WORK}/output-styles")));
    sb.mcc()
        .args(["config", "set", "profiles.work.link", "skills"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("mcc link work"));
    assert!(!is_link(&sb.path(&format!("{WORK}/skills"))));
}

#[test]
fn failed_link_rolls_back() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.write(&format!("{WORK}/agents/mine.md"), "only in work");
    let before = manifest(&sb);
    // ~/.mcc read-only: the links are made, then saving config.toml fails.
    let mcc_dir = sb.path(".mcc");
    std::fs::set_permissions(&mcc_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let out = sb
        .mcc()
        .args(["link", "work", "agents,skills"])
        .output()
        .unwrap();
    std::fs::set_permissions(&mcc_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    // It failed saving config.toml, after linking.
    assert!(stderr.contains("rolled back"), "{stderr}");
    for item in ["agents", "skills"] {
        let p = sb.path(&format!("{WORK}/{item}"));
        assert!(!is_link(&p) && p.is_dir(), "{item} restored");
    }
    assert_eq!(sb.read(&format!("{WORK}/agents/mine.md")), "only in work");
    assert_eq!(sb.read(".claude/agents/reviewer.md"), "agent");
    assert!(!sb.exists(".claude/agents/mine.md"));
    assert_eq!(manifest(&sb), before);
    assert_eq!(links(&sb), ["plugins"]);
}

#[test]
fn link_refuses_a_profile_whose_dir_is_the_default() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    // Hand-edit the profile's dir to the default's (directly, then via a
    // symlink, then a parent of it).
    let cfg = sb.read(".mcc/config.toml");
    let work = sb.path(WORK).display().to_string();
    assert!(cfg.contains(&work));
    std::os::unix::fs::symlink(sb.path(".claude"), sb.path("alias-of-claude")).unwrap();
    for dir in [
        sb.path(".claude"),
        sb.path("alias-of-claude"),
        sb.home().to_path_buf(),
    ] {
        sb.write(
            ".mcc/config.toml",
            &cfg.replace(&work, &dir.display().to_string()),
        );
        for args in [
            &["link", "work", "agents"][..],
            &["link", "work", "agents", "--off"],
        ] {
            sb.mcc()
                .args(args)
                .assert()
                .failure()
                .stderr(predicate::str::contains("overlaps the default profile"));
        }
        let agents = sb.path(".claude/agents");
        assert!(!is_link(&agents));
        assert_eq!(sb.read(".claude/agents/reviewer.md"), "agent");
    }
}

/// `list --json`'s installed-plugins count for `work`.
fn plugins_installed(sb: &Sandbox) -> serde_json::Value {
    let out = stdout_of(sb.mcc().args(["list", "--json", "--no-usage"]));
    let all: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    by_name(&all, "work")["plugins_installed"].clone()
}

/// Issue #47: an existing profile with its own copy of the plugins opts in to
/// the default's. Its copy is moved to a backup, never deleted.
#[test]
fn link_plugins_moves_the_copy_to_a_backup_and_shares_the_defaults() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--clone", "--copy-plugins"])
        .assert()
        .success();
    sb.write(
        &format!("{WORK}/plugins/data/p1/state"),
        "work's plugin data",
    );
    assert!(manifest_has(&sb, "plugins/"));
    assert!(links(&sb).is_empty());
    let default_before = sb.read(".claude/plugins/installed_plugins.json");

    sb.mcc()
        .args(["link", "work", "plugins"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Move the profile's plugins to a backup",
        ))
        .stderr(predicate::str::contains("shared plugins: \"work\""))
        .stderr(predicate::str::contains(
            "remove that folder to free the space",
        ));

    // The profile's own copy is in the backup, whole.
    assert!(!sb.exists(&format!("{WORK}/plugins")));
    let backups: Vec<_> = std::fs::read_dir(sb.path(&format!("{WORK}/.mcc-backup")))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(
        backups[0]
            .join("plugins/cache/m/p1/1.0.0/plugin.json")
            .is_file()
    );
    assert_eq!(
        std::fs::read_to_string(backups[0].join("plugins/data/p1/state")).unwrap(),
        "work's plugin data"
    );
    // The default is untouched; the launcher points at it.
    assert_eq!(
        sb.read(".claude/plugins/installed_plugins.json"),
        default_before
    );
    assert!(
        sb.read(".local/bin/work")
            .contains("export CLAUDE_CODE_PLUGIN_CACHE_DIR=\"$HOME/.claude/plugins\"\n")
    );
    assert_eq!(links(&sb), ["plugins"]);
    assert!(!manifest_has(&sb, "plugins/"));
    assert_eq!(plugins_installed(&sb), 2);

    // Sync and diff leave plugins alone, and never copy them back.
    sb.mcc()
        .args(["sync", "work", "--prune", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "linked to the default profile: plugins",
        ));
    assert!(!sb.exists(&format!("{WORK}/plugins")));
    assert_eq!(sync_status(&sb)["behind"], 0);

    sb.mcc()
        .args(["link", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("shared →"));

    // Again: nothing to move, nothing changes.
    sb.mcc()
        .args(["link", "work", "plugins"])
        .assert()
        .success()
        .stderr(predicate::str::contains("the profile has no plugins/"));
    assert_eq!(links(&sb), ["plugins"]);
}

#[test]
fn link_plugins_off_copies_them_back_and_drops_the_variable() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    assert_eq!(links(&sb), ["plugins"]);
    assert!(!sb.exists(&format!("{WORK}/plugins")));

    sb.mcc()
        .args(["link", "work", "plugins", "--off"])
        .assert()
        .success()
        .stderr(predicate::str::contains("has its own copy of the plugins"));
    assert!(sb.exists(&format!("{WORK}/plugins/cache/m/p1/1.0.0/plugin.json")));
    let home = sb.home().display().to_string();
    assert!(
        sb.read(&format!("{WORK}/plugins/installed_plugins.json"))
            .contains(&format!("{home}/.mcc/profiles/work/plugins/cache"))
    );
    assert!(
        !sb.read(".local/bin/work")
            .contains("export CLAUDE_CODE_PLUGIN_CACHE_DIR")
    );
    assert!(links(&sb).is_empty());
    assert!(manifest_has(&sb, "plugins/"));
    assert_eq!(sync_status(&sb)["behind"], 0);

    // Off again does nothing.
    sb.mcc()
        .args(["link", "work", "plugins", "--off"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already has its own plugins"));
}

#[test]
fn failed_link_plugins_rolls_back() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .args(["add", "work", "--clone", "--copy-plugins"])
        .assert()
        .success();
    let launcher = sb.read(".local/bin/work");
    let before = manifest(&sb);
    // ~/.mcc read-only: the plugins are moved, then saving config.toml fails.
    let mcc_dir = sb.path(".mcc");
    std::fs::set_permissions(&mcc_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let out = sb.mcc().args(["link", "work", "plugins"]).output().unwrap();
    std::fs::set_permissions(&mcc_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(sb.exists(&format!("{WORK}/plugins/cache/m/p1/1.0.0/plugin.json")));
    assert!(
        !sb.exists(&format!("{WORK}/.mcc-backup")) || {
            std::fs::read_dir(sb.path(&format!("{WORK}/.mcc-backup")))
                .unwrap()
                .count()
                == 0
        }
    );
    assert_eq!(sb.read(".local/bin/work"), launcher);
    assert_eq!(manifest(&sb), before);
    assert!(links(&sb).is_empty());
}
