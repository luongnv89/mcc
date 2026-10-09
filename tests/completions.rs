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
fn rejects_unsupported_shell() {
    let sb = Sandbox::new();
    sb.mcc()
        .args(["completions", "tcsh"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid value"));
    sb.mcc().arg("completions").assert().code(2);
}
