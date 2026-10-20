//! Session sharing: `mcc share`, `<alias> --mcc-share`, `mcc refresh-launchers`.

mod common;

use std::path::PathBuf;
use std::process::Output;

use common::{Sandbox, by_name, json, with_profile};
use predicates::prelude::*;

/// The terms-of-service note, verbatim (issue #25).
const NOTE: &str = "⚠ Session sharing makes conversation history available across your Claude Code accounts. Use it only between accounts you own. Don't use it to get around usage limits, and never let other people use your account or credentials — doing so can violate Anthropic's Consumer Terms (https://www.anthropic.com/legal/consumer-terms) and Usage Policy (https://www.anthropic.com/legal/aup), including the updated Usage Policy that takes effect November 12, 2026.";

/// The one-line reminder printed once the note was shown (issue #36).
const REMINDER: &str = "⚠ Session sharing is on: use it only between accounts you own, never to get around usage limits or to let other people use your account (Anthropic's Consumer Terms and Usage Policy; full note: `mcc share --help`).";

const SESSION: &str = "projects/-tmp-proj/session1.jsonl";

fn out(o: &[u8]) -> String {
    String::from_utf8_lossy(o).into_owned()
}

/// True when `stderr` carries the note as a line of its own, byte for byte.
fn has_note(stderr: &str) -> bool {
    stderr.lines().any(|l| l == NOTE)
}

/// True when `stderr` carries the one-line reminder as a line of its own.
fn has_reminder(stderr: &str) -> bool {
    stderr.lines().any(|l| l == REMINDER)
}

fn launch(sb: &Sandbox, alias: &str, args: &[&str]) -> Output {
    sb.launcher(alias).args(args).output().unwrap()
}

fn is_link(sb: &Sandbox, rel: &str) -> bool {
    std::fs::symlink_metadata(sb.path(rel))
        .unwrap()
        .file_type()
        .is_symlink()
}

fn link_target(sb: &Sandbox, rel: &str) -> PathBuf {
    std::fs::read_link(sb.path(rel)).unwrap()
}

fn store(sb: &Sandbox) -> PathBuf {
    sb.path(".mcc/shared/projects")
}

/// The `[profiles.<alias>]` table of config.toml, as text.
fn profile_section(sb: &Sandbox, alias: &str) -> String {
    let cfg = sb.read(".mcc/config.toml");
    let start = cfg
        .find(&format!("[profiles.{alias}]"))
        .unwrap_or_else(|| panic!("no [profiles.{alias}] in {cfg}"));
    let rest = &cfg[start + 1..];
    let end = rest.find("\n[").map_or(cfg.len(), |i| start + 1 + i);
    cfg[start..end].to_string()
}

/// default + `work` (clone), with a session of its own in work's projects/.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.write(&format!(".mcc/profiles/work/{SESSION}"), "{\"work\":1}\n");
    sb
}

/// The launcher `mcc add` wrote before session sharing existed (v0.2.0).
fn legacy_wrapper(sb: &Sandbox, alias: &str) -> String {
    format!(
        "#!/bin/sh\n# managed-by: mcc\n# Claude Code profile \"{alias}\". Remove with: mcc delete {alias}\n\
         export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/{alias}\"\n\
         if command -v claude >/dev/null 2>&1; then exec claude \"$@\"; fi\n\
         exec '{}' \"$@\"\n",
        sb.path("fakebin/claude").display()
    )
}

/// Turn `alias`'s launcher and rc alias back into the v0.2.0 ones.
fn make_legacy(sb: &Sandbox, alias: &str) {
    sb.write(&format!(".local/bin/{alias}"), &legacy_wrapper(sb, alias));
    common::chmod_x(&sb.path(&format!(".local/bin/{alias}")));
    let rc = sb.read(".zshrc").replace(
        &format!("alias {alias}='\"$HOME/.local/bin/{alias}\"'"),
        &format!("alias {alias}='CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/{alias}\" claude'"),
    );
    sb.write(".zshrc", &rc);
}

#[test]
fn sharing_is_off_by_default_and_launching_is_unchanged() {
    let sb = sandbox();
    assert!(!is_link(&sb, ".mcc/profiles/work/projects"));
    assert!(!sb.exists(".mcc/shared"));
    let section = profile_section(&sb, "work");
    assert!(!section.contains("share_sessions"), "{section}");

    let o = launch(&sb, "work", &["hello"]);
    assert!(o.status.success());
    assert_eq!(out(&o.stdout), "fake claude hello\n");
    assert!(
        !out(&o.stderr).contains("Session sharing"),
        "{}",
        out(&o.stderr)
    );
    assert!(!is_link(&sb, ".mcc/profiles/work/projects"));

    let o = sb
        .mcc()
        .args(["list", "--json", "--no-usage"])
        .assert()
        .success();
    let all = json(&o.get_output().stdout);
    let arr = all.as_array().unwrap();
    assert_eq!(by_name(arr, "work")["share_sessions"], false);
    assert_eq!(by_name(arr, "work")["launcher_current"], true);
    assert_eq!(by_name(arr, "default")["share_sessions"], false);
}

#[test]
fn launcher_keeps_the_same_claude_config_dir() {
    let sb = sandbox();
    // From here on claude only reports the config dir it was given (the
    // keychain service name is a hash of exactly this string).
    sb.write(
        "fakebin/claude",
        "#!/bin/sh\necho \"dir=$CLAUDE_CONFIG_DIR\"\n",
    );
    common::chmod_x(&sb.path("fakebin/claude"));
    let expected = format!("dir={}\n", sb.path(".mcc/profiles/work").display());

    // The v0.2.0 rc alias and launcher…
    let alias = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" claude",
        ])
        .env_clear()
        .env("HOME", sb.home())
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", sb.path("fakebin").display()),
        )
        .output()
        .unwrap();
    assert_eq!(out(&alias.stdout), expected);
    let new = launch(&sb, "work", &[]);
    make_legacy(&sb, "work");
    assert_eq!(out(&launch(&sb, "work", &[]).stdout), expected);
    // …the current launcher, and the refreshed one: all the same.
    assert_eq!(out(&new.stdout), expected);
    sb.mcc()
        .args(["refresh-launchers", "work"])
        .assert()
        .success();
    assert_eq!(out(&launch(&sb, "work", &[]).stdout), expected);
    assert_eq!(out(&launch(&sb, "work", &["--mcc-share"]).stdout), expected);
}

