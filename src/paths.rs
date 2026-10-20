//! Every filesystem location mcc touches, derived from one home directory.
//!
//! `MCC_HOME` overrides `HOME`, which keeps tests and demos fully sandboxed.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::error::CliError;

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    /// Default Claude Code config dir (`~/.claude`).
    pub default_dir: PathBuf,
    /// Default Claude Code state file (`~/.claude.json`, outside the dir).
    pub default_json: PathBuf,
    pub profiles_dir: PathBuf,
    pub config_file: PathBuf,
    /// Advisory lock held by every command that writes `config_file`.
    pub lock_file: PathBuf,
    pub log_file: PathBuf,
    pub backups_dir: PathBuf,
    /// Data shared between profiles that opted in (`mcc share`).
    pub shared_dir: PathBuf,
    pub local_bin: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("MCC_HOME")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
            .map(PathBuf::from)
            .ok_or_else(|| {
                CliError::input("cannot determine your home directory")
                    .hint("set HOME (or MCC_HOME) and try again")
            })?;
        Ok(Self::new(home))
    }

    pub fn new(home: PathBuf) -> Self {
        let mcc_dir = home.join(".mcc");
        Self {
            default_dir: home.join(".claude"),
            default_json: home.join(".claude.json"),
            profiles_dir: mcc_dir.join("profiles"),
            config_file: mcc_dir.join("config.toml"),
            lock_file: mcc_dir.join(".lock"),
            log_file: mcc_dir.join("logs").join("mcc.log"),
            backups_dir: mcc_dir.join("backups"),
            shared_dir: mcc_dir.join("shared"),
            local_bin: home.join(".local").join("bin"),
            home,
        }
    }

    pub fn profile_dir(&self, alias: &str) -> PathBuf {
        self.profiles_dir.join(alias)
    }

    /// The shared session store: every sharing profile's `projects/` links here.
    pub fn shared_projects_dir(&self) -> PathBuf {
        self.shared_dir.join("projects")
    }

    pub fn wrapper(&self, alias: &str) -> PathBuf {
        self.local_bin.join(alias)
    }

    /// Render a path with `~` for display.
    pub fn display(&self, p: &Path) -> String {
        match p.strip_prefix(&self.home) {
            Ok(rel) if rel.as_os_str().is_empty() => "~".to_string(),
            Ok(rel) => format!("~/{}", rel.display()),
            Err(_) => p.display().to_string(),
        }
    }

    /// Render a path relative to `$HOME` for use inside shell scripts.
    pub fn shell_expr(&self, p: &Path) -> String {
        match p.strip_prefix(&self.home) {
            Ok(rel) => format!("$HOME/{}", rel.display()),
            Err(_) => p.display().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let p = Paths::new(PathBuf::from("/h"));
        assert_eq!(p.default_dir, PathBuf::from("/h/.claude"));
        assert_eq!(p.default_json, PathBuf::from("/h/.claude.json"));
        assert_eq!(
            p.profile_dir("work"),
            PathBuf::from("/h/.mcc/profiles/work")
        );
        assert_eq!(p.wrapper("work"), PathBuf::from("/h/.local/bin/work"));
        assert_eq!(p.config_file, PathBuf::from("/h/.mcc/config.toml"));
        assert_eq!(p.log_file, PathBuf::from("/h/.mcc/logs/mcc.log"));
        assert_eq!(p.shared_dir, PathBuf::from("/h/.mcc/shared"));
        assert_eq!(
            p.shared_projects_dir(),
            PathBuf::from("/h/.mcc/shared/projects")
        );
    }

    #[test]
    fn display_and_shell_expr() {
        let p = Paths::new(PathBuf::from("/h"));
        assert_eq!(
            p.display(&PathBuf::from("/h/.mcc/profiles/x")),
            "~/.mcc/profiles/x"
        );
        assert_eq!(p.display(&PathBuf::from("/h")), "~");
        assert_eq!(p.display(&PathBuf::from("/etc/x")), "/etc/x");
        assert_eq!(
            p.shell_expr(&PathBuf::from("/h/.mcc/profiles/x")),
            "$HOME/.mcc/profiles/x"
        );
    }
}
