//! `mcc usage` — each profile's 5-hour and weekly usage in detail (text or JSON),
//! with when it was last used and when its login runs out (#100).
//!
//! `mcc usage --all` also lets Claude Code refresh each account whose live
//! lookup finds its token expired, then looks it up again (#106; see
//! [`crate::refresh::refresh_expired`]). Its notices go to stderr, so
//! `--json` stays as it was.
//!
//! With other machines set up (`[machines.NAME]`, #130) and `--remote`,
//! it also reads each of them over SSH ([`crate::machines`]) and shows one
//! table per machine. Every row of an account then shows that account's
//! newest numbers from any machine, saved to this machine's cache too, and
//! `--all` looks each account up live once, on one machine. Without
//! `--remote`, nothing of this runs: SSH is slow, so only this machine's
//! accounts are shown by default.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use console::{measure_text_width, style};
use serde::{Deserialize, Serialize};

use crate::cli::UsageArgs;
use crate::config::{AuthKind, Config, LOCAL_MACHINE};
use crate::devin;
use crate::error::{CliError, EXIT_INTERRUPTED};
use crate::machines::{self, MachineError};
use crate::metrics::{self, Provider};
use crate::paths::Paths;
use crate::ui;
use crate::usage::{self, DeadlineSource, LoginDeadline, Reason, Status, Usage, Window};

/// Printed on stderr each time `mcc usage --all` looks every account's
/// usage up live (even with `--quiet`; never with `MCC_NO_USAGE`).
pub const USAGE_ALL_NOTE: &str = crate::cli::usage_all_note!();

/// Printed on its own line after [`USAGE_ALL_NOTE`] when Devin is set up
/// here (#124): `--all` looks Devin's usage up live too.
pub const USAGE_ALL_DEVIN_NOTE: &str = crate::cli::usage_all_devin_note!();

/// Printed on its own line after [`USAGE_ALL_NOTE`] when other machines
/// are set up (#130): `--all` refreshes them too.
pub const USAGE_ALL_MACHINES_NOTE: &str = crate::cli::usage_all_machines_note!();

/// Printed on stderr the first time `mcc usage` reads other machines
/// (#130); `[usage] machines_disclosed_at` records that it was.
pub const MACHINES_DISCLOSURE: &str = crate::cli::machines_disclosure!();

/// How the table is laid out: the bar width of the usage cells (0 = no bar),
/// whether the absolute reset time follows the relative one, and whether
/// the UPDATED column (the age of cached usage) is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    bar: usize,
    reset_time: bool,
    updated: bool,
}

/// Layouts tried, most generous first: the absolute reset time goes first,
/// then the bar shrinks in the same 10/5/0 steps as the `mcc list` table
/// (narrow terminals). Cached usage's UPDATED column goes before the 5-cell
/// bar does, so the table (LAST ACTIVE and LOGIN ENDS included) still fits
/// 80 columns with a bar.
const LAYOUTS: [Layout; 6] = [
    Layout {
        bar: 10,
        reset_time: true,
        updated: true,
    },
    Layout {
        bar: 10,
        reset_time: false,
        updated: true,
    },
    Layout {
        bar: 5,
        reset_time: false,
        updated: true,
    },
    Layout {
        bar: 5,
        reset_time: false,
        updated: false,
    },
    Layout {
        bar: 0,
        reset_time: false,
        updated: true,
    },
    Layout {
        bar: 0,
        reset_time: false,
        updated: false,
    },
];

/// In the table's place for usage when nothing is cached: short enough for
/// the row to fit 80 columns. The hint on stderr says more.
const NO_DATA_CELL: &str = "— (no usage cached; run /usage)";

/// One profile's usage, as printed and as serialized by `--json`. The fields
/// after `usage` were added in #100 (schema 1 only gains fields). Read back
/// from another machine's `mcc usage` (#130).
#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    name: String,
    command: String,
    is_default: bool,
    account: Option<String>,
    usage: Usage,
    /// When the profile was last used for a conversation (its history or
    /// transcripts); null when unknown.
    #[serde(
        serialize_with = "ser_time",
        deserialize_with = "usage::de_time",
        default
    )]
    last_activity: Option<DateTime<Utc>>,
    /// When the login needs a new sign-in; null when unknown.
    #[serde(
        serialize_with = "ser_time",
        deserialize_with = "usage::de_time",
        default
    )]
    login_expires_at: Option<DateTime<Utc>>,
    /// Where that was read: `credentials_file` or `keychain`.
    login_expires_source: Option<DeadlineSource>,
    /// The login runs out within `[usage] login_warn_days` (or has).
    login_warning: bool,
    /// The profile plain `claude` and `mcc run` start outside any project:
    /// always ~/.claude, whose account `mcc default` switches. Added in #104.
    is_global_default: bool,
    /// `claude` for a Claude Code profile, `devin` for Devin. Added in #124.
    provider: Provider,
    /// The account as machines match it ([`usage::account_id`]), never
    /// the email. Only where machines are compared (#130): left out of
    /// the JSON otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    /// The other machines this account is signed in on (#130), `local`
    /// for this one. Only with machines set up; left out otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    also_on: Option<Vec<String>>,
    /// For the machine reading this one (#130), on a row whose numbers
    /// came from another machine: when this machine last updated it itself
    /// ([`Usage::own_updated_at`]), null when never. Only in a
    /// `--machine-protocol` answer; left out everywhere else.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_own_time",
        deserialize_with = "de_own_time"
    )]
    own_updated_at: Option<Option<DateTime<Utc>>>,
    /// `api_key` for an API-key profile (#132), shown apart, without
    /// windows; its `usage` has its estimated spend and budget. Left out
    /// of the JSON for every other row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth: Option<AuthKind>,
    #[serde(skip)]
    config_dir: PathBuf,
    /// An API-key profile's monthly budget, for its usage.
    #[serde(skip)]
    budget_usd: Option<f64>,
}

