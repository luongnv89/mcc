//! API-key Claude Code profiles (#132): how a profile gets its key, and how
//! mcc recognizes the key file it stores.
//!
//! Claude Code reads the key through `apiKeyHelper` in the profile's
//! `settings.json`, set in one of two modes:
//!
//! - helper mode (`mcc add --api-key-helper CMD`): the user's own command;
//!   mcc stores nothing and never runs it;
//! - managed mode (`--api-key-stdin`, `--api-key-prompt`): mcc writes the key
//!   once to `~/.mcc/keys/<dir name>-<hash>.key` (0600, in a 0700
//!   directory) and sets `apiKeyHelper` to `cat '<that file>'`. mcc never
//!   reads the file again: Claude Code does, when it needs the key.
//!
//! The file is named after the profile's directory, never its alias, so a
//! rename keeps it, the way Claude Code names a profile's Keychain item
//! ([`crate::usage::keychain_service`]).

use std::fmt;
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::CliError;
use crate::paths::Paths;
use crate::{fsutil, ui};

/// What every Anthropic API key starts with. An argument containing it is
/// redacted from logs and errors, and refused as an alias.
pub const KEY_PREFIX: &str = "sk-ant-";

/// Refusing a key-shaped value given on the command line, without repeating it.
pub const KEY_ON_COMMAND_LINE: &str = "that looks like an API key; pass keys with --api-key-stdin or --api-key-prompt, never on the command line";

/// Longest key accepted, in bytes: far above a real key, low enough that a
/// wrong pipe (a whole file) is refused instead of stored.
const KEY_MAX: usize = 1024;

/// The settings key Claude Code runs to get a profile's API key.
pub const HELPER_KEY: &str = "apiKeyHelper";

/// Whether `text` holds something shaped like an API key.
pub fn looks_like_key(text: &str) -> bool {
    text.contains(KEY_PREFIX)
}

/// An API key read from stdin or a hidden prompt. Its `Debug` hides it and
/// it has no `Display`, so it can't reach a log line or a message.
pub struct ApiKey(String);

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// A key as typed or piped: surrounding whitespace (the trailing newline)
/// dropped; empty, oversized, or with spaces or control characters inside
/// refused. The message never repeats the key.
pub fn parse_key(raw: &str) -> std::result::Result<ApiKey, String> {
    let key = raw.trim();
    if key.is_empty() {
        return Err("the API key is empty".into());
    }
    if key.len() > KEY_MAX {
        return Err(format!("the API key is longer than {KEY_MAX} bytes"));
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the API key contains spaces or control characters".into());
    }
    Ok(ApiKey(key.to_string()))
}

/// `--api-key-stdin`: the key piped in. A terminal on stdin is refused
/// (typing there would echo the key).
pub fn read_stdin() -> Result<ApiKey> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(CliError::usage(
            "--api-key-stdin reads the key from a pipe, but stdin is a terminal",
        )
        .hint("pipe it in, e.g. `mcc add api --api-key-stdin < key.txt`, or use --api-key-prompt to type it hidden")
        .into());
    }
    let mut raw = String::new();
    stdin
        .lock()
        .take(KEY_MAX as u64 + 2)
        .read_to_string(&mut raw)
        .context("reading the API key from stdin")?;
    parse_key(&raw).map_err(|m| {
        CliError::usage(m)
            .hint("pipe exactly one key, e.g. `mcc add api --api-key-stdin < key.txt`")
            .into()
    })
}

/// `--api-key-prompt`: the key typed at a hidden prompt. Without a
/// terminal, a usage error pointing to `--api-key-stdin`.
pub fn read_prompt() -> Result<ApiKey> {
    if !ui::is_interactive() {
        return Err(
            CliError::usage("--api-key-prompt needs a terminal to ask for the key")
                .hint("pipe the key in with --api-key-stdin instead")
                .into(),
        );
    }
    let raw = ui::password("API key (input hidden)", "pass --api-key-stdin")?;
    parse_key(&raw).map_err(|m| {
        CliError::usage(m)
            .hint("run the command again and paste the whole key")
            .into()
    })
}

