//! Command-line interface definition.

use clap::{Args, Parser, Subcommand};

use crate::env_detect::Shell;

const ABOUT: &str = "mcc — run several Claude Code accounts side by side.

Each profile is a separate Claude Code config directory (CLAUDE_CONFIG_DIR)
with its own login, launched by its own command. The default profile is
~/.claude, started with `claude`; mcc profiles live in ~/.mcc/profiles/<alias>.";

const EXAMPLES: &str = "\
Examples:
  mcc init                       Detect OS, terminal and shell (runs automatically on first use)
  mcc add work --clone           New profile `work` with the default's setup, then log in
  mcc add personal --fresh       New, empty profile `personal`
  mcc list                       Show all profiles with skills, plugins, MCP servers, size…
  mcc list --json | jq '.[].name'
  mcc usage                      5-hour and weekly usage of every profile, in detail
  mcc usage work --json          The same for `work`, as JSON
  mcc sync work --dry-run        Preview copying the default's setup into `work`
  mcc sync --all                 Bring every profile up to date with the default
  mcc delete personal            Log out and remove the profile completely

Run `mcc help <command>` for details on a command.
Logs: ~/.mcc/logs/mcc.log";

#[derive(Debug, Parser)]
#[command(
    name = "mcc",
    version,
    about = "Manage multiple Claude Code profiles (accounts)",
    long_about = ABOUT,
    after_help = EXAMPLES,
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Show detailed progress (also always written to ~/.mcc/logs/mcc.log)
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Only print errors and requested data
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Disable colored output (also honors the NO_COLOR environment variable)
    #[arg(long, global = true)]
    pub no_color: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Detect OS, terminal, shell and rc file so mcc's setup takes effect
    #[command(after_help = "Examples:\n  mcc init\n  mcc init --force --shell bash")]
    Init(InitArgs),

    /// Add a Claude Code profile (creates the default one if ~/.claude is missing)
    #[command(
        after_help = "Examples:\n  mcc add                       Interactive: asks for alias and setup\n  mcc add work --clone          Copy the default's setup (no credentials), then log in\n  mcc add lab --fresh --no-login\n\nAfter it finishes, start the profile with its alias, e.g. `work`."
    )]
    Add(AddArgs),

    /// List all profiles with their metrics
    #[command(
        visible_alias = "ls",
        after_help = "Examples:\n  mcc list\n  mcc list --long\n  mcc list --no-usage\n  mcc list --json | jq '.[] | {name, skills, plugins_installed}'\n  mcc list --json | jq '.[] | {name, five_hour: .usage.five_hour.percent}'\n\n5H and WEEK show each account's 5-hour and weekly usage (a bar, percent used\nand time until reset), read with the profile's own login; `!` marks 80% or\nmore and `!!` 95% or more. Bars are dropped when the terminal is too narrow.\n\"unavailable\" when it can't be determined (`mcc list --long` says why);\n`mcc usage` shows every window in detail."
    )]
    List(ListArgs),

    /// Show each profile's 5-hour and weekly usage in detail
    #[command(
        after_help = "Examples:\n  mcc usage                     Every profile, the default included\n  mcc usage work                Only the `work` profile\n  mcc usage default             Only the default profile (~/.claude)\n  mcc usage --json | jq '.[] | {name, weekly: .usage.weekly.percent}'\n\nEach window shows a bar of the limit used, the percent used and left, and\nwhen it resets. `!` marks 80% or more and `!!` 95% or more; bars shrink or\ndisappear when the terminal is narrow. A profile whose usage can't be read\nshows \"unavailable\" and why. MCC_NO_USAGE=1 skips fetching (no network)."
    )]
    Usage(UsageArgs),

    /// Delete a profile: log out, remove its directory, command and shell alias
    #[command(
        visible_alias = "rm",
        after_help = "Examples:\n  mcc delete lab\n  mcc delete lab --yes --keep-auth"
    )]
    Delete(DeleteArgs),

    /// Copy the default profile's setup into a profile (one-way: default → profile)
    #[command(
        after_help = "Items: settings, claude-md, keybindings, statusline, agents, skills,\n       commands, hooks, output-styles, plugins, mcp\n\nExamples:\n  mcc sync work --dry-run\n  mcc sync work --only skills,agents\n  mcc sync --all --prune"
    )]
    Sync(SyncArgs),

    /// Print a shell completion script to stdout
    #[command(
        after_help = "Examples:\n  mcc completions zsh > ~/.zfunc/_mcc\n  mcc completions bash > ~/.local/share/bash-completion/completions/mcc\n  mcc completions fish > ~/.config/fish/completions/mcc.fish"
    )]
    Completions(CompletionsArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Re-run detection even if mcc is already initialized
    #[arg(long)]
    pub force: bool,

    /// Shell whose rc file mcc manages (default: detected from $SHELL)
    #[arg(long, value_enum)]
    pub shell: Option<Shell>,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Command name for the new profile (prompted if omitted)
    pub alias: Option<String>,

    /// Copy the default profile's setup (settings, skills, plugins, MCP…; never credentials)
    #[arg(long, conflicts_with = "fresh")]
    pub clone: bool,

    /// Start with an empty profile
    #[arg(long)]
    pub fresh: bool,

    /// Skip `claude auth login` (log in later by running the alias)
    #[arg(long)]
    pub no_login: bool,

    /// Accept defaults without prompting
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Output JSON (to stdout) instead of a table
    #[arg(long, conflicts_with = "long")]
    pub json: bool,

    /// Show a detailed card per profile
    #[arg(short, long)]
    pub long: bool,

    /// Skip fetching 5-hour/weekly usage (no network; also MCC_NO_USAGE=1)
    #[arg(long)]
    pub no_usage: bool,
}

#[derive(Debug, Args)]
pub struct UsageArgs {
    /// Only this profile (`default` or `claude` for ~/.claude)
    pub profile: Option<String>,

    /// Output JSON (to stdout) instead of text
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Profile to delete
    pub alias: String,

    /// Skip the confirmation prompt
    #[arg(short, long)]
    pub yes: bool,

    /// Do not run `claude auth logout` before deleting
    #[arg(long)]
    pub keep_auth: bool,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Profile to update from the default
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub alias: Option<String>,

    /// Sync every mcc profile
    #[arg(long)]
    pub all: bool,

    /// Show what would change without writing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Also remove items that exist in the profile but not in the default
    #[arg(long)]
    pub prune: bool,

    /// Only sync these items (comma-separated)
    #[arg(long, value_delimiter = ',', value_name = "ITEMS")]
    pub only: Option<Vec<String>>,

    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }
}
