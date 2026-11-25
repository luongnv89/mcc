//! Other machines' usage for `mcc usage` (#130): the `[machines.NAME]` set
//! up in `~/.mcc/config.toml`, each read by running `mcc usage` there over
//! SSH.
//!
//! The SSH command is an argument list, never a local shell string:
//! `ssh -o BatchMode=yes -o ConnectTimeout=N -- HOST 'WORD' 'WORD' …`, so
//! the host can't be read as an option and no password prompt can hang the
//! run. ssh hands the remote command to the remote shell as one string, so
//! every word of it is single-quoted ([`shell_quote`]), and a profile name
//! another machine reports is checked against the alias rules before it is
//! ever put on a command line ([`parse_reply`]). mcc never reads
//! `~/.ssh/config`: ssh does.
//!
//! The other machine answers in the machine protocol (`mcc usage --json-v2
//! --machine-protocol`): the schema-2 envelope, marked with [`PROTOCOL`],
//! each row carrying `account_id`, the SHA-256 of the account's email and
//! organization ([`crate::usage::account_id`]). That is how the same
//! account is matched across machines: [`select`] picks one row per
//! account to look up live (`mcc usage --all`), and [`newest`] finds the
//! latest numbers each account has on any machine.
//!
//! `mcc remote` (#142) runs other commands there: the ones Claude Code's
//! single tool asks for ([`crate::remote_tool`]), each through
//! [`agent_ssh_args`], a fixed, hardened ssh argument list, with only the
//! environment ssh needs ([`EnvPolicy::Allowlist`]). [`preflight`] checks
//! first that the machine takes them.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::Machine;
use crate::usage::{Status, Usage};

/// The marker of a machine-protocol reply (the envelope's `protocol`).
pub const PROTOCOL: &str = "mcc-machines/1";

/// The flag that asks another machine's mcc for a machine-protocol reply.
pub const PROTOCOL_FLAG: &str = "--machine-protocol";

/// How long ssh may take to connect (its `ConnectTimeout`, in seconds).
pub const CONNECT_TIMEOUT_SECS: u32 = 10;

/// How long reading another machine's cached usage may take in all.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long another machine's `mcc usage --all` may take beyond its own
/// refresh budget ([`crate::refresh::RUN_BUDGET`]): its live lookups and
/// the SSH round trip. Without it, a slow token refresh there would look
/// like a machine that doesn't answer.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(30);

/// The most of a reply mcc reads; a longer one is not mcc's.
const MAX_REPLY: u64 = 4 * 1024 * 1024;

/// Why a machine's usage couldn't be read. Serialized as its [`code`] in
/// `--json-v2`'s `machines[].error`.
///
/// [`code`]: MachineError::code
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineError {
    /// Its `[machines.NAME]` is not usable ([`Machine::check`]).
    Config(String),
    /// ssh could not be started here.
    NoSsh,
    /// ssh could not connect or log in (its exit status 255).
    Unreachable,
    /// No answer in time.
    TimedOut,
    /// The remote shell found no mcc command (exit 126 or 127).
    NotFound,
    /// The mcc there doesn't know the machine protocol.
    Outdated,
    /// The answer was not a machine-protocol reply.
    Incompatible(&'static str),
    /// `mcc usage` failed there with this exit status.
    Failed(i32),
}

impl MachineError {
    /// Stable code, as in the JSON output.
    pub fn code(&self) -> &'static str {
        match self {
            MachineError::Config(_) => "invalid_config",
            MachineError::NoSsh => "no_ssh",
            MachineError::Unreachable => "unreachable",
            MachineError::TimedOut => "timed_out",
            MachineError::NotFound => "mcc_not_found",
            MachineError::Outdated => "outdated",
            MachineError::Incompatible(_) => "incompatible",
            MachineError::Failed(_) => "failed",
        }
    }

    /// What went wrong on machine `name` (host `host`), and how to fix it.
    pub fn describe(&self, name: &str, host: &str) -> String {
        match self {
            MachineError::Config(why) => {
                format!("[machines.{name}] is not usable: {why}; fix it with `mcc config edit`")
            }
            MachineError::NoSsh => "ssh could not be started: install OpenSSH's ssh client".into(),
            MachineError::Unreachable => format!(
                "ssh could not connect to {host} without a prompt: check that `ssh {host} true` works with your key"
            ),
            MachineError::TimedOut => format!("{host} did not answer in time"),
            MachineError::NotFound => format!(
                "no mcc on {host}'s PATH over SSH: set mcc = \"/full/path/to/mcc\" under [machines.{name}]"
            ),
            MachineError::Outdated => {
                format!("the mcc on {host} is too old to share its usage: upgrade it there")
            }
            MachineError::Incompatible(what) => format!(
                "unexpected answer from the mcc on {host} ({what}): use the same mcc version on both machines"
            ),
            MachineError::Failed(code) => {
                format!("`mcc usage` failed on {host} (exit {code})")
            }
        }
    }
}

