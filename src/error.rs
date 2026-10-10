//! Error type carrying an exit code and an optional "how to fix it" hint.

use std::fmt;

/// Exit codes (canonical table).
pub const EXIT_RUNTIME: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_INPUT: i32 = 3;
pub const EXIT_INTERRUPTED: i32 = 130;

#[derive(Debug)]
pub struct CliError {
    pub code: i32,
    pub message: String,
    pub hint: Option<String>,
}

impl CliError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_USAGE,
            message: message.into(),
            hint: None,
        }
    }

    pub fn input(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_INPUT,
            message: message.into(),
            hint: None,
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_RUNTIME,
            message: message.into(),
            hint: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// Map any error to (exit code, hint), looking through anyhow context chains.
pub fn classify(err: &anyhow::Error) -> (i32, Option<String>) {
    for cause in err.chain() {
        if let Some(e) = cause.downcast_ref::<CliError>() {
            return (e.code, e.hint.clone());
        }
        if let Some(e) = cause.downcast_ref::<dialoguer::Error>() {
            let dialoguer::Error::IO(io) = e;
            if io.kind() == std::io::ErrorKind::Interrupted {
                return (EXIT_INTERRUPTED, None);
            }
        }
    }
    (EXIT_RUNTIME, None)
}
