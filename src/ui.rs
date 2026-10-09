//! Terminal output: headers, numbered steps, status lines, prompts.
//!
//! Everything here writes to **stderr** so stdout stays clean for data
//! (`mcc list --json | jq`). Every line is also mirrored to the log file.

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
    let _ = SETTINGS.set(Settings { verbose, quiet, color, interactive });
}

pub fn settings() -> Settings {
    *SETTINGS.get().unwrap_or(&Settings { verbose: false, quiet: false, color: false, interactive: false })
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
    emit(&format!("{} {}", style("◆").cyan().bold(), style(title).bold()));
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
        emit(&format!("    {:<18} {value}", style(format!("{key}:")).dim()));
    }
}

/// Closing summary: `Result: DONE — …` style line.
pub fn result(ok: bool, msg: &str) {
    log::write(if ok { "INFO" } else { "WARN" }, &format!("result: {msg}"));
    if settings().quiet && ok {
        return;
    }
    let label = if ok { style("Result:").green().bold() } else { style("Result:").yellow().bold() };
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
        log::write("INFO", &format!("[{}/{}] {label}", self.current, self.total));
        if !settings().quiet {
            emit(&format!(
                "  {} {label}",
                style(format!("[{}/{}]", self.current, self.total)).cyan()
            ));
        }
        Step { label: label.to_string(), started: Instant::now(), index: self.current, total: self.total, finished: false }
    }
}

impl Step {
    fn finish(&mut self, mark: console::StyledObject<&str>, word: &str, note: &str) {
        self.finished = true;
        let ms = self.started.elapsed().as_millis();
        let note_s = if note.is_empty() { String::new() } else { format!(" — {note}") };
        log::write(
            "INFO",
            &format!("[{}/{}] {} {word}{note_s} ({ms}ms)", self.index, self.total, self.label),
        );
        if !settings().quiet {
            emit(&format!("        {mark} {word}{note_s} {}", style(format!("({ms}ms)")).dim()));
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
        Err(CliError::usage(format!("{what} is required but no terminal is attached"))
            .hint(flag_hint.to_string())
            .into())
    }
}

pub fn input(prompt: &str, flag_hint: &str) -> anyhow::Result<String> {
    require_interactive(prompt, flag_hint)?;
    let v: String = dialoguer::Input::with_theme(&theme()).with_prompt(prompt).interact_text()?;
    Ok(v.trim().to_string())
}

pub fn select(prompt: &str, items: &[&str], default: usize, flag_hint: &str) -> anyhow::Result<usize> {
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
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