/// Where mcc keeps the key of the profile whose directory is `dir`:
/// `~/.mcc/keys/<dir name>-<first 8 hex of SHA-256 of dir>.key`, the
/// directory as registered (it may be gone when `mcc delete` asks).
pub fn managed_key_path(paths: &Paths, dir: &Path) -> PathBuf {
    let digest = Sha256::digest(dir.to_string_lossy().as_bytes());
    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    let name = dir
        .file_name()
        .map_or_else(|| "profile".into(), |n| n.to_string_lossy().into_owned());
    paths.keys_dir.join(format!("{name}-{hex}.key"))
}

/// The `apiKeyHelper` that hands Claude Code the key in `key_file`.
pub fn managed_helper(key_file: &Path) -> String {
    format!(
        "cat {}",
        crate::shell::sh_quote(&key_file.to_string_lossy())
    )
}

/// The `apiKeyHelper` of the profile at `dir`, if its `settings.json` has
/// one. Read, never run.
pub fn helper_of(dir: &Path) -> Option<String> {
    let bytes = std::fs::read(dir.join("settings.json")).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get(HELPER_KEY)?.as_str().map(str::to_string)
}

/// The key file mcc stored for the profile at `dir`: only when its
/// `apiKeyHelper` is exactly the command mcc wrote for it, so a helper of
/// the user's is never taken for one of mcc's.
pub fn managed_key_of(paths: &Paths, dir: &Path) -> Option<PathBuf> {
    let key = managed_key_path(paths, dir);
    (helper_of(dir)? == managed_helper(&key)).then_some(key)
}

/// A helper command given with `--api-key-helper`: one line of text, and
/// never a key itself. The message never repeats it.
pub fn check_helper(cmd: &str) -> std::result::Result<(), String> {
    if cmd.trim().is_empty() {
        return Err("--api-key-helper is empty".into());
    }
    if looks_like_key(cmd) {
        return Err(KEY_ON_COMMAND_LINE.into());
    }
    if cmd.chars().any(char::is_control) {
        return Err("--api-key-helper contains control characters (a newline?)".into());
    }
    Ok(())
}

/// A key file written by [`write_key_file`], with the file it replaced
/// moved aside, so a rollback can put it back ([`undo_write`]).
#[derive(Debug, Clone)]
pub struct Written {
    pub path: PathBuf,
    backup: Option<PathBuf>,
}

/// Write `key` to `path` (inside `~/.mcc/keys`, made 0700): created 0600
/// with `O_EXCL` as a temporary file next to it, then renamed over `path`.
/// A file already at `path` (kept by `mcc delete --keep-auth`) is moved
/// aside first and comes back if the profile is rolled back.
pub fn write_key_file(path: &Path, key: &ApiKey) -> Result<Written> {
    let dir = path.parent().context("a key file has a directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    fsutil::set_private_dir(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pid = std::process::id();
    let tmp = dir.join(format!(".{name}.mcc-tmp-{pid}"));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(key.0.as_bytes())?;
            f.sync_all()
        });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    let backup = if std::fs::symlink_metadata(path).is_ok() {
        let b = dir.join(format!(".{name}.mcc-backup-{pid}"));
        if let Err(e) = std::fs::rename(path, &b) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("moving {} aside", path.display()));
        }
        Some(b)
    } else {
        None
    };
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        if let Some(b) = &backup {
            let _ = std::fs::rename(b, path);
        }
        return Err(e).with_context(|| format!("writing {}", path.display()));
    }
    Ok(Written {
        path: path.to_path_buf(),
        backup,
    })
}

/// Undo [`write_key_file`]: remove the new key file and put back the one
/// it replaced.
pub fn undo_write(w: &Written) -> Result<()> {
    fsutil::remove_any(&w.path)?;
    if let Some(b) = &w.backup {
        std::fs::rename(b, &w.path).with_context(|| format!("restoring {}", w.path.display()))?;
    }
    Ok(())
}

/// Keep [`write_key_file`]'s result: drop the replaced key moved aside.
pub fn keep_write(w: &Written) -> Result<()> {
    match &w.backup {
        Some(b) => fsutil::remove_any(b),
        None => Ok(()),
    }
}

