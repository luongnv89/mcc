//! `mcc remote-tool` (#142): the one tool Claude Code has in `mcc remote`,
//! served over stdio as an MCP server (newline-delimited JSON-RPC).
//!
//! Its only tool, `run`, runs one command on one host through
//! [`machines::agent_ssh_args`]: a fixed, hardened ssh argument list with
//! the command as one word, and only the environment ssh needs. No local
//! shell is ever started. Requests are read on their own thread, so a
//! `ping` is answered while a command runs and the end of stdin (Claude
//! Code is gone) stops that command at once.
//!
//! Each call adds one line to the `--log` file, which `mcc remote` reads
//! for its summary: its exit status, how long it took and whether it timed
//! out. Never the command or its output.

use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

use crate::cli::RemoteToolArgs;
use crate::error::CliError;
use crate::machines::{self, End, EnvPolicy};

/// The MCP protocol version answered when the client names none.
const DEFAULT_PROTOCOL: &str = "2025-06-18";

/// How long one command may run by default (`--command-timeout`).
pub const COMMAND_TIMEOUT_SECS: u64 = 600;

/// The tool's name; Claude Code calls it `mcp__<server>__run`.
pub const TOOL: &str = "run";

/// The most of each output (stdout, stderr) a reply carries.
const OUTPUT_CAP: u64 = 256 * 1024;

/// The longest command accepted, in bytes.
const MAX_COMMAND: usize = 128 * 1024;

/// The longest request line read; a longer one is refused unread.
const MAX_LINE: usize = 4 * 1024 * 1024;

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

/// The server: replies are written one line at a time, from either thread.
struct Server {
    ssh: std::path::PathBuf,
    host: String,
    log: std::path::PathBuf,
    timeout: Duration,
    out: Mutex<std::io::Stdout>,
}

