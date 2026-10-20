//! Shared plugins: a profile that uses the default profile's plugins root
//! instead of a copy of its own (`mcc link <alias> plugins`, and the default
//! for `mcc add --clone`).
//!
//! Claude Code reads every plugin file from one *plugins root*, by default
//! `<CLAUDE_CONFIG_DIR>/plugins`. `CLAUDE_CODE_PLUGIN_CACHE_DIR` replaces that
//! root (despite its name, it is not only the cache): checked against Claude
//! Code 2.1.286 and https://code.claude.com/docs/en/env-vars, the root holds
//! `installed_plugins.json`, `known_marketplaces.json`, `marketplaces/`,
//! `cache/`, `data/` (each plugin's `${CLAUDE_PLUGIN_DATA}`), `store/` and the
//! per-account `synced/` plugins. Setting it to the default's
//! `~/.claude/plugins` shares all of that, so nothing is duplicated.
//!
//! What stays per profile: `enabledPlugins` (and the rest of the settings),
//! which live in the profile's `settings.json` and its overlay. What is
//! shared: which plugins and marketplaces are installed, their files and their
//! data. Installing, updating or uninstalling a plugin in one profile does it
//! for the default and every other sharing profile (write-through).
//!
//! The variable only reaches Claude Code when mcc starts it: the profile's
//! launcher, `mcc run`, `mcc exec` and `mcc env` set it. A profile started
//! any other way (e.g. `CLAUDE_CONFIG_DIR=… claude`) falls back to its own,
//! empty, plugins root.
//!
//! Version caveat: this was verified on Claude Code 2.1.286; other versions
//! may treat `CLAUDE_CODE_PLUGIN_CACHE_DIR` differently. If a sharing profile
//! shows no plugins, `mcc link <alias> plugins --off` gives it its own copy
//! again (or clone with `--copy-plugins`).

use std::path::PathBuf;

use crate::config::Profile;
use crate::paths::Paths;

/// The sync item, and the `link` entry, for the plugin files.
pub const ITEM: &str = "plugins";

/// The Claude Code variable that replaces the plugins root.
pub const ENV: &str = "CLAUDE_CODE_PLUGIN_CACHE_DIR";

/// The plugins root, relative to a config dir.
pub const DIR: &str = "plugins";

/// The default profile's plugins root, which sharing profiles use.
pub fn shared_root(paths: &Paths) -> PathBuf {
    paths.default_dir.join(DIR)
}

/// Whether `link` (a profile's link list) shares the default's plugins.
pub fn in_link(link: &[String]) -> bool {
    link.iter().any(|l| l == ITEM)
}

/// Whether the registered profile shares the default's plugins.
pub fn shared(p: &Profile) -> bool {
    in_link(&p.link)
}

/// The plugins root to give Claude Code for the profile: the default's when
/// it shares plugins, else `None` (its own `<dir>/plugins`).
pub fn root_for(paths: &Paths, p: &Profile) -> Option<PathBuf> {
    shared(p).then(|| shared_root(paths))
}

/// Whether `value` (the inherited `CLAUDE_CODE_PLUGIN_CACHE_DIR`) is the
/// default's plugins root, as mcc sets it for sharing profiles. Such a value
/// is mcc's own (e.g. left by `eval "$(mcc env work)"`), so a profile with
/// its own plugins clears it; any other value is the user's and is kept.
pub fn is_mccs(paths: &Paths, value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| std::path::Path::new(v) == shared_root(paths))
}

/// Printed whenever a profile starts sharing the default's plugins.
pub fn shared_note(alias: &str) -> String {
    format!(
        "shared plugins: \"{alias}\" now uses the default profile's plugins ({ENV}). Which plugins are enabled stays per profile (enabledPlugins in settings.json), but installing, updating or uninstalling a plugin or marketplace in \"{alias}\" does it for the default and every profile sharing plugins. Start \"{alias}\" with its command, `mcc run` or `mcc exec`, which set the variable; `mcc link {alias} plugins --off` gives it its own copy again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_root_is_the_defaults_plugins_dir() {
        let paths = Paths::new(PathBuf::from("/home/me"));
        assert_eq!(
            shared_root(&paths),
            PathBuf::from("/home/me/.claude/plugins")
        );
        assert!(in_link(&["skills".into(), "plugins".into()]));
        assert!(!in_link(&["skills".into()]));
        let ours = shared_root(&paths);
        assert!(is_mccs(&paths, Some(ours.as_os_str())));
        assert!(!is_mccs(&paths, Some(std::ffi::OsStr::new("/opt/seed"))));
        assert!(!is_mccs(&paths, None));
    }
}
