//! `mcc doctor` — check every profile's setup and print a fix for each problem.
//!
//! Read-only: doctor never writes `~/.mcc/config.toml` (not even the first-run
//! setup), never takes the config lock, and changes no file. Every finding
//! has a stable check id, a status and, for warnings and problems, a fix: a
//! command to run or, where no command does it, what to change by hand.
//!
//! Tokens: whether a profile is logged in comes from `claude auth status`
//! (Claude Code reading its own login). mcc reads a token itself, to see
//! whether it has expired, only with `[usage] live = true` and `MCC_NO_USAGE`
//! unset, the setting that already lets it read tokens; the token never
//! leaves [`usage::token_state`].

use std::path::{Path, PathBuf};

use anyhow::Result;
use regex::Regex;
use serde::Serialize;

use crate::claude::{self, AuthStatus, Claude};
use crate::cli::DoctorArgs;
use crate::config::{Config, Profile};
use crate::error::CliError;
use crate::paths::Paths;
use crate::{copy, env_detect, fsutil, manifest, plugins, shell, ui, usage, validate};

use super::share::{self, Projects};
use super::statusline;

/// How a check went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    /// Worth fixing, but nothing is broken: doesn't fail the command.
    Warn,
    /// Broken: mcc exits 1.
    Problem,
    /// Not checked, and why (e.g. needs a newer Claude Code).
    Skipped,
}

/// One check's result. Serialized as is for `--json`.
#[derive(Debug, Serialize)]
pub struct Finding {
    /// The profile checked; `null` for the checks every profile shares.
    pub profile: Option<String>,
    /// Stable id of the check, e.g. `launcher` or `rc_config_dir`.
    pub check: &'static str,
    pub status: Level,
    pub message: String,
    /// What fixes it: a command, or what to change by hand.
    pub fix: Option<String>,
    /// Whether `fix` is a command (shown in backticks).
    #[serde(skip)]
    command: bool,
}

/// The variable mcc profiles are told apart by.
const CONFIG_DIR_VAR: &str = "CLAUDE_CONFIG_DIR";

struct Doctor<'a> {
    paths: &'a Paths,
    cfg: &'a Config,
    claude_bin: Option<PathBuf>,
    claude_version: Option<String>,
    /// Token expiry may be checked: `[usage] live = true`, MCC_NO_USAGE unset.
    read_tokens: bool,
    profile: Option<String>,
    out: Vec<Finding>,
}

impl Doctor<'_> {
    fn push(&mut self, check: &'static str, status: Level, message: String) {
        self.out.push(Finding {
            profile: self.profile.clone(),
            check,
            status,
            message,
            fix: None,
            command: false,
        });
    }

    /// A finding fixed by running `cmd`.
    fn push_cmd(&mut self, check: &'static str, status: Level, message: String, cmd: String) {
        self.push(check, status, message);
        let f = self.out.last_mut().expect("just pushed");
        f.fix = Some(cmd);
        f.command = true;
    }

    /// A finding fixed by hand, as `how` says.
    fn push_manual(&mut self, check: &'static str, status: Level, message: String, how: String) {
        self.push(check, status, message);
        self.out.last_mut().expect("just pushed").fix = Some(how);
    }

    fn show(&self, p: &Path) -> String {
        self.paths.display(p)
    }
}

