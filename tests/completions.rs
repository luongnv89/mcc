//! `mcc completions <shell>`.

mod common;

use common::Sandbox;
use predicates::prelude::*;

#[test]
fn generates_for_supported_shells() {
    let sb = Sandbox::new();
    for (shell, marker) in [
        ("bash", "complete -F"),
        ("zsh", "#compdef mcc"),
        ("fish", "complete -c mcc"),
    ] {
        sb.mcc()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains(marker))
            .stdout(predicate::str::contains("sync"))
            .stderr(predicate::str::is_empty());
    }
    // Completions never trigger first-run detection or write state.
    assert!(!sb.exists(".mcc"));
}

#[test]
fn logs_only_when_the_log_dir_exists() {
    let sb = Sandbox::new();
    sb.mcc().args(["list"]).assert().success();
    let before = sb.read(".mcc/logs/mcc.log");
    sb.mcc()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef mcc"))
        .stderr(predicate::str::is_empty());
    let after = sb.read(".mcc/logs/mcc.log");
    assert!(after.starts_with(&before), "{after}");
    let added = &after[before.len()..];
    assert_eq!(added.lines().count(), 1, "{added}");
    assert!(added.contains("[\"completions\", \"zsh\"]"), "{added}");
}

#[test]
fn rejects_unsupported_shell() {
    let sb = Sandbox::new();
    sb.mcc()
        .args(["completions", "tcsh"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid value"));
    sb.mcc().arg("completions").assert().code(2);
}
