//! `mcc remote` (#142): a profile works on another machine over SSH.
//! Claude Code (the fake in `tests/common`) runs here with one MCP tool,
//! `mcc remote-tool`, which runs each command on the machine through the
//! hardened ssh argument list; the machine is a sandbox of its own behind
//! the fake ssh. Nothing here reaches a network or a real claude.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{Sandbox, json, mcc_bin, with_profile};
use predicates::prelude::*;
use serde_json::Value;

/// The note every run prints on stderr, verbatim.
const DISCLOSURE: &str = "ⓘ mcc remote sends no login or token to the machine. Claude Code runs here, as the profile, with no tools of its own: it can only run commands on the machine through mcc, over SSH with your own key and config, as the remote user. What Claude knows (the task, the profile's CLAUDE.md, earlier output) can reach the machine in those commands, and output from the machine can steer Claude. A command may keep running there after mcc stops.";

/// The terms-of-service note for starting another account than the project's.
const RUN_NOTE: &str = "⚠ You chose to start an account other than this project's. Use only accounts you own, and don't use several accounts to get around usage limits — doing so can violate Anthropic's Consumer Terms (https://www.anthropic.com/legal/consumer-terms) and Usage Policy (https://www.anthropic.com/legal/aup), including the updated Usage Policy that takes effect November 12, 2026.";

/// The ssh options every command on the machine runs with, in order.
const HARDENED: [&str; 24] = [
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
];

/// Credential and Claude Code variables set in mcc's own environment, which
/// must reach neither Claude Code (but CLAUDE_CONFIG_DIR) nor ssh.
const SECRETS: [(&str, &str); 5] = [
    ("ANTHROPIC_API_KEY", "test-api-key"),
    ("ANTHROPIC_AUTH_TOKEN", "test-auth-token"),
    ("CLAUDE_CODE_OAUTH_TOKEN", "test-oauth-token"),
    ("CLAUDE_CODE_ENABLE_CFC", "1"),
    ("CLAUDE_CONFIG_DIR", "/elsewhere/claude"),
];

/// A task that must never be logged or put on a command line.
const TASK: &str = "free disk space: SENTINEL-TASK-7f3a 'quoted' $HOME";

/// This machine with profile `work`, and machine `box` (host `me@box`)
/// served from a sandbox of its own.
fn setup() -> (Sandbox, Sandbox) {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--fresh");
    let host = Sandbox::new();
    sb.add_machine("box", "me@box", Some(&host), None);
    (sb, host)
}