pub fn run(args: &RemoteToolArgs) -> Result<()> {
    if !args.ssh.is_absolute() {
        return Err(CliError::usage(format!(
            "--ssh {} is not an absolute path",
            args.ssh.display()
        ))
        .into());
    }
    machines::check_agent_host(&args.host).map_err(CliError::input)?;
    let server = Arc::new(Server {
        ssh: args.ssh.clone(),
        host: args.host.clone(),
        log: args.log.clone(),
        timeout: Duration::from_secs(args.command_timeout),
        out: Mutex::new(std::io::stdout()),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let (calls, queue) = mpsc::channel::<(Value, Value)>();
    {
        let (server, stop) = (Arc::clone(&server), Arc::clone(&stop));
        std::thread::spawn(move || {
            read_requests(&server, &calls);
            stop.store(true, Ordering::SeqCst);
        });
    }
    for (id, params) in queue {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Some(result) = server.call(&params, &stop) {
            server.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
        }
    }
    Ok(())
}

/// Read stdin until it ends, answering every request but `tools/call`,
/// which goes to `calls` (its id and params).
fn read_requests(server: &Server, calls: &mpsc::Sender<(Value, Value)>) {
    let mut stdin = std::io::stdin().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut stdin)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line);
        match read {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.len() > MAX_LINE && line.last() != Some(&b'\n') {
            let _ = stdin.skip_until(b'\n');
            server.send(&error(Value::Null, PARSE_ERROR, "request line too long"));
            continue;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match handle(&line, &server.host, server.timeout) {
            Action::Reply(v) => server.send(&v),
            Action::Call(id, params) => {
                if calls.send((id, params)).is_err() {
                    return;
                }
            }
            Action::Nothing => {}
        }
    }
}

/// What a request line asks for.
#[derive(Debug, PartialEq)]
enum Action {
    /// This reply, at once.
    Reply(Value),
    /// Run the tool (`tools/call`): the request's id and params.
    Call(Value, Value),
    /// Nothing: a notification, or a response to a request never sent.
    Nothing,
}

/// The [`Action`] for one request line.
fn handle(line: &[u8], host: &str, timeout: Duration) -> Action {
    let Ok(msg) = serde_json::from_slice::<Value>(line) else {
        return Action::Reply(error(Value::Null, PARSE_ERROR, "parse error"));
    };
    let Some(obj) = msg.as_object() else {
        return Action::Reply(error(Value::Null, INVALID_REQUEST, "invalid request"));
    };
    let Some(id) = obj.get("id").cloned() else {
        return Action::Nothing;
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        if obj.contains_key("result") || obj.contains_key("error") {
            return Action::Nothing;
        }
        return Action::Reply(error(id, INVALID_REQUEST, "invalid request"));
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mcc-remote", "version": env!("CARGO_PKG_VERSION")},
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": [tool(host, timeout)]}),
        "tools/call" => return Action::Call(id, params),
        _ => return Action::Reply(error(id, METHOD_NOT_FOUND, "method not found")),
    };
    Action::Reply(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// The `run` tool as `tools/list` describes it.
fn tool(host: &str, timeout: Duration) -> Value {
    json!({
        "name": TOOL,
        "description": format!(
            "Run one shell command on {host} over ssh, as the remote user, and return its exit status, stdout and stderr. \
             Each call is a new ssh session: the working directory and variables do not carry over. \
             A command still running after {} seconds is stopped here but may keep running on {host}, \
             so wrap long-running commands in a remote-side `timeout`.",
            timeout.as_secs()
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line the remote user's shell runs, e.g. `uname -a`",
                },
            },
            "required": ["command"],
        },
    })
}

/// A JSON-RPC error reply.
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// A `tools/call` result: `text`, flagged as an error or not.
fn tool_result(text: String, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

impl Server {
    /// Write one message, as one line.
    fn send(&self, msg: &Value) {
        let Ok(mut out) = self.out.lock() else { return };
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }

    /// Run a `tools/call`; `None` when it was stopped (stdin ended, so no
    /// one is left to answer).
    fn call(&self, params: &Value, stop: &AtomicBool) -> Option<Value> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        if name != TOOL {
            return Some(tool_result(
                format!("unknown tool {name:?}: the only tool is `{TOOL}`"),
                true,
            ));
        }
        let command = match params.pointer("/arguments/command") {
            Some(Value::String(c)) => c.as_str(),
            _ => {
                return Some(tool_result(
                    "`command` is missing or not a string".into(),
                    true,
                ));
            }
        };
        if let Err(why) = check_command(command) {
            return Some(tool_result(why, true));
        }
        let start = Instant::now();
        let ran = machines::run_raw(
            self.ssh.as_os_str(),
            &machines::agent_ssh_args(&self.host, command),
            EnvPolicy::Allowlist,
            self.timeout,
            OUTPUT_CAP,
            Some(stop),
        );
        let ms = start.elapsed().as_millis();
        let host = &self.host;
        let (text, is_error, exit, timed_out) = match ran {
            Err(e) => (
                format!("ssh ({}) could not be started: {e}", self.ssh.display()),
                true,
                None,
                false,
            ),
            Ok(ran) => {
                let outputs = outputs(&ran.stdout, &ran.stderr);
                match ran.end {
                    End::Exited(255) => (
                        format!(
                            "exit status 255: ssh could not connect to {host} or lost the connection (or the command itself exited 255)\n{outputs}"
                        ),
                        true,
                        Some(255),
                        false,
                    ),
                    End::Exited(code) => (
                        format!("exit status {code}\n{outputs}"),
                        false,
                        Some(code),
                        false,
                    ),
                    End::TimedOut => (
                        format!(
                            "timed out after {} seconds and ssh was stopped: the command MAY STILL BE RUNNING on {host}. Check whether it is (e.g. with `ps`) before running it again.\n{outputs}",
                            self.timeout.as_secs()
                        ),
                        true,
                        None,
                        true,
                    ),
                    End::Lost => (
                        format!("lost track of ssh: the command may still be running on {host}"),
                        true,
                        None,
                        false,
                    ),
                    End::Stopped => {
                        self.record(None, ms, false);
                        return None;
                    }
                }
            }
        };
        self.record(exit, ms, timed_out);
        Some(tool_result(text, is_error))
    }

    /// Add this call's line to the `--log` file (created 0600).
    fn record(&self, exit: Option<i32>, ms: u128, timed_out: bool) {
        let line = json!({"exit": exit, "ms": ms, "timed_out": timed_out});
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.log)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = written {
            eprintln!(
                "mcc remote-tool: could not write {}: {e}",
                self.log.display()
            );
        }
    }
}

