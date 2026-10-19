//! Per-project profiles: `mcc project` and `mcc run`.

mod common;

use std::path::PathBuf;

use common::{HOUR_MS, Sandbox, UsageStub, json, now_ms, rfc3339_in, stdout_of, with_profile};
use predicates::prelude::*;

/// default, `work`, `side` and `lab`, each logged in with its own token, and
/// a git repository at ~/code/app.
fn run_sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    for alias in ["work", "side", "lab"] {
        with_profile(&sb, alias, "--fresh");
    }
    for (dir, token) in [
        (".claude", "tok-default"),
        (".mcc/profiles/work", "tok-work"),
        (".mcc/profiles/side", "tok-side"),
        (".mcc/profiles/lab", "tok-lab"),
    ] {
        sb.seed_oauth(dir, token, now_ms() + HOUR_MS);
        let state = if dir == ".claude" {
            ".claude.json".to_string()
        } else {
            format!("{dir}/.claude.json")
        };
        sb.write(
            &state,
            &format!(r#"{{"oauthAccount":{{"emailAddress":"{token}@example.com"}}}}"#),
        );
    }
    sb.write("code/app/.git/HEAD", "ref: refs/heads/main\n");
    sb.write("code/app/src/main.rs", "");
    sb
}

/// The directory as mcc stores it (symlinks resolved: /var → /private/var).
fn real(sb: &Sandbox, rel: &str) -> PathBuf {
    std::fs::canonicalize(sb.path(rel)).unwrap()
}

/// Answers each token with its (5-hour, weekly) percent used; unknown tokens get 401.
fn stub(usage: &[(&str, f64, f64)]) -> UsageStub {
    let usage: Vec<(String, f64, f64)> = usage
        .iter()
        .map(|(t, f, w)| (format!("Bearer {t}"), *f, *w))
        .collect();
    let (five, week) = (rfc3339_in(35 * 60), rfc3339_in(3 * 86400 + 4 * 3600));
    UsageStub::start(move |req| {
        match usage
            .iter()
            .find(|(t, _, _)| req.authorization.as_deref() == Some(t.as_str()))
        {
            Some((_, f, w)) => (
                200,
                format!(
                    r#"{{"five_hour":{{"utilization":{f},"resets_at":"{five}"}},"seven_day":{{"utilization":{w},"resets_at":"{week}"}}}}"#
                ),
            ),
            None => (401, "{}".into()),
        }
    })
}

/// `mcc run --dry-run` in ~/code/app/src: (stdout, stderr).
fn dry_run(sb: &Sandbox, stub: &UsageStub) -> (String, String) {
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .current_dir(sb.path("code/app/src"))
        .args(["run", "--dry-run"])
        .assert()
        .success();
    let out = o.get_output();
    (
        String::from_utf8(out.stdout.clone()).unwrap(),
        String::from_utf8(out.stderr.clone()).unwrap(),
    )
}

fn set_project(sb: &Sandbox, profile: &str) {
    sb.mcc()
        .current_dir(sb.path("code/app/src"))
        .args(["project", "set", profile])
        .assert()
        .success();
}