fn ser_time<S: serde::Serializer>(t: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
    match t {
        Some(t) => s.serialize_str(&t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        None => s.serialize_none(),
    }
}

fn ser_own_time<S: serde::Serializer>(
    t: &Option<Option<DateTime<Utc>>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    ser_time(&t.flatten(), s)
}

/// Present, even as null, is `Some`: absent (an older mcc) stays `None`.
fn de_own_time<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<DateTime<Utc>>>, D::Error> {
    usage::de_time(d).map(Some)
}

impl Entry {
    fn new(name: &str, command: &str, is_default: bool, dir: &Path, state_json: &Path) -> Self {
        let seat = metrics::account_info(state_json).unwrap_or_default();
        Self {
            name: name.to_string(),
            command: command.to_string(),
            is_default,
            account: seat.email.clone(),
            account_id: seat
                .email
                .as_deref()
                .map(|e| usage::account_id(e, seat.organization_uuid.as_deref())),
            also_on: None,
            own_updated_at: None,
            usage: Usage::default(),
            last_activity: metrics::last_activity(dir).map(DateTime::<Utc>::from),
            login_expires_at: None,
            login_expires_source: None,
            login_warning: false,
            is_global_default: false,
            provider: Provider::Claude,
            auth: None,
            config_dir: dir.to_path_buf(),
            budget_usd: None,
        }
    }

    /// An mcc profile's row: an API-key profile's says so (#132).
    fn profile(name: &str, p: &crate::config::Profile) -> Self {
        let mut e = Self::new(name, name, false, &p.dir, &p.dir.join(".claude.json"));
        if p.is_api_key() {
            e.auth = Some(AuthKind::ApiKey);
            e.budget_usd = p.api_budget_usd;
        }
        e
    }

    /// Devin's row (#124): its usage, and nothing of a Claude Code login.
    fn devin(usage: Usage) -> Self {
        Self {
            name: devin::NAME.into(),
            command: devin::NAME.into(),
            is_default: false,
            account: None,
            usage,
            last_activity: None,
            login_expires_at: None,
            login_expires_source: None,
            login_warning: false,
            is_global_default: false,
            provider: Provider::Devin,
            account_id: None,
            also_on: None,
            own_updated_at: None,
            auth: None,
            config_dir: PathBuf::new(),
            budget_usd: None,
        }
    }

    /// This row as [`machines::select`] weighs it: its login is fine
    /// unless known to have run out.
    fn candidate(&self, local: bool, now: DateTime<Utc>) -> machines::Candidate<'_> {
        machines::Candidate {
            local,
            account_id: self.account_id.as_deref(),
            login_ok: self.login_expires_at.is_none_or(|t| t > now),
        }
    }

    /// A row as another machine sent it, kept to what this machine checks
    /// and shows: no account text (another machine never sends an email,
    /// and a changed one could send terminal escapes), nor its own
    /// `also_on`, which [`merge_accounts`] sets here; and no time of its
    /// numbers later than this machine's clock ([`machines::drop_future_time`]).
    fn received(mut self, machine: &str, now: DateTime<Utc>) -> Self {
        self.account = None;
        self.also_on = None;
        // Its own last update goes with its numbers, as `Usage` keeps it.
        if let Some(own) = self.own_updated_at.take() {
            self.usage.own_updated_at = Some(own);
        }
        if machines::drop_future_time(&mut self.usage, now) {
            crate::log::write(
                "WARN",
                &format!(
                    "machine {machine}: {}'s usage time is ahead of this clock; not used as newest",
                    self.name
                ),
            );
        }
        self
    }

    /// What the usage lookup needs of this profile.
    fn target(&self) -> usage::Target {
        usage::Target {
            name: self.name.clone(),
            config_dir: self.config_dir.clone(),
            is_default: self.is_default,
            api_key: self.auth.is_some(),
            budget_usd: self.budget_usd,
        }
    }

    fn set_login(&mut self, deadline: LoginDeadline, now: DateTime<Utc>, window_days: u32) {
        self.login_expires_at = deadline.at();
        self.login_expires_source = deadline.source();
        self.login_warning = deadline
            .at()
            .is_some_and(|at| usage::login_warning(at, now, window_days).is_some());
    }
}

