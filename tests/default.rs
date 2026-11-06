//! `mcc default`: choose the profile plain `claude` and `mcc run` start,
//! without moving any profile's data.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{Sandbox, by_name, json, stdout_of, with_profile};
use predicates::prelude::*;

const BEGIN: &str = "# >>> mcc:default >>>";

/// A sandbox with ~/.claude and two profiles, `work` and `lab`.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--fresh");
    sb
}

/// `mcc default` (show): what it prints on stdout.
fn shown_default(sb: &Sandbox) -> String {
    stdout_of(sb.mcc().arg("default"))
}

/// Every file under `rel` with its bytes, to tell that nothing changed.
fn snapshot(sb: &Sandbox, rel: &str) -> BTreeMap<String, Vec<u8>> {
    walk(&sb.path(rel))
}

fn walk(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                stack.push(p);
            } else if meta.is_file() {
                out.insert(p.display().to_string(), std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn backups(sb: &Sandbox) -> usize {
    std::fs::read_dir(sb.path(".mcc/backups")).map_or(0, |d| d.count())
}

/// `mcc list --json` (usage off).
fn list(sb: &Sandbox) -> Vec<serde_json::Value> {
    json(stdout_of(sb.mcc().args(["list", "--json", "--no-usage"])).as_bytes())
        .as_array()
        .unwrap()
        .clone()
}

/// `mcc run --dry-run` in `dir`: the profile it would start, and stderr.
fn run_in(sb: &Sandbox, dir: &Path) -> (String, String) {
    let out = sb
        .mcc()
        .env("MCC_NO_USAGE", "1")
        .args(["run", "--dry-run"])
        .current_dir(dir)
        .assert()
        .success()
        .get_output()
        .clone();
    (
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn set_show_and_reset_the_default_profile() {
    let sb = sandbox();
    let rc_before = sb.read(".zshrc");
    let default_before = snapshot(&sb, ".claude");
    let default_json = sb.read(".claude.json");
    let work_before = snapshot(&sb, ".mcc/profiles/work");
    assert_eq!(shown_default(&sb), "default\n");

    sb.mcc()
        .args(["default", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "the default profile is now `work` (was `default` (~/.claude))",
        ))
        .stderr(predicate::str::contains("`command claude`"))
        .stderr(predicate::str::contains("exec $SHELL"));
    assert_eq!(shown_default(&sb), "work\n");
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("default_profile = \"work\"\n"), "{cfg}");
    // The shell's `claude` runs work's launcher, only without CLAUDE_CONFIG_DIR
    // and without MCC_PROFILE (~/.claude chosen explicitly).
    let rc = sb.read(".zshrc");
    assert!(rc.starts_with(&rc_before), "{rc}");
    assert_eq!(rc.matches(BEGIN).count(), 1, "{rc}");
    assert!(
        rc.contains(
            "function claude {\n  if [ -z \"${CLAUDE_CONFIG_DIR:-}\" ] && [ -z \"${MCC_PROFILE:-}\" ]; then \"$HOME/.local/bin/work\" \"$@\"; else command claude \"$@\"; fi\n}\n# <<< mcc:default <<<\n"
        ),
        "{rc}"
    );

    // AC3: nothing moved and no data was touched.
    assert_eq!(snapshot(&sb, ".claude"), default_before);
    assert_eq!(sb.read(".claude.json"), default_json);
    assert_eq!(snapshot(&sb, ".mcc/profiles/work"), work_before);
    assert!(sb.exists(".mcc/profiles/work/.credentials.json"));

    // Back to ~/.claude: the key and the block go, the rc file is as it was.
    sb.mcc()
        .args(["default", "claude"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "the default profile is now `default` (~/.claude) (was `work`)",
        ));
    assert_eq!(shown_default(&sb), "default\n");
    assert!(!sb.read(".mcc/config.toml").contains("default_profile"));
    assert_eq!(sb.read(".zshrc"), rc_before);
    assert_eq!(snapshot(&sb, ".claude"), default_before);
    assert_eq!(snapshot(&sb, ".mcc/profiles/work"), work_before);
}

#[test]
fn an_unknown_profile_exits_2_and_changes_nothing() {
    let sb = sandbox();
    for setup in [None, Some("work")] {
        if let Some(p) = setup {
            sb.mcc().args(["default", p]).assert().success();
        }
        let (cfg, rc, n) = (sb.read(".mcc/config.toml"), sb.read(".zshrc"), backups(&sb));
        sb.mcc()
            .args(["default", "ghost"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("no profile named \"ghost\""));
        assert_eq!(sb.read(".mcc/config.toml"), cfg);
        assert_eq!(sb.read(".zshrc"), rc);
        assert_eq!(backups(&sb), n);
    }
}

#[test]
fn the_profile_that_is_already_the_default_changes_nothing() {
    let sb = sandbox();
    for name in ["default", "work", "work"] {
        if name == "work" && !sb.read(".mcc/config.toml").contains("default_profile") {
            sb.mcc().args(["default", "work"]).assert().success();
        }
        let (cfg, rc, n) = (sb.read(".mcc/config.toml"), sb.read(".zshrc"), backups(&sb));
        sb.mcc()
            .args(["default", name])
            .assert()
            .success()
            .stderr(predicate::str::contains(
                "is already the default profile — nothing changed",
            ));
        assert_eq!(sb.read(".mcc/config.toml"), cfg);
        assert_eq!(sb.read(".zshrc"), rc);
        assert_eq!(backups(&sb), n, "no rc backup: nothing was written");
    }
}

#[test]
fn the_already_default_profile_repairs_a_missing_shell_block() {
    let sb = sandbox();
    let rc_before = sb.read(".zshrc");
    sb.mcc().args(["default", "work"]).assert().success();
    let with_block = sb.read(".zshrc");
    sb.write(".zshrc", &rc_before);
    let cfg = sb.read(".mcc/config.toml");

    // doctor sees it, and its fix puts the block back.
    let checks =
        json(stdout_of(sb.mcc().env("MCC_NO_USAGE", "1").args(["doctor", "--json"])).as_bytes());
    let f = checks
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "default_profile")
        .expect("a default_profile finding");
    assert_eq!(f["status"], "warn", "{f}");
    assert_eq!(f["fix"], "mcc default work", "{f}");

    sb.mcc()
        .args(["default", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "repaired the shell's `claude` command",
        ));
    assert_eq!(sb.read(".zshrc"), with_block);
    assert_eq!(sb.read(".mcc/config.toml"), cfg);
}

#[test]
fn a_project_default_beats_the_global_default_which_beats_claude() {
    let sb = sandbox();
    let app = sb.path("code/app");
    std::fs::create_dir_all(&app).unwrap();
    sb.mcc()
        .args(["project", "set", "lab", "--path"])
        .arg(&app)
        .assert()
        .success();
    let home = sb.home().to_path_buf();

    // No global default yet: ~/.claude outside the project.
    let (name, err) = run_in(&sb, &home);
    assert_eq!(name, "default");
    assert!(err.contains("no project default here"), "{err}");

    sb.mcc().args(["default", "work"]).assert().success();
    let (name, err) = run_in(&sb, &home);
    assert_eq!(name, "work");
    assert!(err.contains("the global default (`mcc default`)"), "{err}");
    assert_eq!(run_in(&sb, &app).0, "lab");
    // A profile picked by name, `default` included, is still that profile.
    let picked = stdout_of(sb.mcc().env("MCC_NO_USAGE", "1").args([
        "run",
        "--dry-run",
        "--profile",
        "default",
    ]));
    assert_eq!(picked.trim(), "default");

    // `mcc env` agrees.
    let env = |dir: &Path| stdout_of(sb.mcc().args(["env", "--shell", "zsh"]).current_dir(dir));
    let work_dir = sb.path(".mcc/profiles/work");
    assert!(
        env(&home).contains(&format!(
            "export CLAUDE_CONFIG_DIR='{}'",
            work_dir.display()
        )),
        "{}",
        env(&home)
    );
    assert!(env(&app).contains(".mcc/profiles/lab'"), "{}", env(&app));

    // And so does the status view.
    let status = stdout_of(sb.mcc().env("MCC_NO_USAGE", "1"));
    assert!(
        status.starts_with("Here: `work` — the global default (`mcc default`)\n"),
        "{status}"
    );
    let status = stdout_of(sb.mcc().env("MCC_NO_USAGE", "1").current_dir(&app));
    assert!(
        status.starts_with("Here: `lab` — the default for "),
        "{status}"
    );

    sb.mcc().args(["default", "default"]).assert().success();
    assert_eq!(run_in(&sb, &home).0, "default");
    assert!(env(&home).contains("unset CLAUDE_CONFIG_DIR"));
}

#[test]
fn list_usage_and_status_mark_the_default_profile() {
    let sb = sandbox();
    let all = list(&sb);
    assert_eq!(by_name(&all, "default")["is_global_default"], true);
    assert_eq!(by_name(&all, "work")["is_global_default"], false);

    sb.mcc().args(["default", "work"]).assert().success();
    let all = list(&sb);
    let (default, work, lab) = (
        by_name(&all, "default"),
        by_name(&all, "work"),
        by_name(&all, "lab"),
    );
    // `is_default` keeps meaning ~/.claude.
    assert_eq!(default["is_default"], true);
    assert_eq!(default["is_global_default"], false);
    assert_eq!(work["is_default"], false);
    assert_eq!(work["is_global_default"], true);
    assert_eq!(lab["is_global_default"], false);

    let usage =
        json(stdout_of(sb.mcc().env("MCC_NO_USAGE", "1").args(["usage", "--json"])).as_bytes());
    let usage = usage.as_array().unwrap();
    assert_eq!(by_name(usage, "default")["is_default"], true);
    assert_eq!(by_name(usage, "default")["is_global_default"], false);
    assert_eq!(by_name(usage, "work")["is_global_default"], true);

    let table = stdout_of(sb.mcc().args(["list", "--no-usage"]));
    assert!(table.contains("\nwork ★"), "{table}");
    assert!(!table.contains("default ★"), "{table}");
    let long = stdout_of(sb.mcc().args(["list", "--long", "--no-usage"]));
    assert!(long.contains("◆ default (~/.claude)\n"), "{long}");
    assert!(long.contains("◆ work ★ (default profile)\n"), "{long}");
    let status = stdout_of(sb.mcc().env("MCC_NO_USAGE", "1"));
    assert!(status.contains("\nwork ★"), "{status}");
}

#[test]
fn rename_moves_the_default_to_the_new_name() {
    let sb = sandbox();
    sb.mcc().args(["default", "work"]).assert().success();
    sb.mcc()
        .args(["rename", "work", "job"])
        .assert()
        .success()
        .stderr(predicate::str::contains("the default profile is now `job`"));
    assert_eq!(shown_default(&sb), "job\n");
    let rc = sb.read(".zshrc");
    assert_eq!(rc.matches(BEGIN).count(), 1, "{rc}");
    assert!(rc.contains("\"$HOME/.local/bin/job\" \"$@\"; else"), "{rc}");
    assert!(!rc.contains(".local/bin/work"), "{rc}");
    assert!(
        sb.read(".mcc/config.toml")
            .contains("default_profile = \"job\"")
    );
    assert_eq!(by_name(&list(&sb), "job")["is_global_default"], true);

    // Renaming another profile leaves the default alone.
    let rc = sb.read(".zshrc");
    sb.mcc().args(["rename", "lab", "lab2"]).assert().success();
    assert_eq!(shown_default(&sb), "job\n");
    let after = sb.read(".zshrc");
    let block = |t: &str| {
        t[t.find(BEGIN).unwrap()..]
            .split("# <<<")
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(block(&after), block(&rc));
}

#[test]
fn delete_goes_back_to_claude_when_the_default_is_deleted() {
    let sb = sandbox();
    let default_before = snapshot(&sb, ".claude");
    sb.mcc().args(["default", "work"]).assert().success();
    sb.mcc().args(["delete", "lab", "--yes"]).assert().success();
    assert_eq!(shown_default(&sb), "work\n");
    assert!(sb.read(".zshrc").contains(BEGIN));

    sb.mcc()
        .args(["delete", "work", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("it was the default profile"))
        .stderr(predicate::str::contains("the default is ~/.claude again"));
    assert_eq!(shown_default(&sb), "default\n");
    assert!(!sb.read(".mcc/config.toml").contains("default_profile"));
    assert!(!sb.read(".zshrc").contains("mcc:default"));
    assert_eq!(snapshot(&sb, ".claude"), default_before);
    assert_eq!(run_in(&sb, sb.home()).0, "default");
}

#[test]
fn a_default_naming_no_profile_is_flagged_and_reset() {
    let sb = sandbox();
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &format!("default_profile = \"ghost\"\n{cfg}"),
    );

    let out = sb
        .mcc()
        .env("MCC_NO_USAGE", "1")
        .args(["doctor", "--json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let checks = json(&out);
    let f = checks
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "default_profile")
        .expect("a default_profile finding");
    assert_eq!(f["status"], "problem", "{f}");
    assert_eq!(f["fix"], "mcc default default", "{f}");

    sb.mcc()
        .env("MCC_NO_USAGE", "1")
        .args(["run", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the global default is \"ghost\", which is not a profile",
        ))
        .stderr(predicate::str::contains("mcc default default"));
    sb.mcc()
        .env("MCC_NO_USAGE", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Here: `ghost` — the global default",
        ))
        .stderr(predicate::str::contains("the global default is \"ghost\""));

    sb.mcc().args(["default", "default"]).assert().success();
    assert_eq!(shown_default(&sb), "default\n");
    assert_eq!(run_in(&sb, sb.home()).0, "default");
}

#[test]
fn doctor_reports_nothing_new_until_a_default_is_chosen() {
    let sb = sandbox();
    let checks = |sb: &Sandbox| -> Vec<serde_json::Value> {
        let out = sb
            .mcc()
            .env("MCC_NO_USAGE", "1")
            .args(["doctor", "--json"])
            .output()
            .unwrap()
            .stdout;
        json(&out)
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["check"] == "default_profile")
            .cloned()
            .collect()
    };
    assert!(checks(&sb).is_empty());
    sb.mcc().args(["default", "work"]).assert().success();
    let found = checks(&sb);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["status"], "ok", "{found:?}");

    // A block left behind with ~/.claude the default is flagged.
    let rc = sb.read(".zshrc");
    sb.mcc().args(["default", "default"]).assert().success();
    sb.write(".zshrc", &rc);
    let found = checks(&sb);
    assert_eq!(found[0]["status"], "warn", "{found:?}");
    assert_eq!(found[0]["fix"], "mcc default default", "{found:?}");
    sb.mcc()
        .args(["default", "default"])
        .assert()
        .success()
        .stderr(predicate::str::contains("repaired"));
    assert!(!sb.read(".zshrc").contains(BEGIN));
}

/// The block, sourced by bash: plain `claude` starts the default profile's
/// launcher; `command claude` and a shell already pointed at a profile
/// start claude as they always did.
#[test]
fn the_shell_function_starts_the_default_only_without_a_config_dir() {
    let sb = sandbox();
    sb.mcc().args(["default", "work"]).assert().success();
    let rc = sb.read(".zshrc");
    let start = rc.find(BEGIN).unwrap();
    sb.write("block.sh", &rc[start..]);
    let config_dir = |script: &str, preset: Option<&str>| -> String {
        let mut cmd = sb.command("/bin/bash");
        cmd.arg("-c")
            .arg(format!(". \"$HOME/block.sh\"; {script}"))
            .env("FAKE_CONFIG_DIRECTORY", "1")
            .env("MCC_NO_USAGE", "1");
        if let Some(dir) = preset {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{out:?}");
        let v = json(&out.stdout);
        v["configDirectory"].as_str().unwrap().to_string()
    };
    let work = sb.path(".mcc/profiles/work").display().to_string();
    assert_eq!(config_dir("claude auth status", None), work);
    assert_eq!(
        config_dir("command claude auth status", None),
        sb.path(".claude").display().to_string()
    );
    let lab = sb.path(".mcc/profiles/lab").display().to_string();
    assert_eq!(config_dir("claude auth status", Some(&lab)), lab);
}

/// With a global default set, a project whose default is ~/.claude
/// (`mcc project set default`) keeps plain `claude` on ~/.claude once
/// `eval "$(mcc env)"` (or direnv) has run there: the project default beats
/// the global default, as it does for `mcc run`.
#[test]
fn the_shell_function_keeps_a_project_default_of_claude() {
    let sb = sandbox();
    sb.mcc().args(["default", "work"]).assert().success();
    let (claude_app, lab_app) = (sb.path("code/claude"), sb.path("code/lab"));
    for (dir, profile) in [(&claude_app, "default"), (&lab_app, "lab")] {
        std::fs::create_dir_all(dir).unwrap();
        sb.mcc()
            .args(["project", "set", profile, "--path"])
            .arg(dir)
            .assert()
            .success();
    }
    let rc = sb.read(".zshrc");
    sb.write("block.sh", &rc[rc.find(BEGIN).unwrap()..]);
    // `eval "$(mcc env)"` in `dir`, then plain `claude` in the same shell.
    let config_dir = |dir: &Path| -> String {
        let env = stdout_of(sb.mcc().args(["env", "--shell", "bash"]).current_dir(dir));
        let out = sb
            .command("/bin/bash")
            .arg("-c")
            .arg(format!(". \"$HOME/block.sh\"\n{env}\nclaude auth status"))
            .env("FAKE_CONFIG_DIRECTORY", "1")
            .env("MCC_NO_USAGE", "1")
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        json(&out.stdout)["configDirectory"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let path = |rel: &str| sb.path(rel).display().to_string();
    assert_eq!(config_dir(&claude_app), path(".claude"));
    assert_eq!(config_dir(&lab_app), path(".mcc/profiles/lab"));
    // No project default: the global default.
    assert_eq!(config_dir(sb.home()), path(".mcc/profiles/work"));

    // `mcc exec`/`mcc run` as ~/.claude say so too, for a shell they start.
    let marker = |profile: &str| {
        stdout_of(sb.mcc().args([
            "exec",
            profile,
            "--",
            "/bin/sh",
            "-c",
            "printf %s \"${MCC_PROFILE-unset}\"",
        ]))
    };
    assert_eq!(marker("default"), "default");
    assert_eq!(marker("lab"), "unset");
}
