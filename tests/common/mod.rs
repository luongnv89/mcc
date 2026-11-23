//! Test sandbox: a temp HOME, a fake `claude` binary, and an `mcc` command
//! wired to them. Nothing here touches the real ~/.claude, rc files, or keychain,
//! and nothing reaches the network: the keychain is a fake `security` script and
//! the usage endpoints (Anthropic's and Devin's) are a refused loopback port
//! unless a test starts a stub. Devin is not set up by default: no
//! credentials file and no `devin` command, unless a test adds them
//! ([`Sandbox::seed_devin_login`], [`Sandbox::install_fake_devin`]).
//! `ssh` is a fake too ([`FAKE_SSH`]): another machine is another sandbox
//! ([`Sandbox::add_machine`]).
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use assert_cmd::Command;
use tempfile::TempDir;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
# Fake claude for tests: login writes a credentials file, logout removes it.
# `auth status` reports $FAKE_EMAIL (default fake@example.com) and, when
# $FAKE_ORG_NAME is set, its organization ($FAKE_ORG_ID, $FAKE_ORG_NAME).
# With $FAKE_CONFIG_DIRECTORY set it also reports `configDirectory` (Claude
# Code 2.1.268+): the directory it uses. `auth login` writes its options to
# $HOME/login-args, one per line. $FAKE_AUTH_METHOD replaces `claude.ai` and
# $FAKE_API_KEY_SOURCE adds `apiKeySource` (a Console sign-in reports
# "api_key" and "/login managed key").
# `mcc login` tests: `auth login` writes $FAKE_LOGIN_CREDS (when set) as the
# credentials and $FAKE_LOGIN_STATE as the state file (.claude.json), then,
# with $FAKE_LOGIN_INTERRUPT, sends Ctrl+C (SIGINT) to its parent. `-p ok` (the
# --refresh-only request) records its arguments in $HOME/p-args and its
# directory in $HOME/p-cwd, exits 1 when $FAKE_REFRESH_MODE=fail, and writes
# $FAKE_REFRESH_CREDS as the credentials when set. It also appends its
# directory to $HOME/p-runs (one line per request), lists what that directory
# holds in $HOME/p-ls, and with $FAKE_REFRESH_INTERRUPT sends Ctrl+C (SIGINT)
# to its parent. Both record the names
# (never values) of credential variables they see in $HOME/claude-env.log.
# Per-directory accounts (`mcc default` tests): `auth status` reports the
# email in <dir>/.fake-account when there is one. With $FAKE_PER_DIR set,
# `auth login` counts itself in $HOME/login-count, fails the Nth time when
# $FAKE_LOGIN_FAIL_AT=N, signs in as $FAKE_LOGIN_AS_<N> if set, else as its
# --email, else as $FAKE_EMAIL, writes that to <dir>/.fake-account and to
# the state file's oauthAccount (replaced whole, as Claude Code does), records organization $FAKE_LOGIN_ORG_<N>
# (an id) in <dir>/.fake-org (which `auth status` reports as orgId), and
# appends "<dir> login <email>" to
# $HOME/auth.log ("<dir> logout" for `auth logout`). `auth login --help`
# lists --email unless $FAKE_NO_EMAIL_FLAG is set (an older Claude Code).
# A <dir>/.fake-status-broken file makes `auth status` print no status
# (an error on stderr, exit 1), as a Claude Code that can't check would;
# $HOME/.fake-status-broken-default does the same for the default location
# only (no CLAUDE_CONFIG_DIR), even when ~/.claude does not exist.
# A login held in the Keychain (macOS), with no ~/.claude directory: when
# $HOME/.fake-keychain-login exists and CLAUDE_CONFIG_DIR is unset, `auth
# status` reports loggedIn true as the email in that file (profiles never
# see it), and the default's `auth logout` removes it.
dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
state="$HOME/.claude.json"
[ -n "$CLAUDE_CONFIG_DIR" ] && state="$CLAUDE_CONFIG_DIR/.claude.json"
envlog() {
  for v in ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR CLAUDE_CODE_USE_BEDROCK CLAUDE_CODE_USE_VERTEX ANTHROPIC_BASE_URL; do
    eval "[ -n \"\${$v+x}\" ]" && echo "$1 $v" >> "$HOME/claude-env.log"
  done
  return 0
}
cfgdir=""
[ -n "$FAKE_CONFIG_DIRECTORY" ] && cfgdir=", \"configDirectory\": \"$dir\""
case "$1 $2" in
  "--version "*) echo "9.9.9 (Claude Code)" ;;
  "auth login")
    if [ "$3" = "--help" ]; then
      echo "Options:"
      [ -z "$FAKE_NO_EMAIL_FLAG" ] && echo "  --email <email>  Pre-populate email address on the login page"
      echo "  --sso            Force SSO login flow"
      exit 0
    fi
    shift 2
    printf '%s\n' "$@" > "$HOME/login-args"
    mkdir -p "$dir"
    envlog login
    [ -n "$FAKE_LOGIN_FAIL" ] && { echo "login aborted" >&2; exit 1; }
    if [ -n "$FAKE_PER_DIR" ]; then
      n=$(( $(cat "$HOME/login-count" 2>/dev/null || echo 0) + 1 ))
      echo "$n" > "$HOME/login-count"
      [ "$FAKE_LOGIN_FAIL_AT" = "$n" ] && { echo "login aborted" >&2; exit 1; }
      who=""
      eval "who=\"\${FAKE_LOGIN_AS_$n:-}\""
      if [ -z "$who" ]; then
        while [ $# -gt 0 ]; do [ "$1" = "--email" ] && who="$2"; shift; done
      fi
      [ -z "$who" ] && who="${FAKE_EMAIL:-fake@example.com}"
      echo "$who" > "$dir/.fake-account"
      org_now=""
      eval "org_now=\"\${FAKE_LOGIN_ORG_$n:-}\""
      if [ -n "$org_now" ]; then echo "$org_now" > "$dir/.fake-org"; else rm -f "$dir/.fake-org"; fi
      echo "$dir login $who" >> "$HOME/auth.log"
      echo '{"token":"fake"}' > "$dir/.credentials.json"
      acct="{\"emailAddress\":\"$who\"}"
      [ -n "$org_now" ] && acct="{\"emailAddress\":\"$who\",\"organizationUuid\":\"$org_now\"}"
      if [ -f "$state" ] && grep -q '"oauthAccount"' "$state"; then
        sed -e "s/\"oauthAccount\": *{[^}]*}/\"oauthAccount\":$acct/" "$state" > "$state.tmp" && mv "$state.tmp" "$state"
      elif [ ! -f "$state" ]; then
        echo "{\"oauthAccount\":$acct}" > "$state"
      fi
      echo "Login successful."
      exit 0
    fi
    if [ -n "$FAKE_LOGIN_CREDS" ]; then
      printf '%s' "$FAKE_LOGIN_CREDS" > "$dir/.credentials.json"
    else
      echo '{"token":"fake"}' > "$dir/.credentials.json"
    fi
    [ -n "$FAKE_LOGIN_STATE" ] && printf '%s' "$FAKE_LOGIN_STATE" > "$state"
    if [ -n "$FAKE_LOGIN_INTERRUPT" ]; then kill -INT "$PPID"; sleep 1; fi
    echo "Login successful." ;;
  "-p ok")
    printf '%s\n' "$@" > "$HOME/p-args"
    pwd > "$HOME/p-cwd"
    pwd >> "$HOME/p-runs"
    ls -A > "$HOME/p-ls"
    envlog p
    if [ -n "$FAKE_REFRESH_INTERRUPT" ]; then kill -INT "$PPID"; sleep 1; fi
    [ "$FAKE_REFRESH_MODE" = fail ] && exit 1
    [ -n "$FAKE_REFRESH_CREDS" ] && printf '%s' "$FAKE_REFRESH_CREDS" > "$dir/.credentials.json"
    echo "fake claude $*" ;;
  "auth status")
    if [ -f "$dir/.fake-status-broken" ]; then
      echo "auth status failed" >&2; exit 1
    elif [ -z "$CLAUDE_CONFIG_DIR" ] && [ -f "$HOME/.fake-status-broken-default" ]; then
      echo "auth status failed" >&2; exit 1
    elif [ -f "$dir/.credentials.json" ]; then
      org=""
      [ -n "$FAKE_ORG_NAME" ] && org=", \"orgId\": \"$FAKE_ORG_ID\", \"orgName\": \"$FAKE_ORG_NAME\""
      [ -f "$dir/.fake-org" ] && org=", \"orgId\": \"$(cat "$dir/.fake-org")\""
      src=""
      [ -n "$FAKE_API_KEY_SOURCE" ] && src=", \"apiKeySource\": \"$FAKE_API_KEY_SOURCE\""
      email="${FAKE_EMAIL:-fake@example.com}"
      [ -f "$dir/.fake-account" ] && email=$(cat "$dir/.fake-account")
      echo "{\"loggedIn\": true, \"authMethod\": \"${FAKE_AUTH_METHOD:-claude.ai}\", \"email\": \"$email\"$org$src$cfgdir}"
    elif [ -z "$CLAUDE_CONFIG_DIR" ] && [ -f "$HOME/.fake-keychain-login" ]; then
      echo "{\"loggedIn\": true, \"authMethod\": \"claude.ai\", \"email\": \"$(cat "$HOME/.fake-keychain-login")\"$cfgdir}"
    else
      echo "{\"loggedIn\": false, \"authMethod\": \"none\"$cfgdir}"
    fi ;;
  "auth logout")
    # Like the real claude (2.1.x): exits 0 even when nothing was logged in.
    rm -f "$dir/.credentials.json" "$dir/.fake-account" "$dir/.fake-org"
    [ -z "$CLAUDE_CONFIG_DIR" ] && rm -f "$HOME/.fake-keychain-login"
    [ -n "$FAKE_PER_DIR" ] && echo "$dir logout" >> "$HOME/auth.log"
    echo "Successfully logged out." ;;
  "plugin-root "*) echo "plugin root: ${CLAUDE_CODE_PLUGIN_CACHE_DIR:-unset}" ;;
  *) echo "fake claude $*" ;;
