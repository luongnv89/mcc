//! Help, version, global flags, and `mcc init`.

mod common;

use common::Sandbox;
use predicates::prelude::*;

#[test]
fn help_and_version() {
    let sb = Sandbox::new();
    sb.mcc()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("run several Claude Code accounts"))
        .stdout(predicate::str::contains("Examples:"));
    sb.mcc()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("mcc 0."));
    sb.mcc()
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Commands:"))
        .stdout(
            predicate::str::is_match(r"(?m)^  usage\s+Show each profile's 5-hour and weekly usage")
                .unwrap(),
        );
}

#[test]
fn every_subcommand_has_help() {
    let sb = Sandbox::new();
    for cmd in [
        "init",
        "add",
        "list",
        "ls",
        "delete",
        "rm",
        "sync",
        "usage",
        "rename",
        "mv",
        "share",
        "refresh-launchers",
        "project",
        "run",
        "completions",
    ] {
        sb.mcc()
            .args([cmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Usage: mcc"));
        sb.mcc().args(["help", cmd]).assert().success();
    }
}

#[test]
fn no_args_shows_help_and_fails() {
    Sandbox::new()
        .mcc()
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage: mcc"));
}

#[test]
fn unknown_argument_is_usage_error() {
    let sb = Sandbox::new();
    sb.mcc()
        .arg("--bogus")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unexpected argument"));
    sb.mcc().arg("frobnicate").assert().code(2);
}

#[test]
fn init_detects_and_saves() {
    let sb = Sandbox::new();
    sb.mcc()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "[1/4] Operating system and terminal",
        ))
        .stderr(predicate::str::contains("zsh → ~/.zshrc"))
        .stderr(predicate::str::contains("9.9.9 (Claude Code)"))
        .stderr(predicate::str::contains("TestTerm"));
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("shell = \"zsh\""), "{cfg}");
    assert!(cfg.contains("local_bin_on_path = true"), "{cfg}");
    assert!(
        sb.read(".mcc/logs/mcc.log")
            .contains("[1/4] Operating system and terminal")
    );

    // Second run reports without re-detecting.
    sb.mcc()
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("already initialized"));
}

#[test]
fn init_shell_override() {
    let sb = Sandbox::new();
    sb.mcc()
        .args(["init", "--shell", "bash"])
        .assert()
        .success();
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("shell = \"bash\""));
    let rc = if cfg!(target_os = "macos") {
        ".bash_profile"
    } else {
        ".bashrc"
    };
    assert!(cfg.contains(rc), "{cfg}");
    sb.mcc().args(["init", "--shell", "tcsh"]).assert().code(2);
}

#[test]
fn first_command_auto_initializes() {
    let sb = Sandbox::new();
    sb.mcc()
        .arg("list")
        .assert()
        .success()
        .stderr(predicate::str::contains("First run"))
        .stderr(predicate::str::contains("Detecting environment"));
    assert!(sb.exists(".mcc/config.toml"));
    sb.mcc()
        .arg("list")
        .assert()
        .success()
        .stderr(predicate::str::contains("First run").not());
}

#[test]
fn missing_claude_binary_is_reported_not_fatal_for_init() {
    let sb = Sandbox::new();
    sb.mcc()
        .env("MCC_CLAUDE_BIN", sb.path("nope/claude"))
        .arg("init")
        .assert()
        .success();
    sb.mcc()
        .env_remove("MCC_CLAUDE_BIN")
        .env("PATH", "/usr/bin:/bin")
        .arg("init")
        .arg("--force")
        .assert()
        .success()
        .stderr(predicate::str::contains("`claude` not found"));
}

#[test]
fn quiet_and_no_color() {
    let sb = Sandbox::new();
    sb.mcc()
        .args(["-q", "init"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
    sb.mcc()
        .args(["--no-color", "init", "--force"])
        .env_remove("NO_COLOR")
        .assert()
        .success()
        .stderr(predicate::str::contains("\x1b[").not());
}

#[test]
fn invalid_config_is_input_error() {
    let sb = Sandbox::new();
    sb.write(".mcc/config.toml", "profiles = = broken");
    sb.mcc()
        .arg("list")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("not valid mcc config"))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn no_color_also_disables_clap_colors() {
    let sb = Sandbox::new();
    let run = |args: &[&str]| {
        let out = sb
            .mcc()
            .env_remove("NO_COLOR")
            .env("CLICOLOR_FORCE", "1")
            .args(args)
            .assert()
            .code(2)
            .get_output()
            .stderr
            .clone();
        String::from_utf8(out).unwrap()
    };
    // CLICOLOR_FORCE makes clap color even without a TTY…
    assert!(run(&["bogus"]).contains('\x1b'));
    // …but --no-color wins, for usage errors and help alike.
    let err = run(&["--no-color", "bogus"]);
    assert!(!err.contains('\x1b'), "{err:?}");
    assert!(err.contains("unrecognized subcommand"));
    let help = sb
        .mcc()
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .args(["--no-color", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!String::from_utf8(help).unwrap().contains('\x1b'));
}
