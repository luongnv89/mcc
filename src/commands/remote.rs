//! `mcc remote PROFILE MACHINE TASK` (#142): have a profile do a task on
//! another machine, one that only needs sshd and a shell.
//!
//! Claude Code runs here, as the profile, in headless mode (`claude -p`)
//! with no tools of its own ([`claude_args`]). Its one tool is an MCP
//! server mcc serves itself ([`crate::remote_tool`]), which runs one
//! command at a time on the machine over ssh. mcc sends no login or token
//! anywhere; the task reaches Claude Code on stdin, never on a command
//! line or in mcc's log.
//!
//! Claude Code runs in its own process group, from a private folder under
//! `~/.mcc/state/remote/` that holds its MCP config and the tool's record
//! of the commands it ran, removed on every way out. Ctrl+C, SIGTERM,
//! SIGHUP and `--timeout` stop the whole group: Claude Code, the tool and
//! any ssh it started.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::cli::RemoteArgs;
use crate::config::{Config, Machine};
use crate::error::{CliError, EXIT_INTERRUPTED};
use crate::machines;
use crate::paths::Paths;
use crate::{claude, log, ui};

use super::exec;
use super::run::{self, Profile};

/// Printed on stderr by every run.
pub const REMOTE_DISCLOSURE: &str = crate::cli::remote_disclosure!();

/// The MCP server's name in Claude Code's config.
const SERVER: &str = "mcc-remote";

/// The longest task accepted, in bytes.
const MAX_TASK: u64 = 1024 * 1024;

/// The most of Claude Code's stdout (its result JSON) mcc reads.
const MAX_RESULT: u64 = 64 * 1024 * 1024;

/// How long Claude Code gets to stop after SIGTERM before SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// How often the run is checked on.
const POLL: Duration = Duration::from_millis(50);

/// Exit status after `--timeout`, as `timeout(1)` uses it.
const EXIT_TIMEOUT: i32 = 124;

/// The longest a refused tool call's input is shown.
const SHOWN_INPUT: usize = 200;

pub fn run(paths: &Paths, cfg: &Config, args: &RemoteArgs) -> Result<()> {
    let code = run_task(paths, cfg, args)?;
    let _ = std::io::stdout().flush();
    if code != 0 {
        // The outcome is already reported; only the status is left.
        std::process::exit(code);
    }
    Ok(())
}

/// Run the task; the exit status once its outcome has been printed.
fn run_task(paths: &Paths, cfg: &Config, args: &RemoteArgs) -> Result<i32> {
    let all = run::profiles(cfg);
    let p = run::find(&all, &args.profile)?;
    let (name, m) = machine(cfg, &args.machine)?;
    m.check(name)
        .and_then(|()| machines::check_agent_host(&m.host))
        .map_err(|why| {
            CliError::input(format!("[machines.{name}] can't be used: {why}"))
                .hint("fix it with `mcc config edit`")
        })?;
    let host = m.host.as_str();
    let ssh = machines::resolve_ssh()?;
    run::note_if_not_the_projects(cfg, &p.name);
    eprintln!("{REMOTE_DISCLOSURE}");
    let task = read_task(args)?;

    if let Err(e) = machines::preflight(&ssh, host) {
        log::write(
            "WARN",
            &format!("remote: machine {name} not reachable: {}", e.code()),
        );
        return Err(
            CliError::runtime(machines::describe_preflight(&e, name, host))
                .hint(format!(
                    "Claude Code was not started; check that `ssh {host} true` works from this machine"
                ))
                .into(),
        );
    }
    let bin = run::claude_binary(cfg).map_err(|e| {
        let (_, hint) = crate::error::classify(&e);
        anyhow::Error::new(CliError {
            code: exec::EXIT_NOT_FOUND,
            message: e.to_string(),
            hint,
        })
    })?;

    let signals = Signals::install();
    claude::INTERRUPTED.store(false, Ordering::SeqCst);
    claude::CHILD_RUNNING.store(true, Ordering::SeqCst);
    let ran = start_and_wait(paths, p, &bin, &ssh, (name, host), &task, args);
    claude::CHILD_RUNNING.store(false, Ordering::SeqCst);
    drop(signals);
    let (end, stdout, commands) = ran?;
    ui::detail(&format!("Claude Code ran as `{}` on {name}", p.name));
    match end {
        Ended::Exited(status) => Ok(report(&p.name, name, status, &stdout, &commands, args)),
        Ended::Stopped(why) => {
            let code = why.code();
            log::write(
                "WARN",
                &format!(
                    "remote: profile {} on machine {name}: stopped ({}), exit {code}",
                    p.name,
                    why.log_name()
                ),
            );
            ui::error(
                &format!(
                    "stopped: {}; a command may still be running on {name} ({host})",
                    why.describe(args.timeout)
                ),
                Some(&format!(
                    "check on {host} (e.g. `ssh {host} ps -ef`) before running the task again"
                )),
            );
            Ok(code)
        }
    }
}