/// The ssh program: `MCC_SSH_BIN` when set, else `ssh` from PATH.
pub fn ssh_bin() -> OsString {
    std::env::var_os("MCC_SSH_BIN")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "ssh".into())
}

/// `word` single-quoted for a POSIX shell, so the remote shell reads it
/// back as exactly one literal word: `it's` becomes `'it'\''s'`.
pub fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The arguments ssh runs `mcc ARGS` on `m` with: options first, `--`
/// right before the host, then the remote command as one string of quoted
/// words.
pub fn ssh_args(m: &Machine, args: &[String]) -> Vec<String> {
    let mcc = m.mcc.as_deref().unwrap_or("mcc");
    let remote: Vec<String> = std::iter::once(mcc)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect();
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"),
        "--".into(),
        m.host.clone(),
        remote.join(" "),
    ]
}

/// The `mcc usage` arguments that read a machine's usage: from its cache,
/// or with `only`, `--all` for those profiles alone.
pub fn usage_args(only: Option<&[String]>) -> Vec<String> {
    let mut args = vec!["usage".to_string()];
    if only.is_some() {
        args.push("--all".into());
    }
    args.extend(["--json-v2".into(), PROTOCOL_FLAG.into()]);
    if let Some(names) = only {
        args.extend(["--only".into(), names.join(",")]);
    }
    args
}

/// One machine to ask: its name, its config, and the `mcc` arguments.
pub struct Request<'a> {
    pub name: &'a str,
    pub machine: &'a Machine,
    pub args: Vec<String>,
}

impl<'a> Request<'a> {
    /// Ask `machine` for its usage ([`usage_args`]).
    pub fn new(name: &'a str, machine: &'a Machine, only: Option<&[String]>) -> Self {
        Self {
            name,
            machine,
            args: usage_args(only),
        }
    }
}

/// Ask every machine at once (one thread each, as many as configured),
/// each for at most `timeout`; the answers come back in request order.
pub fn ask_all<T: DeserializeOwned + Send>(
    requests: &[Request],
    timeout: Duration,
) -> Vec<Result<Vec<T>, MachineError>> {
    std::thread::scope(|s| {
        let handles: Vec<_> = requests
            .iter()
            .map(|r| s.spawn(move || ask(r, timeout)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(Err(MachineError::Failed(1))))
            .collect()
    })
}

/// Ask one machine: run ssh, then read and check its answer.
fn ask<T: DeserializeOwned>(r: &Request, timeout: Duration) -> Result<Vec<T>, MachineError> {
    r.machine.check(r.name).map_err(MachineError::Config)?;
    let out = run_ssh(r.machine, &r.args, timeout)?;
    let result = classify(out.code, &out.stdout, &out.stderr)
        .and_then(|()| parse_reply(r.name, &out.stdout));
    match &result {
        Ok(rows) => crate::log::write(
            "INFO",
            &format!("machine {}: {} profiles", r.name, rows.len()),
        ),
        Err(e) => crate::log::write("WARN", &format!("machine {}: {}", r.name, e.code())),
    }
    result
}

/// How an ssh run ended.
struct SshOutput {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run ssh for `args` on `m`, stopped after `timeout`. Both pipes are
/// drained while it runs, so a long answer can't stall it.
fn run_ssh(m: &Machine, args: &[String], timeout: Duration) -> Result<SshOutput, MachineError> {
    let argv: Vec<OsString> = ssh_args(m, args).into_iter().map(OsString::from).collect();
    let ran = run_raw(
        &ssh_bin(),
        &argv,
        EnvPolicy::Inherit,
        timeout,
        MAX_REPLY,
        None,
    )
    .map_err(|_| MachineError::NoSsh)?;
    match ran.end {
        End::Exited(code) => Ok(SshOutput {
            code,
            stdout: ran.stdout,
            stderr: ran.stderr,
        }),
        End::TimedOut => Err(MachineError::TimedOut),
        End::Stopped | End::Lost => Err(MachineError::Failed(1)),
    }
}

/// Which environment a command started by [`run_raw`] gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvPolicy {
    /// mcc's own environment, unchanged (`mcc usage` on other machines).
    Inherit,
    /// Only what ssh needs to find the user's keys, agent and config
    /// ([`allowed_env`]): no credential, no Claude Code or mcc variable.
    Allowlist,
}

/// How a command started by [`run_raw`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// It exited with this code (a signal counts as 128 + its number).
    Exited(i32),
    /// It ran past its timeout and was killed.
    TimedOut,
    /// The caller's stop flag was raised and it was killed.
    Stopped,
    /// Waiting for it failed.
    Lost,
}

