//! `mcc list` — all profiles with their metrics (table, cards, or JSON).

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use console::{measure_text_width, style};

use crate::cli::ListArgs;
use crate::config::Config;
use crate::metrics::{self, ProfileMetrics};
use crate::paths::Paths;
use crate::ui;
use crate::usage::{self, Reason, Status, Usage, Window};

pub fn run(paths: &Paths, cfg: &Config, args: &ListArgs) -> Result<()> {
    let mut all = vec![metrics::default_profile(paths)];
    all.extend(
        cfg.profiles
            .iter()
            .map(|(name, p)| metrics::mcc_profile(name, p, paths)),
    );
    for m in &mut all {
        m.project_defaults = cfg
            .projects
            .iter()
            .filter(|(_, p)| p.profile == m.name)
            .map(|(dir, _)| dir.clone())
            .collect();
    }
    if !usage_disabled(args) {
        attach_usage(paths, cfg, &mut all);
    }

    let mut out = std::io::stdout().lock();
    if args.json {
        serde_json::to_writer_pretty(&mut out, &all)?;
        writeln!(out)?;
    } else if args.long {
        let now = Utc::now();
        let bar = card_bar(&all, now);
        for m in &all {
            card(&mut out, m, bar, now, paths)?;
        }
    } else {
        table(&mut out, &all)?;
        if !cfg.projects.is_empty() {
            writeln!(out)?;
            for line in project_lines(paths, cfg, &all) {
                writeln!(out, "{line}")?;
            }
        }
    }
    drop(out);

    for m in &all {
        for w in &m.warnings {
            ui::warn(&format!("{}: {w}", m.name));
        }
    }
    if !args.json {
        let total: u64 = all.iter().map(|m| m.size_bytes).sum();
        ui::info(&format!(
            "{} profile{} · {} on disk · add one with `mcc add <alias>`",
            all.len(),
            if all.len() == 1 { "" } else { "s" },
            ui::human_size(total)
        ));
        sharing_notes(&all);
        let names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
        let usages: Vec<&Usage> = all.iter().map(|m| &m.usage).collect();
        if let Some(hint) = usage::no_data_hint(&names, &usages) {
            ui::hint(&hint);
        }
    }
    Ok(())
}

/// Footnotes (stderr) for session sharing and launchers that predate it.
fn sharing_notes(all: &[ProfileMetrics]) {
    if all.iter().any(|m| m.share_sessions) {
        ui::info(
            "shared: sessions shared with every sharing profile (their session counts include all of them)",
        );
    }
    let stale = all.iter().filter(|m| !m.launcher_current).count();
    if stale > 0 {
        ui::hint(&format!(
            "{stale} launcher{} predate{} session sharing — run `mcc refresh-launchers` so `<alias> --mcc-share` works",
            if stale == 1 { "" } else { "s" },
            if stale == 1 { "s" } else { "" },
        ));
    }
}

/// `--no-usage`, or `MCC_NO_USAGE` set to anything but "" / "0".
fn usage_disabled(args: &ListArgs) -> bool {
    args.no_usage || usage::disabled_by_env()
}

/// Every profile's usage (default included): from the statusline cache, or
/// looked up live with `[usage] live = true`. Failures become an
/// "unavailable" status: never a warning, never a failed command.
fn attach_usage(paths: &Paths, cfg: &Config, all: &mut [ProfileMetrics]) {
    let targets: Vec<usage::Target> = all
        .iter()
        .map(|m| usage::Target {
            name: m.name.clone(),
            config_dir: m.config_dir.clone(),
            is_default: m.is_default,
        })
        .collect();
    for (m, u) in all
        .iter_mut()
        .zip(usage::lookup(paths, &targets, cfg.usage.live))
    {
        m.usage = u;
    }
}

/// Bar widths tried for the table's usage cells, widest first.
const TABLE_BARS: [usize; 3] = [10, 5, 0];
/// Bar widths tried for the `--long` usage rows, widest first.
const CARD_BARS: [usize; 3] = [20, 10, 0];
/// Width of a card row's `    Key:            ` prefix.
const CARD_KEY_WIDTH: usize = 21;

/// Table cell for one window: "████░░░░░░ 42% (2h)" (just "42% (2h)" with a
/// zero-width bar), "—" (no data / not fetched), or "unavailable".
fn usage_cell(u: &Usage, w: Option<&Window>, bar: usize, now: DateTime<Utc>) -> String {
    match (u.status, w) {
        (Status::Ok, Some(w)) => match w.resets_at {
            Some(t) => format!(
                "{} ({})",
                ui::gauge(w.percent, bar),
                usage::until_short(t, now)
            ),
            None => ui::gauge(w.percent, bar),
        },
        (Status::Ok, None) => "—".into(),
        _ if matches!(u.reason, Some(Reason::Disabled | Reason::NoData)) => "—".into(),
        _ => "unavailable".into(),
    }
}