/// `[machines.NAME]`, by name.
fn machine<'c>(cfg: &'c Config, name: &'c str) -> Result<(&'c str, &'c Machine)> {
    if let Some(m) = cfg.machines.get(name) {
        return Ok((name, m));
    }
    let known: Vec<String> = cfg.machines.keys().map(|k| format!("`{k}`")).collect();
    let hint = if known.is_empty() {
        "no machines are set up: add a [machines.NAME] table with host = \"user@host\" to ~/.mcc/config.toml (`mcc config edit`)".to_string()
    } else {
        format!(
            "the machines set up are {}; add another with `mcc config edit`: a [machines.NAME] table with host = \"user@host\"",
            known.join(", ")
        )
    };
    Err(CliError::usage(format!("no machine named \"{name}\""))
        .hint(hint)
        .into())
}

/// The task: TASK, the file `--task-file` names, or stdin for `-`. Never
/// empty, at most [`MAX_TASK`] bytes, and text.
fn read_task(args: &RemoteArgs) -> Result<String> {
    let (bytes, from) = match (args.task.as_deref(), &args.task_file) {
        (Some("-"), _) => (read_capped(std::io::stdin().lock()), "stdin".to_string()),
        (Some(task), _) => (Ok(task.as_bytes().to_vec()), "TASK".to_string()),
        (None, Some(path)) => (
            std::fs::File::open(path).and_then(read_capped),
            path.display().to_string(),
        ),
        (None, None) => unreachable!("clap requires TASK or --task-file"),
    };
    let bytes = bytes.map_err(|e| {
        CliError::input(format!("could not read the task from {from}: {e}"))
            .hint("give the task as an argument, with --task-file PATH, or on stdin with `-`")
    })?;
    if bytes.len() as u64 > MAX_TASK {
        return Err(CliError::input(format!(
            "the task from {from} is longer than {} KiB",
            MAX_TASK / 1024
        ))
        .hint("shorten it: Claude can read longer material on the machine itself")
        .into());
    }
    let task = String::from_utf8(bytes).map_err(|_| {
        CliError::input(format!("the task from {from} is not UTF-8 text")).hint("save it as UTF-8")
    })?;
    if task.trim().is_empty() {
        return Err(CliError::usage(format!("the task from {from} is empty"))
            .hint("say what to do, e.g. mcc remote work box \"free disk space under /var/log\"")
            .into());
    }
    Ok(task)
}

/// Up to [`MAX_TASK`] + 1 bytes of `r`.
fn read_capped(r: impl Read) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    r.take(MAX_TASK + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

/// What Claude Code is told about where it works, on one line.
fn system_prompt(name: &str, host: &str) -> String {
    format!(
        "You are working on the machine {name} ({host}) over SSH; it may not have Claude Code installed. \
         You have exactly one tool, `run`, which runs one shell command there as the remote user and returns its exit status, stdout and stderr; each call is a new SSH session. \
         You cannot read or change files on this local machine. \
         Wrap long-running commands in a remote-side timeout (e.g. `timeout 300 ...`). \
         Finish with a short report of what you did and whether the task succeeded."
    )
}

/// The arguments Claude Code runs with: headless, JSON result, no tools
/// of its own, only mcc's MCP server and only its `run` tool, every other
/// tool refused without a prompt, only the user's own settings, no hooks,
/// no saved session.
fn claude_args(mcp_config: &Path, max_turns: u32, prompt: &str) -> Vec<OsString> {
    let tool = format!("mcp__{SERVER}__{}", crate::remote_tool::TOOL);
    [
        "-p",
        "--output-format",
        "json",
        "--tools",
        "",
        "--no-chrome",
        "--strict-mcp-config",
        "--mcp-config",
    ]
    .into_iter()
    .map(OsString::from)
    .chain([mcp_config.as_os_str().to_owned()])
    .chain(
        [
            "--allowedTools",
            &tool,
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--setting-sources",
            "user",
            "--no-session-persistence",
            "--max-turns",
            &max_turns.to_string(),
            "--settings",
            r#"{"disableAllHooks":true}"#,
            "--append-system-prompt",
            prompt,
        ]
        .into_iter()
        .map(OsString::from),
    )
    .collect()
}

/// How Claude Code's run ended.
enum Ended {
    /// On its own, with this status.
    Exited(i32),
    /// mcc stopped it.
    Stopped(Stop),
}

/// Why mcc stopped the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Interrupted,
    Signal(i32),
    TimedOut,
}

