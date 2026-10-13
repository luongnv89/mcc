pub mod add;
pub mod completions;
pub mod delete;
pub mod init;
pub mod list;
pub mod rename;
pub mod sync;
pub mod usage;

use crate::error::CliError;

/// The usage error for an alias that names no profile.
pub fn unknown_profile(alias: &str) -> anyhow::Error {
    CliError::usage(format!("no profile named \"{alias}\""))
        .hint("run `mcc list` to see your profiles")
        .into()
}
