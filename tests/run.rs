//! Per-project profiles: `mcc project` and `mcc run`.

mod common;

use std::path::{Path, PathBuf};

use common::{HOUR_MS, Sandbox, UsageStub, json, now_ms, rfc3339_in, stdout_of, with_profile};
use predicates::prelude::*;

/// The terms-of-service note for starting another account, verbatim (issue #31).
const RUN_NOTE: &str = "⚠ You chose to start an account other than this project's. Use only accounts you own, and don't use several accounts to get around usage limits — doing so can violate Anthropic's Consumer Terms (https://www.anthropic.com/legal/consumer-terms) and Usage Policy (https://www.anthropic.com/legal/aup), including the updated Usage Policy that takes effect November 12, 2026.";

/// True when `stderr` carries the note as a line of its own, byte for byte.
fn has_run_note(stderr: &str) -> bool {
    stderr.lines().any(|l| l == RUN_NOTE)
}

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
    common::enable_live_usage(&sb);
    sb
}

/// The directory as mcc stores it (symlinks resolved: /var → /private/var).
fn real(sb: &Sandbox, rel: &str) -> PathBuf {
    std::fs::canonicalize(sb.path(rel)).unwrap()
}

/// How mcc shows `path` in output: `~/…` when it sits under the sandbox HOME.
/// On Linux the canonical temp path keeps the HOME prefix; on macOS it moves
/// to /private/var/…, so the same project shows as an absolute path there.
fn shown(sb: &Sandbox, path: &Path) -> String {
    match path.strip_prefix(sb.home()) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
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

/// `mcc project set PROFILE --allow ALLOW` in ~/code/app.
fn set_project_allowing(sb: &Sandbox, profile: &str, allow: &str) {
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["project", "set", profile, "--allow", allow])
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
            shown(&sb, &app)
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
    let (code_s, app_s) = (shown(&sb, &code), shown(&sb, &app));
    assert_eq!(
        rows,
        [
            vec!["PROJECT", "PROFILE", "ALLOW"],
            vec![&*code_s, "lab", "—"],
            vec![&*app_s, "default", "—", "←", "here"],
        ],
        "{list}"
    );
    let entries = json(stdout_of(sb.mcc().args(["project", "list", "--json"])).as_bytes());
    assert_eq!(
        entries,
        serde_json::json!([
            {"path": code.to_string_lossy(), "profile": "lab", "allow": []},
            {"path": app.to_string_lossy(), "profile": "default", "allow": []},
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
            shown(&sb, &app)
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
fn run_at_the_limit_defaults_to_waiting_and_shows_no_other_usage() {
    let sb = run_sandbox();
    // Even with another profile allowed, only the project's usage is read.
    set_project_allowing(&sb, "work", "lab");
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
        "`mcc run` would ask what to do; the default is to wait",
        "wait until `work` resets, or start it anyway with `mcc run --profile work` (this project also allows `lab`)",
    ] {
        assert!(err.contains(line), "missing {line:?} in:\n{err}");
    }
    assert!(err.contains("`work` (the default for "), "{err}");
    // No other account's usage, and no profile the project doesn't allow.
    assert!(!err.contains('%'), "{err}");
    assert!(!err.contains("side") && !err.contains("`default`"), "{err}");
    assert_eq!(stub.tokens(), ["tok-work"]);
    assert!(!has_run_note(&err), "{err}");
}

#[test]
fn run_at_the_limit_without_a_terminal_exits_2_without_other_usage() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    let s = stub(&[("tok-work", 10.0, 100.0), ("tok-lab", 50.0, 60.0)]);
    let run = || {
        let o = sb
            .mcc()
            .env("MCC_USAGE_URL", &s.url)
            .current_dir(sb.path("code/app"))
            .args(["run", "--", "--resume"])
            .assert()
            .code(2)
            .stdout(predicate::str::is_empty());
        String::from_utf8(o.get_output().stderr.clone()).unwrap()
    };
    let err = run();
    for line in [
        "is at its weekly limit, resets in 3d 4h",
        "no terminal is attached",
        "wait until `work` resets, or start it anyway with `mcc run --profile work`",
    ] {
        assert!(err.contains(line), "missing {line:?} in:\n{err}");
    }
    // Not allowed here: `lab` isn't offered at all.
    assert!(!err.contains('%') && !err.contains("lab"), "{err}");

    // Allowed: named, still without its usage.
    set_project_allowing(&sb, "work", "lab");
    let err = run();
    assert!(
        err.contains("(this project also allows `lab`)") && !err.contains('%'),
        "{err}"
    );
    assert_eq!(s.tokens(), ["tok-work", "tok-work"]);
}

#[test]
fn run_offers_only_allowed_profiles_that_exist_and_are_not_the_projects() {
    let sb = run_sandbox();
    set_project_allowing(&sb, "work", "lab");
    // A hand-edited list: the project's own profile, a profile that doesn't
    // exist, and a name twice.
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("allow = [\"lab\"]\n"), "{cfg}");
    sb.write(
        ".mcc/config.toml",
        &cfg.replace(
            "allow = [\"lab\"]",
            "allow = [\"work\", \"ghost\", \"lab\", \"lab\"]",
        ),
    );
    let s = stub(&[("tok-work", 100.0, 40.0), ("tok-lab", 1.0, 1.0)]);
    let o = sb
        .mcc()
        .env("MCC_USAGE_URL", &s.url)
        .current_dir(sb.path("code/app"))
        .arg("run")
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
    let err = String::from_utf8(o.get_output().stderr.clone()).unwrap();
    assert!(
        err.lines().any(|l| l.ends_with(
            "wait until `work` resets, or start it anyway with `mcc run --profile work` (this project also allows `lab`)"
        )),
        "{err}"
    );
    assert!(!err.contains("ghost") && !err.contains('%'), "{err}");
    assert_eq!(s.tokens(), ["tok-work"]);
}

#[test]
fn project_set_allow_names_other_profiles_mcc_run_may_offer() {
    let sb = run_sandbox();
    let app = real(&sb, "code/app");
    // Comma-separated or repeated; `claude` is the default profile; each
    // name once, and never the project's own profile.
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["project", "set", "work", "--allow", "lab,side", "--allow"])
        .args(["claude", "--allow", "lab", "--allow", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "`mcc run` starts `work` here; at its limit it can also offer `lab`, `side`, `default`",
        ));
    let entries = json(stdout_of(sb.mcc().args(["project", "list", "--json"])).as_bytes());
    assert_eq!(
        entries,
        serde_json::json!([
            {"path": app.to_string_lossy(), "profile": "work", "allow": ["lab", "side", "default"]},
        ])
    );

    // Narrowing the list names what it no longer allows.
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["project", "set", "work", "--allow", "lab"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "`mcc run` starts `work` here (no longer allows `side`, `default`); at its limit it can also offer `lab`",
        ));
    let cfg = sb.read(".mcc/config.toml");
    assert!(
        cfg.contains(&format!(
            "[projects.\"{}\"]\nprofile = \"work\"\nallow = [\"lab\"]\n",
            app.display()
        )),
        "{cfg}"
    );
    let rows: Vec<Vec<String>> = stdout_of(
        sb.mcc()
            .current_dir(sb.path("code/app"))
            .args(["project", "list"]),
    )
    .lines()
    .map(|l| l.split_whitespace().map(str::to_string).collect())
    .collect();
    assert_eq!(
        rows,
        [
            vec!["PROJECT", "PROFILE", "ALLOW"],
            vec![&*shown(&sb, &app), "work", "lab", "←", "here"],
        ]
    );

    // An unknown name is rejected, and nothing is written.
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["project", "set", "work", "--allow", "lab,nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"nope\""));
    assert_eq!(sb.read(".mcc/config.toml"), cfg);

    // Setting it again without --allow clears the list, and says so.
    sb.mcc()
        .current_dir(sb.path("code/app/src"))
        .args(["project", "set", "work"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "`mcc run` starts `work` here (no longer allows `lab`)\n",
        ));
    assert!(!sb.read(".mcc/config.toml").contains("allow"));
    let entries = json(stdout_of(sb.mcc().args(["project", "list", "--json"])).as_bytes());
    assert_eq!(entries[0]["allow"], serde_json::json!([]));

    // Switching to a profile it allowed doesn't count that one as dropped.
    set_project_allowing(&sb, "work", "lab,side");
    sb.mcc()
        .current_dir(sb.path("code/app"))
        .args(["project", "set", "lab"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "`mcc run` starts `lab` here (was `work`; no longer allows `side`)\n",
        ));
}

