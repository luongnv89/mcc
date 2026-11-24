//! `mcc default` — the account of the default profile (~/.claude).
//!
//! ~/.claude is always the default profile: plain `claude`, `mcc run`
//! outside projects, IDEs and scripts start it. Changing the default
//! account means signing ~/.claude out (`claude auth logout`) and in again
//! (`claude auth login`, in the browser) with another account. mcc only
//! runs those two commands: it never reads, copies or moves a credential,
//! and the profile's setup, history and sessions stay where they are.
//!
//! - `mcc default PROFILE` hands ~/.claude the account PROFILE is signed in
//!   to. That account is already on this machine, so mcc asks first, then
//!   trades: ~/.claude signs in to PROFILE's account and PROFILE to the one
//!   ~/.claude had, so both accounts stay reachable.
//! - `mcc default --new` signs ~/.claude in to another account, after
//!   offering to keep the account it leaves as a new profile when no
//!   profile holds it. A new account that turns out to be a profile's is
//!   offered the same trade.
//!
//! A sign-out can't be undone, so everything that can be checked is checked
//! before the first one (both accounts known and different, no Console
//! login, no session-sharing group left across organizations, nothing else
//! refreshing either login), each sign-in is verified against the account
//! it should have landed on before the next step, and a stop prints the
//! exact state and the command that recovers it.
//!
//! mcc 0.7.0–0.9.x made `mcc default PROFILE` a pointer instead:
//! `default_profile` in config.toml and a `claude` function in the rc file
//! (`# >>> mcc:default >>>`). [`retire_pointer`] removes both.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};

use crate::claude::{self, LoginOptions};
use crate::cli::{AddArgs, DefaultArgs};
use crate::config::{Account, AccountKind, Config, OrgMatch};
use crate::error::{CliError, EXIT_INTERRUPTED};
use crate::paths::Paths;
use crate::refresh::{self, RefreshLock};
use crate::{shell, ui};

use super::login::describe;
use super::run::Profile;

pub fn run(paths: &Paths, cfg: &mut Config, args: &DefaultArgs) -> Result<()> {
    if !args.new {
        let only_new = [
            (args.email.is_some(), "--email"),
            (args.sso, "--sso"),
            (args.keep_as.is_some(), "--keep-as"),
        ];
        if let Some((_, flag)) = only_new.iter().find(|(given, _)| *given) {
            return Err(CliError::usage(format!("{flag} goes with --new"))
                .hint("example: mcc default --new --email me@example.com --keep-as personal")
                .into());
        }
    }
    retire_pointer(paths, cfg)?;
    if let Some(email) = &args.email {
        super::add::check_email(email).map_err(|m| {
            CliError::usage(m).hint("example: mcc default --new --email me@example.com")
        })?;
    }
    if let Some(label) = &args.label {
        super::add::check_label(label)
            .map_err(|m| CliError::usage(m).hint("example: mcc default work --label personal"))?;
    }
    // #132: refused before any lock, sign-out or sign-in.
    if let Some(alias) = args
        .profile
        .as_deref()
        .filter(|a| !matches!(*a, "default" | "claude"))
    {
        refuse_api_key(paths, cfg, alias)?;
    }
    match args.profile.as_deref() {
        Some("default" | "claude") => Err(CliError::usage(
            "~/.claude is the default profile already: name the profile whose account it should take",
        )
        .hint("`mcc default <profile>` trades accounts with a profile; `mcc default --new` signs ~/.claude in to another account")
        .into()),
        Some(alias) => swap(paths, cfg, args, alias),
        None if args.new => switch_new(paths, cfg, args),
        None => show(paths, cfg),
    }
}

/// A trade needs a Claude account on both sides: an API-key profile (or a
/// ~/.claude whose settings give Claude Code a key) has none (#132).
fn refuse_api_key(paths: &Paths, cfg: &Config, alias: &str) -> Result<()> {
    if cfg.profiles.get(alias).is_some_and(|p| p.is_api_key()) {
        return Err(CliError::usage(format!(
            "`{alias}` is an API-key profile: it has no Claude account to trade with ~/.claude"
        ))
        .hint(format!(
            "start it with `{alias}`; `mcc default --new` signs ~/.claude in to another account"
        ))
        .into());
    }
    if crate::config::settings_use_api_key(&paths.default_dir) {
        return Err(CliError::usage(format!(
            "~/.claude uses an API key (apiKeyHelper or env.ANTHROPIC_API_KEY in {}): it has no Claude account to trade with `{alias}`",
            paths.display(&paths.default_dir.join("settings.json"))
        ))
        .hint(format!("remove the key from ~/.claude/settings.json first, or start `{alias}` with its own command"))
        .into());
    }
    Ok(())
}

// ---------------------------------------------------------------- legacy

/// The rc files an older mcc may have written a `claude` function to: the
/// current shell's, then every profile's (each once).
fn rc_files(cfg: &Config) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let all = cfg
        .env
        .iter()
        .filter_map(|e| e.rc_file.clone())
        .chain(cfg.profiles.values().filter_map(|p| p.rc_file.clone()));
    for rc in all {
        if !files.contains(&rc) {
            files.push(rc);
        }
    }
    files
}

/// The rc files that still hold the `claude` function of an older `mcc
/// default PROFILE` (`mcc doctor` reports them).
pub(crate) fn leftover_blocks(cfg: &Config) -> Vec<PathBuf> {
    rc_files(cfg)
        .into_iter()
        .filter(|rc| shell::block_body(rc, shell::DEFAULT_BLOCK).is_some())
        .collect()
}

