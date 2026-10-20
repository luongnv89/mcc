//! `mcc statusline` — an opt-in Claude Code status-line shim.
//!
//! `mcc statusline install` puts the shim in the default profile's
//! `settings.json` as `statusLine` (backing the file up first) and keeps the
//! previous `statusLine` in `~/.mcc/state/default/statusline.json`. Profiles
//! receive the shim with their settings on the next `mcc sync`; when
//! `[sync] exclude` has `settings`, `install` offers to add it to each profile
//! directly, which keeps that profile's own `statusLine` in
//! `~/.mcc/state/<alias>/statusline.json`.
//!
//! Claude Code runs the shim (bare `mcc statusline`) on every status-line
//! refresh with the session's JSON on stdin. It works out the profile from
//! `CLAUDE_CONFIG_DIR`, saves `rate_limits` to
//! `~/.mcc/state/<profile>/usage.json`, then runs the previous `statusLine`
//! command with the same stdin and prints its output. That path runs many
//! times a minute: it never takes the config lock, never writes
//! `~/.mcc/config.toml`, never logs, and never fails because of mcc's own
//! bookkeeping.

use std::ffi::OsStr;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::cli::{StatuslineAction, StatuslineArgs};
use crate::config::Config;
use crate::copy::{self, Rewriter};
use crate::error::CliError;
use crate::paths::Paths;
use crate::{fsutil, ui};

/// The default profile's name in `~/.mcc/state/`.
pub const DEFAULT_PROFILE: &str = "default";
/// The cached rate limits, inside a profile's state dir.
pub const USAGE_FILE: &str = "usage.json";
/// The `statusLine` the shim replaced, inside a profile's state dir.
pub const RECORD_FILE: &str = "statusline.json";
/// Set for the wrapped command, so a shim that ends up wrapping itself
/// prints instead of recursing.
const NESTED_ENV: &str = "MCC_STATUSLINE_NESTED";

pub fn run(paths: &Paths, cfg: &Config, args: &StatuslineArgs) -> Result<()> {
    match &args.action {
        Some(StatuslineAction::Install { prefix, yes }) => install(paths, cfg, *prefix, *yes),
        Some(StatuslineAction::Uninstall) => uninstall(paths, cfg),
        None => render(paths, args.prefix),
    }
}

// ---------------------------------------------------------------------------
// The shim (hot path)
// ---------------------------------------------------------------------------

/// The shim itself: cache the rate limits, run the wrapped command, print.
pub fn render(paths: &Paths, prefix: bool) -> Result<()> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(CliError::usage(
            "`mcc statusline` reads Claude Code's status-line JSON on stdin",
        )
        .hint("run `mcc statusline install` to have Claude Code call it")
        .into());
    }
    let mut input = Vec::new();
    stdin.read_to_end(&mut input).context("reading stdin")?;

    // Read-only and lock-free: the config is only used to name the profile.
    let cfg = Config::load(&paths.config_file).unwrap_or_default();
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
    let profile = profile_for(paths, &cfg, config_dir.as_deref());

    if let Some(name) = &profile
        && let Ok(status) = serde_json::from_slice::<Value>(&input)
        && let Some(usage) = usage_record(name, &status, chrono::Utc::now())
    {
        // Best effort: a full disk must not blank the status line.
        let _ = write_json(&paths.profile_state_dir(name).join(USAGE_FILE), &usage);
    }

    let output = match wrapped_command(paths, &cfg, profile.as_deref()) {
        Some(cmd) if std::env::var_os(NESTED_ENV).is_none() => run_wrapped(&cmd, &input),
        _ => Vec::new(),
    };
    let text = String::from_utf8_lossy(&output);
    let shown = match profile.as_deref().filter(|_| prefix) {
        Some(name) => with_prefix(
            &segment(
                name,
                super::shown_label(&cfg, name),
                super::profile_color(&cfg, name),
            ),
            &text,
        ),
        None => text.into_owned(),
    };
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(shown.as_bytes());
    let _ = out.flush();
    Ok(())
}