/// How old the cached usage is ("12m ago"), or "—".
fn updated_cell(u: &Usage, now: DateTime<Utc>) -> String {
    match u.status {
        Status::Ok => u.age(now).unwrap_or_else(|| "—".into()),
        Status::Unavailable => "—".into(),
    }
}

/// Card value for one window: "████████░░░░░░░░░░░░ 42% (resets in 2h 14m)" or "—".
fn usage_line(w: Option<&Window>, bar: usize, now: DateTime<Utc>) -> String {
    match w {
        Some(w) => match w.resets_at {
            Some(t) => format!(
                "{} (resets in {})",
                ui::gauge(w.percent, bar),
                usage::until(t, now)
            ),
            None => ui::gauge(w.percent, bar),
        },
        None => "—".into(),
    }
}

/// The card's usage rows: one per window, then how old cached numbers are,
/// or a single "Usage" row saying why there is none.
fn usage_rows(u: &Usage, bar: usize, now: DateTime<Utc>) -> Vec<(&'static str, String)> {
    match (u.status, u.reason) {
        (Status::Ok, _) => {
            let mut rows = vec![
                ("5h usage", usage_line(u.five_hour.as_ref(), bar, now)),
                ("Weekly usage", usage_line(u.weekly.as_ref(), bar, now)),
            ];
            if let Some(age) = u.age(now) {
                rows.push(("Usage updated", format!("{age} (statusline cache)")));
            }
            rows
        }
        (_, Some(r @ (Reason::Disabled | Reason::NoData))) => {
            vec![("Usage", format!("— ({})", r.describe()))]
        }
        (_, reason) => vec![(
            "Usage",
            format!(
                "unavailable ({})",
                reason.map_or("unknown", Reason::describe)
            ),
        )],
    }
}

/// The widest bar that keeps every card's usage rows within the terminal.
fn card_bar(all: &[ProfileMetrics], now: DateTime<Utc>) -> usize {
    ui::fit_bar(&CARD_BARS, |bar| {
        all.iter()
            .flat_map(|m| usage_rows(&m.usage, bar, now))
            .map(|(_, v)| CARD_KEY_WIDTH + measure_text_width(&v))
            .max()
            .unwrap_or(0)
    })
}

