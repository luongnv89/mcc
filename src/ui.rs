//! Terminal output: headers, numbered steps, status lines, prompts.
//!
//! Everything here writes to **stderr** so stdout stays clean for data
//! (`mcc list --json | jq`). Every line is also mirrored to the log file.
//! The exception is the usage meter helpers at the end, which only build
//! strings for the data commands (`mcc list`, `mcc usage`) to print.

use std::io::{IsTerminal, Write};
use std::sync::OnceLock;
use std::time::Instant;

use console::{Style, style};
use dialoguer::theme::ColorfulTheme;

use crate::error::CliError;
use crate::log;

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub verbose: bool,
    pub quiet: bool,
    pub color: bool,
    pub interactive: bool,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

pub fn init(verbose: bool, quiet: bool, no_color: bool) {
    let allowed = !no_color && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
    let color = allowed && std::io::stderr().is_terminal();
    console::set_colors_enabled(allowed && std::io::stdout().is_terminal());
    console::set_colors_enabled_stderr(color);
    let interactive = std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && std::env::var_os("MCC_NON_INTERACTIVE").is_none();
    let _ = SETTINGS.set(Settings {
        verbose,
        quiet,
        color,
        interactive,
    });
}

pub fn settings() -> Settings {
    *SETTINGS.get().unwrap_or(&Settings {
        verbose: false,
        quiet: false,
        color: false,
        interactive: false,
    })
}

pub fn is_interactive() -> bool {
    settings().interactive
}

fn emit(line: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{line}");
}

/// A section header, e.g. `◆ Add profile "work"`.
pub fn header(title: &str) {
    log::write("INFO", &format!("== {title}"));
    if settings().quiet {
        return;
    }
    emit("");
    emit(&format!(
        "{} {}",
        style("◆").cyan().bold(),
        style(title).bold()
    ));
}

pub fn info(msg: &str) {
    log::write("INFO", msg);
    if !settings().quiet {
        emit(&format!("  {} {msg}", style("●").blue()));
    }
}

pub fn detail(msg: &str) {
    log::write("DEBUG", msg);
    if settings().verbose && !settings().quiet {
        emit(&format!("      {}", style(msg).dim()));
    }
}

pub fn success(msg: &str) {
    log::write("INFO", msg);
    if !settings().quiet {
        emit(&format!("  {} {msg}", style("✓").green().bold()));
    }
}

pub fn warn(msg: &str) {
    log::write("WARN", msg);
    emit(&format!("  {} {msg}", style("⚠").yellow().bold()));
}

/// A problem found by a check (not a failure of the command itself).
pub fn problem(msg: &str) {
    log::write("WARN", &format!("problem: {msg}"));
    emit(&format!("  {} {msg}", style("✗").red().bold()));
}

/// Lines to copy as they are (e.g. into an rc file), indented under a hint.
pub fn snippet(text: &str) {
    log::write("INFO", &format!("snippet:\n{text}"));
    if !settings().quiet {
        for line in text.lines() {
            emit(&format!("        {line}"));
        }
    }
}

pub fn hint(msg: &str) {
    log::write("INFO", &format!("hint: {msg}"));
    if !settings().quiet {
        emit(&format!("    {} {}", style("→").dim(), style(msg).dim()));
    }
}

/// Final error line; always shown, even with --quiet.
pub fn error(msg: &str, hint: Option<&str>) {
    log::write("ERROR", msg);
    emit(&format!("{} {msg}", style("✗ error:").red().bold()));
    if let Some(h) = hint {
        log::write("ERROR", &format!("hint: {h}"));
        emit(&format!("  {} {h}", style("hint:").yellow()));
    }
}

/// Aligned `key: value` row used by reports.
pub fn kv(key: &str, value: &str) {
    log::write("INFO", &format!("{key}: {value}"));
    if !settings().quiet {
        emit(&format!(
            "    {:<18} {value}",
            style(format!("{key}:")).dim()
        ));
    }
}

/// Closing summary: `Result: DONE — …` style line.
pub fn result(ok: bool, msg: &str) {
    log::write(if ok { "INFO" } else { "WARN" }, &format!("result: {msg}"));
    if settings().quiet && ok {
        return;
    }
    let label = if ok {
        style("Result:").green().bold()
    } else {
        style("Result:").yellow().bold()
    };
    emit("");
    emit(&format!("  {label} {msg}"));
}