#[test]
fn project_set_list_and_unset() {
    let sb = run_sandbox();
    let app = real(&sb, "code/app");
    // From a subdirectory, the project is the top of the git repository.
    sb.mcc()
        .current_dir(sb.path("code/app/src"))
        .args(["project", "set", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "{}: `mcc run` starts `work` here",
            app.display()
        )));
    let cfg = sb.read(".mcc/config.toml");
    assert!(
        cfg.contains(&format!(
            "[projects.\"{}\"]\nprofile = \"work\"\n",
            app.display()
        )),
        "{cfg}"
    );
    // Setting it again replaces it; `claude` names the default profile.
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["project", "set", "claude"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "starts `default` here (was `work`)",
        ));
    sb.mcc()
        .args(["project", "set", "lab", "--path"])
        .arg(sb.path("code"))
        .assert()
        .success();

    let list = stdout_of(
        sb.mcc()
            .current_dir(sb.path("code/app/src"))
            .args(["project", "list"]),
    );
    let code = real(&sb, "code");
    let rows: Vec<Vec<&str>> = list
        .lines()
        .map(|l| l.split_whitespace().collect())
        .collect();
    let (code_s, app_s) = (code.to_string_lossy(), app.to_string_lossy());
    assert_eq!(
        rows,
        [
            vec!["PROJECT", "PROFILE"],
            vec![&*code_s, "lab"],
            vec![&*app_s, "default", "←", "here"],
        ],
        "{list}"
    );
    let entries = json(stdout_of(sb.mcc().args(["project", "list", "--json"])).as_bytes());
    assert_eq!(
        entries,
        serde_json::json!([
            {"path": code.to_string_lossy(), "profile": "lab"},
            {"path": app.to_string_lossy(), "profile": "default"},
        ])
    );

    // Unset removes the deepest project that applies here, then the next.
    sb.mcc()
        .current_dir(sb.path("code/app/src"))
        .args(["project", "unset"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "{}: removed its project default (`default`)",
            app.display()
        )));
    sb.mcc()
        .current_dir(sb.path("code/app/src"))
        .args(["project", "unset"])
        .assert()
        .success();
    sb.mcc()
        .current_dir(sb.path("code/app/src"))
        .args(["project", "unset"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no project default applies"));
    assert!(!sb.read(".mcc/config.toml").contains("[projects"));
}

#[test]
fn project_set_rejects_unknown_profiles_and_directories() {
    let sb = run_sandbox();
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["project", "set", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"nope\""));
    sb.mcc()
        .args(["project", "set", "work", "--path"])
        .arg(sb.path("code/missing"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a directory"));
    sb.mcc()
        .args(["project", "unset", "--path"])
        .arg(sb.path("code/app"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("has no project default"));
    assert!(!sb.read(".mcc/config.toml").contains("[projects"));
}

#[test]
fn run_starts_the_project_default_and_only_asks_for_its_usage() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    let stub = stub(&[
        ("tok-work", 42.0, 18.0),
        ("tok-side", 0.0, 0.0),
        ("tok-default", 0.0, 0.0),
    ]);
    let (out, err) = dry_run(&sb, &stub);
    assert_eq!(out, "work\n");
    assert!(
        err.contains("would start `work` — the default for")
            && err.contains("(5h 42%, week 18% used)"),
        "{err}"
    );
    // Below its limit, no other profile's usage is fetched.
    assert_eq!(stub.tokens(), ["tok-work"]);
}

#[test]
fn run_at_the_limit_lists_every_profile_and_never_switches_on_its_own() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    let stub = stub(&[
        ("tok-work", 100.0, 40.0),
        ("tok-default", 70.0, 10.0),
        ("tok-side", 20.0, 35.0),
        ("tok-lab", 1.0, 1.0),
    ]);
    let (out, err) = dry_run(&sb, &stub);
    // The project's profile, never another one picked for you.
    assert_eq!(out, "work\n");
    for line in [
        "is at its 5-hour limit, resets in 35m",
        "● default — 5h 70%, week 10% used",
        "● lab — 5h 1%, week 1% used",
        "● side — 5h 20%, week 35% used",
        "`mcc run` would ask which profile to start; start a profile with `mcc run --profile <name>`, or wait until `work` resets",
    ] {
        assert!(err.contains(line), "missing {line:?} in:\n{err}");
    }
    assert!(err.contains("`work` (the default for "), "{err}");
    assert_eq!(
        stub.tokens(),
        ["tok-default", "tok-lab", "tok-side", "tok-work"]
    );
}

#[test]
fn run_at_the_limit_without_a_terminal_stops_and_names_profile_flag() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    let s = stub(&[("tok-work", 10.0, 100.0), ("tok-lab", 50.0, 60.0)]);
    sb.mcc()
        .env("MCC_USAGE_URL", &s.url)
        .current_dir(sb.path("code/app"))
        .args(["run", "--", "--resume"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "is at its weekly limit, resets in 3d 4h",
        ))
        .stderr(predicate::str::contains("● lab — 5h 50%, week 60% used"))
        .stderr(predicate::str::contains("no terminal is attached"))
        .stderr(predicate::str::contains("mcc run --profile <name>"));
}

#[test]
fn run_profile_flag_starts_that_profile_without_checking_usage() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    let s = stub(&[("tok-work", 100.0, 100.0), ("tok-lab", 0.0, 0.0)]);
    let pick = |name: &str| {
        stdout_of(
            sb.mcc()
                .env("MCC_USAGE_URL", &s.url)
                .current_dir(sb.path("code/app"))
                .args(["run", "--dry-run", "--profile", name]),
        )
    };
    assert_eq!(pick("lab"), "lab\n");
    assert_eq!(pick("claude"), "default\n");
    assert!(s.requests().is_empty());
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["run", "--profile", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"nope\""));
}

#[test]
fn run_starts_the_default_when_its_usage_is_unknown() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    sb.seed_oauth(".mcc/profiles/work", "tok-work", now_ms() - 1000);
    let s = stub(&[("tok-side", 0.0, 0.0)]);
    let (out, err) = dry_run(&sb, &s);
    assert_eq!(out, "work\n");
    assert!(err.contains("usage unknown: token expired"), "{err}");
    assert!(s.requests().is_empty());

    // MCC_NO_USAGE: nothing is fetched at all.
    sb.seed_oauth(".mcc/profiles/work", "tok-work", now_ms() + HOUR_MS);
    let s = stub(&[("tok-work", 100.0, 100.0)]);
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &s.url)
        .env("MCC_NO_USAGE", "1")
        .current_dir(sb.path("code/app"))
        .args(["run", "--dry-run"])
        .assert()
        .success();
    assert_eq!(String::from_utf8_lossy(&o.get_output().stdout), "work\n");
    assert!(s.requests().is_empty());
}

