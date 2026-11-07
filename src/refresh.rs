//! Let Claude Code refresh a profile's access token itself: one tiny,
//! isolated `claude -p` request run as the profile. `mcc login
//! --refresh-only` (#100) makes it on request; `mcc usage --all` makes it for
//! each account its live lookup finds with an expired token
//! ([`refresh_expired`], #106). mcc never sends a refresh token or calls the
//! OAuth endpoint itself: Claude Code does the refresh.
//!
//! Every request holds the profile's `~/.mcc/state/<profile>/refresh.lock`
//! ([`try_lock`]), so two mcc commands never refresh the same profile at
//! once; the lock is only tried, never waited for.
//!
//! `mcc usage --all` runs the request in the first project mapped to the
//! profile (`mcc project set`), where `mcc run` starts that very profile, or,
//! for a profile no project maps to, in a new empty folder outside every
//! project, started as that profile the way `mcc run --profile` does. In a
//! project, Claude Code reads it as it would for any session there (its git
//! status, its CLAUDE.md), but only the user's own settings, no tools, no
//! MCP servers and no hooks. A
//! failed refresh is not tried again for [`FAILURE_COOLDOWN`], and one run
//! spends at most [`RUN_BUDGET`] on refreshes.

use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::claude;
use crate::commands::run::{self, Profile};
use crate::config::{Config, FileLock};
use crate::paths::Paths;
use crate::ui;
use crate::usage::{self, AccessExpiry, Reason, Usage};

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

/// The request Claude Code makes, as small and as isolated as it gets: one
/// turn on the smallest model, no tools, no MCP servers, no hooks, only the
/// user's own settings, and no saved session.
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

/// How long one request may run before mcc stops it.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(120);

/// The most one `mcc usage --all` spends on refreshes, all accounts together.
pub const RUN_BUDGET: Duration = Duration::from_secs(180);

/// No request starts with less of [`RUN_BUDGET`] left than this.
pub const MIN_REQUEST_TIME: Duration = Duration::from_secs(15);

/// How long `mcc usage --all` leaves a profile alone after a failed refresh
/// (30 minutes), so a broken login doesn't spend quota on every run.
pub const FAILURE_COOLDOWN: chrono::Duration = chrono::Duration::minutes(30);

/// `~/.mcc/state/<profile>/refresh-failed-at`: when `mcc usage --all` last
/// failed to refresh the profile (RFC 3339); removed by a refresh that works.
pub const FAILED_AT_FILE: &str = "refresh-failed-at";

/// The profile's environment ([`run::profile_env`]), without any variable
/// that would make Claude Code use another credential.
pub(crate) fn child_env(cmd: &mut Command, paths: &Paths, p: &Profile) {
    run::profile_env(cmd, paths, p);
    for var in STRIPPED_ENV {
        cmd.env_remove(var);
    }
}

/// Proof that this process holds a profile's refresh lock; [`spawn`] takes
/// it so no request runs without it.
#[derive(Debug)]
pub(crate) struct RefreshLock {
    _lock: FileLock,
}

/// `~/.mcc/state/<profile>/refresh.lock`.
pub(crate) fn lock_path(paths: &Paths, profile: &str) -> PathBuf {
    paths.profile_state_dir(profile).join("refresh.lock")
}

/// Take `profile`'s refresh lock only if it is free: `None` (at once) while
/// another mcc command refreshes it. Never the config lock.
pub(crate) fn try_lock(paths: &Paths, profile: &str) -> Result<Option<RefreshLock>> {
    Ok(crate::config::try_lock_file(&lock_path(paths, profile))?.map(|l| RefreshLock { _lock: l }))
}

/// How a request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Claude Code exited on its own.
    Exited(ExitStatus),
    /// It ran longer than its timeout and was stopped.
    TimedOut,
    /// Ctrl+C reached it (whether it died of it or handled it).
    Interrupted,
}