/// Numbered step tracker: `[2/7] Validating alias … ✓ (12ms)`.
pub struct Steps {
    total: usize,
    current: usize,
}

pub struct Step {
    label: String,
    started: Instant,
    index: usize,
    total: usize,
    finished: bool,
}

impl Steps {
    pub fn new(total: usize) -> Self {
        Self { total, current: 0 }
    }

    pub fn start(&mut self, label: &str) -> Step {
        self.current += 1;
        log::write(
            "INFO",
            &format!("[{}/{}] {label}", self.current, self.total),
        );
        if !settings().quiet {
            emit(&format!(
                "  {} {label}",
                style(format!("[{}/{}]", self.current, self.total)).cyan()
            ));
        }
        Step {
            label: label.to_string(),
            started: Instant::now(),
            index: self.current,
            total: self.total,
            finished: false,
        }
    }
}

impl Step {
    fn finish(&mut self, mark: console::StyledObject<&str>, word: &str, note: &str) {
        self.finished = true;
        let ms = self.started.elapsed().as_millis();
        let note_s = if note.is_empty() {
            String::new()
        } else {
            format!(" — {note}")
        };
        log::write(
            "INFO",
            &format!(
                "[{}/{}] {} {word}{note_s} ({ms}ms)",
                self.index, self.total, self.label
            ),
        );
        if !settings().quiet {
            emit(&format!(
                "        {mark} {word}{note_s} {}",
                style(format!("({ms}ms)")).dim()
            ));
        }
    }

    pub fn ok(mut self, note: &str) {
        self.finish(style("✓").green().bold(), "done", note);
    }

    pub fn skip(mut self, note: &str) {
        self.finish(style("○").dim(), "skipped", note);
    }

    pub fn warn(mut self, note: &str) {
        self.finish(style("⚠").yellow().bold(), "warning", note);
    }

    pub fn fail(mut self, note: &str) {
        self.finish(style("✗").red().bold(), "failed", note);
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(style("✗").red().bold(), "failed", "");
        }
    }
}

// ---- prompts (stderr) -------------------------------------------------------

fn theme() -> ColorfulTheme {
    if settings().color {
        ColorfulTheme::default()
    } else {
        ColorfulTheme {
            defaults_style: Style::new(),
            prompt_style: Style::new(),
            prompt_prefix: style("?".to_string()),
            prompt_suffix: style("›".to_string()),
            success_prefix: style("✔".to_string()),
            success_suffix: style("·".to_string()),
            error_prefix: style("✘".to_string()),
            error_style: Style::new(),
            hint_style: Style::new(),
            values_style: Style::new(),
            active_item_style: Style::new(),
            inactive_item_style: Style::new(),
            active_item_prefix: style("❯".to_string()),
            inactive_item_prefix: style(" ".to_string()),
            checked_item_prefix: style("✔".to_string()),
            unchecked_item_prefix: style("⬚".to_string()),
            picked_item_prefix: style("❯".to_string()),
            unpicked_item_prefix: style(" ".to_string()),
        }
    }
}

pub fn require_interactive(what: &str, flag_hint: &str) -> anyhow::Result<()> {
    if is_interactive() {
        Ok(())
    } else {
        Err(
            CliError::usage(format!("{what} is required but no terminal is attached"))
                .hint(flag_hint.to_string())
                .into(),
        )
    }
}

pub fn input(prompt: &str, flag_hint: &str) -> anyhow::Result<String> {
    require_interactive(prompt, flag_hint)?;
    let v: String = dialoguer::Input::with_theme(&theme())
        .with_prompt(prompt)
        .interact_text()?;
    Ok(v.trim().to_string())
}

pub fn select(
    prompt: &str,
    items: &[&str],
    default: usize,
    flag_hint: &str,
) -> anyhow::Result<usize> {
    require_interactive(prompt, flag_hint)?;
    Ok(dialoguer::Select::with_theme(&theme())
        .with_prompt(prompt)
        .items(items)
        .default(default)
        .interact()?)
}

pub fn confirm(prompt: &str, default: bool, flag_hint: &str) -> anyhow::Result<bool> {
    require_interactive(prompt, flag_hint)?;
    Ok(dialoguer::Confirm::with_theme(&theme())
        .with_prompt(prompt)
        .default(default)
        .interact()?)
}

/// Format a byte count as a human-readable size.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

