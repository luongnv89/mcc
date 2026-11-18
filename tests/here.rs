//! `mcc here` (issue #116): which profile, and so which account, `mcc run`
//! starts in the current directory, and why. Read-only, even before setup.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use common::{Sandbox, usage_stub, with_profile};
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

/// Set up with the default (Acme) and `work` (Globex, labelled), and:
/// a workspace ~/ws (not a git repository) defaulting to `work`, with
/// ~/ws/lab overriding it with `default`; a git repository ~/code/app
/// defaulting to `work` (set from a subfolder, so on the repository's top).
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    sb.write(
        ".mcc/profiles/work/.claude.json",
        r#"{"oauthAccount":{"emailAddress":"me@globex.com","organizationName":"Globex"}}"#,
    );
    sb.mcc()
        .args(["config", "set", "profiles.work.label", "Globex (work)"])
        .assert()
        .success();
    for dir in ["ws/site/src", "ws/lab/src", "code/app/src", "elsewhere"] {
        std::fs::create_dir_all(sb.path(dir)).unwrap();
    }
    sb.write("code/app/.git/HEAD", "ref: refs/heads/main\n");
    for (dir, args) in [
        ("ws", &["project", "set", "work"][..]),
        ("ws/lab", &["project", "set", "default"][..]),
        ("code/app/src", &["project", "set", "work"][..]),
    ] {
        sb.mcc()
            .current_dir(sb.path(dir))
            .args(args)
            .assert()
            .success();
    }
    sb
}

/// `mcc here` in `rel`: (stdout, stderr).
fn here_in(sb: &Sandbox, rel: &str) -> (String, String) {
    let o = sb
        .mcc()
        .current_dir(sb.path(rel))
        .arg("here")
        .assert()
        .success();
    let out = o.get_output();
    (
        String::from_utf8(out.stdout.clone()).unwrap(),
        String::from_utf8(out.stderr.clone()).unwrap(),
    )
}

/// The profile `mcc run` starts in `rel`, as `mcc run --dry-run` prints it.
fn run_starts_in(sb: &Sandbox, rel: &str) -> String {
    let o = sb
        .mcc()
        .env("MCC_NO_USAGE", "1")
        .current_dir(sb.path(rel))
        .args(["run", "--dry-run"])
        .assert()
        .success();
    String::from_utf8_lossy(&o.get_output().stdout)
        .trim_end()
        .to_string()
}

/// The profile named on `mcc here`'s first line: "Here: `work` …".
fn profile_named(out: &str) -> &str {
    let first = out.lines().next().unwrap_or_default();
    let rest = first
        .strip_prefix("Here: `")
        .unwrap_or_else(|| panic!("{out}"));
    &rest[..rest.find('`').unwrap()]
}

#[test]
fn reports_the_profile_why_and_account_mcc_run_uses_in_each_directory() {
    let sb = sandbox();
    // (directory, profile, why) — the why names the folder whose default
    // applies, said to be inherited when it is a parent folder.
    for (dir, profile, why) in [
        ("ws", "work", "the default for "),
        ("ws/site/src", "work", "inherited from parent folder "),
        ("ws/lab", "default", "the default for "),
        ("ws/lab/src", "default", "inherited from parent folder "),
        ("code/app/src", "work", "inherited from parent folder "),
        (
            "elsewhere",
            "default",
            "no project default here or in a parent folder",
        ),
    ] {
        let (out, err) = here_in(&sb, dir);
        // The same profile `mcc run` starts there.
        assert_eq!(profile_named(&out), run_starts_in(&sb, dir), "{dir}: {out}");
        assert_eq!(profile_named(&out), profile, "{dir}: {out}");
        let first = out.lines().next().unwrap();
        assert!(first.contains(&format!(" — {why}")), "{dir}: {out}");
        assert!(
            out.contains("\n  directory  ") && out.contains(&format!("/{dir}\n")),
            "{dir}: {out}"
        );
        // The account, its email hidden by default, and where it lives.
        let (account, config) = if profile == "work" {
            ("Globex (email hidden)", ".mcc/profiles/work")
        } else {
            ("Acme (email hidden)", ".claude")
        };
        assert!(
            out.contains(&format!("\n  account    {account}\n")),
            "{dir}: {out}"
        );
        assert!(out.contains(&format!("/{config}\n")), "{dir}: {out}");
        assert!(
            !out.contains("@globex.com") && !out.contains("@example.com"),
            "{out}"
        );
        assert!(!err.contains("not set up"), "{err}");
    }
    let (out, err) = here_in(&sb, "ws/site/src");
    assert!(
        out.starts_with("Here: `work` (Globex (work)) — inherited from parent folder "),
        "{out}"
    );
    assert!(
        err.contains("`mcc project set <profile> --path .` gives this folder its own"),
        "{err}"
    );
    let (_, err) = here_in(&sb, "elsewhere");
    assert!(
        err.contains("no folder from here up to / has a default"),
        "{err}"
    );

    // `[privacy] show_email` shows the email.
    sb.mcc()
        .args(["config", "set", "privacy.show_email", "true"])
        .assert()
        .success();
    let (out, _) = here_in(&sb, "ws");
    assert!(
        out.contains("\n  account    me@globex.com (Globex)\n"),
        "{out}"
    );
}