/// Run the request as `profile` in `dir`, output discarded, stopped after
/// `timeout`. Ctrl+C while it runs reaches the child (the handler only
/// records it) and is reported as [`Outcome::Interrupted`]. Prints nothing.
pub(crate) fn spawn(
    bin: &Path,
    paths: &Paths,
    profile: &Profile,
    dir: &Path,
    timeout: Duration,
    _lock: &RefreshLock,
) -> std::io::Result<Outcome> {
    let mut cmd = Command::new(bin);
    cmd.args(REFRESH_ARGS)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    child_env(&mut cmd, paths, profile);
    claude::CHILD_RUNNING.store(true, Ordering::SeqCst);
    let outcome = cmd
        .spawn()
        .and_then(|child| wait_with_timeout(child, timeout));
    claude::CHILD_RUNNING.store(false, Ordering::SeqCst);
    let interrupted = claude::INTERRUPTED.swap(false, Ordering::SeqCst)
        || matches!(&outcome, Ok(Some(s)) if s.signal() == Some(libc::SIGINT));
    if interrupted {
        return Ok(Outcome::Interrupted);
    }
    Ok(match outcome? {
        Some(status) => Outcome::Exited(status),
        None => Outcome::TimedOut,
    })
}

/// Wait for `child` at most `timeout`; `None` when it had to be stopped.
pub(crate) fn wait_with_timeout(
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
pub(crate) fn scratch_dir() -> Result<PathBuf> {
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
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

/// How long the next request may run, `elapsed` into a run's
/// [`RUN_BUDGET`]: what is left, at most [`REFRESH_TIMEOUT`]; `None` when
/// less than [`MIN_REQUEST_TIME`] is left.
pub(crate) fn request_timeout(elapsed: Duration) -> Option<Duration> {
    let left = RUN_BUDGET.checked_sub(elapsed)?;
    (left >= MIN_REQUEST_TIME).then(|| left.min(REFRESH_TIMEOUT))
}

/// When a profile that failed to refresh at `failed_at` may be tried again,
/// while that is still ahead of `now`; `None` once it may. A time from the
/// future (a clock set back) is ignored.
pub(crate) fn retry_at(
    failed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    failed_at
        .filter(|t| *t <= now)
        .map(|t| t + FAILURE_COOLDOWN)
        .filter(|r| *r > now)
}

fn failed_at_path(paths: &Paths, profile: &str) -> PathBuf {
    paths.profile_state_dir(profile).join(FAILED_AT_FILE)
}

fn read_failed_at(paths: &Paths, profile: &str) -> Option<DateTime<Utc>> {
    let text = std::fs::read_to_string(failed_at_path(paths, profile)).ok()?;
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Remember a failed refresh (best effort: a failed write is logged).
fn record_failure(paths: &Paths, profile: &str, now: DateTime<Utc>) {
    let path = failed_at_path(paths, profile);
    let write = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::write(
                &path,
                now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )
        });
    if let Err(e) = write {
        crate::log::write(
            "WARN",
            &format!("refresh {profile}: failure not recorded: {e}"),
        );
    }
}

/// Where `mcc usage --all` refreshes a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Place {
    /// A project mapped to it (`mcc project set`), as stored (canonical).
    Project(PathBuf),
    /// A new empty folder outside every project: no project maps to the
    /// profile, so it is started there by name (`mcc run --profile`).
    Scratch,
}

/// Where `profile` is refreshed: the first existing project folder mapped
/// to it (in the config's order), else a new empty folder.
pub(crate) fn place(cfg: &Config, profile: &str) -> Place {
    let project = cfg
        .projects
        .iter()
        .filter(|(_, p)| p.profile == profile)
        .map(|(dir, _)| Path::new(dir))
        .find(|dir| dir.is_dir());
    match project {
        Some(dir) => Place::Project(dir.to_path_buf()),
        None => Place::Scratch,
    }
}

/// Whether `mcc run` in `dir` starts `profile` ([`run::applies_in`]),
/// checked on the folder with symlinks resolved, as project keys are.
fn starts_there(cfg: &Config, dir: &Path, profile: &str) -> bool {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    run::applies_in(cfg, &dir).0 == profile
}

/// Whether `dir` lies outside every project (`mcc project set`), checked
/// with symlinks resolved: a new empty folder must not land in one.
fn outside_projects(cfg: &Config, dir: &Path) -> bool {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    crate::commands::project::lookup(cfg, &dir).is_none()
}

