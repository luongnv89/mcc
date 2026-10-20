//! `~/.mcc/config.toml`: detected environment, profile registry, sync settings,
//! and the profile each project starts with (`mcc project`, `mcc run`).

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
    /// Project directory (absolute, symlinks resolved) → the profile `mcc run`
    /// starts there first. Left out of the file while empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, Project>,
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
    /// Share this profile's sessions (`projects/`) with the other sharing
    /// profiles. Off by default and left out of the file while off. Set it
    /// with `mcc share <alias>` (which also links the directory and prints
    /// the terms-of-service note); editing the key by hand does nothing alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub share_sessions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// An mcc profile's name, or `default` for ~/.claude.
    pub profile: String,
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

    /// Point the project defaults naming `old` at `new` (`mcc rename`).
    /// Returns how many changed.
    pub fn rename_project_defaults(&mut self, old: &str, new: &str) -> usize {
        let mut n = 0;
        for p in self.projects.values_mut().filter(|p| p.profile == old) {
            p.profile = new.to_string();
            n += 1;
        }
        n
    }

    /// Remove the project defaults naming `alias` (`mcc delete`). Returns
    /// their project directories.
    pub fn remove_project_defaults(&mut self, alias: &str) -> Vec<String> {
        let gone: Vec<String> = self
            .projects
            .iter()
            .filter(|(_, p)| p.profile == alias)
            .map(|(dir, _)| dir.clone())
            .collect();
        for dir in &gone {
            self.projects.remove(dir);
        }
        gone
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
                share_sessions: true,
            },
        );
        cfg.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back.profiles, cfg.profiles);
        assert_eq!(back.sync.exclude, vec!["plugins".to_string()]);
        assert!(back.usage.show_email);
    }

    /// A config written before session sharing existed: loads with sharing
    /// off, and saving it again writes the same bytes (no new key).
    #[test]
    fn sharing_off_by_default_and_legacy_file_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let legacy = "# mcc configuration — managed by `mcc`; safe to edit by hand.\n\n\
            [sync]\nexclude = []\nextra = []\n\n\
            [usage]\nshow_email = false\n\n\
            [profiles.work]\ndir = \"/h/.mcc/profiles/work\"\nmode = \"clone\"\n\
            created_at = \"2026-10-10T00:00:00+02:00\"\nwrapper = \"/h/.local/bin/work\"\n\
            rc_file = \"/h/.zshrc\"\n";
        std::fs::write(&path, legacy).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert!(!cfg.profiles["work"].share_sessions);
        cfg.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);

        let mut cfg = cfg;
        cfg.profiles.get_mut("work").unwrap().share_sessions = true;
        cfg.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("share_sessions = true"), "{text}");
        assert!(Config::load(&path).unwrap().profiles["work"].share_sessions);
    }

    #[test]
    fn project_defaults_round_trip_and_follow_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        for (dir, profile) in [
            ("/src/a", "work"),
            ("/src/b", "default"),
            ("/src/c", "work"),
        ] {
            cfg.projects.insert(
                dir.into(),
                Project {
                    profile: profile.into(),
                },
            );
        }
        cfg.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[projects.\"/src/a\"]\nprofile = \"work\"\n"),
            "{text}"
        );
        let mut back = Config::load(&path).unwrap();
        assert_eq!(back.projects, cfg.projects);

        assert_eq!(back.rename_project_defaults("work", "job"), 2);
        assert_eq!(back.projects["/src/a"].profile, "job");
        assert_eq!(back.projects["/src/b"].profile, "default");
        assert_eq!(back.remove_project_defaults("job"), ["/src/a", "/src/c"]);
        assert_eq!(back.projects.keys().collect::<Vec<_>>(), ["/src/b"]);
        assert!(back.remove_project_defaults("nope").is_empty());
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
