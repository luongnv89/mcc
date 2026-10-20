//! Alias validation: syntax plus every way a name can already be taken.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::config::Config;
use crate::paths::Paths;

const RESERVED_NAMES: &[&str] = &[
    "claude",
    "mcc",
    "default",
    "help",
    "all",
    "share",
    "link",
    "refresh-launchers",
    "project",
    "run",
    "env",
    "exec",
    "statusline",
    crate::shell::PATH_BLOCK,
];

/// Shell reserved words and builtins across sh/bash/zsh/fish.
const SHELL_WORDS: &[&str] = &[
    "alias",
    "autoload",
    "bg",
    "bind",
    "break",
    "builtin",
    "bye",
    "case",
    "cd",
    "chdir",
    "command",
    "compgen",
    "complete",
    "continue",
    "coproc",
    "declare",
    "dirs",
    "disown",
    "do",
    "done",
    "echo",
    "elif",
    "else",
    "emulate",
    "enable",
    "end",
    "esac",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fc",
    "fg",
    "fi",
    "for",
    "foreach",
    "function",
    "functions",
    "getopts",
    "hash",
    "history",
    "if",
    "in",
    "integer",
    "jobs",
    "kill",
    "let",
    "limit",
    "local",
    "logout",
    "noglob",
    "popd",
    "print",
    "printf",
    "pushd",
    "pwd",
    "read",
    "readonly",
    "rehash",
    "repeat",
    "return",
    "select",
    "set",
    "setopt",
    "shift",
    "source",
    "suspend",
    "test",
    "then",
    "time",
    "times",
    "trap",
    "true",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "unfunction",
    "unhash",
    "unlimit",
    "unset",
    "unsetopt",
    "until",
    "wait",
    "whence",
    "where",
    "which",
    "while",
];

/// rc files scanned for aliases and functions (relative to home).
const RC_FILES: &[&str] = &[
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_aliases",
    ".profile",
    ".aliases",
    ".config/fish/config.fish",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub reason: String,
}

/// Validate alias syntax only.
pub fn check_syntax(alias: &str) -> Result<(), String> {
    let re = Regex::new(r"^[a-z][a-z0-9_-]{0,31}$").expect("static regex");
    if re.is_match(alias) {
        Ok(())
    } else {
        Err(format!(
            "\"{alias}\" is not a valid alias: use 1–32 characters, lowercase letters, digits, '-' or '_', starting with a letter"
        ))
    }
}

/// Every reason `alias` cannot be used. Empty = available.
pub fn conflicts(
    alias: &str,
    paths: &Paths,
    cfg: &Config,
    shell_path: Option<&str>,
) -> Vec<Conflict> {
    conflicts_for(alias, paths, cfg, shell_path, None)
}

/// [`conflicts`] for a profile being renamed to `alias`: its own directory
/// (`own_dir`, which never moves) is not a conflict, so a profile can take
/// back the name it was created with.
pub fn conflicts_for(
    alias: &str,
    paths: &Paths,
    cfg: &Config,
    shell_path: Option<&str>,
    own_dir: Option<&Path>,
) -> Vec<Conflict> {
    let mut out = Vec::new();
    let mut add = |r: String| out.push(Conflict { reason: r });

    if RESERVED_NAMES.contains(&alias) {
        add(format!("\"{alias}\" is reserved by mcc"));
    }
    if SHELL_WORDS.contains(&alias) {
        add(format!("\"{alias}\" is a shell keyword or builtin"));
    }
    if cfg.profiles.contains_key(alias) {
        add(format!("an mcc profile named \"{alias}\" already exists"));
    }
    let dir = paths.profile_dir(alias);
    if dir.exists() && own_dir != Some(dir.as_path()) {
        // A renamed profile keeps the directory of its first name.
        match cfg.profiles.iter().find(|(_, p)| p.dir == dir) {
            Some((owner, _)) if owner != alias => add(format!(
                "profile directory {} is used by profile \"{owner}\"",
                paths.display(&dir)
            )),
            _ => add(format!(
                "profile directory {} already exists",
                paths.display(&dir)
            )),
        }
    }
    let wrapper = paths.wrapper(alias);
    if std::fs::symlink_metadata(&wrapper).is_ok() {
        add(format!("{} already exists", paths.display(&wrapper)));
    }
    for hit in path_executables(alias) {
        if hit != wrapper {
            add(format!(
                "a command named \"{alias}\" exists at {}",
                hit.display()
            ));
        }
    }
    for (file, kind) in rc_definitions(alias, &paths.home) {
        add(format!(
            "{kind} \"{alias}\" is defined in {}",
            paths.display(&file)
        ));
    }
    if let Some(found) = shell_probe(alias, paths, shell_path)
        && out.is_empty()
    {
        out.push(Conflict {
            reason: format!("your shell already resolves \"{alias}\": {found}"),
        });
    }
    out
}