/// Set `apiKeyHelper` to `cmd` in the `settings.json` of the profile at
/// `dir` (a new file when it has none), keeping every other key.
pub fn set_helper(dir: &Path, cmd: &str) -> Result<()> {
    let path = dir.join("settings.json");
    let mut settings = match crate::copy::read_json(&path)? {
        None => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => {
            return Err(
                CliError::input(format!("{} is not a JSON object", path.display()))
                    .hint("fix the file by hand, then retry")
                    .into(),
            );
        }
    };
    settings.insert(HELPER_KEY.into(), Value::String(cmd.into()));
    let mut text = serde_json::to_string_pretty(&Value::Object(settings))?;
    text.push('\n');
    fsutil::write_atomic(&path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn keys_are_trimmed_and_checked_without_being_repeated() {
        let k = parse_key("sk-ant-test-fixture\n").unwrap();
        assert_eq!(k.0, "sk-ant-test-fixture");
        assert_eq!(format!("{k:?}"), "ApiKey(<redacted>)");
        for bad in ["", " \n", "sk-ant-a b", "sk-ant-a\tb", "sk-ant-a\u{7}"] {
            let m = parse_key(bad).unwrap_err();
            assert!(!m.contains("sk-ant"), "{m}");
        }
        assert!(parse_key(&"k".repeat(KEY_MAX)).is_ok());
        assert!(parse_key(&"k".repeat(KEY_MAX + 1)).is_err());
    }

    #[test]
    fn helpers_never_carry_a_key_or_a_second_line() {
        assert!(check_helper("op read op://vault/anthropic/key").is_ok());
        for bad in ["", "  ", "echo sk-ant-test-fixture", "a\nb"] {
            let m = check_helper(bad).unwrap_err();
            assert!(!m.contains("sk-ant-test"), "{m}");
        }
    }

    #[test]
    fn the_key_file_follows_the_directory_not_the_alias() {
        let p = Paths::new(PathBuf::from("/h"));
        let a = managed_key_path(&p, Path::new("/h/.mcc/profiles/api"));
        assert!(a.starts_with("/h/.mcc/keys"), "{a:?}");
        let name = a.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("api-")
                && name.ends_with(".key")
                && name.len() == "api-".len() + 8 + 4,
            "{name}"
        );
        assert_eq!(a, managed_key_path(&p, Path::new("/h/.mcc/profiles/api")));
        assert_ne!(a, managed_key_path(&p, Path::new("/h/.mcc/profiles/api2")));
        // A home with a quote in it is still one shell word.
        let q = Path::new("/Users/o'neil/.mcc/keys/api-0.key");
        assert_eq!(
            managed_helper(q),
            "cat '/Users/o'\\''neil/.mcc/keys/api-0.key'"
        );
    }

    #[test]
    fn key_files_are_private_and_a_rollback_restores_the_old_one() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("keys/api-00000000.key");
        let w = write_key_file(&path, &parse_key("sk-ant-test-fixture").unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "sk-ant-test-fixture"
        );
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        keep_write(&w).unwrap();

        // A second write replaces it; undone, the first comes back.
        let w2 = write_key_file(&path, &parse_key("sk-ant-test-other").unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sk-ant-test-other");
        undo_write(&w2).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "sk-ant-test-fixture"
        );
        // Kept instead: nothing is left beside it.
        let w3 = write_key_file(&path, &parse_key("sk-ant-test-other").unwrap()).unwrap();
        keep_write(&w3).unwrap();
        let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["api-00000000.key"]);
    }

    #[test]
    fn only_the_helper_mcc_wrote_marks_a_managed_key() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let dir = paths.profile_dir("api");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(managed_key_of(&paths, &dir), None);
        set_helper(&dir, "op read op://x").unwrap();
        assert_eq!(managed_key_of(&paths, &dir), None);
        let key = managed_key_path(&paths, &dir);
        set_helper(&dir, &managed_helper(&key)).unwrap();
        assert_eq!(managed_key_of(&paths, &dir), Some(key));
    }
}