/// A command [`run_raw`] ran: how it ended, and the first `cap + 1` bytes
/// of each of its outputs (so a caller can tell they were longer).
pub(crate) struct Ran {
    pub(crate) end: End,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// How long a killed command's outputs are still read: a process it left
/// behind may hold its pipes open for good.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Run `program` with `args` (an argument list, never a shell string),
/// stdin closed and both outputs drained while it runs, so a long answer
/// can't stall it. It is killed after `timeout`, or as soon as `stop`
/// turns true. The error is the one that kept it from starting.
pub(crate) fn run_raw(
    program: &std::ffi::OsStr,
    args: &[OsString],
    env: EnvPolicy,
    timeout: Duration,
    cap: u64,
    stop: Option<&AtomicBool>,
) -> std::io::Result<Ran> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if env == EnvPolicy::Allowlist {
        cmd.env_clear().envs(allowed_env(std::env::vars_os()));
    }
    let mut child = cmd.spawn()?;
    let (tx, rx) = mpsc::channel();
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let (shared, tx) = (Arc::clone(&buf), tx.clone());
        std::thread::spawn(move || {
            if let Some(p) = pipe {
                let mut p = p.take(cap + 1);
                let mut chunk = [0u8; 8192];
                while let Ok(n) = p.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut b) = shared.lock() {
                        b.extend_from_slice(&chunk[..n]);
                    }
                }
                let _ = std::io::copy(&mut p.into_inner(), &mut std::io::sink());
            }
            let _ = tx.send(());
        });
        buf
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let start = Instant::now();
    let end = loop {
        match child.try_wait() {
            Ok(Some(status)) => break End::Exited(crate::refresh::exit_code(status)),
            Ok(None) => {}
            Err(_) => break End::Lost,
        }
        let stopped = stop.is_some_and(|s| s.load(Ordering::SeqCst));
        if stopped || start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break if stopped { End::Stopped } else { End::TimedOut };
        }
        std::thread::sleep(POLL);
    };
    // Wait for both outputs to close: at once for a command that exited;
    // for a killed one, only a short grace, since a process it started may
    // still hold them open.
    let deadline = Instant::now() + DRAIN_GRACE;
    for _ in 0..2 {
        let closed = match end {
            End::Exited(_) => rx.recv().is_ok(),
            _ => rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok(),
        };
        if !closed {
            break;
        }
    }
    let take = |b: Arc<Mutex<Vec<u8>>>| {
        b.lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    };
    Ok(Ran {
        end,
        stdout: take(out),
        stderr: take(err),
    })
}

/// How often [`run_raw`] checks on its command.
const POLL: Duration = Duration::from_millis(50);

/// The variables of `vars` an agent's ssh keeps ([`EnvPolicy::Allowlist`]):
/// what it needs to find the user, their keys and agent, their Kerberos
/// ticket and a locale. Never a credential, nor anything Claude Code or mcc
/// reads; `SSH_ASKPASS*` are left out too, so nothing can prompt.
pub(crate) fn allowed_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    const KEPT: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "KRB5CCNAME",
    ];
    vars.into_iter()
        .filter(|(k, _)| {
            let Some(k) = k.to_str() else { return false };
            KEPT.contains(&k)
                || k.starts_with("LC_")
                || (k.starts_with("SSH_") && !k.starts_with("SSH_ASKPASS"))
        })
        .collect()
}