esac
"#;

const FAKE_SECURITY: &str = r#"#!/bin/sh
# Fake macOS `security`: serves $HOME/fake-keychain/<service> for account
# $FAKE_KEYCHAIN_ACCOUNT (default mcc-test), else "item not found" (exit 44)
# like the real one.
echo "$*" >> "$HOME/security.log"
svc=""; acct=""
while [ $# -gt 0 ]; do
  case "$1" in -s) svc="$2"; shift ;; -a) acct="$2"; shift ;; esac
  shift
done
f="$HOME/fake-keychain/$svc"
if [ "$acct" = "${FAKE_KEYCHAIN_ACCOUNT:-mcc-test}" ] && [ -f "$f" ]; then cat "$f"; exit 0; fi
echo "security: The specified item could not be found in the keychain." >&2
exit 44
"#;

/// Fake `ssh` (#130). It checks the arguments mcc gives it (options first:
/// `-o BatchMode=yes -o ConnectTimeout=N`, then `--` right before the host,
/// then the remote command as one word) and exits 98 when they differ.
/// Otherwise it runs the remote command with `sh -c`, as sshd has the
/// user's shell do, in the sandbox `$HOME/ssh-hosts/<host>` names: HOME and
/// MCC_HOME there, its fakes, the mcc under test on PATH ($HOME/ssh-bin),
/// and this run's usage endpoints. Each run appends its arguments to
/// $HOME/ssh.log, one line, tab-separated. Hosts `down` (unreachable: exit
/// 255), `old` (an mcc without the machine protocol: clap's usage error,
/// exit 2), `garbage` (an answer that isn't JSON) and `canned` (whatever
/// $HOME/ssh-canned holds) need no sandbox; any other host without one
/// can't be resolved (exit 255).
const FAKE_SSH: &str = r#"#!/bin/sh
tab=$(printf '\t'); line=ssh
for a in "$@"; do line="$line$tab$a"; done
printf '%s\n' "$line" >> "$HOME/ssh.log"
if [ $# -ne 7 ] || [ "$1" != "-o" ] || [ "$2" != "BatchMode=yes" ] || [ "$3" != "-o" ] || [ "$5" != "--" ]; then
  echo "fake ssh: unexpected arguments: $*" >&2; exit 98
fi
case "$4" in ConnectTimeout=[1-9]*) ;; *) echo "fake ssh: no ConnectTimeout: $4" >&2; exit 98 ;; esac
host="$6"
case "$host" in
  down) echo "ssh: connect to host down port 22: Connection refused" >&2; exit 255 ;;
  old) echo "error: unexpected argument '--machine-protocol' found" >&2; exit 2 ;;
  garbage) echo "Welcome to garbage!"; exit 0 ;;
  canned) cat "$HOME/ssh-canned"; exit 0 ;;