#[test]
fn run_outside_a_project_uses_the_default_profile() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    let s = stub(&[("tok-default", 5.0, 5.0)]);
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &s.url)
        .current_dir(sb.path("code"))
        .args(["run", "--dry-run"])
        .assert()
        .success();
    let out = o.get_output();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "default\n");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("would start `default` — no project default here"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_execs_claude_with_the_profiles_config_dir() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    sb.write(
        "bin/claude-env",
        "#!/bin/sh\necho \"CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR-unset} args=$*\"\n",
    );
    common::chmod_x(&sb.path("bin/claude-env"));
    let run = |dir: &str, args: &[&str]| {
        stdout_of(
            sb.mcc()
                .env("MCC_CLAUDE_BIN", sb.path("bin/claude-env"))
                .env("MCC_NO_USAGE", "1")
                .env("CLAUDE_CONFIG_DIR", "/somewhere/else")
                .current_dir(sb.path(dir))
                .arg("run")
                .args(args),
        )
    };
    // The registered directory, exactly: the keychain login is keyed on it.
    assert_eq!(
        run("code/app/src", &["--", "--resume", "two words"]),
        format!(
            "CLAUDE_CONFIG_DIR={} args=--resume two words\n",
            sb.path(".mcc/profiles/work").display()
        )
    );
    // Arguments after the first one that isn't mcc's go to claude as well.
    assert!(run("code/app", &["-c"]).ends_with(" args=-c\n"));
    // The default profile runs with CLAUDE_CONFIG_DIR unset.
    assert_eq!(run("code", &[]), "CLAUDE_CONFIG_DIR=unset args=\n");
    assert_eq!(
        run("code/app", &["--profile", "lab", "--", "-c"]),
        format!(
            "CLAUDE_CONFIG_DIR={} args=-c\n",
            sb.path(".mcc/profiles/lab").display()
        )
    );
}

#[test]
fn run_rejects_a_project_default_that_is_not_a_profile() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &cfg.replace("profile = \"work\"", "profile = \"gone\""),
    );
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["run", "--dry-run"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "is \"gone\", which is not a profile",
        ));
}

#[test]
fn rename_and_delete_keep_project_defaults_in_step() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    sb.mcc()
        .args(["rename", "work", "job"])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 project default now uses `job`"));
    assert!(sb.read(".mcc/config.toml").contains("profile = \"job\""));

    sb.mcc()
        .args(["delete", "job", "--yes", "--keep-auth"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "removed it as the project default of {}",
            real(&sb, "code/app").display()
        )));
    assert!(!sb.read(".mcc/config.toml").contains("[projects"));
}

#[test]
fn project_and_run_are_reserved_names() {
    let sb = Sandbox::new();
    sb.seed_default();
    for name in ["project", "run"] {
        sb.mcc()
            .args(["add", name, "--fresh", "--no-login"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("reserved by mcc"));
    }
}

#[test]
fn list_shows_which_profile_each_project_starts() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    sb.write("code/old/x", "");
    for (profile, dir) in [("default", "code"), ("lab", "code/old")] {
        sb.mcc()
            .args(["project", "set", profile, "--path"])
            .arg(sb.path(dir))
            .assert()
            .success();
    }
    std::fs::remove_dir_all(sb.path("code/old")).unwrap();
    let (code, app) = (real(&sb, "code"), real(&sb, "code/app"));
    let old = code.join("old");

    let list = |args: &[&str]| {
        stdout_of(
            sb.mcc()
                .current_dir(sb.path("code/app/src"))
                .arg("list")
                .args(["--no-usage"])
                .args(args),
        )
    };
    // Under the profile table: each project, its profile and account.
    let out = list(&[]);
    let section = &out[out.find("PROJECT").expect(&out)..];
    let rows: Vec<Vec<&str>> = section
        .lines()
        .map(|l| l.split_whitespace().collect())
        .collect();
    let (code_s, app_s, old_s) = (
        code.to_string_lossy(),
        app.to_string_lossy(),
        old.to_string_lossy(),
    );
    assert_eq!(
        rows,
        [
            vec!["PROJECT", "PROFILE", "ACCOUNT"],
            vec![&*code_s, "default", "tok-default@example.com"],
            vec![&*app_s, "work", "tok-work@example.com", "←", "here"],
            vec![&*old_s, "(missing)", "lab", "tok-lab@example.com"],
        ],
        "{out}"
    );

    // --long: a `Default for` row in the profile's card.
    let long = list(&["--long"]);
    let work = &long[long.find("◆ work").expect(&long)..];
    assert!(
        work.contains(&format!("    Default for:     {}\n", app.display())),
        "{long}"
    );

    // --json: the project directories of each profile.
    let all = json(list(&["--json"]).as_bytes());
    let defaults =
        |name: &str| common::by_name(all.as_array().unwrap(), name)["project_defaults"].clone();
    assert_eq!(defaults("work"), serde_json::json!([app_s]));
    assert_eq!(defaults("default"), serde_json::json!([code_s]));
    assert_eq!(defaults("side"), serde_json::json!([]));

    // A project naming a profile that no longer exists says so.
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &cfg.replace("profile = \"lab\"", "profile = \"gone\""),
    );
    assert!(list(&[]).contains("gone (no such profile)"));
}
