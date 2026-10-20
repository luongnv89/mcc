//! `mcc usage` — each profile's 5-hour and weekly usage in detail (text or JSON).

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
use crate::usage::{self, Reason, Status, Usage, Window};

/// How the table is laid out: the bar width of the usage cells (0 = no bar)
/// and whether the absolute reset time follows the relative one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    bar: usize,
    reset_time: bool,
}

/// Layouts tried, most generous first: the absolute reset time goes first,
/// then the bar shrinks in the same 10/5/0 steps as the `mcc list` table
/// (narrow terminals).
const LAYOUTS: [Layout; 4] = [
    Layout {
        bar: 10,
        reset_time: true,
    },
    Layout {
        bar: 10,
        reset_time: false,
    },
    Layout {
        bar: 5,
        reset_time: false,
    },
    Layout {
        bar: 0,
        reset_time: false,
    },
];

/// One profile's usage, as printed and as serialized by `--json`.
#[derive(Debug, Serialize)]
struct Entry {
    name: String,
    command: String,
    is_default: bool,
    account: Option<String>,
    usage: Usage,
    #[serde(skip)]
    config_dir: PathBuf,
}

impl Entry {
    fn new(name: &str, command: &str, is_default: bool, dir: &Path, state_json: &Path) -> Self {
        Self {
            name: name.to_string(),
            command: command.to_string(),
            is_default,
            account: metrics::account_email(state_json),
            usage: Usage::default(),
            config_dir: dir.to_path_buf(),
        }
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

    let show_email = (cfg.usage.show_email || args.show_email) && !args.hide_email;
    if !show_email {
        for e in &mut entries {
            e.account = None;
        }
    }

    if usage::disabled_by_env() {
        crate::log::write("INFO", "usage not fetched: MCC_NO_USAGE is set");
    } else {
        let targets: Vec<usage::Target> = entries
            .iter()
            .map(|e| usage::Target {
                name: e.name.clone(),
                config_dir: e.config_dir.clone(),
                is_default: e.is_default,
            })
            .collect();
        for (e, u) in entries
            .iter_mut()
            .zip(usage::lookup(paths, &targets, cfg.usage.live))
        {
            e.usage = u;
        }
    }

    let mut out = std::io::stdout().lock();
    if args.json || args.json_v2 {
        super::write_json(&mut out, "profiles", &entries, args.json_v2)?;
    } else {
        let now = Utc::now();
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
    }
    Ok(())
}

/// One row per profile under a header, laid out as `layout`:
///
/// ```text
/// PROFILE    5-HOUR          RESETS IN                  WEEKLY          RESETS IN                 UPDATED
/// default ★  ████░░░░░░ 42%  2h 14m (Sat Oct 10 16:00)  ██░░░░░░░░ 18%  3d 4h (Wed Oct 14 00:00)  12m ago
/// lab        — (no recent usage cached; run /usage in Claude Code)
/// ```
///
/// An ACCOUNT column follows PROFILE when `show_email` is set. UPDATED, the
/// age of usage read from the statusline cache, is there only for cached
/// usage. A profile without usage has its reason in place of the usage
/// columns.
fn render(entries: &[Entry], layout: Layout, show_email: bool, now: DateTime<Utc>) -> Vec<String> {
    let mut headers = vec!["PROFILE"];
    if show_email {
        headers.push("ACCOUNT");
    }
    headers.extend(["5-HOUR", "RESETS IN", "WEEKLY", "RESETS IN"]);
    let cached = entries.iter().any(|e| e.usage.updated_at.is_some());
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
                (_, Some(r @ (Reason::Disabled | Reason::NoData))) => {
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
            cells(Some(&w(85.0, Some(reset))), LAYOUTS[3]),
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
        let entries = [
            entry("default", Some("me@example.com"), ok),
            entry("lab", None, Usage::unavailable(Reason::NotLoggedIn)),
        ];
        let lines = |email| -> Vec<String> {
            render(&entries, LAYOUTS[1], email, now)
                .iter()
                .map(|l| plain(l))
                .collect()
        };
        assert_eq!(
            lines(false),
            [
                "PROFILE    5-HOUR          RESETS IN  WEEKLY          RESETS IN  UPDATED",
                "default ★  ████░░░░░░ 42%  2h 14m     ██░░░░░░░░ 18%             12m ago",
                "lab        unavailable (not logged in)",
            ]
        );
        assert_eq!(
            lines(true),
            [
                "PROFILE    ACCOUNT         5-HOUR          RESETS IN  WEEKLY          RESETS IN  UPDATED",
                "default ★  me@example.com  ████░░░░░░ 42%  2h 14m     ██░░░░░░░░ 18%             12m ago",
                "lab        —               unavailable (not logged in)",
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
                "PROFILE  ACCOUNT  5-HOUR  RESETS IN  WEEKLY  RESETS IN",
                "lab      —        unavailable (not logged in)",
            ]
        );
        let mut both = vec![entry("default", Some("me@example.com"), Usage::default())];
        both.extend(entries.into_iter().skip(1));
        let lines: Vec<String> = render(&both, LAYOUTS[0], false, now)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(
            lines,
            [
                "PROFILE    5-HOUR  RESETS IN  WEEKLY  RESETS IN",
                "default ★  — (not fetched: --no-usage or MCC_NO_USAGE)",
                "lab        unavailable (not logged in)",
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
            "idle     — (no recent usage cached; run /usage in Claude Code)"
        );
    }
}
