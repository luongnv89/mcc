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

/// How a window row is laid out: its bar width (0 = no bar) and whether the
/// absolute reset time follows the relative "resets in …".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    bar: usize,
    reset_time: bool,
}

/// Layouts tried, most generous first: the absolute reset time goes first,
/// then the bar shrinks in the same 20/10/0 steps as `mcc list --long`
/// (narrow terminals).
const LAYOUTS: [Layout; 4] = [
    Layout {
        bar: 20,
        reset_time: true,
    },
    Layout {
        bar: 20,
        reset_time: false,
    },
    Layout {
        bar: 10,
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
        for (e, u) in entries.iter_mut().zip(usage::fetch_all(&targets)) {
            e.usage = u;
        }
    }

    let mut out = std::io::stdout().lock();
    if args.json {
        serde_json::to_writer_pretty(&mut out, &entries)?;
        writeln!(out)?;
    } else {
        let now = Utc::now();
        let layout = ui::fit_layout(
            &LAYOUTS,
            |l| l.bar,
            |l| {
                render(&entries, l, now)
                    .iter()
                    .map(|line| measure_text_width(line))
                    .max()
                    .unwrap_or(0)
            },
        );
        for line in render(&entries, layout, now) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

/// One block per profile, separated by blank lines, laid out as `layout`:
///
/// ```text
/// ◆ default ★  claude · me@example.com
///     5-hour   ████████░░░░░░░░░░░░ 42% used · 58% left · resets in 2h 14m (Sat Oct 10 16:00)
///     Weekly   ████░░░░░░░░░░░░░░░░ 18% used · 82% left · resets in 3d 4h (Wed Oct 14 00:00)
/// ```
fn render(entries: &[Entry], layout: Layout, now: DateTime<Utc>) -> Vec<String> {
    let mut lines = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            lines.push(String::new());
        }
        let title = if e.is_default {
            format!("{} ★", e.name)
        } else {
            e.name.clone()
        };
        let about = match &e.account {
            Some(a) => format!("{} · {a}", e.command),
            None => e.command.clone(),
        };
        lines.push(format!(
            "{} {}  {}",
            style("◆").cyan().bold(),
            style(title).bold(),
            style(about).dim()
        ));
        let row = |label: &str, value: String| format!("    {label:<8} {value}");
        let u = &e.usage;
        match (u.status, u.reason) {
            (Status::Ok, _) => {
                lines.push(row("5-hour", window(u.five_hour.as_ref(), layout, now)));
                lines.push(row("Weekly", window(u.weekly.as_ref(), layout, now)));
            }
            (_, Some(Reason::Disabled)) => {
                lines.push(row("Usage", format!("— ({})", Reason::Disabled.describe())));
            }
            (_, reason) => lines.push(row(
                "Usage",
                format!(
                    "unavailable ({})",
                    reason.map_or("unknown", Reason::describe)
                ),
            )),
        }
    }
    lines
}

/// "████░░░░ 42% used · 58% left · resets in 2h 14m (Sat Oct 10 16:00)", or
/// "— (no data)" when the API reported no such window. The bar and the
/// absolute reset time follow `layout`; "resets in …" is always there.
fn window(w: Option<&Window>, layout: Layout, now: DateTime<Utc>) -> String {
    let Some(w) = w else {
        return "— (no data)".into();
    };
    let left = usage::percent((100.0 - w.percent.round()).max(0.0));
    let mut s = format!("{} used · {left} left", ui::gauge(w.percent, layout.bar));
    if let Some(t) = w.resets_at {
        s.push_str(&format!(" · resets in {}", usage::until(t, now)));
        if layout.reset_time {
            s.push_str(&format!(" ({})", usage::local_time(t)));
        }
    }
    s
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
    fn window_text() {
        let now = at("2026-10-10T12:00:00Z");
        let reset = at("2026-10-10T14:14:00Z");
        let w = |percent, resets_at| Window { percent, resets_at };
        let full = |bar| Layout {
            bar,
            reset_time: true,
        };
        assert_eq!(
            plain(&window(Some(&w(42.4, None)), full(10), now)),
            "████░░░░░░ 42% used · 58% left"
        );
        assert_eq!(
            window(Some(&w(96.0, Some(reset))), full(0), now),
            format!(
                "96%!! used · 4% left · resets in 2h 14m ({})",
                usage::local_time(reset)
            )
        );
        // Used and left always add up to 100%, and left never goes negative.
        // Compact layouts drop the absolute reset time, never "resets in".
        assert_eq!(
            plain(&window(Some(&w(42.4, Some(reset))), LAYOUTS[2], now)),
            "████░░░░░░ 42% used · 58% left · resets in 2h 14m"
        );
        assert_eq!(
            window(Some(&w(42.5, None)), full(0), now),
            "43% used · 57% left"
        );
        assert_eq!(
            window(Some(&w(130.0, None)), full(0), now),
            "130%!! used · 0% left"
        );
        assert_eq!(window(None, full(10), now), "— (no data)");
    }
}