#[test]
fn launch_flag_turns_sharing_on_and_is_stripped() {
    let sb = sandbox();
    let o = launch(&sb, "work", &["--mcc-share", "--resume"]);
    let err = out(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert_eq!(out(&o.stdout), "fake claude --resume\n");
    assert!(has_note(&err), "{err}");
    assert!(err.contains("https://www.anthropic.com/legal/consumer-terms"));
    assert!(err.contains("https://www.anthropic.com/legal/aup"));
    // Launch mode prints the note and nothing else.
    assert_eq!(err.trim_end(), NOTE, "{err}");

    assert_eq!(link_target(&sb, ".mcc/profiles/work/projects"), store(&sb));
    assert_eq!(
        std::fs::read_to_string(store(&sb).join("-tmp-proj/session1.jsonl")).unwrap(),
        "{\"work\":1}\n"
    );
    assert!(profile_section(&sb, "work").contains("share_sessions = true"));

    // Later launches with the flag show the one-line reminder instead.
    let o = launch(&sb, "work", &["--mcc-share"]);
    assert_eq!(out(&o.stdout), "fake claude \n");
    assert_eq!(out(&o.stderr).trim_end(), REMINDER);
}

#[test]
fn launch_flag_only_counts_as_the_first_argument() {
    let sb = sandbox();
    let o = launch(&sb, "work", &["--resume", "--mcc-share"]);
    assert_eq!(out(&o.stdout), "fake claude --resume --mcc-share\n");
    assert!(!has_note(&out(&o.stderr)));
    assert!(!is_link(&sb, ".mcc/profiles/work/projects"));
    assert!(!profile_section(&sb, "work").contains("share_sessions"));
}

#[test]
fn launch_flag_fails_closed() {
    let sb = sandbox();
    let elsewhere = sb.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::remove_dir_all(sb.path(".mcc/profiles/work/projects")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, sb.path(".mcc/profiles/work/projects")).unwrap();

    let o = launch(&sb, "work", &["--mcc-share", "--resume"]);
    assert_eq!(o.status.code(), Some(3), "{}", out(&o.stderr));
    // claude never started.
    assert_eq!(out(&o.stdout), "");
    assert!(out(&o.stderr).contains("not to the shared store"));
    assert_eq!(link_target(&sb, ".mcc/profiles/work/projects"), elsewhere);
}

#[test]
fn share_prints_the_note_and_records_the_setting() {
    let sb = sandbox();
    let o = sb.mcc().args(["share", "work"]).assert().success();
    let err = out(&o.get_output().stderr);
    assert!(has_note(&err), "{err}");
    assert!(out(&o.get_output().stdout).is_empty());
    assert!(err.contains("Shared store:"), "{err}");
    let section = profile_section(&sb, "work");
    assert!(section.contains("share_sessions = true"), "{section}");
    assert_eq!(link_target(&sb, ".mcc/profiles/work/projects"), store(&sb));

    // Idempotent; the note was shown, so later runs print the reminder.
    let o = sb.mcc().args(["share", "work"]).assert().success();
    let err = out(&o.get_output().stderr);
    assert!(!has_note(&err), "{err}");
    assert!(has_reminder(&err), "{err}");
    assert!(err.contains("already shared"), "{err}");
    // Even with --quiet.
    let o = sb.mcc().args(["share", "work", "-q"]).assert().success();
    assert!(has_reminder(&out(&o.get_output().stderr)));
}

/// Issue #36: the full note is shown the first time sharing is turned on
/// for a profile, and when is recorded in its config entry.
#[test]
fn the_full_note_is_shown_once_and_acknowledged() {
    let sb = sandbox();
    assert!(!profile_section(&sb, "work").contains("share_note_acknowledged_at"));
    let o = launch(&sb, "work", &["--mcc-share"]);
    let err = out(&o.stderr);
    assert!(has_note(&err), "{err}");
    assert!(!has_reminder(&err), "{err}");
    let section = profile_section(&sb, "work");
    let stamp = section
        .lines()
        .find_map(|l| l.strip_prefix("share_note_acknowledged_at = "))
        .unwrap_or_else(|| panic!("no acknowledgement in {section}"));
    let stamp = stamp.trim_matches('"');
    assert!(
        chrono::DateTime::parse_from_rfc3339(stamp).is_ok(),
        "{stamp}"
    );

    // Turning sharing off and on again keeps the acknowledgement: reminder.
    sb.mcc().args(["share", "work", "--off"]).assert().success();
    assert!(profile_section(&sb, "work").contains(stamp));
    let o = sb.mcc().args(["share", "work"]).assert().success();
    let err = out(&o.get_output().stderr);
    assert!(has_reminder(&err) && !has_note(&err), "{err}");
    assert!(profile_section(&sb, "work").contains(stamp));
}

/// A profile that was sharing before acknowledgements existed sees the full
/// note once more, then the reminder.
#[test]
fn a_profile_shared_before_acknowledgements_sees_the_note_once() {
    let sb = sandbox();
    sb.mcc().args(["share", "work"]).assert().success();
    let cfg = sb.read(".mcc/config.toml");
    let legacy: String = cfg
        .lines()
        .filter(|l| !l.starts_with("share_note_acknowledged_at"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(legacy, cfg);
    sb.write(".mcc/config.toml", &legacy);

    assert!(has_note(
        &out(&launch(&sb, "work", &["--mcc-share"]).stderr)
    ));
    assert!(profile_section(&sb, "work").contains("share_note_acknowledged_at = "));
    assert!(has_reminder(&out(
        &launch(&sb, "work", &["--mcc-share"]).stderr
    )));
}

/// Issue #35: `--mcc-share` on a profile that already shares changes
/// nothing, so it does not rewrite config.toml.
#[test]
fn launching_an_already_shared_profile_leaves_the_config_alone() {
    use std::os::unix::fs::MetadataExt;
    let sb = sandbox();
    launch(&sb, "work", &["--mcc-share"]);
    let path = sb.path(".mcc/config.toml");
    let before = std::fs::metadata(&path).unwrap();
    let text = sb.read(".mcc/config.toml");
    let o = launch(&sb, "work", &["--mcc-share"]);
    assert!(o.status.success(), "{}", out(&o.stderr));
    let after = std::fs::metadata(&path).unwrap();
    // An atomic rewrite would have replaced the file (new inode).
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.mtime_nsec(), after.mtime_nsec());
    assert_eq!(sb.read(".mcc/config.toml"), text);
}

/// Issue #35: comments and formatting written by hand survive `mcc share`.
#[test]
fn share_keeps_hand_written_comments() {
    let sb = sandbox();
    let cfg = sb.read(".mcc/config.toml").replace(
        "[profiles.work]\n",
        "# my work account\n[profiles.work]   # keep me\n",
    );
    sb.write(".mcc/config.toml", &cfg);
    sb.mcc().args(["share", "work"]).assert().success();
    let after = sb.read(".mcc/config.toml");
    assert!(
        after.contains("# my work account\n[profiles.work]   # keep me\n"),
        "{after}"
    );
    assert!(profile_section(&sb, "work").contains("share_sessions = true"));
}

#[test]
fn only_projects_is_shared() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    let own = [
        "history.jsonl",
        "sessions/4242.json",
        "shell-snapshots/snap.sh",
        "file-history/abc/1",
        "todos/t.json",
        ".credentials.lock",
        "policy-limits.json",
        "remote-settings.json",
    ];
    for (i, rel) in own.iter().enumerate() {
        sb.write(&format!(".mcc/profiles/work/{rel}"), &format!("work-{i}"));
    }
    // Written by `mcc add` (login + onboarding).
    let account = [".credentials.json", ".claude.json"];
    let before: Vec<(String, Vec<u8>)> = own
        .iter()
        .chain(&account)
        .map(|rel| {
            let p = format!(".mcc/profiles/work/{rel}");
            let bytes = std::fs::read(sb.path(&p)).unwrap();
            (p, bytes)
        })
        .collect();
    let security_log = || std::fs::read(sb.path("security.log")).unwrap_or_default();
    let security_before = security_log();

    sb.mcc().args(["share", "work"]).assert().success();
    sb.mcc().args(["share", "lab"]).assert().success();

    for (rel, bytes) in &before {
        assert!(!is_link(&sb, rel), "{rel} must not be linked");
        assert_eq!(
            &std::fs::read(sb.path(rel)).unwrap(),
            bytes,
            "{rel} changed"
        );
    }
    for dir in ["sessions", "shell-snapshots", "file-history", "todos"] {
        assert!(!is_link(&sb, &format!(".mcc/profiles/work/{dir}")));
    }
    assert!(!sb.exists(".mcc/profiles/lab/history.jsonl"));
    assert!(!sb.exists(".mcc/profiles/lab/.credentials.lock"));
    assert_ne!(
        sb.read(".mcc/profiles/lab/.credentials.json"),
        "",
        "lab keeps its own login"
    );
    assert_eq!(security_log(), security_before, "the keychain was read");

    // work's session moved into the store and is visible from lab.
    assert!(is_link(&sb, ".mcc/profiles/work/projects"));
    assert_eq!(
        sb.read(&format!(".mcc/profiles/lab/{SESSION}")),
        "{\"work\":1}\n"
    );
    // The default profile is never touched.
    assert!(!is_link(&sb, ".claude/projects"));
    assert_eq!(sb.read(&format!(".claude/{SESSION}")), "{}\n");
}

#[test]
fn deleting_a_sharing_profile_keeps_the_store() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    sb.mcc().args(["share", "work"]).assert().success();
    sb.mcc().args(["share", "lab"]).assert().success();
    sb.write(".mcc/profiles/lab/projects/-tmp-lab/s2.jsonl", "lab\n");

    sb.mcc()
        .args(["delete", "work", "--yes"])
        .assert()
        .success();
    assert!(!sb.exists(".mcc/profiles/work"));
    assert_eq!(
        std::fs::read_to_string(store(&sb).join("-tmp-proj/session1.jsonl")).unwrap(),
        "{\"work\":1}\n"
    );
    assert!(store(&sb).join("-tmp-lab/s2.jsonl").is_file());
    assert_eq!(
        sb.read(&format!(".mcc/profiles/lab/{SESSION}")),
        "{\"work\":1}\n"
    );
}