/// The profile a `CLAUDE_CONFIG_DIR` value belongs to: unset or empty is the
/// default profile, as for Claude Code itself; a registered profile's dir is
/// its alias; any other dir under `~/.mcc/profiles` is its directory name.
/// `None` for a directory mcc doesn't manage.
pub fn profile_for(paths: &Paths, cfg: &Config, config_dir: Option<&OsStr>) -> Option<String> {
    let dir = match config_dir.filter(|d| !d.is_empty()) {
        None => return Some(DEFAULT_PROFILE.into()),
        Some(d) => PathBuf::from(d),
    };
    let dir = expand_tilde(paths, &dir);
    if same_dir(&dir, &paths.default_dir) {
        return Some(DEFAULT_PROFILE.into());
    }
    if let Some((alias, _)) = cfg.profiles.iter().find(|(_, p)| same_dir(&dir, &p.dir)) {
        return Some(alias.clone());
    }
    let parent = dir.parent()?;
    if same_dir(parent, &paths.profiles_dir) {
        let name = dir.file_name()?.to_str()?;
        if !name.is_empty()
            && !name.starts_with('.')
            && name != DEFAULT_PROFILE
            && !cfg.profiles.contains_key(name)
        {
            return Some(name.to_string());
        }
    }
    None
}

pub(crate) fn expand_tilde(paths: &Paths, p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => paths.home.join(rest),
        Err(_) => p.to_path_buf(),
    }
}