/// Remove what an older `mcc default PROFILE` set up: the `claude` function
/// in the rc files and `default_profile` in config.toml, together, so plain
/// `claude` and `mcc run` agree again (both start ~/.claude). Says so once.
/// Returns whether anything was removed. Runs under the config lock.
pub(crate) fn retire_pointer(paths: &Paths, cfg: &mut Config) -> Result<bool> {
    let mut removed = Vec::new();
    for rc in rc_files(cfg) {
        if shell::remove_block(paths, &rc, shell::DEFAULT_BLOCK)? {
            removed.push(paths.display(&rc));
        }
    }
    let pointer = cfg.default_profile.take();
    if pointer.is_none() && removed.is_empty() {
        return Ok(false);
    }
    if pointer.is_some()
        && let Err(e) = cfg.save(&paths.config_file)
    {
        cfg.default_profile = pointer;
        return Err(e);
    }
    crate::log::write(
        "INFO",
        "retired the default-profile pointer of an older mcc",
    );
    let mut what = Vec::new();
    if !removed.is_empty() {
        what.push(format!("the `claude` function in {}", removed.join(", ")));
    }
    if pointer.is_some() {
        what.push(format!(
            "`default_profile` in {}",
            paths.display(&paths.config_file)
        ));
    }
    ui::info(&format!(
        "`mcc default` now switches the account of ~/.claude instead of pointing plain `claude` at another profile: removed {}",
        what.join(" and ")
    ));
    match pointer.filter(|p| cfg.profiles.contains_key(p) && !super::has_control(p)) {
        Some(p) => ui::hint(&format!(
            "plain `claude` (in new shells) and `mcc run` start ~/.claude again; `{p}` still starts with `{p}`, and `mcc default {p}` makes its account the default"
        )),
        None => ui::hint("plain `claude` (in new shells) and `mcc run` start ~/.claude again"),
    }
    Ok(true)
}

// ---------------------------------------------------------------- sides

/// ~/.claude or an mcc profile: what signs out and in.
struct Side {
    /// Its name and `CLAUDE_CONFIG_DIR`, as `mcc run` starts it.
    profile: Profile,
    /// Its `.claude.json`.
    state_json: PathBuf,
    /// Whether its sign-in can be pre-filled (`claude auth login --email`,
    /// which older Claude Code versions lack).
    prefill: bool,
}

impl Side {
    fn home(paths: &Paths, prefill: bool) -> Side {
        Side {
            profile: Profile {
                name: "default".into(),
                config_dir: None,
                shares_plugins: false,
                api_key: false,
            },
            state_json: paths.default_json.clone(),
            prefill,
        }
    }

    fn profile(cfg: &Config, alias: &str, prefill: bool) -> Result<Side> {
        let p = cfg
            .profiles
            .get(alias)
            .ok_or_else(|| super::unknown_profile(alias))?;
        Ok(Side {
            profile: Profile {
                name: alias.to_string(),
                config_dir: Some(p.dir.clone()),
                shares_plugins: crate::plugins::shared(p),
                api_key: p.is_api_key(),
            },
            state_json: p.dir.join(".claude.json"),
            prefill,
        })
    }

    fn name(&self) -> &str {
        &self.profile.name
    }

    /// How messages name it: ~/.claude, or `work`.
    fn shown(&self) -> String {
        match self.profile.config_dir {
            None => "~/.claude".into(),
            Some(_) => format!("`{}`", self.profile.name),
        }
    }

    /// `claude ARGS` as this profile, using its stored login only: the
    /// environment `mcc login` gives Claude Code, without the credential
    /// variables (`ANTHROPIC_API_KEY`…) that would stand in for it.
    fn claude(&self, bin: &Path, paths: &Paths, args: &[&str]) -> Command {
        let mut cmd = Command::new(bin);
        cmd.args(args);
        refresh::child_env(&mut cmd, paths, &self.profile);
        cmd
    }

    /// `claude auth status --json`, or `None` when it printed no status mcc
    /// can read (an error, a crash, no `loggedIn`): a side mcc can't read
    /// must never pass for a signed-out one. The exit code tells nothing
    /// apart: Claude Code exits 1 when signed out.
    fn status(&self, bin: &Path, paths: &Paths) -> Result<Option<claude::AuthStatus>> {
        let out = self
            .claude(bin, paths, &["auth", "status", "--json"])
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {} auth status", bin.display()))?;
        Ok(claude::known_status_from(&out.stdout))
    }

    fn logout(&self, bin: &Path, paths: &Paths) -> Result<bool> {
        let out = self
            .claude(bin, paths, &["auth", "logout"])
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {} auth logout", bin.display()))?;
        Ok(out.status.success())
    }

    /// `claude auth login`, attached to the terminal.
    fn login(&self, bin: &Path, paths: &Paths, opts: &LoginOptions) -> Result<bool> {
        let mut args = vec!["auth", "login"];
        args.extend(opts.args());
        let mut cmd = self.claude(bin, paths, &args);
        claude::CHILD_RUNNING.store(true, Ordering::SeqCst);
        let status = cmd.status();
        claude::CHILD_RUNNING.store(false, Ordering::SeqCst);
        Ok(status
            .with_context(|| format!("running {} auth login", bin.display()))?
            .success())
    }

    /// The command that signs it in by hand, pre-filled with `account`
    /// where Claude Code can.
    fn login_command(&self, account: &Account) -> String {
        let email = self
            .prefill_for(account)
            .map(|e| format!(" --email {e}"))
            .unwrap_or_default();
        match self.profile.config_dir {
            None => format!("claude auth login{email}"),
            Some(_) => format!("mcc exec {} -- claude auth login{email}", self.name()),
        }
    }

    /// The address to pre-fill its sign-in with, for `account`.
    fn prefill_for<'a>(&self, account: &'a Account) -> Option<&'a str> {
        account.email.as_deref().filter(|_| self.prefill)
    }
}

/// Say what a Claude Code whose `claude auth login` lacks `--email` means:
/// each sign-in starts blank.
fn warn_no_prefill() {
    ui::warn(
        "this Claude Code can't pre-fill the sign-in page (`claude auth login --email`): sign in with the account each step names, or update Claude Code first (`claude update`)",
    );
}

/// What `claude auth status` says a side is signed in to.
enum Login {
    Out,
    /// An Anthropic Console sign-in (API usage billing): Claude Code keeps
    /// it outside the profile's directory, so it can't be moved.
    Console,
    In(Account),
    /// `claude auth status` could not be asked.
    Unknown,
}

