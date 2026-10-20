//! `mcc config get|set|edit|path` — read and change `~/.mcc/config.toml`
//! without hand-editing TOML.
//!
//! `set` accepts a fixed list of keys ([`SETTABLE`]), checks the value's type
//! and meaning, and saves through [`Config::save`], so comments and
//! formatting survive. `edit` opens `$VISUAL`/`$EDITOR` on a copy and keeps
//! the result only when it is valid mcc config. A rejected value or edit
//! leaves the file as it was.
//!
//! Locking: `set` holds `~/.mcc/.lock` for the whole run (taken in
//! `main.rs`). `edit` does not hold it while the editor is open, since that
//! would block every other mcc command (launches with `--mcc-share`
//! included) for as long as the editor stays open. It takes the lock only to
//! save, and refuses to save when another command changed the file in the
//! meantime, keeping the edited copy for you. `get` and `path` never lock.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};

use crate::cli::{ConfigAction, ConfigArgs};
use crate::config::{self, Config};
use crate::error::{CliError, EXIT_INTERRUPTED};
use crate::paths::Paths;
use crate::{claude, copy, ui};

/// The keys `mcc config set` accepts. `ALIAS` stands for a profile's name.
pub const SETTABLE: &[(&str, &str)] = &[
    (
        "sync.exclude",
        "sync items to skip, comma-separated (\"\" for none)",
    ),
    (
        "sync.extra",
        "extra paths to sync, relative to the config dir, comma-separated (\"\" for none)",
    ),
    ("privacy.show_email", "true or false"),
    ("usage.live", "true or false"),
    ("launch.banner", "true or false"),
    (
        "profiles.ALIAS.label",
        "a label of 1–64 characters (\"\" removes it)",
    ),
    (
        "profiles.ALIAS.color",
        "black, red, green, yellow, blue, magenta, cyan or white (\"\" removes it)",
    ),
];

/// A key `mcc config set` accepts, parsed.
#[derive(Debug, PartialEq)]
enum Key {
    SyncExclude,
    SyncExtra,
    ShowEmail,
    Live,
    Banner,
    Label(String),
    Color(String),
}

pub fn run(paths: &Paths, args: &ConfigArgs) -> Result<()> {
    match &args.action {
        ConfigAction::Path => {
            println!("{}", paths.config_file.display());
            Ok(())
        }
        ConfigAction::Get { key } => get(paths, key),
        ConfigAction::Set { key, value } => set(paths, key, value),
        ConfigAction::Edit => edit(paths),
    }
}

/// `KEY` split into its parts, as TOML reads a dotted key
/// (`projects."/Users/me/app".profile`).
fn split_key(key: &str) -> Result<Vec<String>> {
    toml_edit::Key::parse(key)
        .map(|parts| parts.iter().map(|k| k.get().to_string()).collect())
        .map_err(|_| {
            CliError::usage(format!("invalid key \"{key}\""))
                .hint(settable_hint())
                .into()
        })
}

fn settable_hint() -> String {
    let keys: Vec<&str> = SETTABLE.iter().map(|(k, _)| *k).collect();
    format!("settable keys: {}", keys.join(", "))
}

/// The settable key `parts` names, or why it can't be set.
fn settable(key: &str, parts: &[String]) -> Result<Key> {
    let p: Vec<&str> = parts.iter().map(String::as_str).collect();
    let managed = |how: &str| -> anyhow::Error {
        CliError::usage(format!("\"{key}\" can't be set with `mcc config set`"))
            .hint(how.to_string())
            .into()
    };
    Ok(match p.as_slice() {
        ["sync", "exclude"] => Key::SyncExclude,
        ["sync", "extra"] => Key::SyncExtra,
        // `usage.show_email` is its name from before `[privacy]`.
        ["privacy" | "usage", "show_email"] => Key::ShowEmail,
        ["usage", "live"] => Key::Live,
        ["launch", "banner"] => Key::Banner,
        ["profiles", alias, "label"] => Key::Label(alias.to_string()),
        ["profiles", alias, "color"] => Key::Color(alias.to_string()),
        ["profiles", alias, f] if f.starts_with("share_") => {
            return Err(managed(&format!(
                "session sharing is set with `mcc share {alias}` (see `mcc help share`)"
            )));
        }
        ["profiles", alias, "link"] => {
            return Err(managed(&format!(
                "link mode is set with `mcc link {alias} ITEMS` (see `mcc help link`), which also creates the symlinks"
            )));
        }
        ["profiles", alias, "settings", ..] => {
            return Err(managed(&format!(
                "a settings overlay is a table of JSON settings: edit [profiles.{alias}.settings] with `mcc config edit`, or write the profile's settings.mcc.json"
            )));
        }
        ["profiles", _, ..] => {
            return Err(managed(
                "mcc keeps a profile's other keys itself; `mcc rename` renames a profile",
            ));
        }
        ["projects", ..] => {
            return Err(managed(
                "project defaults are set with `mcc project set` (see `mcc help project`)",
            ));
        }
        ["env", ..] => {
            return Err(managed(
                "the detected environment is updated with `mcc init --force`",
            ));
        }
        ["usage", "live_disclosed_at"] => {
            return Err(managed("mcc records it when it shows the live-usage note"));
        }
        _ => {
            return Err(CliError::usage(format!("unknown config key \"{key}\""))
                .hint(settable_hint())
                .into());
        }
    })
}