/// `mcc remote ARGS`, with the secrets in its environment.
fn remote(sb: &Sandbox, args: &[&str]) -> assert_cmd::Command {
    let mut cmd = sb.mcc();
    cmd.arg("remote").args(args).envs(SECRETS);
    cmd
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Whether the fake claude ran as `mcc remote` starts it.
fn claude_ran(sb: &Sandbox) -> bool {
    sb.exists("remote-args")
}

/// What is left of the runs' private folders.
fn scratch_left(sb: &Sandbox) -> Vec<PathBuf> {
    std::fs::read_dir(sb.path(".mcc/state/remote"))
        .map(|d| d.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default()
}

/// Wait up to `secs` for `done`; whether it came true.
fn wait_for(secs: u64, mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

/// Whether any process of group `pgid` is left.
fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 only checks that the group exists.
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// The fake claude's process group, once it has started.
fn claude_group(sb: &Sandbox) -> i32 {
    assert!(
        wait_for(20, || sb.exists("remote-pid")),
        "claude never started"
    );
    sb.read("remote-pid").trim().parse().unwrap()
}

// ---- mcc remote-tool: the MCP server, over stdio --------------------------

/// `mcc remote-tool` running, with a thread reading its answers.
struct Tool {
    child: Child,
    input: Option<ChildStdin>,
    answers: mpsc::Receiver<String>,
}

impl Tool {
    fn start(sb: &Sandbox, host: &str, extra: &[&str]) -> Self {
        let mut child = sb
            .command(mcc_bin())
            .args(["remote-tool", "--ssh"])
            .arg(sb.path("fakebin/ssh"))
            .args(["--host", host, "--log"])
            .arg(sb.path("tool.log"))
            .args(extra)
            .envs(SECRETS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, answers) = mpsc::channel();
        std::thread::spawn(move || {
            for line in out.lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Self {
            input: child.stdin.take(),
            child,
            answers,
        }
    }

    fn send(&mut self, line: &str) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{line}").unwrap();
        input.flush().unwrap();
    }

    /// The next answer, within 20 seconds.
    fn recv(&self) -> Value {
        let line = self
            .answers
            .recv_timeout(Duration::from_secs(20))
            .expect("an answer");
        json(line.as_bytes())
    }

    /// `tools/call` of `run` with `arguments`, and its result.
    fn call(&mut self, id: u32, arguments: Value) -> Value {
        let req = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "run", "arguments": arguments}});
        self.send(&req.to_string());
        let answer = self.recv();
        assert_eq!(answer["id"], id, "{answer}");
        answer["result"].clone()
    }

    /// Close stdin; its exit code, which must come within `secs`.
    fn finish(mut self, secs: u64) -> i32 {
        drop(self.input.take());
        let mut status = None;
        assert!(
            wait_for(secs, || {
                status = self.child.try_wait().unwrap();
                status.is_some()
            }),
            "remote-tool did not exit"
        );
        status.unwrap().code().unwrap()
    }
}

/// The text of a `tools/call` result, and whether it is an error.
fn tool_text(result: &Value) -> (String, bool) {
    (
        result["content"][0]["text"].as_str().unwrap().to_string(),
        result["isError"].as_bool().unwrap(),
    )
}

/// The fake ssh's runs: each one's arguments.
fn ssh_runs(sb: &Sandbox) -> Vec<Vec<String>> {
    sb.ssh_log()
        .iter()
        .map(|l| l.split('\t').skip(1).map(str::to_string).collect())
        .collect()
}

/// AC3, T7: the MCP handshake Claude Code makes, answered as the protocol
/// says; `ping` gets `{}`, notifications nothing.
#[test]
fn remote_tool_answers_the_mcp_handshake() {
    let sb = Sandbox::new();
    let mut tool = Tool::start(&sb, "me@box", &[]);
    tool.send(r#"{"jsonrpc":"2.0","id":0,"method":"server/discover","params":{}}"#);
    let discover = tool.recv();
    assert_eq!(discover["id"], 0);
    assert_eq!(discover["error"]["code"], -32601);
    tool.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"claude-code","version":"2.1.331"}}}"#);
    let init = tool.recv();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        init["result"]["capabilities"],
        serde_json::json!({"tools": {}})
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "mcc-remote");
    tool.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    tool.send(r#"{"jsonrpc":"2.0","id":"p1","method":"ping"}"#);
    // The notification got no answer: the next one is the ping's.
    assert_eq!(
        tool.recv(),
        serde_json::json!({"jsonrpc": "2.0", "id": "p1", "result": {}})
    );
    tool.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let tools = tool.recv()["result"]["tools"].clone();
    assert_eq!(tools.as_array().unwrap().len(), 1, "{tools}");
    assert_eq!(tools[0]["name"], "run");
    assert_eq!(
        tools[0]["inputSchema"]["required"],
        serde_json::json!(["command"])
    );
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["command"]["type"],
        "string"
    );
    tool.send("{not json");
    let bad = tool.recv();
    assert_eq!(bad["error"]["code"], -32700);
    assert_eq!(bad["id"], Value::Null);
    assert_eq!(tool.finish(10), 0);
    assert!(sb.ssh_log().is_empty(), "nothing asked to run a command");
}