pub fn run(paths: &Paths, cfg: &Config, args: &UsageArgs) -> Result<()> {
    let default = || {
        Entry::new(
            "default",
            "claude",
            true,
            &paths.default_dir,
            &paths.default_json,
        )
    };
    // `mcc usage devin`: Devin alone (a profile of that name, from before
    // the name was reserved, still comes first).
    let devin_only =
        args.profile.as_deref() == Some(devin::NAME) && !cfg.profiles.contains_key(devin::NAME);
    if devin_only && !devin::detected(paths) {
        return Err(CliError::usage(format!(
            "Devin is not set up here: no Devin CLI login at {}",
            paths.display(&paths.devin_credentials)
        ))
        .hint("sign the Devin CLI in with `devin auth login` (or `mcc devin -- auth login`)")
        .into());
    }
    if args.remote && cfg.machines.is_empty() {
        return Err(CliError::usage("--remote: no other machines are set up")
            .hint("add one as [machines.NAME] with host = \"…\" in ~/.mcc/config.toml (`mcc config edit`)")
            .into());
    }
    let protocol = args.machine_protocol;
    // `--only` names profiles of this machine alone: Devin is left out.
    let with_devin =
        (args.profile.is_none() || devin_only) && args.only.is_none() && devin::detected(paths);
    let mut entries: Vec<Entry> = match args.profile.as_deref() {
        None => std::iter::once(default())
            .chain(cfg.profiles.iter().map(|(n, p)| Entry::profile(n, p)))
            .collect(),
        Some(_) if devin_only => Vec::new(),
        Some("default" | "claude") => vec![default()],
        Some(name) => match cfg.profiles.get(name) {
            Some(p) => vec![Entry::profile(name, p)],
            None => return Err(super::unknown_profile(name)),
        },
    };
    if let Some(only) = &args.only {
        entries = only_these(entries, only);
    }

    for e in &mut entries {
        e.is_global_default = e.is_default;
    }

    // Accounts are matched across machines (#130) only where machines are
    // read, or for the machine reading this one; nowhere else.
    let fan_out = fans_out(cfg, args);
    if !fan_out && !protocol {
        for e in &mut entries {
            e.account_id = None;
        }
    }
    // Another machine gets the account's hash, never its email.
    let show_email = (cfg.show_email() || args.show_email) && !args.hide_email && !protocol;
    if !show_email {
        for e in &mut entries {
            e.account = None;
        }
    }
    // API-key profiles (#132) are a category of their own: no login to
    // expire, no windows to look up or sort by, never matched across
    // machines. They get their estimated spend instead.
    let (mut api_entries, mut entries): (Vec<Entry>, Vec<Entry>) =
        entries.into_iter().partition(|e| e.auth.is_some());

    // Login expiry: the credentials file's login deadline alone (no token,
    // no access-token expiry, no network); the Keychain only where the live lookup may read it anyway.
    let now = Utc::now();
    let window_days = cfg.usage.login_warn_days();
    // Ctrl+C while Claude Code refreshed a token: the table is still shown,
    // then mcc exits 130.
    let mut interrupted = false;
    // Another machine reading this one gets the cache unless it asks for
    // --all, whatever `[usage] live` says here (#130): its read never
    // looks an account up live by accident.
    let live = args.all || (cfg.usage.live && !protocol);
    let allow_keychain = live && !usage::disabled_by_env();
    for e in &mut entries {
        let times = usage::login_times(&e.config_dir, e.is_default, allow_keychain);
        e.set_login(times.deadline, now, window_days);
    }

    let mut machines: Option<Vec<MachineView>> = None;
    // This machine's own last update, from its own numbers (#130): taken
    // before the merge puts other machines' newer numbers into its rows.
    let mut local_updated = None;
    if usage::disabled_by_env() {
        crate::log::write("INFO", "usage not fetched: MCC_NO_USAGE is set");
    } else if fan_out {
        let (views, own, stopped) =
            lookup_with_machines(paths, cfg, args, &mut entries, with_devin);
        machines = Some(views);
        local_updated = own;
        interrupted = stopped;
    } else {
        // `--all`: every account looked up live for this run only, the
        // default included, whatever `[usage] live` says.
        if args.all {
            eprintln!("{USAGE_ALL_NOTE}");
            if with_devin {
                eprintln!("{USAGE_ALL_DEVIN_NOTE}");
            }
            crate::log::write("INFO", "mcc usage --all: live lookup for every profile");
        }
        let targets: Vec<usage::Target> = entries.iter().map(Entry::target).collect();
        let mut found = usage::lookup(paths, &targets, live);
        if args.all {
            // Only --all refreshes, and only expired tokens (#106); the
            // lookups that follow replace those in `found`, so they are
            // what is shown and saved.
            interrupted = crate::refresh::refresh_expired(paths, cfg, &targets, &mut found);
            // So that `mcc usage` without --all shows these numbers next time.
            usage::save_to_cache(paths, &targets, &found);
        }
        if protocol {
            // So the machine reading this one can tell which numbers are newer.
            usage::stamp_live(&mut found, Utc::now());
        }
        for (e, u) in entries.iter_mut().zip(found) {
            e.usage = u;
        }
    }
    if !usage::disabled_by_env() {
        for e in &mut api_entries {
            e.usage = usage::api_key_usage(paths, &e.target(), live);
        }
    }
    // The row order (#126): --sort for this run, else [usage] sort, else
    // weekly usage least to most. The API-key and Devin blocks below are
    // appended after the table, never part of this sort.
    let order = args.sort.unwrap_or_else(|| cfg.usage.sort());
    sort_entries(&mut entries, order);
    for v in machines.iter_mut().flatten() {
        sort_entries(&mut v.profiles, order);
        // Another machine's API-key and Devin rows follow its table, as
        // this one's do.
        v.profiles.sort_by_key(category);
    }
    // Devin: looked up live only as the profiles are (`[usage] live`, or
    // --all), never with MCC_NO_USAGE; else its last lookup.
    let devin = with_devin.then(|| {
        let how = devin::Lookup::of(live);
        Entry::devin(devin::lookup(paths, how).usage)
    });

    // Devin is never merged with another machine: its time is its own.
    let local_updated = local_updated.max(devin.as_ref().and_then(|d| machines::time_of(&d.usage)));

    // The machine reading this one tells its own numbers from those this
    // one got from another machine (#130); nobody else sees it.
    if protocol {
        for e in &mut entries {
            e.own_updated_at = e.usage.own_updated_at;
        }
    }

    let mut out = std::io::stdout().lock();
    if args.json || args.json_v2 {
        let rows: Vec<&Entry> = entries
            .iter()
            .chain(&api_entries)
            .chain(devin.as_ref())
            .collect();
        let mut extra = Vec::new();
        if protocol {
            extra.push(("protocol", serde_json::json!(machines::PROTOCOL)));
        }
        if let Some(views) = &machines {
            // Only with machines: this machine's own last update, as
            // each of `machines[]` has its own.
            let at = local_updated.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            extra.push(("updated_at", serde_json::json!(at)));
            extra.push(("machines", serde_json::to_value(views)?));
        }
        super::write_json_with(&mut out, "profiles", &rows, args.json_v2, &extra)?;
    } else {
        // Each line, and whether it counts for the layout's width: a
        // machine's heading doesn't (its error may be long), and is drawn
        // as a rule as wide as the tables once the layout is known. Nor
        // does an API KEY block: no layout narrows it, so a row wider than
        // the terminal would only push the table above it into a narrower
        // layout than it needs (#132).
        let lines = |l: Layout| {
            let mut lines: Vec<(Line, bool)> = Vec::new();
            if machines.is_some() {
                lines.push((
                    Line::Heading(heading(LOCAL_MACHINE, None, local_updated, None, now)),
                    false,
                ));
            }
            if !entries.is_empty() {
                lines.extend(
                    render(&entries, l, show_email, now)
                        .into_iter()
                        .map(|t| (Line::Text(t), true)),
                );
            }
            if !api_entries.is_empty() {
                if !entries.is_empty() {
                    lines.push((Line::Text(String::new()), false));
                }
                lines.extend(
                    render_api_keys(&api_entries, entries.len(), now)
                        .into_iter()
                        .map(|t| (Line::Text(t), false)),
                );
            }
            if let Some(d) = &devin {
                if !entries.is_empty() || !api_entries.is_empty() {
                    lines.push((Line::Text(String::new()), false));
                }
                lines.extend(
                    render_devin(d, entries.len() + api_entries.len(), l, now)
                        .into_iter()
                        .map(|t| (Line::Text(t), true)),
                );
            }
            for v in machines.iter().flatten() {
                lines.push((Line::Text(String::new()), false));
                lines.push((
                    Line::Heading(heading(
                        &v.name,
                        Some(&v.host),
                        v.updated_at,
                        v.error.as_ref(),
                        now,
                    )),
                    false,
                ));
                let n = v
                    .profiles
                    .iter()
                    .take_while(|e| category(e) == (false, false))
                    .count();
                let (profiles, rest) = v.profiles.split_at(n);
                let k = rest
                    .iter()
                    .take_while(|e| category(e) == (false, true))
                    .count();
                let (apis, devins) = rest.split_at(k);
                if !profiles.is_empty() {
                    lines.extend(
                        render(profiles, l, show_email, now)
                            .into_iter()
                            .map(|t| (Line::Text(t), true)),
                    );
                }
                if !apis.is_empty() {
                    if !profiles.is_empty() {
                        lines.push((Line::Text(String::new()), false));
                    }
                    lines.extend(
                        render_api_keys(apis, n, now)
                            .into_iter()
                            .map(|t| (Line::Text(t), false)),
                    );
                }
                for d in devins {
                    if !profiles.is_empty() || !apis.is_empty() {
                        lines.push((Line::Text(String::new()), false));
                    }
                    lines.extend(
                        render_devin(d, n + k, l, now)
                            .into_iter()
                            .map(|t| (Line::Text(t), true)),
                    );
                }
            }
            lines
        };
        // The widest text line: of those that count for the layout, or of all.
        let width = |lines: &[(Line, bool)], all: bool| {
            lines
                .iter()
                .filter_map(|(line, counts)| match line {
                    Line::Text(t) if all || *counts => Some(measure_text_width(t)),
                    _ => None,
                })
                .max()
                .unwrap_or(0)
        };
        let layout = ui::fit_layout(&LAYOUTS, |l| l.bar, |l| width(&lines(l), false));
        let lines = lines(layout);
        // A heading's rule spans the tables (API KEY blocks too), never
        // past the terminal.
        let tables = width(&lines, true);
        let rule_width = ui::terminal_width().map_or(tables, |w| tables.min(w));
        for (line, _) in lines {
            match line {
                Line::Text(t) => writeln!(out, "{t}")?,
                Line::Heading(h) => writeln!(out, "{}", rule(&h, rule_width))?,
            }
        }
        drop(out);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let usages: Vec<&Usage> = entries.iter().map(|e| &e.usage).collect();
        if let Some(hint) = usage::no_data_hint(&names, &usages) {
            ui::hint(&hint);
        }
        if !usage::disabled_by_env() {
            let api_names: Vec<&str> = api_entries.iter().map(|e| e.name.as_str()).collect();
            let api_usages: Vec<&Usage> = api_entries.iter().map(|e| &e.usage).collect();
            if let Some(hint) = usage::no_cost_hint(&api_names, &api_usages) {
                ui::hint(&hint);
            }
        }
        for line in login_warnings(&entries, now, window_days) {
            ui::warn(&line);
        }
        // On macOS a login may live only in the Keychain, which plain
        // `mcc usage` leaves alone.
        let unsure = entries
            .iter()
            .any(|e| e.login_warning || e.login_expires_at.is_none());
        if cfg!(target_os = "macos") && !allow_keychain && unsure && !usage::disabled_by_env() {
            ui::hint(
                "mcc read login expiry from each profile's .credentials.json only; `mcc usage --all` also checks the macOS Keychain, where Claude Code may keep a newer login",
            );
        }
    }
    if interrupted {
        return Err(CliError {
            code: EXIT_INTERRUPTED,
            message: "interrupted".into(),
            hint: None,
        }
        .into());
    }
    Ok(())
}

