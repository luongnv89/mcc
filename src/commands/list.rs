//! `mcc list` — all profiles with their metrics (table, cards, or JSON).
//!
//! The table keeps to the columns that fit 120 columns (NAME, ACCOUNT, 5H,
//! WEEK, LAST USED, SYNC); `--long` cards and JSON have every metric.
//! Account emails are hidden unless `[privacy] show_email` or
//! `--show-email` says otherwise, as in `mcc usage`.

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
            .map(|(name, p)| metrics::mcc_profile(name, p, paths, cfg.launch.banner)),
    );
    for m in &mut all {
        m.project_defaults = cfg
            .projects
            .iter()
            .filter(|(_, p)| p.profile == m.name)
            .map(|(dir, _)| dir.clone())
            .collect();
    }
    for m in all.iter_mut().filter(|m| !m.is_default) {
        if let Some(p) = cfg.profiles.get(&m.name) {
            m.sync = super::sync::status(paths, cfg, &p.dir);
        }
    }
    if !usage_disabled(args) {
        attach_usage(paths, cfg, &mut all);
    }
    let show_email = (cfg.show_email() || args.show_email) && !args.hide_email;
    // Which profiles have an account email, so cards can tell a hidden one
    // from none.
    let known: Vec<bool> = all.iter().map(|m| m.account.is_some()).collect();
    if !show_email {
        all.iter_mut().for_each(hide_email);
    }
    // A label with control characters, hand-written in the config file,
    // could drive the terminal: leave it out (null in JSON).
    for m in &mut all {
        if m.label.as_deref().is_some_and(super::has_control) {
            m.label = None;
        }
    }

    let mut out = std::io::stdout().lock();
    if args.json || args.json_v2 {
        super::write_json(&mut out, "profiles", &all, args.json_v2)?;
    } else if args.long {
        let now = Utc::now();
        let bar = card_bar(&all, now);
        for (m, known) in all.iter().zip(known) {
            card(&mut out, m, known && !show_email, bar, now, paths)?;
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
    if !(args.json || args.json_v2) {
        let total: u64 = all.iter().map(|m| m.size_bytes).sum();
        ui::info(&format!(
            "{} profile{} · {} on disk · add one with `mcc add <alias>`",
            all.len(),
            if all.len() == 1 { "" } else { "s" },
            ui::human_size(total)
        ));
        sharing_notes(&all, cfg.launch.banner);
        let names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
        let usages: Vec<&Usage> = all.iter().map(|m| &m.usage).collect();
        if let Some(hint) = usage::no_data_hint(&names, &usages) {
            ui::hint(&hint);
        }
    }
    Ok(())
}

/// Footnotes (stderr) for session sharing and out-of-date launchers.
/// `banner` is `[launch] banner`: the launch banner is mentioned only then.
fn sharing_notes(all: &[ProfileMetrics], banner: bool) {
    if all.iter().any(|m| m.share_sessions) {
        ui::info(
            "shared: sessions shared with every sharing profile of the same group (their session counts include all of them)",
        );
    }
    let stale = all.iter().filter(|m| !m.launcher_current).count();
    if stale > 0 && banner {
        ui::hint(&format!(
            "{stale} launcher{} {} out of date — run `mcc refresh-launchers` so `<alias> --mcc-share` and the launch banner work",
            if stale == 1 { "" } else { "s" },
            if stale == 1 { "is" } else { "are" },
        ));
    } else if stale > 0 {
        ui::hint(&format!(
            "{stale} launcher{} predate{} session sharing — run `mcc refresh-launchers` so `<alias> --mcc-share` works",
            if stale == 1 { "" } else { "s" },
            if stale == 1 { "s" } else { "" },
        ));
    }
}

/// Leave the account email out (null in JSON), and the organization and
/// label too when they contain an email, as personal plans' organizations
/// do ("me@x.com's Organization"; `mcc add` offers it as the label).
fn hide_email(m: &mut ProfileMetrics) {
    m.account = None;
    let has_email = |v: &Option<String>| v.as_deref().is_some_and(|s| s.contains('@'));
    if has_email(&m.organization) {
        m.organization = None;
    }
    if has_email(&m.label) {
        m.label = None;
    }
}

/// The widest ACCOUNT cell; longer ones end in "…", so the table keeps to
/// 120 columns.
const ACCOUNT_WIDTH: usize = 32;

/// The ACCOUNT cell: the profile's label, else its organization, with the
/// email in front when it is shown ("me@x.com (work)"); "—" when nothing is
/// known.
fn account_cell(m: &ProfileMetrics) -> String {
    let tag = m.label.as_deref().or(m.organization.as_deref());
    let cell = match (m.account.as_deref(), tag) {
        (Some(a), Some(t)) => format!("{a} ({t})"),
        (Some(a), None) => a.to_string(),
        (None, Some(t)) => t.to_string(),
        (None, None) => "—".into(),
    };
    console::truncate_str(&cell, ACCOUNT_WIDTH, "…").into_owned()
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

/// The SYNC cell: "✓", e.g. "3 behind, 2 local edits", or "—" for the
/// default profile and profiles that can't be compared.
fn sync_cell(m: &ProfileMetrics) -> String {
    m.sync
        .as_ref()
        .map_or_else(|| "—".into(), super::sync::status_text)
}

/// Header and rows, with usage bars `bar` cells wide. Counts, size, the
/// command and the organization are in the `--long` cards and JSON.
fn table_lines(all: &[ProfileMetrics], bar: usize, now: DateTime<Utc>) -> Vec<String> {
    let headers = ["NAME", "ACCOUNT", "5H", "WEEK", "LAST USED", "SYNC"];
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|m| {
            // In the profile's color (stdout to a terminal only); the
            // widths below measure without the escapes.
            let name = ui::paint(&m.name, m.color.as_deref());
            let name = if m.is_default {
                format!("{name} ★")
            } else {
                name
            };
            let name = match m.share_group.as_deref() {
                Some(g) if m.share_sessions && g != "default" => format!("{name} (shared: {g})"),
                _ if m.share_sessions => format!("{name} (shared)"),
                _ => name,
            };
            let name = if m.exists {
                name
            } else {
                format!("{name} (missing)")
            };
            vec![
                name,
                account_cell(m),
                usage_cell(&m.usage, m.usage.five_hour.as_ref(), bar, now),
                usage_cell(&m.usage, m.usage.weekly.as_ref(), bar, now),
                metrics::ago(m.last_used_time),
                sync_cell(m),
            ]
        })
        .collect();
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
            .zip(&widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w.saturating_sub(measure_text_width(c)))))
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
/// ~/code/app   work     Acme  ← here
/// ~/code/site  default  personal
/// ```
///
/// ACCOUNT is the table's ([`account_cell`]), so emails show only when
/// they are shown there:
///
/// ```text
/// ~/code/app   work     work@corp.com (Acme)  ← here
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
                    Some(m) => ui::paint(&p.profile, m.color.as_deref()),
                    None => format!("{} (no such profile)", p.profile),
                },
                m.map_or_else(|| "—".into(), account_cell),
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

