//! Every filesystem location mcc touches, derived from one home directory.
//!
//! `MCC_HOME` overrides `HOME`, which keeps tests and demos fully sandboxed.

use std::path::{Component, Path, PathBuf};

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
    /// Per-profile state written by mcc itself (`state/<profile>/`), such as
    /// the rate limits the statusline shim caches.
    pub state_dir: PathBuf,
    /// What mcc keeps about the providers beside the profiles
    /// (`providers/<provider>/`, e.g. Devin's last quota). Kept out of
    /// `state_dir`, whose entries are named after profiles, so no profile
    /// (not even one named like a provider) shares, renames or deletes it.
    pub providers_dir: PathBuf,
    pub local_bin: PathBuf,
    /// The Devin CLI's login: `$XDG_DATA_HOME/devin/credentials.toml`, else
    /// `~/.local/share/devin/credentials.toml` (see [`devin_credentials`]).
    /// mcc only ever reads it.
    pub devin_credentials: PathBuf,
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
        let data_home = std::env::var_os("XDG_DATA_HOME");
        Ok(Self::new(home).with_data_home(data_home.as_deref()))
    }

    /// The same paths, with Devin's credentials under `data_home`
    /// (`XDG_DATA_HOME`) when it is set and not empty.
    pub fn with_data_home(mut self, data_home: Option<&std::ffi::OsStr>) -> Self {
        self.devin_credentials = devin_credentials(&self.home, data_home);
        self
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
            state_dir: mcc_dir.join("state"),
            providers_dir: mcc_dir.join("providers"),
            local_bin: home.join(".local").join("bin"),
            devin_credentials: devin_credentials(&home, None),
            home,
        }
    }

    pub fn profile_dir(&self, alias: &str) -> PathBuf {
        self.profiles_dir.join(alias)
    }

    /// The default group's session store: the profiles sharing without a
    /// group name link their `projects/` here. It is the one store every
    /// sharing profile used before groups existed.
    pub fn shared_projects_dir(&self) -> PathBuf {
        self.shared_dir.join("projects")
    }

    /// The session store of sharing group `group` (`None`: the default group).
    /// `None` when `group` is not a single plain directory name (empty,
    /// `.`, `..`, absolute, or with a separator), so the store is always
    /// inside `shared_dir/groups/`.
    pub fn share_store(&self, group: Option<&str>) -> Option<PathBuf> {
        match group {
            None => Some(self.shared_projects_dir()),
            Some(g) => {
                let mut parts = Path::new(g).components();
                match (parts.next(), parts.next()) {
                    (Some(Component::Normal(name)), None) if name == g => {
                        Some(self.shared_dir.join("groups").join(g).join("projects"))
                    }
                    _ => None,
                }
            }
        }
    }

    /// `~/.mcc/state/<profile>`; the default profile is `default`.
    pub fn profile_state_dir(&self, profile: &str) -> PathBuf {
        self.state_dir.join(profile)
    }

    /// What the launch-time catch-up last did for `profile` (`[sync] auto`):
    /// `~/.mcc/state/<profile>/auto-sync.json`.
    pub fn auto_sync_state(&self, profile: &str) -> PathBuf {
        self.profile_state_dir(profile).join("auto-sync.json")
    }

    /// What mcc keeps about Devin (`~/.mcc/providers/devin/`): its last
    /// quota lookup and organization, never its token.
    pub fn devin_dir(&self) -> PathBuf {
        self.providers_dir.join("devin")
    }

    /// Devin's last successful quota lookup:
    /// `~/.mcc/providers/devin/quota.json`.
    pub fn devin_quota_cache(&self) -> PathBuf {
        self.devin_dir().join("quota.json")
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

/// Where the Devin CLI keeps its login, as Devin (and custats) resolve it:
/// `<data_home>/devin/credentials.toml` when `data_home` (`XDG_DATA_HOME`)
/// is set and not empty, with a leading `~` expanded to `home`; else
/// `<home>/.local/share/devin/credentials.toml`.
pub fn devin_credentials(home: &Path, data_home: Option<&std::ffi::OsStr>) -> PathBuf {
    let base = match data_home.filter(|v| !v.is_empty()) {
        Some(v) => {
            let v = Path::new(v);
            match v.strip_prefix("~") {
                Ok(rest) => home.join(rest),
                Err(_) => v.to_path_buf(),
            }
        }
        None => home.join(".local").join("share"),
    };
    base.join("devin").join("credentials.toml")
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
            p.profile_state_dir("work"),
            PathBuf::from("/h/.mcc/state/work")
        );
        assert_eq!(
            p.auto_sync_state("work"),
            PathBuf::from("/h/.mcc/state/work/auto-sync.json")
        );
        assert_eq!(
            p.shared_projects_dir(),
            PathBuf::from("/h/.mcc/shared/projects")
        );
        assert_eq!(
            p.devin_credentials,
            PathBuf::from("/h/.local/share/devin/credentials.toml")
        );
        assert_eq!(
            p.devin_quota_cache(),
            PathBuf::from("/h/.mcc/providers/devin/quota.json")
        );
        // Outside the per-profile state, so a profile named `devin` (from
        // before the name was reserved) never shares, renames or deletes it.
        assert!(!p.devin_quota_cache().starts_with(&p.state_dir));
        assert!(!p.devin_quota_cache().starts_with(&p.profiles_dir));
        assert_eq!(p.share_store(None), Some(p.shared_projects_dir()));
        assert_eq!(
            p.share_store(Some("work")),
            Some(PathBuf::from("/h/.mcc/shared/groups/work/projects"))
        );
    }

    #[test]
    fn devin_credentials_follow_xdg_data_home() {
        let home = Path::new("/h");
        let at = |v: Option<&str>| devin_credentials(home, v.map(std::ffi::OsStr::new));
        let default = PathBuf::from("/h/.local/share/devin/credentials.toml");
        assert_eq!(at(None), default);
        assert_eq!(at(Some("")), default);
        assert_eq!(
            at(Some("/data")),
            PathBuf::from("/data/devin/credentials.toml")
        );
        assert_eq!(
            at(Some("~/xdg")),
            PathBuf::from("/h/xdg/devin/credentials.toml")
        );
        assert_eq!(at(Some("~")), PathBuf::from("/h/devin/credentials.toml"));
        let p = Paths::new(PathBuf::from("/h")).with_data_home(Some("/d".as_ref()));
        assert_eq!(
            p.devin_credentials,
            PathBuf::from("/d/devin/credentials.toml")
        );
    }

    #[test]
    fn share_store_stays_inside_the_groups_dir() {
        let p = Paths::new(PathBuf::from("/h"));
        for bad in [
            "", ".", "..", "../..", "a/b", "/", "/etc", "a/", "./a", "a/..",
        ] {
            assert_eq!(p.share_store(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(
            p.share_store(Some("a.b")),
            Some(PathBuf::from("/h/.mcc/shared/groups/a.b/projects"))
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
