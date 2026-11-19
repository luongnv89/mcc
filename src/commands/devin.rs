//! `mcc devin [-- ARGS]` — start the Devin CLI as it is (#124).
//!
//! The `devin` binary ([`crate::devin::find_binary`]) replaces mcc with the
//! arguments as given: no `--config`, and no change to `HOME`, `XDG_*` or
//! any other variable, so Devin uses its default configuration and the
//! login its own `devin auth login` keeps in its credentials file. mcc
//! never signs Devin in and never writes that file or `~/.local/bin/devin`.

use std::os::unix::process::CommandExt;

use anyhow::Result;

use crate::cli::DevinArgs;
use crate::paths::Paths;
use crate::{devin, ui};

pub fn run(paths: &Paths, args: &DevinArgs) -> Result<()> {
    let bin = devin::require_binary(paths)?;
    if args.dry_run {
        let words: Vec<String> = std::iter::once(bin.as_os_str())
            .chain(args.args.iter().map(|a| a.as_os_str()))
            .map(|w| crate::shell::sh_quote(&w.to_string_lossy()))
            .collect();
        println!("{}", words.join(" "));
        return Ok(());
    }
    // Starting Devin without a login is how one signs in (`devin auth
    // login`), so a missing login is only a hint.
    let signing_in = args.args.first().is_some_and(|a| a == "auth");
    if !devin::detected(paths) && !signing_in {
        ui::hint(&format!(
            "no Devin login at {}: sign in with `devin auth login` (or `mcc devin -- auth login`)",
            paths.display(&paths.devin_credentials)
        ));
    }
    crate::log::write("INFO", &format!("devin: exec {}", bin.display()));
    let err = std::process::Command::new(&bin).args(&args.args).exec();
    Err(anyhow::Error::new(err).context(format!("starting {}", bin.display())))
}