/// Why `host` can't be given to an agent's ssh ([`agent_ssh_args`]), if it
/// can't: only letters, digits and `._@:-`, not starting with `-`. Stricter
/// than [`Machine::check`]: no character a shell, ssh's `%` tokens or its
/// pattern matching would read.
pub(crate) fn check_agent_host(host: &str) -> std::result::Result<(), String> {
    if host.is_empty() {
        return Err(
            "host is empty: set it to what `ssh` connects to, e.g. host = \"me@build-box\"".into(),
        );
    }
    if host.starts_with('-') {
        return Err(format!(
            "host \"{host}\" starts with '-', which ssh would read as an option"
        ));
    }
    if let Some(c) = host
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "._@:-".contains(*c)))
    {
        return Err(format!(
            "host {host:?} contains {c:?}: `mcc remote` takes only letters, digits and . _ @ : - (a host name, user@host or a Host alias from your SSH config)"
        ));
    }
    Ok(())
}

/// The ssh options of an agent's command after `BatchMode` and
/// `ConnectTimeout`: no agent or X11 forwarding, no port forwarding, no
/// local command, no TTY, no shared connection, and no `RemoteCommand`
/// from the user's config replacing the command.
const AGENT_SSH_HARDENING: &[&str] = &[
    "ForwardAgent=no",
    "ForwardX11=no",
    "ForwardX11Trusted=no",
    "ClearAllForwardings=yes",
    "PermitLocalCommand=no",
    "RequestTTY=no",
    "ControlMaster=no",
    "ControlPath=none",
    "RemoteCommand=none",
];

/// The arguments ssh runs an agent's `command` on `host` with: `-n`, no
/// prompt, [`CONNECT_TIMEOUT_SECS`] and [`AGENT_SSH_HARDENING`], `--`
/// right before the host, then the command as one word, which the remote
/// user's shell reads as a command line.
pub(crate) fn agent_ssh_args(host: &str, command: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-n".into()];
    let timeout = format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}");
    for opt in ["BatchMode=yes", &timeout]
        .into_iter()
        .chain(AGENT_SSH_HARDENING.iter().copied())
    {
        args.push("-o".into());
        args.push(opt.into());
    }
    args.extend(["--".into(), host.into(), command.into()]);
    args
}

/// The ssh program `mcc remote` runs, as an absolute path:
/// `MCC_SSH_BIN` (absolute, or a name looked up on PATH), else `ssh` from
/// PATH. It is handed to Claude Code's tool, which must not search PATH.
pub(crate) fn resolve_ssh() -> anyhow::Result<std::path::PathBuf> {
    let wanted = ssh_bin();
    let wanted_path = std::path::Path::new(&wanted);
    let found = if wanted_path.is_absolute() {
        crate::fsutil::is_executable(wanted_path).then(|| wanted_path.to_path_buf())
    } else if wanted_path.components().count() == 1 {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|d| d.join(wanted_path))
                .find(|p| p.is_absolute() && crate::fsutil::is_executable(p))
        })
    } else {
        None
    };
    found.ok_or_else(|| {
        crate::error::CliError {
            code: crate::commands::exec::EXIT_NOT_FOUND,
            message: format!(
                "ssh ({}) was not found or is not executable",
                wanted.to_string_lossy()
            ),
            hint: Some(
                "install OpenSSH's ssh client, or set MCC_SSH_BIN to the full path of ssh".into(),
            ),
        }
        .into()
    })
}

/// How long the check that `host` takes an agent's commands may take.
pub(crate) const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(30);

/// Check that `ssh` (absolute) reaches `host` and its shell runs `true`,
/// the way an agent's commands will run there.
pub(crate) fn preflight(ssh: &std::path::Path, host: &str) -> Result<(), MachineError> {
    let ran = run_raw(
        ssh.as_os_str(),
        &agent_ssh_args(host, "true"),
        EnvPolicy::Allowlist,
        PREFLIGHT_TIMEOUT,
        MAX_REPLY,
        None,
    )
    .map_err(|_| MachineError::NoSsh)?;
    match ran.end {
        End::Exited(0) => Ok(()),
        End::Exited(255) => Err(MachineError::Unreachable),
        End::Exited(126 | 127) => Err(MachineError::NotFound),
        End::Exited(code) => Err(MachineError::Failed(code)),
        End::TimedOut => Err(MachineError::TimedOut),
        End::Stopped | End::Lost => Err(MachineError::Failed(1)),
    }
}

/// What a failed [`preflight`] of `host` means, and how to fix it.
pub(crate) fn describe_preflight(err: &MachineError, name: &str, host: &str) -> String {
    match err {
        MachineError::NotFound => format!(
            "the remote shell on {host} could not run `true` (exit 126 or 127): check the remote user's login shell"
        ),
        MachineError::Failed(code) => {
            format!("`true` failed on {host} (exit {code}): check the remote user's login shell")
        }
        other => other.describe(name, host),
    }
}