#[test]
fn off_gives_the_profile_its_own_copy() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    sb.mcc().args(["share", "work"]).assert().success();
    sb.mcc().args(["share", "lab"]).assert().success();
    std::os::unix::fs::symlink("session1.jsonl", store(&sb).join("-tmp-proj/latest")).unwrap();

    let o = sb.mcc().args(["share", "work", "--off"]).assert().success();
    let err = out(&o.get_output().stderr);
    assert!(!err.contains("Session sharing"), "{err}");
    assert!(err.contains(".mcc/shared/projects"), "{err}");
    assert!(!is_link(&sb, ".mcc/profiles/work/projects"));
    assert_eq!(
        sb.read(&format!(".mcc/profiles/work/{SESSION}")),
        "{\"work\":1}\n"
    );
    assert_eq!(
        link_target(&sb, ".mcc/profiles/work/projects/-tmp-proj/latest"),
        PathBuf::from("session1.jsonl")
    );
    // The store and the other profile's link are untouched.
    assert!(store(&sb).join("-tmp-proj/session1.jsonl").is_file());
    assert_eq!(link_target(&sb, ".mcc/profiles/lab/projects"), store(&sb));
    assert!(!profile_section(&sb, "work").contains("share_sessions"));
    assert!(profile_section(&sb, "lab").contains("share_sessions = true"));

    // Sessions written now stay with work.
    sb.write(".mcc/profiles/work/projects/-tmp-proj/new.jsonl", "{}\n");
    assert!(!store(&sb).join("-tmp-proj/new.jsonl").exists());

    sb.mcc()
        .args(["share", "work", "--off"])
        .assert()
        .success()
        .stderr(predicate::str::contains("does not share its sessions"));
}