impl Stop {
    /// mcc's exit status: 130 (Ctrl+C), 128 + the signal, or 124.
    fn code(self) -> i32 {
        match self {
            Stop::Interrupted => EXIT_INTERRUPTED,
            Stop::Signal(sig) => 128 + sig,
            Stop::TimedOut => EXIT_TIMEOUT,
        }
    }

    fn log_name(self) -> &'static str {
        match self {
            Stop::Interrupted => "interrupted",
            Stop::Signal(libc::SIGHUP) => "SIGHUP",
            Stop::Signal(_) => "SIGTERM",
            Stop::TimedOut => "timeout",
        }
    }

    fn describe(self, timeout: Option<u64>) -> String {
        match self {
            Stop::Interrupted => "interrupted (Ctrl+C)".into(),
            Stop::Signal(_) => format!("mcc received {}", self.log_name()),
            Stop::TimedOut => format!(
                "the run took longer than --timeout {}s",
                timeout.unwrap_or_default()
            ),
        }
    }
}

/// Start Claude Code in a new private folder and wait for it; the folder
/// is removed whatever happens. `(name, host)` is the machine. Its end,
/// its stdout, and the exit status of each command the tool ran.
fn start_and_wait(
    paths: &Paths,
    p: &Profile,
    bin: &Path,
    ssh: &Path,
    (name, host): (&str, &str),
    task: &str,
    args: &RemoteArgs,
) -> Result<(Ended, Vec<u8>, Vec<Option<i32>>)> {
    let scratch = Scratch::create(paths)?;
    let tool_log = scratch.dir.join("tool.log");
    let mcp_config = scratch.dir.join("mcp.json");
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .context("finding mcc's own path")?;
    let server = json!({"mcpServers": {SERVER: {
        "command": utf8(&exe)?,
        "args": ["remote-tool", "--ssh", utf8(ssh)?, "--host", host, "--log", utf8(&tool_log)?],
    }}});
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&mcp_config)
        .and_then(|mut f| f.write_all(server.to_string().as_bytes()))
        .with_context(|| format!("writing {}", mcp_config.display()))?;

    let mut cmd = Command::new(bin);
    cmd.args(claude_args(
        &mcp_config,
        args.max_turns,
        &system_prompt(name, host),
    ))
    .current_dir(&scratch.dir)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .process_group(0);
    crate::refresh::child_env(&mut cmd, paths, p);
    cmd.env_remove("CLAUDE_CODE_ENABLE_CFC");
    log::write(
        "INFO",
        &format!(
            "remote: starting {} as profile {} for machine {name}",
            bin.display(),
            p.name
        ),
    );
    let mut child = cmd
        .spawn()
        .map_err(|e| exec::not_started(&bin.display().to_string(), e))?;

    // Both on threads, so neither a task Claude Code doesn't read nor a
    // long result can keep mcc from stopping it.
    if let Some(mut stdin) = child.stdin.take() {
        let task = task.to_string();
        std::thread::spawn(move || {
            // A Claude Code that exits without reading it: its status says why.
            let _ = stdin.write_all(task.as_bytes());
        });
    }
    let reader = child.stdout.take().map(|out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.take(MAX_RESULT).read_to_end(&mut buf);
            buf
        })
    });

    let deadline = args
        .timeout
        .map(|s| Instant::now() + Duration::from_secs(s));
    let end = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ended::Exited(crate::refresh::exit_code(status)),
            Ok(None) => {}
            Err(e) => {
                stop_group(&mut child);
                return Err(anyhow::Error::new(e).context("waiting for Claude Code"));
            }
        }
        let why = if claude::INTERRUPTED.swap(false, Ordering::SeqCst) {
            Some(Stop::Interrupted)
        } else if let sig @ 1.. = SIGNALLED.swap(0, Ordering::SeqCst) {
            Some(Stop::Signal(sig))
        } else if deadline.is_some_and(|d| Instant::now() >= d) {
            Some(Stop::TimedOut)
        } else {
            None
        };
        if let Some(why) = why {
            stop_group(&mut child);
            scratch.remove();
            // Not waiting for its stdout: a process left behind may hold it.
            return Ok((Ended::Stopped(why), Vec::new(), Vec::new()));
        }
        std::thread::sleep(POLL);
    };
    let stdout = reader
        .map(|r| r.join().unwrap_or_default())
        .unwrap_or_default();
    let commands = crate::remote_tool::read_log(&tool_log);
    Ok((end, stdout, commands))
}