#[test]
fn run_prints_the_note_when_another_account_starts() {
    let sb = run_sandbox();
    set_project(&sb, "work");
    // `mcc [GLOBAL…] run --dry-run --profile NAME` in `dir`: (stdout, stderr).
    let run = |dir: &str, global: &[&str], name: &str| {
        let o = sb
            .mcc()
            .current_dir(sb.path(dir))
            .args(global)
            .args(["run", "--dry-run", "--profile", name])
            .assert()
            .success();
        let out = o.get_output();
        (
            String::from_utf8(out.stdout.clone()).unwrap(),
            String::from_utf8(out.stderr.clone()).unwrap(),
        )
    };
    // Another account than the project's: the note, even with --quiet.
    let (out, err) = run("code/app/src", &[], "lab");
    assert_eq!(out, "lab\n");
    assert!(has_run_note(&err), "{err}");
    assert!(has_run_note(&run("code/app", &["-q"], "lab").1));
    assert!(has_run_note(&run("code/app", &[], "claude").1));

    // The project's own profile, or any profile outside every project: none.
    let (out, err) = run("code/app", &[], "work");
    assert_eq!(out, "work\n");
    assert!(!err.contains("Consumer Terms"), "{err}");
    let (out, err) = run("code", &[], "lab");
    assert_eq!(out, "lab\n");
    assert!(!err.contains("Consumer Terms"), "{err}");

    // A broken project default doesn't stop --profile.
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &cfg.replace("profile = \"work\"", "profile = \"gone\""),
    );
    let (out, err) = run("code/app", &[], "lab");
    assert_eq!(out, "lab\n");
    assert!(has_run_note(&err), "{err}");
}

