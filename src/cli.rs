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
  mcc diff work                  Show how `work` differs from the default
  mcc rename work job            Rename profile `work` (and its command) to `job`
  mcc share work                 Share `work`'s sessions with your other sharing profiles
  mcc project set work           Start `work` in this project (git repo) with `mcc run`
  mcc run                        Start Claude Code with this project's profile
  eval \"$(mcc env)\"              Point CLAUDE_CONFIG_DIR at this project's profile
  mcc exec --all -- claude plugin marketplace update
  mcc statusline install         Show the profile in Claude Code's status line, cache rate limits
  mcc refresh-launchers          Update older launchers so `<alias> --mcc-share` works
  mcc doctor                     Check every profile's setup and print a fix for each problem
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
        after_help = "Examples:\n  mcc list\n  mcc list --long               Every metric: command, org, skills, plugins, size...\n  mcc list --show-email\n  mcc list --no-usage\n  mcc list --json | jq '.[] | {name, skills, plugins_installed}'\n  mcc list --json | jq '.[] | {name, five_hour: .usage.five_hour.percent}'\n  mcc list --json-v2 | jq '.profiles[].name'    Versioned: {\"schema\": 2, \"profiles\": [...]}\n\nThe table fits 120 columns: NAME, ACCOUNT (the profile's label, else its\norganization; with the email in front when emails are shown), 5H, WEEK,\nLAST USED and SYNC. `--long` has the rest: command, organization, counts\nof skills, agents, commands, plugins, MCP servers and hooks, size, and\nwhen usage was cached.\n\n5H and WEEK show each account's 5-hour and weekly usage (a bar, percent used\nand time until reset) as `mcc statusline` cached it; `!` marks 80% or more\nand `!!` 95% or more. Bars shrink to fit the terminal; under 80 columns\nthey are dropped when even the smallest does not fit. \"—\" when nothing\ncurrent is cached (run /usage in Claude Code); `[usage] live = true` looks\nusage up with each profile's login instead (`mcc list --long` says why it\nis unavailable). `mcc usage` shows every window in detail.\n\nAccount emails are hidden by default, as in `mcc usage` (`account` is null\nin --json and --json-v2, and so is an `organization` that contains an\nemail). --show-email shows them for one run; to always show them, set in\n~/.mcc/config.toml:\n\n  [privacy]\n  show_email = true"
    )]
    List(ListArgs),

    /// Show each profile's 5-hour and weekly usage in detail
    #[command(
        after_help = "Examples:\n  mcc usage                     Every profile, the default included\n  mcc usage work                Only the `work` profile\n  mcc usage default             Only the default profile (~/.claude)\n  mcc usage --show-email        Also show each profile's account email\n  mcc usage --json | jq '.[] | {name, weekly: .usage.weekly.percent}'\n  mcc usage --json-v2 | jq '.profiles[].name'   Versioned: {\"schema\": 2, \"profiles\": [...]}\n\nOne table row per profile: for the 5-hour and weekly windows, a bar of the\nlimit used with its percent, then the time until it resets (and the reset\ntime when it fits). `!` marks 80% or more and `!!` 95% or more. On a narrow\nterminal the reset time goes first, then bars shrink; under 80 columns bars\nare dropped when nothing fits. UPDATED is how old the numbers cached by\n`mcc statusline` are; a profile with nothing current cached shows \"—\"\n(run /usage in Claude Code), and one whose usage can't be read shows\n\"unavailable\" and why. MCC_NO_USAGE=1 skips usage entirely.\n\nAccount emails are hidden by default (also `null` in --json). To always\nshow them, or to look usage up live with each profile's login (off by\ndefault; a one-time note explains what it sends), set in ~/.mcc/config.toml:\n\n  [privacy]\n  show_email = true\n\n  [usage]\n  live = true\n\n`[privacy] show_email` applies to `mcc list` too; `[usage] show_email`, its\nold name, is still read."
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
        after_help = "Items: settings, claude-md, keybindings, statusline, agents, skills,\n       commands, hooks, output-styles, plugins, mcp\n\nEach sync records what it wrote in <profile>/.mcc-sync.json. The next sync\ncompares the default and the profile with it: a path changed only in the\ndefault is updated; one edited (or deleted) only in the profile is kept and\nreported as a local edit; one changed in both is a conflict, kept unless\n--force. Everything overwritten or removed is first backed up to\n<profile>/.mcc-backup/<timestamp>/. `mcc diff ALIAS` shows the differences.\nItems linked to the default with `mcc link` are left alone.\n\nExamples:\n  mcc sync work --dry-run\n  mcc sync work --only skills,agents\n  mcc sync work --force\n  mcc sync --all --prune"
    )]
    Sync(SyncArgs),

    /// Show how a profile differs from the default's setup, as `mcc sync` sees it
    #[command(
        after_help = "Each differing path (and MCP server) is listed as one of:\n  new in default       the next `mcc sync` adds it\n  changed in default   the next `mcc sync` updates it\n  local edit           edited (or deleted) in the profile; `mcc sync` keeps it\n  conflict             changed in both; `mcc sync` keeps it unless --force\n  only in profile      the default doesn't have it; kept unless --prune\nwith a line diff (default → profile) for text files. Nothing is written.\n\nExamples:\n  mcc diff work\n  mcc diff work --only skills,settings"
    )]
    Diff(DiffArgs),

    /// Rename a profile: its command, launcher, shell alias and registry entry
    #[command(
        visible_alias = "mv",
        after_help = "Examples:\n  mcc rename work job\n\nThe new name is checked like `mcc add` checks an alias: it must not be an\nexisting profile, command, alias, function or shell builtin. The profile's\ndirectory, login and setup stay as they are; only the command changes."
    )]
    Rename(RenameArgs),

    /// Share a profile's sessions with your other sharing profiles (off by default)
    #[command(after_help = concat!(
        "Examples:\n",
        "  mcc share work                Share `work`'s sessions with the other sharing profiles\n",
        "  mcc share work --group acme   …only with the profiles of group `acme`\n",
        "  mcc share work --off          Stop sharing; `work` keeps a copy of the shared sessions\n",
        "  work --mcc-share              Start `work`, turning sharing on first (like `mcc share work`)\n",
        "  work --mcc-share --resume     …then pick a session from any sharing profile\n",
        "\n",
        "Sharing links the profile's projects/ directory (the sessions `claude --resume`\n",
        "and /resume list) to the store of its group: ~/.mcc/shared/groups/<name>/projects\n",
        "with `--group <name>`, else ~/.mcc/shared/projects (the `default` group, the one\n",
        "store older mcc versions used). A profile shares in one group at a time; the\n",
        "group is kept as `share_group` and reused by the next `mcc share` and\n",
        "`--mcc-share`. To change it, run `mcc share <alias> --off` first.\n",
        "\n",
        "Joining a group with profiles of another work organization (by the account in\n",
        "the profile's .claude.json, else the one `mcc add` recorded) is refused unless\n",
        "`--allow-cross-org` is passed, and so is joining with an account mcc can't\n",
        "identify. Personal plans (Free, Pro, Max) have an organization of their own;\n",
        "its type tells them from Team and Enterprise, and an unknown type counts as a\n",
        "possible work organization. Organization ↔ personal (or an unknown account) is\n",
        "the strictest: `--allow-cross-org` must also be confirmed in a terminal, since\n",
        "work sessions resumed under a personal account fall under\n",
        "consumer terms. Profiles that already share are warned, never cut off.\n",
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

    /// Link read-mostly items (or share plugins) with the default profile instead of copying them
    #[command(after_help = concat!(
        "Items: claude-md, agents, skills, commands, output-styles, plugins\n",
        "\n",
        "Examples:\n",
        "  mcc link work                 Show which items `work` links\n",
        "  mcc link work skills,agents   Link `work`'s skills/ and agents/ to the default's\n",
        "  mcc link work skills --off    Give `work` its own copy of skills/ again\n",
        "  mcc add lab --clone --link skills   Clone, linking skills/ instead of copying it\n",
        "  mcc link work plugins         Use the default's plugins instead of a copy\n",
        "  mcc link work plugins --off   Give `work` its own copy of the plugins again\n",
        "\n",
        "A linked item is a symlink to the default profile's (~/.claude/skills), so\n",
        "every profile that links it sees the same files: nothing is duplicated and\n",
        "nothing drifts, and `mcc sync` (and `mcc diff`) leave it alone.\n",
        "\n",
        "Write-through: editing a linked item in the profile, by hand or by Claude Code\n",
        "(e.g. installing a skill), changes the default's real files, and so every other\n",
        "profile that links it. This note is printed each time link mode is turned on.\n",
        "\n",
        "Turning link mode on first backs up the profile's own copy (it may hold local\n",
        "edits) to <profile>/.mcc-backup/<timestamp>/. `--off` removes only the symlink,\n",
        "never the default's files it points at, and copies the default's item in its\n",
        "place. A failed step undoes the whole change. settings can't be linked: each\n",
        "profile's settings.json is merged with its own overlay.\n",
        "\n",
        "plugins is shared, not symlinked: the profile's launcher (and `mcc run`,\n",
        "`mcc exec`, `mcc env`) sets CLAUDE_CODE_PLUGIN_CACHE_DIR to ~/.claude/plugins,\n",
        "the plugins root Claude Code then uses instead of <profile>/plugins. That root\n",
        "holds installed_plugins.json, known_marketplaces.json, marketplaces/, cache/\n",
        "and each plugin's data/, so all of it is shared. Which plugins are enabled\n",
        "stays per profile (enabledPlugins in settings.json and its overlay), but\n",
        "installing, updating or uninstalling a plugin in one profile does it for\n",
        "every profile sharing plugins and the default. A profile started without\n",
        "the variable (e.g. `CLAUDE_CONFIG_DIR=... claude`) has no plugins. Linking\n",
        "moves the profile's own plugins/ to <profile>/.mcc-backup/<timestamp>/\n",
        "(remove it to free the space); `mcc add --clone` shares plugins unless\n",
        "--copy-plugins is given.\n",
        "\n",
        "The setting is kept per profile in ~/.mcc/config.toml (default: none linked):\n",
        "\n",
        "  [profiles.work]\n",
        "  link = [\"skills\", \"agents\"]  # set it with `mcc link`\n",
    ))]
    Link(LinkArgs),

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

    /// Print the shell code that sets CLAUDE_CONFIG_DIR for a profile (for eval or direnv)
    #[command(after_help = concat!(
        "Examples:\n",
        "  eval \"$(mcc env)\"             Use this project's profile in the current shell\n",
        "  mcc env work                  export CLAUDE_CONFIG_DIR='/Users/me/.mcc/profiles/work'\n",
        "  mcc env work --shell fish     set -gx CLAUDE_CONFIG_DIR '/Users/me/.mcc/profiles/work'\n",
        "  mcc env default               unset CLAUDE_CONFIG_DIR (the default profile, ~/.claude)\n",
        "  mcc env | source              The same as eval, in fish\n",
        "\n",
        "Without PROFILE it prints the profile `mcc run` would start here: the\n",
        "project's default (`mcc project set`), else the default profile. Usage\n",
        "isn't checked. The shell is --shell, else bash inside a direnv .envrc\n",
        "(direnv evaluates it with bash), else $SHELL; zsh and bash get the same\n",
        "POSIX line. With direnv, put this in the project's .envrc:\n",
        "\n",
        "  eval \"$(mcc env)\"\n",
        "\n",
        "Then `claude`, IDE extensions and scripts started in the project use its\n",
        "profile. Naming a profile other than the project's prints the note shown\n",
        "by `mcc help run` on stderr."
    ))]
    Env(EnvArgs),

    /// Run a command with one profile's CLAUDE_CONFIG_DIR, or once per profile
    #[command(after_help = concat!(
        "Examples:\n",
        "  mcc exec work -- claude mcp list\n",
        "  mcc exec default -- claude plugin list\n",
        "  mcc exec --all -- claude plugin marketplace update\n",
        "\n",
        "With PROFILE the command runs with that profile's CLAUDE_CONFIG_DIR (unset for\n",
        "the default profile), and mcc exits with its exit status. With --all it runs\n",
        "once per profile, the default first, one after the other. Each profile's output\n",
        "comes under its name, followed by its exit status. mcc then exits 0 if the\n",
        "command succeeded in every profile, else with the first failing exit status.\n",
        "A command that can't be found counts as exit 127 (126 when it can't be run).\n",
        "Ctrl+C stops at the current profile. mcc holds no lock while the command runs, so it may run mcc itself."
    ))]
    Exec(ExecArgs),

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

    /// Check every profile's setup and print a fix for each problem (read-only)
    #[command(after_help = concat!(
        "Examples:\n",
        "  mcc doctor                    Every profile, the default included\n",
        "  mcc doctor work               Only `work` (and the checks every profile shares)\n",
        "  mcc doctor --json | jq '.[] | select(.status == \"problem\")'\n",
        "  mcc doctor --json-v2 | jq '.checks[].fix'   Versioned: {\"schema\": 2, \"checks\": [...]}\n",
        "\n",
        "Checks: mcc is set up and claude is found; ~/.local/bin is on PATH;\n",
        "no rc file exports CLAUDE_CONFIG_DIR globally (plain `claude` would then\n",
        "stop starting the default profile). Per profile: its directory exists\n",
        "with mode 0700; its launcher is present and current; its alias block is\n",
        "in the rc file; `claude auth status` reports the profile's directory\n",
        "(configDirectory, Claude Code 2.1.268+) and a login; its session\n",
        "sharing link and leftovers of an interrupted `mcc share`; drift against\n",
        "the default; and the `mcc statusline` shim when it is installed.\n",
        "\n",
        "Each finding is ok, warn, problem or skipped; warnings and problems come\n",
        "with a fix. mcc exits 1 when there is a problem. Nothing is written: not\n",
        "~/.mcc/config.toml, not even on first use. Token expiry is checked only\n",
        "with `[usage] live = true`, the one setting that lets mcc read tokens."
    ))]
    Doctor(DoctorArgs),

    /// Read or change ~/.mcc/config.toml: get, set, edit, path
    #[command(after_help = concat!(
        "Examples:\n",
        "  mcc config get usage.live          Print a value (lists one item per line)\n",
        "  mcc config set usage.live true\n",
        "  mcc config set sync.exclude plugins,mcp\n",
        "  mcc config set sync.exclude \"\"     Exclude nothing\n",
        "  mcc config set profiles.work.label \"Acme (work)\"\n",
        "  mcc config edit                    Open the file in $VISUAL or $EDITOR\n",
        "  mcc config path                    Print the file's path\n",
        "\n",
        "Settable keys:\n",
        "  sync.exclude           sync items to skip, comma-separated (\"\" for none)\n",
        "  sync.extra             extra paths to sync, relative to the config dir\n",
        "  privacy.show_email     true or false (account emails in list and usage)\n",
        "  usage.live             true or false\n",
        "  profiles.ALIAS.label   1–64 characters (\"\" removes it)\n",
        "\n",
        "`get` reads any key, dotted as in TOML (projects.\"/Users/me/app\".profile).\n",
        "A value `set` rejects leaves the file unchanged, and comments are kept.\n",
        "Keys other commands manage say which one to use (`mcc share`, `mcc project`).\n",
        "\n",
        "`edit` opens a copy of the file and saves it only when it is valid mcc\n",
        "config; otherwise it offers to edit again, or leaves the file unchanged.\n",
        "mcc holds no lock while the editor is open, so other mcc commands keep\n",
        "working. If one of them changes the file meanwhile, nothing is saved and\n",
        "your edited copy is kept next to it."
    ))]
    Config(ConfigArgs),

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

    /// Link these items to the default's copy instead of copying them (see `mcc help link`)
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "ITEMS",
        conflicts_with = "fresh"
    )]
    pub link: Vec<String>,

    /// Give the clone its own copy of the default's plugins instead of sharing them (see `mcc help link`)
    #[arg(long, conflicts_with = "fresh")]
    pub copy_plugins: bool,

    /// Label the account, e.g. "personal" or the work organization (asked if omitted)
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,

    /// Accept defaults without prompting
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Output JSON (to stdout) instead of a table: a bare array (schema 1)
    #[arg(long, conflicts_with_all = ["long", "json_v2"])]
    pub json: bool,

    /// Output versioned JSON: {"schema": 2, "profiles": [...]}
    #[arg(long = "json-v2", conflicts_with = "long")]
    pub json_v2: bool,

    /// Show a detailed card per profile
    #[arg(short, long)]
    pub long: bool,

    /// Skip fetching 5-hour/weekly usage (no network; also MCC_NO_USAGE=1)
    #[arg(long)]
    pub no_usage: bool,

    /// Show account emails (overrides `[privacy] show_email` in ~/.mcc/config.toml)
    #[arg(long, conflicts_with = "hide_email")]
    pub show_email: bool,

    /// Hide account emails (the default)
    #[arg(long)]
    pub hide_email: bool,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Only check this profile (`default` or `claude` for ~/.claude)
    pub profile: Option<String>,

    /// Output JSON (to stdout): a bare array of checks (schema 1)
    #[arg(long, conflicts_with = "json_v2")]
    pub json: bool,

    /// Output versioned JSON: {"schema": 2, "checks": [...]}
    #[arg(long = "json-v2")]
    pub json_v2: bool,
}

