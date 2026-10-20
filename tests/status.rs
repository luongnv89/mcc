//! Bare `mcc` (issue #54): the status view once mcc is set up, the help
//! before that. Read-only and fast: nothing written, no live usage lookup.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use common::{Sandbox, seed_usage_cache, stdout_of, usage_stub, with_profile};
use predicates::prelude::*;

const CONFIG: &str = ".mcc/config.toml";

/// Every file under `dir` with its content and modification time.
fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                stack.push(p.clone());
            }
            let bytes = if meta.is_file() {
                std::fs::read(&p).unwrap()
            } else {
                Vec::new()
            };
            out.insert(p.display().to_string(), (bytes, meta.modified().unwrap()));
        }
    }
    out
}

/// Set up, with the default profile and `work` cloned from it.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb
}

#[test]
fn before_setup_bare_mcc_prints_the_help_and_writes_nothing() {
    let sb = Sandbox::new();
    sb.seed_default();
    sb.mcc()
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains("Usage: mcc"))
        .stderr(predicate::str::contains("Commands:"));
    assert!(
        !sb.exists(".mcc"),
        "bare mcc must not run the first-run setup"
    );
    // Global flags alone are still a usage error before setup.
    sb.mcc()
        .arg("--no-color")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("requires a subcommand"));
    assert!(!sb.exists(".mcc"));
}

#[test]
fn shows_the_profile_here_and_every_profile() {
    let sb = sandbox();
    seed_usage_cache(&sb, "work", Some((42.0, 2 * 3600)), Some((18.0, 86400)), 60);
    let out = stdout_of(&mut sb.mcc());
    assert!(
        out.starts_with("Here: `default` — no project default here\n\n"),
        "{out}"
    );
    assert!(
        predicate::str::is_match(r"(?m)^NAME\s+ACCOUNT\s+5H\s+WEEK\s+LAST USED\s+SYNC$")
            .unwrap()
            .eval(&out),
        "{out}"
    );
    assert!(
        predicate::str::is_match(r"(?m)^default ★\s+Acme\s")
            .unwrap()
            .eval(&out),
        "{out}"
    );
    assert!(
        predicate::str::is_match(r"(?m)^work\s.*42%.*18%")
            .unwrap()
            .eval(&out),
        "{out}"
    );
    // Not a command's help, and no escape codes with NO_COLOR or no terminal.
    assert!(
        !out.contains("Usage: mcc") && !out.contains('\u{1b}'),
        "{out}"
    );

    // Global flags alone show it too.
    let quiet = sb.mcc().args(["-q", "--no-color"]).assert().success();
    let quiet = quiet.get_output();
    assert!(String::from_utf8_lossy(&quiet.stdout).starts_with("Here: "));
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("profiles ·"),
        "--quiet drops the info line"
    );
}

#[test]
fn says_which_project_default_applies_here_and_why() {
    let sb = sandbox();
    sb.mcc()
        .args(["config", "set", "profiles.work.label", "Acme (work)"])
        .assert()
        .success();
    std::fs::create_dir_all(sb.path("app/src")).unwrap();
    sb.mcc()
        .args(["project", "set", "work", "--path"])
        .arg(sb.path("app"))
        .assert()
        .success();
    let out = stdout_of(sb.mcc().current_dir(sb.path("app/src")));
    // `~/app` here; on macOS the temp HOME is behind a symlink
    // (/var → /private/var), so the stored, resolved path shows in full.
    let first = out.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("Here: `work` (Acme (work)) — the default for ")
            && first.ends_with("/app"),
        "{out}"
    );

    // A project default naming no profile is reported, not an error.
    let cfg = sb.read(CONFIG);
    sb.write(
        CONFIG,
        &cfg.replace("profile = \"work\"", "profile = \"ghost\""),
    );
    sb.mcc()
        .current_dir(sb.path("app"))
        .assert()
        .success()
        .stdout(
            predicate::str::is_match(
                r"^Here: `ghost` — the default for \S*/app \(no such profile\)\n",
            )
            .unwrap(),
        )
        .stderr(predicate::str::contains(
            "\"ghost\", which is not a profile",
        ))
        .stderr(predicate::str::contains("mcc doctor"));
}

#[test]
fn hides_emails_unless_shown() {
    let sb = sandbox();
    sb.mcc()
        .args([
            "config",
            "set",
            "profiles.work.label",
            "me@example.com's Org",
        ])
        .assert()
        .success();
    let out = stdout_of(&mut sb.mcc());
    assert!(!out.contains('@'), "{out}");

    let cfg = sb.read(CONFIG);
    sb.write(CONFIG, &format!("{cfg}\n[privacy]\nshow_email = true\n"));
    let out = stdout_of(&mut sb.mcc());
    assert!(out.contains("me@example.com (Acme)"), "{out}");
    // The label wins over the organization (cut to fit 120 columns).
    assert!(out.contains("fake@example.com (me@example.co"), "{out}");
}

#[test]
fn warns_about_drift_and_stale_launchers_and_points_at_doctor() {
    let sb = sandbox();
    // Nothing wrong yet: no warning, no pointer to doctor.
    sb.mcc()
        .assert()
        .success()
        .stderr(predicate::str::contains("drift").not())
        .stderr(predicate::str::contains("mcc doctor").not())
        .stderr(predicate::str::contains("2 profiles · "));

    sb.write(".claude/CLAUDE.md", "# changed in the default\n");
    sb.mcc()
        .args(["config", "set", "launch.banner", "true"])
        .assert()
        .success();
    sb.mcc()
        .assert()
        .success()
        .stdout(predicate::str::contains("1 behind"))
        .stderr(predicate::str::contains(
            "drift against the default: `work` (1 behind)",
        ))
        .stderr(predicate::str::contains("mcc refresh-launchers"))
        .stderr(predicate::str::contains("run `mcc doctor`"));
}

#[test]
fn fresh_profiles_never_synced_are_not_drift() {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "lab", "--fresh");
    sb.mcc()
        .assert()
        .success()
        .stderr(predicate::str::contains("drift").not());
}

#[test]
fn is_read_only_and_never_looks_usage_up_live() {
    let sb = sandbox();
    sb.seed_oauth(
        ".claude",
        "test-token-default",
        common::now_ms() + common::HOUR_MS,
    );
    // Live lookup on, its one-time disclosure not shown yet: bare mcc must
    // neither show it (that records it in config.toml) nor go live.
    let cfg = sb.read(CONFIG);
    sb.write(
        CONFIG,
        &cfg.replacen("[usage]\n", "[usage]\nlive = true\n", 1),
    );
    assert!(sb.read(CONFIG).contains("live = true"));
    let stub = usage_stub();
    let before = snapshot(sb.home());
    sb.mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .assert()
        .success()
        .stderr(predicate::str::contains("Live usage lookup is on").not());
    assert!(stub.requests().is_empty(), "{:?}", stub.requests());
    assert!(snapshot(sb.home()) == before, "bare mcc changed a file");
}

#[test]
fn help_is_unchanged_once_set_up() {
    let sb = sandbox();
    sb.mcc()
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Commands:"));
    sb.mcc()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("run several Claude Code accounts"));
    sb.mcc()
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: mcc"));
}
