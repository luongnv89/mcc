//! `mcc list` — all profiles with their metrics (table, cards, or JSON).

use std::io::Write;

use anyhow::Result;
use chrono::{DateTime, Utc};
use console::{measure_text_width, style};

use crate::cli::ListArgs;
use crate::config::Config;
use crate::log;
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
    if !usage_disabled(args) {
        attach_usage(&mut all);
    }

    let mut out = std::io::stdout().lock();
    if args.json {
        serde_json::to_writer_pretty(&mut out, &all)?;
        writeln!(out)?;
    } else if args.long {
        for m in &all {
            card(&mut out, m)?;
        }
    } else {
        table(&mut out, &all)?;
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
    }
    Ok(())
}

/// `--no-usage`, or `MCC_NO_USAGE` set to anything but "" / "0".
fn usage_disabled(args: &ListArgs) -> bool {
    args.no_usage || std::env::var_os("MCC_NO_USAGE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Fetch every profile's usage (default included), in parallel. Failures
/// become an "unavailable" status: never a warning, never a failed command.
fn attach_usage(all: &mut [ProfileMetrics]) {
    let targets: Vec<usage::Target> = all
        .iter()
        .map(|m| usage::Target {
            config_dir: m.config_dir.clone(),
            is_default: m.is_default,
        })
        .collect();
    log::write(
        "INFO",
        &format!("fetching 5h/weekly usage for {} profiles", targets.len()),
    );
    for (m, u) in all.iter_mut().zip(usage::fetch_all(&targets)) {
        let outcome = match u.reason {
            Some(r) => format!("unavailable ({})", r.code()),
            None => "ok".into(),
        };
        log::write("INFO", &format!("usage {}: {outcome}", m.name));
        m.usage = u;
    }
}

/// Table cell for one window: "42% (2h)", "—" (no data / not fetched), or "unavailable".
fn usage_cell(u: &Usage, w: Option<&Window>, now: DateTime<Utc>) -> String {
    match (u.status, w) {
        (Status::Ok, Some(w)) => match w.resets_at {
            Some(t) => format!(
                "{} ({})",
                usage::percent(w.percent),
                usage::until_short(t, now)
            ),
            None => usage::percent(w.percent),
        },
        (Status::Ok, None) => "—".into(),
        _ if u.reason == Some(Reason::Disabled) => "—".into(),
        _ => "unavailable".into(),
    }
}

/// Card value for one window: "42% (resets in 2h 14m)" or "—".
fn usage_line(w: Option<&Window>, now: DateTime<Utc>) -> String {
    match w {
        Some(w) => match w.resets_at {
            Some(t) => format!(
                "{} (resets in {})",
                usage::percent(w.percent),
                usage::until(t, now)
            ),
            None => usage::percent(w.percent),
        },
        None => "—".into(),
    }
}

fn table(out: &mut impl Write, all: &[ProfileMetrics]) -> Result<()> {
    let now = Utc::now();
    let headers = [
        "NAME",
        "COMMAND",
        "ACCOUNT",
        "5H",
        "WEEK",
        "SKILLS",
        "AGENTS",
        "CMDS",
        "PLUGINS",
        "MCP",
        "HOOKS",
        "SIZE",
        "LAST USED",
        "SYNCED",
    ];
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|m| {
            let name = if m.is_default {
                format!("{} ★", m.name)
            } else {
                m.name.clone()
            };
            let name = if m.exists {
                name
            } else {
                format!("{name} (missing)")
            };
            vec![
                name,
                m.command.clone(),
                m.account.clone().unwrap_or_else(|| "—".into()),
                usage_cell(&m.usage, m.usage.five_hour.as_ref(), now),
                usage_cell(&m.usage, m.usage.weekly.as_ref(), now),
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
            ]
        })
        .collect();
    let numeric = |i: usize| (5..=11).contains(&i);
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
    writeln!(out, "{}", style(head).bold())?;
    for r in rows {
        writeln!(out, "{}", fmt_row(r))?;
    }
    Ok(())
}

fn card(out: &mut impl Write, m: &ProfileMetrics) -> Result<()> {
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
    let now = Utc::now();
    match (m.usage.status, m.usage.reason) {
        (Status::Ok, _) => {
            row(out, "5h usage", usage_line(m.usage.five_hour.as_ref(), now))?;
            row(
                out,
                "Weekly usage",
                usage_line(m.usage.weekly.as_ref(), now),
            )?;
        }
        (_, Some(Reason::Disabled)) => {
            row(out, "Usage", format!("— ({})", Reason::Disabled.describe()))?
        }
        (_, reason) => row(
            out,
            "Usage",
            format!(
                "unavailable ({})",
                reason.map_or("unknown", Reason::describe)
            ),
        )?,
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
        format!("{} ({} sessions)", m.projects, m.sessions),
    )?;
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
