//! Test sandbox: a temp HOME, a fake `claude` binary, and an `mcc` command
//! wired to them. Nothing here touches the real ~/.claude, rc files, or keychain,
//! and nothing reaches the network: the keychain is a fake `security` script and
//! the usage endpoint is a refused loopback port unless a test starts a stub.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use assert_cmd::Command;
use tempfile::TempDir;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
# Fake claude for tests: login writes a credentials file, logout removes it.
dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
case "$1 $2" in
  "--version "*) echo "9.9.9 (Claude Code)" ;;
  "auth login")
    mkdir -p "$dir"
    [ -n "$FAKE_LOGIN_FAIL" ] && { echo "login aborted" >&2; exit 1; }
    echo '{"token":"fake"}' > "$dir/.credentials.json"
    echo "Login successful." ;;
  "auth status")
    if [ -f "$dir/.credentials.json" ]; then
      echo '{"loggedIn": true, "authMethod": "claude.ai", "email": "fake@example.com"}'
    else
      echo '{"loggedIn": false, "authMethod": "none"}'
    fi ;;
  "auth logout")
    # Like the real claude (2.1.x): exits 0 even when nothing was logged in.
    rm -f "$dir/.credentials.json"; echo "Successfully logged out." ;;
  *) echo "fake claude $*" ;;
esac
"#;

const FAKE_SECURITY: &str = r#"#!/bin/sh
# Fake macOS `security`: serves $HOME/fake-keychain/<service> for account
# mcc-test, else "item not found" (exit 44) like the real one.
echo "$*" >> "$HOME/security.log"
svc=""; acct=""
while [ $# -gt 0 ]; do
  case "$1" in -s) svc="$2"; shift ;; -a) acct="$2"; shift ;; esac
  shift
done
f="$HOME/fake-keychain/$svc"
if [ "$acct" = "mcc-test" ] && [ -f "$f" ]; then cat "$f"; exit 0; fi
echo "security: The specified item could not be found in the keychain." >&2
exit 44
"#;

/// Nothing listens on the discard port: connections are refused at once.
pub const REFUSED_URL: &str = "http://127.0.0.1:9/api/oauth/usage";

pub struct Sandbox {
    pub tmp: TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("fakebin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(tmp.path().join(".local/bin")).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).unwrap();
        chmod_x(&claude);
        let security = bin.join("security");
        std::fs::write(&security, FAKE_SECURITY).unwrap();
        chmod_x(&security);
        Self { tmp }
    }

    pub fn home(&self) -> &Path {
        self.tmp.path()
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }

    pub fn write(&self, rel: &str, content: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
    }

    pub fn exists(&self, rel: &str) -> bool {
        std::fs::symlink_metadata(self.path(rel)).is_ok()
    }

    /// The sandboxed environment every command here runs with. PATH holds
    /// the fakes and `~/.local/bin`, never a real `claude` or `mcc`.
    fn env(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        let home = self.home();
        let path = format!(
            "{}:{}:/usr/bin:/bin",
            home.join("fakebin").display(),
            home.join(".local/bin").display()
        );
        vec![
            ("HOME", home.into()),
            ("MCC_HOME", home.into()),
            ("PATH", path.into()),
            ("SHELL", "/bin/zsh".into()),
            ("TERM_PROGRAM", "TestTerm".into()),
            ("MCC_CLAUDE_BIN", home.join("fakebin/claude").into()),
            ("MCC_SECURITY_BIN", home.join("fakebin/security").into()),
            ("MCC_USAGE_URL", REFUSED_URL.into()),
            ("USER", "mcc-test".into()),
            ("MCC_NO_SHELL_PROBE", "1".into()),
            ("MCC_NON_INTERACTIVE", "1".into()),
            ("NO_COLOR", "1".into()),
        ]
    }

    /// `mcc` with a sandboxed environment (non-interactive: no TTY in tests).
    pub fn mcc(&self) -> Command {
        let mut cmd = Command::cargo_bin("mcc").unwrap();
        cmd.env_clear().envs(self.env()).current_dir(self.home());
        cmd
    }

    /// The launcher `~/.local/bin/<alias>`, run with the sandboxed environment.
    /// `mcc` is not on its PATH, so `--mcc-share` uses the mcc under test.
    pub fn launcher(&self, alias: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new(self.path(&format!(".local/bin/{alias}")));
        cmd.env_clear().envs(self.env()).current_dir(self.home());
        cmd
    }

