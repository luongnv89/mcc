//! `mcc login` (alias `mcc renew`) — renew a profile's Claude Code sign-in
//! before its login runs out (#100).
//!
//! A Claude Code login lasts about 30 days from its last sign-in
//! (`claudeAiOauth.refreshTokenExpiresAt`), and a normal token refresh keeps
//! that deadline. So the default is a new sign-in in place: `claude auth
//! login` for the profile, attached to the terminal, with nothing logged
//! out, reset, or written to `~/.mcc/config.toml`. `--refresh-only` instead
//! asks Claude Code to refresh the access token itself with one tiny,
//! isolated `claude -p` request; mcc never sends or refreshes a token.
//!
//! Success is decided by the deadline alone (read before and after from the
//! credentials file and the Keychain, timestamps only): it must be known,
//! later than before, and outside the warning window. A file rewritten with
//! the same deadline is a failure, as is a deadline known before and
//! unreadable after; only a sign-in with no deadline before or after succeeds
//! with the expiry unknown. Signing in to another account or
//! organization than before is a failure too, as is a session-sharing profile
//! that ends up sharing across organizations.

use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::cli::LoginArgs;
use crate::config::{Account, Config, OrgMatch};
use crate::error::{CliError, EXIT_INTERRUPTED};
use crate::paths::Paths;
use crate::usage::{self, AccessExpiry, LoginDeadline};
use crate::{claude, metrics, ui};

use super::run::{self, Profile};

/// Variables that would make Claude Code use another credential than the
/// profile's own login (or another provider), removed from every child.
pub const STRIPPED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "ANTHROPIC_BASE_URL",
];

/// `--refresh-only`: the request Claude Code makes, as small and as isolated
/// as it gets: one turn on the smallest model, no tools, no MCP servers, no
/// hooks, only the user's own settings, and no saved session.
pub const REFRESH_ARGS: &[&str] = &[
    "-p",
    "ok",
    "--model",
    "haiku",
    "--max-turns",
    "1",
    "--tools",
    "",
    "--no-session-persistence",
    "--strict-mcp-config",
    "--setting-sources",
    "user",
    "--settings",
    r#"{"disableAllHooks":true}"#,
];

/// How long `--refresh-only` waits for Claude Code before stopping it.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(120);

/// The profile being renewed.
struct Target<'a> {
    profile: &'a Profile,
    config_dir: PathBuf,
    is_default: bool,
    state_json: PathBuf,
}

pub fn run(paths: &Paths, cfg: &Config, args: &LoginArgs) -> Result<()> {
    let all = run::profiles(cfg);
    let profile = run::find(&all, &args.profile)?;
    let is_default = profile.config_dir.is_none();
    let config_dir = profile
        .config_dir
        .clone()
        .unwrap_or_else(|| paths.default_dir.clone());
    let state_json = if is_default {
        paths.default_json.clone()
    } else {
        config_dir.join(".claude.json")
    };
    let t = Target {
        profile,
        config_dir,
        is_default,
        state_json,
    };
    let bin = claude::require_binary(cfg.env.as_ref().and_then(|e| e.claude_bin.as_deref()))?;
    if usage::keychain_available() {
        ui::info(
            "reading the login's expiry from .credentials.json and the macOS Keychain (timestamps only)",
        );
    }
    // The access token's expiry is read only for `--refresh-only`, which
    // needs it to know whether Claude Code would refresh now.
    let access = if args.refresh_only {
        AccessExpiry::Read
    } else {
        AccessExpiry::Skip
    };
    let before = usage::read_login_times(&t.config_dir, t.is_default, access);
    if args.refresh_only {
        refresh_only(
            paths,
            cfg,
            &t,
            &bin,
            before.deadline,
            before.access_expires_at,
        )
    } else {
        sign_in(paths, cfg, &t, &bin, before.deadline)
    }
}