/// `p` as a string, for the MCP config (JSON).
fn utf8(p: &Path) -> Result<&str> {
    p.to_str().ok_or_else(|| {
        CliError::runtime(format!("{} is not a UTF-8 path", p.display()))
            .hint("`mcc remote` needs mcc, ssh and ~/.mcc at UTF-8 paths")
            .into()
    })
}

/// Stop Claude Code's whole process group (it, the tool, every ssh): SIGTERM,
/// then SIGKILL once it has exited or after [`STOP_GRACE`]. It is reaped
/// only after that, so its group id can't have been reused meanwhile.
fn stop_group(child: &mut Child) {
    let Ok(pgid) = libc::pid_t::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    // SAFETY: plain syscalls on the group of a child not yet reaped.
    unsafe { libc::killpg(pgid, libc::SIGTERM) };
    let until = Instant::now() + STOP_GRACE;
    while Instant::now() < until && !exited(pgid) {
        std::thread::sleep(POLL);
    }
    // SAFETY: as above; the leader is at most a zombie, so the group is ours.
    unsafe { libc::killpg(pgid, libc::SIGKILL) };
    let _ = child.wait();
}

/// Whether child `pid` has exited, without reaping it.
fn exited(pid: libc::pid_t) -> bool {
    // SAFETY: `info` is a plain C struct, zeroed as waitid expects.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let id = libc::id_t::try_from(pid).unwrap_or_default();
    // SAFETY: `info` outlives the call; WNOWAIT leaves the child unreaped.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    rc != 0 || info.si_signo != 0
}

/// The signal (SIGTERM or SIGHUP) mcc received during the run; 0: none.
static SIGNALLED: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    SIGNALLED.store(sig, Ordering::SeqCst);
}

/// SIGTERM and SIGHUP recorded in [`SIGNALLED`] instead of ending mcc,
/// for as long as this lives; the previous handlers come back after. A
/// signal ignored before (`nohup`) stays ignored.
struct Signals {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl Signals {
    fn install() -> Self {
        SIGNALLED.store(0, Ordering::SeqCst);
        let mut previous = Vec::new();
        for sig in [libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: sigaction structs are plain C data; the handler only
            // stores to an atomic, which is async-signal-safe.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, std::ptr::null(), &mut old) != 0
                    || old.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut new: libc::sigaction = std::mem::zeroed();
                new.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                libc::sigemptyset(&mut new.sa_mask);
                if libc::sigaction(sig, &new, &mut old) == 0 {
                    previous.push((sig, old));
                }
            }
        }
        Self { previous }
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (sig, old) in &self.previous {
            // SAFETY: restores the action saved by `install`.
            unsafe { libc::sigaction(*sig, old, std::ptr::null_mut()) };
        }
    }
}

/// The run's private folder, `~/.mcc/state/remote/<pid>-<nanos>` (0700),
/// removed when dropped.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn create(paths: &Paths) -> Result<Self> {
        let parent = paths.state_dir.join("remote");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = parent.join(format!("{}-{nanos}", std::process::id()));
        // Not recursive: a folder already there is not this run's.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    fn remove(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.remove();
    }
}

/// What Claude Code's result says.
#[derive(Debug, PartialEq)]
struct Outcome {
    /// Its closing report.
    result: Option<String>,
    /// `success`, `error_max_turns`, … (`unknown` when missing or odd).
    subtype: String,
    is_error: bool,
    turns: Option<u64>,
    cost: Option<f64>,
    /// Each refused tool call: its tool and input, shown.
    refused: Vec<String>,
}

