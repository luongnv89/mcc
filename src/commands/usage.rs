//! `mcc usage` — each profile's 5-hour and weekly usage in detail (text or JSON),
//! with when it was last used and when its login runs out (#100).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use console::{measure_text_width, style};
use serde::Serialize;

use crate::cli::UsageArgs;
use crate::config::Config;
use crate::metrics;
use crate::paths::Paths;
use crate::ui;
use crate::usage::{self, DeadlineSource, LoginDeadline, Reason, Status, Usage, Window};

/// Printed on stderr each time `mcc usage --all` looks every account's
/// usage up live (even with `--quiet`; never with `MCC_NO_USAGE`).
pub const USAGE_ALL_NOTE: &str = crate::cli::usage_all_note!();

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
/// after `usage` were added in #100 (schema 1 only gains fields).
#[derive(Debug, Serialize)]
struct Entry {
    name: String,
    command: String,
    is_default: bool,
    account: Option<String>,
    usage: Usage,
    /// When the profile was last used for a conversation (its history or
    /// transcripts); null when unknown.
    #[serde(serialize_with = "ser_time")]
    last_activity: Option<DateTime<Utc>>,
    /// When the login needs a new sign-in; null when unknown.
    #[serde(serialize_with = "ser_time")]
    login_expires_at: Option<DateTime<Utc>>,
    /// Where that was read: `credentials_file` or `keychain`.
    login_expires_source: Option<DeadlineSource>,
    /// The login runs out within `[usage] login_warn_days` (or has).
    login_warning: bool,
    #[serde(skip)]
    config_dir: PathBuf,
}

