//! `mcc env` (issue #42) and `mcc exec` (issue #43).

mod common;

use common::{Sandbox, stdout_of, with_profile};
use predicates::prelude::*;

/// default, `work` and `lab`, and a git repository at ~/code/app whose
/// project default is `work`.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    for alias in ["work", "lab"] {
        with_profile(&sb, alias, "--fresh");
    }
    sb.write("code/app/.git/HEAD", "ref: refs/heads/main\n");
    sb.write("code/app/src/main.rs", "");
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["project", "set", "work"])
        .assert()
        .success();
    sb
}

fn dir(sb: &Sandbox, alias: &str) -> String {
    sb.path(&format!(".mcc/profiles/{alias}"))
        .display()
        .to_string()
}

/// A script under ~/bin that prints CLAUDE_CONFIG_DIR and its arguments, then
/// exits 3 as `lab`, else with `$PROBE_EXIT` (0 when unset).
fn probe(sb: &Sandbox) -> String {
    sb.write(
        "bin/probe",
        "#!/bin/sh\necho \"CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR-unset} args=$*\"\n\
         case \"${CLAUDE_CONFIG_DIR-unset}\" in */lab) exit 3;; esac\nexit \"${PROBE_EXIT:-0}\"\n",
    );
    common::chmod_x(&sb.path("bin/probe"));
    sb.path("bin/probe").display().to_string()
}

#[test]
fn env_prints_the_projects_profile_or_a_named_one() {
    let sb = sandbox();
    let env = |cwd: &str, args: &[&str]| {
        stdout_of(sb.mcc().current_dir(sb.path(cwd)).arg("env").args(args))
    };
    let work = format!("export CLAUDE_CONFIG_DIR='{}'\n", dir(&sb, "work"));
    assert_eq!(env("code/app/src", &[]), work);
    // Outside every project: the default profile, which runs with it unset.
    assert_eq!(env("code", &[]), "unset CLAUDE_CONFIG_DIR\n");
    assert_eq!(
        env("code", &["lab"]),
        format!("export CLAUDE_CONFIG_DIR='{}'\n", dir(&sb, "lab"))
    );
    assert_eq!(env("code/app", &["claude"]), "unset CLAUDE_CONFIG_DIR\n");
    sb.mcc()
        .args(["env", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"nope\""));
}

#[test]
fn env_prints_the_fish_form_for_fish() {
    let sb = sandbox();
    let fish = format!("set -gx CLAUDE_CONFIG_DIR '{}'\n", dir(&sb, "work"));
    let app = sb.path("code/app");
    assert_eq!(
        stdout_of(
            sb.mcc()
                .current_dir(&app)
                .env("SHELL", "/usr/bin/fish")
                .arg("env")
        ),
        fish
    );
    assert_eq!(
        stdout_of(sb.mcc().current_dir(&app).args(["env", "--shell", "fish"])),
        fish
    );
    assert_eq!(
        stdout_of(sb.mcc().args(["env", "default", "--shell", "fish"])),
        "set -e CLAUDE_CONFIG_DIR\n"
    );
    // direnv evaluates .envrc with bash, whatever the login shell is.
    assert!(
        stdout_of(
            sb.mcc()
                .current_dir(&app)
                .env("SHELL", "/usr/bin/fish")
                .env("DIRENV_IN_ENVRC", "1")
                .arg("env")
        )
        .starts_with("export CLAUDE_CONFIG_DIR='")
    );
}

#[test]
fn env_output_survives_eval_with_spaces_and_quotes_in_the_path() {
    let sb = sandbox();
    let odd = sb.path("it's a $HOME dir").display().to_string();
    let cfg = sb.read(".mcc/config.toml");
    let lab = format!("\"{}\"", dir(&sb, "lab"));
    assert!(cfg.contains(&lab), "{cfg}");
    sb.write(
        ".mcc/config.toml",
        &cfg.replace(&lab, &format!("\"{odd}\"")),
    );
    let line = stdout_of(sb.mcc().args(["env", "lab"]));
    let out = std::process::Command::new("sh")
        .args([
            "-c",
            "eval \"$1\"; printf %s \"$CLAUDE_CONFIG_DIR\"",
            "sh",
            &line,
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), odd);
}