impl Login {
    fn describe(&self) -> String {
        match self {
            Login::Out => "signed out".into(),
            Login::Console => "signed in to an Anthropic Console account".into(),
            Login::In(a) if a.is_anonymous() => {
                "signed in to an account Claude Code doesn't name".into()
            }
            Login::In(a) => format!("signed in to {}", describe(a)),
            Login::Unknown => "in a state mcc could not check (`claude auth status` failed)".into(),
        }
    }
}

fn login_of(side: &Side, bin: &Path, paths: &Paths) -> Result<Login> {
    let Some(status) = side.status(bin, paths)? else {
        return Ok(Login::Unknown);
    };
    Ok(if !status.logged_in {
        Login::Out
    } else if status.is_console() {
        Login::Console
    } else {
        Login::In(super::add::logged_in_account(&status, &side.state_json))
    })
}

/// Refusal for a side whose `claude auth status` mcc can't read: it may be
/// signed in, so signing anything out could lose an account.
fn unreadable(shown: &str) -> anyhow::Error {
    CliError::runtime(format!(
        "mcc can't read {shown}'s sign-in: `claude auth status --json` printed no status it understands"
    ))
    .hint("run `claude auth status --json` for it to see why, then try again")
    .into()
}

/// The account `side` is signed in to, or why it can't take part in a
/// trade.
fn known_account(side: &Side, bin: &Path, paths: &Paths) -> Result<Account> {
    let shown = side.shown();
    match login_of(side, bin, paths)? {
        Login::In(a) if !a.is_anonymous() => Ok(a),
        Login::In(_) => Err(CliError::runtime(format!(
            "{shown} is signed in, but Claude Code doesn't say to which account"
        ))
        .hint("update Claude Code (`claude update`) and try again")
        .into()),
        Login::Unknown => Err(unreadable(&shown)),
        Login::Out => Err(CliError::runtime(format!("{shown} is not signed in"))
            .hint(match side.profile.config_dir {
                None => "sign ~/.claude in with `mcc default --new`".to_string(),
                Some(_) => format!("sign it in first with `{}`", side.name()),
            })
            .into()),
        Login::Console => Err(CliError::runtime(format!(
            "{shown} is signed in to an Anthropic Console account (API usage billing), which Claude Code keeps outside the profile's directory: mcc can't trade it"
        ))
        .hint("sign it in to a Claude subscription first (`claude auth login --claudeai`)")
        .into()),
    }
}

/// The profiles (never the default) signed in to `account` in the same
/// organization, `except` left out. With `bin`, each candidate's live login
/// confirms it: a signed-out profile whose state file still names the
/// account holds nothing.
fn holders(
    paths: &Paths,
    cfg: &Config,
    bin: Option<&Path>,
    account: &Account,
    except: &[&str],
) -> Vec<String> {
    super::add::other_accounts(paths, cfg, Some("default"))
        .into_iter()
        .filter(|(n, a)| n != "default" && !except.contains(&n.as_str()) && a.same_seat(account))
        .map(|(n, _)| n)
        .filter(|n| {
            let Some(bin) = bin else { return true };
            let Ok(side) = Side::profile(cfg, n, false) else {
                return false;
            };
            matches!(login_of(&side, bin, paths), Ok(Login::In(a)) if a.same_seat(account))
        })
        .collect()
}

/// Hold the refresh locks of `names`, so no `mcc usage --all` or `mcc
/// login --refresh-only` refreshes a login while it changes hands.
fn lock_logins(paths: &Paths, names: &[&str]) -> Result<Vec<RefreshLock>> {
    names
        .iter()
        .map(|name| {
            refresh::try_lock(paths, name)?.ok_or_else(|| {
                CliError::runtime(format!(
                    "another mcc command is refreshing `{name}`'s login right now"
                ))
                .hint("wait for it to finish, then try again")
                .into()
            })
        })
        .collect()
}

/// Ask before the first sign-out: `-y` skips, no terminal without it is a
/// usage error, and a "no" cancels with nothing changed.
fn confirm(args: &DefaultArgs, prompt: &str) -> Result<()> {
    if args.yes {
        return Ok(());
    }
    if !ui::is_interactive() {
        return Err(CliError::usage(
            "signing out needs a confirmation, but no terminal is attached",
        )
        .hint("pass -y to confirm")
        .into());
    }
    if ui::confirm(prompt, false, "pass -y to confirm")? {
        Ok(())
    } else {
        Err(CliError::runtime("cancelled")
            .hint("nothing was changed")
            .into())
    }
}

