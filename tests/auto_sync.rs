//! Auto sync (issue #99): when a profile starts, from its launcher or
//! `mcc run`, mcc catches it (and then the other profiles) up with the
//! default's setup — the safe part of `mcc sync` only, never breaking the
//! launch, never writing on its stdout. `[sync] auto` and
//! `profiles.ALIAS.auto_sync` turn it off.

mod common;

use common::{Sandbox, json, stdout_of, with_profile};
use predicates::prelude::*;

const CONFIG: &str = ".mcc/config.toml";

/// default + `work` and `lab`, both cloned from it.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--clone");
    sb
}

/// Start `alias` from its launcher as `alias -p hi`, piped: it must start
/// claude with a clean stdout and say nothing on stderr (not a terminal).
fn launch(sb: &Sandbox, alias: &str) {
    let o = sb.launcher(alias).args(["-p", "hi"]).output().unwrap();
    assert!(o.status.success(), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "fake claude -p hi\n");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
}

fn set(sb: &Sandbox, key: &str, value: &str) {
    sb.mcc()
        .args(["config", "set", key, value])
        .assert()
        .success();
}

fn backups(sb: &Sandbox, alias: &str) -> usize {
    std::fs::read_dir(sb.path(&format!(".mcc/profiles/{alias}/.mcc-backup")))
        .map(|d| d.count())
        .unwrap_or(0)
}

fn state(sb: &Sandbox, alias: &str) -> serde_json::Value {
    json(
        sb.read(&format!(".mcc/state/{alias}/auto-sync.json"))
            .as_bytes(),
    )
}

fn list_json(sb: &Sandbox) -> Vec<serde_json::Value> {
    let v = json(stdout_of(sb.mcc().args(["list", "--json", "--no-usage"])).as_bytes());
    v.as_array().unwrap().clone()
}

/// The SYNC column of `mcc list` for `alias`.
fn sync_cell(sb: &Sandbox, alias: &str) -> String {
    let out = stdout_of(sb.mcc().args(["list", "--no-usage"]));
    // The columns before SYNC hold no "behind", "✓" or "(held)".
    out.lines()
        .find(|l| l.starts_with(&format!("{alias} ")))
        .unwrap_or_else(|| panic!("no row for {alias}: {out}"))
        .to_string()
}

/// A change in the default: a new skill and an edited CLAUDE.md.
fn change_default(sb: &Sandbox) {
    sb.write(".claude/skills/gamma/SKILL.md", "gamma");
    sb.write(".claude/CLAUDE.md", "# my rules v2\n");
}

fn caught_up(sb: &Sandbox, alias: &str) -> bool {
    let dir = format!(".mcc/profiles/{alias}");
    sb.exists(&format!("{dir}/skills/gamma/SKILL.md"))
        && sb.read(&format!("{dir}/CLAUDE.md")) == "# my rules v2\n"
}

