//! Command-line interface definition.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::env_detect::Shell;

/// The terms-of-service note printed the first time session sharing is
/// turned on for a profile.
/// A macro so the help text below can include it verbatim.
macro_rules! share_tos_note {
    () => {
        "⚠ Session sharing makes conversation history available across your Claude Code accounts. Use it only between accounts you own. Don't use it to get around usage limits, and never let other people use your account or credentials — doing so can violate Anthropic's Consumer Terms (https://www.anthropic.com/legal/consumer-terms) and Usage Policy (https://www.anthropic.com/legal/aup), including the updated Usage Policy that takes effect November 12, 2026."
    };
}
pub(crate) use share_tos_note;

/// The one-line reminder printed instead of the note once a profile's
/// sharing note was shown. A macro so the help text below can include it.
macro_rules! share_tos_reminder {
    () => {
        "⚠ Session sharing is on: use it only between accounts you own, never to get around usage limits or to let other people use your account (Anthropic's Consumer Terms and Usage Policy; full note: `mcc share --help`)."
    };
}
pub(crate) use share_tos_reminder;

/// The terms-of-service note printed whenever `mcc run` starts a profile
/// other than the project's. A macro so the help text below can include it
/// verbatim.
macro_rules! run_tos_note {
    () => {
        "⚠ You chose to start an account other than this project's. Use only accounts you own, and don't use several accounts to get around usage limits — doing so can violate Anthropic's Consumer Terms (https://www.anthropic.com/legal/consumer-terms) and Usage Policy (https://www.anthropic.com/legal/aup), including the updated Usage Policy that takes effect November 12, 2026."
    };
}
pub(crate) use run_tos_note;

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
  mcc rename work job            Rename profile `work` (and its command) to `job`
  mcc share work                 Share `work`'s sessions with your other sharing profiles
  mcc project set work           Start `work` in this project (git repo) with `mcc run`
  mcc run                        Start Claude Code with this project's profile
  mcc statusline install         Show the profile in Claude Code's status line, cache rate limits
  mcc refresh-launchers          Update older launchers so `<alias> --mcc-share` works
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
        after_help = "Examples:\n  mcc add                       Interactive: asks for alias and setup\n  mcc add work --clone          Copy the default's setup (no credentials), then log in\n  mcc add lab --fresh --no-login\n  mcc add side --clone --label personal\n\nAfter logging in, mcc records the account's email and organization under\n[profiles.ALIAS.account] in ~/.mcc/config.toml (shown by `mcc list`) and\nwarns when another profile is already logged in to the same account. It\nthen asks for an optional label, such as \"personal\" or the work\norganization; --label sets it without asking, and -y skips it.\n\nAfter it finishes, start the profile with its alias, e.g. `work`."
    )]
    Add(AddArgs),

    /// List all profiles with their metrics
    #[command(
        visible_alias = "ls",
        after_help = "Examples:\n  mcc list\n  mcc list --long\n  mcc list --no-usage\n  mcc list --json | jq '.[] | {name, skills, plugins_installed}'\n  mcc list --json | jq '.[] | {name, five_hour: .usage.five_hour.percent}'\n\n5H and WEEK show each account's 5-hour and weekly usage (a bar, percent used\nand time until reset) as `mcc statusline` cached it, and UPDATED how old it\nis; `!` marks 80% or more and `!!` 95% or more. Bars shrink to fit the\nterminal; under 80 columns they are dropped when even the smallest does not\nfit. \"—\" when nothing current is cached (run /usage in Claude Code);\n`[usage] live = true` looks usage up with each profile's login instead\n(`mcc list --long` says why it is unavailable). `mcc usage` shows every\nwindow in detail."
    )]
    List(ListArgs),

    /// Show each profile's 5-hour and weekly usage in detail
    #[command(
        after_help = "Examples:\n  mcc usage                     Every profile, the default included\n  mcc usage work                Only the `work` profile\n  mcc usage default             Only the default profile (~/.claude)\n  mcc usage --show-email        Also show each profile's account email\n  mcc usage --json | jq '.[] | {name, weekly: .usage.weekly.percent}'\n\nOne table row per profile: for the 5-hour and weekly windows, a bar of the\nlimit used with its percent, then the time until it resets (and the reset\ntime when it fits). `!` marks 80% or more and `!!` 95% or more. On a narrow\nterminal the reset time goes first, then bars shrink; under 80 columns bars\nare dropped when nothing fits. UPDATED is how old the numbers cached by\n`mcc statusline` are; a profile with nothing current cached shows \"—\"\n(run /usage in Claude Code), and one whose usage can't be read shows\n\"unavailable\" and why. MCC_NO_USAGE=1 skips usage entirely.\n\nAccount emails are hidden by default (also `null` in --json). To always\nshow them, or to look usage up live with each profile's login (off by\ndefault; a one-time note explains what it sends), set in ~/.mcc/config.toml:\n\n  [usage]\n  show_email = true\n  live = true"
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

    /// Rename a profile: its command, launcher, shell alias and registry entry
    #[command(
        visible_alias = "mv",
        after_help = "Examples:\n  mcc rename work job\n\nThe new name is checked like `mcc add` checks an alias: it must not be an\nexisting profile, command, alias, function or shell builtin. The profile's\ndirectory, login and setup stay as they are; only the command changes."
    )]
    Rename(RenameArgs),

    /// Share a profile's sessions with your other sharing profiles (off by default)
    #[command(after_help = concat!(
        "Examples:\n",
        "  mcc share work                Share `work`'s sessions with every other sharing profile\n",
        "  mcc share work --off          Stop sharing; `work` keeps a copy of the shared sessions\n",
        "  work --mcc-share              Start `work`, turning sharing on first (like `mcc share work`)\n",
        "  work --mcc-share --resume     …then pick a session from any sharing profile\n",
        "\n",
        "Sharing links the profile's projects/ directory (the sessions `claude --resume`\n",
        "and /resume list) to ~/.mcc/shared/projects, which every sharing profile uses.\n",
        "Nothing else is shared: credentials, the account (.claude.json), prompt history,\n",
        "live sessions and shell snapshots stay per profile. The default profile\n",
        "(~/.claude) can't share. `--mcc-share` must be the first argument; launchers\n",
        "written by an older mcc need `mcc refresh-launchers` once before it works.\n",
        "\n",
        "The setting is kept per profile in ~/.mcc/config.toml (default: off):\n",
        "\n",
        "  [profiles.work]\n",
        "  share_sessions = true  # default: false; set it with `mcc share`\n",
        "\n",
        "Editing the key by hand does nothing on its own: `mcc share <alias>` links the\n",
        "directory. The first time sharing is turned on for a profile, this note is\n",
        "printed on stderr, and when is kept as `share_note_acknowledged_at`:\n",
        "\n",
        share_tos_note!(),
        "\n\n",
        "After that, each `mcc share` or `--mcc-share` prints a one-line reminder:\n",
        "\n",
        share_tos_reminder!()
    ))]
    Share(ShareArgs),

    /// Choose the profile `mcc run` starts with in a project
    #[command(
        after_help = "Examples:\n  mcc project set work          `mcc run` starts `work` in this project\n  mcc project set work --allow lab\n  mcc project set default --path ~/code/site\n  mcc project list              Every project, its profile and allowed profiles\n  mcc project unset             Remove the project default that applies here\n\nA project is a directory: by default the top of the git repository you are\nin, else the current directory. It covers everything below it, and the\ndeepest project containing the current directory applies.\n\nWhen the project's profile is at its limit, `mcc run` asks whether to wait\n(the default) or start it anyway. It offers another profile only when the\nproject allows it with `--allow` (repeat it, or separate names with commas).\nRunning `set` again replaces the list: without `--allow`, it is cleared.\nProject defaults are kept in ~/.mcc/config.toml:\n\n  [projects.\"/Users/me/code/app\"]\n  profile = \"work\"\n  allow = [\"lab\"]\n\n`mcc rename` updates them; `mcc delete` removes the ones naming the profile\nand takes it off the allow lists."
    )]
    Project(ProjectArgs),

    /// Start Claude Code with the current project's profile
    #[command(after_help = concat!(
        "Examples:\n",
        "  mcc run                       Start Claude Code with this project's profile\n",
        "  mcc run -- --resume           Pass arguments to claude after `--`\n",
        "  mcc run --profile lab         Start `lab` here instead (usage isn't checked)\n",
        "  mcc run --dry-run             Print the profile that would start, and why\n",
        "\n",
        "The profile that starts is the project's default (`mcc project set`), or\n",
        "the default profile (~/.claude) outside any project. mcc never switches\n",
        "accounts on its own. When that profile is at its 5-hour or weekly limit,\n",
        "`mcc run` asks what to do. Waiting (starting nothing) is the default, and\n",
        "starting it anyway comes next. Other profiles are offered only when the\n",
        "project allows them (`mcc project set work --allow lab`), by name and\n",
        "without their usage. Without a terminal it stops instead (exit 2).\n",
        "\n",
        "Starting a profile other than the project's, from the prompt or with\n",
        "`--profile`, prints this note on stderr:\n",
        "\n",
        run_tos_note!(),
        "\n\n",
        "If the profile's usage can't be read (offline, expired login,\n",
        "MCC_NO_USAGE=1), it starts. Sessions are kept per profile, so `--resume` in\n",
        "another profile won't list this project's sessions unless both profiles\n",
        "share sessions (`mcc help share`)."
    ))]
    Run(RunArgs),

    /// Show the active profile in Claude Code's status line and cache its rate limits (opt-in)
    #[command(
        after_help = "Examples:\n  mcc statusline install         Wrap your statusLine in ~/.claude/settings.json\n  mcc statusline install --prefix\n                                 …and start the status line with the profile's name, e.g. [work]\n  mcc statusline uninstall       Put the previous statusLine back everywhere mcc installed it\n\n`install` and `uninstall` are the only mcc commands that change an existing\ndefault profile (~/.claude/settings.json; a symlinked file stays a symlink). It backs the file up to ~/.mcc/backups/ first,\nkeeps your statusLine command and sets `mcc statusline` in its place; running\nit again never wraps the shim twice. Profiles receive it with their next\n`mcc sync`; when [sync] exclude has \"settings\", install offers to add it to\neach profile directly (`--yes` accepts).\n\nClaude Code then runs `mcc statusline` on every status-line refresh, with the\nsession's JSON on stdin. The shim works out the profile from CLAUDE_CONFIG_DIR\n(unset means the default profile), saves the 5-hour and weekly rate limits in\nthat JSON to ~/.mcc/state/<profile>/usage.json, runs your previous statusLine\ncommand with the same stdin and prints its output. It never touches\ncredentials and never writes ~/.mcc/config.toml. Claude Code sends rate limits\nonly to Pro and Max subscribers, after the session's first response, so an idle\nprofile's file goes stale. A statusLine in a project's own .claude/settings.json\noverrides the shim there."
    )]
    Statusline(StatuslineArgs),

    /// Rewrite profiles' launchers and shell aliases from the current templates
    #[command(
        after_help = "Examples:\n  mcc refresh-launchers          Every profile\n  mcc refresh-launchers work lab Only these profiles\n\nNeeded once for profiles created by an older mcc, so that `<alias> --mcc-share`\nworks: the shell alias then runs the launcher (~/.local/bin/<alias>) instead of\nclaude directly. CLAUDE_CONFIG_DIR stays exactly the same, so logins are kept.\nmcc never rewrites existing launchers on its own. Launchers that are already\ncurrent are left alone, and files mcc didn't write are skipped."
    )]
    RefreshLaunchers(RefreshLaunchersArgs),

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

    /// Label the account, e.g. "personal" or the work organization (asked if omitted)
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,

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

    /// Output JSON (to stdout) instead of a table
    #[arg(long)]
    pub json: bool,

    /// Show account emails (overrides `[usage] show_email` in ~/.mcc/config.toml)
    #[arg(long, conflicts_with = "hide_email")]
    pub show_email: bool,

    /// Hide account emails (the default)
    #[arg(long)]
    pub hide_email: bool,
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
pub struct RenameArgs {
    /// Profile to rename
    pub alias: String,