/// What a sign-in must land on.
#[derive(Clone, Copy)]
enum Want<'a> {
    /// This account, in this organization (a trade).
    Account(&'a Account),
    /// Any account but this one (`--new`); `None`: any account.
    Other(Option<&'a Account>),
}

/// Why `now` is not what `want` asks for; `None` when it is.
fn mismatch(now: &Login, want: Want) -> Option<String> {
    match (now, want) {
        (Login::Out, _) => Some("the sign-in did not finish".into()),
        (Login::Unknown, _) => Some("`claude auth status` failed".into()),
        (Login::Console, _) => Some(
            "signed in to an Anthropic Console account (API usage billing), not a Claude subscription".into(),
        ),
        (Login::In(a), _) if a.is_anonymous() => {
            Some("signed in, but Claude Code doesn't say to which account".into())
        }
        (Login::In(a), Want::Account(t)) if !a.same_seat(t) => Some(format!(
            "signed in to {}, not {}",
            describe(a),
            describe(t)
        )),
        (Login::In(a), Want::Other(Some(old))) if a.same_seat(old) => {
            Some(format!("still signed in to {}", describe(old)))
        }
        (Login::In(_), _) => None,
    }
}

/// The error a stop after a sign-out ends with: `stopped(state)`, the state
/// read again now, with `cause` (what went wrong) in front. A Ctrl+C keeps
/// its exit code (130).
fn stop(
    side: &Side,
    bin: &Path,
    paths: &Paths,
    stopped: &dyn Fn(&Login) -> CliError,
    cause: Option<anyhow::Error>,
    interrupted: bool,
) -> anyhow::Error {
    let now = login_of(side, bin, paths).unwrap_or(Login::Unknown);
    let mut e = stopped(&now);
    if let Some(cause) = &cause {
        e.message = format!("{cause:#} — {}", e.message);
    }
    let by_ctrl_c = cause
        .as_ref()
        .is_some_and(|c| crate::error::classify(c).0 == EXIT_INTERRUPTED);
    if interrupted || by_ctrl_c {
        e.code = EXIT_INTERRUPTED;
    }
    crate::log::write("WARN", &format!("default: {} stopped", side.name()));
    e.into()
}

/// Sign `side` out and in again until it lands on what `want` asks for:
/// a mismatch offers another try in a terminal, else stops with
/// `stopped(state)` (the state it was left in), as does any error once it
/// is signed out. `signed_out_before` says whether another side was already
/// signed out before this one (the profile half of a trade): then even an
/// error on the first sign-out stops with `stopped(state)`. Returns the
/// account.
#[allow(clippy::too_many_arguments)]
fn switch_side(
    steps: &mut ui::Steps,
    side: &Side,
    bin: &Path,
    paths: &Paths,
    want: Want,
    opts: &LoginOptions,
    signed_out_before: bool,
    stopped: &dyn Fn(&Login) -> CliError,
) -> Result<Account> {
    let shown = side.shown();
    let target = match want {
        Want::Account(t) => format!(" to {}", describe(t)),
        Want::Other(_) => String::new(),
    };
    let fail = |cause: Option<anyhow::Error>, interrupted: bool| {
        stop(side, bin, paths, stopped, cause, interrupted)
    };
    let mut first = true;
    loop {
        if !first {
            steps.extend(3);
        }
        let step = steps.start(&format!("Sign {shown} out"));
        crate::log::write("INFO", &format!("default: {} auth logout", side.name()));
        match side.logout(bin, paths) {
            Ok(true) => step.ok(""),
            Ok(false) => {
                step.warn("`claude auth logout` failed; the sign-in replaces the login anyway")
            }
            // Nothing signed out yet: this is the first round, and no other
            // side was signed out before this one.
            Err(e) if first && !signed_out_before => {
                step.fail("");
                return Err(e);
            }
            Err(e) => {
                step.fail("");
                return Err(fail(Some(e), false));
            }
        }
        first = false;
        let step = steps.start(&format!(
            "Sign {shown} in{target} (complete the sign-in in your browser)"
        ));
        crate::log::write("INFO", &format!("default: {} auth login", side.name()));
        let finished = match side.login(bin, paths, opts) {
            Ok(f) => f,
            Err(e) => {
                step.fail("");
                return Err(fail(Some(e), false));
            }
        };
        if claude::INTERRUPTED.swap(false, Ordering::SeqCst) {
            step.fail("interrupted");
            return Err(fail(None, true));
        }
        if finished {
            step.ok("");
        } else {
            step.fail("`claude auth login` did not finish");
        }
        let step = steps.start(&format!("Verify {shown}'s account"));
        let now = login_of(side, bin, paths).unwrap_or(Login::Unknown);
        let Some(why) = mismatch(&now, want) else {
            let Login::In(account) = now else {
                unreachable!("mismatch() accepts only a known account")
            };
            step.ok(&describe(&account));
            onboarded(&side.state_json);
            crate::log::write("INFO", &format!("default: {} verified", side.name()));
            return Ok(account);
        };
        step.warn(&why);
        crate::log::write("WARN", &format!("default: {} not as expected", side.name()));
        if ui::is_interactive() {
            match ui::confirm(&format!("Sign {shown} in again?"), true, "") {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => return Err(fail(Some(e), false)),
            }
        }
        return Err(fail(None, false));
    }
}

/// `claude auth login` doesn't record `hasCompletedOnboarding`: set it when
/// missing, so the profile doesn't show Claude Code's first-run setup.
/// Best effort; the file is left alone when it already says so.
fn onboarded(state_json: &Path) {
    let done = crate::copy::read_json(state_json)
        .ok()
        .flatten()
        .and_then(|v| v.get("hasCompletedOnboarding").and_then(|b| b.as_bool()))
        == Some(true);
    if !done && let Err(e) = super::add::mark_onboarded(state_json, None) {
        ui::warn(&format!(
            "could not update {}: {e:#} — Claude Code may show its first-run setup once",
            state_json.display()
        ));
    }
}

/// Forget what mcc cached about `name`'s old account: its usage, and a
/// failed token refresh.
fn forget_account_state(paths: &Paths, name: &str) {
    let dir = paths.profile_state_dir(name);
    for file in [super::statusline::USAGE_FILE, refresh::FAILED_AT_FILE] {
        let path = dir.join(file);
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            crate::log::write(
                "WARN",
                &format!("default: could not remove {}: {e}", path.display()),
            );
        }
    }
}

/// "a personal account" or "organization Acme", for the history note.
fn kind_of(a: &Account) -> String {
    let org = a
        .organization
        .as_deref()
        .or(a.organization_uuid.as_deref())
        .unwrap_or("?");
    match a.kind() {
        AccountKind::Personal => "a personal account".into(),
        AccountKind::Organization | AccountKind::Unclassified => format!("organization {org}"),
        AccountKind::Unknown => "an unknown account".into(),
    }
}

/// Each profile keeps its history and sessions: a profile that changes
/// accounts lets the new one resume what the old one did. Said when the two
/// are not the same organization (or both personal).
fn note_history(shown: &str, from: &Account, to: &Account) {
    if from.org_match(to) == OrgMatch::Same && from.kind() == to.kind() {
        return;
    }
    let who = |a: &Account| a.email.clone().unwrap_or_else(|| describe(a));
    ui::warn(&format!(
        "{shown} keeps its history and sessions: what {} ({}) did there can be resumed under {} ({})",
        who(from),
        kind_of(from),
        who(to),
        kind_of(to)
    ));
}

// ---------------------------------------------------------------- show

