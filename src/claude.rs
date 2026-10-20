//! Thin wrapper around the `claude` binary (version, auth login/status/logout).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::error::CliError;
use crate::paths::Paths;

/// True while an interactive child (`claude auth login`) owns the terminal;
/// the Ctrl+C handler lets the child handle the signal in that window.
pub static CHILD_RUNNING: AtomicBool = AtomicBool::new(false);

/// Set by the Ctrl+C handler when Ctrl+C arrives while `CHILD_RUNNING`, so a
/// loop over children (`mcc exec --all`) stops even when the child handled
/// the signal and exited normally.
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Locate the claude binary: `MCC_CLAUDE_BIN`, then a remembered path, then `$PATH`.
pub fn find_binary(remembered: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MCC_CLAUDE_BIN").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = remembered.filter(|p| crate::fsutil::is_executable(p)) {
        return Some(p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("claude"))
        .find(|p| crate::fsutil::is_executable(p))
}

pub fn require_binary(remembered: Option<&Path>) -> Result<PathBuf> {
    find_binary(remembered).ok_or_else(|| {
        CliError::runtime("the `claude` command was not found on your PATH")
            .hint("install Claude Code (https://docs.claude.com/en/docs/claude-code) or set MCC_CLAUDE_BIN=/path/to/claude")
            .into()
    })
}

pub fn version(bin: &Path) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// A `claude` invocation bound to one profile. `config_dir: None` = default profile.
pub struct Claude<'a> {
    pub bin: &'a Path,
    pub paths: &'a Paths,
    pub config_dir: Option<&'a Path>,
}

#[derive(Debug, Clone, Default)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub method: Option<String>,
    pub email: Option<String>,
    /// The account's organization, when this Claude Code reports it.
    pub org_name: Option<String>,
    pub org_id: Option<String>,
}

impl Claude<'_> {
    fn command(&self) -> Command {
        let mut cmd = Command::new(self.bin);
        // Keep the child consistent with mcc's view of HOME (MCC_HOME sandboxes).
        cmd.env("HOME", &self.paths.home);
        match self.config_dir {
            Some(dir) => {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            None => {
                cmd.env_remove("CLAUDE_CONFIG_DIR");
            }
        }
        cmd
    }

    /// Run `claude auth login` attached to the terminal.
    pub fn login(&self) -> Result<bool> {
        let mut cmd = self.command();
        cmd.args(["auth", "login"]);
        CHILD_RUNNING.store(true, Ordering::SeqCst);
        let status = cmd
            .status()
            .with_context(|| format!("running {} auth login", self.bin.display()));
        CHILD_RUNNING.store(false, Ordering::SeqCst);
        Ok(status?.success())
    }

    pub fn logout(&self) -> Result<bool> {
        let out = self
            .command()
            .args(["auth", "logout"])
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {} auth logout", self.bin.display()))?;
        Ok(out.status.success())
    }

    pub fn status(&self) -> Result<AuthStatus> {
        let out = self
            .command()
            .args(["auth", "status", "--json"])
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {} auth status", self.bin.display()))?;
        let raw: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        Ok(parse_status(raw))
    }
}

fn parse_status(raw: Value) -> AuthStatus {
    let str_at = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| raw.pointer(k).and_then(Value::as_str).map(str::to_string))
    };
    AuthStatus {
        logged_in: raw
            .get("loggedIn")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        method: str_at(&["/authMethod"]).filter(|m| m != "none"),
        email: str_at(&[
            "/email",
            "/emailAddress",
            "/account/email",
            "/account/emailAddress",
        ]),
        org_name: str_at(&["/orgName", "/organizationName", "/account/organizationName"]),
        org_id: str_at(&["/orgId", "/organizationUuid", "/account/organizationUuid"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing() {
        let s = parse_status(serde_json::json!({"loggedIn": false, "authMethod": "none"}));
        assert!(!s.logged_in);
        assert_eq!(s.method, None);
        let s = parse_status(
            serde_json::json!({"loggedIn": true, "authMethod": "claude.ai", "email": "a@b.c"}),
        );
        assert!(s.logged_in);
        assert_eq!(s.email.as_deref(), Some("a@b.c"));
        assert_eq!(s.org_name, None);
        let s = parse_status(serde_json::json!({
            "loggedIn": true, "email": "a@b.c", "orgId": "org-1", "orgName": "Acme"
        }));
        assert_eq!(s.org_name.as_deref(), Some("Acme"));
        assert_eq!(s.org_id.as_deref(), Some("org-1"));
        assert!(!parse_status(Value::Null).logged_in);
    }
}
