//! `~/.mcc/config.toml`: detected environment, profile registry, sync settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::env_detect::EnvInfo;
use crate::error::CliError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub env: Option<EnvInfo>,
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Built-in sync groups to skip (e.g. `["plugins"]`).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Extra paths (relative to the config dir) to copy verbatim.
    #[serde(default)]
    pub extra: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageConfig {
    /// Show each profile's account email in `mcc usage` (hidden by default;
    /// `--show-email` / `--hide-email` override it for one run).
    #[serde(default)]
    pub show_email: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub dir: PathBuf,
    /// "clone" or "fresh".
    pub mode: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rc_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).map_err(|e| {
            CliError::input(format!(
                "{} is not valid mcc config: {}",
                path.display(),
                e.message()
            ))
            .hint("fix the file by hand, or move it away and run `mcc init --force`".to_string())
            .into()
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let body = toml::to_string_pretty(self).context("serializing config")?;
        let text =
            format!("# mcc configuration — managed by `mcc`; safe to edit by hand.\n\n{body}");
        crate::fsutil::write_atomic(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.sync.exclude = vec!["plugins".into()];
        cfg.usage.show_email = true;
        cfg.profiles.insert(
            "work".into(),
            Profile {
                dir: "/h/.mcc/profiles/work".into(),
                mode: "clone".into(),
                created_at: "2026-10-10T00:00:00+02:00".into(),
                wrapper: Some("/h/.local/bin/work".into()),
                rc_file: Some("/h/.zshrc".into()),
                last_sync: None,
            },
        );
        cfg.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back.profiles, cfg.profiles);
        assert_eq!(back.sync.exclude, vec!["plugins".to_string()]);
        assert!(back.usage.show_email);
    }

    #[test]
    fn email_hidden_unless_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[sync]\nexclude = []\n").unwrap();
        assert!(!Config::load(&path).unwrap().usage.show_email);
        std::fs::write(&path, "[usage]\nshow_email = true\n").unwrap();
        assert!(Config::load(&path).unwrap().usage.show_email);
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&dir.path().join("nope.toml")).unwrap();
        assert!(cfg.profiles.is_empty());
        assert!(cfg.env.is_none());
    }

    #[test]
    fn invalid_file_is_input_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "profiles = 3 = 4").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert_eq!(crate::error::classify(&err).0, crate::error::EXIT_INPUT);
    }
}