/// Whether this `mcc usage` reads the other machines set up (#130): only
/// with `--remote` (SSH is slow, so it is never the default), for every
/// profile of this machine as a table or `--json-v2`, usage not turned
/// off. `mcc usage PROFILE`, `--only`, `--json` (schema 1) and another
/// machine's own read (the machine protocol) read this machine alone.
pub(crate) fn fans_out(cfg: &Config, args: &UsageArgs) -> bool {
    args.remote
        && !cfg.machines.is_empty()
        && args.profile.is_none()
        && args.only.is_none()
        && !args.json
        && !args.machine_protocol
        && !usage::disabled_by_env()
}

/// The entries `--only` names (`claude` stands for the default). A name
/// that is no profile here is skipped, and logged: it may have been
/// removed since the machine reading this one listed it.
fn only_these(entries: Vec<Entry>, only: &[String]) -> Vec<Entry> {
    let wanted = |name: &str| {
        only.iter()
            .any(|o| o == name || (o == "claude" && name == "default"))
    };
    for o in only {
        if !entries
            .iter()
            .any(|e| e.name == *o || (o == "claude" && e.name == "default"))
        {
            crate::log::write(
                "INFO",
                &format!("usage --only: no profile named {o}; skipped"),
            );
        }
    }
    entries.into_iter().filter(|e| wanted(&e.name)).collect()
}

/// Another machine's table (#130), as shown and as `--json-v2`'s
/// `machines[]`.
#[derive(Debug, Serialize)]
struct MachineView {
    name: String,
    host: String,
    /// The newest of its own numbers (before any newer ones from another
    /// machine replace them); null when it has none.
    #[serde(serialize_with = "ser_time")]
    updated_at: Option<DateTime<Utc>>,
    /// Why it couldn't be read (or refreshed), as a code; null when it was.
    #[serde(serialize_with = "ser_error")]
    error: Option<MachineError>,
    profiles: Vec<Entry>,
}

fn ser_error<S: serde::Serializer>(e: &Option<MachineError>, s: S) -> Result<S::Ok, S::Error> {
    match e {
        Some(e) => s.serialize_str(e.code()),
        None => s.serialize_none(),
    }
}

/// Usage with other machines (#130): read each of them over SSH, and this
/// machine's profiles as `[usage] live` says. With `--all`, look each
/// account up live once ([`machines::select`]): this machine's accounts
/// here, every other account on one machine that has it, with `mcc usage
/// --all --only …` there, while this machine's lookups run. Then each row
/// of an account gets its newest numbers from any machine, which are saved
/// to this machine's cache when newer. Returns the other machines' tables,
/// this machine's own last update (from its own numbers, before any are
/// replaced), and whether Ctrl+C stopped a refresh here.
fn lookup_with_machines(
    paths: &Paths,
    cfg: &Config,
    args: &UsageArgs,
    entries: &mut [Entry],
    with_devin: bool,
) -> (Vec<MachineView>, Option<DateTime<Utc>>, bool) {
    if args.all {
        eprintln!("{USAGE_ALL_NOTE}");
        if with_devin {
            eprintln!("{USAGE_ALL_DEVIN_NOTE}");
        }
        eprintln!("{USAGE_ALL_MACHINES_NOTE}");
        crate::log::write(
            "INFO",
            "mcc usage --all: live lookup for every account, on this machine and the others",
        );
    }
    let configured: Vec<(&str, &crate::config::Machine)> =
        cfg.machines.iter().map(|(n, m)| (n.as_str(), m)).collect();
    let first: Vec<machines::Request> = configured
        .iter()
        .map(|&(name, m)| machines::Request::new(name, m, None))
        .collect();
    let mut views: Vec<MachineView> = configured
        .iter()
        .zip(machines::ask_all::<Entry>(&first, machines::READ_TIMEOUT))
        .map(|((name, m), answer)| {
            let (profiles, error) = match answer {
                Ok(rows) => (
                    rows.into_iter()
                        .map(|e| e.received(name, Utc::now()))
                        .collect(),
                    None,
                ),
                Err(e) => (Vec::new(), Some(e)),
            };
            MachineView {
                name: name.to_string(),
                host: m.host.clone(),
                updated_at: None,
                error,
                profiles,
            }
        })
        .collect();

    let mut interrupted = false;
    if args.all {
        let now = Utc::now();
        // Devin rows of other machines keep what they had: not an account
        // machines share.
        let remote: Vec<(usize, usize)> = views
            .iter()
            .enumerate()
            .flat_map(|(v, view)| {
                view.profiles
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.provider == Provider::Claude && e.auth.is_none())
                    .map(move |(r, _)| (v, r))
            })
            .collect();
        let candidates: Vec<machines::Candidate> = entries
            .iter()
            .map(|e| e.candidate(true, now))
            .chain(
                remote
                    .iter()
                    .map(|&(v, r)| views[v].profiles[r].candidate(false, now)),
            )
            .collect();
        let chosen = machines::select(&candidates);
        let (here, there) = chosen.split_at(entries.len());
        let mut only: Vec<Vec<String>> = vec![Vec::new(); views.len()];
        for (&(v, r), _) in remote.iter().zip(there).filter(|(_, c)| **c) {
            only[v].push(views[v].profiles[r].name.clone());
        }
        let second: Vec<machines::Request> = configured
            .iter()
            .zip(&only)
            .filter(|(_, names)| !names.is_empty())
            .map(|(&(name, m), names)| machines::Request::new(name, m, Some(names)))
            .collect();
        let (live, cached): (Vec<usize>, Vec<usize>) = (0..entries.len()).partition(|&i| here[i]);
        let live_targets: Vec<usage::Target> = live.iter().map(|&i| entries[i].target()).collect();
        let cached_targets: Vec<usage::Target> =
            cached.iter().map(|&i| entries[i].target()).collect();
        let answers = std::thread::scope(|s| {
            // The other machines' lookups run while this one's do; theirs
            // may refresh a token too, so they get its budget and more.
            let there = s.spawn(|| {
                machines::ask_all::<Entry>(
                    &second,
                    crate::refresh::RUN_BUDGET + machines::REFRESH_MARGIN,
                )
            });
            let mut found = if live_targets.is_empty() {
                Vec::new()
            } else {
                usage::fetch_all(&live_targets)
            };
            interrupted = crate::refresh::refresh_expired(paths, cfg, &live_targets, &mut found);
            usage::save_to_cache(paths, &live_targets, &found);
            usage::stamp_live(&mut found, Utc::now());
            for (&i, u) in live.iter().zip(found) {
                entries[i].usage = u;
            }
            for (&i, u) in cached.iter().zip(usage::read_all(paths, &cached_targets)) {
                entries[i].usage = u;
            }
            there.join().unwrap_or_default()
        });
        for (req, answer) in second.iter().zip(answers) {
            let Some(view) = views.iter_mut().find(|v| v.name == req.name) else {
                continue;
            };
            match answer {
                Ok(rows) => {
                    for row in rows.into_iter().map(|e| e.received(req.name, Utc::now())) {
                        if let Some(old) = view.profiles.iter_mut().find(|e| e.name == row.name) {
                            *old = row;
                        }
                    }
                }
                // What it had cached stays, with why it isn't newer.
                Err(e) => view.error = Some(e),
            }
        }
    } else {
        let targets: Vec<usage::Target> = entries.iter().map(Entry::target).collect();
        let mut found = usage::lookup(paths, &targets, cfg.usage.live);
        if cfg.usage.live {
            usage::stamp_live(&mut found, Utc::now());
        }
        for (e, u) in entries.iter_mut().zip(found) {
            e.usage = u;
        }
    }
    // Each machine's own last update: never numbers it got from another
    // machine (its cache keeps its own time beside them), nor those the
    // merge below puts in its rows.
    for v in &mut views {
        v.updated_at = v
            .profiles
            .iter()
            .filter_map(|e| machines::own_time(&e.usage))
            .max();
    }
    let local_updated = entries
        .iter()
        .filter_map(|e| machines::own_time(&e.usage))
        .max();
    merge_accounts(paths, entries, &mut views);
    (views, local_updated, interrupted)
}