esac
[ -f "$HOME/ssh-hosts/$host" ] || { echo "ssh: Could not resolve hostname $host" >&2; exit 255; }
root=$(cat "$HOME/ssh-hosts/$host")
exec env -i HOME="$root" MCC_HOME="$root" PATH="$root/fakebin:$root/.local/bin:$HOME/ssh-bin:/usr/bin:/bin" \
  SHELL=/bin/zsh TERM_PROGRAM=TestTerm USER=mcc-test NO_COLOR=1 MCC_NO_SHELL_PROBE=1 MCC_NON_INTERACTIVE=1 \
  MCC_CLAUDE_BIN="$root/fakebin/claude" MCC_SECURITY_BIN="$root/fakebin/security" MCC_SSH_BIN="$root/fakebin/ssh" \
  MCC_USAGE_URL="$MCC_USAGE_URL" MCC_DEVIN_API_URL="$MCC_DEVIN_API_URL" \
  sh -c "$7"
"#;

/// Nothing listens on the discard port: connections are refused at once.
pub const REFUSED_URL: &str = "http://127.0.0.1:9/api/oauth/usage";

/// A fake `devin` CLI: records its arguments (one per line) in
/// $HOME/devin-args and the variables mcc could have changed in
/// $HOME/devin-env (`NAME=value`, or `NAME unset`), then exits with
/// $FAKE_DEVIN_EXIT (0 by default).
const FAKE_DEVIN: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/devin-args"
: > "$HOME/devin-env"
for v in HOME XDG_CONFIG_HOME XDG_DATA_HOME CLAUDE_CONFIG_DIR MCC_PROFILE DEVIN_CONFIG; do
  if eval "[ -n \"\${$v+x}\" ]"; then eval "echo \"$v=\${$v}\"" >> "$HOME/devin-env"; else echo "$v unset" >> "$HOME/devin-env"; fi