/// What ssh's exit status (and, for a usage error, its stderr) says.
fn classify(code: i32, stdout: &[u8], stderr: &[u8]) -> Result<(), MachineError> {
    match code {
        0 if stdout.len() as u64 > MAX_REPLY => Err(MachineError::Incompatible("too long")),
        0 => Ok(()),
        255 => Err(MachineError::Unreachable),
        126 | 127 => Err(MachineError::NotFound),
        2 if unknown_flag(stderr) => Err(MachineError::Outdated),
        c => Err(MachineError::Failed(c)),
    }
}

/// Whether stderr is clap's usage error for a flag of the machine
/// protocol: what an mcc from before it (or before `--only`) answers.
fn unknown_flag(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("unexpected argument")
        && (text.contains(PROTOCOL_FLAG) || text.contains("'--only'"))
}

/// Whether `id` looks like an [`crate::usage::account_id`]: 64 lowercase
/// hex digits.
fn is_account_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The rows of a machine-protocol reply from `machine`. The envelope must
/// be schema 2 with the [`PROTOCOL`] marker. A row whose name breaks the
/// alias rules (it could not be passed back safely), whose `account_id`
/// isn't one, or that doesn't read as a row is dropped, and the log says
/// why.
pub fn parse_reply<T: DeserializeOwned>(
    machine: &str,
    stdout: &[u8],
) -> Result<Vec<T>, MachineError> {
    let v: Value =
        serde_json::from_slice(stdout).map_err(|_| MachineError::Incompatible("not JSON"))?;
    if v.get("schema").and_then(Value::as_u64) != Some(u64::from(crate::commands::JSON_SCHEMA)) {
        return Err(MachineError::Incompatible("another JSON schema"));
    }
    if v.get("protocol").and_then(Value::as_str) != Some(PROTOCOL) {
        return Err(MachineError::Incompatible("not the machine protocol"));
    }
    let rows = v
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or(MachineError::Incompatible("no profiles"))?;
    let skip = |i: usize, why: &str| {
        crate::log::write(
            "WARN",
            &format!("machine {machine}: row {i} dropped: {why}"),
        );
    };
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let name = row.get("name").and_then(Value::as_str);
        if !name.is_some_and(|n| crate::validate::check_syntax(n).is_ok()) {
            skip(i, "not a valid profile name");
            continue;
        }
        match row.get("account_id") {
            None | Some(Value::Null) => {}
            Some(Value::String(id)) if is_account_id(id) => {}
            Some(_) => {
                skip(i, "not a valid account_id");
                continue;
            }
        }
        match serde_json::from_value::<T>(row.clone()) {
            Ok(r) => out.push(r),
            Err(_) => skip(i, "not a usage row"),
        }
    }
    Ok(out)
}

/// One row as [`select`] sees it.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    /// On this machine (`false`: another one).
    pub local: bool,
    /// Its account; `None` when unknown (not signed in), which never
    /// matches another row.
    pub account_id: Option<&'a str>,
    /// Its login is not known to have run out.
    pub login_ok: bool,
}

/// Which rows `mcc usage --all` looks up live: one row per account, so
/// each account is asked for once, whatever number of machines and
/// profiles share it; and every row without a known account, on its own
/// machine. Of an account's rows, the first on this machine wins, else
/// the first whose login isn't known to have run out, else the first.
/// `rows` come in config order: this machine's, then each other machine's.
pub fn select(rows: &[Candidate]) -> Vec<bool> {
    let mut chosen = vec![false; rows.len()];
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, r) in rows.iter().enumerate() {
        let Some(id) = r.account_id else {
            chosen[i] = true;
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        let same = || (i..rows.len()).filter(|&j| rows[j].account_id == Some(id));
        let pick = same()
            .find(|&j| rows[j].local)
            .or_else(|| same().find(|&j| rows[j].login_ok))
            .unwrap_or(i);
        chosen[pick] = true;
    }
    chosen
}