/// Every row of an account, on any machine, gets the account's newest
/// numbers (AC5 of #130) and the other machines it is on (`also_on`);
/// this machine's profiles also save those numbers to their cache when
/// they are newer than what it holds, so `mcc list`, `mcc run` and the
/// status line show them too.
fn merge_accounts(paths: &Paths, entries: &mut [Entry], views: &mut [MachineView]) {
    let rows = || {
        entries
            .iter()
            .map(|e| (LOCAL_MACHINE, e))
            .chain(
                views
                    .iter()
                    .flat_map(|v| v.profiles.iter().map(move |e| (v.name.as_str(), e))),
            )
            .filter(|(_, e)| e.provider == Provider::Claude)
    };
    let mut on: HashMap<String, Vec<String>> = HashMap::new();
    for (machine, e) in rows() {
        if let Some(id) = &e.account_id {
            let list = on.entry(id.clone()).or_default();
            if !list.iter().any(|m| m == machine) {
                list.push(machine.to_string());
            }
        }
    }
    fn points<'a>((_, e): (&str, &'a Entry)) -> Option<(&'a str, &'a Usage)> {
        Some((e.account_id.as_deref()?, &e.usage))
    }
    let best: HashMap<String, Usage> = machines::newest(rows().filter_map(points))
        .into_iter()
        .map(|(id, u)| (id.to_string(), u.clone()))
        .collect();
    // Each account's newest point on this machine: the best is this
    // machine's own when it is as new (and was never another machine's).
    let here: HashMap<String, DateTime<Utc>> = machines::newest(
        rows()
            .filter(|(m, _)| *m == LOCAL_MACHINE)
            .filter_map(points),
    )
    .into_iter()
    .filter_map(|(id, u)| Some((id.to_string(), machines::time_of(u)?)))
    .collect();
    let update = |machine: &str, e: &mut Entry| {
        if e.provider != Provider::Claude {
            return;
        }
        let Some(id) = e.account_id.as_deref() else {
            e.also_on = Some(Vec::new());
            return;
        };
        e.also_on = Some(
            on.get(id)
                .into_iter()
                .flatten()
                .filter(|m| *m != machine)
                .cloned()
                .collect(),
        );
        if let Some(b) = best.get(id) {
            let own = machines::time_of(&e.usage);
            let newer = machines::time_of(b);
            if own.is_none_or(|t| newer.is_some_and(|n| n > t)) {
                let mut u = b.clone();
                // This machine's row keeps its own last update beside
                // another machine's numbers, in its cache too.
                if machine == LOCAL_MACHINE {
                    let from_here = b.own_updated_at.is_none() && here.get(id) == newer.as_ref();
                    u.own_updated_at = (!from_here).then(|| machines::own_time(&e.usage));
                }
                e.usage = u;
            }
        }
    };
    for e in entries.iter_mut() {
        update(LOCAL_MACHINE, e);
        if e.account_id.is_some() {
            usage::save_point(paths, &e.name, &e.usage);
        }
    }
    for v in views.iter_mut() {
        for e in &mut v.profiles {
            update(&v.name, e);
        }
    }
}

/// One line of `mcc usage`'s table output: a table's text as it is, or a
/// machine's heading, drawn by [`rule`] once the tables' width is known.
enum Line {
    Text(String),
    Heading(String),
}

/// A machine's heading as a rule `width` columns wide, so each machine's
/// tables stand apart: the heading in bold between dim `─` lines. A
/// heading too long for the rule (a long error) gets no tail.
///
/// ```text
/// ── lab (me@lab-box) · updated 12m ago ──────────────────────────────
/// ```
fn rule(text: &str, width: usize) -> String {
    let lead = "── ";
    let fill = width.saturating_sub(lead.chars().count() + measure_text_width(text) + 1);
    let mut line = format!("{}{}", style(lead).dim(), style(text).bold());
    if fill > 0 {
        line.push_str(&format!(" {}", style("─".repeat(fill)).dim()));
    }
    line
}

/// A machine's heading over its table (#130): its name (and host), and
/// when its numbers were last updated, or "—" and why it couldn't be read.
/// [`rule`] draws it.
///
/// ```text
/// lab (me@lab-box) · updated 12m ago
/// box (box) · updated — · ssh could not connect to box without a prompt: …
/// ```
fn heading(
    name: &str,
    host: Option<&str>,
    updated: Option<DateTime<Utc>>,
    error: Option<&MachineError>,
    now: DateTime<Utc>,
) -> String {
    let mut text = match host {
        Some(h) => format!("{name} ({h})"),
        None => name.to_string(),
    };
    text.push_str(&format!(
        " · updated {}",
        updated.map_or_else(|| "—".into(), |t| usage::age(t, now))
    ));
    if let Some(e) = error {
        text.push_str(&format!(" · {}", e.describe(name, host.unwrap_or(name))));
    }
    text
}

/// Order `entries` as `order` says (#126): alphabetically by name, or by
/// a window's percent used or reset time, least to most — a reset order
/// puts the soonest reset first. An entry without the order's key (no
/// such window, or no reset time for it) keeps its row but sorts last;
/// `sort_by` is stable, so entries that tie keep their previous order.
fn sort_entries(entries: &mut [Entry], order: usage::SortOrder) {
    match order {
        usage::SortOrder::Name => entries.sort_by(|a, b| a.name.cmp(&b.name)),
        usage::SortOrder::FiveHour => entries.sort_by(|a, b| {
            by_key(
                a.usage.five_hour.as_ref().map(|w| w.percent),
                b.usage.five_hour.as_ref().map(|w| w.percent),
                |x, y| x.total_cmp(y),
            )
        }),
        usage::SortOrder::FiveHourReset => entries.sort_by(|a, b| {
            by_key(
                a.usage.five_hour.as_ref().and_then(|w| w.resets_at),
                b.usage.five_hour.as_ref().and_then(|w| w.resets_at),
                Ord::cmp,
            )
        }),
        usage::SortOrder::Weekly => entries.sort_by(|a, b| {
            by_key(
                a.usage.weekly.as_ref().map(|w| w.percent),
                b.usage.weekly.as_ref().map(|w| w.percent),
                |x, y| x.total_cmp(y),
            )
        }),
        usage::SortOrder::WeeklyReset => entries.sort_by(|a, b| {
            by_key(
                a.usage.weekly.as_ref().and_then(|w| w.resets_at),
                b.usage.weekly.as_ref().and_then(|w| w.resets_at),
                Ord::cmp,
            )
        }),
    }
}

/// Two sort keys compared with a missing key last: `None` after every
/// `Some`; `cmp` orders two present keys.
fn by_key<T>(a: Option<T>, b: Option<T>, cmp: impl FnOnce(&T, &T) -> Ordering) -> Ordering {
    match (&a, &b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => cmp(x, y),
    }
}

/// One warning per profile whose login runs out within `window_days` (or
/// has run out), naming it and how to renew it; none for the others.
fn login_warnings(entries: &[Entry], now: DateTime<Utc>, window_days: u32) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| {
            let at = e.login_expires_at?;
            let left = usage::login_warning(at, now, window_days)?;
            let name = &e.name;
            Some(if left <= chrono::Duration::zero() {
                format!("Login for '{name}' has expired — run `mcc login {name}` to sign in again")
            } else {
                format!(
                    "Login for '{name}' expires in {} ({}) — run `mcc login {name}` to renew it",
                    usage::until(at, now),
                    usage::local_time(at)
                )
            })
        })
        .collect()
}

