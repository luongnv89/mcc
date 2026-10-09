//! Small filesystem helpers shared across commands.

use std::path::Path;

use anyhow::{Context, Result};
use walkdir::WalkDir;

/// Write a file via a temp sibling + rename so readers never see a partial file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
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
    std::fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
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
}
