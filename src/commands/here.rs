//! `mcc here` — which profile, and so which account, `mcc run` starts in the
//! current directory, and why: the default of this folder, inherited from a
//! parent folder, or none (the default profile, ~/.claude).
//!
//! It resolves the profile exactly as `mcc run` does (the same
//! [`super::status::here`] the bare `mcc` status view prints, which calls
//! [`super::run::applies_in`]), so the two never disagree. Read-only, even
//! before mcc is set up: no lock, no first-run setup, no log line, no usage
//! lookup, nothing written, and `claude` is never run. The account comes
//! from the files Claude Code and mcc already keep (`.claude.json`,
//! `~/.mcc/config.toml`); its email is hidden unless `[privacy] show_email`
//! is set, as in `mcc list`.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::{Account, Config};
use crate::paths::Paths;
use crate::{metrics, ui};

use super::run::Origin;

pub fn run(paths: &Paths, cfg: &Config) -> Result<()> {
    let here = super::status::here(paths, cfg);
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", here.line)?;
    if let Some(cwd) = &here.cwd {
        writeln!(out, "  directory  {}", paths.display(cwd))?;
    }
    if let Some((dir, account)) = profile_of(paths, cfg, here.name) {
        writeln!(out, "  account    {}", account_text(cfg, &account))?;
        writeln!(out, "  config     {}", paths.display(&dir))?;
    }
    drop(out);

    if let Some(w) = &here.warning {
        ui::warn(&format!("`mcc run` stops with an error here: {w}"));
    }
    match here.origin {
        Origin::Project(..) => ui::hint(
            "set on this folder with `mcc project set`; folders below it without a default of their own inherit it, and `mcc project unset` removes it",
        ),
        Origin::Inherited(dir, _) => ui::hint(&format!(
            "set on {} with `mcc project set`, and inherited by every folder below it without a default of its own; `mcc project set <profile> --path .` gives this folder its own",
            paths.display(Path::new(dir))
        )),
        Origin::Builtin => ui::hint(
            "no folder from here up to / has a default, so `mcc run` starts the default profile (~/.claude); give one a default with `mcc project set <profile>`",
        ),
    }
    if cfg.env.is_none() {
        ui::info(
            "mcc is not set up yet (no ~/.mcc/config.toml with its [env]): `mcc run` here starts the default profile (~/.claude); run `mcc init` to set mcc up",
        );
    }
    Ok(())
}

/// The config directory and account of profile `name`; `None` when it
/// names no profile (`mcc run` then stops with an error).
fn profile_of(paths: &Paths, cfg: &Config, name: &str) -> Option<(PathBuf, Account)> {
    if name == "default" {
        let account = metrics::account_info(&paths.default_json).unwrap_or_default();
        return Some((paths.default_dir.clone(), account));
    }
    let p = cfg.profiles.get(name)?;
    Some((p.dir.clone(), metrics::profile_account(p)))
}

/// The account as `mcc list` shows it: the email (only when emails are
/// shown) and the organization (left out when it contains an email and
/// emails are hidden, as personal plans' organizations do).
fn account_text(cfg: &Config, a: &Account) -> String {
    let show = cfg.show_email();
    // Hand-edited or odd state files could hold control characters: never
    // print those.
    fn clean(v: &Option<String>) -> Option<&str> {
        v.as_deref()
            .filter(|s| !s.is_empty() && !super::has_control(s))
    }
    let email = clean(&a.email);
    let org = clean(&a.organization).filter(|o| show || !o.contains('@'));
    match (email, org) {
        (Some(e), Some(o)) if show => format!("{e} ({o})"),
        (Some(e), None) if show => e.to_string(),
        (Some(_), Some(o)) => format!("{o} (email hidden)"),
        (Some(_), None) => "signed in (email hidden)".to_string(),
        (None, Some(o)) => o.to_string(),
        (None, None) => "unknown (not signed in, or not recorded yet)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(email: Option<&str>, org: Option<&str>) -> Account {
        Account {
            email: email.map(str::to_string),
            organization: org.map(str::to_string),
            ..Account::default()
        }
    }

    fn cfg(show_email: bool) -> Config {
        let mut cfg = Config::default();
        cfg.privacy.show_email = Some(show_email);
        cfg
    }

    #[test]
    fn the_account_hides_its_email_unless_shown() {
        let (hidden, shown) = (cfg(false), cfg(true));
        let work = account(Some("me@acme.com"), Some("Acme"));
        assert_eq!(account_text(&hidden, &work), "Acme (email hidden)");
        assert_eq!(account_text(&shown, &work), "me@acme.com (Acme)");
        // A personal plan's organization names the email: hidden too.
        let personal = account(Some("me@x.com"), Some("me@x.com's Organization"));
        assert_eq!(account_text(&hidden, &personal), "signed in (email hidden)");
        assert_eq!(
            account_text(&shown, &personal),
            "me@x.com (me@x.com's Organization)"
        );
        assert_eq!(
            account_text(&hidden, &account(None, None)),
            "unknown (not signed in, or not recorded yet)"
        );
        assert_eq!(
            account_text(&hidden, &account(None, Some("Acme\u{1b}[2J"))),
            "unknown (not signed in, or not recorded yet)"
        );
    }
}