/// `mcc default`: `default` on stdout (the default profile's name, as
/// before), its account and how to change it on stderr. Writes nothing
/// (beyond retiring an older mcc's pointer).
fn show(paths: &Paths, cfg: &Config) -> Result<()> {
    println!("default");
    let bin = claude::find_binary(cfg.env.as_ref().and_then(|e| e.claude_bin.as_deref()));
    let login = match &bin {
        Some(bin) => login_of(&Side::home(paths, false), bin, paths)?,
        None => crate::metrics::account_info(&paths.default_json).map_or(Login::Out, Login::In),
    };
    ui::info(&format!(
        "the default profile is ~/.claude ({}): plain `claude`, `mcc run` outside projects, IDEs and scripts start it",
        paths.display(&paths.default_dir)
    ));
    let shown = |a: &Account| {
        if cfg.show_email() {
            describe(a)
        } else {
            match a.organization.as_deref() {
                Some(org) => format!("an account of {org} (email hidden)"),
                None => "an account (email hidden)".to_string(),
            }
        }
    };
    match &login {
        Login::In(a) if !a.is_anonymous() => {
            ui::info(&format!("it is signed in to {}", shown(a)));
            let same = holders(paths, cfg, bin.as_deref(), a, &[]);
            if !same.is_empty() {
                ui::warn(&format!(
                    "{} {} signed in to the same account: they share its usage limits",
                    super::project::ticked(&same),
                    if same.len() == 1 { "is" } else { "are" }
                ));
            }
        }
        other => ui::info(&format!("it is {}", other.describe())),
    }
    ui::hint(
        "`mcc default <profile>` trades accounts with a profile; `mcc default --new` signs ~/.claude in to another account",
    );
    Ok(())
}

// ---------------------------------------------------------------- trade

/// The projects whose default or allow list names `a` or `b`, as shown.
fn projects_naming(paths: &Paths, cfg: &Config, a: &str, b: &str) -> Vec<String> {
    cfg.projects
        .iter()
        .filter(|(_, p)| {
            [a, b].contains(&p.profile.as_str()) || p.allow.iter().any(|n| n == a || n == b)
        })
        .map(|(dir, _)| paths.display(Path::new(dir)))
        .collect()
}

/// Whether the projects naming `default` or `alias` follow their account
/// (the names trade places) or keep their profile. `--keep-projects` keeps,
/// `-y` follows, else asked.
fn projects_follow(paths: &Paths, cfg: &Config, args: &DefaultArgs, alias: &str) -> Result<bool> {
    let dirs = projects_naming(paths, cfg, "default", alias);
    if dirs.is_empty() || args.keep_projects {
        return Ok(false);
    }
    ui::info(&format!(
        "project defaults naming `default` or `{alias}`: {}",
        dirs.join(", ")
    ));
    if args.yes {
        return Ok(true);
    }
    let follow = format!("Follow their account (`default` and `{alias}` trade places in them)");
    let keep = "Keep their profile (they start the other account from now on)".to_string();
    Ok(ui::select(
        "These projects choose an account through their profile. After the trade:",
        &[follow.as_str(), keep.as_str()],
        0,
        "pass -y to move them with their account, or --keep-projects",
    )? == 0)
}

/// Refuse when `alias` shares its sessions with profiles that `account`
/// would put in another organization.
fn check_sharing(cfg: &Config, alias: &str, account: &Account) -> Result<()> {
    let Some(p) = cfg.profiles.get(alias).filter(|p| p.share_sessions) else {
        return Ok(());
    };
    let (_, others) = super::share::risks_for(cfg, alias, account, &p.share_group);
    if others.is_empty() {
        return Ok(());
    }
    Err(CliError::runtime(format!(
        "`{alias}` shares its sessions in group \"{}\" with {}: signed in to {} it would share them across organizations",
        super::share::group_name(&p.share_group),
        super::share::risk_list(&others),
        describe(account)
    ))
    .hint(format!(
        "stop sharing first with `mcc share {alias} --off`, then try again"
    ))
    .into())
}

/// `mcc default PROFILE`: ~/.claude and PROFILE trade accounts.
fn swap(paths: &Paths, cfg: &mut Config, args: &DefaultArgs, alias: &str) -> Result<()> {
    let bin = claude::require_binary(cfg.env.as_ref().and_then(|e| e.claude_bin.as_deref()))?;
    let prefill = claude::login_takes_email(&bin, paths);
    let home = Side::home(paths, prefill);
    let other = Side::profile(cfg, alias, prefill)?;
    let old = known_account(&home, &bin, paths)?;
    let theirs = known_account(&other, &bin, paths)?;
    if old.same_seat(&theirs) {
        return Err(CliError::runtime(format!(
            "~/.claude and `{alias}` are both signed in to {}: there is nothing to trade",
            describe(&old)
        ))
        .hint("`mcc default --new` signs ~/.claude in to another account")
        .into());
    }
    check_sharing(cfg, alias, &old)?;
    let _locks = lock_logins(paths, &["default", alias])?;

    ui::header(&format!("Trade accounts between ~/.claude and `{alias}`"));
    ui::kv(
        "~/.claude",
        &format!("{} → {}", describe(&old), describe(&theirs)),
    );
    ui::kv(
        &format!("`{alias}`"),
        &format!("{} → {}", describe(&theirs), describe(&old)),
    );
    ui::info(&format!(
        "{} is already `{alias}`'s account: ~/.claude takes it over and `{alias}` takes {} in exchange — each signs out, then in again in the browser",
        describe(&theirs),
        describe(&old)
    ));
    note_history("~/.claude", &old, &theirs);
    note_history(&format!("`{alias}`"), &theirs, &old);
    ui::warn(&format!(
        "close running Claude Code sessions of ~/.claude and `{alias}` first: signing out ends their login"
    ));
    if !prefill {
        warn_no_prefill();
    }
    confirm(
        args,
        &format!("Sign ~/.claude and `{alias}` out and in again?"),
    )?;
    let follow = projects_follow(paths, cfg, args, alias)?;

    let mut steps = ui::Steps::new(7);
    let first = switch_side(
        &mut steps,
        &home,
        &bin,
        paths,
        Want::Account(&theirs),
        &LoginOptions {
            email: home.prefill_for(&theirs),
            sso: false,
        },
        false,
        &|now| {
            CliError::runtime(format!(
                "the trade stopped: ~/.claude is {}; `{alias}` is unchanged",
                now.describe()
            ))
            .hint(format!(
                "sign ~/.claude back in to {} with `{}`, then run `mcc default {alias}` to try again",
                describe(&old),
                home.login_command(&old)
            ))
        },
    )?;
    trade_profile(
        paths, cfg, args, &mut steps, &bin, &other, &first, &old, follow,
    )
}