// ---- usage meters (strings for stdout) --------------------------------------

/// Usage at or above this percent is near the limit (`!`, yellow).
pub const HIGH_PERCENT: f64 = 80.0;
/// Usage at or above this percent is at the limit (`!!`, red).
pub const CRITICAL_PERCENT: f64 = 95.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Normal,
    High,
    Critical,
}

/// How close a usage percent is to its limit, judged on the percent as shown
/// (rounded), so "80%" always carries the `!` marker. NaN is [`Level::Normal`].
pub fn level(percent: f64) -> Level {
    let shown = percent.round();
    if shown >= CRITICAL_PERCENT {
        Level::Critical
    } else if shown >= HIGH_PERCENT {
        Level::High
    } else {
        Level::Normal
    }
}

/// Colour-independent near-limit marker: "" / "!" / "!!".
pub fn level_marker(percent: f64) -> &'static str {
    match level(percent) {
        Level::Normal => "",
        Level::High => "!",
        Level::Critical => "!!",
    }
}

/// Percent used with its near-limit marker: "42%", "85%!", "96%!!".
pub fn percent_text(percent: f64) -> String {
    format!(
        "{}{}",
        crate::usage::percent(percent),
        level_marker(percent)
    )
}

/// A btop-style bar of `width` cells, `█` used and `░` left, e.g.
/// `████░░░░░░` for 42%. The percent is clamped to 0–100 (NaN = 0). Colour
/// (green / yellow / red by [`level`]) comes only from `console::style`, so
/// the bar is plain text when colours are off.
pub fn meter(percent: f64, width: usize) -> String {
    let p = if percent.is_nan() {
        0.0
    } else {
        percent.clamp(0.0, 100.0)
    };
    let filled = ((p / 100.0 * width as f64).round() as usize).min(width);
    let used = "█".repeat(filled);
    let left = "░".repeat(width - filled);
    let used = match level(percent) {
        Level::Normal => style(used).green(),
        Level::High => style(used).yellow(),
        Level::Critical => style(used).red(),
    };
    format!("{used}{}", style(left).dim())
}

/// [`meter`] followed by [`percent_text`]: "████░░░░░░ 42%"; only the percent
/// when `width` is 0 (narrow terminals).
pub fn gauge(percent: f64, width: usize) -> String {
    if width == 0 {
        percent_text(percent)
    } else {
        format!("{} {}", meter(percent, width), percent_text(percent))
    }
}

/// Columns available on stdout: `COLUMNS` when set to a positive integer,
/// else the terminal's width; `None` when stdout is not a terminal (piped
/// output is not width-limited, so it stays the same everywhere).
pub fn terminal_width() -> Option<usize> {
    if let Some(cols) = parse_columns(std::env::var("COLUMNS").ok().as_deref()) {
        return Some(cols);
    }
    let term = console::Term::stdout();
    if !term.is_term() {
        return None;
    }
    term.size_checked().map(|(_, cols)| cols as usize)
}

fn parse_columns(v: Option<&str>) -> Option<usize> {
    v?.trim().parse::<usize>().ok().filter(|&n| n > 0)
}

/// Below this many columns a layout that overflows the terminal drops its
/// bar entirely; at or above it the most compact bar is kept, since the
/// output wraps either way and a small meter still reads at a glance.
pub const MIN_BAR_COLUMNS: usize = 80;

/// The first bar width in `candidates` (widest first) whose output,
/// `need(width)` columns wide, fits [`terminal_width`]; see [`fit_layout`]
/// for what happens when none fits or the width is unknown.
pub fn fit_bar(candidates: &[usize], need: impl Fn(usize) -> usize) -> usize {
    fit_layout(candidates, |w| w, need)
}

/// The first of `layouts` (most generous first) whose output, `need(layout)`
/// columns wide, fits [`terminal_width`]. `bar(layout)` is its bar width.
/// When the width is unknown (piped) the first layout; when none fits, the
/// one with the smallest non-zero bar on terminals of at least
/// [`MIN_BAR_COLUMNS`] columns, and the one with no bar below that.
pub fn fit_layout<L: Copy>(
    layouts: &[L],
    bar: impl Fn(L) -> usize,
    need: impl Fn(L) -> usize,
) -> L {
    choose_layout(terminal_width(), layouts, bar, need)
}