#[test]
fn rename_and_delete_keep_allow_lists_in_step() {
    let sb = run_sandbox();
    set_project_allowing(&sb, "work", "lab");
    sb.mcc()
        .args(["rename", "lab", "lab2"])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 project now allows `lab2`"));
    assert!(
        sb.read(".mcc/config.toml")
            .contains("profile = \"work\"\nallow = [\"lab2\"]\n")
    );

    // Deleting an allowed profile keeps the project.
    sb.mcc()
        .args(["delete", "lab2", "--yes", "--keep-auth"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "removed it from the allowed profiles of {}",
            shown(&sb, &real(&sb, "code/app"))
        )));
    let cfg = sb.read(".mcc/config.toml");
    assert!(
        cfg.contains("profile = \"work\"\n") && !cfg.contains("allow"),
        "{cfg}"
    );
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
            shown(&sb, &real(&sb, "code/app"))
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
    let (code_s, app_s, old_s) = (shown(&sb, &code), shown(&sb, &app), shown(&sb, &old));
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
        work.contains(&format!("    Default for:     {}\n", shown(&sb, &app))),
        "{long}"
    );

    // --json: the project directories of each profile.
    let all = json(list(&["--json"]).as_bytes());
    let defaults =
        |name: &str| common::by_name(all.as_array().unwrap(), name)["project_defaults"].clone();
    assert_eq!(defaults("work"), serde_json::json!([app.to_string_lossy()]));
    assert_eq!(
        defaults("default"),
        serde_json::json!([code.to_string_lossy()])
    );
    assert_eq!(defaults("side"), serde_json::json!([]));

    // A project naming a profile that no longer exists says so.
    let cfg = sb.read(".mcc/config.toml");
    sb.write(
        ".mcc/config.toml",
        &cfg.replace("profile = \"lab\"", "profile = \"gone\""),
    );
    assert!(list(&[]).contains("gone (no such profile)"));
}