/// AC1, T2, T3: a command reaches the machine as one word after the host,
/// expanded by the machine's shell, never here; ssh gets no credential,
/// Claude Code or mcc variable; the record has no command text.
#[test]
fn remote_tool_runs_one_word_on_the_machine() {
    let sb = Sandbox::new();
    let host = Sandbox::new();
    sb.add_machine("box", "me@box", Some(&host), None);
    let mut tool = Tool::start(&sb, "me@box", &[]);
    let command =
        r#"echo "$HOME" $(echo "$USER") `echo x`; printf 'ü <b>' | cat > c; echo 'q' "a;b""#;
    let (out, is_error) = tool_text(&tool.call(1, serde_json::json!({"command": command})));
    assert!(!is_error, "{out}");
    assert!(out.starts_with("exit status 0\n--- stdout ---\n"), "{out}");
    // Expanded there: the machine's HOME, its user, its command output.
    assert_eq!(
        out,
        format!(
            "exit status 0\n--- stdout ---\n{} remote-user x\nq a;b\n\n--- stderr ---\n",
            host.home().display()
        )
    );
    assert_eq!(host.read("c"), "ü <b>");
    assert!(!sb.exists("c"));
    let runs = ssh_runs(&sb);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0][..24], HARDENED);
    assert_eq!(runs[0][24..], ["me@box", command]);
    assert_eq!(sb.read("ssh-env.log"), "", "variables reached ssh");
    let log = sb.read("tool.log");
    assert!(
        !log.contains("echo") && !log.contains("remote-user"),
        "{log}"
    );
    let record = json(log.lines().next().unwrap().as_bytes());
    assert_eq!(record["exit"], 0);
    assert_eq!(record["timed_out"], false);
    assert!(record["ms"].is_u64(), "{record}");
    let mode = std::fs::metadata(sb.path("tool.log"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // A failing command is a normal result, with its status and stderr.
    let (out, is_error) =
        tool_text(&tool.call(2, serde_json::json!({"command": "echo oops >&2; exit 3"})));
    assert!(!is_error, "{out}");
    assert_eq!(
        out,
        "exit status 3\n--- stdout ---\n\n--- stderr ---\noops\n"
    );
    assert_eq!(tool.finish(10), 0);
    assert_eq!(sb.read("tool.log").lines().count(), 2);
}

/// AC4: a machine that can't be reached, a command that runs too long, and
/// calls that can't run are errors Claude Code sees as such; a ping is
/// answered while a command runs.
#[test]
fn remote_tool_reports_what_could_not_run() {
    let sb = Sandbox::new();
    let host = Sandbox::new();
    sb.add_machine("box", "me@box", Some(&host), None);

    let mut down = Tool::start(&sb, "down", &[]);
    let (out, is_error) = tool_text(&down.call(1, serde_json::json!({"command": "true"})));
    assert!(is_error);
    assert!(
        out.starts_with("exit status 255: ssh could not connect to down"),
        "{out}"
    );
    assert!(out.contains("Connection refused"), "{out}");
    assert_eq!(down.finish(10), 0);

    let mut tool = Tool::start(&sb, "me@box", &["--command-timeout", "1"]);
    tool.send(
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"run","arguments":{"command":"exec sleep 30"}}}"#,
    );
    tool.send(r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#);
    // The ping is answered before the command ends.
    assert_eq!(tool.recv()["id"], 6);
    let slow = tool.recv();
    assert_eq!(slow["id"], 5);
    let (out, is_error) = tool_text(&slow["result"]);
    assert!(is_error);
    assert!(out.starts_with("timed out after 1 seconds"), "{out}");
    assert!(out.contains("MAY STILL BE RUNNING on me@box"), "{out}");

    for (args, why) in [
        (
            serde_json::json!({}),
            "`command` is missing or not a string",
        ),
        (
            serde_json::json!({"command": 7}),
            "`command` is missing or not a string",
        ),
        (serde_json::json!({"command": " "}), "`command` is empty"),
    ] {
        let (out, is_error) = tool_text(&tool.call(7, args));
        assert!(is_error);
        assert_eq!(out, why);
    }
    tool.send(r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"Bash","arguments":{"command":"id"}}}"#);
    let (out, is_error) = tool_text(&tool.recv()["result"]);
    assert!(is_error);
    assert_eq!(out, "unknown tool \"Bash\": the only tool is `run`");
    assert_eq!(tool.finish(10), 0);

    let records: Vec<Value> = sb
        .read("tool.log")
        .lines()
        .map(|l| json(l.as_bytes()))
        .collect();
    assert_eq!(records.len(), 2, "only commands that ran are recorded");
    assert_eq!(records[0]["exit"], 255);
    assert_eq!(records[1]["exit"], Value::Null);
    assert_eq!(records[1]["timed_out"], true);
}

