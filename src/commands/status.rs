//! Bare `mcc` once mcc is set up: one screen with the profile that applies
//! in the current directory and why, then every profile's account (label,
//! else organization), cached usage, last use and drift — `mcc list`'s
//! table — and warnings for drift, out-of-date launchers and other obvious
//! problems, with a pointer to `mcc doctor`.
//!
//! Fast and read-only: no lock, no first-run setup, no log line, nothing
//! written. Usage comes from the `mcc statusline` cache only, never looked
//! up live (even with `[usage] live = true`): no token is read and nothing
//! goes over the network. Counts and sizes on disk are left to `mcc list`.
//! Account emails are hidden unless `[privacy] show_email` is set, and
//! colors follow `--no-color`, `NO_COLOR` and whether stdout is a terminal.
//!
//! Before mcc is set up (no `[env]` in `~/.mcc/config.toml`), bare `mcc`
//! prints the help as it always did (see `main.rs`).

use std::io::Write;
use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::metrics::ProfileMetrics;
use crate::paths::Paths;
use crate::{manifest, ui, usage};

pub fn run(paths: &Paths, cfg: &Config) -> Result<()> {
    let cached = (!usage::disabled_by_env()).then_some(false);
    let mut all = super::list::profiles(paths, cfg, false, cached);
    super::list::redact(&mut all, cfg.show_email());

    let here = here(paths, cfg);
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", here.line)?;
    writeln!(out)?;
    super::list::table(&mut out, &all)?;
    drop(out);

    let mut problems = 0;
    if let Some(w) = &here.warning {
        ui::warn(w);
        problems += 1;
    }
    for m in &all {
        for w in &m.warnings {
            ui::warn(&format!("{}: {w}", m.name));
            problems += 1;
        }
    }
    if let Some(w) = drift_warning(&all) {
        ui::warn(&w);
        problems += 1;
    }
    super::list::sharing_notes(&all, cfg.launch.banner);
    problems += all.iter().filter(|m| !m.launcher_current).count();
    let names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
    let usages: Vec<&usage::Usage> = all.iter().map(|m| &m.usage).collect();
    if let Some(hint) = usage::no_data_hint(&names, &usages) {
        ui::hint(&hint);
    }
    if problems > 0 {
        ui::hint("run `mcc doctor` for a full check of every profile, with a fix for each problem");
    }
    ui::info(&format!(
        "{} profile{} · `mcc list --long` for every metric · `mcc help` for every command",
        all.len(),
        if all.len() == 1 { "" } else { "s" },
    ));
    Ok(())
}

/// The first line, and a warning when the project's default names no profile.
struct Here {
    line: String,
    warning: Option<String>,
}

/// "Here: `work` (Acme) — the default for ~/code/app", or "Here: `default`
/// — no project default here": the profile `mcc run` (and `mcc env`) use in
/// the current directory, and why.
fn here(paths: &Paths, cfg: &Config) -> Here {
    let cwd = super::project::current_dir().ok();
    let project = cwd.as_deref().and_then(|d| super::project::lookup(cfg, d));
    let name = project.map_or("default", |(_, p)| p.profile.as_str());
    let why = super::run::why(paths, project);
    let known = name == "default" || cfg.profiles.contains_key(name);
    // A profile name hand-written in the config file could hold control
    // characters: never print those.
    let shown_name = if super::has_control(name) { "?" } else { name };
    let tick = ui::paint(&format!("`{shown_name}`"), super::profile_color(cfg, name));
    let shown = match super::shown_label(cfg, name) {
        Some(label) => format!("{tick} ({label})"),
        None => tick,
    };
    let mut line = format!("Here: {shown} — {why}");
    let mut warning = None;
    if !known && let Some((dir, _)) = project {
        line.push_str(" (no such profile)");
        warning = Some(format!(
            "the project default of {} is \"{shown_name}\", which is not a profile — set another with `mcc project set <profile>`, or remove it with `mcc project unset`",
            paths.display(Path::new(dir))
        ));
    }
    Here { line, warning }
}

/// One warning naming every profile a sync would change: "drift: `work` (3
/// behind), `lab` (1 conflict) — …". Local edits alone are kept by design
/// and aren't drift, and neither is a fresh profile never synced from the
/// default (as `mcc doctor` sees it).
fn drift_warning(all: &[ProfileMetrics]) -> Option<String> {
    let drifted: Vec<String> = all
        .iter()
        .filter(|m| !never_synced(m))
        .filter_map(|m| {
            let s = m.sync.as_ref()?;
            (s.behind > 0 || s.conflicts > 0)
                .then(|| format!("`{}` ({})", m.name, super::sync::status_text(s)))
        })
        .collect();
    (!drifted.is_empty()).then(|| {
        format!(
            "drift against the default: {} — review with `mcc diff <alias>`, update with `mcc sync <alias>`",
            drifted.join(", ")
        )
    })
}

/// A `--fresh` profile no `mcc sync` has copied the default's setup into.
fn never_synced(m: &ProfileMetrics) -> bool {
    m.mode.as_deref() == Some("fresh")
        && matches!(manifest::load(&m.config_dir), manifest::Loaded::Missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copy::Status;

    fn metrics(name: &str, sync: Option<Status>) -> ProfileMetrics {
        let mut m = crate::metrics::default_profile(
            &Paths::new(std::path::PathBuf::from("/nonexistent")),
            false,
        );
        m.name = name.into();
        m.is_default = false;
        m.sync = sync;
        m
    }

    #[test]
    fn drift_names_profiles_a_sync_would_change() {
        let status = |behind, local_edits, conflicts| Status {
            behind,
            local_edits,
            conflicts,
        };
        let all = [
            metrics("ok", Some(status(0, 0, 0))),
            metrics("edited", Some(status(0, 2, 0))),
            metrics("work", Some(status(3, 0, 0))),
            metrics("lab", Some(status(0, 1, 1))),
            metrics("gone", None),
        ];
        let w = drift_warning(&all).unwrap();
        assert!(w.contains("`work` (3 behind)"), "{w}");
        assert!(w.contains("`lab` (1 local edit, 1 conflict)"), "{w}");
        assert!(
            !w.contains("`ok`") && !w.contains("`edited`") && !w.contains("`gone`"),
            "{w}"
        );
        assert_eq!(drift_warning(&all[..2]), None);
    }
}