pub fn run(paths: &Paths, cfg: &Config, args: &DoctorArgs) -> Result<()> {
    let (default, names) = selected(cfg, args.profile.as_deref())?;
    let claude_bin = claude::find_binary(cfg.env.as_ref().and_then(|e| e.claude_bin.as_deref()));
    let mut d = Doctor {
        paths,
        cfg,
        claude_version: claude_bin.as_deref().and_then(claude::version),
        claude_bin,
        read_tokens: cfg.usage.live && !usage::disabled_by_env(),
        profile: None,
        out: Vec::new(),
    };

    check_setup(&mut d);
    check_path(&mut d);
    check_rc_files(&mut d);
    check_environment(&mut d);
    if default {
        d.profile = Some(statusline::DEFAULT_PROFILE.into());
        check_default(&mut d);
    }
    for name in &names {
        d.profile = Some(name.clone());
        check_profile(&mut d, name, &cfg.profiles[name]);
    }

    let findings = d.out;
    if args.json || args.json_v2 {
        super::write_json(
            &mut std::io::stdout().lock(),
            "checks",
            &findings,
            args.json_v2,
        )?;
    } else {
        print(paths, &findings);
    }
    let count = |l: Level| findings.iter().filter(|f| f.status == l).count();
    let (problems, warnings) = (count(Level::Problem), count(Level::Warn));
    if problems > 0 {
        return Err(CliError::runtime(format!(
            "{} found{}",
            plural(problems, "problem"),
            if warnings > 0 {
                format!(" (and {})", plural(warnings, "warning"))
            } else {
                String::new()
            }
        ))
        .hint("apply the fixes above, then run `mcc doctor` again")
        .into());
    }
    if !(args.json || args.json_v2) {
        ui::result(
            true,
            &if warnings > 0 {
                format!("no problems; {} (fixes above)", plural(warnings, "warning"))
            } else {
                "no problems found".into()
            },
        );
    }
    Ok(())
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// Whether to check the default profile, and which mcc profiles.
fn selected(cfg: &Config, only: Option<&str>) -> Result<(bool, Vec<String>)> {
    match only {
        None => Ok((true, cfg.profiles.keys().cloned().collect())),
        Some("default" | "claude") => Ok((true, Vec::new())),
        Some(alias) if cfg.profiles.contains_key(alias) => Ok((false, vec![alias.to_string()])),
        Some(alias) => Err(super::unknown_profile(alias)),
    }
}

/// The findings on stderr, grouped under one header per profile.
fn print(paths: &Paths, findings: &[Finding]) {
    let mut current: Option<Option<&str>> = None;
    for f in findings {
        if current != Some(f.profile.as_deref()) {
            current = Some(f.profile.as_deref());
            ui::header(&match f.profile.as_deref() {
                None => "mcc doctor: setup".to_string(),
                Some(statusline::DEFAULT_PROFILE) => format!(
                    "Profile \"default\" ({})",
                    paths.display(&paths.default_dir)
                ),
                Some(name) => format!("Profile \"{name}\""),
            });
        }
        match f.status {
            Level::Ok => ui::success(&f.message),
            Level::Warn => ui::warn(&f.message),
            Level::Problem => ui::problem(&f.message),
            Level::Skipped => ui::info(&format!("skipped: {}", f.message)),
        }
        match (&f.fix, f.command) {
            (Some(cmd), true) => ui::hint(&format!("fix: `{cmd}`")),
            (Some(how), false) => {
                let mut lines = how.splitn(2, '\n');
                ui::hint(&format!("fix: {}", lines.next().unwrap_or_default()));
                if let Some(rest) = lines.next() {
                    ui::snippet(rest);
                }
            }
            (None, _) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Checks every profile shares
// ---------------------------------------------------------------------------

/// mcc set up (`[env]` recorded) and claude found.
fn check_setup(d: &mut Doctor) {
    match &d.cfg.env {
        None => d.push_cmd(
            "init",
            Level::Problem,
            format!(
                "mcc is not set up yet (no [env] in {})",
                d.show(&d.paths.config_file)
            ),
            "mcc init".into(),
        ),
        Some(env) => {
            let msg = match (env.shell, &env.rc_file) {
                (Some(sh), Some(rc)) => format!("mcc is set up for {} ({})", sh.name(), d.show(rc)),
                _ => "mcc is set up (shell not supported: no rc file is managed)".into(),
            };
            d.push("init", Level::Ok, msg);
        }
    }
    match &d.claude_bin {
        None => d.push_manual(
            "claude",
            Level::Problem,
            "the `claude` command was not found on your PATH".into(),
            "install Claude Code (https://docs.claude.com/en/docs/claude-code) or set MCC_CLAUDE_BIN=/path/to/claude".into(),
        ),
        Some(bin) => {
            let msg = match &d.claude_version {
                Some(v) => format!("Claude Code {v} at {}", d.show(bin)),
                None => format!("Claude Code at {}", d.show(bin)),
            };
            d.push("claude", Level::Ok, msg);
        }
    }
}

/// `~/.local/bin`, where launchers live, on PATH (in this shell).
fn check_path(d: &mut Doctor) {
    let local_bin = d.show(&d.paths.local_bin);
    if env_detect::dir_on_path(&d.paths.local_bin) {
        d.push("path", Level::Ok, format!("{local_bin} is on PATH"));
        return;
    }
    // Launchers are what need it; without any, it is only worth a warning.
    let level = if d.cfg.profiles.values().any(|p| p.wrapper.is_some()) {
        Level::Problem
    } else {
        Level::Warn
    };
    let msg = format!("{local_bin} (where profile launchers live) is not on PATH");
    let env = d.cfg.env.clone();
    match env.as_ref().and_then(|e| e.shell.zip(e.rc_file.clone())) {
        Some((_, rc)) if shell::has_block(&rc, shell::PATH_BLOCK) => d.push_cmd(
            "path",
            level,
            format!(
                "{msg} in this shell, though {} adds it: open a new terminal, or",
                d.show(&rc)
            ),
            format!("source {}", d.show(&rc)),
        ),
        Some((sh, rc)) => d.push_manual(
            "path",
            level,
            msg,
            format!(
                "run `mcc init --force` and let it add the PATH entry, or add this line to {}:\n{}",
                d.show(&rc),
                shell::path_block(sh)
            ),
        ),
        None => d.push_manual(
            "path",
            level,
            msg,
            format!("add {local_bin} to PATH in your shell's startup file"),
        ),
    }
}

/// The rc files a global `CLAUDE_CONFIG_DIR` could hide in: the usual ones,
/// the managed one and every profile's, each once.
fn rc_files(d: &Doctor) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = validate::RC_FILES
        .iter()
        .map(|rel| d.paths.home.join(rel))
        .collect();
    let extra = d
        .cfg
        .env
        .iter()
        .filter_map(|e| e.rc_file.clone())
        .chain(d.cfg.profiles.values().filter_map(|p| p.rc_file.clone()));
    for rc in extra {
        if !files.contains(&rc) {
            files.push(rc);
        }
    }
    files
}

/// `CLAUDE_CONFIG_DIR` set for every shell by an rc file: plain `claude`
/// then no longer starts the default profile.
fn check_rc_files(d: &mut Doctor) {
    let mut found = false;
    for rc in rc_files(d) {
        let Ok(text) = std::fs::read_to_string(&rc) else {
            continue;
        };
        for (n, line) in global_exports(&text) {
            found = true;
            d.push_manual(
                "rc_config_dir",
                Level::Problem,
                format!(
                    "{}:{n} sets {CONFIG_DIR_VAR} for every shell (`{}`): plain `claude` no longer starts the default profile (~/.claude), and every profile's shell sees it",
                    d.show(&rc),
                    line.trim()
                ),
                format!(
                    "remove line {n} from {} (start profiles with their alias, `mcc run`, or `{}` instead), then open a new terminal",
                    d.show(&rc),
                    env_usage(
                        matches!(super::refresh_launchers::rc_shell(&rc), env_detect::Shell::Fish),
                        "<alias>"
                    )
                ),
            );
        }
    }
    if !found {
        d.push(
            "rc_config_dir",
            Level::Ok,
            format!("no rc file sets {CONFIG_DIR_VAR} globally"),
        );
    }
}

/// `CLAUDE_CONFIG_DIR` in doctor's own environment. Expected inside a
/// profile's shell (`mcc exec`, `mcc env`, a Claude Code session), so only
/// a warning.
fn check_environment(d: &mut Doctor) {
    match std::env::var_os(CONFIG_DIR_VAR).filter(|v| !v.is_empty()) {
        None => d.push(
            "env_config_dir",
            Level::Ok,
            format!("{CONFIG_DIR_VAR} is not set in this shell"),
        ),
        Some(v) => {
            let dir = PathBuf::from(&v);
            let fish = matches!(
                d.cfg.env.as_ref().and_then(|e| e.shell),
                Some(env_detect::Shell::Fish)
            );
            d.push_cmd(
                "env_config_dir",
                Level::Warn,
                format!(
                    "{CONFIG_DIR_VAR} is set in this shell ({}): plain `claude` starts that profile here, not the default. Expected inside `mcc exec` or after `{}`; otherwise find where it is set",
                    d.show(&dir),
                    env_usage(fish, "")
                ),
                unset_line(fish),
            );
        }
    }
}

/// How a shell applies `mcc env [profile]`: `| source` in fish, `eval` in sh.
fn env_usage(fish: bool, profile: &str) -> String {
    let cmd = if profile.is_empty() {
        "mcc env".to_string()
    } else {
        format!("mcc env {profile}")
    };
    if fish {
        format!("{cmd} | source")
    } else {
        format!("eval \"$({cmd})\"")
    }
}

/// The command that clears `CLAUDE_CONFIG_DIR` in the shell: fish has no
/// `unset`.
fn unset_line(fish: bool) -> String {
    if fish {
        format!("set -e {CONFIG_DIR_VAR}")
    } else {
        format!("unset {CONFIG_DIR_VAR}")
    }
}

/// The 1-based numbers and text of the lines in an rc file's `text` that set
/// `CLAUDE_CONFIG_DIR` for the whole shell. Comments, aliases and mcc's own
/// blocks are skipped; a one-command prefix (`CLAUDE_CONFIG_DIR=… claude`)
/// is not global and doesn't count.
fn global_exports(text: &str) -> Vec<(usize, &str)> {
    let mut in_block = false;
    let mut hits = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if shell::is_block_begin(line) {
            in_block = true;
            continue;
        }
        if shell::is_block_end(line) {
            in_block = false;
            continue;
        }
        if !in_block && sets_config_dir(line) {
            hits.push((i + 1, line));
        }
    }
    hits
}

/// Whether one rc line sets `CLAUDE_CONFIG_DIR` for the shell: `export`
/// (or `declare -x` / `typeset -x`), a bare assignment on its own, or fish's
/// `set -x` / `set --export`.
fn sets_config_dir(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with("alias ") || trimmed.starts_with("alias\t") {
        return false;
    }
    if !line.contains(CONFIG_DIR_VAR) {
        return false;
    }
    // Where a command can start: the line's start, or after a separator or
    // a keyword such as `then`.
    const START: &str = r"(?:^|[;&|{(]|\bthen\b|\bdo\b|\belse\b)\s*";
    let patterns = [
        // export / declare -x / typeset -x, maybe among other names.
        format!(
            r"{START}(?:export|(?:declare|typeset)\s+-[a-zA-Z]*x[a-zA-Z]*)\s+(?:[^;&|#]*\s)?{CONFIG_DIR_VAR}(?:=|\s|;|$)"
        ),
        // A bare assignment that is the whole command.
        format!(r#"{START}{CONFIG_DIR_VAR}=(?:"[^"]*"|'[^']*'|[^\s;&|]*)\s*(?:$|[;&|#])"#),
    ];
    if patterns
        .iter()
        .any(|p| Regex::new(p).expect("static regex").is_match(line))
    {
        return true;
    }
    // fish: set with an export flag.
    let fish = Regex::new(&format!(
        r"(?:^|;|\band\b|\bor\b)\s*set\s+((?:-{{1,2}}[a-zA-Z-]+\s+)*){CONFIG_DIR_VAR}(?:\s|$)"
    ))
    .expect("static regex");
    fish.captures_iter(line).any(|c| {
        c[1].split_whitespace().any(|flag| {
            flag == "--export"
                || (flag.starts_with('-') && !flag.starts_with("--") && flag.contains('x'))
        })
    })
}

// ---------------------------------------------------------------------------
// Per profile
// ---------------------------------------------------------------------------

fn check_default(d: &mut Doctor) {
    if !d.paths.default_dir.is_dir() {
        d.push(
            "dir",
            Level::Skipped,
            format!(
                "{} doesn't exist yet (run `claude` once, or `mcc add`)",
                d.show(&d.paths.default_dir)
            ),
        );
        return;
    }
    let dir = d.paths.default_dir.clone();
    check_auth(d, statusline::DEFAULT_PROFILE, &dir, true, None);
}

fn check_profile(d: &mut Doctor, name: &str, p: &Profile) {
    if !p.dir.is_dir() {
        d.push_cmd(
            "dir",
            Level::Problem,
            format!("the profile's directory {} is missing", d.show(&p.dir)),
            format!("mcc delete {name}"),
        );
        return;
    }
    check_mode(d, &p.dir);
    check_launcher(d, name, p);
    check_alias(d, name, p);
    let launcher = p
        .wrapper
        .as_deref()
        .filter(|w| shell::is_mcc_wrapper(w) && fsutil::is_executable(w));
    check_auth(d, name, &p.dir, false, launcher);
    check_share(d, name, p);
    check_leftovers(d, name, p);
    check_drift(d, name, p);
    check_statusline(d, name, p);
}

/// The profile directory is private to the user (0700), as `mcc add` makes it.
#[cfg(unix)]
fn check_mode(d: &mut Doctor, dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(dir) else {
        return;
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode == 0o700 {
        d.push(
            "dir_mode",
            Level::Ok,
            format!("{} is private (0700)", d.show(dir)),
        );
    } else {
        d.push_cmd(
            "dir_mode",
            Level::Problem,
            format!(
                "{} has mode {mode:04o}: other users can read its sessions and settings",
                d.show(dir)
            ),
            format!("chmod 700 {}", shell_arg(&d.show(dir))),
        );
    }
}

#[cfg(not(unix))]
fn check_mode(_: &mut Doctor, _: &Path) {}

/// A path for a fix command: `~/…` stays unquoted so the shell expands it.
fn shell_arg(shown: &str) -> String {
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+~".contains(c))
    };
    if plain(shown) {
        shown.to_string()
    } else if let Some(rest) = shown.strip_prefix("~/") {
        format!("~/{}", shell::sh_quote(rest))
    } else {
        shell::sh_quote(shown)
    }
}

/// The launcher `~/.local/bin/<alias>`: there, mcc's, executable, starting
/// the profile's directory, with the plugin root iff the profile shares
/// plugins, and from the current template.
fn check_launcher(d: &mut Doctor, name: &str, p: &Profile) {
    let refresh = format!("mcc refresh-launchers {name}");
    let Some(w) = &p.wrapper else {
        d.push(
            "launcher",
            Level::Skipped,
            "no launcher recorded (a profile from an older mcc): start it with its shell alias"
                .into(),
        );
        return;
    };
    let shown = d.show(w);
    if std::fs::symlink_metadata(w).is_err() {
        d.push_cmd(
            "launcher",
            Level::Problem,
            format!("the launcher {shown} is missing"),
            refresh,
        );
        return;
    }
    if !shell::is_mcc_wrapper(w) {
        d.push_manual(
            "launcher",
            Level::Problem,
            format!("{shown} is not a launcher written by mcc"),
            format!("move {shown} out of the way, then run `{refresh}`"),
        );
        return;
    }
    let text = std::fs::read_to_string(w).unwrap_or_default();
    let mut ok = true;
    if !fsutil::is_executable(w) {
        ok = false;
        d.push_cmd(
            "launcher",
            Level::Problem,
            format!("the launcher {shown} is not executable"),
            format!("chmod +x {}", shell_arg(&shown)),
        );
    }
    let export = format!("export {CONFIG_DIR_VAR}=\"{}\"", d.paths.shell_expr(&p.dir));
    if !text.lines().any(|l| l.trim() == export) {
        ok = false;
        d.push_cmd(
            "launcher",
            Level::Problem,
            format!(
                "the launcher {shown} doesn't start the profile's directory {}",
                d.show(&p.dir)
            ),
            refresh.clone(),
        );
    }
    let plugin_export = format!("export {}=", plugins::ENV);
    let exports_plugins = text.lines().any(|l| l.trim().starts_with(&plugin_export));
    if exports_plugins != plugins::shared(p) {
        ok = false;
        d.push_cmd(
            "launcher",
            Level::Problem,
            if plugins::shared(p) {
                format!(
                    "the launcher {shown} doesn't point Claude Code at the default's plugins, which this profile shares"
                )
            } else {
                format!(
                    "the launcher {shown} points Claude Code at the default's plugins, but this profile has its own"
                )
            },
            refresh.clone(),
        );
    }
    if !shell::launcher_is_current(w) {
        ok = false;
        d.push_cmd(
            "launcher",
            Level::Warn,
            format!(
                "the launcher {shown} predates session sharing (`{name} {}` doesn't work)",
                shell::SHARE_FLAG
            ),
            refresh,
        );
    } else if d.cfg.launch.banner && !shell::launcher_shows_banner(w) {
        ok = false;
        d.push_cmd(
            "launcher",
            Level::Warn,
            format!(
                "the launcher {shown} doesn't show the launch banner, which `[launch] banner` turns on"
            ),
            refresh,
        );
    }
    if ok {
        d.push(
            "launcher",
            Level::Ok,
            format!("launcher {shown} is current"),
        );
    }
}

/// The alias block in the rc file the profile was added to. No mcc command
/// adds a missing one back, so the fix is the block itself.
fn check_alias(d: &mut Doctor, name: &str, p: &Profile) {
    let Some(rc) = &p.rc_file else {
        d.push(
            "alias",
            Level::Skipped,
            format!(
                "no shell alias recorded (the shell wasn't supported when the profile was added): start it with {}",
                p.wrapper.as_deref().map_or_else(|| "its launcher".into(), |w| d.show(w))
            ),
        );
        return;
    };
    if shell::has_block(rc, name) {
        d.push(
            "alias",
            Level::Ok,
            format!("alias `{name}` is defined in {}", d.show(rc)),
        );
        return;
    }
    let body = shell::alias_block(
        d.paths,
        super::refresh_launchers::rc_shell(rc),
        name,
        &p.dir,
        p.wrapper.as_deref(),
    );
    d.push_manual(
        "alias",
        Level::Problem,
        format!(
            "the alias block for `{name}` is missing from {}",
            d.show(rc)
        ),
        format!(
            "add these lines to {}, then open a new terminal:\n{}",
            d.show(rc),
            shell::block_text(name, &body)
        ),
    );
}

/// What `claude auth status` says for the profile: the config directory it
/// resolved, and whether it is logged in; then, only when mcc may read
/// tokens, whether the access token has expired.
fn check_auth(d: &mut Doctor, name: &str, dir: &Path, is_default: bool, launcher: Option<&Path>) {
    let Some(bin) = d.claude_bin.clone() else {
        let why = "`claude` was not found (see above)".to_string();
        d.push("config_dir", Level::Skipped, why.clone());
        d.push("login", Level::Skipped, why);
        check_token(d, name, dir, is_default, None);
        return;
    };
    // Through the launcher when there is one, so the directory it exports
    // is the one checked; else as mcc starts the profile itself.
    let mut via = launcher.map(|l| format!("its launcher {}", d.show(l)));
    let mut status = launcher.and_then(|l| claude::launcher_status(l, d.paths).ok().flatten());
    if status.is_none() {
        via = None;
        let claude = Claude {
            bin: &bin,
            paths: d.paths,
            config_dir: (!is_default).then_some(dir),
        };
        status = claude.try_status().ok().flatten();
    }
    let Some(status) = status else {
        let why = "`claude auth status --json` printed no status".to_string();
        d.push("config_dir", Level::Skipped, why.clone());
        d.push("login", Level::Skipped, why);
        check_token(d, name, dir, is_default, None);
        return;
    };
    check_config_dir(d, name, dir, is_default, &status, via.as_deref());
    let login = if is_default {
        "claude auth login".to_string()
    } else {
        format!("mcc exec {name} -- claude auth login")
    };
    if status.logged_in {
        let who = status
            .email
            .as_deref()
            .map_or_else(String::new, |e| format!(" as {e}"));
        d.push("login", Level::Ok, format!("logged in{who}"));
    } else {
        d.push_cmd(
            "login",
            Level::Problem,
            format!(
                "not logged in (or start it with `{}` and log in there)",
                if is_default { "claude" } else { name }
            ),
            login,
        );
    }
    check_token(d, name, dir, is_default, Some(&status));
}

/// `configDirectory` (Claude Code 2.1.268+) against the registry.
fn check_config_dir(
    d: &mut Doctor,
    name: &str,
    dir: &Path,
    is_default: bool,
    status: &AuthStatus,
    via: Option<&str>,
) {
    let Some(reported) = &status.config_directory else {
        d.push(
            "config_dir",
            Level::Skipped,
            format!(
                "Claude Code {}doesn't report its config directory (configDirectory needs 2.1.268 or later)",
                d.claude_version
                    .as_deref()
                    .map_or_else(String::new, |v| format!("{v} "))
            ),
        );
        return;
    };
    let reported = statusline::expand_tilde(d.paths, Path::new(reported));
    if statusline::same_dir(&reported, dir) {
        d.push(
            "config_dir",
            Level::Ok,
            format!(
                "Claude Code uses {}{}",
                d.show(dir),
                via.map_or_else(String::new, |v| format!(" (through {v})"))
            ),
        );
        return;
    }
    let msg = format!(
        "Claude Code uses {}{}, but the profile's directory is {}",
        d.show(&reported),
        via.map_or_else(String::new, |v| format!(" through {v}")),
        d.show(dir)
    );
    if is_default {
        d.push_manual(
            "config_dir",
            Level::Problem,
            msg,
            format!(
                "find what sets {CONFIG_DIR_VAR} for plain `claude` (see the rc file check above) and remove it"
            ),
        );
    } else {
        d.push_cmd(
            "config_dir",
            Level::Problem,
            format!("{msg}; also make sure no rc file sets {CONFIG_DIR_VAR} (see above)"),
            format!("mcc refresh-launchers {name}"),
        );
    }
}

/// Token expiry: only with `[usage] live = true` (and MCC_NO_USAGE unset),
/// the setting under which mcc reads tokens at all. Not reported for a
/// profile `claude` says is logged out (the login check covers it).
fn check_token(
    d: &mut Doctor,
    name: &str,
    dir: &Path,
    is_default: bool,
    status: Option<&AuthStatus>,
) {
    if status.is_some_and(|s| !s.logged_in) {
        return;
    }
    if !d.read_tokens {
        d.push(
            "token",
            Level::Skipped,
            if d.cfg.usage.live {
                "token expiry not checked: MCC_NO_USAGE is set".into()
            } else {
                "token expiry not checked: mcc reads tokens only with `[usage] live = true` in ~/.mcc/config.toml".into()
            },
        );
        return;
    }
    match usage::token_state(dir, is_default) {
        usage::TokenState::Valid => {
            d.push("token", Level::Ok, "the OAuth access token is valid".into());
        }
        usage::TokenState::Expired => d.push_cmd(
            "token",
            Level::Warn,
            "the OAuth access token has expired: Claude Code usually refreshes it on the next start; if it doesn't, log in again".into(),
            if is_default {
                "claude auth login".into()
            } else {
                format!("mcc exec {name} -- claude auth login")
            },
        ),
        usage::TokenState::Missing => d.push(
            "token",
            Level::Ok,
            "no OAuth token stored (an API key or another login method)".into(),
        ),
    }
}

/// Session sharing: `projects/` links to the group's store exactly when the
/// registry says the profile shares, and the store is there.
fn check_share(d: &mut Doctor, name: &str, p: &Profile) {
    let group = match p.share_group.as_deref().map(share::check_group).transpose() {
        Ok(g) => g.flatten(),
        Err(_) => {
            d.push_manual(
                "share",
                Level::Problem,
                format!(
                    "invalid share_group \"{}\" in {}",
                    p.share_group.as_deref().unwrap_or_default(),
                    d.show(&d.paths.config_file)
                ),
                format!(
                    "fix `share_group` in the [profiles.{name}] entry (letters, digits, `-`, `_` or `.`), or remove it for the default group"
                ),
            );
            return;
        }
    };
    let Some(store) = d.paths.share_store(group.as_deref()) else {
        return;
    };
    let group_name = group.as_deref().unwrap_or("default");
    let projects = p.dir.join("projects");
    let shown = d.show(&projects);
    let state = match share::projects_state(&projects, &store) {
        Ok(s) => s,
        Err(e) => {
            d.push(
                "share",
                Level::Skipped,
                format!("could not read {shown}: {e:#}"),
            );
            return;
        }
    };
    let share = format!("mcc share {name}");
    match (state, p.share_sessions) {
        (Projects::Shared, true) if !store.is_dir() => d.push_cmd(
            "share",
            Level::Problem,
            format!(
                "{shown} links to the store of group \"{group_name}\", {}, which is missing",
                d.show(&store)
            ),
            share,
        ),
        (Projects::Shared, true) => d.push(
            "share",
            Level::Ok,
            format!("sessions shared in group \"{group_name}\""),
        ),
        (Projects::Shared, false) if !store.is_dir() => d.push_cmd(
            "share",
            Level::Problem,
            format!(
                "{shown} links to the missing store {}, but sharing is off",
                d.show(&store)
            ),
            share,
        ),
        (Projects::Shared, false) => d.push_manual(
            "share",
            Level::Problem,
            format!(
                "{shown} links to the store of group \"{group_name}\", but sharing is off in {}",
                d.show(&d.paths.config_file)
            ),
            format!(
                "run `{share} --off` to give the profile its own copy, or `{share}` to keep sharing"
            ),
        ),
        (Projects::Foreign(target), true) => d.push_manual(
            "share",
            Level::Problem,
            format!(
                "sharing is on, but {shown} links to {}, not to the store {}",
                target.display(),
                d.show(&store)
            ),
            format!("move {shown} out of the way, then run `{share}`"),
        ),
        (Projects::Foreign(target), false) => d.push(
            "share",
            Level::Ok,
            format!(
                "{shown} is a symlink of your own (to {}), not managed by mcc",
                target.display()
            ),
        ),
        (Projects::Other, _) => d.push_manual(
            "share",
            Level::Problem,
            format!("{shown} is not a directory"),
            format!("move {shown} out of the way"),
        ),
        (Projects::Dir | Projects::Missing, true) => d.push_manual(
            "share",
            Level::Problem,
            format!(
                "sharing is on in {}, but {shown} doesn't link to the store of group \"{group_name}\"",
                d.show(&d.paths.config_file)
            ),
            format!("run `{share}` to link it, or `{share} --off` to record that it doesn't share"),
        ),
        (Projects::Dir | Projects::Missing, false) => {
            d.push("share", Level::Ok, "sessions not shared".into());
        }
    }
}

/// Directories left by an interrupted `mcc share` (`.projects.mcc-share-*`)
/// or `mcc share --off` (`.projects.mcc-unshare-*`).
fn check_leftovers(d: &mut Doctor, name: &str, p: &Profile) {
    let staged = share::leftover_dirs(&p.dir, share::SHARE_STAGING).unwrap_or_default();
    let unshared = share::leftover_dirs(&p.dir, share::UNSHARE_STAGING).unwrap_or_default();
    let projects = p.dir.join("projects");
    for dir in &staged {
        let shown = d.show(dir);
        let msg =
            format!("{shown} was left by an interrupted `mcc share` (sessions not merged yet)");
        if p.share_sessions {
            d.push_cmd(
                "share_leftover",
                Level::Problem,
                msg,
                format!("mcc share {name}"),
            );
        } else {
            d.push_manual(
                "share_leftover",
                Level::Problem,
                msg,
                format!(
                    "move its contents into {}, then remove it",
                    d.show(&projects)
                ),
            );
        }
    }
    for dir in &unshared {
        let shown = d.show(dir);
        let msg = format!(
            "{shown} was left by an interrupted `mcc share --off` (a copy of the shared sessions)"
        );
        if std::fs::symlink_metadata(&projects).is_err() {
            d.push_cmd(
                "share_leftover",
                Level::Problem,
                format!("{msg}, and {} is missing", d.show(&projects)),
                format!("mv {} {}", shell_arg(&shown), shell_arg(&d.show(&projects))),
            );
        } else {
            d.push_cmd(
                "share_leftover",
                Level::Problem,
                format!(
                    "{msg}; {} is in place, so it is safe to remove",
                    d.show(&projects)
                ),
                format!("rm -r {}", shell_arg(&shown)),
            );
        }
    }
    if staged.is_empty() && unshared.is_empty() {
        d.push(
            "share_leftover",
            Level::Ok,
            "no leftovers of an interrupted `mcc share`".into(),
        );
    }
}

/// Drift against the default, as `mcc list` shows it (linked items,
/// overlays and shared plugins are accounted for). Local edits are kept by
/// design; only what a sync would change is reported.
fn check_drift(d: &mut Doctor, name: &str, p: &Profile) {
    if p.mode == "fresh" && matches!(manifest::load(&p.dir), manifest::Loaded::Missing) {
        d.push(
            "drift",
            Level::Skipped,
            format!(
                "fresh profile, never synced from the default (`mcc sync {name}` copies its setup)"
            ),
        );
        return;
    }
    let Some(s) = super::sync::status(d.paths, d.cfg, &p.dir) else {
        d.push(
            "drift",
            Level::Skipped,
            format!(
                "can't compare with the default ({} missing, or the comparison failed: see `mcc diff {name}`)",
                d.show(&d.paths.default_dir)
            ),
        );
        return;
    };
    let text = super::sync::status_text(&s);
    if s.conflicts > 0 {
        d.push_cmd(
            "drift",
            Level::Warn,
            format!(
                "{text} against the default: review with `mcc diff {name}`; --force overwrites the conflicts (backed up first)"
            ),
            format!("mcc sync {name} --force"),
        );
    } else if s.behind > 0 {
        d.push_cmd(
            "drift",
            Level::Warn,
            format!("{text} the default (review with `mcc diff {name}`)"),
            format!("mcc sync {name}"),
        );
    } else if s.local_edits > 0 {
        d.push(
            "drift",
            Level::Ok,
            format!("up to date with the default ({text}, kept by `mcc sync`)"),
        );
    } else {
        d.push("drift", Level::Ok, "up to date with the default".into());
    }
}

/// With the `mcc statusline` shim in the default's settings, each profile's
/// settings should have it too, unless its overlay sets a statusLine.
fn check_statusline(d: &mut Doctor, name: &str, p: &Profile) {
    if !statusline::shim_set_in(&d.paths.default_dir.join("settings.json")) {
        return;
    }
    let settings = p.dir.join("settings.json");
    if statusline::shim_set_in(&settings) {
        d.push("statusline", Level::Ok, "the mcc statusline is set".into());
        return;
    }
    let overlaid = copy::settings_overlay(p.settings.as_ref(), &p.dir)
        .ok()
        .flatten()
        .is_some_and(|o| o.contains_key("statusLine"));
    if overlaid {
        d.push(
            "statusline",
            Level::Ok,
            "the profile's settings overlay sets its own statusLine".into(),
        );
        return;
    }
    let excluded = d.cfg.sync.exclude.iter().any(|g| g == "settings");
    let conflict = !excluded && settings_conflict(d, &p.dir);
    d.push_cmd(
        "statusline",
        Level::Warn,
        format!(
            "the mcc statusline is installed in the default profile but not in {}: its usage isn't cached{}",
            d.show(&settings),
            if conflict {
                " (settings.json was edited in the profile too, so a plain sync skips it as a conflict; --force backs it up first)"
            } else {
                ""
            }
        ),
        if excluded {
            "mcc statusline install --yes".into()
        } else if conflict {
            format!("mcc sync {name} --only settings --force")
        } else {
            format!("mcc sync {name} --only settings")
        },
    );
}

/// Whether the profile's settings.json is a sync conflict (changed in the
/// default and edited in the profile), which `mcc sync` skips unless
/// --force. Read-only: the same plan `mcc diff` makes.
fn settings_conflict(d: &Doctor, dir: &Path) -> bool {
    let Ok(spec) = copy::spec(Some(&["settings".to_string()]), &d.cfg.sync) else {
        return false;
    };
    let loaded = manifest::load(dir);
    super::sync::plan_for(d.paths, d.cfg, dir, &spec, loaded.manifest(), false, false).is_ok_and(
        |plan| {
            plan.changes.iter().any(|c| {
                !c.dir && c.action == copy::Action::Conflict && c.rel == Path::new("settings.json")
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_are_found() {
        for line in [
            "export CLAUDE_CONFIG_DIR=~/.claude-work",
            "  export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\"",
            "export EDITOR=vim CLAUDE_CONFIG_DIR=/x",
            "export CLAUDE_CONFIG_DIR",
            "declare -x CLAUDE_CONFIG_DIR=/x",
            "typeset -gx CLAUDE_CONFIG_DIR=/x",
            "CLAUDE_CONFIG_DIR=/x",
            "CLAUDE_CONFIG_DIR=\"/a b\"; export CLAUDE_CONFIG_DIR",
            "CLAUDE_CONFIG_DIR='/x' # work",
            "if [ -d /x ]; then export CLAUDE_CONFIG_DIR=/x; fi",
            "[ -d /x ] && export CLAUDE_CONFIG_DIR=/x",
            "set -gx CLAUDE_CONFIG_DIR ~/.claude-work",
            "set -x CLAUDE_CONFIG_DIR /x",
            "set -Ux CLAUDE_CONFIG_DIR /x",
            "set --export CLAUDE_CONFIG_DIR /x",
            "set -g --export CLAUDE_CONFIG_DIR /x",
        ] {
            assert!(sets_config_dir(line), "{line:?}");
        }
    }

    #[test]
    fn non_global_uses_are_ignored() {
        for line in [
            "# export CLAUDE_CONFIG_DIR=/x",
            "   #export CLAUDE_CONFIG_DIR=/x",
            "alias work='CLAUDE_CONFIG_DIR=~/.work claude'",
            "alias work='export CLAUDE_CONFIG_DIR=/x; claude'",
            "alias work 'env CLAUDE_CONFIG_DIR=/x claude'",
            "CLAUDE_CONFIG_DIR=/x claude",
            "CLAUDE_CONFIG_DIR=\"/a b\" claude --resume",
            "work() { CLAUDE_CONFIG_DIR=/x claude \"$@\"; }",
            "env CLAUDE_CONFIG_DIR=/x claude",
            "echo $CLAUDE_CONFIG_DIR",
            "export MY_CLAUDE_CONFIG_DIR=/x",
            "export CLAUDE_CONFIG_DIRS=/x",
            "set -g CLAUDE_CONFIG_DIR /x",
            "set CLAUDE_CONFIG_DIR /x",
            "unset CLAUDE_CONFIG_DIR",
            "set -e CLAUDE_CONFIG_DIR",
            "ls # export CLAUDE_CONFIG_DIR=/x",
        ] {
            assert!(!sets_config_dir(line), "{line:?}");
        }
    }

    #[test]
    fn mcc_blocks_and_line_numbers() {
        let rc = "\
export EDITOR=vim
# >>> mcc:work >>>
export CLAUDE_CONFIG_DIR=/never-written-by-mcc-but-inside-its-block
# <<< mcc:work <<<
# export CLAUDE_CONFIG_DIR=/commented
export CLAUDE_CONFIG_DIR=$HOME/.claude-work
";
        assert_eq!(
            global_exports(rc),
            vec![(6, "export CLAUDE_CONFIG_DIR=$HOME/.claude-work")]
        );
    }

    #[test]
    fn fix_paths_are_shell_words() {
        assert_eq!(shell_arg("~/.mcc/profiles/work"), "~/.mcc/profiles/work");
        assert_eq!(shell_arg("~/my dir/x"), "~/'my dir/x'");
        assert_eq!(shell_arg("/a b"), "'/a b'");
    }

    #[test]
    fn selection() {
        let mut cfg = Config::default();
        cfg.profiles.insert(
            "work".into(),
            toml::from_str("dir = \"/h/.mcc/profiles/work\"\nmode = \"clone\"\ncreated_at = \"\"")
                .unwrap(),
        );
        assert_eq!(selected(&cfg, None).unwrap(), (true, vec!["work".into()]));
        assert_eq!(selected(&cfg, Some("claude")).unwrap(), (true, vec![]));
        assert_eq!(
            selected(&cfg, Some("work")).unwrap(),
            (false, vec!["work".into()])
        );
        assert!(selected(&cfg, Some("nope")).is_err());
    }
}
