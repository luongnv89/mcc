//! Launcher management: `~/.local/bin/<alias>` wrappers and managed rc blocks.
//!
//! rc blocks are delimited by `# >>> mcc:<id> >>>` / `# <<< mcc:<id> <<<` so
//! they can be updated or removed without touching anything else in the file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::env_detect::Shell;
use crate::fsutil;
use crate::paths::Paths;

pub const WRAPPER_MARKER: &str = "# managed-by: mcc";

/// Block id of the `~/.local/bin` PATH entry. Profile blocks use the alias as
/// their id, so this name is reserved and can never be an alias.
pub const PATH_BLOCK: &str = "path";

fn begin(id: &str) -> String {
    format!("# >>> mcc:{id} >>>")
}

fn end(id: &str) -> String {
    format!("# <<< mcc:{id} <<<")
}

/// Wrapper script that launches claude with the profile's config dir. Uses
/// `claude` from PATH when available, else the binary found at `add` time
/// (so it also works from cron, IDE tasks, or shells with a trimmed PATH).
pub fn wrapper_script(
    paths: &Paths,
    alias: &str,
    profile_dir: &Path,
    claude_bin: Option<&Path>,
) -> String {
    let fallback = match claude_bin {
        Some(bin) => format!("exec {} \"$@\"", sh_quote(&bin.to_string_lossy())),
        None => "echo \"claude not found on PATH\" >&2\nexit 127".to_string(),
    };
    format!(
        "#!/bin/sh\n{WRAPPER_MARKER}\n# Claude Code profile \"{alias}\". Remove with: mcc delete {alias}\n\
         export CLAUDE_CONFIG_DIR=\"{dir}\"\n\
         if command -v claude >/dev/null 2>&1; then exec claude \"$@\"; fi\n\
         {fallback}\n",
        dir = paths.shell_expr(profile_dir)
    )
}

/// Single-quote a string for POSIX sh.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn write_wrapper(
    paths: &Paths,
    alias: &str,
    profile_dir: &Path,
    claude_bin: Option<&Path>,
) -> Result<PathBuf> {
    let path = paths.wrapper(alias);
    fsutil::write_atomic(
        &path,
        wrapper_script(paths, alias, profile_dir, claude_bin).as_bytes(),
    )?;
    fsutil::set_executable(&path)?;
    Ok(path)
}

/// True when the file at `path` is a wrapper mcc wrote (safe to delete).
pub fn is_mcc_wrapper(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.contains(WRAPPER_MARKER))
        .unwrap_or(false)
}

/// The rc block body defining the alias for one profile.
pub fn alias_block(paths: &Paths, shell: Shell, alias: &str, profile_dir: &Path) -> String {
    let dir = paths.shell_expr(profile_dir);
    let line = match shell {
        Shell::Fish => format!("alias {alias} 'env CLAUDE_CONFIG_DIR=\"{dir}\" claude'"),
        Shell::Zsh | Shell::Bash => format!("alias {alias}='CLAUDE_CONFIG_DIR=\"{dir}\" claude'"),
    };
    format!(
        "# Claude Code profile \"{alias}\" — managed by mcc (remove with: mcc delete {alias})\n{line}"
    )
}

/// rc block that puts `~/.local/bin` on PATH.
pub fn path_block(shell: Shell) -> String {
    match shell {
        Shell::Fish => "fish_add_path -g $HOME/.local/bin".to_string(),
        Shell::Zsh | Shell::Bash => "export PATH=\"$HOME/.local/bin:$PATH\"".to_string(),
    }
}

pub fn has_block(rc: &Path, id: &str) -> bool {
    std::fs::read_to_string(rc)
        .map(|s| s.contains(&begin(id)))
        .unwrap_or(false)
}

/// Insert or replace the block `id` in `rc`. Backs the file up first.
pub fn upsert_block(paths: &Paths, rc: &Path, id: &str, body: &str) -> Result<()> {
    let original = read_or_empty(rc)?;
    let (stripped, _) = strip_block(&original, id);
    let mut out = stripped.trim_end_matches('\n').to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "{}\n{}\n{}\n",
        begin(id),
        body.trim_end(),
        end(id)
    ));
    backup(paths, rc, &original)?;
    fsutil::write_atomic(&write_target(rc), out.as_bytes())
}

/// Remove the block `id` from `rc`. Returns false when there was no block.
pub fn remove_block(paths: &Paths, rc: &Path, id: &str) -> Result<bool> {
    if !rc.exists() {
        return Ok(false);
    }
    let original = read_or_empty(rc)?;
    let (stripped, found) = strip_block(&original, id);
    if found {
        backup(paths, rc, &original)?;
        fsutil::write_atomic(&write_target(rc), stripped.as_bytes())?;
    }
    Ok(found)
}

/// The real file behind `rc`. rc files are often symlinks into a dotfiles repo
/// (GNU stow, yadm, ...); writing through the link (temp file + rename next to
/// the target) keeps the link intact instead of replacing it with a plain file.
/// Dangling links resolve to their target, which is then created. Regular and
/// missing files resolve to themselves.
fn write_target(rc: &Path) -> PathBuf {
    let mut path = rc.to_path_buf();
    // Bounded like the kernel's ELOOP limit so a link cycle cannot spin forever.
    for _ in 0..40 {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            break;
        };
        if !meta.file_type().is_symlink() {
            break;
        }
        let Ok(target) = std::fs::read_link(&path) else {
            break;
        };
        path = match path.parent() {
            Some(dir) if target.is_relative() => dir.join(target),
            _ => target,
        };
    }
    path
}