// ---------------------------------------------------------------- get

fn get(paths: &Paths, key: &str) -> Result<()> {
    let parts = split_key(key)?;
    let cfg = load(paths)?;
    let text = lookup(&cfg, key, &parts)?;
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())?;
    Ok(())
}

/// The value of `key` as `mcc config get` prints it: a string as is, a
/// number or boolean as TOML writes it, a list one item per line, a table
/// as TOML. Settable keys always have a value (their default when the file
/// leaves them out).
fn lookup(cfg: &Config, key: &str, parts: &[String]) -> Result<String> {
    let doc = toml::Value::try_from(cfg).context("serializing config")?;
    let mut node = Some(&doc);
    for part in parts {
        node = node.and_then(|n| n.get(part.as_str()));
    }
    let value = match (node, settable(key, parts)) {
        // Read through its old name too, and false while unset.
        (_, Ok(Key::ShowEmail)) => toml::Value::Boolean(cfg.show_email()),
        (Some(v), _) => v.clone(),
        // Left out of the file while it has its default.
        (None, Ok(Key::Live | Key::Banner)) => toml::Value::Boolean(false),
        (None, Ok(Key::Label(alias))) if cfg.profiles.contains_key(&alias) => {
            return Err(CliError::runtime(format!("\"{key}\" is not set"))
                .hint(format!("set it with `mcc config set {key} <LABEL>`"))
                .into());
        }
        (None, Ok(Key::Label(alias))) => return Err(super::unknown_profile(&alias)),
        (None, Ok(Key::Color(alias))) if cfg.profiles.contains_key(&alias) => {
            return Err(CliError::runtime(format!("\"{key}\" is not set"))
                .hint(format!("set it with `mcc config set {key} <COLOR>`"))
                .into());
        }
        (None, Ok(Key::Color(alias))) => return Err(super::unknown_profile(&alias)),
        (None, _) => {
            return Err(CliError::runtime(format!("\"{key}\" is not set"))
                .hint(format!(
                    "{}; `mcc config path` shows the file",
                    settable_hint()
                ))
                .into());
        }
    };
    Ok(render(&value))
}

fn render(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("{s}\n"),
        toml::Value::Array(items) => items.iter().map(render).collect(),
        toml::Value::Table(t) => toml::to_string_pretty(t).unwrap_or_default(),
        other => format!("{other}\n"),
    }
}

// ---------------------------------------------------------------- set

fn set(paths: &Paths, key: &str, value: &str) -> Result<()> {
    let parts = split_key(key)?;
    let which = settable(key, &parts)?;
    // Held since main.rs took the lock, so nothing changes it meanwhile.
    let mut cfg = load(paths)?;
    let banner = cfg.launch.banner;
    apply(&mut cfg, &which, value)?;
    cfg.save(&paths.config_file)?;
    crate::log::write("INFO", &format!("config: set {key} = {value:?}"));
    ui::success(&format!(
        "{key} set in {}",
        paths.display(&paths.config_file)
    ));
    // Launchers include the banner call only while it is on; mcc never
    // rewrites them on its own.
    if which == Key::Banner && cfg.launch.banner != banner && !cfg.profiles.is_empty() {
        ui::hint(&format!(
            "run `mcc refresh-launchers` so profiles' launchers {} the launch banner",
            if cfg.launch.banner {
                "show"
            } else {
                "stop calling"
            }
        ));
    }
    Ok(())
}

