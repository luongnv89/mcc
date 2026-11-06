//! `mcc env` — print the shell code that points `CLAUDE_CONFIG_DIR` at a
//! profile, for `eval "$(mcc env)"` (e.g. in a direnv `.envrc`).
//!
//! Without a profile it is the one `mcc run` would start here: the project's
//! default, else the global default (`mcc default`), else the default
//! profile (~/.claude), whose code unsets the variable and sets
//! `MCC_PROFILE=default` (any other profile clears it), so the `claude`
//! function of `mcc default` keeps that shell on ~/.claude. Usage is never
//! checked, and nothing is written or locked.
//!
//! A profile sharing the default's plugins also gets
//! `CLAUDE_CODE_PLUGIN_CACHE_DIR`; for any other profile, mcc's own value of
//! it (the default's plugins root) is unset, and a value of the user's is
//! left alone.

use std::path::Path;

use anyhow::Result;

use crate::cli::EnvArgs;
use crate::config::Config;
use crate::env_detect::Shell;
use crate::error::CliError;
use crate::paths::Paths;
use crate::{plugins, ui};

use super::run;

pub fn run(paths: &Paths, cfg: &Config, args: &EnvArgs) -> Result<()> {
    let all = run::profiles(cfg);
    let p = match args.profile.as_deref() {
        Some(name) => {
            let p = run::find(&all, name)?;
            run::note_if_not_the_projects(cfg, &p.name);
            ui::detail(&format!("`{}` — picked by name", p.name));
            p
        }
        None => {
            let (p, origin) = run::project_default(paths, cfg, &all)?;
            ui::detail(&format!("`{}` — {}", p.name, run::why(paths, origin)));
            p
        }
    };
    let fish = matches!(shell(args.shell), Some(Shell::Fish));
    println!("{}", export_line(p.config_dir.as_deref(), fish)?);
    println!("{}", marker_line(p.config_dir.is_none(), fish));
    let inherited = std::env::var_os(plugins::ENV);
    let root = plugins::shared_root(paths);
    let plugins_root = if p.shares_plugins {
        Some(Some(root.as_path()))
    } else if plugins::is_mccs(paths, inherited.as_deref()) {
        Some(None)
    } else {
        None
    };
    if let Some(root) = plugins_root {
        println!("{}", plugins_line(root, fish)?);
    }
    Ok(())
}

/// The line that points `CLAUDE_CODE_PLUGIN_CACHE_DIR` at the default's
/// plugins (a profile sharing them), or clears mcc's value (`None`).
fn plugins_line(root: Option<&Path>, fish: bool) -> Result<String> {
    let var = plugins::ENV;
    let Some(root) = root else {
        return Ok(if fish {
            format!("set -e {var}")
        } else {
            format!("unset {var}")
        });
    };
    let Some(root) = root.to_str() else {
        return Err(CliError::runtime(format!(
            "the plugins directory {} is not valid UTF-8 and can't be printed for a shell",
            root.display()
        ))
        .into());
    };
    Ok(if fish {
        format!("set -gx {var} {}", fish_quote(root))
    } else {
        format!("export {var}={}", sh_quote(root))
    })
}

/// The shell to print for: `--shell`, else bash inside a direnv `.envrc`
/// (direnv evaluates it with bash whatever your shell is), else `$SHELL`.
fn shell(flag: Option<Shell>) -> Option<Shell> {
    flag.or_else(|| std::env::var_os("DIRENV_IN_ENVRC").map(|_| Shell::Bash))
        .or_else(|| {
            std::env::var("SHELL")
                .ok()
                .and_then(|s| Shell::from_path(&s))
        })
}

/// The line that sets `CLAUDE_CONFIG_DIR` to `dir` (unsets it for `None`),
/// in fish syntax or POSIX sh syntax (bash, zsh).
fn export_line(dir: Option<&Path>, fish: bool) -> Result<String> {
    let Some(dir) = dir else {
        return Ok(if fish {
            "set -e CLAUDE_CONFIG_DIR".into()
        } else {
            "unset CLAUDE_CONFIG_DIR".into()
        });
    };
    let Some(dir) = dir.to_str() else {
        return Err(CliError::runtime(format!(
            "the profile directory {} is not valid UTF-8 and can't be printed for a shell",
            dir.display()
        ))
        .into());
    };
    Ok(if fish {
        format!("set -gx CLAUDE_CONFIG_DIR {}", fish_quote(dir))
    } else {
        format!("export CLAUDE_CONFIG_DIR={}", sh_quote(dir))
    })
}

/// The line that sets [`crate::shell::PROFILE_ENV`] to `default` when the default
/// profile (~/.claude) is the one printed, else clears it. Every output sets
/// or clears it, so evaluating `mcc env` again (or leaving a direnv
/// directory) never leaves a stale value behind. Without it, the unset
/// `CLAUDE_CONFIG_DIR` would let the `claude` function of `mcc default`
/// start the global default instead of ~/.claude.
fn marker_line(default: bool, fish: bool) -> String {
    let var = crate::shell::PROFILE_ENV;
    match (default, fish) {
        (true, true) => format!("set -gx {var} default"),
        (true, false) => format!("export {var}=default"),
        (false, true) => format!("set -e {var}"),
        (false, false) => format!("unset {var}"),
    }
}

/// `s` in POSIX single quotes: a `'` becomes `'\''`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `s` in fish single quotes, where only `\` and `'` are escaped.
fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_with_spaces_and_quotes_are_quoted_for_each_shell() {
        let dir = Path::new("/home/me/it's a \\dir/$HOME");
        assert_eq!(
            export_line(Some(dir), false).unwrap(),
            r"export CLAUDE_CONFIG_DIR='/home/me/it'\''s a \dir/$HOME'"
        );
        assert_eq!(
            export_line(Some(dir), true).unwrap(),
            r"set -gx CLAUDE_CONFIG_DIR '/home/me/it\'s a \\dir/$HOME'"
        );
    }

    #[test]
    fn a_profile_sharing_plugins_gets_the_plugins_root() {
        let root = Path::new("/home/me/.claude/plugins");
        assert_eq!(
            plugins_line(Some(root), false).unwrap(),
            "export CLAUDE_CODE_PLUGIN_CACHE_DIR='/home/me/.claude/plugins'"
        );
        assert_eq!(
            plugins_line(Some(root), true).unwrap(),
            "set -gx CLAUDE_CODE_PLUGIN_CACHE_DIR '/home/me/.claude/plugins'"
        );
        assert_eq!(
            plugins_line(None, false).unwrap(),
            "unset CLAUDE_CODE_PLUGIN_CACHE_DIR"
        );
        assert_eq!(
            plugins_line(None, true).unwrap(),
            "set -e CLAUDE_CODE_PLUGIN_CACHE_DIR"
        );
    }

    #[test]
    fn the_default_profile_unsets_the_variable() {
        assert_eq!(export_line(None, false).unwrap(), "unset CLAUDE_CONFIG_DIR");
        assert_eq!(export_line(None, true).unwrap(), "set -e CLAUDE_CONFIG_DIR");
    }

    #[test]
    fn only_the_default_profile_sets_the_marker() {
        assert_eq!(marker_line(true, false), "export MCC_PROFILE=default");
        assert_eq!(marker_line(true, true), "set -gx MCC_PROFILE default");
        assert_eq!(marker_line(false, false), "unset MCC_PROFILE");
        assert_eq!(marker_line(false, true), "set -e MCC_PROFILE");
    }
}
