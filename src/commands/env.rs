//! `mcc env` — print the shell code that points `CLAUDE_CONFIG_DIR` at a
//! profile, for `eval "$(mcc env)"` (e.g. in a direnv `.envrc`).
//!
//! Without a profile it is the one `mcc run` would start here: the project's
//! default, else the default profile (~/.claude), whose code unsets the
//! variable. Usage is never checked, and nothing is written or locked.

use std::path::Path;

use anyhow::Result;

use crate::cli::EnvArgs;
use crate::config::Config;
use crate::env_detect::Shell;
use crate::error::CliError;
use crate::paths::Paths;
use crate::ui;

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
            let (p, project) = run::project_default(paths, cfg, &all)?;
            ui::detail(&format!("`{}` — {}", p.name, run::why(paths, project)));
            p
        }
    };
    let fish = matches!(shell(args.shell), Some(Shell::Fish));
    println!("{}", export_line(p.config_dir.as_deref(), fish)?);
    Ok(())
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
    fn the_default_profile_unsets_the_variable() {
        assert_eq!(export_line(None, false).unwrap(), "unset CLAUDE_CONFIG_DIR");
        assert_eq!(export_line(None, true).unwrap(), "set -e CLAUDE_CONFIG_DIR");
    }
}