fn choose_layout<L: Copy>(
    cols: Option<usize>,
    layouts: &[L],
    bar: impl Fn(L) -> usize,
    need: impl Fn(L) -> usize,
) -> L {
    let first = *layouts.first().expect("at least one layout");
    let Some(cols) = cols else {
        return first;
    };
    if let Some(l) = layouts.iter().copied().find(|&l| need(l) <= cols) {
        return l;
    }
    let smallest = |nonzero: bool| {
        layouts
            .iter()
            .copied()
            .filter(|&l| (bar(l) > 0) == nonzero)
            .min_by_key(|&l| bar(l))
    };
    let fallback = if cols >= MIN_BAR_COLUMNS {
        smallest(true).or_else(|| smallest(false))
    } else {
        smallest(false).or_else(|| smallest(true))
    };
    fallback.unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    fn plain(s: &str) -> String {
        console::strip_ansi_codes(s).into_owned()
    }

    #[test]
    fn meter_fills_and_clamps() {
        assert_eq!(plain(&meter(42.0, 10)), "████░░░░░░");
        assert_eq!(plain(&meter(0.0, 4)), "░░░░");
        assert_eq!(plain(&meter(-5.0, 4)), "░░░░");
        assert_eq!(plain(&meter(150.0, 4)), "████");
        assert_eq!(plain(&meter(f64::NAN, 4)), "░░░░");
        assert_eq!(plain(&meter(42.4, 20)), "████████░░░░░░░░░░░░");
        assert_eq!(plain(&meter(50.0, 0)), "");
        for w in [1, 5, 10, 20] {
            assert_eq!(console::measure_text_width(&meter(33.0, w)), w);
        }
    }

    #[test]
    fn levels_and_markers() {
        assert_eq!(level(0.0), Level::Normal);
        assert_eq!(level(79.4), Level::Normal);
        assert_eq!(level(79.5), Level::High);
        assert_eq!(level(80.0), Level::High);
        assert_eq!(level(94.4), Level::High);
        assert_eq!(level(95.0), Level::Critical);
        assert_eq!(level(130.0), Level::Critical);
        assert_eq!(level(f64::NAN), Level::Normal);
        assert_eq!(percent_text(42.4), "42%");
        assert_eq!(percent_text(79.5), "80%!");
        assert_eq!(percent_text(88.0), "88%!");
        assert_eq!(percent_text(96.0), "96%!!");
        assert_eq!(percent_text(f64::NAN), "0%");
    }

    #[test]
    fn gauge_drops_the_bar_at_width_zero() {
        assert_eq!(plain(&gauge(42.0, 10)), "████░░░░░░ 42%");
        assert_eq!(plain(&gauge(96.0, 5)), "█████ 96%!!");
        assert_eq!(gauge(85.0, 0), "85%!");
    }

    #[test]
    fn columns_override() {
        assert_eq!(parse_columns(Some("120")), Some(120));
        assert_eq!(parse_columns(Some(" 40 ")), Some(40));
        for bad in [None, Some(""), Some("0"), Some("-3"), Some("wide")] {
            assert_eq!(parse_columns(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn layout_fallbacks() {
        let bars = [10, 5, 0];
        // Each cell of bar costs one column on top of a 100-column row.
        let need = |w: usize| 100 + w;
        let pick = |cols| choose_layout(cols, &bars, |w| w, need);
        assert_eq!(pick(None), 10);
        assert_eq!(pick(Some(500)), 10);
        assert_eq!(pick(Some(107)), 5);
        assert_eq!(pick(Some(102)), 0);
        // Nothing fits: keep the compact bar from 80 columns up, none below.
        assert_eq!(pick(Some(99)), 5);
        assert_eq!(pick(Some(MIN_BAR_COLUMNS)), 5);
        assert_eq!(pick(Some(MIN_BAR_COLUMNS - 1)), 0);
        assert_eq!(pick(Some(40)), 0);
        // Layouts carrying more than a bar width.
        let layouts = [(20, true), (10, true), (10, false), (5, false), (0, false)];
        let need = |(w, abs): (usize, bool)| 60 + w + if abs { 20 } else { 0 };
        let pick = |cols| choose_layout(cols, &layouts, |l| l.0, need);
        assert_eq!(pick(None), (20, true));
        assert_eq!(pick(Some(80)), (10, false));
        assert_eq!(pick(Some(65)), (5, false));
        assert_eq!(pick(Some(40)), (0, false));
    }
}