/// Equal paths, ignoring trailing slashes, `.` parts and symlinks.
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.components().collect::<PathBuf>();
    if norm(a) == norm(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// What `usage.json` holds, or `None` when the status JSON has no rate limits
/// (API-key logins, or before the session's first response).
pub fn usage_record(
    profile: &str,
    status: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Value> {
    let limits = status.get("rate_limits")?.as_object()?;
    let window = |key: &str| {
        let w = limits.get(key)?.as_object()?;
        let mut out = Map::new();
        for field in ["used_percentage", "resets_at"] {
            out.insert(field.into(), w.get(field).cloned().unwrap_or(Value::Null));
        }
        Some(Value::Object(out))
    };
    let (five_hour, seven_day) = (window("five_hour"), window("seven_day"));
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(json!({
        "profile": profile,
        "updated_at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "five_hour": five_hour,
        "seven_day": seven_day,
    }))
}

/// The command the shim wraps for `profile`: its own record, else (for a
/// profile that got the shim through `mcc sync`) the default profile's, with
/// paths rewritten the way sync rewrites them.
fn wrapped_command(paths: &Paths, cfg: &Config, profile: Option<&str>) -> Option<String> {
    let profile = profile.unwrap_or(DEFAULT_PROFILE);
    if let Some(rec) = read_record(paths, profile) {
        return command_of(rec.original.as_ref()).map(str::to_string);
    }
    if profile == DEFAULT_PROFILE {
        return None;
    }
    let rec = read_record(paths, DEFAULT_PROFILE)?;
    let cmd = command_of(rec.original.as_ref())?;
    let dir = cfg
        .profiles
        .get(profile)
        .map(|p| p.dir.clone())
        .unwrap_or_else(|| paths.profile_dir(profile));
    Some(Rewriter::new(paths, &dir, &cfg.sync).apply(cmd))
}

/// A `statusLine`'s command, when it is a command (and not the shim).
fn command_of(status_line: Option<&Value>) -> Option<&str> {
    let cmd = status_line?.get("command")?.as_str()?;
    (!cmd.trim().is_empty() && !is_shim(cmd)).then_some(cmd)
}

/// Run `cmd` through `sh -c` with `input` on stdin; its stdout, whatever
/// its exit status. Its stderr goes to Claude Code like the shim's.
fn run_wrapped(cmd: &str, input: &[u8]) -> Vec<u8> {
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env(NESTED_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let Ok(mut child) = child else {
        return Vec::new();
    };
    // Feed stdin from a thread so a command that prints before reading all
    // of it can't deadlock against a full pipe.
    let writer = child.stdin.take().map(|mut w| {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = w.write_all(&input);
        })
    });
    let out = child
        .wait_with_output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    if let Some(w) = writer {
        let _ = w.join();
    }
    out
}

/// The `--prefix` segment: `[work]`, `[work · Acme]` with a label, in the
/// profile's color unless `--no-color` or `NO_COLOR` is set (Claude Code
/// renders the escapes; stdout is never a terminal here).
pub fn segment(name: &str, label: Option<&str>, color: Option<&str>) -> String {
    let text = match label {
        Some(l) => format!("[{name} · {l}]"),
        None => format!("[{name}]"),
    };
    ui::paint_forced(&text, color)
}

/// `segment` and a space before the first line (just `segment` when there
/// is no output).
pub fn with_prefix(segment: &str, text: &str) -> String {
    if text.trim().is_empty() {
        segment.to_string()
    } else {
        format!("{segment} {text}")
    }
}

// ---------------------------------------------------------------------------
// Recognizing and writing the shim's statusLine
// ---------------------------------------------------------------------------

/// Whether a statusLine command is the mcc shim: an `mcc` binary (any
/// path, quoted or not) followed by `statusline` and no subcommand.
pub fn is_shim(cmd: &str) -> bool {
    let words = shell_words(cmd);
    let mut it = words.iter().map(String::as_str);
    let Some(bin) = it.next() else {
        return false;
    };
    let is_mcc = Path::new(bin).file_name().is_some_and(|n| n == "mcc");
    is_mcc
        && it.next() == Some("statusline")
        && !matches!(it.next(), Some("install" | "uninstall" | "help"))
}

/// Split a command line on unquoted whitespace, removing quotes. Enough to
/// read back the commands this module writes, plus hand-typed variants.
fn shell_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '"' => {
                has = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => cur.extend(chars.next()),
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                has = true;
                cur.extend(chars.next());
            }
            c if c.is_whitespace() => {
                if has {
                    words.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        words.push(cur);
    }
    words
}

/// The command written as `statusLine.command`.
pub fn shim_command(bin: &Path, prefix: bool) -> String {
    let bin = bin.to_string_lossy();
    let bin = if bin
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        bin.into_owned()
    } else {
        crate::shell::sh_quote(&bin)
    };
    if prefix {
        format!("{bin} statusline --prefix")
    } else {
        format!("{bin} statusline")
    }
}

/// The mcc to call from settings: the `mcc` on PATH when it is this binary
/// (a stable path such as Homebrew's, which survives upgrades), else this
/// executable's own path.
fn shim_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the mcc binary")?;
    let real = std::fs::canonicalize(&exe).unwrap_or_else(|_| exe.clone());
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join("mcc");
            if fsutil::is_executable(&cand) && std::fs::canonicalize(&cand).is_ok_and(|c| c == real)
            {
                return Ok(cand);
            }
        }
    }
    Ok(exe)
}

// ---------------------------------------------------------------------------
// The statusLine record
// ---------------------------------------------------------------------------

/// What `install` replaced in one profile.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// The settings file the shim was written to.
    pub settings: PathBuf,
    /// The previous `statusLine` value (`None`: there was none).
    pub original: Option<Value>,
    pub installed_at: String,
}

fn record_path(paths: &Paths, profile: &str) -> PathBuf {
    paths.profile_state_dir(profile).join(RECORD_FILE)
}