done
echo "fake devin $*"
exit "${FAKE_DEVIN_EXIT:-0}"
"#;

/// Where the Devin CLI keeps its login in the sandbox (no XDG_DATA_HOME).
pub const DEVIN_CREDENTIALS: &str = ".local/share/devin/credentials.toml";

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
        let ssh = bin.join("ssh");
        std::fs::write(&ssh, FAKE_SSH).unwrap();
        chmod_x(&ssh);
        // The mcc another machine runs: the one under test.
        let remote_bin = tmp.path().join("ssh-bin");
        std::fs::create_dir_all(&remote_bin).unwrap();
        std::os::unix::fs::symlink(mcc_bin(), remote_bin.join("mcc")).unwrap();
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
            ("MCC_SSH_BIN", home.join("fakebin/ssh").into()),
            ("MCC_USAGE_URL", REFUSED_URL.into()),
            ("MCC_DEVIN_API_URL", REFUSED_URL.into()),
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

    /// `program` run with the sandboxed environment, in the sandbox HOME.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
        let mut cmd = std::process::Command::new(program);
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
    /// Set up `[machines.<name>]` (#130) with `host`, which the fake ssh
    /// serves from `remote`'s sandbox (`None`: one of its built-in hosts,
    /// or an unknown one), and `mcc` when given.
    pub fn add_machine(&self, name: &str, host: &str, remote: Option<&Sandbox>, mcc: Option<&str>) {
        if let Some(r) = remote {
            self.write(
                &format!("ssh-hosts/{host}"),
                &r.home().display().to_string(),
            );
        }
        let quoted = |v: &str| toml::Value::String(v.into()).to_string();
        let mut table = format!("\n[machines.{name}]\nhost = {}\n", quoted(host));
        if let Some(m) = mcc {
            table.push_str(&format!("mcc = {}\n", quoted(m)));
        }
        let cfg = if self.exists(".mcc/config.toml") {
            self.read(".mcc/config.toml")
        } else {
            String::new()
        };
        self.write(".mcc/config.toml", &format!("{cfg}{table}"));
    }

    /// The fake ssh's runs: one line each, its arguments tab-separated
    /// after `ssh`. Empty when ssh never ran.
    pub fn ssh_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("ssh.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Sign the Devin CLI in: its credentials file, as `devin auth login`
    /// writes it, holding `token` (a plainly fake value) under the legacy
    /// `windsurf_api_key` key, with the sibling keys custats ignores.
    pub fn seed_devin_login(&self, token: &str) {
        let body = [
            format!("windsurf_api_key = \"{token}\""),
            "api_server_url = \"https://server.example.invalid\"".to_string(),
            "devin_webapp_host = \"app.example.invalid\"".to_string(),
            String::new(),
        ]
        .join("\n");
        self.write(DEVIN_CREDENTIALS, &body);
    }

    /// Put a fake `devin` on PATH (`fakebin/devin`, see [`FAKE_DEVIN`]).
    pub fn install_fake_devin(&self) -> PathBuf {
        let p = self.path("fakebin/devin");
        std::fs::write(&p, FAKE_DEVIN).unwrap();
        chmod_x(&p);
        p
    }

    /// Sign the config dir `rel_dir` in to `email` for the fake `claude`
    /// (`auth status` reports it), with the state file `state_rel` saying so
    /// too (`oauthAccount`), keeping the rest of a state file already there.
    pub fn sign_in(&self, rel_dir: &str, state_rel: &str, email: &str) {
        self.write(
            &format!("{rel_dir}/.credentials.json"),
            r#"{"token":"fake"}"#,
        );
        self.write(&format!("{rel_dir}/.fake-account"), &format!("{email}\n"));
        let mut state = if self.exists(state_rel) {
            json(self.read(state_rel).as_bytes())
        } else {
            serde_json::json!({})
        };
        state["oauthAccount"] = serde_json::json!({ "emailAddress": email });
        self.write(state_rel, &serde_json::to_string(&state).unwrap());
    }

    /// Claude Code OAuth credentials (`claudeAiOauth`) in `<rel_dir>/.credentials.json`.
    pub fn seed_oauth(&self, rel_dir: &str, token: &str, expires_at_ms: i64) {
        self.write(
            &format!("{rel_dir}/.credentials.json"),
            &oauth_blob(token, expires_at_ms),
        );
    }

    /// [`Self::seed_oauth`] with a login deadline (`refreshTokenExpiresAt`).
    pub fn seed_login(&self, rel_dir: &str, token: &str, expires_at_ms: i64, deadline_ms: i64) {
        self.write(
            &format!("{rel_dir}/.credentials.json"),
            &oauth_blob_with_deadline(token, expires_at_ms, Some(deadline_ms)),
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
    oauth_blob_with_deadline(token, expires_at_ms, None)
}

/// Claude Code OAuth credentials, with the login deadline
/// (`refreshTokenExpiresAt`) when given.
pub fn oauth_blob_with_deadline(
    token: &str,
    expires_at_ms: i64,
    deadline_ms: Option<i64>,
) -> String {
    let deadline = deadline_ms
        .map(|ms| format!(r#","refreshTokenExpiresAt":{ms}"#))
        .unwrap_or_default();
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"refresh-{token}","expiresAt":{expires_at_ms}{deadline},"scopes":["user:inference","user:profile"],"subscriptionType":"max"}}}}"#
    )
}