/// Why `command` can't be run, if it can't.
fn check_command(command: &str) -> std::result::Result<(), String> {
    if command.trim().is_empty() {
        return Err("`command` is empty".into());
    }
    if command.len() > MAX_COMMAND {
        return Err(format!(
            "`command` is longer than {} KiB: copy long scripts in smaller steps",
            MAX_COMMAND / 1024
        ));
    }
    if command.contains('\0') {
        return Err("`command` contains a NUL byte".into());
    }
    Ok(())
}

/// Both outputs under their headings, each cut at [`OUTPUT_CAP`].
fn outputs(stdout: &[u8], stderr: &[u8]) -> String {
    format!(
        "--- stdout ---\n{}\n--- stderr ---\n{}",
        capped(stdout),
        capped(stderr)
    )
}

fn capped(bytes: &[u8]) -> String {
    let cap = OUTPUT_CAP as usize;
    if bytes.len() <= cap {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    format!(
        "{}\n[truncated: more than {} KiB]",
        String::from_utf8_lossy(&bytes[..cap]),
        cap / 1024
    )
}

/// The `--log` records of one `mcc remote` run, in order: each command's
/// exit status, `None` when it has none (ssh not started, timed out,
/// stopped). Lines that don't read as a record are skipped.
pub(crate) fn read_log(path: &Path) -> Vec<Option<i32>> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.is_object())
        .map(|v| {
            v.get("exit")
                .and_then(Value::as_i64)
                .and_then(|c| i32::try_from(c).ok())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(600);

    fn reply(line: &str) -> Value {
        match handle(line.as_bytes(), "me@lab", T) {
            Action::Reply(v) => v,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn requests_get_their_answers() {
        assert_eq!(
            reply(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#),
            json!({"jsonrpc":"2.0","id":7,"result":{}})
        );
        let init = reply(
            r#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
        );
        assert_eq!(init["id"], "a");
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(init["result"]["capabilities"], json!({"tools": {}}));
        assert_eq!(
            reply(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)["result"]["protocolVersion"],
            DEFAULT_PROTOCOL
        );
        let list = reply(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "run");
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["command"]));
        assert!(tools[0]["description"].as_str().unwrap().contains("me@lab"));
    }

    #[test]
    fn errors_follow_json_rpc() {
        assert_eq!(
            reply(r#"{"jsonrpc":"2.0","id":3,"method":"server/discover"}"#)["error"]["code"],
            METHOD_NOT_FOUND
        );
        let bad = reply("{not json");
        assert_eq!(bad["error"]["code"], PARSE_ERROR);
        assert_eq!(bad["id"], Value::Null);
        assert_eq!(reply("[1,2]")["error"]["code"], INVALID_REQUEST);
        assert_eq!(reply(r#"{"id":4}"#)["error"]["code"], INVALID_REQUEST);
        // Notifications and responses get no reply.
        for quiet in [
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","method":"nope"}"#,
            r#"{"jsonrpc":"2.0","id":5,"result":{}}"#,
        ] {
            assert_eq!(handle(quiet.as_bytes(), "h", T), Action::Nothing, "{quiet}");
        }
        assert_eq!(
            handle(
                br#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"run"}}"#,
                "h",
                T
            ),
            Action::Call(json!(6), json!({"name": "run"}))
        );
    }

    #[test]
    fn commands_are_checked_before_ssh_runs() {
        assert!(check_command("uname -a").is_ok());
        assert!(check_command("é ü 'q' \"x\" $(id)").is_ok());
        assert_eq!(check_command("  "), Err("`command` is empty".into()));
        assert!(check_command("a\0b").unwrap_err().contains("NUL"));
        assert!(check_command(&"x".repeat(MAX_COMMAND)).is_ok());
        assert!(
            check_command(&"x".repeat(MAX_COMMAND + 1))
                .unwrap_err()
                .contains("longer than 128 KiB")
        );
    }

    #[test]
    fn outputs_are_cut_at_the_cap() {
        let cap = OUTPUT_CAP as usize;
        assert_eq!(capped(&vec![b'a'; cap]).len(), cap);
        let long = capped(&vec![b'a'; cap + 1]);
        assert!(
            long.ends_with("\n[truncated: more than 256 KiB]"),
            "{}",
            &long[cap..]
        );
        assert_eq!(
            outputs(b"out\n", b""),
            "--- stdout ---\nout\n\n--- stderr ---\n"
        );
    }
}