    /// A realistic default profile at ~/.claude + ~/.claude.json.
    pub fn seed_default(&self) {
        let home = self.home().display().to_string();
        self.write(
            ".claude/settings.json",
            &format!(
                r#"{{"model":"opus","enabledPlugins":{{"p1@m":true,"p2@m":false}},"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"bash '{home}/.claude/hooks/notify.sh'"}}]}}]}},"statusLine":{{"command":"~/.claude/statusline.sh"}}}}"#
            ),
        );
        self.write(".claude/CLAUDE.md", "# my rules\n");
        self.write(".claude/skills/alpha/SKILL.md", "alpha");
        self.write(".claude/skills/beta/SKILL.md", "beta");
        self.write(".claude/agents/reviewer.md", "agent");
        self.write(".claude/commands/deploy.md", "cmd");
        self.write(".claude/hooks/notify.sh", "#!/bin/sh\necho hi\n");
        self.write(
            ".claude/plugins/installed_plugins.json",
            &format!(r#"{{"version":2,"plugins":{{"p1@m":[{{"installPath":"{home}/.claude/plugins/cache/m/p1/1.0.0"}}],"p2@m":[]}}}}"#),
        );
        self.write(".claude/plugins/cache/m/p1/1.0.0/plugin.json", "{}");
        self.write(".claude/history.jsonl", "{}\n");
        self.write(".claude/projects/-tmp-proj/session1.jsonl", "{}\n");
        self.write(".claude/.credentials.json", r#"{"secret":"default-token"}"#);
        self.write(
            ".claude.json",
            r#"{"oauthAccount":{"emailAddress":"me@example.com","organizationName":"Acme"},"userID":"user-123","theme":"dark-ansi","hasCompletedOnboarding":true,"mcpServers":{"fs":{"command":"npx","args":["fs"]}},"projects":{}}"#,
        );
        self.write(
            ".zshrc",
            "export EDITOR=vim\nalias cus_luong='CLAUDE_CONFIG_DIR=~/.cus_luong claude'\n",
        );
    }
}

impl Sandbox {
    /// Claude Code OAuth credentials (`claudeAiOauth`) in `<rel_dir>/.credentials.json`.
    pub fn seed_oauth(&self, rel_dir: &str, token: &str, expires_at_ms: i64) {
        self.write(
            &format!("{rel_dir}/.credentials.json"),
            &oauth_blob(token, expires_at_ms),
        );
    }

    /// The same credentials as a fake-keychain item for `service`.
    pub fn seed_keychain(&self, service: &str, token: &str, expires_at_ms: i64) {
        self.write(
            &format!("fake-keychain/{service}"),
            &oauth_blob(token, expires_at_ms),
        );
    }
}

fn oauth_blob(token: &str, expires_at_ms: i64) -> String {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"refresh-{token}","expiresAt":{expires_at_ms},"scopes":["user:inference","user:profile"],"subscriptionType":"max"}}}}"#
    )
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// One request received by a [`UsageStub`].
#[derive(Debug, Clone)]
pub struct StubRequest {
    pub request_line: String,
    pub authorization: Option<String>,
    pub beta: Option<String>,
}

/// A loopback HTTP server standing in for the usage endpoint. It answers each
/// request with `handler(&request) -> (status, body)` and records it.
pub struct UsageStub {
    pub url: String,
    requests: Arc<Mutex<Vec<StubRequest>>>,
}

impl UsageStub {
    pub fn start(handler: impl Fn(&StubRequest) -> (u16, String) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/api/oauth/usage",
            listener.local_addr().unwrap().port()
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut lines = Vec::new();
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line == "\r\n" => break,
                        Ok(_) => lines.push(line.trim_end().to_string()),
                    }
                }
                let header = |name: &str| {
                    lines.iter().skip(1).find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                    })
                };
                let req = StubRequest {
                    request_line: lines.first().cloned().unwrap_or_default(),
                    authorization: header("authorization"),
                    beta: header("anthropic-beta"),
                };
                let (status, body) = handler(&req);
                seen.lock().unwrap().push(req);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        });
        Self { url, requests }
    }

    pub fn requests(&self) -> Vec<StubRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// The bearer tokens received, sorted.
    pub fn tokens(&self) -> Vec<String> {
        let mut t: Vec<String> = self
            .requests()
            .iter()
            .filter_map(|r| {
                r.authorization
                    .as_deref()?
                    .strip_prefix("Bearer ")
                    .map(str::to_string)
            })
            .collect();
        t.sort();
        t
    }
}

#[cfg(unix)]
pub fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("invalid JSON ({e}): {}", String::from_utf8_lossy(bytes)))
}

// ---- usage fixtures shared by the `mcc list` and `mcc usage` tests ----------

pub fn with_profile(sb: &Sandbox, alias: &str, mode: &str) {
    sb.mcc().args(["add", alias, mode]).assert().success();
}

pub const HOUR_MS: i64 = 3_600_000;

pub fn rfc3339_in(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn stdout_of(cmd: &mut assert_cmd::Command) -> String {
    String::from_utf8(cmd.assert().success().get_output().stdout.clone()).unwrap()
}

pub fn by_name<'a>(arr: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    arr.iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("no profile {name}"))
}

/// default + `work` logged in (each with its own token), `lab` logged out.
pub fn usage_sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--fresh");
    sb.seed_oauth(".claude", "test-token-default", now_ms() + HOUR_MS);
    sb.seed_oauth(".mcc/profiles/work", "test-token-work", now_ms() + HOUR_MS);
    sb
}

/// Default answers with the flat keys, work with the `limits[]` shape.
pub fn usage_stub() -> UsageStub {
    let (five, week) = (
        rfc3339_in(2 * 3600 + 14 * 60),
        rfc3339_in(3 * 86400 + 4 * 3600),
    );
    UsageStub::start(move |req| match req.authorization.as_deref() {
        Some("Bearer test-token-default") => (
            200,
            format!(
                r#"{{"five_hour":{{"utilization":42.4,"resets_at":"{five}"}},"seven_day":{{"utilization":18,"resets_at":"{week}"}},"seven_day_opus":null}}"#
            ),
        ),
        Some("Bearer test-token-work") => (
            200,
            format!(
                r#"{{"five_hour":null,"seven_day":null,"limits":[{{"kind":"session","percent":7,"resets_at":"{five}"}},{{"kind":"weekly_all","percent":64,"resets_at":"{week}"}}]}}"#
            ),
        ),
        _ => (401, r#"{"error":"unknown token"}"#.into()),
    })
}