/// Set `which` to `value` in `cfg`, or fail (leaving `cfg` unusable for
/// saving: callers drop it) when the value is invalid.
fn apply(cfg: &mut Config, which: &Key, value: &str) -> Result<()> {
    match which {
        Key::SyncExclude => {
            let mut list: Vec<String> = Vec::new();
            for item in split_list(value) {
                if !list.contains(&item) {
                    list.push(item);
                }
            }
            cfg.sync.exclude = list;
            copy::spec(None, &cfg.sync)?;
        }
        Key::SyncExtra => {
            cfg.sync.extra = split_list(value);
            copy::spec(None, &cfg.sync)?;
        }
        Key::ShowEmail => cfg.privacy.show_email = Some(parse_bool(value)?),
        Key::Live => cfg.usage.live = parse_bool(value)?,
        Key::Banner => cfg.launch.banner = parse_bool(value)?,
        Key::Label(alias) => {
            let profile = cfg
                .profiles
                .get_mut(alias)
                .ok_or_else(|| super::unknown_profile(alias))?;
            let trimmed = value.trim();
            profile.label = if trimmed.is_empty() {
                None
            } else {
                super::add::check_label(trimmed).map_err(|m| {
                    CliError::usage(format!("invalid label: {m}"))
                        .hint("example: mcc config set profiles.work.label \"Acme (work)\"")
                })?;
                Some(trimmed.to_string())
            };
        }
        Key::Color(alias) => {
            let profile = cfg
                .profiles
                .get_mut(alias)
                .ok_or_else(|| super::unknown_profile(alias))?;
            profile.color = if value.trim().is_empty() {
                None
            } else {
                Some(ui::check_color(value).map_err(|m| {
                    CliError::usage(format!("invalid color: {m}"))
                        .hint("example: mcc config set profiles.work.color blue")
                })?)
            };
        }
    }
    Ok(())
}

/// A comma-separated list: items trimmed, empty ones dropped (so `""` is
/// the empty list).
fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(
            CliError::usage(format!("invalid value \"{value}\": expected true or false")).into(),
        ),
    }
}

fn load(paths: &Paths) -> Result<Config> {
    Config::load(&paths.config_file).map_err(|e| {
        let (code, _) = crate::error::classify(&e);
        CliError {
            code,
            message: format!("{e:#}"),
            hint: Some("fix it with `mcc config edit`".into()),
        }
        .into()
    })
}

// ---------------------------------------------------------------- edit

/// Check edited config text: it must load as mcc config and pass the
/// checks the commands that set those keys make.
pub fn check(text: &str) -> Result<Config> {
    let cfg: Config = toml::from_str(text)
        .map_err(|e| CliError::input(format!("not valid mcc config: {}", e.message().trim())))?;
    copy::spec(None, &cfg.sync)?;
    for (alias, p) in &cfg.profiles {
        if let Some(label) = &p.label {
            super::add::check_label(label)
                .map_err(|m| CliError::input(format!("profiles.{alias}.label: {m}")))?;
        }
        if let Some(color) = &p.color {
            ui::check_color(color)
                .map_err(|m| CliError::input(format!("profiles.{alias}.color: {m}")))?;
        }
        super::link::check_items(&p.link)
            .map_err(|e| CliError::input(format!("profiles.{alias}.link: {e}")))?;
        if let Some(group) = &p.share_group {
            super::share::check_group(group)
                .map_err(|e| CliError::input(format!("profiles.{alias}.share_group: {e}")))?;
        }
    }
    Ok(cfg)
}

fn edit(paths: &Paths) -> Result<()> {
    let file = &paths.config_file;
    let original = match std::fs::read_to_string(file) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let start = original.clone().unwrap_or_else(|| {
        "# mcc configuration — managed by `mcc`; safe to edit by hand.\n".to_string()
    });
    let copy = edit_copy_path(file);
    write_private(&copy, &start)?;
    let edited = loop {
        if let Err(e) = run_editor(&copy) {
            let _ = std::fs::remove_file(&copy);
            return Err(e);
        }
        let edited = std::fs::read_to_string(&copy)
            .with_context(|| format!("reading {}", copy.display()))?;
        if Some(&edited) == original.as_ref() || (original.is_none() && edited == start) {
            let _ = std::fs::remove_file(&copy);
            ui::info("no changes");
            return Ok(());
        }
        match check(&edited) {
            Ok(_) => break edited,
            Err(e) => {
                let again = ui::is_interactive() && {
                    ui::error(&format!("{e:#}"), None);
                    ui::confirm("Edit again? (no keeps config.toml unchanged)", true, "")?
                };
                if !again {
                    let _ = std::fs::remove_file(&copy);
                    return Err(e.context(format!("{} left unchanged", paths.display(file))));
                }
            }
        }
    };

    // Saving: take the lock, and keep what another command wrote meanwhile.
    let _lock = config::lock(paths)?;
    let now = match std::fs::read_to_string(file) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    if now != original {
        return Err(CliError::runtime(format!(
            "{} changed while you were editing it; it was left as is",
            paths.display(file)
        ))
        .hint(format!(
            "your edits are in {}: run `mcc config edit` again and copy them over",
            paths.display(&copy)
        ))
        .into());
    }
    crate::fsutil::write_atomic(file, edited.as_bytes())?;
    let _ = std::fs::remove_file(&copy);
    crate::log::write("INFO", "config: edited by hand with `mcc config edit`");
    ui::success(&format!("saved {}", paths.display(file)));
    Ok(())
}