#[test]
fn exec_runs_the_command_as_a_profile_and_returns_its_exit_status() {
    let sb = sandbox();
    let probe = probe(&sb);
    sb.mcc()
        .env("CLAUDE_CONFIG_DIR", "/somewhere/else")
        .args(["exec", "work", "--", &probe, "mcp", "list"])
        .assert()
        .success()
        .stdout(format!(
            "CLAUDE_CONFIG_DIR={} args=mcp list\n",
            dir(&sb, "work")
        ));
    sb.mcc()
        .env("CLAUDE_CONFIG_DIR", "/somewhere/else")
        .env("PROBE_EXIT", "7")
        .args(["exec", "default", "--", &probe])
        .assert()
        .code(7)
        .stdout("CLAUDE_CONFIG_DIR=unset args=\n");
    sb.mcc()
        .args(["exec", "work", "--", "no-such-command-mcc"])
        .assert()
        .code(127);
    // PROFILE or --all, and the command after `--`.
    sb.mcc().args(["exec", "--", &probe]).assert().code(2);
    sb.mcc().args(["exec", "work"]).assert().code(2);
}

#[test]
fn exec_all_runs_once_per_profile_and_reports_each_result() {
    let sb = sandbox();
    let probe = probe(&sb);
    let out = sb
        .mcc()
        .args(["exec", "--all", "--", &probe, "x"])
        .assert()
        .code(3);
    let out = out.get_output();
    assert_eq!(
        String::from_utf8(out.stdout.clone()).unwrap(),
        format!(
            "CLAUDE_CONFIG_DIR=unset args=x\nCLAUDE_CONFIG_DIR={} args=x\nCLAUDE_CONFIG_DIR={} args=x\n",
            dir(&sb, "lab"),
            dir(&sb, "work")
        )
    );
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    for line in ["`default`: exit 0", "`lab`: exit 3", "`work`: exit 0"] {
        assert!(stderr.contains(line), "{stderr}");
    }
    assert!(
        stderr.contains("failed in 1 of 3 profiles: lab (exit 3)"),
        "{stderr}"
    );

    // Everywhere fine: exit 0.
    sb.mcc()
        .args(["exec", "--all", "--", "true"])
        .assert()
        .success()
        .stderr(predicate::str::contains("succeeded in all 3 profiles"));
}

#[test]
fn exec_holds_no_lock_so_its_command_can_change_the_config() {
    let sb = sandbox();
    let mcc = env!("CARGO_BIN_EXE_mcc");
    let path = sb.path("code/app").display().to_string();
    // Would deadlock (or fail on the lock) if `mcc exec` held ~/.mcc/.lock.
    sb.mcc()
        .timeout(std::time::Duration::from_secs(30))
        .args([
            "exec", "--all", "--", mcc, "project", "set", "lab", "--path", &path,
        ])
        .assert()
        .success();
    assert!(sb.read(".mcc/config.toml").contains("profile = \"lab\""));
}

#[test]
fn exec_all_stops_at_ctrl_c_even_when_the_command_handles_it() {
    use std::os::unix::process::CommandExt;
    let sb = sandbox();
    // Exits 0 on Ctrl+C, as programs that catch SIGINT do.
    sb.write(
        "bin/trap",
        "#!/bin/sh\ntrap 'sleep 0.3; exit 0' INT\necho \"ran ${CLAUDE_CONFIG_DIR-unset}\" >> \"$HOME/ran\"\n\
         touch \"$HOME/ready\"\nsleep 5 & wait\n",
    );
    common::chmod_x(&sb.path("bin/trap"));
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_mcc"));
    cmd.env_clear()
        .env("HOME", sb.home())
        .env("MCC_HOME", sb.home())
        .env("PATH", "/usr/bin:/bin")
        .env("MCC_NON_INTERACTIVE", "1")
        .current_dir(sb.home())
        .args(["exec", "--all", "--"])
        .arg(sb.path("bin/trap"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let mut child = cmd.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !sb.exists("ready") {
        assert!(
            std::time::Instant::now() < deadline,
            "the command never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Ctrl+C in a terminal signals the whole foreground process group.
    // SAFETY: plain kill(2) on the process group the test just created.
    assert_eq!(unsafe { libc::kill(-(child.id() as i32), libc::SIGINT) }, 0);
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert_eq!(sb.read("ran"), "ran unset\n", "went on to the next profile");
}
