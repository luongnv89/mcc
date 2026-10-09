//! `mcc completions <shell>` — print a completion script to stdout.

use anyhow::Result;
use clap::CommandFactory;

use crate::cli::{Cli, CompletionsArgs};

pub fn run(args: &CompletionsArgs) -> Result<()> {
    clap_complete::generate(args.shell, &mut Cli::command(), "mcc", &mut std::io::stdout());
    Ok(())
}