/// The second half of a trade, once ~/.claude is verified on `first`, the
/// account `side` (a profile) held: the registry follows the account at
/// once (so a stop below leaves it right), then `side` signs in to `old`,
/// the account ~/.claude had.
#[allow(clippy::too_many_arguments)]
fn trade_profile(
    paths: &Paths,
    cfg: &mut Config,
    args: &DefaultArgs,
    steps: &mut ui::Steps,
    bin: &Path,
    side: &Side,
    first: &Account,
    old: &Account,
    follow: bool,
) -> Result<()> {
    let alias = side.name();
    forget_account_state(paths, "default");

    // What described `first` under `alias` moves with it: the record, the
    // label, the cross-organization allowance and the cached usage go, and
    // the projects follow (when chosen).
    let step = steps.start("Update registry");
    let previous = cfg.clone();
    let p = cfg
        .profiles
        .get_mut(alias)
        .expect("checked before the trade");
    let had_label = p.label.take();
    p.account = None;
    p.share_cross_org_allowed_at = None;
    let moved = if follow {
        cfg.swap_project_profiles("default", alias)
    } else {
        0
    };
    if let Err(e) = cfg.save(&paths.config_file) {
        *cfg = previous;
        step.fail("");
        return Err(CliError::runtime(format!(
            "~/.claude is signed in to {} now, but mcc could not record the trade, so `{alias}` was left as it is: {e:#}",
            describe(first)
        ))
        .hint(format!(
            "check that {} can be written; then sign `{alias}` in to {} with `{}`, or ~/.claude back in to it with `{}`",
            paths.display(&paths.config_file),
            describe(old),
            side.login_command(old),
            Side::home(paths, side.prefill).login_command(old)
        ))
        .into());
    }
    forget_account_state(paths, alias);
    let mut note = paths.display(&paths.config_file);
    if moved > 0 {
        note.push_str(&format!(
            "; {moved} project{} followed {} account",
            if moved == 1 { "" } else { "s" },
            if moved == 1 { "its" } else { "their" }
        ));
    }
    step.ok(&note);

    let got = switch_side(
        steps,
        side,
        bin,
        paths,
        Want::Account(old),
        &LoginOptions {
            email: side.prefill_for(old),
            sso: false,
        },
        true,
        &|now| {
            CliError::runtime(format!(
                "the trade stopped halfway: ~/.claude is signed in to {}, but `{alias}` is {} (mcc has recorded the trade already)",
                describe(first),
                now.describe()
            ))
            .hint(format!(
                "sign `{alias}` in to {} with `{}` to finish",
                describe(old),
                side.login_command(old)
            ))
        },
    )?;
    crate::log::write("INFO", &format!("default: traded accounts with {alias}"));

    if let Some(l) = &had_label
        && args.label.is_none()
    {
        ui::info(&format!(
            "`{alias}`'s label \"{l}\" was for {}, which ~/.claude has now",
            describe(first)
        ));
    }
    let label =
        super::add::pick_label(args.label.as_deref(), args.yes, Some(&got)).unwrap_or_else(|e| {
            ui::warn(&format!("no label set: {e:#}"));
            None
        });
    let p = cfg
        .profiles
        .get_mut(alias)
        .expect("checked before the trade");
    p.account = Some(got.clone());
    p.label = label;
    if let Err(e) = cfg.save(&paths.config_file) {
        ui::warn(&format!(
            "could not record `{alias}`'s new account and label in {}: {e:#} — `mcc list` reads the account from Claude Code anyway",
            paths.display(&paths.config_file)
        ));
    }

    ui::result(
        true,
        &format!(
            "~/.claude is now signed in to {}, and `{alias}` to {}",
            describe(first),
            describe(&got)
        ),
    );
    ui::kv("Plain `claude`", &format!("starts {}", describe(first)));
    ui::kv(&format!("`{alias}`"), &format!("starts {}", describe(&got)));
    ui::hint(
        "IDEs and Claude Code sessions still running pick up the change when they start again",
    );
    Ok(())
}

// ---------------------------------------------------------------- --new

/// `--keep-as`, else asked in a terminal (without `-y`): the alias to keep
/// `old` under, or `None` to let it go.
fn keep_as(args: &DefaultArgs, old: &Account) -> Result<Option<String>> {
    if let Some(alias) = &args.keep_as {
        return Ok(Some(alias.clone()));
    }
    if args.yes || !ui::is_interactive() {
        return Ok(None);
    }
    let keep = format!("Keep {} as a new profile", describe(old));
    let drop = "Sign it out (it can be added again later with `mcc add`)";
    if ui::select(
        &format!(
            "No profile is signed in to {}, the account ~/.claude is leaving.",
            describe(old)
        ),
        &[keep.as_str(), drop],
        0,
        "pass --keep-as ALIAS, or -y to sign it out",
    )? != 0
    {
        return Ok(None);
    }
    Ok(Some(ui::input("Profile name", "pass --keep-as ALIAS")?))
}