fn read_record(paths: &Paths, profile: &str) -> Option<Record> {
    let bytes = std::fs::read(record_path(paths, profile)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_json(path: &Path, v: &Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(v).context("serializing JSON")?;
    text.push('\n');
    fsutil::write_atomic(path, text.as_bytes())
}

/// Copy a settings file to `~/.mcc/backups/` before changing it; never
/// overwrites an earlier backup. Returns where it went.
fn backup_settings(paths: &Paths, profile: &str, settings: &Path) -> Result<Option<PathBuf>> {
    let bytes = match std::fs::read(settings) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", settings.display())),
    };
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
    let base = format!("settings.json.{profile}.{stamp}");
    let mut dest = paths.backups_dir.join(&base);
    let mut n = 1;
    while dest.exists() {
        dest = paths.backups_dir.join(format!("{base}.{n}"));
        n += 1;
    }
    fsutil::write_atomic(&dest, &bytes)?;
    Ok(Some(dest))
}

/// A settings file as a JSON object (`{}` when missing).
fn read_settings(path: &Path) -> Result<Map<String, Value>> {
    match copy::read_json(path)? {
        None => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m),
        Some(_) => Err(
            CliError::input(format!("{} is not a JSON object", path.display()))
                .hint("fix the file by hand, then retry")
                .into(),
        ),
    }
}

/// Write a settings file in place. A symlinked file (e.g. from a dotfiles
/// manager) stays a symlink: its target is written instead.
fn write_settings(path: &Path, settings: &Map<String, Value>) -> Result<()> {
    write_json(&link_target(path), &Value::Object(settings.clone()))
}

/// The file a symlink finally points at (`path` itself when not a link).
fn link_target(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    // Bounded, so a symlink loop can't spin forever.
    for _ in 0..40 {
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(&p) {
                Ok(t) if t.is_absolute() => p = t,
                Ok(t) => p = p.parent().unwrap_or(Path::new("/")).join(t),
                Err(_) => break,
            },
            _ => break,
        }
    }
    p
}

/// Whether the settings file at `path` sets the shim as its statusLine
/// (`false` when it is missing or unreadable).
pub(crate) fn shim_set_in(path: &Path) -> bool {
    read_settings(path)
        .ok()
        .and_then(|s| statusline_command(&s).map(is_shim))
        .unwrap_or(false)
}

fn statusline_command(settings: &Map<String, Value>) -> Option<&str> {
    settings.get("statusLine")?.get("command")?.as_str()
}

// ---------------------------------------------------------------------------
// install / uninstall
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Installed {
    /// The shim replaced the previous statusLine (backed up to the path).
    New(Option<PathBuf>),
    /// The shim was there with other flags; only its command changed.
    Updated(Option<PathBuf>),
    /// The shim was already there, exactly.
    Already,
}

