//! Append-only file log at `~/.mcc/logs/mcc.log`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

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
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z");
        let _ = writeln!(f, "{ts} [{level:<5}] {msg}");
    }
}