/// `mcc default --new`: sign ~/.claude in to another account.
fn switch_new(paths: &Paths, cfg: &mut Config, args: &DefaultArgs) -> Result<()> {
    let bin = claude::require_binary(cfg.env.as_ref().and_then(|e| e.claude_bin.as_deref()))?;
    let prefill = claude::login_takes_email(&bin, paths);
    let home = Side::home(paths, prefill);
    // What ~/.claude leaves: an account, nothing, or a Console sign-in
    // (which no profile can keep). One it can't name is not signed out.
    let (old, console) = match login_of(&home, &bin, paths)? {
        Login::In(a) if !a.is_anonymous() => (Some(a), false),
        Login::Out => (None, false),
        Login::Console => (None, true),
        Login::Unknown => return Err(unreadable("~/.claude")),
        Login::In(_) => {
            return Err(CliError::runtime(
                "~/.claude is signed in, but Claude Code doesn't say to which account: mcc can't tell what signing it out would lose",
            )
            .hint("update Claude Code (`claude update`) and try again")
            .into());
        }
    };
    let email = args.email.as_deref().map(str::trim);

    // The account asked for is a profile's: that is a trade.
    if let (Some(email), Some(old)) = (email, &old) {
        let wanted = Account {
            email: Some(email.to_string()),
            ..Account::default()
        };
        let owners: Vec<String> = super::add::other_accounts(paths, cfg, Some("default"))
            .into_iter()
            .filter(|(n, a)| n != "default" && a.same_login(&wanted) && !a.same_seat(old))
            .map(|(n, _)| n)
            .collect();
        if let [owner] = owners.as_slice() {
            ui::warn(&format!(
                "{email} is the account of profile `{owner}`: switching ~/.claude to it means trading accounts with `{owner}`"
            ));
            return swap(paths, cfg, args, owner);
        }
        if owners.len() > 1 {
            return Err(CliError::usage(format!(
                "{email} is the account of profiles {}",
                super::project::ticked(&owners)
            ))
            .hint("name the one to trade with: `mcc default <profile>`")
            .into());
        }
    }

    if email.is_some() && !prefill {
        return Err(CliError::usage(
            "this Claude Code's `claude auth login` has no --email option",
        )
        .hint("update Claude Code (`claude update`), or leave --email out and pick the account on the sign-in page")
        .into());
    }

    // The account ~/.claude leaves: kept by a profile already, kept as a
    // new one, or signed out of this machine.
    let kept_by = old
        .as_ref()
        .map(|a| holders(paths, cfg, Some(&bin), a, &[]))
        .unwrap_or_default();
    let keep = match &old {
        Some(a) if kept_by.is_empty() => keep_as(args, a)?,
        _ => {
            if let Some(alias) = &args.keep_as {
                ui::warn(&format!(
                    "--keep-as {alias} is not needed: {}",
                    match &old {
                        Some(_) => format!(
                            "{} keeps the account ~/.claude is leaving",
                            super::project::ticked(&kept_by)
                        ),
                        None if console =>
                            "an Anthropic Console sign-in can't be kept as a profile".to_string(),
                        None => "~/.claude is not signed in to any account".to_string(),
                    }
                ));
            }
            None
        }
    };
    if let Some(alias) = &keep
        && (cfg.profiles.contains_key(alias) || alias == "default" || alias == "claude")
    {
        return Err(
            CliError::usage(format!("profile \"{alias}\" already exists"))
                .hint("pick another name for --keep-as")
                .into(),
        );
    }
    let _lock = lock_logins(paths, &["default"])?;

    ui::header("Switch ~/.claude to another account");
    let to = email.map_or("the account you sign in to".to_string(), str::to_string);
    let from = match (&old, console) {
        (Some(a), _) => describe(a),
        (None, true) => "an Anthropic Console account".to_string(),
        (None, false) => "signed out".to_string(),
    };
    ui::kv("~/.claude", &format!("{from} → {to}"));
    match (&old, &keep) {
        (Some(a), Some(alias)) => ui::kv(
            "Kept as",
            &format!(
                "`{alias}` (a clone of ~/.claude's setup, signed in to {})",
                describe(a)
            ),
        ),
        (Some(a), None) if kept_by.is_empty() => ui::warn(&format!(
            "{} will no longer be signed in on this machine",
            describe(a)
        )),
        (Some(a), None) => ui::info(&format!(
            "{} stays signed in to {}",
            describe(a),
            super::project::ticked(&kept_by)
        )),
        (None, _) if console => ui::warn(
            "signing out removes the Anthropic Console sign-in (API usage billing); no profile can keep it, since Claude Code stores it outside the profile's directory",
        ),
        (None, _) => {}
    }
    if old.is_some() || console {
        ui::warn(
            "~/.claude keeps its history and sessions: the account you sign in to can resume them",
        );
        ui::warn(
            "close running Claude Code sessions of ~/.claude first: signing out ends their login",
        );
    }
    confirm(args, "Switch ~/.claude to another account?")?;

    if let (Some(alias), Some(a)) = (&keep, &old) {
        if !prefill {
            warn_no_prefill();
        }
        keep_old_account(paths, cfg, args, &bin, alias, a, prefill)?;
    }

    let mut steps = ui::Steps::new(3);
    let new = switch_side(
        &mut steps,
        &home,
        &bin,
        paths,
        Want::Other(old.as_ref()),
        &LoginOptions {
            email,
            sso: args.sso,
        },
        false,
        &|now| {
            let mut e = CliError::runtime(format!(
                "~/.claude did not switch accounts: it is {}",
                now.describe()
            ));
            if let Some(a) = &old {
                e = e.hint(format!(
                    "run `mcc default --new` to try again; `{}` signs it back in to {}",
                    home.login_command(a),
                    describe(a)
                ));
            } else {
                e = e.hint("run `mcc default --new` to try again");
            }
            e
        },
    )?;
    forget_account_state(paths, "default");
    crate::log::write("INFO", "default: switched to another account");

    // The new account is a profile's too. ~/.claude's old account is on no
    // profile: offer to give it to that profile, so both accounts stay on
    // this machine. ~/.claude has switched already, so whatever goes wrong
    // from here says so.
    let except: Vec<&str> = keep.iter().map(String::as_str).collect();
    let same = holders(paths, cfg, Some(&bin), &new, &except);
    if let Some(owner) = same.first().cloned() {
        ui::warn(&format!(
            "{} is also the account of {}: profiles on the same account share its usage limits",
            describe(&new),
            super::project::ticked(&same)
        ));
        if let Some(a) = old
            .as_ref()
            .filter(|_| keep.is_none() && kept_by.is_empty())
        {
            let side = Side::profile(cfg, &owner, prefill).expect("holders are registered");
            let switched = |e: anyhow::Error| -> anyhow::Error {
                let (code, _) = crate::error::classify(&e);
                CliError {
                    code,
                    message: format!(
                        "~/.claude is now signed in to {}, but `{owner}` was not traded: {e:#}",
                        describe(&new)
                    ),
                    hint: Some(format!(
                        "to keep {} on this machine, sign `{owner}` in to it with `{}`",
                        describe(a),
                        side.login_command(a)
                    )),
                }
                .into()
            };
            note_history(&format!("`{owner}`"), &new, a);
            let trade = args.yes
                || (ui::is_interactive()
                    && ui::confirm(
                        &format!(
                            "Sign `{owner}` out and in to {}, so that account stays on this machine?",
                            describe(a)
                        ),
                        true,
                        "",
                    )
                    .map_err(switched)?);
            if trade {
                if let Err(e) = check_sharing(cfg, &owner, a) {
                    ui::warn(&format!("`{owner}` keeps its account: {e:#}"));
                } else {
                    let _lock = lock_logins(paths, &[owner.as_str()]).map_err(switched)?;
                    let follow = projects_follow(paths, cfg, args, &owner).map_err(switched)?;
                    steps.extend(4);
                    return trade_profile(
                        paths, cfg, args, &mut steps, &bin, &side, &new, a, follow,
                    );
                }
            }
        }
    }

    ui::result(
        true,
        &format!("~/.claude is now signed in to {}", describe(&new)),
    );
    if let (Some(alias), Some(a)) = (&keep, &old) {
        ui::kv(&format!("`{alias}`"), &format!("starts {}", describe(a)));
    }
    ui::hint(
        "IDEs and Claude Code sessions still running pick up the change when they start again",
    );
    Ok(())
}

