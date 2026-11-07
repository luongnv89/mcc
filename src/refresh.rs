//! Let Claude Code refresh a profile's access token itself: one tiny,
//! isolated `claude -p` request run as the profile. `mcc login
//! --refresh-only` (#100) uses it; mcc never sends or refreshes a token.
//!
//! Every request holds the profile's `~/.mcc/state/<profile>/refresh.lock`
//! ([`try_lock`]), so two mcc commands never refresh the same profile at
//! once; the lock is only tried, never waited for.

use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::claude;
use crate::commands::run::{self, Profile};
use crate::config::FileLock;
use crate::paths::Paths;

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
}
