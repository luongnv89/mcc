pub mod add;
pub mod banner;
pub mod completions;
pub mod config;
pub mod delete;
pub mod diff;
pub mod doctor;
pub mod env;
pub mod exec;
pub mod init;
pub mod link;
pub mod list;
pub mod project;
pub mod refresh_launchers;
pub mod rename;
pub mod run;
pub mod share;
pub mod status;
pub mod statusline;
pub mod sync;
pub mod usage;

use std::io::Write;

use anyhow::Result;
use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::error::CliError;

/// The schema version of the `--json-v2` envelope: `{"schema": 2, "<key>": [...]}`.
///
/// The bare array printed by `--json` is schema 1 and keeps its shape: fields
/// may be added to its objects, never renamed or removed. A change that breaks
/// either shape bumps this number (and gets a flag of its own).
pub const JSON_SCHEMA: u32 = 2;

/// `{"schema": JSON_SCHEMA, key: items}`, with `schema` first.
struct Envelope<'a, T: ?Sized> {
    key: &'a str,
    items: &'a T,
}

impl<T: Serialize + ?Sized> Serialize for Envelope<'_, T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("schema", &JSON_SCHEMA)?;
        map.serialize_entry(self.key, self.items)?;
        map.end()
    }
}

/// Print `items` as pretty JSON and a newline: the bare array (`--json`,
/// schema 1), or wrapped in the versioned envelope under `key` (`--json-v2`).
pub fn write_json<T: Serialize + ?Sized>(
    out: &mut impl Write,
    key: &str,
    items: &T,
    envelope: bool,
) -> Result<()> {
    if envelope {
        serde_json::to_writer_pretty(&mut *out, &Envelope { key, items })?;
    } else {
        serde_json::to_writer_pretty(&mut *out, items)?;
    }
    writeln!(out)?;
    Ok(())
}

/// The usage error for an alias that names no profile.
pub fn unknown_profile(alias: &str) -> anyhow::Error {
    CliError::usage(format!("no profile named \"{alias}\""))
        .hint("run `mcc list` to see your profiles")
        .into()
}

/// The label shown for profile `alias` outside `mcc list` (`mcc run`, the
/// statusline): `None` when it has none (the default profile never has one),
/// while emails are hidden for a label containing one, as `mcc list` hides
/// it, and for a label with control characters (hand-written in the config
/// file; `mcc add` and `mcc config set` refuse them), which could drive the
/// terminal.
pub(crate) fn shown_label<'a>(cfg: &'a crate::config::Config, alias: &str) -> Option<&'a str> {
    let label = cfg.profiles.get(alias)?.label.as_deref()?;
    (!has_control(label) && (cfg.show_email() || !label.contains('@'))).then_some(label)
}

/// Whether `s` contains control characters (ESC, BEL, newline, ...), which
/// `add::check_label` refuses in a label.
pub(crate) fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Profile `alias`'s color, as set; `None` for the default profile.
pub(crate) fn profile_color<'a>(cfg: &'a crate::config::Config, alias: &str) -> Option<&'a str> {
    cfg.profiles.get(alias)?.color.as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(label: &str) -> crate::config::Config {
        let label = toml::Value::String(label.into());
        toml::from_str(&format!(
            "[profiles.work]\ndir = \"/d\"\nmode = \"clone\"\ncreated_at = \"t\"\nlabel = {label}\n"
        ))
        .unwrap()
    }

    #[test]
    fn shown_label_hides_a_label_with_control_characters() {
        assert_eq!(shown_label(&config("Acme"), "work"), Some("Acme"));
        for bad in ["Acme\u{1b}]0;pwned\u{7}\u{1b}[2J", "Acme\nwork", "\u{7}"] {
            assert_eq!(shown_label(&config(bad), "work"), None, "{bad:?}");
        }
    }
}