#[test]
fn changes_nothing_and_looks_nothing_up() {
    let sb = sandbox();
    sb.seed_oauth(".claude", "tok-default", common::now_ms() + common::HOUR_MS);
    sb.seed_oauth(
        ".mcc/profiles/work",
        "tok-work",
        common::now_ms() + common::HOUR_MS,
    );
    // Live lookup on with its disclosure not shown yet, and an older mcc's
    // default-profile pointer: other commands write config.toml for either.
    let cfg = sb.read(CONFIG);
    sb.write(
        CONFIG,
        &format!(
            "default_profile = \"work\"\n{}",
            cfg.replacen("[usage]\n", "[usage]\nlive = true\n", 1)
        ),
    );
    let stub = usage_stub();
    let before = snapshot(sb.home());
    for dir in ["ws/site/src", "code/app/src", "elsewhere"] {
        sb.mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .current_dir(sb.path(dir))
            .arg("here")
            .assert()
            .success()
            .stderr(predicate::str::contains("Live usage lookup is on").not())
            .stderr(predicate::str::contains("pointer").not());
    }
    assert!(stub.requests().is_empty(), "{:?}", stub.requests());
    assert!(snapshot(sb.home()) == before, "mcc here changed a file");
}

#[test]
fn before_setup_it_says_so_and_names_the_default_profile() {
    let sb = Sandbox::new();
    sb.seed_default();
    let before = snapshot(sb.home());
    let (out, err) = here_in(&sb, ".");
    assert!(
        out.starts_with("Here: `default` — no project default here or in a parent folder\n"),
        "{out}"
    );
    assert!(
        out.contains("\n  account    Acme (email hidden)\n"),
        "{out}"
    );
    assert!(err.contains("mcc is not set up yet"), "{err}");
    assert!(err.contains("run `mcc init`"), "{err}");
    assert!(
        !sb.exists(".mcc"),
        "mcc here must not run the first-run setup"
    );
    assert!(snapshot(sb.home()) == before, "mcc here changed a file");
    // `mcc run` there starts the same profile.
    assert_eq!(run_starts_in(&sb, "."), "default");
}

#[test]
fn a_project_default_naming_no_profile_is_reported_not_an_error() {
    let sb = sandbox();
    let cfg = sb.read(CONFIG);
    sb.write(
        CONFIG,
        &cfg.replace("profile = \"work\"", "profile = \"ghost\""),
    );
    let (out, err) = here_in(&sb, "ws/site");
    assert!(
        predicate::str::is_match(
            r"^Here: `ghost` — inherited from parent folder \S*/ws \(no such profile\)\n"
        )
        .unwrap()
        .eval(&out),
        "{out}"
    );
    assert!(!out.contains("account"), "{out}");
    assert!(err.contains("`mcc run` stops with an error here"), "{err}");
    assert!(err.contains("\"ghost\", which is not a profile"), "{err}");
    // As it says: `mcc run` stops there.
    sb.mcc()
        .env("MCC_NO_USAGE", "1")
        .current_dir(sb.path("ws/site"))
        .args(["run", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "\"ghost\", which is not a profile",
        ));
}