#[test]
fn share_rejects_the_default_profile_and_foreign_links() {
    let sb = sandbox();
    for name in ["default", "claude"] {
        sb.mcc()
            .args(["share", name])
            .assert()
            .code(3)
            .stderr(predicate::str::contains("can't share sessions"));
    }
    assert!(!is_link(&sb, ".claude/projects"));
    sb.mcc()
        .args(["share", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"nope\""));

    let elsewhere = sb.path("dotfiles/projects");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::remove_dir_all(sb.path(".mcc/profiles/work/projects")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, sb.path(".mcc/profiles/work/projects")).unwrap();
    sb.mcc()
        .args(["share", "work"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("is a symlink to"))
        .stderr(predicate::function(|e: &str| !has_note(e)));
    assert_eq!(link_target(&sb, ".mcc/profiles/work/projects"), elsewhere);
    assert!(!profile_section(&sb, "work").contains("share_sessions"));
}

#[test]
fn share_keeps_the_stores_copy_on_a_conflict() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    sb.write(&format!(".mcc/profiles/lab/{SESSION}"), "{\"lab\":1}\n");
    sb.write(".mcc/profiles/lab/projects/-tmp-proj/same.jsonl", "same\n");
    sb.write(".mcc/profiles/work/projects/-tmp-proj/same.jsonl", "same\n");
    sb.mcc().args(["share", "lab"]).assert().success();

    sb.mcc()
        .args(["share", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "1 session file(s) already existed",
        ));
    assert_eq!(
        std::fs::read_to_string(store(&sb).join("-tmp-proj/session1.jsonl")).unwrap(),
        "{\"lab\":1}\n"
    );
    let backups: Vec<_> = std::fs::read_dir(sb.path(".mcc/backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("shared-projects-work-")
        })
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(
        std::fs::read_to_string(backups[0].join("-tmp-proj/session1.jsonl")).unwrap(),
        "{\"work\":1}\n"
    );
    assert!(!backups[0].join("-tmp-proj/same.jsonl").exists());
    // No staging directory is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(sb.path(".mcc/profiles/work"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("mcc-share"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn share_recreates_a_deleted_store() {
    use std::os::unix::fs::PermissionsExt;
    let sb = sandbox();
    sb.mcc().args(["share", "work"]).assert().success();
    std::fs::remove_dir_all(store(&sb)).unwrap();
    assert!(std::fs::metadata(sb.path(".mcc/profiles/work/projects")).is_err());

    sb.mcc()
        .args(["share", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already shared"));
    assert!(store(&sb).is_dir());
    assert_eq!(
        std::fs::metadata(store(&sb)).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(link_target(&sb, ".mcc/profiles/work/projects"), store(&sb));
    assert!(
        std::fs::metadata(sb.path(".mcc/profiles/work/projects"))
            .unwrap()
            .is_dir()
    );
}

#[test]
fn share_recovers_an_interrupted_merge() {
    let sb = sandbox();
    sb.mcc().args(["share", "work"]).assert().success();
    // What a Ctrl+C between linking and merging leaves behind.
    sb.write(
        ".mcc/profiles/work/.projects.mcc-share-4242/-tmp-old/lost.jsonl",
        "lost\n",
    );
    // Not a staging directory: a symlink with the same name pattern.
    sb.write("elsewhere/-tmp-x/keep.jsonl", "keep\n");
    std::os::unix::fs::symlink(
        sb.path("elsewhere"),
        sb.path(".mcc/profiles/work/.projects.mcc-share-7"),
    )
    .unwrap();

    sb.mcc()
        .args(["share", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("recovered 1 item(s)"));
    assert_eq!(
        std::fs::read_to_string(store(&sb).join("-tmp-old/lost.jsonl")).unwrap(),
        "lost\n"
    );
    assert_eq!(
        sb.read(".mcc/profiles/work/projects/-tmp-old/lost.jsonl"),
        "lost\n"
    );
    assert!(!sb.exists(".mcc/profiles/work/.projects.mcc-share-4242"));
    assert_eq!(
        link_target(&sb, ".mcc/profiles/work/.projects.mcc-share-7"),
        sb.path("elsewhere")
    );
    assert_eq!(sb.read("elsewhere/-tmp-x/keep.jsonl"), "keep\n");
    assert!(!store(&sb).join("-tmp-x").exists());
}

#[test]
fn the_link_survives_a_rename() {
    let sb = sandbox();
    sb.mcc().args(["share", "work"]).assert().success();
    sb.mcc().args(["rename", "work", "job"]).assert().success();
    assert_eq!(link_target(&sb, ".mcc/profiles/work/projects"), store(&sb));
    assert!(profile_section(&sb, "job").contains("share_sessions = true"));
    // The acknowledgement moved with the profile: no full note again.
    let o = launch(&sb, "job", &["--mcc-share", "-c"]);
    assert_eq!(out(&o.stdout), "fake claude -c\n");
    assert!(has_reminder(&out(&o.stderr)), "{}", out(&o.stderr));
}

#[test]
fn list_marks_sharing_profiles() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    sb.mcc().args(["share", "work"]).assert().success();

    let o = sb.mcc().args(["list", "--no-usage"]).assert().success();
    let stdout = out(&o.get_output().stdout);
    let err = out(&o.get_output().stderr);
    assert!(stdout.contains("work (shared)"), "{stdout}");
    assert!(!stdout.contains("lab (shared)"), "{stdout}");
    assert!(err.contains("session counts include all of them"), "{err}");
    assert!(!err.contains("refresh-launchers"), "{err}");

    let o = sb
        .mcc()
        .args(["list", "--json", "--no-usage"])
        .assert()
        .success();
    let all = json(&o.get_output().stdout);
    let arr = all.as_array().unwrap();
    assert_eq!(by_name(arr, "work")["share_sessions"], true);
    assert_eq!(by_name(arr, "lab")["share_sessions"], false);
    assert_eq!(by_name(arr, "work")["launcher_current"], true);
}

#[test]
fn legacy_launchers_stay_as_they_are_until_refreshed() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    make_legacy(&sb, "work");
    let (wrapper, rc) = (sb.read(".local/bin/work"), sb.read(".zshrc"));

    sb.mcc().args(["list", "--no-usage"]).assert().success();
    sb.mcc().args(["sync", "work", "--yes"]).assert().success();
    sb.mcc()
        .args(["share", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("mcc refresh-launchers work"));
    sb.mcc().args(["share", "work", "--off"]).assert().success();
    assert_eq!(sb.read(".local/bin/work"), wrapper);
    assert_eq!(sb.read(".zshrc"), rc);

    // `mcc list` points at the fix, for the stale launcher only.
    sb.mcc()
        .args(["list", "--no-usage"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "1 launcher predates session sharing — run `mcc refresh-launchers`",
        ));
    let o = sb
        .mcc()
        .args(["list", "--json", "--no-usage"])
        .assert()
        .success();
    let all = json(&o.get_output().stdout);
    let arr = all.as_array().unwrap();
    assert_eq!(by_name(arr, "work")["launcher_current"], false);
    assert_eq!(by_name(arr, "lab")["launcher_current"], true);
    assert_eq!(by_name(arr, "default")["launcher_current"], true);
}

#[test]
fn refresh_launchers_upgrades_old_launchers() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    make_legacy(&sb, "work");

    sb.mcc()
        .arg("refresh-launchers")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "work: launcher and shell alias refreshed",
        ))
        .stderr(predicate::str::contains("lab: already up to date"))
        .stderr(predicate::str::contains(
            "1 refreshed, 1 already up to date, 0 skipped",
        ));
    let wrapper = sb.read(".local/bin/work");
    assert!(wrapper.contains("# mcc-launcher: 2"), "{wrapper}");
    assert!(
        wrapper.contains("export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\"\n"),
        "{wrapper}"
    );
    let rc = sb.read(".zshrc");
    assert!(
        rc.contains("alias work='\"$HOME/.local/bin/work\"'"),
        "{rc}"
    );
    assert!(!rc.contains("alias work='CLAUDE_CONFIG_DIR"), "{rc}");
    assert!(rc.contains("alias cus_luong="), "{rc}");
    sb.mcc()
        .args(["list", "--no-usage"])
        .assert()
        .success()
        .stderr(predicate::str::contains("refresh-launchers").not());

    // Idempotent: nothing rewritten, no new rc backup.
    let backups = || std::fs::read_dir(sb.path(".mcc/backups")).unwrap().count();
    let n = backups();
    sb.mcc()
        .arg("refresh-launchers")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "0 refreshed, 2 already up to date",
        ));
    assert_eq!(sb.read(".local/bin/work"), wrapper);
    assert_eq!(sb.read(".zshrc"), rc);
    assert_eq!(backups(), n);

    // The refreshed launcher understands the flag.
    let o = launch(&sb, "work", &["--mcc-share", "--resume"]);
    assert_eq!(out(&o.stdout), "fake claude --resume\n");
    assert!(has_note(&out(&o.stderr)));
}

