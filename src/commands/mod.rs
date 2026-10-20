pub mod add;
pub mod completions;
pub mod config;
pub mod delete;
pub mod diff;
pub mod env;
pub mod exec;
pub mod init;
pub mod list;
pub mod project;
pub mod refresh_launchers;
pub mod rename;
pub mod run;
pub mod share;
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