fn ser_time<S: serde::Serializer>(t: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
    match t {
        Some(t) => s.serialize_str(&t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        None => s.serialize_none(),
    }
}

impl Entry {
    fn new(name: &str, command: &str, is_default: bool, dir: &Path, state_json: &Path) -> Self {
        Self {
            name: name.to_string(),
            command: command.to_string(),
            is_default,
            account: metrics::account_email(state_json),
            usage: Usage::default(),
            last_activity: metrics::last_activity(dir).map(DateTime::<Utc>::from),
            login_expires_at: None,
            login_expires_source: None,
            login_warning: false,
            config_dir: dir.to_path_buf(),
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
    let profile =
        |name: &str, dir: &Path| Entry::new(name, name, false, dir, &dir.join(".claude.json"));
    let mut entries = match args.profile.as_deref() {
        None => std::iter::once(default())
            .chain(cfg.profiles.iter().map(|(n, p)| profile(n, &p.dir)))
            .collect(),
        Some("default" | "claude") => vec![default()],
        Some(name) => match cfg.profiles.get(name) {
            Some(p) => vec![profile(name, &p.dir)],
            None => return Err(super::unknown_profile(name)),
        },
    };

    let show_email = (cfg.show_email() || args.show_email) && !args.hide_email;
    if !show_email {
        for e in &mut entries {
            e.account = None;
        }
    }

    // Login expiry: the credentials file's login deadline alone (no token,
    // no access-token expiry, no network); the Keychain only where the live lookup may read it anyway.
    let now = Utc::now();
    let window_days = cfg.usage.login_warn_days();
    let allow_keychain = (cfg.usage.live || args.all) && !usage::disabled_by_env();
    for e in &mut entries {
        let times = usage::login_times(&e.config_dir, e.is_default, allow_keychain);
        e.set_login(times.deadline, now, window_days);
    }

    if usage::disabled_by_env() {
        crate::log::write("INFO", "usage not fetched: MCC_NO_USAGE is set");
    } else {
        // `--all`: every account looked up live for this run only, the
        // default included, whatever `[usage] live` says.
        if args.all {
            eprintln!("{USAGE_ALL_NOTE}");
            crate::log::write("INFO", "mcc usage --all: live lookup for every profile");
        }
        let live = cfg.usage.live || args.all;
        let targets: Vec<usage::Target> = entries
            .iter()
            .map(|e| usage::Target {
                name: e.name.clone(),
                config_dir: e.config_dir.clone(),
                is_default: e.is_default,
            })
            .collect();
        let found = usage::lookup(paths, &targets, live);
        if args.all {
            // So that `mcc usage` without --all shows these numbers next time.
            usage::save_to_cache(paths, &targets, &found);
        }
        for (e, u) in entries.iter_mut().zip(found) {
            e.usage = u;
        }
    }

    let mut out = std::io::stdout().lock();
    if args.json || args.json_v2 {
        super::write_json(&mut out, "profiles", &entries, args.json_v2)?;
    } else {
        let layout = ui::fit_layout(
            &LAYOUTS,
            |l| l.bar,
            |l| {
                render(&entries, l, show_email, now)
                    .iter()
                    .map(|line| measure_text_width(line))
                    .max()
                    .unwrap_or(0)
            },
        );
        for line in render(&entries, layout, show_email, now) {
            writeln!(out, "{line}")?;
        }
        drop(out);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let usages: Vec<&Usage> = entries.iter().map(|e| &e.usage).collect();
        if let Some(hint) = usage::no_data_hint(&names, &usages) {
            ui::hint(&hint);
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
    Ok(())
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

/// One row per profile under a header, laid out as `layout`:
///
/// ```text
/// PROFILE    LAST ACTIVE  LOGIN ENDS  5-HOUR          RESETS IN                  WEEKLY          RESETS IN                 UPDATED
/// default ★  2h ago       27d 3h      ████░░░░░░ 42%  2h 14m (Sat Oct 10 16:00)  ██░░░░░░░░ 18%  3d 4h (Wed Oct 14 00:00)  12m ago
/// lab        unknown      unknown     — (no usage cached; run /usage)
/// ```
///
/// An ACCOUNT column follows PROFILE when `show_email` is set. LAST ACTIVE
/// and LOGIN ENDS lead the usage, so a profile without usage (an idle
/// one, typically) shows them too. UPDATED, the age of usage read from the
/// statusline cache, is there only for cached usage, and only when `layout`
/// keeps it. A profile without usage has its reason in place of the usage
/// columns.
fn render(entries: &[Entry], layout: Layout, show_email: bool, now: DateTime<Utc>) -> Vec<String> {
    let mut headers = vec!["PROFILE"];
    if show_email {
        headers.push("ACCOUNT");
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
        .map(|e| {
            let mut cells = vec![if e.is_default {
                format!("{} ★", e.name)
            } else {
                e.name.clone()
            }];
            if show_email {
                cells.push(e.account.clone().unwrap_or_else(|| "—".into()));
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
    // A spanning reason (a short row's last cell) doesn't widen the column
    // it starts in.
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
            config_dir: PathBuf::new(),
        }
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
                "PROFILE    LAST ACTIVE  LOGIN ENDS  5-HOUR          RESETS IN  WEEKLY          RESETS IN  UPDATED",
                "default ★  2h ago       27d 3h      ████░░░░░░ 42%  2h 14m     ██░░░░░░░░ 18%             12m ago",
                "lab        unknown      unknown     unavailable (not logged in)",
            ]
        );
        assert_eq!(
            lines(true),
            [
                "PROFILE    ACCOUNT         LAST ACTIVE  LOGIN ENDS  5-HOUR          RESETS IN  WEEKLY          RESETS IN  UPDATED",
                "default ★  me@example.com  2h ago       27d 3h      ████░░░░░░ 42%  2h 14m     ██░░░░░░░░ 18%             12m ago",
                "lab        —               unknown      unknown     unavailable (not logged in)",
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
                "PROFILE  ACCOUNT  LAST ACTIVE  LOGIN ENDS  5-HOUR  RESETS IN  WEEKLY  RESETS IN",
                "lab      —        unknown      unknown     unavailable (not logged in)",
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
                "PROFILE    LAST ACTIVE  LOGIN ENDS  5-HOUR  RESETS IN  WEEKLY  RESETS IN",
                "default ★  unknown      expired     — (not fetched: --no-usage or MCC_NO_USAGE)",
                "lab        unknown      unknown     unavailable (not logged in)",
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
            "idle     unknown      unknown     — (no usage cached; run /usage)"
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