/// The LAST ACTIVE cell: "3h ago", or "unknown".
fn activity_cell(e: &Entry, now: DateTime<Utc>) -> String {
    e.last_activity
        .map_or_else(|| "unknown".into(), |t| usage::age(t, now))
}

/// The LOGIN ENDS cell: the time left ("27d 3h"), "expired", or "unknown".
fn login_cell(e: &Entry, now: DateTime<Utc>) -> String {
    match e.login_expires_at {
        Some(t) if t <= now => "expired".into(),
        Some(t) => usage::until(t, now),
        None => "unknown".into(),
    }
}

/// One numbered row per profile under a header, laid out as `layout`:
///
/// ```text
/// # PROFILE    LAST ACTIVE  LOGIN ENDS  5-HOUR          RESETS IN                  WEEKLY          RESETS IN                 UPDATED
/// 1 default ★  2h ago       27d 3h      ████░░░░░░ 42%  2h 14m (Sat Oct 10 16:00)  ██░░░░░░░░ 18%  3d 4h (Wed Oct 14 00:00)  12m ago
/// 2 lab        unknown      unknown     — (no usage cached; run /usage)
/// ```
///
/// An ACCOUNT column follows PROFILE when `show_email` is set, then ALSO ON
/// when an account here is signed in on other machines too (#130): their
/// names, "—" for the rows of no other machine. LAST ACTIVE
/// and LOGIN ENDS lead the usage, so a profile without usage (an idle
/// one, typically) shows them too. UPDATED, the age of usage read from the
/// statusline cache, is there only for cached usage, and only when `layout`
/// keeps it. A profile without usage has its reason in place of the usage
/// columns.
fn render(entries: &[Entry], layout: Layout, show_email: bool, now: DateTime<Utc>) -> Vec<String> {
    let profile_head = super::list::indexed("PROFILE", None, entries.len());
    let mut headers = vec![profile_head.as_str()];
    if show_email {
        headers.push("ACCOUNT");
    }
    let also_on = entries
        .iter()
        .any(|e| e.also_on.as_ref().is_some_and(|m| !m.is_empty()));
    if also_on {
        headers.push("ALSO ON");
    }
    headers.extend(["LAST ACTIVE", "LOGIN ENDS"]);
    headers.extend(["5-HOUR", "RESETS IN", "WEEKLY", "RESETS IN"]);
    let cached = layout.updated && entries.iter().any(|e| e.usage.updated_at.is_some());
    if cached {
        headers.push("UPDATED");
    }
    // Each row: the leading cells, then either all four usage cells or one
    // reason that spans them.
    let rows: Vec<Vec<String>> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let name = if e.is_global_default {
                format!("{} ★", e.name)
            } else {
                e.name.clone()
            };
            let mut cells = vec![super::list::indexed(&name, Some(i), entries.len())];
            if show_email {
                cells.push(e.account.clone().unwrap_or_else(|| "—".into()));
            }
            if also_on {
                cells.push(match e.also_on.as_deref() {
                    Some(m) if !m.is_empty() => m.join(", "),
                    _ => "—".into(),
                });
            }
            cells.extend([activity_cell(e, now), login_cell(e, now)]);
            let u = &e.usage;
            match (u.status, u.reason) {
                (Status::Ok, _) => {
                    for w in [u.five_hour.as_ref(), u.weekly.as_ref()] {
                        let (used, resets) = window(w, layout, now);
                        cells.extend([used, resets]);
                    }
                    if cached {
                        cells.push(u.age(now).unwrap_or_default());
                    }
                }
                (_, Some(Reason::NoData)) => cells.push(NO_DATA_CELL.into()),
                (_, Some(r @ Reason::Disabled)) => {
                    cells.push(format!("— ({})", r.describe()));
                }
                (_, reason) => cells.push(format!(
                    "unavailable ({})",
                    reason.map_or("unknown", Reason::describe)
                )),
            }
            cells
        })
        .collect();
    lay_out(&headers, &rows)
}

/// Devin's block under the profiles' table (#124), laid out as `layout`:
///
/// ```text
/// # PROVIDER  DAILY           RESETS IN  WEEKLY          RESETS IN  OVERAGE  UPDATED
/// 3 devin     █░░░░░░░░░ 12%  8h 0m      █████░░░░░ 54%  3d 4h      $3.50    12m ago
/// ```
///
/// Devin has a daily window, not a 5-hour one ("—" when its plan hides
/// it), and an overage balance (shown when positive). Without usage, the
/// reason spans the usage columns. Its `#` carries on from the `after`
/// profile rows above it.
fn render_devin(e: &Entry, after: usize, layout: Layout, now: DateTime<Utc>) -> Vec<String> {
    let u = &e.usage;
    let provider_head = super::list::indexed("PROVIDER", None, after + 1);
    let mut headers = vec![
        provider_head.as_str(),
        "DAILY",
        "RESETS IN",
        "WEEKLY",
        "RESETS IN",
    ];
    let overage = u.overage_usd.filter(|o| *o > 0.0);
    if overage.is_some() {
        headers.push("OVERAGE");
    }
    let cached = layout.updated && u.updated_at.is_some();
    if cached {
        headers.push("UPDATED");
    }
    let mut cells = vec![super::list::indexed(&e.name, Some(after), after + 1)];
    match (u.status, u.reason) {
        (Status::Ok, _) => {
            for w in [u.daily.as_ref(), u.weekly.as_ref()] {
                let (used, resets) = window(w, layout, now);
                cells.extend([used, resets]);
            }
            if let Some(o) = overage {
                cells.push(devin::dollars(o));
            }
            if cached {
                cells.push(u.age(now).unwrap_or_default());
            }
        }
        (_, Some(r @ (Reason::Disabled | Reason::NotLookedUp))) => {
            cells.push(format!("— ({})", devin::describe(r)));
        }
        (_, reason) => cells.push(format!(
            "unavailable ({})",
            reason.map_or("unknown", devin::describe)
        )),
    }
    lay_out(&headers, &[cells])
}

/// Which block a row goes in: Claude subscription profiles (the table),
/// API-key profiles, then Devin, in that order.
fn category(e: &Entry) -> (bool, bool) {
    (e.provider == Provider::Devin, e.auth.is_some())
}

/// The API-key profiles' block under the profiles' table (#132): no 5-hour
/// or weekly window, their estimated spend this month instead (and the
/// budget, when set). The `#` carries on from the `after` rows above.
///
/// ```text
/// # API KEY  LAST ACTIVE  EST. SPEND (THIS MONTH, THIS MACHINE)
/// 3 api      2h ago       est. $1.23 this month of $50.00 budget (2%)
/// ```
fn render_api_keys(entries: &[Entry], after: usize, now: DateTime<Utc>) -> Vec<String> {
    let total = after + entries.len();
    let head = super::list::indexed("API KEY", None, total);
    let headers = [
        head.as_str(),
        "LAST ACTIVE",
        "EST. SPEND (THIS MONTH, THIS MACHINE)",
    ];
    let rows: Vec<Vec<String>> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            vec![
                super::list::indexed(&e.name, Some(after + i), total),
                activity_cell(e, now),
                usage::spend_text(&e.usage),
            ]
        })
        .collect();
    lay_out(&headers, &rows)
}