/// The default: a new sign-in for the profile, in place.
fn sign_in(
    paths: &Paths,
    cfg: &Config,
    t: &Target,
    bin: &Path,
    before: LoginDeadline,
) -> Result<()> {
    let name = &t.profile.name;
    let account_before = metrics::account_info(&t.state_json);
    ui::info(&format!(
        "signing `{name}` in again: Claude Code opens the sign-in page; use the same account"
    ));
    crate::log::write("INFO", &format!("login {name}: claude auth login"));
    let mut cmd = Command::new(bin);
    cmd.args(["auth", "login"]);
    child_env(&mut cmd, paths, t.profile);
    claude::CHILD_RUNNING.store(true, Ordering::SeqCst);
    let status = cmd.status();
    claude::CHILD_RUNNING.store(false, Ordering::SeqCst);
    check_interrupted(status.as_ref().ok())?;
    let status = status.with_context(|| format!("running {} auth login", bin.display()))?;
    if !status.success() {
        crate::log::write("WARN", &format!("login {name}: sign-in failed"));
        return Err(CliError::runtime(format!(
            "signing `{name}` in failed: `claude auth login` exited with {}",
            exit_code(status)
        ))
        .hint(format!(
            "the login is as it was; run `mcc login {name}` to try again"
        ))
        .into());
    }

    let account_after = metrics::account_info(&t.state_json);
    check_account(cfg, t, account_before.as_ref(), account_after.as_ref())?;
    let after = usage::read_login_times(&t.config_dir, t.is_default, AccessExpiry::Skip).deadline;
    judge(
        name,
        Mode::SignIn,
        before,
        after,
        Utc::now(),
        cfg.usage.login_warn_days(),
    )
}