/// `email_hidden`: the profile has an account email, left out because
/// emails are hidden.
fn card(
    out: &mut impl Write,
    m: &ProfileMetrics,
    email_hidden: bool,
    bar: usize,
    now: DateTime<Utc>,
    paths: &Paths,
) -> Result<()> {
    let name = ui::paint(&m.name, m.color.as_deref());
    let title = if m.is_default {
        format!("{name} ★ (~/.claude)")
    } else {
        name
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
            (None, Some(o)) if email_hidden => format!("{o} (email hidden; --show-email shows it)"),
            (None, None) if email_hidden => "email hidden (--show-email shows it)".into(),
            _ => "— (no account info yet; log in by running the command)".into(),
        },
    )?;
    if let Some(uuid) = &m.organization_uuid {
        row(out, "Organization ID", uuid.clone())?;
    }
    if let Some(label) = &m.label {
        row(out, "Label", label.clone())?;
    }
    if let Some(color) = &m.color {
        row(out, "Color", ui::paint(color, Some(color)))?;
    }
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
            match m.share_group.as_deref() {
                Some(g) if m.share_sessions && g != "default" =>
                    format!(", shared with the sharing profiles of group {g}"),
                _ if m.share_sessions => ", shared with every sharing profile".to_string(),
                _ => String::new(),
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
        row(out, "Sync", sync_cell(m))?;
    }
    writeln!(out)?;
    Ok(())
}
