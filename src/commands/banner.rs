//! The launch banner (`[launch] banner = true`): one line naming the profile
//! that starts, e.g. `▶ work · Acme · 5h 42% · week 18%`.
//!
//! A profile's launcher runs the hidden `mcc launch-banner <alias>` before
//! claude, only when its stderr is a terminal, with mcc's stdout sent to that
//! stderr and mcc's own stderr discarded; `mcc run` prints the same line on
//! stderr itself. Either way nothing reaches the stdout Claude Code writes to,
//! so `<alias> -p "…" | jq` sees only Claude Code's output.
//!
//! The banner runs on every launch, so it stays cheap and silent: it reads
//! config.toml and the usage `mcc statusline` cached, never the network, a
//! token or the Keychain (even with `[usage] live = true`), takes no lock,
//! writes nothing, logs nothing and never fails.

use std::io::{IsTerminal, Write};

use crate::config::Config;
use crate::paths::Paths;
use crate::ui;
use crate::usage::{self, Status, Usage};

/// `mcc launch-banner <alias>`: the banner on stdout (the launcher sends it
/// to its stderr) when it is on and stdout is a terminal. Never fails: a
/// missing or broken config, or an unknown profile, prints nothing.
pub fn run(alias: &str) {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let Ok(paths) = Paths::from_env() else { return };
    let Ok(cfg) = Config::load(&paths.config_file) else {
        return;
    };
    if !cfg.launch.banner || !cfg.profiles.contains_key(alias) {
        return;
    }
    let line = text(&cfg, alias, &cached(&paths, &cfg, alias), |t, c| {
        ui::paint(t, c)
    });
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// For `mcc run`: the banner on stderr, just before claude starts, when it
/// is on, stderr is a terminal and `--quiet` is not set.
pub fn show_on_stderr(paths: &Paths, cfg: &Config, name: &str) {
    if !cfg.launch.banner || ui::settings().quiet || !std::io::stderr().is_terminal() {
        return;
    }
    let line = text(cfg, name, &cached(paths, cfg, name), |t, c| {
        ui::paint_stderr(t, c)
    });
    eprintln!("{line}");
}

/// The profile's usage from the statusline cache only (`MCC_NO_USAGE`
/// skips it): never the live lookup, whatever `[usage] live` says.
fn cached(paths: &Paths, cfg: &Config, name: &str) -> Usage {
    if usage::disabled_by_env() {
        return Usage::default();
    }
    let config_dir = cfg
        .profiles
        .get(name)
        .map_or(paths.default_dir.clone(), |p| p.dir.clone());
    let target = usage::Target {
        name: name.to_string(),
        config_dir,
        is_default: !cfg.profiles.contains_key(name),
    };
    usage::read_all(paths, std::slice::from_ref(&target))
        .pop()
        .unwrap_or_default()
}

/// The banner line: `▶`, the profile's name in its color (`paint`), its
/// label (as `mcc run` shows it: none with control characters, nor one
/// with an email while emails are hidden), then the cached 5-hour and
/// weekly usage that is still current. Parts that are unknown are left out.
fn text(
    cfg: &Config,
    name: &str,
    usage: &Usage,
    paint: impl Fn(&str, Option<&str>) -> String,
) -> String {
    let mut parts = vec![paint(name, super::profile_color(cfg, name))];
    if let Some(label) = super::shown_label(cfg, name) {
        parts.push(label.to_string());
    }
    if usage.status == Status::Ok {
        for (what, window) in [("5h", &usage.five_hour), ("week", &usage.weekly)] {
            if let Some(w) = window {
                parts.push(format!("{what} {}", usage::percent(w.percent)));
            }
        }
    }
    format!("▶ {}", parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{Reason, Window};

    fn config(extra: &str) -> Config {
        toml::from_str(&format!(
            "[profiles.work]\ndir = \"/d\"\nmode = \"clone\"\ncreated_at = \"t\"\n{extra}"
        ))
        .unwrap()
    }

    fn plain(t: &str, _: Option<&str>) -> String {
        t.to_string()
    }

    fn window(percent: f64) -> Option<Window> {
        Some(Window {
            percent,
            resets_at: None,
        })
    }

    #[test]
    fn banner_names_the_profile_its_label_and_cached_usage() {
        let cfg = config("label = \"Acme\"\ncolor = \"blue\"\n");
        let u = Usage::ok(window(42.4), window(18.0));
        assert_eq!(
            text(&cfg, "work", &u, plain),
            "▶ work · Acme · 5h 42% · week 18%"
        );
        // The name is painted in the profile's color.
        let painted = text(&cfg, "work", &u, |t, c| {
            format!("<{}>{t}", c.unwrap_or("-"))
        });
        assert!(painted.starts_with("▶ <blue>work · "), "{painted}");
    }

    #[test]
    fn unknown_parts_are_left_out() {
        let cfg = config("");
        let none = Usage::unavailable(Reason::NoData);
        assert_eq!(text(&cfg, "work", &none, plain), "▶ work");
        assert_eq!(text(&cfg, "work", &Usage::default(), plain), "▶ work");
        let five_only = Usage::ok(window(7.0), None);
        assert_eq!(text(&cfg, "work", &five_only, plain), "▶ work · 5h 7%");
    }

    #[test]
    fn labels_that_could_mislead_or_leak_are_left_out() {
        let none = Usage::default();
        let cfg = config("label = \"me@example.com\"\n");
        assert_eq!(text(&cfg, "work", &none, plain), "▶ work");
        let cfg = config("label = \"Acme\\u001b[2J\"\n");
        assert_eq!(text(&cfg, "work", &none, plain), "▶ work");
    }
}