impl Outcome {
    /// Claude Code's `--output-format json` result; `None` when `stdout`
    /// isn't one.
    fn parse(stdout: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(stdout).ok()?;
        if v.get("type").and_then(Value::as_str) != Some("result") {
            return None;
        }
        let subtype = v
            .get("subtype")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
            .unwrap_or("unknown")
            .to_string();
        let refused = v
            .get("permission_denials")
            .and_then(Value::as_array)
            .map(|denials| denials.iter().map(shown_denial).collect())
            .unwrap_or_default();
        Some(Self {
            result: v.get("result").and_then(Value::as_str).map(str::to_string),
            subtype,
            // A result that doesn't say it succeeded didn't.
            is_error: v.get("is_error").and_then(Value::as_bool) != Some(false),
            turns: v.get("num_turns").and_then(Value::as_u64),
            cost: v.get("total_cost_usd").and_then(Value::as_f64),
            refused,
        })
    }

    fn succeeded(&self) -> bool {
        !self.is_error && self.subtype == "success"
    }

    /// `box: success, 3 turns, $0.0123, 2 remote commands (0 failed)`.
    fn summary(&self, machine: &str, commands: &[Option<i32>]) -> String {
        let failed = commands.iter().filter(|c| **c != Some(0)).count();
        let turns = self.turns.map_or("?".to_string(), |t| t.to_string());
        let cost = self
            .cost
            .map_or("cost unknown".to_string(), |c| format!("${c:.4}"));
        format!(
            "{machine}: {}, {turns} turns, {cost}, {} remote command{} ({failed} failed)",
            self.subtype,
            commands.len(),
            if commands.len() == 1 { "" } else { "s" }
        )
    }
}

/// A refused tool call as shown: its tool, then its input as JSON, cut at
/// [`SHOWN_INPUT`] characters. Control characters never reach the terminal.
fn shown_denial(d: &Value) -> String {
    let tool = d.get("tool_name").and_then(Value::as_str).unwrap_or("?");
    let input = d
        .get("tool_input")
        .map(Value::to_string)
        .unwrap_or_default();
    let mut cut: String = input.chars().take(SHOWN_INPUT).collect();
    if cut.len() < input.len() {
        cut.push('…');
    }
    printable(&format!("{tool} {cut}"))
}

/// `text` with every control character but newline and tab replaced, so
/// what came from the machine can't drive the terminal.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

/// Print how the run went, and return mcc's exit status: Claude Code's
/// when it failed, else 0 when the task succeeded and 1 when it didn't
/// (or its result can't be read). Nothing of the task, the result or a
/// refused call reaches mcc's log.
fn report(
    profile: &str,
    machine: &str,
    status: i32,
    stdout: &[u8],
    commands: &[Option<i32>],
    args: &RemoteArgs,
) -> i32 {
    let Some(outcome) = Outcome::parse(stdout) else {
        let _ = std::io::stdout().write_all(stdout);
        let code = if status == 0 { 1 } else { status };
        log::write(
            "WARN",
            &format!("remote: profile {profile} on machine {machine}: exit {code}, no result"),
        );
        say(
            false,
            &format!("{machine}: could not read Claude Code's result (it exited {status})"),
        );
        return code;
    };
    if args.json {
        let _ = std::io::stdout().write_all(stdout);
        if !stdout.ends_with(b"\n") {
            println!();
        }
    } else if let Some(text) = &outcome.result {
        println!("{}", printable(text));
    }
    for refused in &outcome.refused {
        eprintln!("  refused: {refused}");
    }
    let code = if status != 0 {
        status
    } else if outcome.succeeded() {
        0
    } else {
        1
    };
    log::write(
        if code == 0 { "INFO" } else { "WARN" },
        &format!(
            "remote: profile {profile} on machine {machine}: exit {code}, {}, {} turns",
            outcome.subtype,
            outcome.turns.map_or("?".to_string(), |t| t.to_string())
        ),
    );
    let summary = outcome.summary(machine, commands);
    say(code == 0, &summary);
    if outcome.subtype == "error_max_turns" {
        eprintln!(
            "  hint: the task needed more turns than --max-turns {}: run it again with more",
            args.max_turns
        );
    }
    code
}