/// T5: when Claude Code goes away (stdin ends), the running command's ssh
/// is stopped and the tool exits 0 at once.
#[test]
fn remote_tool_stops_its_command_when_stdin_ends() {
    let sb = Sandbox::new();
    let host = Sandbox::new();
    sb.add_machine("box", "me@box", Some(&host), None);
    let mut tool = Tool::start(&sb, "me@box", &[]);
    tool.send(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"run","arguments":{"command":"exec sleep 30"}}}"#,
    );
    assert!(wait_for(20, || !sb.ssh_log().is_empty()));
    let start = Instant::now();
    assert_eq!(tool.finish(10), 0);
    assert!(start.elapsed() < Duration::from_secs(10));
    let record = json(sb.read("tool.log").lines().next().unwrap().as_bytes());
    assert_eq!(record["exit"], Value::Null);
}

/// T4: a host ssh could read as an option or a shell could act on, or a
/// relative ssh, is refused before anything runs.
#[test]
fn remote_tool_refuses_bad_hosts_and_a_relative_ssh() {
    let sb = Sandbox::new();
    for host in ["--host=a;b", "--host=-oProxyCommand=x", "--host=box*"] {
        let out = sb
            .command(mcc_bin())
            .args(["remote-tool", "--ssh"])
            .arg(sb.path("fakebin/ssh"))
            .args([host, "--log", "t.log"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{host}: {}", text(&out.stderr));
    }
    let out = sb
        .command(mcc_bin())
        .args([
            "remote-tool",
            "--ssh",
            "fakebin/ssh",
            "--host",
            "box",
            "--log",
            "t.log",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("is not an absolute path"));
    assert!(sb.ssh_log().is_empty());
}

// ---- mcc remote -------------------------------------------------------------

/// AC1, AC3, T1, T3, T6: one command starts the profile on the task with
/// exactly Claude Code's headless flags and mcc's tool alone, from a
/// private folder under ~/.mcc/state/remote that is gone afterwards; the
/// task goes in on stdin only and never reaches mcc.log.
#[test]
fn remote_starts_the_profile_headless_with_only_the_tool() {
    let (sb, _host) = setup();
    let out = remote(&sb, &["work", "box", TASK]).assert().success();
    let (stdout, stderr) = (
        text(&out.get_output().stdout),
        text(&out.get_output().stderr),
    );
    assert_eq!(stdout, "fake report\n");
    assert!(stderr.lines().any(|l| l == DISCLOSURE), "{stderr}");
    assert!(
        stderr.contains("✓ box: success, 2 turns, $0.0123, 0 remote commands (0 failed)"),
        "{stderr}"
    );

    let argv: Vec<String> = sb.read("remote-args").lines().map(str::to_string).collect();
    let at = argv.iter().position(|a| a == "--mcp-config").unwrap();
    let mcp = PathBuf::from(&argv[at + 1]);
    let scratch = mcp.parent().unwrap().to_path_buf();
    assert!(
        scratch.starts_with(sb.path(".mcc/state/remote")),
        "{}",
        scratch.display()
    );
    assert_eq!(mcp.file_name().unwrap(), "mcp.json");
    let expected = [
        "-p",
        "--output-format",
        "json",
        "--tools",
        "",
        "--no-chrome",
        "--strict-mcp-config",
        "--mcp-config",
        mcp.to_str().unwrap(),
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
    ];
    assert_eq!(argv[..argv.len() - 1], expected, "{argv:#?}");
    let prompt = argv.last().unwrap();
    assert!(prompt.contains("the machine box (me@box)"), "{prompt}");
    assert!(prompt.contains("exactly one tool, `run`"), "{prompt}");
    for never in ["--bare", "--restricted", "--dangerously-skip-permissions"] {
        assert!(!argv.iter().any(|a| a == never), "{never}");
    }

    // The task: on stdin, nowhere else.
    assert_eq!(sb.read("remote-stdin"), TASK);
    assert!(!argv.iter().any(|a| a.contains("SENTINEL-TASK")));
    let log = sb.read(".mcc/logs/mcc.log");
    assert!(!log.contains("SENTINEL-TASK"), "{log}");
    assert!(log.contains("<task redacted>"), "{log}");
    assert!(!log.contains("fake report"), "{log}");
    assert!(
        log.contains("remote: profile work on machine box: exit 0, success, 2 turns"),
        "{log}"
    );

    // The MCP config: mcc's own tool for this host, private.
    let cfg = json(sb.read("remote-mcp.json").as_bytes());
    let server = &cfg["mcpServers"]["mcc-remote"];
    assert_eq!(cfg["mcpServers"].as_object().unwrap().len(), 1);
    let exe = std::fs::canonicalize(mcc_bin()).unwrap();
    assert_eq!(server["command"], exe.to_str().unwrap());
    let tool_log = scratch.join("tool.log");
    assert_eq!(
        server["args"],
        serde_json::json!([
            "remote-tool",
            "--ssh",
            sb.path("fakebin/ssh").to_str().unwrap(),
            "--host",
            "me@box",
            "--log",
            tool_log.to_str().unwrap()
        ])
    );
    let mode = std::fs::metadata(sb.path("remote-mcp.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // Run from that folder (0700), as the profile, with no other credential.
    let cwd = PathBuf::from(sb.read("remote-cwd").trim());
    let state = std::fs::canonicalize(sb.path(".mcc/state/remote")).unwrap();
    assert!(cwd.starts_with(&state), "{}", cwd.display());
    assert!(sb.read("remote-cwd-mode").starts_with("drwx------"));
    assert_eq!(
        sb.read("remote-env"),
        format!(
            "CLAUDE_CONFIG_DIR={}\n",
            sb.path(".mcc/profiles/work").display()
        )
    );
    assert!(!scratch.exists(), "the private folder is removed");
    assert!(scratch_left(&sb).is_empty());

    // Only the preflight ran on the machine.
    let runs = ssh_runs(&sb);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0][..24], HARDENED);
    assert_eq!(runs[0][24..], ["me@box", "true"]);
    assert_eq!(sb.read("ssh-env.log"), "");
}

/// AC1, AC2: end to end, Claude Code's tool runs a command on a machine
/// with no Claude Code and no mcc (only a shell), and the summary counts it.
#[test]
fn remote_runs_commands_on_a_machine_without_claude_code() {
    let (sb, host) = setup();
    let out = remote(&sb, &["work", "box", "say hello"])
        .env("FAKE_REMOTE_CALL", "echo hello > greeting; exit 4")
        .assert()
        .success();
    let stderr = text(&out.get_output().stderr);
    assert!(
        stderr.contains("✓ box: success, 2 turns, $0.0123, 1 remote command (1 failed)"),
        "{stderr}"
    );
    assert_eq!(host.read("greeting"), "hello\n");
    let answers = sb.read("remote-mcp-out");
    assert!(answers.contains("exit status 4"), "{answers}");
    let runs = ssh_runs(&sb);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1][24..], ["me@box", "echo hello > greeting; exit 4"]);
    assert_eq!(sb.read("ssh-env.log"), "", "variables reached ssh");
    assert!(scratch_left(&sb).is_empty());
}

/// AC4, T4: a machine that can't be reached is reported with the reason,
/// and Claude Code never starts.
#[test]
fn remote_says_why_it_could_not_start() {
    let (sb, _host) = setup();
    sb.add_machine("off", "down", None, None);
    remote(&sb, &["work", "off", TASK])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "ssh could not connect to down without a prompt: check that `ssh down true` works with your key",
        ))
        .stderr(predicate::str::contains("Claude Code was not started"));
    assert!(!claude_ran(&sb));
    assert_eq!(ssh_runs(&sb).len(), 1);
    assert!(scratch_left(&sb).is_empty());
    assert!(!sb.read(".mcc/logs/mcc.log").contains("SENTINEL-TASK"));
}