/// The newest numbers of each account among `points` (account, usage):
/// only usage that is there and has a time competes, so a failed lookup
/// never hides numbers another machine has.
pub fn newest<'a>(
    points: impl IntoIterator<Item = (&'a str, &'a Usage)>,
) -> HashMap<&'a str, &'a Usage> {
    let mut best: HashMap<&str, &Usage> = HashMap::new();
    for (id, u) in points {
        let Some(at) = time_of(u) else { continue };
        if best.get(id).and_then(|b| time_of(b)).is_none_or(|b| at > b) {
            best.insert(id, u);
        }
    }
    best
}

/// When `u`'s numbers are from, when it has any.
pub fn time_of(u: &Usage) -> Option<DateTime<Utc>> {
    u.updated_at.filter(|_| u.status == Status::Ok)
}

/// When the machine reporting `u` last updated it itself (#130), for its
/// heading and `updated_at`: the time it kept beside numbers that came
/// from another machine, else [`time_of`]. Never for choosing the newest.
pub fn own_time(u: &Usage) -> Option<DateTime<Utc>> {
    u.own_updated_at.unwrap_or_else(|| time_of(u))
}

/// How far ahead of this machine's clock another machine's time may be
/// and still be believed: clocks a little apart are normal.
pub const CLOCK_TOLERANCE: chrono::Duration = chrono::Duration::seconds(60);