/// A closing line on stderr, ✓ or ✗; never logged (it may hold remote
/// text). `✓` lines are left out with --quiet.
fn say(ok: bool, msg: &str) {
    if ok {
        if !ui::settings().quiet {
            eprintln!("{} {msg}", console::style("✓").for_stderr().green().bold());
        }
    } else {
        eprintln!("{} {msg}", console::style("✗").for_stderr().red().bold());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_runs_headless_with_only_the_remote_tool() {
        let args = claude_args(Path::new("/s/mcp.json"), 30, "be careful");
        let expected = [
            "-p",
            "--output-format",
            "json",
            "--tools",
            "",
            "--no-chrome",
            "--strict-mcp-config",
            "--mcp-config",
            "/s/mcp.json",
            "--allowedTools",
            "mcp__mcc-remote__run",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--setting-sources",
            "user",
            "--no-session-persistence",
            "--max-turns",
            "30",
            "--settings",
            r#"{"disableAllHooks":true}"#,
            "--append-system-prompt",
            "be careful",
        ];
        assert_eq!(args, expected.map(OsString::from));
        let prompt = system_prompt("box", "me@box");
        assert!(!prompt.contains('\n'));
        assert!(prompt.contains("box (me@box)"));
    }

    fn result(extra: &str) -> Vec<u8> {
        format!(
            r#"{{"type":"result","result":"done","num_turns":3,"total_cost_usd":0.01234{extra}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn results_are_read_and_summed_up() {
        let ok = Outcome::parse(&result(r#","subtype":"success","is_error":false"#)).unwrap();
        assert!(ok.succeeded());
        assert_eq!(ok.result.as_deref(), Some("done"));
        assert_eq!(
            ok.summary("box", &[Some(0), Some(2), None]),
            "box: success, 3 turns, $0.0123, 3 remote commands (2 failed)"
        );
        assert_eq!(
            ok.summary("box", &[Some(0)]),
            "box: success, 3 turns, $0.0123, 1 remote command (0 failed)"
        );
        let max =
            Outcome::parse(&result(r#","subtype":"error_max_turns","is_error":false"#)).unwrap();
        assert!(!max.succeeded());
        let err = Outcome::parse(&result(r#","subtype":"success","is_error":true"#)).unwrap();
        assert!(!err.succeeded());
        // No is_error: not a success.
        assert!(
            !Outcome::parse(&result(r#","subtype":"success""#))
                .unwrap()
                .succeeded()
        );
        // An odd subtype is not echoed.
        let odd = Outcome::parse(&result(r#","subtype":"\u001b[2J","is_error":false"#)).unwrap();
        assert_eq!(odd.subtype, "unknown");
        let bare = Outcome::parse(br#"{"type":"result"}"#).unwrap();
        assert_eq!(
            bare.summary("box", &[]),
            "box: unknown, ? turns, cost unknown, 0 remote commands (0 failed)"
        );
        for not_a_result in [&b"hello"[..], b"", br#"{"type":"assistant"}"#, b"[1]"] {
            assert_eq!(Outcome::parse(not_a_result), None);
        }
    }

    #[test]
    fn refused_calls_are_shown_short_and_inert() {
        let long = "é".repeat(300);
        let v: Value = serde_json::from_str(&format!(
            r#"{{"tool_name":"Bash\u001b]0;x","tool_input":{{"command":"{long}"}}}}"#
        ))
        .unwrap();
        let shown = shown_denial(&v);
        assert!(
            shown.starts_with("Bash\u{fffd}]0;x {\"command\":\""),
            "{shown}"
        );
        assert!(shown.ends_with('…'), "{shown}");
        assert_eq!(
            shown.chars().count(),
            "Bash\u{fffd}]0;x ".chars().count() + SHOWN_INPUT + 1
        );
        assert_eq!(
            shown_denial(&json!({"tool_name": "mcp__x__y", "tool_input": {"a": "<b> & 'c'"}})),
            r#"mcp__x__y {"a":"<b> & 'c'"}"#
        );
        assert_eq!(
            printable("a\u{1b}[31mb\r\nc\td"),
            "a\u{fffd}[31mb\u{fffd}\nc\td"
        );
    }

    #[test]
    fn stops_exit_as_shells_expect() {
        assert_eq!(Stop::Interrupted.code(), 130);
        assert_eq!(Stop::Signal(libc::SIGTERM).code(), 143);
        assert_eq!(Stop::Signal(libc::SIGHUP).code(), 129);
        assert_eq!(Stop::TimedOut.code(), 124);
        assert_eq!(
            Stop::TimedOut.describe(Some(5)),
            "the run took longer than --timeout 5s"
        );
    }
}
