//! Detect OS, terminal, shell, rc file, and the `claude` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::paths::Paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    pub fn name(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::Fish => "fish",
        }
    }

    pub fn from_path(shell: &str) -> Option<Shell> {
        let base = Path::new(shell).file_name()?.to_string_lossy().to_string();
        match base.trim_start_matches('-') {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }

    /// The rc file that interactive shells of this kind read on this OS.
    pub fn rc_file(self, paths: &Paths, os: &str) -> PathBuf {
        match self {
            Shell::Zsh => {
                let zdot = std::env::var_os("ZDOTDIR")
                    .filter(|_| std::env::var_os("MCC_HOME").is_none())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| paths.home.clone());
                zdot.join(".zshrc")
            }
            // macOS terminals start login shells, which read .bash_profile.
            Shell::Bash if os == "macos" => paths.home.join(".bash_profile"),
            Shell::Bash => paths.home.join(".bashrc"),
            Shell::Fish => paths.home.join(".config").join("fish").join("config.fish"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvInfo {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub terminal: String,
    pub shell: Option<Shell>,
    pub shell_path: String,
    pub rc_file: Option<PathBuf>,
    pub claude_bin: Option<PathBuf>,
    pub claude_version: Option<String>,
    pub local_bin_on_path: bool,
    pub detected_at: String,
}

/// Run detection. `shell_override` wins over `$SHELL`.
pub fn detect(paths: &Paths, shell_override: Option<Shell>) -> EnvInfo {
    let os = std::env::consts::OS.to_string();
    let shell_path = std::env::var("SHELL").unwrap_or_default();
    let shell = shell_override.or_else(|| Shell::from_path(&shell_path));
    let rc_file = shell.map(|s| s.rc_file(paths, &os));
    let claude_bin = crate::claude::find_binary(None);
    let claude_version = claude_bin.as_deref().and_then(crate::claude::version);
    EnvInfo {
        os_version: os_version(&os),
        arch: std::env::consts::ARCH.to_string(),
        terminal: terminal(),
        shell,
        shell_path,
        rc_file,
        claude_bin,
        claude_version,
        local_bin_on_path: dir_on_path(&paths.local_bin),
        detected_at: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        os,
    }
}

fn os_version(os: &str) -> String {
    match os {
        "macos" => cmd_out("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}")),
        "linux" => std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            }),
        _ => None,
    }
    .or_else(|| cmd_out("uname", &["-sr"]))
    .unwrap_or_else(|| "unknown".into())
}

fn terminal() -> String {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut name = get("TERM_PROGRAM")
        .map(|p| match get("TERM_PROGRAM_VERSION") {
            Some(v) => format!("{p} {v}"),
            None => p,
        })
        .or_else(|| get("TERMINAL_EMULATOR"))
        .or_else(|| get("WT_SESSION").map(|_| "Windows Terminal".into()))
        .or_else(|| get("TERM"))
        .unwrap_or_else(|| "unknown".into());
    if get("TMUX").is_some() {
        name.push_str(" (in tmux)");
    }
    if get("SSH_CONNECTION").is_some() {
        name.push_str(" (over ssh)");
    }
    name
}

fn cmd_out(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn dir_on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_from_path() {
        assert_eq!(Shell::from_path("/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(
            Shell::from_path("/opt/homebrew/bin/bash"),
            Some(Shell::Bash)
        );
        assert_eq!(Shell::from_path("-zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_path("/usr/bin/fish"), Some(Shell::Fish));
        assert_eq!(Shell::from_path("/bin/tcsh"), None);
        assert_eq!(Shell::from_path(""), None);
    }

    #[test]
    fn rc_files_per_os() {
        let p = Paths::new(PathBuf::from("/h"));
        assert_eq!(
            Shell::Bash.rc_file(&p, "macos"),
            PathBuf::from("/h/.bash_profile")
        );
        assert_eq!(
            Shell::Bash.rc_file(&p, "linux"),
            PathBuf::from("/h/.bashrc")
        );
        assert_eq!(
            Shell::Fish.rc_file(&p, "linux"),
            PathBuf::from("/h/.config/fish/config.fish")
        );
    }
}
