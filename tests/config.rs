//! `~/.mcc/config.toml` writes: hand-written comments survive, and the
//! advisory lock on `~/.mcc/.lock` keeps concurrent commands from losing
//! each other's changes (issue #35).

mod common;

use std::fs::File;
use std::os::fd::AsRawFd;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use common::{Sandbox, with_profile};

fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb
}

/// `mcc project set work --path ~/<rel>`, creating the directory.
fn project_set(sb: &Sandbox, rel: &str) -> std::process::Command {
    std::fs::create_dir_all(sb.path(rel)).unwrap();
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("mcc"));
    let base = sb.mcc();
    cmd.env_clear()
        .envs(base.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))))
        .current_dir(sb.home())
        .args(["project", "set", "work", "--path"])
        .arg(sb.path(rel));
    cmd
}

/// Hold `~/.mcc/.lock` the way mcc does, until the file is dropped.
fn hold_lock(sb: &Sandbox) -> File {
    let f = File::create(sb.path(".mcc/.lock")).unwrap();
    // SAFETY: the descriptor stays open for the duration of the call.
    assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
    f
}

/// Wait up to `secs` for `child` to exit; kill it and fail if it doesn't.
fn wait_for(mut child: Child, secs: u64) -> std::process::Output {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("command did not finish within {secs}s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn comments_survive_mutating_commands() {
    let sb = sandbox();
    let cfg = sb.read(".mcc/config.toml");
    let commented = format!("# my own note at the top\n{cfg}")
        .replace("[sync]\n", "# what to skip when syncing\n[sync]\n")
        .replace("[profiles.work]\n", "[profiles.work]  # the day job\n");
    assert_ne!(commented, format!("# my own note at the top\n{cfg}"));
    sb.write(".mcc/config.toml", &commented);

    let o = project_set(&sb, "code/app").output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    sb.mcc().args(["share", "work"]).assert().success();

    let after = sb.read(".mcc/config.toml");
    for comment in [
        "# my own note at the top\n",
        "# what to skip when syncing\n[sync]\n",
        "[profiles.work]  # the day job\n",
    ] {
        assert!(after.contains(comment), "lost {comment:?} in:\n{after}");
    }
    assert!(after.contains("profile = \"work\""), "{after}");
    assert!(after.contains("share_sessions = true"), "{after}");

    // Undoing the changes keeps them too.
    sb.mcc()
        .args(["project", "unset", "--path"])
        .arg(std::fs::canonicalize(sb.path("code/app")).unwrap())
        .assert()
        .success();
    let after = sb.read(".mcc/config.toml");
    assert!(after.contains("# what to skip when syncing\n"), "{after}");
    assert!(!after.contains("[projects"), "{after}");
}

#[test]
fn concurrent_writers_lose_nothing() {
    let sb = sandbox();
    let children: Vec<Child> = (0..8)
        .map(|i| {
            project_set(&sb, &format!("code/p{i}"))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let o = wait_for(child, 60);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let cfg = sb.read(".mcc/config.toml");
    for i in 0..8 {
        assert!(
            cfg.contains(&format!("/code/p{i}\"]")),
            "lost p{i} in:\n{cfg}"
        );
    }
}

#[test]
fn a_writer_waits_for_the_lock() {
    let sb = sandbox();
    let lock = hold_lock(&sb);
    let child = project_set(&sb, "code/app")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(!sb.read(".mcc/config.toml").contains("[projects"));
    drop(lock);
    let o = wait_for(child, 30);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("waiting for another mcc command"), "{err}");
    assert!(sb.read(".mcc/config.toml").contains("[projects"));
}

#[test]
fn read_only_commands_do_not_wait_for_the_lock() {
    let sb = sandbox();
    let _lock = hold_lock(&sb);
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("mcc"));
    let base = sb.mcc();
    let child = cmd
        .env_clear()
        .envs(base.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))))
        .current_dir(sb.home())
        .args(["list", "--no-usage"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let o = wait_for(child, 30);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