/// Forget the time of usage another machine sent when it is later than
/// `now` (plus [`CLOCK_TOLERANCE`]): a clock set ahead, or a broken
/// answer, would otherwise make older numbers look newest everywhere and
/// overwrite this machine's cache with them. The numbers stay, with no
/// time, so they never compete in [`newest`]. `true` when it was dropped.
pub fn drop_future_time(u: &mut Usage, now: DateTime<Utc>) -> bool {
    let ahead = |t: DateTime<Utc>| t > now + CLOCK_TOLERANCE;
    // Its own last update, shown in its heading, is no more believed.
    if let Some(own) = &mut u.own_updated_at {
        *own = own.filter(|&t| !ahead(t));
    }
    let future = u.updated_at.is_some_and(ahead);
    if future {
        u.updated_at = None;
    }
    future
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A POSIX shell's reading of single-quoted words (all `quoted` holds):
    /// inside quotes everything is literal, `\'` outside them is a quote.
    fn unquote(quoted: &str) -> Vec<String> {
        let (mut words, mut word, mut inside, mut chars) =
            (Vec::new(), None::<String>, false, quoted.chars());
        while let Some(c) = chars.next() {
            match (inside, c) {
                (true, '\'') => inside = false,
                (true, c) => word.get_or_insert_default().push(c),
                (false, '\'') => {
                    inside = true;
                    word.get_or_insert_default();
                }
                (false, '\\') => word.get_or_insert_default().push(chars.next().unwrap()),
                (false, ' ') => words.extend(word.take()),
                (false, c) => panic!("unquoted {c:?} in {quoted}"),
            }
        }
        assert!(!inside, "unterminated quote in {quoted}");
        words.extend(word);
        words
    }

    // No shell is started here (a child started while another test holds
    // a lock would inherit it); tests/machines.rs runs these words through
    // `sh -c`, as ssh does.
    #[test]
    fn shell_quote_keeps_every_word_literal_and_whole() {
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$(id)"), "'$(id)'");
        assert_eq!(shell_quote(""), "''");
        let words = [
            "plain",
            "two words",
            "it's",
            "''",
            "$HOME",
            "$(touch pwned)",
            "a;b && c",
            "`id`",
            "back\\slash",
            "*",
            "new\nline",
            "",
            "üñí",
        ];
        let line: Vec<String> = words.iter().map(|w| shell_quote(w)).collect();
        assert_eq!(unquote(&line.join(" ")), words);
        // Nothing but quotes, quoted quotes and spaces between the words:
        // no character the shell would act on is left unquoted.
        for q in &line {
            assert!(q.starts_with('\'') && q.ends_with('\''), "{q}");
        }
    }

    #[test]
    fn ssh_args_put_options_first_and_the_host_after_a_double_dash() {
        let m = Machine {
            host: "me@lab-box".into(),
            mcc: Some("/opt/my tools/mcc".into()),
        };
        let args = ssh_args(&m, &usage_args(Some(&["work".into(), "default".into()])));
        assert_eq!(
            args[..6],
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "--",
                "me@lab-box"
            ]
        );
        assert_eq!(args.len(), 7);
        assert_eq!(
            unquote(&args[6]),
            [
                "/opt/my tools/mcc",
                "usage",
                "--all",
                "--json-v2",
                "--machine-protocol",
                "--only",
                "work,default"
            ]
        );
        let plain = Machine {
            host: "lab".into(),
            mcc: None,
        };
        assert_eq!(
            ssh_args(&plain, &usage_args(None))[6],
            "'mcc' 'usage' '--json-v2' '--machine-protocol'"
        );
    }

    /// #142: the exact argv every agent command runs with, the command
    /// one word after the host whatever it holds.
    #[test]
    fn agent_ssh_args_are_the_hardened_list_then_the_host_and_one_word() {
        let cmd = r#"echo "$HOME" $(id) `x`; a | b > c 'q'"#;
        let args = agent_ssh_args("me@lab-box", cmd);
        let expected = [
            "-n",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "ForwardX11Trusted=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "RequestTTY=no",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "RemoteCommand=none",
            "--",
            "me@lab-box",
            cmd,
        ];
        assert_eq!(args, expected.map(OsString::from));
    }

    #[test]
    fn agent_hosts_are_plain_words() {
        for ok in [
            "lab",
            "me@lab-box",
            "10.0.0.7",
            "lab_2.example.com",
            "::1",
            "a-b",
        ] {
            assert_eq!(check_agent_host(ok), Ok(()), "{ok}");
        }
        for (bad, why) in [
            ("", "host is empty"),
            ("-oProxyCommand=x", "starts with '-'"),
            ("-", "starts with '-'"),
            ("lab;id", "contains ';'"),
            ("lab*", "contains '*'"),
            ("lab box", "contains ' '"),
            ("lab%h", "contains '%'"),
            ("$(id)", "contains '$'"),
            ("lab\n", "contains '\\n'"),
            ("lab/x", "contains '/'"),
            ("läb", "contains 'ä'"),
        ] {
            let err = check_agent_host(bad).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
    }

    #[test]
    fn the_agent_env_keeps_only_what_ssh_needs() {
        let vars = [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TMPDIR",
            "XDG_RUNTIME_DIR",
            "KRB5CCNAME",
            "SSH_AUTH_SOCK",
            "SSH_AGENT_PID",
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            "DISPLAY",
            "TERM",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_ENABLE_CFC",
            "CLAUDE_CONFIG_DIR",
            "MCC_HOME",
            "MCC_SSH_BIN",
            "AWS_SECRET_ACCESS_KEY",
        ]
        .map(|k| (OsString::from(k), OsString::from("v")));
        let kept: Vec<String> = allowed_env(vars)
            .into_iter()
            .map(|(k, _)| k.into_string().unwrap())
            .collect();
        assert_eq!(
            kept,
            [
                "PATH",
                "HOME",
                "USER",
                "LOGNAME",
                "LANG",
                "LC_ALL",
                "LC_CTYPE",
                "TMPDIR",
                "XDG_RUNTIME_DIR",
                "KRB5CCNAME",
                "SSH_AUTH_SOCK",
                "SSH_AGENT_PID",
            ]
        );
    }

    #[test]
    fn preflight_failures_never_mention_mcc() {
        for e in [
            MachineError::NoSsh,
            MachineError::Unreachable,
            MachineError::TimedOut,
            MachineError::NotFound,
            MachineError::Failed(3),
        ] {
            let text = describe_preflight(&e, "lab", "me@lab");
            assert!(!text.contains("mcc"), "{text}");
            assert!(
                text.contains("me@lab") || e == MachineError::NoSsh,
                "{text}"
            );
        }
        assert_eq!(
            describe_preflight(&MachineError::Unreachable, "lab", "me@lab"),
            "ssh could not connect to me@lab without a prompt: check that `ssh me@lab true` works with your key"
        );
    }

    #[test]
    fn exit_statuses_are_classified() {
        assert_eq!(classify(0, b"{}", b""), Ok(()));
        assert_eq!(classify(255, b"", b""), Err(MachineError::Unreachable));
        assert_eq!(classify(127, b"", b""), Err(MachineError::NotFound));
        assert_eq!(
            classify(
                2,
                b"",
                b"error: unexpected argument '--machine-protocol' found\n"
            ),
            Err(MachineError::Outdated)
        );
        assert_eq!(
            classify(2, b"", b"error: no profile"),
            Err(MachineError::Failed(2))
        );
        assert_eq!(classify(1, b"", b""), Err(MachineError::Failed(1)));
    }

    #[derive(serde::Deserialize, Debug)]
    struct Row {
        name: String,
    }

    #[test]
    fn replies_are_checked_and_bad_rows_dropped() {
        let id = "a".repeat(64);
        let reply =
            |rows: &str| format!(r#"{{"schema":2,"protocol":"{PROTOCOL}","profiles":[{rows}]}}"#);
        let rows: Vec<Row> = parse_reply(
            "lab",
            reply(&format!(
                r#"{{"name":"work","account_id":"{id}"}},{{"name":"default","account_id":null}},
                {{"name":"x;y","account_id":null}},{{"name":"$(touch pwned)"}},{{"name":"Bad"}},
                {{"name":"lab","account_id":"{}"}},{{"name":"ok","account_id":"short"}},{{"nom":"z"}}"#,
                "A".repeat(64)
            ))
            .as_bytes(),
        )
        .unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["work", "default"]);
        for (bad, why) in [
            ("hello".to_string(), "not JSON"),
            (
                r#"{"schema":2,"profiles":[]}"#.to_string(),
                "not the machine protocol",
            ),
            (
                format!(r#"{{"schema":3,"protocol":"{PROTOCOL}","profiles":[]}}"#),
                "another JSON schema",
            ),
            (
                format!(r#"{{"schema":2,"protocol":"{PROTOCOL}"}}"#),
                "no profiles",
            ),
        ] {
            assert_eq!(
                parse_reply::<Row>("lab", bad.as_bytes()).unwrap_err(),
                MachineError::Incompatible(why)
            );
        }
    }

    fn c(local: bool, id: Option<&str>, login_ok: bool) -> Candidate<'_> {
        Candidate {
            local,
            account_id: id,
            login_ok,
        }
    }

    #[test]
    fn select_looks_each_account_up_once_preferring_this_machine() {
        let rows = [
            c(false, Some("a"), true),  // 0: a elsewhere
            c(true, Some("a"), false),  // 1: a here: wins
            c(true, Some("a"), true),   // 2: a here again
            c(false, Some("b"), false), // 3: b, login ran out
            c(false, Some("b"), true),  // 4: b: wins
            c(false, Some("b"), true),  // 5
            c(false, Some("c"), false), // 6: c: only bad logins, the first wins
            c(false, Some("c"), false), // 7
            c(true, None, false),       // 8: unknown accounts: each its own
            c(false, None, false),      // 9
        ];
        assert_eq!(
            select(&rows),
            [
                false, true, false, false, true, false, true, false, true, true
            ]
        );
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn newest_takes_the_latest_point_that_has_usage() {
        let point = |pct: f64, t: Option<&str>| {
            let mut u = Usage::ok(
                None,
                Some(crate::usage::Window {
                    percent: pct,
                    resets_at: None,
                }),
            );
            u.updated_at = t.map(at);
            u
        };
        let old = point(10.0, Some("2026-10-10T10:00:00Z"));
        let new = point(70.0, Some("2026-10-10T11:00:00Z"));
        let untimed = point(99.0, None);
        let mut failed = Usage::unavailable(crate::usage::Reason::TokenExpired);
        failed.updated_at = Some(at("2026-10-10T12:00:00Z"));
        let best = newest([
            ("a", &old),
            ("a", &failed),
            ("a", &new),
            ("a", &untimed),
            ("b", &old),
            ("c", &failed),
        ]);
        assert_eq!(best["a"].weekly.as_ref().unwrap().percent, 70.0);
        assert_eq!(best["b"].weekly.as_ref().unwrap().percent, 10.0);
        assert!(!best.contains_key("c"));
    }

    #[test]
    fn a_time_later_than_this_machines_clock_is_dropped() {
        let now = at("2026-10-10T12:00:00Z");
        let point = |t: &str| {
            let mut u = Usage::ok(None, None);
            u.updated_at = Some(at(t));
            let dropped = drop_future_time(&mut u, now);
            (dropped, u.updated_at)
        };
        // Within the tolerance, or in the past: kept.
        assert_eq!(
            point("2026-10-10T12:01:00Z"),
            (false, Some(at("2026-10-10T12:01:00Z")))
        );
        assert_eq!(
            point("2026-10-10T11:00:00Z"),
            (false, Some(at("2026-10-10T11:00:00Z")))
        );
        // Beyond it: no time left, so it never competes.
        assert_eq!(point("2026-10-10T12:01:01Z"), (true, None));
        assert_eq!(point("9999-12-31T23:59:59Z"), (true, None));
    }
}