#[test]
fn refresh_launchers_skips_what_it_cannot_refresh() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    // A profile without a recorded launcher, and a launcher mcc didn't write.
    let cfg = sb.read(".mcc/config.toml");
    let lab_wrapper = format!("wrapper = \"{}\"\n", sb.path(".local/bin/lab").display());
    assert!(cfg.contains(&lab_wrapper), "{cfg}");
    sb.write(".mcc/config.toml", &cfg.replace(&lab_wrapper, ""));
    sb.write(".local/bin/work", "#!/bin/sh\necho mine\n");

    sb.mcc()
        .arg("refresh-launchers")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "lab: no launcher recorded — skipped",
        ))
        .stderr(predicate::str::contains(
            "work: ~/.local/bin/work was not created by mcc",
        ))
        .stderr(predicate::str::contains(
            "0 refreshed, 0 already up to date, 2 skipped",
        ));
    assert_eq!(sb.read(".local/bin/work"), "#!/bin/sh\necho mine\n");
    // Sharing does not need a launcher.
    sb.mcc().args(["share", "lab"]).assert().success();

    sb.mcc()
        .args(["refresh-launchers", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"nope\""));
}

#[test]
fn share_and_refresh_launchers_are_reserved_names() {
    let sb = Sandbox::new();
    sb.seed_default();
    for name in ["share", "refresh-launchers"] {
        sb.mcc()
            .args(["add", name, "--fresh", "--no-login"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("reserved by mcc"));
    }
}

#[test]
fn help_documents_the_flag_the_setting_and_the_note() {
    let sb = Sandbox::new();
    let o = sb.mcc().args(["share", "--help"]).assert().success();
    let help = out(&o.get_output().stdout);
    for needle in [
        "--off",
        "work --mcc-share",
        "share_sessions = true  # default: false",
        "does nothing on its own",
        "mcc refresh-launchers",
        "--group <NAME>",
        "--allow-cross-org",
        "~/.mcc/shared/groups/<name>/projects",
        "share_group",
    ] {
        assert!(help.contains(needle), "missing {needle:?} in:\n{help}");
    }
    // clap wraps long lines; the note's words are all there, in order.
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words(&help).contains(&words(NOTE)), "{help}");
    assert!(words(&help).contains(&words(REMINDER)), "{help}");
    assert!(help.contains("share_note_acknowledged_at"), "{help}");
    assert!(!help.contains("--from-launch"), "{help}");
    sb.mcc()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("mcc share work"))
        .stdout(predicate::str::contains("mcc refresh-launchers"));
}

