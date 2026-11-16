//! `mcc default`: switch the account of the default profile (~/.claude) by
//! signing it out and in again; trade accounts with a profile that holds
//! the one asked for; retire the pointer an older `mcc default` kept.

mod common;

use std::path::Path;

use common::{Sandbox, by_name, json, stdout_of, with_profile};
use predicates::prelude::*;

const HOME_ACCOUNT: &str = "me@home.test";
const WORK_ACCOUNT: &str = "dev@acme.test";
const LAB_ACCOUNT: &str = "lab@lab.test";

const OLD_BLOCK: &str = "# >>> mcc:default >>>\n\
    # Plain `claude` starts profile \"work\" — managed by mcc (reset with: mcc default default).\n\
    function claude {\n  \"$HOME/.local/bin/work\" \"$@\"\n}\n\
    # <<< mcc:default <<<\n";

/// ~/.claude on HOME_ACCOUNT, `work` (a clone) on WORK_ACCOUNT with a
/// label, `lab` (fresh) on LAB_ACCOUNT.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--fresh");
    sb.sign_in(".claude", ".claude.json", HOME_ACCOUNT);
    sb.sign_in(
        ".mcc/profiles/work",
        ".mcc/profiles/work/.claude.json",
        WORK_ACCOUNT,
    );
    sb.sign_in(
        ".mcc/profiles/lab",
        ".mcc/profiles/lab/.claude.json",
        LAB_ACCOUNT,
    );
    sb.mcc()
        .args(["config", "set", "profiles.work.label", "Acme (work)"])
        .assert()
        .success();
    sb
}

/// `mcc` with the fake `claude` keeping one account per directory.
fn mcc(sb: &Sandbox) -> assert_cmd::Command {
    let mut cmd = sb.mcc();
    cmd.env("FAKE_PER_DIR", "1");
    cmd
}

/// The account the fake `claude` has `rel_dir` signed in to.
fn account(sb: &Sandbox, rel_dir: &str) -> Option<String> {
    sb.exists(&format!("{rel_dir}/.fake-account")).then(|| {
        sb.read(&format!("{rel_dir}/.fake-account"))
            .trim()
            .to_string()
    })
}

/// What the fake `claude` did, in order, with the sandbox path cut off:
/// `.claude logout`, `.mcc/profiles/work login a@b`, …
fn auth_log(sb: &Sandbox) -> Vec<String> {
    if !sb.exists("auth.log") {
        return Vec::new();
    }
    let home = format!("{}/", sb.home().display());
    sb.read("auth.log")
        .lines()
        .map(|l| l.replace(&home, ""))
        .collect()
}

/// `mcc list --json` (usage off, emails shown).
fn list(sb: &Sandbox) -> Vec<serde_json::Value> {
    json(
        stdout_of(
            sb.mcc()
                .args(["list", "--json", "--no-usage", "--show-email"]),
        )
        .as_bytes(),
    )
    .as_array()
    .unwrap()
    .clone()
}

fn project(sb: &Sandbox, name: &str, profile: &str) -> std::path::PathBuf {
    let dir = sb.path(name);
    std::fs::create_dir_all(&dir).unwrap();
    sb.mcc()
        .args(["project", "set", profile, "--path"])
        .arg(&dir)
        .assert()
        .success();
    dir
}

/// `mcc run --dry-run` in `dir`: the profile it would start.
fn runs_in(sb: &Sandbox, dir: &Path) -> String {
    let out = sb
        .mcc()
        .env("MCC_NO_USAGE", "1")
        .args(["run", "--dry-run"])
        .current_dir(dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap().trim().to_string()
}

#[test]
fn show_names_the_default_profile_and_its_account() {
    let sb = sandbox();
    let cfg = sb.read(".mcc/config.toml");
    mcc(&sb)
        .arg("default")
        .assert()
        .success()
        .stdout("default\n")
        .stderr(predicate::str::contains("the default profile is ~/.claude"))
        .stderr(predicate::str::contains("an account (email hidden)"))
        .stderr(predicate::str::contains(HOME_ACCOUNT).not())
        .stderr(predicate::str::contains("mcc default --new"));
    mcc(&sb)
        .args(["config", "set", "privacy.show_email", "true"])
        .assert()
        .success();
    mcc(&sb)
        .arg("default")
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "it is signed in to {HOME_ACCOUNT}"
        )));
    assert!(auth_log(&sb).is_empty());
    assert!(!cfg.contains("default_profile"));
}

