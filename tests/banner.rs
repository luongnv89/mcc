//! The opt-in launch banner (issue #59): `[launch] banner = true` prints one
//! line on stderr when a profile starts from its launcher or `mcc run`, from
//! cached data only, and only when stderr is a terminal.

mod common;

use common::{HOUR_MS, Sandbox, enable_live_usage, now_ms, seed_usage_cache, with_profile};
use predicates::prelude::*;

const CONFIG: &str = ".mcc/config.toml";
const BANNER: &str = "▶ work · Acme · 5h 42% · week 18%";

fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    set(&sb, "profiles.work.label", "Acme");
    seed_usage_cache(&sb, "work", Some((42.4, 3600)), Some((18.0, 86400)), 60);
    sb
}

fn set(sb: &Sandbox, key: &str, value: &str) {
    sb.mcc()
        .args(["config", "set", key, value])
        .assert()
        .success();
}

/// Turn the banner on and rewrite the launchers, as the hint says.
fn turn_on(sb: &Sandbox) {
    set(sb, "launch.banner", "true");
    sb.mcc()
        .args(["refresh-launchers"])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 refreshed"));
}

#[test]
fn the_banner_is_off_by_default_and_set_with_mcc_config() {
    let sb = sandbox();
    assert!(!sb.read(CONFIG).contains("[launch]"));
    sb.mcc()
        .args(["config", "get", "launch.banner"])
        .assert()
        .success()
        .stdout("false\n");

    // Turning it on (or off) says to refresh the launchers, which mcc
    // doesn't rewrite on its own.
    let launcher = sb.read(".local/bin/work");
    sb.mcc()
        .args(["config", "set", "launch.banner", "true"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "run `mcc refresh-launchers` so profiles' launchers show the launch banner",
        ));
    assert_eq!(sb.read(".local/bin/work"), launcher);
    // Setting it to what it already is: no hint.
    sb.mcc()
        .args(["config", "set", "launch.banner", "true"])
        .assert()
        .success()
        .stderr(predicate::str::contains("refresh-launchers").not());
    assert!(
        sb.read(CONFIG).contains("[launch]\nbanner = true\n"),
        "{}",
        sb.read(CONFIG)
    );
    sb.mcc()
        .args(["config", "get", "launch.banner"])
        .assert()
        .success()
        .stdout("true\n");

    let before = sb.read(CONFIG);
    sb.mcc()
        .args(["config", "set", "launch.banner", "sometimes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("expected true or false"));
    assert_eq!(sb.read(CONFIG), before);

    sb.mcc()
        .args(["config", "set", "launch.banner", "false"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "run `mcc refresh-launchers` so profiles' launchers stop calling the launch banner",
        ));
    assert!(!sb.read(CONFIG).contains("[launch]"), "{}", sb.read(CONFIG));

    // `mcc help config` lists it.
    sb.mcc()
        .args(["help", "config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("launch.banner"));
}

#[test]
fn launchers_call_launch_banner_only_while_it_is_on() {
    let sb = sandbox();
    // Off: the launcher runs nothing but claude (no `mcc` on a plain launch).
    let off = sb.read(".local/bin/work");
    assert!(!off.contains("launch-banner"), "{off}");
    assert!(off.contains("# mcc-launcher: 2\n"), "{off}");

    // On: refresh-launchers adds the call; a profile added now has it too.
    turn_on(&sb);
    let on = sb.read(".local/bin/work");
    assert!(
        on.contains("mcc launch-banner 'work' >&2 2>/dev/null"),
        "{on}"
    );
    assert!(on.contains("# mcc-launcher: 2\n"), "{on}");
    with_profile(&sb, "lab", "--fresh");
    assert!(sb.read(".local/bin/lab").contains("launch-banner 'lab'"));

    // Off again: refresh-launchers takes it out, back to the same bytes.
    set(&sb, "launch.banner", "false");
    sb.mcc().args(["refresh-launchers"]).assert().success();
    assert_eq!(sb.read(".local/bin/work"), off);
    assert!(!sb.read(".local/bin/lab").contains("launch-banner"));
}

#[test]
fn launch_banner_never_prints_or_fails_without_a_terminal() {
    let sb = sandbox();
    set(&sb, "launch.banner", "true");
    for alias in ["work", "nope"] {
        sb.mcc()
            .args(["launch-banner", alias])
            .assert()
            .success()
            .stdout("")
            .stderr("");
    }
    // A config that doesn't parse: still silent, still exit 0.
    sb.write(CONFIG, "this is [not toml");
    sb.mcc()
        .args(["launch-banner", "work"])
        .assert()
        .success()
        .stdout("")
        .stderr("");
    // Hidden from the help.
    sb.mcc()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("launch-banner").not());
}

#[test]
fn piped_launches_get_no_banner_and_a_clean_stdout() {
    let sb = sandbox();
    turn_on(&sb);
    let o = sb.launcher("work").args(["-p", "hi"]).output().unwrap();
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout), "fake claude -p hi\n");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");

    let o = sb
        .mcc()
        .args(["run", "--profile", "work", "--", "-p", "hi"])
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout), "fake claude -p hi\n");
    assert!(!String::from_utf8_lossy(&o.stderr).contains('▶'));
}

