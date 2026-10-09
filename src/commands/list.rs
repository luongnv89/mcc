//! `mcc list` — all profiles with their metrics (table, cards, or JSON).

use std::io::Write;

use anyhow::Result;
use console::{measure_text_width, style};

use crate::cli::ListArgs;
use crate::config::Config;
use crate::metrics::{self, ProfileMetrics};
use crate::paths::Paths;
use crate::ui;

pub fn run(paths: &Paths, cfg: &Config, args: &ListArgs) -> Result<()> {
    let mut all = vec![metrics::default_profile(paths)];
    all.extend(cfg.profiles.iter().map(|(name, p)| metrics::mcc_profile(name, p, paths)));

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

fn table(out: &mut impl Write, all: &[ProfileMetrics]) -> Result<()> {
    let headers = ["NAME", "COMMAND", "ACCOUNT", "SKILLS", "AGENTS", "CMDS", "PLUGINS", "MCP", "HOOKS", "SIZE", "LAST USED", "SYNCED"];
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|m| {
            let name = if m.is_default { format!("{} ★", m.name) } else { m.name.clone() };
            let name = if m.exists { name } else { format!("{name} (missing)") };
            vec![
                name,
                m.command.clone(),
                m.account.clone().unwrap_or_else(|| "—".into()),
                m.skills.to_string(),
                m.agents.to_string(),
                m.commands.to_string(),
                format!("{}/{}", m.plugins_enabled, m.plugins_installed),
                m.mcp_servers.to_string(),
                m.hooks.to_string(),
                ui::human_size(m.size_bytes),
                metrics::ago(m.last_used_time),
                if m.is_default { "source".into() } else { metrics::ago_str(m.last_sync.as_deref()) },
            ]
        })
        .collect();
    let numeric = |i: usize| (3..=9).contains(&i);
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| rows.iter().map(|r| measure_text_width(&r[i])).chain([headers[i].len()]).max().unwrap_or(0))
        .collect();
    let fmt_row = |cells: Vec<String>| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = widths[i].saturating_sub(measure_text_width(c));
                if numeric(i) { format!("{}{c}", " ".repeat(pad)) } else { format!("{c}{}", " ".repeat(pad)) }
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
    let title = if m.is_default { format!("{} ★ (~/.claude)", m.name) } else { m.name.clone() };
    writeln!(out, "{} {}", style("◆").cyan().bold(), style(title).bold())?;
    let row = |out: &mut dyn Write, k: &str, v: String| writeln!(out, "    {:<16} {v}", format!("{k}:"));
    row(out, "Command", m.command.clone())?;
    row(out, "Directory", format!("{}{}", m.dir, if m.exists { "" } else { " (missing)" }))?;
    row(
        out,
        "Account",
        match (&m.account, &m.organization) {
            (Some(a), Some(o)) => format!("{a} ({o})"),
            (Some(a), None) => a.clone(),
            _ => "— (no account info yet; log in by running the command)".into(),
        },
    )?;
    if let Some(model) = &m.model {
        row(out, "Model", model.clone())?;
    }
    row(out, "Skills", m.skills.to_string())?;
    row(out, "Agents", m.agents.to_string())?;
    row(out, "Commands", m.commands.to_string())?;
    row(out, "Output styles", m.output_styles.to_string())?;
    row(out, "Plugins", format!("{} enabled / {} installed", m.plugins_enabled, m.plugins_installed))?;
    row(out, "MCP servers", m.mcp_servers.to_string())?;
    row(out, "Hooks", m.hooks.to_string())?;
    row(out, "Projects", format!("{} ({} sessions)", m.projects, m.sessions))?;
    row(out, "Size", ui::human_size(m.size_bytes))?;
    row(out, "Last used", metrics::ago(m.last_used_time))?;
    if let Some(mode) = &m.mode {
        row(out, "Created", format!("{} ({mode})", m.created_at.clone().unwrap_or_default()))?;
        row(out, "Last sync", metrics::ago_str(m.last_sync.as_deref()))?;
    }
    writeln!(out)?;
    Ok(())
}