#[test]
fn trading_with_a_profile_swaps_both_accounts() {
    let sb = sandbox();
    sb.write(".mcc/state/default/usage.json", "{}");
    sb.write(".mcc/state/work/usage.json", "{}");
    mcc(&sb)
        .args(["default", "work", "-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "~/.claude is now signed in to {WORK_ACCOUNT}, and `work` to {HOME_ACCOUNT}"
        )))
        .stderr(predicate::str::contains(
            "`work`'s label \"Acme (work)\" was for",
        ));
    assert_eq!(account(&sb, ".claude").as_deref(), Some(WORK_ACCOUNT));
    assert_eq!(
        account(&sb, ".mcc/profiles/work").as_deref(),
        Some(HOME_ACCOUNT)
    );
    assert_eq!(
        account(&sb, ".mcc/profiles/lab").as_deref(),
        Some(LAB_ACCOUNT)
    );
    // ~/.claude first, each sign-in pre-filled with the account it takes.
    assert_eq!(
        auth_log(&sb),
        [
            ".claude logout".to_string(),
            format!(".claude login {WORK_ACCOUNT}"),
            ".mcc/profiles/work logout".to_string(),
            format!(".mcc/profiles/work login {HOME_ACCOUNT}"),
        ]
    );
    assert_eq!(sb.read("login-args"), format!("--email\n{HOME_ACCOUNT}\n"));

    // The registry follows: `work`'s account, its label (for the account
    // it gave away) gone, both caches forgotten.
    let cfg = sb.read(".mcc/config.toml");
    assert!(
        cfg.contains(&format!(
            "[profiles.work.account]\nemail = \"{HOME_ACCOUNT}\""
        )),
        "{cfg}"
    );
    assert!(!cfg.contains("Acme (work)"), "{cfg}");
    assert!(!sb.exists(".mcc/state/default/usage.json"));
    assert!(!sb.exists(".mcc/state/work/usage.json"));
    let all = list(&sb);
    assert_eq!(by_name(&all, "default")["account"], WORK_ACCOUNT);
    assert_eq!(by_name(&all, "work")["account"], HOME_ACCOUNT);
    assert_eq!(by_name(&all, "default")["is_global_default"], true);
    assert_eq!(by_name(&all, "work")["is_global_default"], false);

    // Trading back restores both, and --label labels the profile.
    mcc(&sb)
        .args(["default", "work", "-y", "--label", "Acme (work)"])
        .assert()
        .success();
    assert_eq!(account(&sb, ".claude").as_deref(), Some(HOME_ACCOUNT));
    assert_eq!(
        account(&sb, ".mcc/profiles/work").as_deref(),
        Some(WORK_ACCOUNT)
    );
    assert!(
        sb.read(".mcc/config.toml")
            .contains("label = \"Acme (work)\"")
    );
}

#[test]
fn without_a_terminal_a_switch_needs_y_and_changes_nothing() {
    let sb = sandbox();
    let cfg = sb.read(".mcc/config.toml");
    for args in [&["default", "work"][..], &["default", "--new"]] {
        mcc(&sb)
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("pass -y to confirm"));
    }
    assert!(auth_log(&sb).is_empty());
    assert_eq!(account(&sb, ".claude").as_deref(), Some(HOME_ACCOUNT));
    assert_eq!(sb.read(".mcc/config.toml"), cfg);
}

#[test]
fn projects_follow_their_account_unless_kept() {
    let sb = sandbox();
    let at_work = project(&sb, "src/acme", "work");
    let at_home = project(&sb, "src/home", "default");
    let at_lab = project(&sb, "src/lab", "lab");
    mcc(&sb)
        .args(["default", "work", "-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "2 projects followed their account",
        ));
    // The repository that used WORK_ACCOUNT through `work` still does.
    assert_eq!(runs_in(&sb, &at_work), "default");
    assert_eq!(runs_in(&sb, &at_home), "work");
    assert_eq!(runs_in(&sb, &at_lab), "lab");

    mcc(&sb)
        .args(["default", "work", "-y", "--keep-projects"])
        .assert()
        .success();
    assert_eq!(runs_in(&sb, &at_work), "default");
    assert_eq!(runs_in(&sb, &at_home), "work");
}