/// `sh -c CMD` with its stdin, stdout and stderr on a pseudo-terminal
/// (util-linux `script`); what that terminal showed. `None` without `script`.
#[cfg(target_os = "linux")]
fn on_a_terminal(sb: &Sandbox, cmd: &str, env: &[(&str, &str)]) -> Option<String> {
    let script = ["/usr/bin/script", "/bin/script"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())?;
    let o = sb
        .command(script)
        .env("SHELL", "/bin/sh")
        .envs(env.iter().copied())
        .args(["-qec", cmd, "/dev/null"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    Some(String::from_utf8_lossy(&o.stdout).replace('\r', ""))
}

#[cfg(target_os = "linux")]
#[test]
fn a_launch_on_a_terminal_shows_the_banner_on_stderr_from_cached_data_only() {
    let sb = sandbox();
    // Live usage is on and a token is there: the banner still reads only
    // the cache — no request, no Keychain.
    enable_live_usage(&sb);
    sb.seed_oauth(".mcc/profiles/work", "test-token-work", now_ms() + HOUR_MS);
    let stub = common::UsageStub::start(|_| (500, "{}".into()));
    let env = [("MCC_USAGE_URL", stub.url.as_str())];
    let launcher = sb.path(".local/bin/work");
    let piped = format!("'{}' -p hi > out.txt", launcher.display());

    // Off by default.
    let Some(shown) = on_a_terminal(&sb, &piped, &env) else {
        return;
    };
    assert!(!shown.contains('▶'), "{shown}");

    turn_on(&sb);
    let shown = on_a_terminal(&sb, &piped, &env).unwrap();
    assert_eq!(shown, format!("{BANNER}\n"));
    // Claude Code's stdout (here a file, as with `| jq`) has no banner.
    assert_eq!(sb.read("out.txt"), "fake claude -p hi\n");

    // `mcc run` shows it too, after its own lines.
    let mcc = assert_cmd::cargo::cargo_bin("mcc");
    let run = format!("'{}' run --profile work -- -p hi > out.txt", mcc.display());
    let shown = on_a_terminal(&sb, &run, &env).unwrap();
    assert!(shown.ends_with(&format!("{BANNER}\n")), "{shown}");
    assert_eq!(sb.read("out.txt"), "fake claude -p hi\n");
    // `--quiet` leaves it out.
    let quiet = format!(
        "'{}' -q run --profile work -- -p hi > out.txt",
        mcc.display()
    );
    let shown = on_a_terminal(&sb, &quiet, &env).unwrap();
    assert!(!shown.contains('▶'), "{shown}");

    assert!(stub.requests().is_empty(), "{:?}", stub.requests());
    assert!(!sb.exists("security.log"), "the Keychain was read");
}

#[cfg(target_os = "linux")]
#[test]
fn the_banner_leaves_out_what_is_unknown_or_hidden() {
    let sb = Sandbox::new();
    sb.seed_default();
    // Turned on before the profile is added: its launcher has the call.
    set(&sb, "launch.banner", "true");
    with_profile(&sb, "work", "--fresh");
    let launcher = format!("'{}' > out.txt", sb.path(".local/bin/work").display());
    // No label, no cached usage: the name alone.
    let Some(shown) = on_a_terminal(&sb, &launcher, &[]) else {
        return;
    };
    assert_eq!(shown, "▶ work\n");
    // A label with an email is hidden while emails are; usage that has
    // reset since it was cached is gone; MCC_NO_USAGE skips the cache.
    set(&sb, "profiles.work.label", "me@example.com");
    seed_usage_cache(&sb, "work", Some((90.0, -60)), Some((30.0, 3600)), 7200);
    assert_eq!(
        on_a_terminal(&sb, &launcher, &[]).unwrap(),
        "▶ work · week 30%\n"
    );
    assert_eq!(
        on_a_terminal(&sb, &launcher, &[("MCC_NO_USAGE", "1")]).unwrap(),
        "▶ work\n"
    );
    set(&sb, "privacy.show_email", "true");
    assert_eq!(
        on_a_terminal(&sb, &launcher, &[("MCC_NO_USAGE", "1")]).unwrap(),
        "▶ work · me@example.com\n"
    );
}