/// The table, as printed: the widest usage bar that fits the terminal; when
/// none does, the 5-cell bar from 80 columns up (the table wraps anyway) and
/// no bar (plain "42% (2h)") below that.
fn table(out: &mut impl Write, all: &[ProfileMetrics]) -> Result<()> {
    let now = Utc::now();
    let width = |lines: &[String]| lines.iter().map(|l| measure_text_width(l)).max();
    let bar = ui::fit_bar(&TABLE_BARS, |bar| {
        width(&table_lines(all, bar, now)).unwrap_or(0)
    });
    for line in table_lines(all, bar, now) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Header and rows, with usage bars `bar` cells wide.
///
/// UPDATED (how old cached usage is) follows WEEK when any profile's usage
/// came from the statusline cache.
fn table_lines(all: &[ProfileMetrics], bar: usize, now: DateTime<Utc>) -> Vec<String> {
    let cached = all.iter().any(|m| m.usage.updated_at.is_some());
    let mut headers = vec!["NAME", "COMMAND", "ACCOUNT", "5H", "WEEK"];
    if cached {
        headers.push("UPDATED");
    }
    headers.extend([
        "SKILLS",
        "AGENTS",
        "CMDS",
        "PLUGINS",
        "MCP",
        "HOOKS",
        "SIZE",
        "LAST USED",
        "SYNCED",
    ]);
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|m| {
            let name = if m.is_default {
                format!("{} ★", m.name)
            } else {
                m.name.clone()
            };
            let name = if m.share_sessions {
                format!("{name} (shared)")
            } else {
                name
            };
            let name = if m.exists {
                name
            } else {
                format!("{name} (missing)")
            };
            let mut cells = vec![
                name,
                m.command.clone(),
                m.account.clone().unwrap_or_else(|| "—".into()),
                usage_cell(&m.usage, m.usage.five_hour.as_ref(), bar, now),
                usage_cell(&m.usage, m.usage.weekly.as_ref(), bar, now),
            ];
            if cached {
                cells.push(updated_cell(&m.usage, now));
            }
            cells.extend([
                m.skills.to_string(),
                m.agents.to_string(),
                m.commands.to_string(),
                format!("{}/{}", m.plugins_enabled, m.plugins_installed),
                m.mcp_servers.to_string(),
                m.hooks.to_string(),
                ui::human_size(m.size_bytes),
                metrics::ago(m.last_used_time),
                if m.is_default {
                    "source".into()
                } else {
                    metrics::ago_str(m.last_sync.as_deref())
                },
            ]);
            cells
        })
        .collect();
    let numeric = |i: usize| {
        matches!(
            headers[i],
            "SKILLS" | "AGENTS" | "CMDS" | "PLUGINS" | "MCP" | "HOOKS" | "SIZE"
        )
    };
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            rows.iter()
                .map(|r| measure_text_width(&r[i]))
                .chain([headers[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let fmt_row = |cells: Vec<String>| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = widths[i].saturating_sub(measure_text_width(c));
                if numeric(i) {
                    format!("{}{c}", " ".repeat(pad))
                } else {
                    format!("{c}{}", " ".repeat(pad))
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let head = fmt_row(headers.iter().map(|h| h.to_string()).collect());
    std::iter::once(style(head).bold().to_string())
        .chain(rows.into_iter().map(fmt_row))
        .collect()
}

/// Under the table: which profile `mcc run` starts in each project, with
/// its account. The project that applies in the current directory is marked.
///
/// ```text
/// PROJECT      PROFILE  ACCOUNT
/// ~/code/app   work     work@corp.com  ← here
/// ~/code/site  default  me@example.com
/// ```
fn project_lines(paths: &Paths, cfg: &Config, all: &[ProfileMetrics]) -> Vec<String> {
    let here = super::project::current_dir().ok();
    let here = here
        .as_deref()
        .and_then(|d| super::project::lookup(cfg, d))
        .map(|(dir, _)| dir);
    let rows: Vec<[String; 4]> = cfg
        .projects
        .iter()
        .map(|(dir, p)| {
            let m = all.iter().find(|m| m.name == p.profile);
            let missing = if Path::new(dir).is_dir() {
                ""
            } else {
                " (missing)"
            };
            [
                format!("{}{missing}", paths.display(Path::new(dir))),
                match m {
                    Some(_) => p.profile.clone(),
                    None => format!("{} (no such profile)", p.profile),
                },
                m.and_then(|m| m.account.clone())
                    .unwrap_or_else(|| "—".into()),
                if Some(dir.as_str()) == here {
                    "← here".into()
                } else {
                    String::new()
                },
            ]
        })
        .collect();
    let head = ["PROJECT", "PROFILE", "ACCOUNT", ""].map(String::from);
    let widths: Vec<usize> = (0..4)
        .map(|i| {
            rows.iter()
                .chain([&head])
                .map(|r| measure_text_width(&r[i]))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let fmt_row = |cells: &[String; 4]| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w - measure_text_width(c))))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    std::iter::once(style(fmt_row(&head)).bold().to_string())
        .chain(rows.iter().map(fmt_row))
        .collect()
}

fn card(
    out: &mut impl Write,
    m: &ProfileMetrics,
    bar: usize,
    now: DateTime<Utc>,
    paths: &Paths,
) -> Result<()> {
    let title = if m.is_default {
        format!("{} ★ (~/.claude)", m.name)
    } else {
        m.name.clone()
    };
    writeln!(out, "{} {}", style("◆").cyan().bold(), style(title).bold())?;
    let row =
        |out: &mut dyn Write, k: &str, v: String| writeln!(out, "    {:<16} {v}", format!("{k}:"));
    row(out, "Command", m.command.clone())?;
    row(
        out,
        "Directory",
        format!("{}{}", m.dir, if m.exists { "" } else { " (missing)" }),
    )?;
    row(
        out,
        "Account",
        match (&m.account, &m.organization) {
            (Some(a), Some(o)) => format!("{a} ({o})"),
            (Some(a), None) => a.clone(),
            _ => "— (no account info yet; log in by running the command)".into(),
        },
    )?;
    for (k, v) in usage_rows(&m.usage, bar, now) {
        row(out, k, v)?;
    }
    if let Some(model) = &m.model {
        row(out, "Model", model.clone())?;
    }
    row(out, "Skills", m.skills.to_string())?;
    row(out, "Agents", m.agents.to_string())?;
    row(out, "Commands", m.commands.to_string())?;
    row(out, "Output styles", m.output_styles.to_string())?;
    row(
        out,
        "Plugins",
        format!(
            "{} enabled / {} installed",
            m.plugins_enabled, m.plugins_installed
        ),
    )?;
    row(out, "MCP servers", m.mcp_servers.to_string())?;
    row(out, "Hooks", m.hooks.to_string())?;
    row(
        out,
        "Projects",
        format!(
            "{} ({} sessions{})",
            m.projects,
            m.sessions,
            if m.share_sessions {
                ", shared with every sharing profile"
            } else {
                ""
            }
        ),
    )?;
    // One project per line, so long paths stay readable.
    for (i, dir) in m.project_defaults.iter().enumerate() {
        let key = if i == 0 { "Default for" } else { "" };
        let shown = paths.display(Path::new(dir));
        if key.is_empty() {
            writeln!(out, "    {:<16} {shown}", "")?;
        } else {
            row(out, key, shown)?;
        }
    }
    row(out, "Size", ui::human_size(m.size_bytes))?;
    row(out, "Last used", metrics::ago(m.last_used_time))?;
    if let Some(mode) = &m.mode {
        row(
            out,
            "Created",
            format!("{} ({mode})", m.created_at.clone().unwrap_or_default()),
        )?;
        row(out, "Last sync", metrics::ago_str(m.last_sync.as_deref()))?;
    }
    writeln!(out)?;
    Ok(())
}