/// Where `edit` keeps the copy being edited: next to the config, so it
/// never leaves `~/.mcc`, and ending in `.toml` for editors' syntax modes.
fn edit_copy_path(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("."));
    dir.join(format!("config.edit-{}.toml", std::process::id()))
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

/// `$VISUAL`, else `$EDITOR`, else `vi`. It may carry arguments
/// (`code --wait`), so the shell runs it.
fn editor() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string())
}

fn run_editor(file: &Path) -> Result<()> {
    let editor = editor();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("mcc-config-edit")
        .arg(file);
    claude::CHILD_RUNNING.store(true, Ordering::SeqCst);
    let status = cmd.status();
    claude::CHILD_RUNNING.store(false, Ordering::SeqCst);
    let interrupted = claude::INTERRUPTED.swap(false, Ordering::SeqCst)
        || matches!(&status, Ok(s) if s.signal() == Some(libc::SIGINT));
    if interrupted {
        let _ = std::fs::remove_file(file);
        ui::warn("interrupted: config.toml left unchanged");
        std::process::exit(EXIT_INTERRUPTED);
    }
    let status = status.with_context(|| format!("starting the editor `{editor}`"))?;
    if !status.success() {
        return Err(CliError::runtime(format!(
            "the editor `{editor}` exited with {status}; config.toml left unchanged"
        ))
        .hint("set $VISUAL or $EDITOR to the editor to use")
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Result<Key> {
        settable(k, &split_key(k)?)
    }

    #[test]
    fn settable_keys_parse() {
        assert_eq!(key("sync.exclude").unwrap(), Key::SyncExclude);
        assert_eq!(key("usage.live").unwrap(), Key::Live);
        assert_eq!(key("launch.banner").unwrap(), Key::Banner);
        assert_eq!(
            key("profiles.work.label").unwrap(),
            Key::Label("work".into())
        );
        assert_eq!(
            key("profiles.\"a.b\".label").unwrap(),
            Key::Label("a.b".into())
        );
        for k in [
            "profiles.work.share_group",
            "profiles.work.dir",
            "projects.\"/x\".profile",
            "env.shell",
            "nope",
            "usage.live_disclosed_at",
        ] {
            assert!(key(k).is_err(), "{k}");
        }
        assert!(split_key("a..b").is_err());
    }

    #[test]
    fn bools_and_lists() {
        assert!(parse_bool("Yes").unwrap());
        assert!(!parse_bool("off").unwrap());
        assert!(parse_bool("maybe").is_err());
        assert_eq!(split_list(" plugins, ,mcp "), vec!["plugins", "mcp"]);
        assert!(split_list("").is_empty());
    }

    #[test]
    fn apply_validates() {
        let mut cfg = Config::default();
        apply(&mut cfg, &Key::SyncExclude, "plugins,plugins,mcp").unwrap();
        assert_eq!(cfg.sync.exclude, vec!["plugins", "mcp"]);
        assert!(apply(&mut cfg, &Key::SyncExclude, "nope").is_err());
        assert!(apply(&mut cfg, &Key::SyncExtra, "../x").is_err());
        assert!(apply(&mut cfg, &Key::Label("work".into()), "x").is_err());
    }

    #[test]
    fn check_rejects_invalid_config() {
        assert!(check("[usage]\nlive = true\n").is_ok());
        assert!(check("[usage\n").is_err());
        assert!(check("[usage]\nlive = \"yes\"\n").is_err());
        assert!(check("[sync]\nexclude = [\"nope\"]\n").is_err());
        let profile = "[profiles.w]\ndir = \"/d\"\nmode = \"clone\"\ncreated_at = \"t\"\n";
        assert!(check(profile).is_ok());
        assert!(check(&format!("{profile}share_group = \"../x\"\n")).is_err());
        assert!(check(&format!("{profile}label = \"\"\n")).is_err());
    }

    #[test]
    fn render_values() {
        assert_eq!(render(&toml::Value::String("a b".into())), "a b\n");
        assert_eq!(render(&toml::Value::Boolean(true)), "true\n");
        let list = toml::Value::Array(vec!["x".into(), "y".into()]);
        assert_eq!(render(&list), "x\ny\n");
    }
}
