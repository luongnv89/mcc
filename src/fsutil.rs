//! Small filesystem helpers shared across commands.

use std::path::Path;

use anyhow::{Context, Result};
use walkdir::WalkDir;

/// Write a file via a temp sibling + rename so readers never see a partial file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.mcc-tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    // Keep the original file's permissions (e.g. 0600 on .claude.json).
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Total size of a tree in bytes (symlinks are not followed).
pub fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Remove a file, symlink, or directory tree. Missing paths are fine.
pub fn remove_any(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
    .with_context(|| format!("removing {}", path.display()))
}

/// Copy a file, symlink or directory tree at `src` to `dst`, creating `dst`'s
/// parent dirs. Symlinks are recreated with their target verbatim and never
/// followed, not even at the root; files keep their permissions (e.g. 0600).
/// A missing `src` copies nothing; other special files (sockets, FIFOs) are
/// skipped.
#[cfg(unix)]
pub fn copy_path(src: &Path, dst: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", src.display())),
    };
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if !meta.is_dir() {
        return copy_entry(src, dst, meta.file_type());
    }
    for entry in WalkDir::new(src)
        .follow_links(false)
        .follow_root_links(false)
    {
        let entry = entry.with_context(|| format!("reading {}", src.display()))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("walk stays under src");
        copy_entry(entry.path(), &dst.join(rel), entry.file_type())?;
    }
    Ok(())
}

/// Copy one directory entry without following it: see [`copy_path`].
#[cfg(unix)]
fn copy_entry(src: &Path, dst: &Path, ft: std::fs::FileType) -> Result<()> {
    if ft.is_symlink() {
        let target =
            std::fs::read_link(src).with_context(|| format!("reading {}", src.display()))?;
        std::os::unix::fs::symlink(target, dst)
            .with_context(|| format!("linking {}", dst.display()))?;
    } else if ft.is_dir() {
        std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    } else if ft.is_file() {
        std::fs::copy(src, dst)
            .with_context(|| format!("copying {} → {}", src.display(), dst.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)?.permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).with_context(|| format!("chmod +x {}", path.display()))
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(unix)]
pub fn set_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod 700 {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_and_remove() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a/b.txt");
        write_atomic(&f, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hello");
        write_atomic(&f, b"bye").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "bye");
        assert_eq!(dir_size(&d.path().join("a")), 3);
        remove_any(&d.path().join("a")).unwrap();
        assert!(!d.path().join("a").exists());
        remove_any(&d.path().join("missing")).unwrap();
    }

    #[test]
    fn copy_path_keeps_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join(".claude.json");
        std::fs::write(&src, "{}").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o600)).unwrap();
        let dst = d.path().join("backup/nested/.claude.json");
        copy_path(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "{}");
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn copy_path_copies_a_tree_and_keeps_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "outside").unwrap();
        let src = d.path().join("skills");
        std::fs::create_dir_all(src.join("one/empty")).unwrap();
        std::fs::write(src.join("one/SKILL.md"), "skill").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("linked")).unwrap();
        std::os::unix::fs::symlink("one/SKILL.md", src.join("rel")).unwrap();

        let dst = d.path().join("backup/skills");
        copy_path(&src, &dst).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("one/SKILL.md")).unwrap(),
            "skill"
        );
        assert!(dst.join("one/empty").is_dir());
        // Links are kept as links with the same target, never followed.
        let linked = dst.join("linked");
        assert!(
            std::fs::symlink_metadata(&linked)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(&linked).unwrap(), outside);
        assert_eq!(
            std::fs::read_link(dst.join("rel")).unwrap(),
            Path::new("one/SKILL.md")
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[test]
    fn copy_path_never_follows_a_root_symlink() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("default/plugins");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("installed_plugins.json"), "{}").unwrap();
        let src = d.path().join("profile/plugins");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &src).unwrap();

        let dst = d.path().join("backup/plugins");
        copy_path(&src, &dst).unwrap();
        let meta = std::fs::symlink_metadata(&dst).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(std::fs::read_link(&dst).unwrap(), target);
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);

        // A missing source copies nothing.
        copy_path(&d.path().join("missing"), &d.path().join("backup/missing")).unwrap();
        assert!(std::fs::symlink_metadata(d.path().join("backup/missing")).is_err());
    }
}