/// Executables named `alias` on `$PATH`.
fn path_executables(alias: &str) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return vec![];
    };
    let mut hits: Vec<PathBuf> = std::env::split_paths(&path)
        .map(|d| d.join(alias))
        .filter(|p| crate::fsutil::is_executable(p))
        .collect();
    hits.dedup();
    hits
}

/// Aliases / functions named `alias` in the user's rc files.
fn rc_definitions(alias: &str, home: &Path) -> Vec<(PathBuf, &'static str)> {
    let a = regex::escape(alias);
    let alias_re = Regex::new(&format!(r"^\s*alias\s+(?:-g\s+)?{a}(?:=|\s)")).expect("regex");
    let func_re = Regex::new(&format!(r"^\s*(?:function\s+{a}\b|{a}\s*\(\s*\))")).expect("regex");
    let mut out = Vec::new();
    for rel in RC_FILES {
        let file = home.join(rel);
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            if alias_re.is_match(line) {
                out.push((file.clone(), "alias"));
                break;
            }
            if func_re.is_match(line) {
                out.push((file.clone(), "function"));
                break;
            }
        }
    }
    let fish_fn = home
        .join(".config/fish/functions")
        .join(format!("{alias}.fish"));
    if fish_fn.exists() {
        out.push((fish_fn, "fish function"));
    }
    out
}

/// Ask the user's interactive shell whether it already resolves `alias`
/// (catches plugins/frameworks the rc scan misses). 3 second budget.
fn shell_probe(alias: &str, paths: &Paths, shell_path: Option<&str>) -> Option<String> {
    if std::env::var_os("MCC_NO_SHELL_PROBE").is_some() {
        return None;
    }
    let shell = shell_path.filter(|s| !s.is_empty())?;
    let script = if shell.ends_with("fish") {
        format!("type {alias}")
    } else {
        format!("type {alias} 2>/dev/null")
    };
    let mut child = Command::new(shell)
        .args(["-ic", &script])
        .env("HOME", &paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut s) = child.stdout.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut out);
                }
                let line = out
                    .lines()
                    .find(|l| l.contains(alias))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                return (status.success() && !line.is_empty()).then_some(line);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax() {
        for ok in [
            "work",
            "a",
            "work-2",
            "cus_luong",
            "abcdefghijklmnopqrstuvwxyz012345",
        ] {
            assert!(check_syntax(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Work",
            "2work",
            "-x",
            "my work",
            "a/b",
            "abcdefghijklmnopqrstuvwxyz0123456",
            "é",
        ] {
            assert!(check_syntax(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rc_scan_finds_aliases_and_functions() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join(".zshrc"),
            "# alias commented='x'\nalias cus_luong='CLAUDE_CONFIG_DIR=~/.cus_luong claude'\nmyfn() { :; }\nfunction other { :; }\n",
        )
        .unwrap();
        assert_eq!(rc_definitions("cus_luong", d.path()).len(), 1);
        assert_eq!(rc_definitions("myfn", d.path())[0].1, "function");
        assert_eq!(rc_definitions("other", d.path())[0].1, "function");
        assert!(rc_definitions("commented", d.path()).is_empty());
        assert!(rc_definitions("cus", d.path()).is_empty());
    }

    #[test]
    fn conflicts_detects_reserved_and_registry() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let mut cfg = Config::default();
        cfg.profiles.insert(
            "taken".into(),
            crate::config::Profile {
                dir: paths.profile_dir("taken"),
                mode: "fresh".into(),
                created_at: String::new(),
                wrapper: None,
                rc_file: None,
                last_sync: None,
                share_sessions: false,
                share_note_acknowledged_at: None,
                share_group: None,
                share_cross_org_allowed_at: None,
                label: None,
                account: None,
                settings: None,
                link: Vec::new(),
            },
        );
        assert!(!conflicts("claude", &paths, &cfg, None).is_empty());
        assert!(!conflicts("share", &paths, &cfg, None).is_empty());
        assert!(!conflicts("refresh-launchers", &paths, &cfg, None).is_empty());
        assert!(!conflicts("project", &paths, &cfg, None).is_empty());
        assert!(!conflicts("run", &paths, &cfg, None).is_empty());
        assert!(!conflicts("path", &paths, &cfg, None).is_empty());
        assert!(!conflicts("export", &paths, &cfg, None).is_empty());
        assert!(!conflicts("taken", &paths, &cfg, None).is_empty());
        assert!(conflicts("zz-unused-name-xyz", &paths, &cfg, None).is_empty());

        // A renamed profile keeps its directory and may take its old name back.
        let own = paths.profile_dir("was");
        std::fs::create_dir_all(&own).unwrap();
        assert!(!conflicts("was", &paths, &cfg, None).is_empty());
        assert!(conflicts_for("was", &paths, &cfg, None, Some(&own)).is_empty());
        assert!(
            !conflicts_for("was", &paths, &cfg, None, Some(&paths.profile_dir("x"))).is_empty()
        );
    }
}