/// Keep `old`, the account ~/.claude is leaving, as the new profile
/// `alias`: `mcc add ALIAS --clone`, its sign-in pre-filled with `old`.
/// Before ~/.claude signs out, so a failure leaves everything signed in.
fn keep_old_account(
    paths: &Paths,
    cfg: &mut Config,
    args: &DefaultArgs,
    bin: &Path,
    alias: &str,
    old: &Account,
    prefill: bool,
) -> Result<()> {
    let add = AddArgs {
        alias: Some(alias.to_string()),
        clone: true,
        fresh: false,
        no_login: false,
        email: old.email.clone().filter(|_| prefill),
        sso: false,
        link: Vec::new(),
        copy_plugins: false,
        label: None,
        color: None,
        api_key_prompt: false,
        api_key_stdin: false,
        api_key_helper: None,
        budget: None,
        yes: args.yes,
        from_default: true,
    };
    super::add::run(paths, cfg, &add).map_err(|e| {
        let (code, hint) = crate::error::classify(&e);
        anyhow::Error::from(CliError {
            code,
            message: format!("could not keep {} as `{alias}`: {e:#}", describe(old)),
            hint: Some(hint.unwrap_or_else(|| {
                "~/.claude was not signed out; fix the problem and run `mcc default --new` again"
                    .into()
            })),
        })
    })?;
    let side = Side::profile(cfg, alias, prefill)?;
    if matches!(login_of(&side, bin, paths), Ok(Login::In(a)) if a.same_seat(old)) {
        return Ok(());
    }
    Err(CliError::runtime(format!(
        "`{alias}` was added, but it is not signed in to {}: ~/.claude was not signed out",
        describe(old)
    ))
    .hint(format!(
        "sign it in with `{}`, or remove it with `mcc delete {alias}`; then run `mcc default --new` again",
        side.login_command(old)
    ))
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(email: &str, org: Option<&str>) -> Account {
        Account {
            email: Some(email.into()),
            organization_uuid: org.map(str::to_string),
            ..Account::default()
        }
    }

    #[test]
    fn a_sign_in_must_land_on_the_wanted_account() {
        let a = account("a@x.com", None);
        let b = account("b@x.com", Some("org-1"));
        let now = Login::In(b.clone());
        assert!(mismatch(&now, Want::Account(&b)).is_none());
        assert!(
            mismatch(&now, Want::Account(&a))
                .unwrap()
                .starts_with("signed in to b@x.com")
        );
        // The same login in another organization is another account.
        let other_org = Login::In(account("b@x.com", Some("org-2")));
        assert!(mismatch(&other_org, Want::Account(&b)).is_some());
        // --new: anything but the account ~/.claude had.
        assert!(mismatch(&now, Want::Other(Some(&a))).is_none());
        assert!(mismatch(&Login::In(a.clone()), Want::Other(Some(&a))).is_some());
        assert!(mismatch(&now, Want::Other(None)).is_none());
        // Never: signed out, a Console login, or an account without a name.
        for bad in [Login::Out, Login::Console, Login::In(Account::default())] {
            assert!(mismatch(&bad, Want::Other(None)).is_some());
            assert!(mismatch(&bad, Want::Account(&b)).is_some());
        }
    }

    #[test]
    fn manual_sign_in_commands_name_the_side() {
        let paths = Paths::new("/h".into());
        let home = Side::home(&paths, true);
        let cfg: Config = toml::from_str(
            "[profiles.work]\ndir = \"/h/.mcc/profiles/work\"\nmode = \"clone\"\ncreated_at = \"t\"\n",
        )
        .unwrap();
        let work = Side::profile(&cfg, "work", true).unwrap();
        let a = account("a@x.com", None);
        assert_eq!(home.login_command(&a), "claude auth login --email a@x.com");
        assert_eq!(
            work.login_command(&a),
            "mcc exec work -- claude auth login --email a@x.com"
        );
        assert_eq!(home.shown(), "~/.claude");
        assert_eq!(work.shown(), "`work`");
        assert_eq!(home.login_command(&Account::default()), "claude auth login");
        // A Claude Code without `--email`: nothing to pre-fill.
        let blank = Side::home(&paths, false);
        assert_eq!(blank.login_command(&a), "claude auth login");
        assert_eq!(blank.prefill_for(&a), None);
    }
}
