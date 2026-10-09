//! Test sandbox: a temp HOME, a fake `claude` binary, and an `mcc` command
//! wired to them. Nothing here touches the real ~/.claude, rc files, or keychain.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

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

    /// `mcc` with a sandboxed environment (non-interactive: no TTY in tests).
    pub fn mcc(&self) -> Command {
        let mut cmd = Command::cargo_bin("mcc").unwrap();
        let home = self.home();
        let path = format!("{}:{}:/usr/bin:/bin", home.join("fakebin").display(), home.join(".local/bin").display());
        cmd.env_clear()
            .env("HOME", home)
            .env("MCC_HOME", home)
            .env("PATH", path)
            .env("SHELL", "/bin/zsh")
            .env("TERM_PROGRAM", "TestTerm")
            .env("MCC_CLAUDE_BIN", home.join("fakebin/claude"))
            .env("MCC_NO_SHELL_PROBE", "1")
            .env("MCC_NON_INTERACTIVE", "1")
            .env("NO_COLOR", "1")
            .current_dir(home);
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
            r#"{"oauthAccount":{"emailAddress":"me@example.com","organizationName":"Acme"},"userID":"user-123","mcpServers":{"fs":{"command":"npx","args":["fs"]}},"projects":{}}"#,
        );
        self.write(".zshrc", "export EDITOR=vim\nalias cus_luong='CLAUDE_CONFIG_DIR=~/.cus_luong claude'\n");
    }
}

#[cfg(unix)]
pub fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| panic!("invalid JSON ({e}): {}", String::from_utf8_lossy(bytes)))
}