/// Whether a request worked: the live lookup no longer finds the token
/// expired, and the access token's `expiresAt` moved later (or appeared).
pub(crate) fn refreshed(
    before: Option<DateTime<Utc>>,
    after: Option<DateTime<Utc>>,
    lookup: &Usage,
) -> bool {
    let moved = match (before, after) {
        (Some(b), Some(a)) => a > b,
        (None, Some(_)) => true,
        (_, None) => false,
    };
    moved && lookup.reason != Some(Reason::TokenExpired)
}

/// How to refresh `profile` by hand: start it once in Claude Code.
pub(crate) fn manual_hint(profile: &str) -> String {
    format!("start `{profile}` in Claude Code once (`mcc run --profile {profile}`) to refresh it")
}

/// Not refreshed: say why, and how to do it by hand.
fn skip(profile: &str, why: &str, hint: &str) {
    crate::log::write("INFO", &format!("refresh {profile}: skipped"));
    ui::warn(why);
    ui::hint(hint);
}

/// Skips `profile` (and says so) while a recent failure holds it back.
fn cooling_down(paths: &Paths, profile: &str, now: DateTime<Utc>, manual: &str) -> bool {
    let Some(at) = retry_at(read_failed_at(paths, profile), now) else {
        return false;
    };
    skip(
        profile,
        &format!(
            "refreshing `{profile}` failed less than {} minutes ago; mcc tries again after {}",
            FAILURE_COOLDOWN.num_minutes(),
            usage::local_time(at)
        ),
        manual,
    );
    true
}

/// Take `profile`'s refresh lock for a request, or skip it (and say so):
/// while another run holds the lock, when the lock can't be taken, or when
/// a failure is recorded by the time it is held. A run that held the lock
/// until now may have just recorded one, and its request must not be sent
/// twice.
fn lock_to_refresh(paths: &Paths, profile: &str, manual: &str) -> Option<RefreshLock> {
    let lock = match try_lock(paths, profile) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            skip(
                profile,
                &format!("a refresh of `{profile}` is already running"),
                manual,
            );
            return None;
        }
        Err(e) => {
            crate::log::write("WARN", &format!("refresh {profile}: {e:#}"));
            skip(
                profile,
                &format!("can't take the refresh lock of `{profile}`"),
                manual,
            );
            return None;
        }
    };
    // Again under the lock: the check before it may predate the failure.
    if cooling_down(paths, profile, Utc::now(), manual) {
        return None;
    }
    Some(lock)
}

/// `mcc usage --all` (#106): for each target whose live lookup in `found`
/// is [`Reason::TokenExpired`], let Claude Code refresh the token with one
/// request in a project mapped to that profile or a new empty folder
/// ([`place`]),
/// then look it up live again and put that in `found`. One at a time, each
/// announced on stderr before it starts; a profile that can't or shouldn't
/// be refreshed is skipped with a warning naming it and how to refresh it
/// by hand. Never fails: returns `true` when Ctrl+C stopped a request, and
/// no further one starts.
pub(crate) fn refresh_expired(
    paths: &Paths,
    cfg: &Config,
    targets: &[usage::Target],
    found: &mut [Usage],
) -> bool {
    let expired: Vec<usize> = (0..found.len().min(targets.len()))
        .filter(|&i| found[i].reason == Some(Reason::TokenExpired))
        .collect();
    if expired.is_empty() {
        return false;
    }
    let profiles = run::profiles(cfg);
    let bin = claude::require_binary(cfg.env.as_ref().and_then(|e| e.claude_bin.as_deref())).ok();
    let start = std::time::Instant::now();
    for i in expired {
        let t = &targets[i];
        let name = t.name.as_str();
        let Some(profile) = profiles.iter().find(|p| p.name == name) else {
            continue;
        };
        if refresh_one(paths, cfg, profile, t, bin.as_deref(), start, &mut found[i]) {
            return true;
        }
    }
    false
}