/// The mcc binary under test.
pub fn mcc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mcc"))
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
    /// Devin's organization header (`x-cog-org-id`).
    pub org: Option<String>,
    pub user_agent: Option<String>,
}

impl StubRequest {
    /// The request's path: "/api/organizations".
    pub fn path(&self) -> &str {
        self.request_line.split(' ').nth(1).unwrap_or_default()
    }
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
                    org: header("x-cog-org-id"),
                    user_agent: header("user-agent"),
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

    /// The stub as Devin's API base (`MCC_DEVIN_API_URL`): `http://…/api`.
    pub fn devin_base(&self) -> String {
        self.url.trim_end_matches("/oauth/usage").to_string()
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
pub const DAY_MS: i64 = 24 * HOUR_MS;

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

/// Turn on the live usage lookup (`[usage] live = true`), its one-time
/// disclosure already acknowledged, in the sandbox config (a new one when
/// there is none yet).
pub fn enable_live_usage(sb: &Sandbox) {
    const LIVE: &str = "[usage]\nlive = true\nlive_disclosed_at = \"2026-10-01T00:00:00Z\"\n";
    if !sb.exists(".mcc/config.toml") {
        sb.write(".mcc/config.toml", LIVE);
        return;
    }
    let cfg = sb.read(".mcc/config.toml");
    assert!(cfg.contains("[usage]\n"), "{cfg}");
    sb.write(".mcc/config.toml", &cfg.replacen("[usage]\n", LIVE, 1));
}

/// What `mcc statusline` caches for `profile`: (percent used, seconds until
/// the reset) per window, `updated_secs_ago` seconds ago.
pub fn seed_usage_cache(
    sb: &Sandbox,
    profile: &str,
    five_hour: Option<(f64, i64)>,
    seven_day: Option<(f64, i64)>,
    updated_secs_ago: i64,
) {
    let now = chrono::Utc::now();
    let window = |w: Option<(f64, i64)>| match w {
        Some((pct, secs)) => serde_json::json!({
            "used_percentage": pct,
            "resets_at": now.timestamp() + secs,
        }),
        None => serde_json::Value::Null,
    };
    let record = serde_json::json!({
        "profile": profile,
        "updated_at": (now - chrono::Duration::seconds(updated_secs_ago))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "five_hour": window(five_hour),
        "seven_day": window(seven_day),
    });
    sb.write(
        &format!(".mcc/state/{profile}/usage.json"),
        &serde_json::to_string_pretty(&record).unwrap(),
    );
}

/// default + `work` logged in (each with its own token), `lab` logged out,
/// with the live usage lookup on.
pub fn usage_sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    with_profile(&sb, "work", "--clone");
    with_profile(&sb, "lab", "--fresh");
    sb.seed_oauth(".claude", "test-token-default", now_ms() + HOUR_MS);
    sb.seed_oauth(".mcc/profiles/work", "test-token-work", now_ms() + HOUR_MS);
    enable_live_usage(&sb);
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

// ---- Devin (#124) -----------------------------------------------------------

/// The fake session token the Devin fixtures use.
pub const DEVIN_TOKEN: &str = "test-devin-token";

/// Devin's API: `/organizations` lists a secondary and a primary
/// organization (`org_main`, served as `org-main`), whose quota has a daily
/// window of 12% (resets in 8 hours), a weekly one of 54% (in 3 days 4
/// hours) and $3.50 of overage balance — for [`DEVIN_TOKEN`] only; any
/// other token gets a 401.
pub fn devin_stub() -> UsageStub {
    devin_stub_except(|_| None)
}

/// [`devin_stub`], except for the requests `answer` answers itself.
pub fn devin_stub_except(
    answer: impl Fn(&StubRequest) -> Option<(u16, String)> + Send + 'static,
) -> UsageStub {
    let (day, week) = (rfc3339_in(8 * 3600), rfc3339_in(3 * 86400 + 4 * 3600));
    UsageStub::start(move |req| {
        if let Some(a) = answer(req) {
            return a;
        }
        if req.authorization.as_deref() != Some(&format!("Bearer {DEVIN_TOKEN}")) {
            return (401, r#"{"detail":"Unauthorized"}"#.into());
        }
        match req.path() {
            "/api/organizations" => (
                200,
                r#"[{"org_id":"org_side","name":"Side"},{"org_id":"org_main","display_name":"Main team","is_primary_org":true}]"#.into(),
            ),
            "/api/org-main/billing/quota/usage" => (
                200,
                format!(
                    r#"{{"is_quota_plan":true,"has_quota_allocation":true,"daily_percentage":12,"weekly_percentage":"54","daily_reset_at":"{day}","weekly_reset_at":"{week}","overage_balance":3.5,"hide_daily_quota":false}}"#
                ),
            ),
            _ => (404, r#"{"detail":"Not Found"}"#.into()),
        }
    })
}

/// A table line without its row number: "work  …" for "2 work  …" (and
/// " 2 work  …" in a table of ten or more). Other lines come back as they are.
pub fn unnumbered(line: &str) -> &str {
    let t = line.trim_start();
    match t.split_once(' ') {
        Some((n, rest)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => rest,
        _ => line,
    }
}