#[test]
fn a_launch_catches_the_profile_up_with_the_default() {
    let sb = sandbox();
    // An old last sync, to see it move.
    let cfg = sb.read(CONFIG);
    let old = cfg
        .lines()
        .find(|l| l.starts_with("last_sync = "))
        .unwrap()
        .to_string();
    sb.write(
        CONFIG,
        &cfg.replace(&old, "last_sync = \"2026-01-01T00:00:00+00:00\""),
    );
    change_default(&sb);
    assert!(sync_cell(&sb, "work").contains("2 behind"));

    launch(&sb, "work");
    assert!(caught_up(&sb, "work"));
    // Backed up first, as `mcc sync` does, and recorded in the manifest and
    // the config, so `mcc list` shows it current.
    assert_eq!(backups(&sb, "work"), 1);
    let backup = std::fs::read_dir(sb.path(".mcc/profiles/work/.mcc-backup"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read_to_string(backup.join("CLAUDE.md")).unwrap(),
        "# my rules\n"
    );
    let m = json(sb.read(".mcc/profiles/work/.mcc-sync.json").as_bytes());
    assert!(m["files"]["skills/gamma/SKILL.md"].is_string(), "{m}");
    let work = list_json(&sb)
        .into_iter()
        .find(|p| p["name"] == "work")
        .unwrap();
    assert_ne!(work["last_sync"], "2026-01-01T00:00:00+00:00", "{work}");
    assert_eq!(work["sync"]["behind"], 0, "{work}");
    assert!(
        sync_cell(&sb, "work").ends_with('✓'),
        "{}",
        sync_cell(&sb, "work")
    );
    // The log says what happened.
    assert!(
        sb.read(".mcc/logs/mcc.log")
            .contains("auto sync: work: copied 2 from the default"),
        "{}",
        sb.read(".mcc/logs/mcc.log")
    );
}

#[test]
fn mcc_run_catches_up_too_with_a_clean_stdout() {
    let sb = sandbox();
    change_default(&sb);
    let o = sb
        .mcc()
        .args(["run", "--profile", "work", "--", "-p", "hi"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "fake claude -p hi\n");
    assert!(
        !String::from_utf8_lossy(&o.stderr).contains("auto sync"),
        "{o:?}"
    );
    assert!(caught_up(&sb, "work"));
    // A dry run starts nothing, and changes nothing.
    sb.write(".claude/agents/new.md", "new");
    sb.mcc()
        .args(["run", "--profile", "work", "--dry-run"])
        .assert()
        .success()
        .stdout("work\n");
    assert!(!sb.exists(".mcc/profiles/work/agents/new.md"));
}

#[test]
fn local_edits_and_conflicts_are_kept_and_still_reported() {
    let sb = sandbox();
    // A local edit, and a path changed on both sides.
    sb.write(".mcc/profiles/work/CLAUDE.md", "# mine\n");
    sb.write(".mcc/profiles/work/agents/reviewer.md", "my agent");
    sb.write(".claude/agents/reviewer.md", "agent v2");
    sb.write(".claude/skills/gamma/SKILL.md", "gamma");

    launch(&sb, "work");
    assert!(sb.exists(".mcc/profiles/work/skills/gamma/SKILL.md"));
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# mine\n");
    assert_eq!(sb.read(".mcc/profiles/work/agents/reviewer.md"), "my agent");
    let cell = sync_cell(&sb, "work");
    assert!(cell.contains("1 local edit, 1 conflict"), "{cell}");
    assert!(!cell.contains("behind"), "{cell}");
    let s = state(&sb, "work");
    assert_eq!(s["conflicts"], 1, "{s}");
    assert_eq!(s["local_edits"], 1, "{s}");
    // `mcc sync` still resolves the conflict the usual way.
    sb.mcc()
        .args(["sync", "work", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 conflict"));
}

#[test]
fn a_catch_up_that_would_remove_something_writes_nothing() {
    let sb = sandbox();
    // The profile's commands/ is a symlink of the user's: a sync would
    // replace it (a removal, which only `mcc sync` asks about).
    sb.write("mine/commands/own.md", "own");
    std::fs::remove_dir_all(sb.path(".mcc/profiles/work/commands")).unwrap();
    std::os::unix::fs::symlink(
        sb.path("mine/commands"),
        sb.path(".mcc/profiles/work/commands"),
    )
    .unwrap();
    change_default(&sb);
    let manifest = sb.read(".mcc/profiles/work/.mcc-sync.json");

    launch(&sb, "work");
    assert!(
        std::fs::symlink_metadata(sb.path(".mcc/profiles/work/commands"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(sb.read("mine/commands/own.md"), "own");
    assert!(!sb.exists(".mcc/profiles/work/skills/gamma"));
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my rules\n");
    assert_eq!(sb.read(".mcc/profiles/work/.mcc-sync.json"), manifest);
    assert_eq!(backups(&sb, "work"), 0);
    assert_eq!(state(&sb, "work")["held"], "removals");
    let cell = sync_cell(&sb, "work");
    assert!(
        cell.contains("behind") && cell.ends_with("(held)"),
        "{cell}"
    );
    // lab, without the symlink, is caught up as usual.
    assert!(caught_up(&sb, "lab"));
}

#[test]
fn a_symlink_where_the_default_has_a_nested_folder_is_never_replaced() {
    let sb = sandbox();
    // The user's own skills/alpha, symlinked into the profile: the default's
    // skills/alpha folder would replace it (a removal).
    sb.write("mine/alpha/SKILL.md", "my alpha");
    std::fs::remove_dir_all(sb.path(".mcc/profiles/work/skills/alpha")).unwrap();
    std::os::unix::fs::symlink(
        sb.path("mine/alpha"),
        sb.path(".mcc/profiles/work/skills/alpha"),
    )
    .unwrap();
    change_default(&sb);
    let manifest = sb.read(".mcc/profiles/work/.mcc-sync.json");

    launch(&sb, "work");
    assert!(
        std::fs::symlink_metadata(sb.path(".mcc/profiles/work/skills/alpha"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(sb.read("mine/alpha/SKILL.md"), "my alpha");
    assert!(!sb.exists(".mcc/profiles/work/skills/gamma"));
    assert_eq!(sb.read(".mcc/profiles/work/CLAUDE.md"), "# my rules\n");
    assert_eq!(sb.read(".mcc/profiles/work/.mcc-sync.json"), manifest);
    assert_eq!(backups(&sb, "work"), 0);
    assert_eq!(state(&sb, "work")["held"], "removals");
    assert!(caught_up(&sb, "lab"));
}

#[test]
fn files_the_sync_record_does_not_cover_are_never_overwritten() {
    let sb = sandbox();
    // The profile has its own notes.md; the default has another. Adding it
    // to the sync set leaves it out of work's sync record, so a two-way
    // compare would take the default's over the profile's.
    sb.write(".claude/notes.md", "default notes");
    sb.write(".mcc/profiles/work/notes.md", "my notes");
    set(&sb, "sync.extra", "notes.md");
    let manifest = sb.read(".mcc/profiles/work/.mcc-sync.json");

    launch(&sb, "work");
    assert_eq!(sb.read(".mcc/profiles/work/notes.md"), "my notes");
    assert_eq!(sb.read(".mcc/profiles/work/.mcc-sync.json"), manifest);
    assert_eq!(backups(&sb, "work"), 0);
    let s = state(&sb, "work");
    assert_eq!(s["held"], "untracked", "{s}");
    assert_eq!(s["changed"], serde_json::json!([]), "{s}");
    let work = list_json(&sb)
        .into_iter()
        .find(|p| p["name"] == "work")
        .unwrap();
    assert_eq!(work["auto_sync"]["held"], "untracked", "{work}");
    sb.mcc()
        .args(["list", "--long", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Held:            nothing copied: it would overwrite files its sync record doesn't cover — run `mcc sync work`",
        ));
    // lab, which lacks notes.md, simply gets it.
    assert_eq!(sb.read(".mcc/profiles/lab/notes.md"), "default notes");

    // The same for an MCP server: one the profile has its own way while
    // its sync record doesn't cover MCP servers is kept.
    let mut m = json(manifest.as_bytes());
    m["items"]
        .as_array_mut()
        .unwrap()
        .retain(|i| i != ":mcp" && i != "notes.md");
    sb.write(".mcc/profiles/work/.mcc-sync.json", &m.to_string());
    sb.write(".mcc/profiles/work/notes.md", "default notes");
    let mine = r#"{"mcpServers":{"fs":{"command":"mine"}}}"#;
    sb.write(".mcc/profiles/work/.claude.json", mine);
    launch(&sb, "work");
    assert_eq!(sb.read(".mcc/profiles/work/.claude.json"), mine);
    assert_eq!(state(&sb, "work")["held"], "untracked");
}

#[test]
fn profiles_without_a_usable_sync_record_are_held() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "fresh", "--fresh");
    // No manifest (a --fresh profile never synced): nothing is copied.
    launch(&sb, "fresh");
    assert!(!sb.exists(".mcc/profiles/fresh/skills"));
    assert!(!sb.exists(".mcc/profiles/fresh/.mcc-sync.json"));
    assert_eq!(backups(&sb, "fresh"), 0);
    assert_eq!(state(&sb, "fresh")["held"], "no_manifest");
    let cell = sync_cell(&sb, "fresh");
    assert!(cell.ends_with("(held)"), "{cell}");
    sb.mcc()
        .args(["list", "--long", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Held:            nothing copied: it has no sync record yet — run `mcc sync fresh`",
        ));

    // A manifest that can't be read: the same.
    sb.write(".mcc/profiles/work/.mcc-sync.json", "not json");
    change_default(&sb);
    launch(&sb, "work");
    assert!(!sb.exists(".mcc/profiles/work/skills/gamma"));
    assert_eq!(sb.read(".mcc/profiles/work/.mcc-sync.json"), "not json");
    assert_eq!(state(&sb, "work")["held"], "invalid_manifest");
}

#[test]
fn launching_one_profile_catches_up_the_others() {
    let sb = sandbox();
    change_default(&sb);
    launch(&sb, "work");
    assert!(caught_up(&sb, "work") && caught_up(&sb, "lab"));
    // `mcc list` shows nobody behind.
    for p in list_json(&sb).iter().filter(|p| p["name"] != "default") {
        assert_eq!(p["sync"]["behind"], 0, "{p}");
    }
    sb.mcc()
        .args(["list", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::contains("behind").not());
}

#[test]
fn an_unchanged_default_is_not_compared_again() {
    let sb = sandbox();
    change_default(&sb);
    launch(&sb, "work");
    let (work, lab) = (
        sb.read(".mcc/state/work/auto-sync.json"),
        sb.read(".mcc/state/lab/auto-sync.json"),
    );
    // Nothing changed: no comparison, so no new check is recorded.
    launch(&sb, "work");
    launch(&sb, "lab");
    assert_eq!(sb.read(".mcc/state/work/auto-sync.json"), work);
    assert_eq!(sb.read(".mcc/state/lab/auto-sync.json"), lab);
    // Claude Code rewriting the rest of ~/.claude.json changes nothing.
    let j = sb.read(".claude.json");
    sb.write(".claude.json", &j.replace("dark-ansi", "light"));
    launch(&sb, "work");
    assert_eq!(sb.read(".mcc/state/work/auto-sync.json"), work);
    // Its MCP servers do.
    sb.write(".claude.json", &j.replace("\"fs\"", "\"fs2\""));
    launch(&sb, "work");
    assert_ne!(sb.read(".mcc/state/work/auto-sync.json"), work);
    let claude_json = json(sb.read(".mcc/profiles/work/.claude.json").as_bytes());
    assert!(
        claude_json["mcpServers"]["fs2"].is_object(),
        "{claude_json}"
    );
}

#[test]
fn auto_sync_can_be_turned_off_for_one_profile_or_all() {
    let sb = sandbox();
    sb.mcc()
        .args(["config", "get", "sync.auto"])
        .assert()
        .success()
        .stdout("true\n");
    sb.mcc()
        .args(["config", "get", "profiles.lab.auto_sync"])
        .assert()
        .success()
        .stdout("true\n");

    // Off for lab: it says to refresh lab's launcher, and lab is left alone
    // at once, even by its old launcher (mcc checks the setting itself).
    sb.mcc()
        .args(["config", "set", "profiles.lab.auto_sync", "false"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "run `mcc refresh-launchers lab` so `lab`'s launcher stops catching up with the default profile when it starts",
        ));
    change_default(&sb);
    launch(&sb, "lab");
    assert!(!caught_up(&sb, "lab") && !caught_up(&sb, "work"));
    launch(&sb, "work");
    assert!(caught_up(&sb, "work"));
    assert!(!caught_up(&sb, "lab"));
    let cell = sync_cell(&sb, "lab");
    assert!(cell.ends_with("(auto sync off)"), "{cell}");
    sb.mcc().args(["refresh-launchers"]).assert().success();
    assert!(!sb.read(".local/bin/lab").contains("launch-sync"));
    assert!(sb.read(".local/bin/work").contains("launch-sync 'work'"));

    // Off for every profile: the launchers and `mcc run` copy nothing.
    set(&sb, "sync.auto", "false");
    sb.write(".claude/agents/new.md", "new");
    launch(&sb, "work");
    sb.mcc()
        .args(["run", "--profile", "work", "--", "-p", "hi"])
        .assert()
        .success()
        .stdout("fake claude -p hi\n");
    assert!(!sb.exists(".mcc/profiles/work/agents/new.md"));
    assert!(sb.read(CONFIG).contains("auto = false"));

    // On again: written as `true` (a `false` left in the file would keep it
    // off), and the next launch catches up.
    set(&sb, "sync.auto", "true");
    let cfg = sb.read(CONFIG);
    assert!(
        cfg.contains("auto = true") && !cfg.contains("auto = false"),
        "{cfg}"
    );
    sb.mcc()
        .args(["config", "get", "sync.auto"])
        .assert()
        .success()
        .stdout("true\n");
    launch(&sb, "work");
    assert!(sb.exists(".mcc/profiles/work/agents/new.md"));
    assert!(!sb.exists(".mcc/profiles/lab/agents/new.md"));
}

#[test]
fn refresh_launchers_adds_or_removes_the_catch_up() {
    let sb = sandbox();
    let on = sb.read(".local/bin/work");
    assert!(
        on.contains("# Catch up with the default profile ([sync] auto).\n"),
        "{on}"
    );
    assert!(on.contains("# mcc-launcher: 2\n"), "{on}");
    sb.mcc()
        .args(["config", "set", "sync.auto", "false"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "run `mcc refresh-launchers` so profiles' launchers stop catching up with the default profile when they start",
        ));
    // mcc never rewrites launchers on its own; a leftover block is harmless.
    assert_eq!(sb.read(".local/bin/work"), on);
    sb.mcc()
        .arg("doctor")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "launcher ~/.local/bin/work is current",
        ));
    sb.mcc().args(["refresh-launchers"]).assert().success();
    let off = sb.read(".local/bin/work");
    assert!(!off.contains("launch-sync"), "{off}");

    // On again, the launcher without the block is out of date.
    set(&sb, "sync.auto", "true");
    sb.mcc()
        .arg("doctor")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "doesn't catch the profile up with the default, which `[sync] auto` turns on",
        ));
    sb.mcc()
        .args(["list", "--no-usage"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "2 launchers are out of date — run `mcc refresh-launchers` so `<alias> --mcc-share` and auto sync work",
        ));
    sb.mcc().args(["refresh-launchers"]).assert().success();
    assert_eq!(sb.read(".local/bin/work"), on);
}

#[test]
fn doctor_runs_launchers_without_catching_up() {
    let sb = sandbox();
    change_default(&sb);
    // doctor runs `claude auth status` through each launcher: read-only.
    sb.mcc().arg("doctor").assert().success();
    assert!(!caught_up(&sb, "work") && !caught_up(&sb, "lab"));
    assert!(!sb.exists(".mcc/state/work/auto-sync.json"));
}

#[test]
fn a_busy_config_lock_skips_the_catch_up_but_never_the_launch() {
    use std::os::fd::AsRawFd;

    let sb = sandbox();
    change_default(&sb);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sb.path(".mcc/.lock"))
        .unwrap();
    // SAFETY: flock only reads the descriptor, which `lock` keeps open.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    launch(&sb, "work");
    sb.mcc()
        .args(["run", "--profile", "work", "--", "-p", "hi"])
        .assert()
        .success()
        .stdout("fake claude -p hi\n");
    // Nothing written, nothing recorded: the next launch tries again.
    assert!(!caught_up(&sb, "work") && !caught_up(&sb, "lab"));
    assert!(!sb.exists(".mcc/state/work/auto-sync.json"));
    assert_eq!(backups(&sb, "work"), 0);
    drop(lock);
    launch(&sb, "work");
    assert!(caught_up(&sb, "work"));
}

#[test]
fn list_shows_when_auto_sync_last_ran_and_what_it_changed() {
    let sb = sandbox();
    sb.write(".claude/skills/gamma/SKILL.md", "gamma");
    launch(&sb, "work");
    let long = stdout_of(sb.mcc().args(["list", "--long", "--no-usage"]));
    assert!(
        long.contains("    Auto sync:       on (when a profile starts)\n"),
        "{long}"
    );
    assert!(long.contains("    Last checked:    "), "{long}");
    assert!(
        long.contains("    Last auto sync:  ")
            && long.contains(" — 1 updated (skills/gamma/SKILL.md)\n"),
        "{long}"
    );

    let all = list_json(&sb);
    let work = all.iter().find(|p| p["name"] == "work").unwrap();
    let a = &work["auto_sync"];
    assert_eq!(a["enabled"], true, "{a}");
    assert!(a["off_by"].is_null(), "{a}");
    assert!(
        a["last_checked"].is_string() && a["last_applied"].is_string(),
        "{a}"
    );
    assert_eq!(a["changed"], serde_json::json!(["skills/gamma/SKILL.md"]));
    assert!(a["held"].is_null(), "{a}");
    let default = all.iter().find(|p| p["name"] == "default").unwrap();
    assert!(default["auto_sync"].is_null(), "{default}");

    // Which setting turned it off.
    set(&sb, "profiles.lab.auto_sync", "false");
    let all = list_json(&sb);
    let lab = all.iter().find(|p| p["name"] == "lab").unwrap();
    assert_eq!(lab["auto_sync"]["enabled"], false);
    assert_eq!(lab["auto_sync"]["off_by"], "profiles.lab.auto_sync");
    sb.mcc()
        .args(["list", "--long", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Auto sync:       off (profiles.lab.auto_sync = false)",
        ));
}