/// The bold header and the rows, each column as wide as its widest cell.
/// A row shorter than the header ends in a reason spanning the remaining
/// columns, which doesn't widen the column it starts in.
fn lay_out(headers: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            rows.iter()
                .filter(|r| r.len() == headers.len() || i + 1 < r.len())
                .map(|r| measure_text_width(&r[i]))
                .chain([headers[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let fmt_row = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = widths[i].saturating_sub(measure_text_width(c));
                format!("{c}{}", " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let head: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    std::iter::once(style(fmt_row(&head)).bold().to_string())
        .chain(rows.iter().map(|r| fmt_row(r)))
        .collect()
}

/// The two cells for one window: "████░░░░░░ 42%" and
/// "2h 14m (Sat Oct 10 16:00)", or "—" and "" when the API reported no such
/// window. The bar and the absolute reset time follow `layout`; the time
/// until the reset is always there.
fn window(w: Option<&Window>, layout: Layout, now: DateTime<Utc>) -> (String, String) {
    let Some(w) = w else {
        return ("—".into(), String::new());
    };
    let resets = match w.resets_at {
        Some(t) if layout.reset_time => {
            format!("{} ({})", usage::until(t, now), usage::local_time(t))
        }
        Some(t) => usage::until(t, now),
        None => String::new(),
    };
    (ui::gauge(w.percent, layout.bar), resets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn plain(s: &str) -> String {
        console::strip_ansi_codes(s).into_owned()
    }

    #[test]
    fn window_cells() {
        let now = at("2026-10-10T12:00:00Z");
        let reset = at("2026-10-10T14:14:00Z");
        let w = |percent, resets_at| Window { percent, resets_at };
        let cells = |w: Option<&Window>, l: Layout| {
            let (used, resets) = window(w, l, now);
            (plain(&used), resets)
        };
        assert_eq!(
            cells(Some(&w(42.4, None)), LAYOUTS[0]),
            ("████░░░░░░ 42%".into(), String::new())
        );
        assert_eq!(
            cells(Some(&w(96.0, Some(reset))), LAYOUTS[0]),
            (
                "██████████ 96%!!".into(),
                format!("2h 14m ({})", usage::local_time(reset))
            )
        );
        // Compact layouts drop the absolute reset time, never the relative one.
        assert_eq!(
            cells(Some(&w(42.4, Some(reset))), LAYOUTS[2]),
            ("██░░░ 42%".into(), "2h 14m".into())
        );
        assert_eq!(
            cells(Some(&w(85.0, Some(reset))), LAYOUTS[4]),
            ("85%!".into(), "2h 14m".into())
        );
        assert_eq!(cells(None, LAYOUTS[0]), ("—".into(), String::new()));
    }

    fn entry(name: &str, account: Option<&str>, usage: Usage) -> Entry {
        Entry {
            name: name.into(),
            command: name.into(),
            is_default: name == "default",
            account: account.map(str::to_string),
            usage,
            last_activity: None,
            login_expires_at: None,
            login_expires_source: None,
            login_warning: false,
            is_global_default: name == "default",
            provider: Provider::Claude,
            account_id: None,
            also_on: None,
            own_updated_at: None,
            auth: None,
            config_dir: PathBuf::new(),
            budget_usd: None,
        }
    }

    /// T5 of #132: an API-key row, as this machine sends it to another
    /// (`--machine-protocol` is `mcc usage --json`), reads through the
    /// `Entry` and `Usage` of an mcc from before API-key profiles, whose
    /// enums are frozen here: it carries only status and reason codes those
    /// know (an older mcc drops a row it can't read), and only additive
    /// fields.
    #[test]
    fn api_key_rows_read_as_before_on_an_older_mcc() {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum OldStatus {
            Ok,
            Unavailable,
        }
        #[derive(Deserialize, Debug, PartialEq)]
        #[serde(rename_all = "snake_case")]
        enum OldReason {
            NotLoggedIn,
            TokenExpired,
            Unauthorized,
            RateLimited,
            Network,
            HttpError,
            ParseError,
            Disabled,
            NoData,
            NotLookedUp,
            NoOrganizations,
            MissingToken,
            PermissionDenied,
            DecodeFailed,
            MissingQuota,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum OldSource {
            Cache,
            Live,
        }
        #[derive(Deserialize, Debug, PartialEq)]
        #[serde(rename_all = "snake_case")]
        enum OldProvider {
            Claude,
            Devin,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum OldDeadlineSource {
            CredentialsFile,
            Keychain,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldWindow {
            percent: f64,
            resets_at: Option<String>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldUsage {
            status: OldStatus,
            reason: Option<OldReason>,
            five_hour: Option<OldWindow>,
            weekly: Option<OldWindow>,
            source: Option<OldSource>,
            #[serde(default)]
            updated_at: Option<String>,
            daily: Option<OldWindow>,
            overage_usd: Option<f64>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldEntry {
            name: String,
            command: String,
            is_default: bool,
            account: Option<String>,
            usage: OldUsage,
            #[serde(default)]
            last_activity: Option<String>,
            #[serde(default)]
            login_expires_at: Option<String>,
            login_expires_source: Option<OldDeadlineSource>,
            login_warning: bool,
            is_global_default: bool,
            provider: OldProvider,
            #[serde(default)]
            account_id: Option<String>,
            #[serde(default)]
            also_on: Option<Vec<String>>,
            #[serde(default)]
            own_updated_at: Option<Option<String>>,
        }

        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let now = Utc::now();
        crate::cost::record(
            &paths,
            "api",
            &serde_json::json!({"session_id": "s", "cost": {"total_cost_usd": 0.0}}),
            now,
        );
        crate::cost::record(
            &paths,
            "api",
            &serde_json::json!({"session_id": "s", "cost": {"total_cost_usd": 2.0}}),
            now,
        );
        for (live, reason) in [(false, OldReason::NoData), (true, OldReason::NotLoggedIn)] {
            let mut e = entry("api", None, Usage::default());
            e.auth = Some(AuthKind::ApiKey);
            e.budget_usd = Some(10.0);
            e.usage = usage::api_key_usage(&paths, &e.target(), live);
            let v = serde_json::to_value(&e).unwrap();
            assert_eq!(v["auth"], "api_key");
            assert_eq!(v["usage"]["est_cost_usd"], 2.0);
            assert_eq!(v["usage"]["budget_usd"], 10.0);
            let old: OldEntry = serde_json::from_value(v.clone())
                .unwrap_or_else(|err| panic!("an older mcc can't read {v}: {err}"));
            assert!(matches!(old.usage.status, OldStatus::Unavailable));
            assert_eq!(old.usage.reason, Some(reason));
            assert_eq!(old.provider, OldProvider::Claude);
        }
        // Every other row keeps its exact key set.
        let v = serde_json::to_value(entry("work", None, Usage::default())).unwrap();
        assert!(v.get("auth").is_none(), "{v}");
        assert!(v["usage"].get("est_cost_usd").is_none(), "{v}");
        assert!(v["usage"].get("budget_usd").is_none(), "{v}");
    }

    #[test]
    fn table_rows() {
        let now = at("2026-10-10T12:00:00Z");
        let reset = Some(at("2026-10-10T14:14:00Z"));
        let ok = Usage {
            status: Status::Ok,
            reason: None,
            five_hour: Some(Window {
                percent: 42.4,
                resets_at: reset,
            }),
            weekly: Some(Window {
                percent: 18.0,
                resets_at: None,
            }),
            source: Some(usage::Source::Cache),
            updated_at: Some(at("2026-10-10T11:48:00Z")),
            daily: None,
            overage_usd: None,
            est_cost_usd: None,
            budget_usd: None,
            own_updated_at: None,
        };
        let mut entries = [
            entry("default", Some("me@example.com"), ok),
            entry("lab", None, Usage::unavailable(Reason::NotLoggedIn)),
        ];
        entries[0].last_activity = Some(at("2026-10-10T09:30:00Z"));
        entries[0].login_expires_at = Some(at("2026-11-06T15:00:00Z"));
        let lines = |email| -> Vec<String> {
            render(&entries, LAYOUTS[1], email, now)
                .iter()
                .map(|l| plain(l))
                .collect()
        };
        assert_eq!(
            lines(false),
            [
                "# PROFILE    LAST ACTIVE  LOGIN ENDS  5-HOUR          RESETS IN  WEEKLY          RESETS IN  UPDATED",
                "1 default ★  2h ago       27d 3h      ████░░░░░░ 42%  2h 14m     ██░░░░░░░░ 18%             12m ago",
                "2 lab        unknown      unknown     unavailable (not logged in)",
            ]
        );
        assert_eq!(
            lines(true),
            [
                "# PROFILE    ACCOUNT         LAST ACTIVE  LOGIN ENDS  5-HOUR          RESETS IN  WEEKLY          RESETS IN  UPDATED",
                "1 default ★  me@example.com  2h ago       27d 3h      ████░░░░░░ 42%  2h 14m     ██░░░░░░░░ 18%             12m ago",
                "2 lab        —               unknown      unknown     unavailable (not logged in)",
            ]
        );
        // With no usage at all, the leading columns still line up.
        let lines: Vec<String> = render(&entries[1..], LAYOUTS[0], true, now)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(
            lines,
            [
                "# PROFILE  ACCOUNT  LAST ACTIVE  LOGIN ENDS  5-HOUR  RESETS IN  WEEKLY  RESETS IN",
                "1 lab      —        unknown      unknown     unavailable (not logged in)",
            ]
        );
        let mut both = vec![entry("default", Some("me@example.com"), Usage::default())];
        both.extend(entries.into_iter().skip(1));
        // A login past its deadline says so.
        both[0].login_expires_at = Some(at("2026-10-10T11:00:00Z"));
        let lines: Vec<String> = render(&both, LAYOUTS[0], false, now)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(
            lines,
            [
                "# PROFILE    LAST ACTIVE  LOGIN ENDS  5-HOUR  RESETS IN  WEEKLY  RESETS IN",
                "1 default ★  unknown      expired     — (not fetched: --no-usage or MCC_NO_USAGE)",
                "2 lab        unknown      unknown     unavailable (not logged in)",
            ]
        );
        // No cached usage: a dash and how to get some, in place of the usage.
        let none = [entry(
            "idle",
            None,
            Usage::unavailable(Reason::NoData).with_source(usage::Source::Cache),
        )];
        let lines: Vec<String> = render(&none, LAYOUTS[0], false, now)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(
            lines[1],
            "1 idle     unknown      unknown     — (no usage cached; run /usage)"
        );
    }

    #[test]
    fn sort_entries_orders_by_the_key_with_missing_values_last() {
        let w = |percent: f64, resets_at: Option<DateTime<Utc>>| Window { percent, resets_at };
        let entries = || {
            vec![
                // 5h 42.4% resets at 13:00; week 18% resets at +5d.
                entry(
                    "default",
                    None,
                    Usage::ok(
                        Some(w(42.4, Some(at("2026-10-10T13:00:00Z")))),
                        Some(w(18.0, Some(at("2026-10-15T12:00:00Z")))),
                    ),
                ),
                // 5h 7% resets at 16:00; week 64% resets at +1d.
                entry(
                    "work",
                    None,
                    Usage::ok(
                        Some(w(7.0, Some(at("2026-10-10T16:00:00Z")))),
                        Some(w(64.0, Some(at("2026-10-11T12:00:00Z")))),
                    ),
                ),
                // A 5-hour window with no reset time; no weekly window.
                entry("lab", None, Usage::ok(Some(w(90.0, None)), None)),
                // No usage at all.
                entry("idle", None, Usage::unavailable(Reason::NoData)),
            ]
        };
        let sorted = |order: usage::SortOrder| {
            let mut entries = entries();
            sort_entries(&mut entries, order);
            entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            sorted(usage::SortOrder::Name),
            ["default", "idle", "lab", "work"]
        );
        // lab has a 5-hour percent, so it counts on the 5-hour order;
        // idle has none and goes last.
        assert_eq!(
            sorted(usage::SortOrder::FiveHour),
            ["work", "default", "lab", "idle"]
        );
        // Soonest reset first; lab's window has no reset time, so both it
        // and idle sort last, in their previous order.
        assert_eq!(
            sorted(usage::SortOrder::FiveHourReset),
            ["default", "work", "lab", "idle"]
        );
        assert_eq!(
            sorted(usage::SortOrder::Weekly),
            ["default", "work", "lab", "idle"]
        );
        assert_eq!(
            sorted(usage::SortOrder::WeeklyReset),
            ["work", "default", "lab", "idle"]
        );
    }

    #[test]
    fn login_warnings_name_only_the_profiles_inside_the_window() {
        let now = at("2026-10-10T12:00:00Z");
        let mut entries = [
            entry("default", None, Usage::default()),
            entry("work", None, Usage::default()),
            entry("lab", None, Usage::default()),
            entry("old", None, Usage::default()),
        ];
        // default: unknown; work: 1d 4h left; lab: 20 days; old: past.
        entries[1].login_expires_at = Some(at("2026-10-11T16:00:00Z"));
        entries[2].login_expires_at = Some(at("2026-10-30T12:00:00Z"));
        entries[3].login_expires_at = Some(at("2026-10-09T12:00:00Z"));
        let lines = login_warnings(&entries, now, 2);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with("Login for 'work' expires in 1d 4h (")
                && lines[0].ends_with(") — run `mcc login work` to renew it"),
            "{lines:?}"
        );
        assert_eq!(
            lines[1],
            "Login for 'old' has expired — run `mcc login old` to sign in again"
        );
        // A wider window takes in `lab` too.
        let lines = login_warnings(&entries, now, 14);
        assert_eq!(lines.len(), 2);
        let lines = login_warnings(&entries, now, 21);
        assert_eq!(lines.len(), 3);
        assert!(
            lines[1].starts_with("Login for 'lab' expires in 20d"),
            "{lines:?}"
        );
    }

    #[test]
    fn set_login_flags_the_warning_from_the_deadline() {
        let now = at("2026-10-10T12:00:00Z");
        let mut e = entry("work", None, Usage::default());
        e.set_login(
            LoginDeadline::Known {
                at: at("2026-10-11T12:00:00Z"),
                source: DeadlineSource::CredentialsFile,
            },
            now,
            2,
        );
        assert!(e.login_warning);
        assert_eq!(
            e.login_expires_source,
            Some(DeadlineSource::CredentialsFile)
        );
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["login_expires_at"], "2026-10-11T12:00:00Z");
        assert_eq!(v["login_expires_source"], "credentials_file");
        assert_eq!(v["login_warning"], true);
        e.set_login(
            LoginDeadline::Unknown(usage::UnknownDeadline::NoCredentials),
            now,
            2,
        );
        assert!(!e.login_warning);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["login_expires_at"], serde_json::Value::Null);
        assert_eq!(v["login_expires_source"], serde_json::Value::Null);
        assert_eq!(v["last_activity"], serde_json::Value::Null);
    }
}