// ---- groups and the cross-organization guard (issue #40) -------------------

/// A symlink (false when missing, as for a fresh profile's `projects/`).
fn linked(sb: &Sandbox, rel: &str) -> bool {
    std::fs::symlink_metadata(sb.path(rel)).is_ok_and(|m| m.file_type().is_symlink())
}

fn group_store(sb: &Sandbox, group: &str) -> PathBuf {
    sb.path(&format!(".mcc/shared/groups/{group}/projects"))
}

/// A fresh profile logged in to organization `(name, uuid, type)`, with
/// the `oauthAccount` Claude Code writes to its `.claude.json` (where the
/// organization type comes from), or to an account without any
/// organization when `org` is `None`.
fn add_in_org(sb: &Sandbox, alias: &str, org: Option<(&str, &str, &str)>) {
    let mut cmd = sb.mcc();
    cmd.args(["add", alias, "--fresh"]);
    if let Some((name, id, _)) = org {
        cmd.env("FAKE_ORG_NAME", name).env("FAKE_ORG_ID", id);
    }
    cmd.assert().success();
    if let Some((name, id, ty)) = org {
        let rel = format!(".mcc/profiles/{alias}/.claude.json");
        let mut state = if sb.exists(&rel) {
            json(sb.read(&rel).as_bytes())
        } else {
            serde_json::json!({})
        };
        state["oauthAccount"] = serde_json::json!({
            "emailAddress": "fake@example.com",
            "organizationName": name,
            "organizationUuid": id,
            "organizationType": ty,
            "billingType": "stripe_subscription",
        });
        sb.write(&rel, &state.to_string());
    }
}

const TEAM: &str = "claude_team";