#[test]
fn landing_on_the_wrong_account_stops_before_the_profile_signs_out() {
    let sb = sandbox();
    let cfg = sb.read(".mcc/config.toml");
    mcc(&sb)
        .env("FAKE_LOGIN_AS_1", "other@else.test")
        .args(["default", "work", "-y"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "the trade stopped: ~/.claude is signed in to other@else.test; `work` is unchanged",
        ))
        .stderr(predicate::str::contains(format!(
            "`claude auth login --email {HOME_ACCOUNT}`"
        )));
    assert_eq!(
        account(&sb, ".mcc/profiles/work").as_deref(),
        Some(WORK_ACCOUNT)
    );
    assert!(auth_log(&sb).iter().all(|l| !l.contains("work")));
    assert_eq!(sb.read(".mcc/config.toml"), cfg);

    // The hint's command puts ~/.claude back; the trade then works.
    sb.command("claude")
        .env("FAKE_PER_DIR", "1")
        .args(["auth", "login", "--email", HOME_ACCOUNT])
        .status()
        .unwrap();
    assert_eq!(account(&sb, ".claude").as_deref(), Some(HOME_ACCOUNT));
    mcc(&sb).args(["default", "work", "-y"]).assert().success();
    assert_eq!(account(&sb, ".claude").as_deref(), Some(WORK_ACCOUNT));
}