/// [`refresh_expired`] for one profile; `true` when Ctrl+C stopped it.
fn refresh_one(
    paths: &Paths,
    cfg: &Config,
    profile: &Profile,
    t: &usage::Target,
    bin: Option<&Path>,
    start: std::time::Instant,
    found: &mut Usage,
) -> bool {
    let name = t.name.as_str();
    let manual = manual_hint(name);
    // The checks `mcc login --refresh-only` makes before any request.
    let now = Utc::now();
    let times = usage::read_login_times(&t.config_dir, t.is_default, AccessExpiry::Read);
    let before = times.access_expires_at;
    if before.is_none() && times.deadline.at().is_none() {
        skip(
            name,
            &format!("no Claude Code login found for `{name}`, so its token can't be refreshed"),
            &format!("sign it in with `mcc login {name}`"),
        );
        return false;
    }
    if times.deadline.at().is_some_and(|at| at <= now) {
        skip(
            name,
            &format!("the login of `{name}` has expired, so Claude Code can't refresh its token"),
            &format!("sign in again with `mcc login {name}`"),
        );
        return false;
    }
    if let Some(at) = before
        && !usage::access_token_due(at, now)
    {
        skip(
            name,
            &format!(
                "the usage API rejected `{name}`'s access token, which is still valid for {}, so Claude Code would not refresh it now; nothing was sent",
                usage::until(at, now)
            ),
            &format!("if it keeps happening, sign in again with `mcc login {name}`"),
        );
        return false;
    }
    if cooling_down(paths, name, now, &manual) {
        return false;
    }
    let Some(timeout) = request_timeout(start.elapsed()) else {
        skip(
            name,
            &format!(
                "no time left to refresh `{name}` (mcc spends at most {} seconds on refreshes per run)",
                RUN_BUDGET.as_secs()
            ),
            &manual,
        );
        return false;
    };
    let place = place(cfg, name);
    let Some(bin) = bin else {
        skip(
            name,
            &format!("the `claude` command was not found, so `{name}`'s token can't be refreshed"),
            &manual,
        );
        return false;
    };
    let Some(lock) = lock_to_refresh(paths, name, &manual) else {
        return false;
    };
    // Only a folder made here is ever removed.
    let (dir, owned) = match &place {
        Place::Project(dir) => (dir.clone(), false),
        Place::Scratch => match scratch_dir() {
            Ok(dir) => (dir, true),
            Err(e) => {
                crate::log::write("WARN", &format!("refresh {name}: {e:#}"));
                skip(
                    name,
                    &format!("can't make a folder to refresh `{name}` in"),
                    &manual,
                );
                return false;
            }
        },
    };
    let remove = |dir: &Path| {
        if owned {
            let _ = std::fs::remove_dir_all(dir);
        }
    };
    let fits = match place {
        Place::Project(_) => starts_there(cfg, &dir, name),
        Place::Scratch => outside_projects(cfg, &dir),
    };
    if !fits {
        remove(&dir);
        let why = if owned {
            format!(
                "the new empty folder {} is inside a project, so mcc did not refresh `{name}` there",
                paths.display(&dir)
            )
        } else {
            format!(
                "`mcc run` does not start `{name}` in {}, so mcc did not refresh it there",
                paths.display(&dir)
            )
        };
        skip(name, &why, &manual);
        return false;
    }

    // Shown even with --quiet: a request is sent on the user's behalf.
    ui::warn(&format!(
        "refreshing the token of `{name}`: starting a one-turn Claude Code session in {} (`claude -p ok`; it uses a little of `{name}`'s quota)",
        paths.display(&dir)
    ));
    crate::log::write("INFO", &format!("refresh {name}: request started"));
    let outcome = spawn(bin, paths, profile, &dir, timeout, &lock);
    remove(&dir);
    let failure = match outcome {
        Ok(Outcome::Interrupted) => {
            crate::log::write("WARN", &format!("refresh {name}: interrupted"));
            ui::warn(&format!(
                "refreshing `{name}` was interrupted; no further token is refreshed"
            ));
            return true;
        }
        Err(e) => {
            crate::log::write("WARN", &format!("refresh {name}: not started: {e}"));
            skip(
                name,
                &format!("Claude Code could not be started to refresh `{name}`"),
                &manual,
            );
            return false;
        }
        Ok(Outcome::TimedOut) => format!(
            "Claude Code did not finish refreshing `{name}` within {} seconds; mcc stopped it",
            timeout.as_secs()
        ),
        Ok(Outcome::Exited(s)) if !s.success() => format!(
            "Claude Code could not refresh `{name}`'s token (exit {})",
            exit_code(s)
        ),
        Ok(Outcome::Exited(_)) => format!("Claude Code ran, but `{name}`'s token is still expired"),
    };
    // The live lookup again, for this profile alone: the final judge.
    let lookup = usage::fetch_all(std::slice::from_ref(t))
        .pop()
        .unwrap_or_else(|| Usage::unavailable(Reason::Network));
    let after =
        usage::read_login_times(&t.config_dir, t.is_default, AccessExpiry::Read).access_expires_at;
    let ok = refreshed(before, after, &lookup);
    *found = lookup;
    // The cooldown is updated before the lock is released, so the next
    // holder's re-read above sees it.
    if ok {
        let _ = std::fs::remove_file(failed_at_path(paths, name));
    } else {
        record_failure(paths, name, Utc::now());
    }
    drop(lock);
    if ok {
        crate::log::write("INFO", &format!("refresh {name}: refreshed"));
        ui::success(&format!("refreshed `{name}`"));
    } else {
        crate::log::write("WARN", &format!("refresh {name}: failed"));
        ui::warn(&failure);
        ui::hint(&manual);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_profile_is_refreshed_by_one_command_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        let held = try_lock(&paths, "work").unwrap();
        assert!(held.is_some());
        assert_eq!(
            lock_path(&paths, "work"),
            tmp.path().join(".mcc/state/work/refresh.lock")
        );
        // Held: another try gets nothing, without waiting; another profile
        // is free.
        assert!(try_lock(&paths, "work").unwrap().is_none());
        assert!(try_lock(&paths, "lab").unwrap().is_some());
        drop(held);
        assert!(try_lock(&paths, "work").unwrap().is_some());
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_request_never_outlasts_the_run_budget() {
        let secs = Duration::from_secs;
        // Plenty left: the full timeout.
        assert_eq!(request_timeout(secs(0)), Some(REFRESH_TIMEOUT));
        assert_eq!(request_timeout(secs(60)), Some(REFRESH_TIMEOUT));
        // Less left than a full timeout: only what is left.
        assert_eq!(request_timeout(secs(100)), Some(secs(80)));
        assert_eq!(
            request_timeout(RUN_BUDGET - MIN_REQUEST_TIME),
            Some(MIN_REQUEST_TIME)
        );
        // Below the floor, or past the budget: no request.
        assert_eq!(
            request_timeout(RUN_BUDGET - MIN_REQUEST_TIME + Duration::from_millis(1)),
            None
        );
        assert_eq!(request_timeout(RUN_BUDGET), None);
        assert_eq!(request_timeout(secs(1000)), None);
        // Whatever the elapsed time, never more than what is left.
        for s in 0..=200 {
            if let Some(t) = request_timeout(secs(s)) {
                assert!(t <= REFRESH_TIMEOUT && t <= RUN_BUDGET - secs(s), "{s}");
                assert!(t >= MIN_REQUEST_TIME, "{s}");
            }
        }
    }

    #[test]
    fn a_failed_refresh_waits_thirty_minutes() {
        let now = at("2026-10-10T12:00:00Z");
        assert_eq!(retry_at(None, now), None);
        assert_eq!(
            retry_at(Some(at("2026-10-10T11:45:00Z")), now),
            Some(at("2026-10-10T12:15:00Z"))
        );
        assert_eq!(retry_at(Some(at("2026-10-10T11:30:00Z")), now), None);
        assert_eq!(retry_at(Some(at("2026-10-10T08:00:00Z")), now), None);
        // A time from the future doesn't block for longer.
        assert_eq!(retry_at(Some(at("2026-10-11T12:00:00Z")), now), None);
    }

    #[test]
    fn a_failure_is_recorded_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(read_failed_at(&paths, "work"), None);
        let now = at("2026-10-10T12:00:00Z");
        record_failure(&paths, "work", now);
        assert_eq!(read_failed_at(&paths, "work"), Some(now));
        assert!(
            tmp.path()
                .join(".mcc/state/work/refresh-failed-at")
                .is_file()
        );
        assert_eq!(read_failed_at(&paths, "lab"), None);
    }

    #[test]
    fn a_failure_recorded_while_the_lock_was_held_stops_the_request() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        let manual = manual_hint("work");
        // Nothing recorded, lock free: the request may go.
        let lock = lock_to_refresh(&paths, "work", &manual);
        assert!(lock.is_some());
        drop(lock);
        // The check before the lock passed (nothing was recorded then).
        assert!(!cooling_down(&paths, "work", Utc::now(), &manual));
        // Meanwhile another run holds the lock, fails, records it, and
        // lets go.
        let other = try_lock(&paths, "work").unwrap().unwrap();
        assert!(lock_to_refresh(&paths, "work", &manual).is_none());
        record_failure(&paths, "work", Utc::now());
        drop(other);
        // Now the lock is free, but the failure under it stops the request,
        // and the lock is not kept.
        assert!(lock_to_refresh(&paths, "work", &manual).is_none());
        assert!(try_lock(&paths, "work").unwrap().is_some());
        // Another profile is not held back.
        assert!(lock_to_refresh(&paths, "lab", &manual).is_some());
    }

    #[test]
    fn success_needs_a_later_expiry_and_a_lookup_that_is_not_expired() {
        let (old, new) = (
            Some(at("2026-10-10T11:00:00Z")),
            Some(at("2026-10-10T20:00:00Z")),
        );
        let ok = Usage::ok(None, None);
        let expired = Usage::unavailable(Reason::TokenExpired);
        assert!(refreshed(old, new, &ok));
        assert!(refreshed(None, new, &ok));
        // Another reason than an expired token still counts: the token works.
        assert!(refreshed(
            old,
            new,
            &Usage::unavailable(Reason::RateLimited)
        ));
        assert!(!refreshed(old, new, &expired));
        assert!(!refreshed(old, old, &ok));
        assert!(!refreshed(new, old, &ok));
        assert!(!refreshed(old, None, &ok));
        assert!(!refreshed(None, None, &ok));
    }

    #[test]
    fn the_folder_is_a_project_of_the_profile_or_a_new_empty_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (a, b, gone) = (root.join("a"), root.join("b"), root.join("gone"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut cfg: Config = toml::from_str(&format!(
            "[projects.\"{}\"]\nprofile = \"work\"\n\
             [projects.\"{}\"]\nprofile = \"lab\"\n\
             [projects.\"{}\"]\nprofile = \"lab\"\n",
            a.display(),
            b.display(),
            gone.display()
        ))
        .unwrap();
        assert_eq!(place(&cfg, "work"), Place::Project(a.clone()));
        // A folder that no longer exists is passed over.
        assert_eq!(place(&cfg, "lab"), Place::Project(b.clone()));
        // Mapped nowhere: a new empty folder, whatever the default is.
        assert_eq!(place(&cfg, "default"), Place::Scratch);
        assert_eq!(place(&cfg, "side"), Place::Scratch);
        cfg.default_profile = Some("side".into());
        assert_eq!(place(&cfg, "side"), Place::Scratch);
        assert_eq!(place(&cfg, "default"), Place::Scratch);
        // `mcc run` starts the profile there, and nowhere else.
        assert!(starts_there(&cfg, &a, "work"));
        assert!(!starts_there(&cfg, &a, "lab"));
        assert!(starts_there(&cfg, &root, "side"));
        // An empty folder must lie outside every project.
        assert!(outside_projects(&cfg, &root));
        assert!(!outside_projects(&cfg, &a.join("x")));
    }

    #[test]
    fn the_manual_way_starts_the_profile_never_refresh_only() {
        for name in ["work", "default"] {
            let hint = manual_hint(name);
            assert!(
                hint.contains(&format!("`mcc run --profile {name}`")),
                "{hint}"
            );
            assert!(!hint.contains("--refresh-only"), "{hint}");
        }
    }
}