/// `--refresh-only`: let Claude Code refresh the access token itself.
fn refresh_only(
    paths: &Paths,
    cfg: &Config,
    t: &Target,
    bin: &Path,
    before: LoginDeadline,
    access_expires_at: Option<DateTime<Utc>>,
) -> Result<()> {
    let name = &t.profile.name;
    let now = Utc::now();
    if access_expires_at.is_none() && before.at().is_none() {
        return Err(
            CliError::runtime(format!("no Claude Code login found for `{name}`"))
                .hint(format!("sign it in with `mcc login {name}`"))
                .into(),
        );
    }
    if let Some(t) = access_expires_at
        && !usage::access_token_due(t, now)
    {
        crate::log::write(
            "INFO",
            &format!("login {name}: access token still valid, nothing sent"),
        );
        return Err(CliError::runtime(format!(
            "the access token of `{name}` is still valid (for {}), so Claude Code would not refresh it now; nothing was sent",
            usage::until(t, now)
        ))
        .hint(format!("run `mcc login {name}` to renew the login"))
        .into());
    }

    let dir = scratch_dir()?;
    let mut cmd = Command::new(bin);
    cmd.args(REFRESH_ARGS)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    child_env(&mut cmd, paths, t.profile);
    crate::log::write("INFO", &format!("login {name}: refresh-only request"));
    ui::info(&format!(
        "asking Claude Code to refresh `{name}`'s access token (one tiny request)"
    ));
    claude::CHILD_RUNNING.store(true, Ordering::SeqCst);
    let outcome = cmd
        .spawn()
        .and_then(|child| wait_with_timeout(child, REFRESH_TIMEOUT));
    claude::CHILD_RUNNING.store(false, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(&dir);
    check_interrupted(outcome.as_ref().ok().and_then(Option::as_ref))?;
    let status = outcome.with_context(|| format!("running {}", bin.display()))?;
    let Some(status) = status else {
        return Err(CliError::runtime(format!(
            "Claude Code did not finish within {} seconds; mcc stopped it",
            REFRESH_TIMEOUT.as_secs()
        ))
        .hint(format!("run `mcc login {name}` to renew the login"))
        .into());
    };
    if !status.success() {
        return Err(CliError::runtime(format!(
            "Claude Code could not refresh `{name}`'s access token (exit {})",
            exit_code(status)
        ))
        .hint(format!("run `mcc login {name}` to sign in again"))
        .into());
    }
    let after = usage::read_login_times(&t.config_dir, t.is_default, AccessExpiry::Skip).deadline;
    judge(
        name,
        Mode::RefreshOnly,
        before,
        after,
        Utc::now(),
        cfg.usage.login_warn_days(),
    )
}

/// The profile's environment ([`run::profile_env`]), without any variable
/// that would make Claude Code use another credential.
fn child_env(cmd: &mut Command, paths: &Paths, p: &Profile) {
    run::profile_env(cmd, paths, p);
    for var in STRIPPED_ENV {
        cmd.env_remove(var);
    }
}

/// Ctrl+C reached the child (whether it died of it or handled it): stop
/// before evaluating anything.
fn check_interrupted(status: Option<&ExitStatus>) -> Result<()> {
    let interrupted = claude::INTERRUPTED.swap(false, Ordering::SeqCst)
        || status.is_some_and(|s| s.signal() == Some(libc::SIGINT));
    if interrupted {
        crate::log::write("WARN", "login: interrupted");
        return Err(CliError {
            code: EXIT_INTERRUPTED,
            message: "interrupted".into(),
            hint: None,
        }
        .into());
    }
    Ok(())
}

/// Wait for `child` at most `timeout`; `None` when it had to be stopped.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A new, empty, private directory for `claude -p` to run in, so it sees no
/// project of the user's.
fn scratch_dir() -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("mcc-login-{}-{nanos}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// A finished child's exit code; a signal counts as 128 + its number.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

/// An account as messages name it: "a@x.com (Acme)".
fn describe(a: &Account) -> String {
    let who = a
        .email
        .as_deref()
        .or(a.uuid.as_deref())
        .unwrap_or("an unknown account");
    match a.organization.as_deref().or(a.organization_uuid.as_deref()) {
        Some(org) => format!("{who} ({org})"),
        None => who.to_string(),
    }
}

/// Whether the login after the sign-in is another account, or the same one
/// in another organization; `false` unless both are known.
fn switched(before: Option<&Account>, after: Option<&Account>) -> bool {
    match (before, after) {
        (Some(b), Some(a)) if !b.is_anonymous() && !a.is_anonymous() => {
            !b.same_login(a) || b.org_match(a) == OrgMatch::Different
        }
        _ => false,
    }
}

/// Fail when the sign-in landed on another account or organization than
/// before, or when a session-sharing profile now shares across
/// organizations. Nothing is written either way.
fn check_account(
    cfg: &Config,
    t: &Target,
    before: Option<&Account>,
    after: Option<&Account>,
) -> Result<()> {
    let name = &t.profile.name;
    let changed = switched(before, after);
    // Only an mcc profile shares sessions; the default never does.
    let sharing = cfg
        .profiles
        .get(name)
        .filter(|p| p.share_sessions)
        .map(|p| (p, super::share::group_risks(cfg, name, p, &p.share_group)));
    if changed {
        let (old, new) = (describe(before.unwrap()), describe(after.unwrap()));
        crate::log::write(
            "WARN",
            &format!("login {name}: signed in to another account"),
        );
        ui::warn(&format!(
            "`{name}` is now signed in to {new}, not {old} as before"
        ));
        let mut hint = format!("to go back, run `mcc login {name}` and sign in with {old}");
        if let Some((p, _)) = &sharing {
            hint.push_str(&format!(
                "; `{name}` shares its sessions in group \"{}\": stop with `mcc share {name} --off` if it should stay apart",
                super::share::group_name(&p.share_group)
            ));
        }
        return Err(CliError::runtime(format!(
            "`{name}` was signed in to a different account than before"
        ))
        .hint(hint)
        .into());
    }
    let Some((p, (me, others))) = sharing else {
        return Ok(());
    };
    if others.is_empty() {
        return Ok(());
    }
    let group = super::share::group_name(&p.share_group);
    let line = format!(
        "`{name}` ({}) shares its sessions in group \"{group}\" with {}, across organizations",
        me.describe(),
        super::share::risk_list(&others)
    );
    // The same account as before: the group was like this already, so this
    // is only reported, as `mcc share` reports it (once allowed, not at all).
    let known_before = before.is_some_and(|b| !b.is_anonymous());
    if known_before {
        if p.share_cross_org_allowed_at.is_none() {
            ui::warn(&line);
        }
        return Ok(());
    }
    crate::log::write(
        "WARN",
        &format!("login {name}: sessions shared across organizations"),
    );
    ui::warn(&format!(
        "{line}: sessions are now shared across a different organization"
    ));
    Err(CliError::runtime(format!(
        "`{name}` now shares its sessions in group \"{group}\" across organizations"
    ))
    .hint(format!(
        "stop sharing with `mcc share {name} --off`, or sign in with the account the group uses (`mcc login {name}`)"
    ))
    .into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    SignIn,
    RefreshOnly,
}

/// Decide from the deadlines alone: success only when the new one is known,
/// later than before, and outside the warning window. The one exception: a
/// sign-in where no deadline was known before or after (a Claude Code that
/// records none) succeeds, saying the expiry is unknown.
fn judge(
    name: &str,
    mode: Mode,
    before: LoginDeadline,
    after: LoginDeadline,
    now: DateTime<Utc>,
    window_days: u32,
) -> Result<()> {
    let Some(at) = after.at() else {
        if mode == Mode::SignIn && before.at().is_none() {
            crate::log::write("INFO", &format!("login {name}: signed in, expiry unknown"));
            ui::success(&format!(
                "`{name}` is signed in; its login expiry is unknown"
            ));
            ui::hint(
                "Claude Code did not record when this login runs out (an older Claude Code, or a login mcc can't read); `mcc usage` shows it once it can",
            );
            return Ok(());
        }
        crate::log::write("WARN", &format!("login {name}: deadline unreadable after"));
        return Err(match mode {
            Mode::SignIn => CliError::runtime(format!(
                "`{name}` signed in, but mcc could not read its login deadline afterwards"
            ))
            .hint(format!(
                "run `mcc login {name}` again, or check `mcc usage --all` to see when the login runs out"
            )),
            Mode::RefreshOnly => CliError::runtime(format!(
                "Claude Code ran, but mcc can't read when `{name}`'s login runs out"
            ))
            .hint(format!("run `mcc login {name}` to sign in again")),
        }
        .into());
    };
    let when = format!("{} ({})", usage::until(at, now), usage::local_time(at));
    let moved = before.at().is_none_or(|b| at > b);
    if !moved {
        crate::log::write("WARN", &format!("login {name}: deadline unchanged"));
        return Err(match mode {
            Mode::SignIn => CliError::runtime(format!(
                "`{name}` signed in, but its login deadline did not move (it still runs out in {when})"
            ))
            .hint(format!(
                "run `mcc login {name}` again and finish the sign-in in the browser; if it keeps happening, sign out first with `mcc exec {name} -- claude auth logout`"
            )),
            Mode::RefreshOnly => CliError::runtime(format!(
                "access token refreshed but login deadline unchanged (`{name}` still runs out in {when})"
            ))
            .hint(format!("run `mcc login {name}` to sign in again")),
        }
        .into());
    }
    if usage::login_warning(at, now, window_days).is_some() {
        crate::log::write("WARN", &format!("login {name}: deadline still soon"));
        return Err(CliError::runtime(format!(
            "`{name}`'s login moved, but it still runs out in {when}"
        ))
        .hint(format!("run `mcc login {name}` to sign in again"))
        .into());
    }
    crate::log::write("INFO", &format!("login {name}: renewed"));
    ui::success(&match mode {
        Mode::SignIn => format!("Login for `{name}` renewed: it now runs out in {when}"),
        Mode::RefreshOnly => format!("Login for `{name}` extended: it now runs out in {when}"),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{DeadlineSource, UnknownDeadline};

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn known(s: &str) -> LoginDeadline {
        LoginDeadline::Known {
            at: at(s),
            source: DeadlineSource::CredentialsFile,
        }
    }

    const UNKNOWN: LoginDeadline = LoginDeadline::Unknown(UnknownDeadline::NotRecorded);

    fn code(r: Result<()>) -> i32 {
        r.map_or_else(|e| crate::error::classify(&e).0, |()| 0)
    }

    #[test]
    fn success_is_decided_by_the_deadline_alone() {
        let now = at("2026-10-10T12:00:00Z");
        let soon = "2026-10-11T00:00:00Z";
        let fresh = "2026-11-09T12:00:00Z";
        for mode in [Mode::SignIn, Mode::RefreshOnly] {
            // Later and outside the window: renewed.
            assert_eq!(code(judge("w", mode, known(soon), known(fresh), now, 2)), 0);
            // No earlier deadline to compare with: a fresh one counts.
            assert_eq!(code(judge("w", mode, UNKNOWN, known(fresh), now, 2)), 0);
            // Unchanged, or earlier: failure.
            assert_eq!(code(judge("w", mode, known(soon), known(soon), now, 2)), 1);
            assert_eq!(code(judge("w", mode, known(fresh), known(soon), now, 2)), 1);
            // Later, but still inside the window: failure.
            assert_eq!(
                code(judge(
                    "w",
                    mode,
                    known(soon),
                    known("2026-10-11T12:00:00Z"),
                    now,
                    2
                )),
                1
            );
        }
        // Known before, unknown afterwards: nothing shows the login was
        // renewed, so both fail.
        for mode in [Mode::SignIn, Mode::RefreshOnly] {
            assert_eq!(code(judge("w", mode, known(soon), UNKNOWN, now, 2)), 1);
        }
        // Unknown before and after: a sign-in still worked (a Claude Code that
        // records no deadline); a refresh can't tell.
        assert_eq!(code(judge("w", Mode::SignIn, UNKNOWN, UNKNOWN, now, 2)), 0);
        assert_eq!(
            code(judge("w", Mode::RefreshOnly, UNKNOWN, UNKNOWN, now, 2)),
            1
        );
        // The widest window still leaves a fresh 30-day login outside it.
        assert_eq!(
            code(judge(
                "w",
                Mode::SignIn,
                known(soon),
                known(fresh),
                now,
                usage::LOGIN_WARN_DAYS_MAX
            )),
            0
        );
    }

    #[test]
    fn unchanged_deadline_message_points_to_a_new_sign_in() {
        let now = at("2026-10-10T12:00:00Z");
        let soon = known("2026-10-11T00:00:00Z");
        let err = judge("work", Mode::RefreshOnly, soon, soon, now, 2).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("access token refreshed but login deadline unchanged"),
            "{err}"
        );
        assert_eq!(
            crate::error::classify(&err).1.as_deref(),
            Some("run `mcc login work` to sign in again")
        );
    }

    #[test]
    fn a_deadline_lost_after_sign_in_is_a_failure_pointing_to_a_retry() {
        let now = at("2026-10-10T12:00:00Z");
        let soon = known("2026-10-11T00:00:00Z");
        let err = judge("work", Mode::SignIn, soon, UNKNOWN, now, 2).unwrap_err();
        assert_eq!(
            err.to_string(),
            "`work` signed in, but mcc could not read its login deadline afterwards"
        );
        let hint = crate::error::classify(&err).1.unwrap();
        assert!(hint.contains("`mcc login work`"), "{hint}");
        assert!(hint.contains("`mcc usage --all`"), "{hint}");
    }

    fn account(email: &str, org: Option<&str>) -> Account {
        Account {
            email: Some(email.into()),
            organization_uuid: org.map(str::to_string),
            ..Account::default()
        }
    }

    #[test]
    fn a_switch_is_another_login_or_organization() {
        let a = account("a@x.com", Some("org-1"));
        assert!(!switched(
            Some(&a),
            Some(&account("A@x.com", Some("org-1")))
        ));
        assert!(switched(Some(&a), Some(&account("b@x.com", Some("org-1")))));
        assert!(switched(Some(&a), Some(&account("a@x.com", Some("org-2")))));
        // Personal before, a work seat after (or the reverse): another org.
        assert!(switched(Some(&account("a@x.com", None)), Some(&a)));
        // Not known on both sides: can't tell.
        assert!(!switched(None, Some(&a)));
        assert!(!switched(Some(&a), None));
        assert!(!switched(Some(&Account::default()), Some(&a)));
    }

    #[test]
    fn ctrl_c_during_the_child_stops_before_anything_is_evaluated() {
        claude::INTERRUPTED.store(true, Ordering::SeqCst);
        let err = check_interrupted(None).unwrap_err();
        assert_eq!(crate::error::classify(&err).0, EXIT_INTERRUPTED);
        // The flag is consumed.
        assert!(check_interrupted(None).is_ok());
        let killed = ExitStatus::from_raw(libc::SIGINT);
        assert_eq!(
            crate::error::classify(&check_interrupted(Some(&killed)).unwrap_err()).0,
            EXIT_INTERRUPTED
        );
        assert!(check_interrupted(Some(&ExitStatus::from_raw(0))).is_ok());
    }

    #[test]
    fn a_child_that_hangs_is_stopped() {
        let child = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let start = Instant::now();
        let out = wait_with_timeout(child, Duration::from_millis(200)).unwrap();
        assert!(out.is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
        let child = Command::new("/usr/bin/true").spawn().unwrap();
        assert!(
            wait_with_timeout(child, Duration::from_secs(5))
                .unwrap()
                .is_some_and(|s| s.success())
        );
    }

    #[test]
    fn the_refresh_request_is_isolated() {
        for flag in [
            "--no-session-persistence",
            "--strict-mcp-config",
            "--tools",
            "--max-turns",
        ] {
            assert!(REFRESH_ARGS.contains(&flag), "{flag}");
        }
        assert!(!REFRESH_ARGS.contains(&"--bare"));
        let dir = scratch_dir().unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir(&dir).unwrap();
    }
}