/// T4, L4: an unknown profile or machine, or a host that isn't a plain
/// word, is refused before ssh or Claude Code runs.
#[test]
fn remote_refuses_unknown_names_and_odd_hosts() {
    let (sb, _host) = setup();
    remote(&sb, &["nope", "box", TASK])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no profile named \"nope\""));
    remote(&sb, &["work", "nope", TASK])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no machine named \"nope\""))
        .stderr(predicate::str::contains("the machines set up are `box`"));
    for (name, host) in [("star", "box*"), ("semi", "box;id"), ("pct", "box%h")] {
        sb.add_machine(name, host, None, None);
        remote(&sb, &["work", name, TASK])
            .assert()
            .code(3)
            .stderr(predicate::str::contains(format!(
                "[machines.{name}] can't be used: host"
            )));
    }
    assert!(sb.ssh_log().is_empty(), "ssh ran");
    assert!(!claude_ran(&sb));

    let bare = Sandbox::new();
    bare.mcc()
        .args(["remote", "default", "box", "x"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no machines are set up"));
}

/// L1: no claude to start is exit 127, and the private folder goes.
#[test]
fn remote_without_claude_code_here_exits_127() {
    let (sb, _host) = setup();
    remote(&sb, &["work", "box", TASK])
        .env("MCC_CLAUDE_BIN", sb.path("fakebin/no-claude"))
        .assert()
        .code(127)
        .stderr(predicate::str::contains("couldn't run"));
    assert!(scratch_left(&sb).is_empty());
}

/// AC4: how the task ended decides the exit status; refused tool calls
/// are listed, and none of it reaches mcc.log.
#[test]
fn remote_reports_how_the_task_ended() {
    let (sb, _host) = setup();
    let max = r#"{"type":"result","subtype":"error_max_turns","is_error":false,"num_turns":31,"total_cost_usd":0.5,"result":"RESULT-SENTINEL","permission_denials":[{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"DENIAL-SENTINEL rm -rf /"}}]}"#;
    let out = remote(&sb, &["work", "box", TASK, "--max-turns", "30"])
        .env("FAKE_REMOTE_RESULT", max)
        .assert()
        .code(1);
    let (stdout, stderr) = (
        text(&out.get_output().stdout),
        text(&out.get_output().stderr),
    );
    assert_eq!(stdout, "RESULT-SENTINEL\n");
    assert!(
        stderr.contains(r#"refused: Bash {"command":"DENIAL-SENTINEL rm -rf /"}"#),
        "{stderr}"
    );
    assert!(
        stderr.contains("✗ box: error_max_turns, 31 turns, $0.5000, 0 remote commands (0 failed)"),
        "{stderr}"
    );
    assert!(stderr.contains("--max-turns 30"), "{stderr}");
    let log = sb.read(".mcc/logs/mcc.log");
    for secret in ["RESULT-SENTINEL", "DENIAL-SENTINEL", "SENTINEL-TASK"] {
        assert!(!log.contains(secret), "{secret} in {log}");
    }
    assert!(log.contains("exit 1, error_max_turns, 31 turns"), "{log}");

    let failed = r#"{"type":"result","subtype":"success","is_error":true,"num_turns":1,"result":"API error"}"#;
    remote(&sb, &["work", "box", TASK])
        .env("FAKE_REMOTE_RESULT", failed)
        .assert()
        .code(1)
        .stdout("API error\n")
        .stderr(predicate::str::contains(
            "✗ box: success, 1 turns, cost unknown",
        ));

    // Claude Code's own failure status passes through.
    remote(&sb, &["work", "box", TASK])
        .env("FAKE_REMOTE_EXIT", "3")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("✗ box: success"));

    // --max-turns reaches Claude Code as given.
    remote(&sb, &["work", "box", TASK, "--max-turns", "7"])
        .assert()
        .success();
    let argv = sb.read("remote-args");
    assert!(argv.contains("\n--max-turns\n7\n"), "{argv}");

    // --json: Claude Code's result as it is, parseable.
    let out = remote(&sb, &["work", "box", TASK, "--json"])
        .assert()
        .success();
    let v = json(&out.get_output().stdout);
    assert_eq!(v["result"], "fake report");
}

/// AC4, L1: output that isn't a result is shown as it is, with a warning,
/// and never counts as success.
#[test]
fn remote_passes_unreadable_output_through() {
    let (sb, _host) = setup();
    remote(&sb, &["work", "box", TASK])
        .env("FAKE_REMOTE_RESULT", "Invalid API key · Please run /login")
        .assert()
        .code(1)
        .stdout("Invalid API key · Please run /login\n")
        .stderr(predicate::str::contains(
            "✗ box: could not read Claude Code's result (it exited 0)",
        ));
    remote(&sb, &["work", "box", TASK])
        .env("FAKE_REMOTE_RESULT", "oops")
        .env("FAKE_REMOTE_EXIT", "2")
        .assert()
        .code(2);
}

/// `mcc remote work box TASK ARGS` started with a Claude Code that sleeps
/// (ignoring SIGTERM with `ignore_term`).
fn start_sleeping(sb: &Sandbox, args: &[&str], ignore_term: bool) -> Child {
    let mut cmd = sb.command(mcc_bin());
    cmd.args(["remote", "work", "box", TASK])
        .args(args)
        .envs(SECRETS)
        .env("FAKE_REMOTE_SLEEP", "30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if ignore_term {
        cmd.env("FAKE_REMOTE_IGNORE_TERM", "1");
    }
    cmd.spawn().unwrap()
}

/// Wait for mcc to exit; its exit code, and its stderr, read once nothing
/// holds it any more. Claude Code's process group must be gone by the
/// time mcc has exited (give or take its last processes' end).
fn stopped(mut child: Child, pgid: i32) -> (i32, String) {
    let mut err = child.stderr.take().unwrap();
    let stderr = std::thread::spawn(move || {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut err, &mut s).unwrap();
        s
    });
    let status = child.wait().unwrap();
    assert!(wait_for(5, || !group_alive(pgid)), "a process was left");
    (status.code().unwrap(), stderr.join().unwrap())
}