#[test]
fn a_failed_second_sign_in_stops_halfway_with_the_way_to_finish() {
    let sb = sandbox();
    let at_work = project(&sb, "src/acme", "work");
    sb.write(".mcc/state/work/usage.json", "{}");
    mcc(&sb)
        .env("FAKE_LOGIN_FAIL_AT", "2")
        .args(["default", "work", "-y"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(format!(
            "the trade stopped halfway: ~/.claude is signed in to {WORK_ACCOUNT}, but `work` is signed out"
        )))
        .stderr(predicate::str::contains(format!(
            "`mcc exec work -- claude auth login --email {HOME_ACCOUNT}`"
        )));
    assert_eq!(account(&sb, ".claude").as_deref(), Some(WORK_ACCOUNT));
    assert_eq!(account(&sb, ".mcc/profiles/work"), None);
    // The registry already follows ~/.claude's move: the project that used
    // WORK_ACCOUNT through `work` uses it through ~/.claude, and what
    // described WORK_ACCOUNT under `work` is gone.
    assert_eq!(runs_in(&sb, &at_work), "default");
    let cfg = sb.read(".mcc/config.toml");
    assert!(!cfg.contains("Acme (work)"), "{cfg}");
    assert!(!cfg.contains(WORK_ACCOUNT), "{cfg}");
    assert!(!sb.exists(".mcc/state/work/usage.json"));

    // The hint's command finishes the trade.
    mcc(&sb)
        .args([
            "exec",
            "work",
            "--",
            "claude",
            "auth",
            "login",
            "--email",
            HOME_ACCOUNT,
        ])
        .assert()
        .success();
    assert_eq!(
        account(&sb, ".mcc/profiles/work").as_deref(),
        Some(HOME_ACCOUNT)
    );
    assert_eq!(by_name(&list(&sb), "work")["account"], HOME_ACCOUNT);
}

#[test]
fn a_trade_is_refused_before_anything_changes() {
    let sb = sandbox();
    let refused = |sb: &Sandbox, args: &[&str], code: i32, msg: &str| {
        mcc(sb)
            .args(args)
            .assert()
            .code(code)
            .stderr(predicate::str::contains(msg));
        assert!(auth_log(sb).is_empty(), "{args:?}");
    };
    refused(&sb, &["default", "default", "-y"], 2, "already");
    refused(&sb, &["default", "claude", "-y"], 2, "already");
    refused(&sb, &["default", "ghost", "-y"], 2, "ghost");
    refused(&sb, &["default", "work", "--new"], 2, "cannot be used with");
    refused(
        &sb,
        &["default", "work", "--keep-as", "x"],
        2,
        "--keep-as goes with --new",
    );
    refused(
        &sb,
        &["default", "--email", "a@b.c"],
        2,
        "--email goes with --new",
    );

    // Both on one account: nothing to trade.
    sb.sign_in(
        ".mcc/profiles/lab",
        ".mcc/profiles/lab/.claude.json",
        HOME_ACCOUNT,
    );
    refused(&sb, &["default", "lab", "-y"], 1, "nothing to trade");
    // A signed-out profile has no account to give.
    std::fs::remove_file(sb.path(".mcc/profiles/lab/.credentials.json")).unwrap();
    refused(&sb, &["default", "lab", "-y"], 1, "`lab` is not signed in");
    // A Console login can't move.
    let mut cmd = mcc(&sb);
    cmd.env("FAKE_AUTH_METHOD", "api_key")
        .env("FAKE_API_KEY_SOURCE", "/login managed key")
        .args(["default", "work", "-y"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Anthropic Console"));
    assert!(auth_log(&sb).is_empty());
}

#[test]
fn a_sharing_profile_is_not_handed_an_account_of_another_kind() {
    let sb = sandbox();
    // `work` and `lab` share sessions in one Team organization.
    for (rel, email) in [("work", WORK_ACCOUNT), ("lab", LAB_ACCOUNT)] {
        sb.write(
            &format!(".mcc/profiles/{rel}/.claude.json"),
            &format!(
                r#"{{"oauthAccount":{{"emailAddress":"{email}","organizationUuid":"org-acme","organizationName":"Acme","organizationType":"claude_team"}}}}"#
            ),
        );
    }
    let cfg = sb.read(".mcc/config.toml");
    let cfg = cfg
        .replace(
            "[profiles.work]\n",
            "[profiles.work]\nshare_sessions = true\n",
        )
        .replace(
            "[profiles.lab]\n",
            "[profiles.lab]\nshare_sessions = true\n",
        );
    sb.write(".mcc/config.toml", &cfg);
    mcc(&sb)
        .args(["default", "work", "-y"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("across organizations"))
        .stderr(predicate::str::contains("mcc share work --off"));
    assert!(auth_log(&sb).is_empty());
}

#[test]
fn new_keeps_the_old_account_as_a_profile_before_signing_out() {
    let sb = sandbox();
    mcc(&sb)
        .args([
            "default",
            "--new",
            "--email",
            "new@else.test",
            "--keep-as",
            "personal",
            "-y",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "~/.claude is now signed in to new@else.test",
        ))
        // The account moves to `personal`: no duplicate warning.
        .stderr(predicate::str::contains("already logged in to").not());
    assert_eq!(account(&sb, ".claude").as_deref(), Some("new@else.test"));
    assert_eq!(
        account(&sb, ".mcc/profiles/personal").as_deref(),
        Some(HOME_ACCOUNT)
    );
    assert_eq!(
        auth_log(&sb),
        [
            format!(".mcc/profiles/personal login {HOME_ACCOUNT}"),
            ".claude logout".to_string(),
            ".claude login new@else.test".to_string(),
        ]
    );
    // A clone of ~/.claude's setup.
    assert!(sb.exists(".mcc/profiles/personal/settings.json"));
    assert!(sb.read(".zshrc").contains("alias personal="));
}

#[test]
fn new_with_y_alone_signs_the_old_account_out_of_the_machine() {
    let sb = sandbox();
    mcc(&sb)
        .args(["default", "--new", "--email", "new@else.test", "-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "{HOME_ACCOUNT} will no longer be signed in on this machine"
        )));
    assert_eq!(account(&sb, ".claude").as_deref(), Some("new@else.test"));
    assert!(!sb.exists(".mcc/profiles/personal"));
}

#[test]
fn a_sign_in_mcc_cant_read_is_never_signed_out() {
    let sb = sandbox();
    // ~/.claude is signed in, but `claude auth status` fails for it: --new
    // must not take it for signed out and sign it out unkept.
    sb.write(".claude/.fake-status-broken", "");
    mcc(&sb)
        .args(["default", "--new", "--email", "new@else.test", "-y"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("can't read ~/.claude's sign-in"));
    assert!(auth_log(&sb).is_empty());
    assert_eq!(account(&sb, ".claude").as_deref(), Some(HOME_ACCOUNT));

    // Nor can a trade go ahead with a side it can't read.
    std::fs::remove_file(sb.path(".claude/.fake-status-broken")).unwrap();
    sb.write(".mcc/profiles/work/.fake-status-broken", "");
    mcc(&sb)
        .args(["default", "work", "-y"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("can't read `work`'s sign-in"));
    assert!(auth_log(&sb).is_empty());
    assert_eq!(account(&sb, ".claude").as_deref(), Some(HOME_ACCOUNT));
}

#[test]
fn new_does_not_keep_an_account_a_profile_holds() {
    let sb = sandbox();
    sb.sign_in(
        ".mcc/profiles/lab",
        ".mcc/profiles/lab/.claude.json",
        HOME_ACCOUNT,
    );
    mcc(&sb)
        .args([
            "default",
            "--new",
            "--email",
            "new@else.test",
            "--keep-as",
            "personal",
            "-y",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("--keep-as personal is not needed"))
        .stderr(predicate::str::contains(format!(
            "{HOME_ACCOUNT} stays signed in to `lab`"
        )));
    assert!(!sb.exists(".mcc/profiles/personal"));
}

#[test]
fn new_with_a_profiles_email_is_a_trade() {
    let sb = sandbox();
    mcc(&sb)
        .args(["default", "--new", "--email", WORK_ACCOUNT, "-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "{WORK_ACCOUNT} is the account of profile `work`"
        )));
    assert_eq!(account(&sb, ".claude").as_deref(), Some(WORK_ACCOUNT));
    assert_eq!(
        account(&sb, ".mcc/profiles/work").as_deref(),
        Some(HOME_ACCOUNT)
    );
}

#[test]
fn new_landing_on_a_profiles_account_offers_the_trade() {
    let sb = sandbox();
    mcc(&sb)
        .env("FAKE_LOGIN_AS_1", WORK_ACCOUNT)
        .args(["default", "--new", "-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "{WORK_ACCOUNT} is also the account of `work`"
        )))
        .stderr(predicate::str::contains(format!(
            "~/.claude is now signed in to {WORK_ACCOUNT}, and `work` to {HOME_ACCOUNT}"
        )));
    assert_eq!(
        account(&sb, ".mcc/profiles/work").as_deref(),
        Some(HOME_ACCOUNT)
    );
}

#[test]
fn new_landing_on_the_same_account_stops() {
    let sb = sandbox();
    mcc(&sb)
        .env("FAKE_LOGIN_AS_1", HOME_ACCOUNT)
        .args(["default", "--new", "-y"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(format!(
            "~/.claude did not switch accounts: it is signed in to {HOME_ACCOUNT}"
        )));
}

#[test]
fn new_signs_in_a_signed_out_default() {
    let sb = sandbox();
    std::fs::remove_file(sb.path(".claude/.credentials.json")).unwrap();
    mcc(&sb)
        .args(["default", "--new", "--email", "new@else.test"])
        .assert()
        .code(2);
    mcc(&sb)
        .args(["default", "--new", "--email", "new@else.test", "-y"])
        .assert()
        .success();
    assert_eq!(account(&sb, ".claude").as_deref(), Some("new@else.test"));
}

#[test]
fn an_older_claude_without_email_prefill_still_trades() {
    let sb = sandbox();
    mcc(&sb)
        .env("FAKE_NO_EMAIL_FLAG", "1")
        .args(["default", "--new", "--email", "new@else.test", "-y"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("has no --email option"));
    assert!(auth_log(&sb).is_empty());
    // A trade signs in blank and checks the account all the same.
    mcc(&sb)
        .env("FAKE_NO_EMAIL_FLAG", "1")
        .env("FAKE_LOGIN_AS_1", WORK_ACCOUNT)
        .env("FAKE_LOGIN_AS_2", HOME_ACCOUNT)
        .args(["default", "work", "-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains("can't pre-fill the sign-in page"));
    assert!(!sb.read("login-args").contains("--email"));
    assert_eq!(account(&sb, ".claude").as_deref(), Some(WORK_ACCOUNT));
}

#[test]
fn a_signed_out_profile_does_not_count_as_keeping_the_old_account() {
    let sb = sandbox();
    // `lab`'s state file still names HOME_ACCOUNT, but it is signed out.
    sb.sign_in(
        ".mcc/profiles/lab",
        ".mcc/profiles/lab/.claude.json",
        HOME_ACCOUNT,
    );
    std::fs::remove_file(sb.path(".mcc/profiles/lab/.credentials.json")).unwrap();
    std::fs::remove_file(sb.path(".mcc/profiles/lab/.fake-account")).unwrap();
    mcc(&sb)
        .args([
            "default",
            "--new",
            "--email",
            "new@else.test",
            "--keep-as",
            "personal",
            "-y",
        ])
        .assert()
        .success();
    assert_eq!(
        account(&sb, ".mcc/profiles/personal").as_deref(),
        Some(HOME_ACCOUNT)
    );
}

#[test]
fn credential_variables_never_reach_the_sign_ins() {
    let sb = sandbox();
    mcc(&sb)
        .env("ANTHROPIC_API_KEY", "sk-test-not-a-key")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "not-a-token")
        .args(["default", "work", "-y"])
        .assert()
        .success();
    assert_eq!(account(&sb, ".claude").as_deref(), Some(WORK_ACCOUNT));
    let seen = if sb.exists("claude-env.log") {
        sb.read("claude-env.log")
    } else {
        String::new()
    };
    assert!(seen.is_empty(), "{seen}");
}

#[test]
fn a_trade_across_organizations_says_what_history_it_opens() {
    let sb = sandbox();
    sb.write(
        ".mcc/profiles/work/.claude.json",
        &format!(
            r#"{{"oauthAccount":{{"emailAddress":"{WORK_ACCOUNT}","organizationUuid":"org-acme","organizationName":"Acme","organizationType":"claude_team"}}}}"#
        ),
    );
    sb.write(".mcc/profiles/work/.fake-org", "org-acme\n");
    mcc(&sb)
        .env("FAKE_LOGIN_ORG_1", "org-acme")
        .args(["default", "work", "-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "~/.claude keeps its history and sessions",
        ))
        .stderr(predicate::str::contains(format!(
            "what {HOME_ACCOUNT} (a personal account) did there can be resumed under {WORK_ACCOUNT} (organization Acme)"
        )));
    // The same organization on both sides of each sign-in: it went through.
    assert_eq!(account(&sb, ".claude").as_deref(), Some(WORK_ACCOUNT));
    // Two personal accounts: nothing to say.
    let sb = sandbox();
    mcc(&sb)
        .args(["default", "lab", "-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains("keeps its history").not());
}

#[test]
fn an_older_pointer_is_retired_on_the_next_run() {
    let sb = sandbox();
    let cfg = sb.read(".mcc/config.toml");
    let rc = sb.read(".zshrc");
    sb.write(
        ".mcc/config.toml",
        &format!("default_profile = \"work\"\n{cfg}"),
    );
    sb.write(".zshrc", &format!("{rc}\n{OLD_BLOCK}"));

    // doctor only reports it.
    let checks =
        json(stdout_of(sb.mcc().env("MCC_NO_USAGE", "1").args(["doctor", "--json"])).as_bytes());
    let f = checks
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "default_profile")
        .expect("a default_profile finding");
    assert_eq!(f["status"], "warn", "{f}");
    assert_eq!(f["fix"], "mcc default", "{f}");

    // Any other command retires both at once.
    sb.mcc()
        .args(["list", "--no-usage"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "`mcc default` now switches the account of ~/.claude",
        ))
        .stderr(predicate::str::contains(
            "`mcc default work` makes its account the default",
        ));
    assert_eq!(sb.read(".mcc/config.toml"), cfg);
    assert!(!sb.read(".zshrc").contains("mcc:default"));
    assert!(sb.read(".zshrc").contains("alias work="));
    assert_eq!(runs_in(&sb, sb.home()), "default");
    // Said once.
    sb.mcc()
        .args(["list", "--no-usage"])
        .assert()
        .success()
        .stderr(predicate::str::contains("mcc default").not());
    let checks =
        json(stdout_of(sb.mcc().env("MCC_NO_USAGE", "1").args(["doctor", "--json"])).as_bytes());
    assert!(
        checks
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["check"] != "default_profile")
    );
}

#[test]
fn a_leftover_shell_function_alone_is_removed_by_mcc_default() {
    let sb = sandbox();
    let rc = sb.read(".zshrc");
    sb.write(".zshrc", &format!("{rc}\n{OLD_BLOCK}"));
    mcc(&sb)
        .arg("default")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "removed the `claude` function in ~/.zshrc",
        ));
    assert!(!sb.read(".zshrc").contains("mcc:default"));
    assert!(auth_log(&sb).is_empty());
}

#[test]
fn the_default_profiles_key_is_not_settable() {
    let sb = sandbox();
    sb.mcc()
        .args(["config", "set", "default_profile", "work"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "~/.claude is always the default profile",
        ));
}