#[test]
fn group_uses_a_store_of_its_own() {
    let sb = sandbox();
    with_profile(&sb, "lab", "--fresh");
    with_profile(&sb, "side", "--fresh");
    let o = sb
        .mcc()
        .args(["share", "work", "--group", "alpha"])
        .assert()
        .success();
    let err = out(&o.get_output().stderr);
    assert!(err.contains("group \"alpha\""), "{err}");
    assert_eq!(
        link_target(&sb, ".mcc/profiles/work/projects"),
        group_store(&sb, "alpha")
    );
    assert!(
        group_store(&sb, "alpha")
            .join("-tmp-proj/session1.jsonl")
            .is_file()
    );
    assert!(profile_section(&sb, "work").contains("share_group = \"alpha\""));
    // The default group's store is another one.
    sb.mcc().args(["share", "side"]).assert().success();
    assert_eq!(link_target(&sb, ".mcc/profiles/side/projects"), store(&sb));
    assert!(!store(&sb).join("-tmp-proj/session1.jsonl").exists());
    assert!(!profile_section(&sb, "side").contains("share_group"));
    // Another profile of the group sees work's sessions.
    sb.mcc()
        .args(["share", "lab", "--group", "alpha"])
        .assert()
        .success();
    assert!(
        sb.path(".mcc/profiles/lab/projects/-tmp-proj/session1.jsonl")
            .is_file()
    );
    // Group names are one group whatever the case.
    with_profile(&sb, "upper", "--fresh");
    sb.mcc()
        .args(["share", "upper", "--group", "ALPHA"])
        .assert()
        .success();
    assert_eq!(
        link_target(&sb, ".mcc/profiles/upper/projects"),
        group_store(&sb, "alpha")
    );
    assert!(profile_section(&sb, "upper").contains("share_group = \"alpha\""));
    // `--group default` is the default group.
    sb.mcc()
        .args(["share", "side", "--group", "default"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already shared"));

    // The launcher keeps the recorded group.
    let o = launch(&sb, "work", &["--mcc-share"]);
    assert!(o.status.success(), "{}", out(&o.stderr));
    assert_eq!(
        link_target(&sb, ".mcc/profiles/work/projects"),
        group_store(&sb, "alpha")
    );

    let o = sb
        .mcc()
        .args(["list", "--json", "--no-usage"])
        .assert()
        .success();
    let all = json(&o.get_output().stdout);
    let arr = all.as_array().unwrap();
    assert_eq!(by_name(arr, "work")["share_group"], "alpha");
    assert_eq!(by_name(arr, "side")["share_group"], "default");
    assert!(by_name(arr, "default")["share_group"].is_null());
    sb.mcc()
        .args(["list", "--no-usage"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work (shared: alpha)"))
        .stdout(predicate::str::contains("side (shared)"));
}

#[test]
fn switching_groups_needs_off_first() {
    let sb = sandbox();
    sb.mcc()
        .args(["share", "work", "--group", "alpha"])
        .assert()
        .success();
    sb.mcc()
        .args(["share", "work", "--group", "beta"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "already shares its sessions in group \"alpha\"",
        ))
        .stderr(predicate::str::contains("mcc share work --off"));
    assert_eq!(
        link_target(&sb, ".mcc/profiles/work/projects"),
        group_store(&sb, "alpha")
    );
    let o = sb.mcc().args(["share", "work", "--off"]).assert().success();
    assert!(out(&o.get_output().stderr).contains("groups/alpha/projects"));
    assert!(!is_link(&sb, ".mcc/profiles/work/projects"));
    // The group is remembered for the next `mcc share`.
    assert!(profile_section(&sb, "work").contains("share_group = \"alpha\""));
    sb.mcc()
        .args(["share", "work", "--group", "beta"])
        .assert()
        .success();
    assert_eq!(
        link_target(&sb, ".mcc/profiles/work/projects"),
        group_store(&sb, "beta")
    );
}

#[test]
fn invalid_group_names_are_usage_errors() {
    let sb = sandbox();
    for bad in ["../x", "a/b", ".hidden", ""] {
        sb.mcc()
            .args(["share", "work", "--group", bad])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("invalid group name"));
    }
    assert!(!is_link(&sb, ".mcc/profiles/work/projects"));
    assert!(!sb.exists(".mcc/shared"));
}

/// Hand-edit `alias`'s `share_group` in config.toml (it must be set).
fn set_share_group(sb: &Sandbox, alias: &str, from: &str, to: &str) {
    let cfg = sb.read(".mcc/config.toml");
    let section = profile_section(sb, alias);
    let edited = section.replace(
        &format!("share_group = \"{from}\""),
        &format!("share_group = {to:?}"),
    );
    assert_ne!(section, edited, "{section}");
    sb.write(".mcc/config.toml", &cfg.replace(&section, &edited));
}

/// Every entry under `dir`, recursively, for comparing before and after.
fn tree(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .map(|e| e.unwrap().into_path())
        .collect();
    all.sort();
    all
}

#[test]
fn a_hand_edited_share_group_is_checked_like_the_flag() {
    for bad in ["../..", "/"] {
        let sb = sandbox();
        with_profile(&sb, "side", "--fresh");
        sb.write(".mcc/profiles/side/projects/-tmp-p/s.jsonl", "{}\n");
        sb.mcc()
            .args(["share", "work", "--group", "alpha"])
            .assert()
            .success();
        sb.mcc()
            .args(["share", "side", "--group", "beta"])
            .assert()
            .success();
        sb.mcc().args(["share", "side", "--off"]).assert().success();
        set_share_group(&sb, "work", "alpha", bad);
        set_share_group(&sb, "side", "beta", bad);
        let home_before = tree(&sb.path(""));

        let refused = |args: &[&str]| {
            sb.mcc()
                .args(args)
                .assert()
                .code(3)
                .stderr(predicate::str::contains("invalid share_group"))
                .stderr(predicate::str::contains("[profiles."))
                .stderr(predicate::str::contains("config.toml"));
        };
        // Sharing: `--off` must not follow the stored group anywhere.
        refused(&["share", "work", "--off"]);
        refused(&["share", "work"]);
        refused(&["share", "work", "--from-launch"]);
        // Not sharing: turning it on must not create a store anywhere.
        refused(&["share", "side"]);
        refused(&["share", "side", "--off"]);
        let o = launch(&sb, "side", &["--mcc-share"]);
        assert_eq!(o.status.code(), Some(3), "{}", out(&o.stderr));
        assert_eq!(out(&o.stdout), "");
        assert!(out(&o.stderr).contains("invalid share_group"));

        assert_eq!(
            link_target(&sb, ".mcc/profiles/work/projects"),
            group_store(&sb, "alpha"),
            "{bad:?}"
        );
        assert!(!is_link(&sb, ".mcc/profiles/side/projects"));
        assert!(
            sb.path(".mcc/profiles/side/projects/-tmp-p/s.jsonl")
                .is_file()
        );
        // Nothing created, moved or removed anywhere (logs aside).
        let logs = sb.path(".mcc/logs");
        let keep = |v: Vec<PathBuf>| -> Vec<PathBuf> {
            v.into_iter().filter(|p| !p.starts_with(&logs)).collect()
        };
        assert_eq!(keep(tree(&sb.path(""))), keep(home_before), "{bad:?}");
    }
}

#[test]
fn a_hand_edited_share_group_is_normalized_like_the_flag() {
    let sb = sandbox();
    sb.mcc()
        .args(["share", "work", "--group", "alpha"])
        .assert()
        .success();
    sb.mcc().args(["share", "work", "--off"]).assert().success();
    set_share_group(&sb, "work", "alpha", "ALPHA");
    sb.mcc().args(["share", "work"]).assert().success();
    assert_eq!(
        link_target(&sb, ".mcc/profiles/work/projects"),
        group_store(&sb, "alpha")
    );
    assert!(profile_section(&sb, "work").contains("share_group = \"alpha\""));
    sb.mcc().args(["share", "work", "--off"]).assert().success();
    set_share_group(&sb, "work", "alpha", "default");
    sb.mcc().args(["share", "work"]).assert().success();
    assert_eq!(link_target(&sb, ".mcc/profiles/work/projects"), store(&sb));
}

#[test]
fn sharing_across_organizations_needs_allow_cross_org() {
    let sb = Sandbox::new();
    sb.seed_default();
    add_in_org(&sb, "acme", Some(("Acme", "org-acme", TEAM)));
    add_in_org(&sb, "acme2", Some(("Acme Inc", "org-acme", TEAM)));
    add_in_org(&sb, "beta", Some(("Beta", "org-beta", "claude_enterprise")));
    sb.mcc().args(["share", "acme"]).assert().success();
    // The same organization (by UUID): shared.
    sb.mcc().args(["share", "acme2"]).assert().success();
    // Another organization: refused, nothing changes.
    sb.mcc()
        .args(["share", "beta"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("refusing to share"))
        .stderr(predicate::str::contains("different organizations"))
        .stderr(predicate::str::contains("\"acme\" (organization Acme)"))
        .stderr(predicate::str::contains("--allow-cross-org"))
        .stderr(predicate::function(|e: &str| !has_note(e)));
    assert!(!linked(&sb, ".mcc/profiles/beta/projects"));
    assert!(!profile_section(&sb, "beta").contains("share_sessions"));
    // From the launcher: fails closed, claude never starts.
    let o = launch(&sb, "beta", &["--mcc-share"]);
    assert_eq!(o.status.code(), Some(3), "{}", out(&o.stderr));
    assert_eq!(out(&o.stdout), "");
    assert!(!linked(&sb, ".mcc/profiles/beta/projects"));
    // A group of its own is fine.
    sb.mcc()
        .args(["share", "beta", "--group", "beta"])
        .assert()
        .success();
    sb.mcc().args(["share", "beta", "--off"]).assert().success();
    // --allow-cross-org shares, warns and records it.
    sb.mcc()
        .args(["share", "beta", "--group", "default", "--allow-cross-org"])
        .assert()
        .success()
        .stderr(predicate::str::contains("sharing across organizations"));
    assert_eq!(link_target(&sb, ".mcc/profiles/beta/projects"), store(&sb));
    let section = profile_section(&sb, "beta");
    assert!(section.contains("share_cross_org_allowed_at"), "{section}");
    // Later launches don't warn again.
    let o = launch(&sb, "beta", &["--mcc-share"]);
    assert!(o.status.success());
    assert_eq!(out(&o.stderr).trim_end(), REMINDER);
    // --off clears the allowance.
    sb.mcc().args(["share", "beta", "--off"]).assert().success();
    assert!(!profile_section(&sb, "beta").contains("share_cross_org_allowed_at"));
}

#[test]
fn organization_to_personal_is_the_strictest() {
    let sb = Sandbox::new();
    sb.seed_default();
    add_in_org(&sb, "work", Some(("Acme", "org-acme", TEAM)));
    // A personal Max plan: Claude Code gives it an organization of its own.
    add_in_org(
        &sb,
        "home",
        Some(("fake@example.com's Organization", "org-home", "claude_max")),
    );
    sb.mcc().args(["share", "work"]).assert().success();
    // Organization sessions would reach a personal account: refused…
    sb.mcc()
        .args(["share", "home"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("consumer terms"))
        .stderr(predicate::str::contains("\"work\" (organization Acme)"))
        .stderr(predicate::str::contains(
            "profile \"home\" (a personal account)",
        ));
    // …and --allow-cross-org alone is not enough without a terminal.
    sb.mcc()
        .args(["share", "home", "--allow-cross-org"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "needs a confirmation in a terminal",
        ));
    assert!(!linked(&sb, ".mcc/profiles/home/projects"));
    assert!(!profile_section(&sb, "home").contains("share_sessions"));
    // The other direction too: an organization joining personal profiles.
    sb.mcc().args(["share", "work", "--off"]).assert().success();
    sb.mcc()
        .args(["share", "home", "--group", "home"])
        .assert()
        .success();
    sb.mcc()
        .args(["share", "work", "--group", "home", "--allow-cross-org"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "needs a confirmation in a terminal",
        ));
    assert!(!linked(&sb, ".mcc/profiles/work/projects"));
}

#[test]
fn unknown_accounts_fail_safe() {
    let sb = Sandbox::new();
    sb.seed_default();
    add_in_org(&sb, "home", None);
    add_in_org(&sb, "work", Some(("Acme", "org-acme", TEAM)));
    sb.mcc()
        .args(["add", "blank", "--fresh", "--no-login"])
        .assert()
        .success();
    sb.mcc().args(["share", "home"]).assert().success();
    // No account recorded: can't tell, so refused.
    sb.mcc()
        .args(["share", "blank"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("can't tell which organization"))
        .stderr(predicate::str::contains("claude auth login"));
    sb.mcc()
        .args(["share", "blank", "--allow-cross-org"])
        .assert()
        .success();
    // An organization joining a group with an unknown account counts as
    // organization → personal.
    sb.mcc()
        .args(["share", "work", "--allow-cross-org"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "needs a confirmation in a terminal",
        ));
}

/// Upgrading: profiles that shared the one global store before groups and
/// the guard existed keep it, and their sessions, as they are.
#[test]
fn profiles_sharing_the_global_store_keep_their_sessions() {
    let sb = Sandbox::new();
    sb.seed_default();
    add_in_org(&sb, "work", Some(("Acme", "org-acme", TEAM)));
    add_in_org(&sb, "home", None);
    // What an older mcc left: both linked to ~/.mcc/shared/projects, with
    // `share_sessions = true` and nothing else.
    std::fs::create_dir_all(store(&sb).join("-tmp-proj")).unwrap();
    sb.write(".mcc/shared/projects/-tmp-proj/old.jsonl", "{\"old\":1}\n");
    for alias in ["work", "home"] {
        let projects = sb.path(&format!(".mcc/profiles/{alias}/projects"));
        if projects.exists() {
            std::fs::remove_dir_all(&projects).unwrap();
        }
        std::os::unix::fs::symlink(store(&sb), &projects).unwrap();
    }
    let cfg = sb.read(".mcc/config.toml");
    let cfg = cfg
        .replace(
            "[profiles.work]\n",
            "[profiles.work]\nshare_sessions = true\nshare_note_acknowledged_at = \"2026-10-01T00:00:00+00:00\"\n",
        )
        .replace(
            "[profiles.home]\n",
            "[profiles.home]\nshare_sessions = true\nshare_note_acknowledged_at = \"2026-10-01T00:00:00+00:00\"\n",
        );
    sb.write(".mcc/config.toml", &cfg);
    let before = sb.read(".mcc/config.toml");

    // Launching still works: a warning, not a refusal; nothing moves.
    let o = launch(&sb, "home", &["--mcc-share", "--resume"]);
    let err = out(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert_eq!(out(&o.stdout), "fake claude --resume\n");
    assert!(err.contains("across organizations"), "{err}");
    assert!(has_reminder(&err), "{err}");
    assert_eq!(link_target(&sb, ".mcc/profiles/home/projects"), store(&sb));
    assert_eq!(link_target(&sb, ".mcc/profiles/work/projects"), store(&sb));
    assert_eq!(
        sb.read(".mcc/profiles/home/projects/-tmp-proj/old.jsonl"),
        "{\"old\":1}\n"
    );
    sb.mcc()
        .args(["share", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already shared"))
        .stderr(predicate::str::contains("mcc share work --off"));
    // The config is left as it was: the default group needs no key.
    assert_eq!(sb.read(".mcc/config.toml"), before);

    // Moving one to a group of its own keeps a copy of every session.
    sb.mcc().args(["share", "home", "--off"]).assert().success();
    sb.mcc()
        .args(["share", "home", "--group", "home"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(group_store(&sb, "home").join("-tmp-proj/old.jsonl")).unwrap(),
        "{\"old\":1}\n"
    );
    // The global store still holds them for work.
    assert!(store(&sb).join("-tmp-proj/old.jsonl").is_file());
}