/// Put the shim into one profile's `settings.json`, never wrapping it twice.
fn install_into(
    paths: &Paths,
    profile: &str,
    settings_path: &Path,
    cmd: &str,
) -> Result<Installed> {
    let mut settings = read_settings(settings_path)?;
    let current = settings.get("statusLine").cloned();
    if let Some(existing) = statusline_command(&settings).filter(|c| is_shim(c)) {
        if existing == cmd {
            return Ok(Installed::Already);
        }
        let backup = backup_settings(paths, profile, settings_path)?;
        if let Some(Value::Object(sl)) = settings.get_mut("statusLine") {
            sl.insert("command".into(), Value::String(cmd.into()));
        }
        write_settings(settings_path, &settings)?;
        return Ok(Installed::Updated(backup));
    }
    let backup = backup_settings(paths, profile, settings_path)?;
    let record = Record {
        settings: settings_path.to_path_buf(),
        original: current.clone(),
        installed_at: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
    };
    write_json(
        &record_path(paths, profile),
        &serde_json::to_value(&record).context("serializing the statusLine record")?,
    )?;
    // Keep the previous statusLine's other fields (such as `padding`).
    let mut sl = match current {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    sl.insert("type".into(), Value::String("command".into()));
    sl.insert("command".into(), Value::String(cmd.into()));
    settings.insert("statusLine".into(), Value::Object(sl));
    write_settings(settings_path, &settings)?;
    Ok(Installed::New(backup))
}

fn install(paths: &Paths, cfg: &Config, prefix: bool, yes: bool) -> Result<()> {
    ui::header("Install the mcc statusline");
    if !paths.default_dir.is_dir() {
        return Err(CliError::usage(format!(
            "the default profile ({}) doesn't exist",
            paths.display(&paths.default_dir)
        ))
        .hint("run `claude` once (or `mcc add`) to create it")
        .into());
    }
    let cmd = shim_command(&shim_binary()?, prefix);
    let settings = paths.default_dir.join("settings.json");
    let outcome = install_into(paths, DEFAULT_PROFILE, &settings, &cmd)?;
    report(paths, "default profile", &settings, &outcome);

    let excluded = cfg.sync.exclude.iter().any(|g| g == "settings");
    let mut warnings = 0;
    let profiles: Vec<(&String, &crate::config::Profile)> = cfg
        .profiles
        .iter()
        .filter(|(_, p)| p.dir.is_dir())
        .collect();
    if !excluded {
        if !profiles.is_empty() {
            ui::hint(
                "profiles get it with their settings on the next `mcc sync` (e.g. `mcc sync --all --only settings`)",
            );
        }
    } else {
        let mut skipped = Vec::new();
        for (alias, p) in &profiles {
            let path = p.dir.join("settings.json");
            let already = read_settings(&path)
                .ok()
                .and_then(|s| statusline_command(&s).map(|c| c == cmd))
                .unwrap_or(false);
            if already {
                report(
                    paths,
                    &format!("profile \"{alias}\""),
                    &path,
                    &Installed::Already,
                );
                continue;
            }
            let add = if yes {
                true
            } else if ui::is_interactive() {
                ui::confirm(
                    &format!(
                        "\"{alias}\" doesn't sync settings ([sync] exclude). Add the statusline to {} too?",
                        paths.display(&path)
                    ),
                    true,
                    "pass --yes",
                )?
            } else {
                false
            };
            if add {
                match install_into(paths, alias, &path, &cmd) {
                    Ok(outcome) => report(paths, &format!("profile \"{alias}\""), &path, &outcome),
                    Err(e) => {
                        warnings += 1;
                        ui::warn(&format!("profile \"{alias}\": {e:#} — not added"));
                    }
                }
            } else {
                skipped.push(alias.as_str());
            }
        }
        if !skipped.is_empty() {
            ui::hint(&format!(
                "not added to {} (settings aren't synced); run `mcc statusline install --yes` to add it",
                skipped.join(", ")
            ));
        }
    }
    ui::result(
        warnings == 0,
        &if warnings == 0 {
            "Claude Code shows the statusline from its next refresh".to_string()
        } else {
            format!("statusline installed with {warnings} warning(s) — see above")
        },
    );
    ui::kv(
        "Rate limits",
        &format!("{}/<profile>/{USAGE_FILE}", paths.display(&paths.state_dir)),
    );
    ui::hint("undo with `mcc statusline uninstall`");
    Ok(())
}

fn report(paths: &Paths, who: &str, settings: &Path, outcome: &Installed) {
    let backup = |b: &Option<PathBuf>| match b {
        Some(b) => format!(" (backup: {})", paths.display(b)),
        None => String::new(),
    };
    match outcome {
        Installed::New(b) => ui::success(&format!(
            "{who}: set statusLine in {}{}",
            paths.display(settings),
            backup(b)
        )),
        Installed::Updated(b) => ui::success(&format!(
            "{who}: updated the statusline in {}{}",
            paths.display(settings),
            backup(b)
        )),
        Installed::Already => ui::info(&format!(
            "{who}: already installed in {}",
            paths.display(settings)
        )),
    }
}

fn uninstall(paths: &Paths, cfg: &Config) -> Result<()> {
    ui::header("Uninstall the mcc statusline");
    let default_record = read_record(paths, DEFAULT_PROFILE);
    let mut targets: Vec<(String, PathBuf, Option<PathBuf>)> = vec![(
        DEFAULT_PROFILE.to_string(),
        paths.default_dir.join("settings.json"),
        None,
    )];
    for (alias, p) in &cfg.profiles {
        targets.push((
            alias.clone(),
            p.dir.join("settings.json"),
            Some(p.dir.clone()),
        ));
    }

    let mut restored = 0;
    let mut warnings = 0;
    // Records still needed because a target couldn't be read: dropping them
    // would lose the only copy of its previous statusLine.
    let mut keep: Vec<&str> = Vec::new();
    for (name, settings_path, dir) in &targets {
        let who = if dir.is_some() {
            format!("profile \"{name}\"")
        } else {
            "default profile".to_string()
        };
        let own = read_record(paths, name);
        if !settings_path.exists() {
            continue;
        }
        let mut settings = match read_settings(settings_path) {
            Ok(s) => s,
            Err(e) => {
                ui::warn(&format!(
                    "{who}: {e:#} — left as is; fix it and run `mcc statusline uninstall` again"
                ));
                warnings += 1;
                keep.push(name);
                if dir.is_some() {
                    keep.push(DEFAULT_PROFILE);
                }
                continue;
            }
        };
        let Some(current) = statusline_command(&settings) else {
            if own.is_some() {
                ui::warn(&format!(
                    "{who}: statusLine in {} was changed since install — left as is",
                    paths.display(settings_path)
                ));
                warnings += 1;
            }
            continue;
        };
        if !is_shim(current) {
            if own.is_some() {
                ui::warn(&format!(
                    "{who}: statusLine in {} was changed since install — left as is",
                    paths.display(settings_path)
                ));
                warnings += 1;
            }
            continue;
        }
        // A profile that got the shim through `mcc sync` gets the default's
        // previous statusLine back, rewritten as sync would.
        let original = match (&own, dir, &default_record) {
            (Some(rec), _, _) => Some(rec.original.clone()),
            (None, Some(dir), Some(rec)) => Some(rec.original.as_ref().map(|v| {
                let text = serde_json::to_string(v).unwrap_or_default();
                serde_json::from_str(&Rewriter::new(paths, dir, &cfg.sync).apply(&text))
                    .unwrap_or_else(|_| v.clone())
            })),
            _ => None,
        };
        let backup = backup_settings(paths, name, settings_path)?;
        match &original {
            Some(Some(v)) => {
                settings.insert("statusLine".into(), v.clone());
            }
            _ => {
                settings.remove("statusLine");
            }
        }
        write_settings(settings_path, &settings)?;
        restored += 1;
        let what = match &original {
            Some(Some(_)) => "restored the previous statusLine",
            Some(None) => "removed the statusline (there was none before)",
            None => {
                warnings += 1;
                "removed the statusline (the previous one wasn't recorded)"
            }
        };
        ui::success(&format!(
            "{who}: {what} in {}{}",
            paths.display(settings_path),
            backup
                .map(|b| format!(" (backup: {})", paths.display(&b)))
                .unwrap_or_default()
        ));
    }
    for (name, _, _) in targets
        .iter()
        .filter(|(n, _, _)| !keep.contains(&n.as_str()))
    {
        let rec = record_path(paths, name);
        if rec.exists() {
            fsutil::remove_any(&rec)?;
        }
    }
    if restored == 0 && warnings == 0 {
        ui::result(true, "the mcc statusline isn't installed — nothing to do");
    } else {
        ui::result(
            warnings == 0,
            &format!(
                "statusline uninstalled{}",
                if warnings > 0 {
                    format!(" with {warnings} warning(s) — see above")
                } else {
                    String::new()
                }
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Profile;

    fn paths() -> Paths {
        Paths::new(PathBuf::from("/h"))
    }

    #[test]
    fn profile_from_config_dir() {
        let p = paths();
        let mut cfg = Config::default();
        cfg.profiles.insert(
            "job".into(),
            Profile {
                dir: PathBuf::from("/h/.mcc/profiles/work"),
                mode: "clone".into(),
                created_at: String::new(),
                wrapper: None,
                rc_file: None,
                last_sync: None,
                share_sessions: false,
                share_note_acknowledged_at: None,
                share_group: None,
                share_cross_org_allowed_at: None,
                label: None,
                color: None,
                account: None,
                settings: None,
                link: Vec::new(),
            },
        );
        let f = |d: Option<&str>| profile_for(&p, &cfg, d.map(OsStr::new));
        assert_eq!(f(None).as_deref(), Some("default"));
        assert_eq!(f(Some("")).as_deref(), Some("default"));
        assert_eq!(f(Some("/h/.claude")).as_deref(), Some("default"));
        assert_eq!(f(Some("/h/.claude/")).as_deref(), Some("default"));
        assert_eq!(f(Some("~/.claude")).as_deref(), Some("default"));
        // A renamed profile keeps its directory: the alias wins.
        assert_eq!(f(Some("/h/.mcc/profiles/work")).as_deref(), Some("job"));
        assert_eq!(f(Some("/h/.mcc/profiles/lab/")).as_deref(), Some("lab"));
        assert_eq!(f(Some("/elsewhere/x")), None);
        assert_eq!(f(Some("/h/.mcc/profiles/.hidden")), None);
        assert_eq!(f(Some("/")), None);
    }

    #[test]
    fn usage_record_keeps_the_two_windows() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-20T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let status = json!({
            "model": {"id": "x"},
            "rate_limits": {
                "five_hour": {"used_percentage": 23.5, "resets_at": 1792500000, "extra": 1},
                "seven_day": {"used_percentage": 41}
            }
        });
        let rec = usage_record("work", &status, now).unwrap();
        assert_eq!(
            rec,
            json!({
                "profile": "work",
                "updated_at": "2026-10-20T10:00:00Z",
                "five_hour": {"used_percentage": 23.5, "resets_at": 1792500000},
                "seven_day": {"used_percentage": 41, "resets_at": null}
            })
        );
        assert!(usage_record("w", &json!({"model": {}}), now).is_none());
        assert!(usage_record("w", &json!({"rate_limits": {}}), now).is_none());
        assert!(usage_record("w", &json!({"rate_limits": null}), now).is_none());
    }

    #[test]
    fn shim_detection() {
        assert!(is_shim("mcc statusline"));
        assert!(is_shim("/opt/homebrew/bin/mcc statusline --prefix"));
        assert!(is_shim("'/Users/me/my tools/mcc' statusline"));
        assert!(is_shim("  \"/x/mcc\"   statusline"));
        assert!(!is_shim("mcc statusline install"));
        assert!(!is_shim("~/.claude/statusline.sh"));
        assert!(!is_shim("bash -c 'mcc statusline'"));
        assert!(!is_shim("mccx statusline"));
        assert!(!is_shim(""));
        for (bin, prefix) in [("/usr/local/bin/mcc", false), ("/a b/it's/mcc", true)] {
            let cmd = shim_command(Path::new(bin), prefix);
            assert!(is_shim(&cmd), "{cmd}");
            assert_eq!(shell_words(&cmd)[0], bin);
            assert_eq!(cmd.ends_with("--prefix"), prefix);
        }
    }

    #[test]
    fn prefix() {
        let work = segment("work", None, None);
        assert_eq!(work, "[work]");
        assert_eq!(
            with_prefix(&work, "Opus 5 | 12%\n"),
            "[work] Opus 5 | 12%\n"
        );
        assert_eq!(with_prefix(&work, "a\nb"), "[work] a\nb");
        assert_eq!(with_prefix(&work, ""), "[work]");
        assert_eq!(with_prefix(&work, " \n"), "[work]");
        assert_eq!(
            segment("work", Some("Acme (work)"), None),
            "[work · Acme (work)]"
        );
    }
}