    /// New name, which becomes the profile's command
    pub new_alias: String,
}

#[derive(Debug, Args)]
pub struct ShareArgs {
    /// Profile whose sessions to share
    pub alias: String,

    /// Stop sharing: give the profile its own copy of the shared sessions
    #[arg(long)]
    pub off: bool,

    /// Called by the launcher for `<alias> --mcc-share`: only the note and errors, on stderr
    #[arg(long, hide = true, conflicts_with = "off")]
    pub from_launch: bool,
}

#[derive(Debug, Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub action: ProjectAction,
}

#[derive(Debug, Subcommand)]
pub enum ProjectAction {
    /// Make PROFILE the one `mcc run` starts with in this project
    Set {
        /// Profile to start (`default` or `claude` for ~/.claude)
        profile: String,

        /// Project directory (default: the top of the current git repository, else the current directory)
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,

        /// Another profile `mcc run` may offer here when PROFILE is at its limit (repeatable or comma-separated; replaces the list)
        #[arg(long, value_name = "PROFILE", value_delimiter = ',')]
        allow: Vec<String>,
    },

    /// Remove a project default (default: the one that applies here)
    Unset {
        /// Project directory, as listed by `mcc project list`
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },

    /// List every project with its profile and allowed profiles
    #[command(visible_alias = "ls")]
    List {
        /// Output JSON (to stdout) instead of a table
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Start this profile instead (`default` or `claude` for ~/.claude); usage isn't checked
    #[arg(long, value_name = "PROFILE")]
    pub profile: Option<String>,

    /// Print the profile that would start (on stdout) and why, without starting it
    #[arg(long)]
    pub dry_run: bool,

    /// Arguments for claude; put them after `--` (e.g. `mcc run -- --resume`)
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "CLAUDE_ARGS"
    )]
    pub args: Vec<OsString>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct StatuslineArgs {
    #[command(subcommand)]
    pub action: Option<StatuslineAction>,

    /// Start the output with the active profile's name, e.g. `[work]` (set by `install --prefix`)
    #[arg(long)]
    pub prefix: bool,
}

#[derive(Debug, Subcommand)]
pub enum StatuslineAction {
    /// Set the shim as the default profile's statusLine, keeping the previous one
    Install {
        /// Start the status line with the active profile's name, e.g. `[work]`
        #[arg(long)]
        prefix: bool,

        /// Also add it to profiles that don't sync settings, without asking
        #[arg(short, long)]
        yes: bool,
    },

    /// Restore the previous statusLine wherever the shim is set
    Uninstall,
}

#[derive(Debug, Args)]
pub struct RefreshLaunchersArgs {
    /// Profiles to refresh (default: all)
    pub aliases: Vec<String>,
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