/// T5: --timeout stops Claude Code and everything it started, removes the
/// private folder, and exits 124.
#[test]
fn remote_timeout_stops_the_whole_run() {
    let (sb, _host) = setup();
    let child = start_sleeping(&sb, &["--timeout", "1"], false);
    let pgid = claude_group(&sb);
    let (code, stderr) = stopped(child, pgid);
    assert_eq!(code, 124, "{stderr}");
    assert!(
        stderr.contains(
            "stopped: the run took longer than --timeout 1s; a command may still be running on box (me@box)"
        ),
        "{stderr}"
    );
    assert!(scratch_left(&sb).is_empty());
}

/// T5: a run stopped while a command runs on the machine leaves no tool
/// and no ssh behind.
#[test]
fn remote_timeout_stops_the_tool_and_its_ssh() {
    let (sb, _host) = setup();
    let mut cmd = sb.command(mcc_bin());
    let child = cmd
        .args(["remote", "work", "box", TASK, "--timeout", "2"])
        .env("FAKE_REMOTE_CALL", "exec sleep 30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pgid = claude_group(&sb);
    // The tool's ssh is running the command (preflight first, then it).
    assert!(
        wait_for(20, || sb.ssh_log().len() == 2),
        "the command never ran"
    );
    assert!(group_alive(pgid));
    let (code, stderr) = stopped(child, pgid);
    assert_eq!(code, 124, "{stderr}");
    assert!(scratch_left(&sb).is_empty());
}

/// T5: a run that ignores SIGTERM is killed once the grace is over.
#[test]
fn remote_timeout_kills_a_run_that_ignores_sigterm() {
    let (sb, _host) = setup();
    let start = Instant::now();
    let child = start_sleeping(&sb, &["--timeout", "1"], true);
    let pgid = claude_group(&sb);
    let (code, stderr) = stopped(child, pgid);
    assert_eq!(code, 124, "{stderr}");
    // The run sleeps 30 seconds: only SIGKILL ends it this soon.
    assert!(
        start.elapsed() < Duration::from_secs(15),
        "{:?}",
        start.elapsed()
    );
    assert!(scratch_left(&sb).is_empty());
}

/// T5: SIGTERM and Ctrl+C (SIGINT) stop the whole run too, and remove the
/// private folder.
#[test]
fn remote_signals_stop_the_whole_run() {
    let (sb, _host) = setup();
    for (signal, code, why) in [
        (
            libc::SIGTERM,
            143,
            "stopped: mcc received SIGTERM; a command may still be running on box (me@box)",
        ),
        (libc::SIGHUP, 129, "stopped: mcc received SIGHUP"),
        (libc::SIGINT, 130, "stopped: interrupted (Ctrl+C)"),
    ] {
        let _ = std::fs::remove_file(sb.path("remote-pid"));
        let child = start_sleeping(&sb, &[], false);
        let pgid = claude_group(&sb);
        // SAFETY: a plain signal to the child started above.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        let (got, stderr) = stopped(child, pgid);
        assert_eq!(got, code, "{stderr}");
        assert!(stderr.contains(why), "{stderr}");
        assert!(scratch_left(&sb).is_empty());
    }
}

/// The task from a file or from stdin; an empty one is refused before
/// anything runs.
#[test]
fn remote_reads_the_task_from_a_file_or_stdin() {
    let (sb, _host) = setup();
    sb.write("plan.md", "step 1: ü <b> 'q'\nstep 2\n");
    remote(&sb, &["work", "box", "--task-file", "plan.md"])
        .assert()
        .success();
    assert_eq!(sb.read("remote-stdin"), "step 1: ü <b> 'q'\nstep 2\n");
    // The file's name is logged, not what it holds.
    let log = sb.read(".mcc/logs/mcc.log");
    assert!(log.contains(r#""--task-file", "plan.md""#), "{log}");
    assert!(!log.contains("step 1"), "{log}");

    remote(&sb, &["work", "box", "-"])
        .write_stdin("from stdin")
        .assert()
        .success();
    assert_eq!(sb.read("remote-stdin"), "from stdin");

    std::fs::remove_file(sb.path("remote-args")).unwrap();
    let before = sb.ssh_log().len();
    for empty in [&["work", "box", "  "][..], &["work", "box", "-"]] {
        remote(&sb, empty)
            .write_stdin("\n")
            .assert()
            .code(2)
            .stderr(predicate::str::contains("is empty"));
    }
    remote(&sb, &["work", "box", "--task-file", "missing.md"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "could not read the task from missing.md",
        ));
    remote(&sb, &["work", "box", "x", "--task-file", "plan.md"])
        .assert()
        .code(2);
    assert!(!claude_ran(&sb));
    assert_eq!(sb.ssh_log().len(), before);
}

/// Starting a profile other than the project's prints the terms note;
/// the project's own profile doesn't.
#[test]
fn remote_prints_the_terms_note_for_another_profile() {
    let (sb, _host) = setup();
    sb.mcc()
        .args(["project", "set", "default"])
        .assert()
        .success();
    let note = |profile: &str| {
        let out = remote(&sb, &[profile, "box", TASK]).assert().success();
        text(&out.get_output().stderr)
            .lines()
            .any(|l| l == RUN_NOTE)
    };
    assert!(note("work"));
    assert!(!note("default"));
}

/// Help lists the flags and carries the note verbatim.
#[test]
fn remote_help_shows_the_note() {
    let sb = Sandbox::new();
    let out = sb.mcc().args(["remote", "--help"]).assert().success();
    let help = text(&out.get_output().stdout);
    for flag in ["--task-file", "--max-turns", "--timeout", "--json"] {
        assert!(help.contains(flag), "{flag}");
    }
    let flat: String = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains(DISCLOSURE), "{help}");
    // The tool is not a command people run.
    let top = sb.mcc().arg("--help").assert().success();
    assert!(!text(&top.get_output().stdout).contains("remote-tool"));
}