fn read_or_empty(rc: &Path) -> Result<String> {
    match std::fs::read_to_string(rc) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", rc.display())),
    }
}

/// Copy the rc file to `~/.mcc/backups/` before modifying it. Never overwrites
/// an earlier backup (several edits can land in the same millisecond).
fn backup(paths: &Paths, rc: &Path, content: &str) -> Result<()> {
    if content.is_empty() {
        return Ok(());
    }
    let name = rc
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "rc".into());
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
    let base = format!("{}.{stamp}", name.trim_start_matches('.'));
    let mut dest = paths.backups_dir.join(&base);
    let mut n = 1;
    while dest.exists() {
        dest = paths.backups_dir.join(format!("{base}.{n}"));
        n += 1;
    }
    fsutil::write_atomic(&dest, content.as_bytes())
}

/// Remove the block (and one blank line before it). Returns (text, found).
fn strip_block(text: &str, id: &str) -> (String, bool) {
    let (b, e) = (begin(id), end(id));
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut found = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == b
            && let Some(off) = lines[i..].iter().position(|l| l.trim() == e)
        {
            found = true;
            if out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            i += off + 1;
            continue;
        }
        out.push(lines[i]);
        i += 1;
    }
    let mut s = out.join("\n");
    if !s.is_empty() && (text.ends_with('\n') || found) {
        s.push('\n');
    }
    (s, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_and_remove_block() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let rc = d.path().join(".zshrc");
        std::fs::write(&rc, "export A=1\nalias x=y\n").unwrap();

        upsert_block(&paths, &rc, "work", "alias work='true'").unwrap();
        let s = std::fs::read_to_string(&rc).unwrap();
        assert!(s.starts_with(
            "export A=1\nalias x=y\n\n# >>> mcc:work >>>\nalias work='true'\n# <<< mcc:work <<<\n"
        ));

        // Idempotent replace.
        upsert_block(&paths, &rc, "work", "alias work='false'").unwrap();
        let s = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(s.matches(">>> mcc:work >>>").count(), 1);
        assert!(s.contains("alias work='false'"));

        assert!(remove_block(&paths, &rc, "work").unwrap());
        assert_eq!(
            std::fs::read_to_string(&rc).unwrap(),
            "export A=1\nalias x=y\n"
        );
        assert!(!remove_block(&paths, &rc, "work").unwrap());
        // One backup per modification, none overwritten, the original kept.
        let backups: Vec<_> = paths
            .backups_dir
            .read_dir()
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 3, "{backups:?}");
        assert!(
            backups
                .iter()
                .any(|p| std::fs::read_to_string(p).unwrap() == "export A=1\nalias x=y\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_symlink_chain() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let real = d.path().join("dotfiles/zshrc");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "export A=1\n").unwrap();
        let mid = d.path().join("mid");
        let rc = d.path().join(".zshrc");
        std::os::unix::fs::symlink(&real, &mid).unwrap();
        std::os::unix::fs::symlink(&mid, &rc).unwrap();

        upsert_block(&paths, &rc, "work", "alias work='true'").unwrap();
        for link in [&rc, &mid] {
            assert!(
                std::fs::symlink_metadata(link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        assert!(std::fs::read_to_string(&real).unwrap().contains("mcc:work"));

        assert!(remove_block(&paths, &rc, "work").unwrap());
        assert!(
            std::fs::symlink_metadata(&rc)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "export A=1\n");
    }

    #[test]
    fn strip_keeps_other_blocks() {
        let text = "a\n\n# >>> mcc:one >>>\nx\n# <<< mcc:one <<<\n\n# >>> mcc:two >>>\ny\n# <<< mcc:two <<<\n";
        let (s, found) = strip_block(text, "one");
        assert!(found);
        assert_eq!(s, "a\n\n# >>> mcc:two >>>\ny\n# <<< mcc:two <<<\n");
    }

    #[test]
    fn alias_lines() {
        let paths = Paths::new("/h".into());
        let dir = paths.profile_dir("work");
        assert!(
            alias_block(&paths, Shell::Zsh, "work", &dir)
                .contains("alias work='CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" claude'")
        );
        assert!(
            alias_block(&paths, Shell::Fish, "work", &dir)
                .contains("alias work 'env CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" claude'")
        );
        let w = wrapper_script(&paths, "work", &dir, Some(Path::new("/opt/it's/claude")));
        assert!(w.starts_with("#!/bin/sh\n# managed-by: mcc\n"));
        assert!(w.contains("export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\"\n"));
        assert!(w.contains("if command -v claude >/dev/null 2>&1; then exec claude \"$@\"; fi\n"));
        assert!(w.contains("exec '/opt/it'\\''s/claude' \"$@\"\n"), "{w}");
        assert!(wrapper_script(&paths, "work", &dir, None).contains("exit 127"));
    }
}