#[derive(Debug, Args)]
pub struct UsageArgs {
    /// Only this profile (`default` or `claude` for ~/.claude)
    pub profile: Option<String>,

    /// Output JSON (to stdout) instead of a table: a bare array (schema 1)
    #[arg(long, conflicts_with = "json_v2")]
    pub json: bool,

    /// Output versioned JSON: {"schema": 2, "profiles": [...]}
    #[arg(long = "json-v2")]
    pub json_v2: bool,

    /// Show account emails (overrides `[privacy] show_email` in ~/.mcc/config.toml)
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

    /// Share only with the profiles of this group, through a store of its
    /// own (default: the profile's current group, else `default`)
    #[arg(long, value_name = "NAME", conflicts_with = "off")]
    pub group: Option<String>,

    /// Allow sharing with profiles of another organization (or whose
    /// organization is unknown); organization ↔ personal also asks to confirm
    #[arg(long, conflicts_with_all = ["off", "from_launch"])]
    pub allow_cross_org: bool,

    /// Called by the launcher for `<alias> --mcc-share`: only the note and errors, on stderr
    #[arg(long, hide = true, conflicts_with = "off")]
    pub from_launch: bool,
}

#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Profile whose items to link
    pub alias: String,

    /// Items to link (comma- or space-separated); none shows each item's mode
    #[arg(value_delimiter = ',', value_name = "ITEMS")]
    pub items: Vec<String>,

    /// Stop linking: give the profile its own copy of the items again
    #[arg(long)]
    pub off: bool,
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
        /// Output JSON (to stdout) instead of a table: a bare array (schema 1)
        #[arg(long, conflicts_with = "json_v2")]
        json: bool,

        /// Output versioned JSON: {"schema": 2, "projects": [...]}
        #[arg(long = "json-v2")]
        json_v2: bool,
    },
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print a value (dotted key, e.g. usage.live or profiles.work.label)
    Get {
        /// The key, dotted as in TOML
        key: String,
    },

    /// Set a value, checking it first (see `mcc help config` for the keys)
    Set {
        /// The key, e.g. usage.live, sync.exclude or profiles.work.label
        key: String,

        /// The value: true/false, a comma-separated list, or text; "" clears a list or label
        #[arg(allow_hyphen_values = true)]
        value: String,
    },

    /// Edit the file in $VISUAL or $EDITOR; saved only when it is valid
    Edit,

    /// Print the path of the config file
    Path,
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
pub struct EnvArgs {
    /// Profile to print (`default` or `claude` for ~/.claude; default: the one `mcc run` would start here)
    pub profile: Option<String>,

    /// Print for this shell (default: bash inside a direnv .envrc, else detected from $SHELL)
    #[arg(long, value_enum)]
    pub shell: Option<Shell>,
}

#[derive(Debug, Args)]
pub struct ExecArgs {
    /// Profile to run the command as (`default` or `claude` for ~/.claude)
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub profile: Option<String>,

    /// Run the command once per profile, the default first
    #[arg(long)]
    pub all: bool,

    /// The command and its arguments, after `--`
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<OsString>,
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

    /// Also overwrite conflicts: paths changed in the default and edited in the profile
    #[arg(long)]
    pub force: bool,

    /// Only sync these items (comma-separated)
    #[arg(long, value_delimiter = ',', value_name = "ITEMS")]
    pub only: Option<Vec<String>>,

    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Profile to compare with the default
    pub alias: String,

    /// Only compare these items (comma-separated)
    #[arg(long, value_delimiter = ',', value_name = "ITEMS")]
    pub only: Option<Vec<String>>,
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
