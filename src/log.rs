//! Append-only file log at `~/.mcc/logs/mcc.log`, rotated once at about 1 MB
//! (the previous log is kept as `mcc.log.1`).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Size at which the log is rotated before the next line is appended.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Enable file logging. Failures to create the log dir are ignored: logging
/// must never break a command.
pub fn init(path: PathBuf) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut p) = LOG_PATH.lock() {
        *p = Some(path);
    }
}

/// Enable file logging only if the log directory already exists. Used by
/// commands that must never create mcc state (`mcc completions`).
pub fn init_if_present(path: PathBuf) {
    if path.parent().is_some_and(|dir| dir.is_dir())
        && let Ok(mut p) = LOG_PATH.lock()
    {
        *p = Some(path);
    }
}

pub fn write(level: &str, msg: &str) {
    let Ok(guard) = LOG_PATH.lock() else { return };
    let Some(path) = guard.as_ref() else { return };
    rotate_if_needed(path, MAX_LOG_BYTES);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z");
        let _ = writeln!(f, "{ts} [{level:<5}] {msg}");
    }
}

/// `mcc.log` → `mcc.log.1`.
fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Move the log to `<log>.1`, replacing an older rotation, once it has
/// reached `max` bytes. Errors are ignored: logging must never break a
/// command.
fn rotate_if_needed(path: &Path, max: u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if !meta.is_file() || meta.len() < max {
        return;
    }
    let _ = std::fs::rename(path, rotated_path(path));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotated_path_appends_suffix() {
        assert_eq!(
            rotated_path(Path::new("/h/.mcc/logs/mcc.log")),
            PathBuf::from("/h/.mcc/logs/mcc.log.1")
        );
    }

    #[test]
    fn small_log_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("mcc.log");
        std::fs::write(&log, "short\n").unwrap();
        rotate_if_needed(&log, 100);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "short\n");
        assert!(!rotated_path(&log).exists());
    }

    #[test]
    fn full_log_rotates_and_replaces_older_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("mcc.log");
        let old = rotated_path(&log);
        std::fs::write(&old, "oldest\n").unwrap();
        std::fs::write(&log, "x".repeat(100)).unwrap();
        rotate_if_needed(&log, 100);
        assert!(!log.exists());
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "x".repeat(100));
    }

    #[test]
    fn missing_log_or_unwritable_target_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        rotate_if_needed(&dir.path().join("absent.log"), 1);
        // A directory in the way of the rotation target: rename fails, ignored.
        let log = dir.path().join("mcc.log");
        std::fs::write(&log, "x".repeat(10)).unwrap();
        std::fs::create_dir(rotated_path(&log)).unwrap();
        std::fs::write(rotated_path(&log).join("keep"), "").unwrap();
        rotate_if_needed(&log, 1);
        assert!(log.exists());
    }
}
