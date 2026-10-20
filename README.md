# mcc — multiple Claude Code profiles

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

Run several Claude Code accounts side by side, each with its own login, its own command, and the same setup as your main one.

Output captured from a sandbox run (small demo setup; `claude` login shown with a test account):

```console
$ mcc add work --clone
◆ Add Claude Code profile "work"
  [1/8] Validate alias
        ✓ done — "work" is available (1ms)
  [2/8] Create ~/.mcc/profiles/work
        ✓ done (0ms)
  [3/8] Copy setup from the default profile
        ✓ done — 7 items (settings 1, claude-md 1, agents 2, skills 3), 1 MCP servers; no credentials copied (4ms)
  [4/8] Install launcher ~/.local/bin/work
        ✓ done (0ms)
  [5/8] Add shell alias
        ✓ done — ~/.zshrc (0ms)
  [6/8] Register profile
        ✓ done — ~/.mcc/config.toml (0ms)
  [7/8] Log in (complete the sign-in in your browser)
Login successful.
        ✓ done (12ms)
  [8/8] Verify login
        ✓ done — fake@example.com (23ms)

  Result: profile "work" is ready — start it with `work`
    Command:           work
    Directory:         ~/.mcc/profiles/work
    Setup:             cloned from default (sync later with `mcc sync`)
    Launcher:          ~/.local/bin/work
    Shell alias:       ~/.zshrc

$ work          # Claude Code with the "work" account
$ claude        # Claude Code with your default account, unchanged
```

Claude Code reads its config from `CLAUDE_CONFIG_DIR`, which defaults to `~/.claude`. mcc gives each extra account its own directory and a command that sets the variable for you. It also keeps these profiles in sync with your default setup.

## Features

- **First-run detection.** mcc detects your OS, terminal, shell (zsh, bash, fish) and the right rc file, so new commands work in your shell.
- **`add`.** If `~/.claude` doesn't exist yet, `add` creates your default profile. Otherwise it asks for an alias and checks that the name isn't already taken by a command, alias, function, shell builtin, or existing profile. You then either **clone** the default's setup (without credentials) or start **fresh**, and the new profile's login opens.
- **`list`.** One row per profile: command, account, 5-hour and weekly usage (with a usage bar), skills, agents, commands, plugins (enabled/installed), MCP servers, hooks, size, last used, last sync. Profiles that share sessions are marked `(shared)`, and a hint names `mcc refresh-launchers` while a launcher is out of date. A second table lists your projects and the profile (and account) `mcc run` starts in each. `--long` and `--json` give more detail.
- **`usage`.** Every profile's 5-hour and weekly limits in one compact table: a usage bar with the percent used, time until reset and the reset time. Near-limit accounts stand out with `!` / `!!` markers and colour. Account emails are hidden unless you ask for them. `--json` for scripts.
- **`rename`.** Gives a profile a new name, and with it a new command. The new name is checked like an alias in `add`. The profile's directory, login and setup stay as they are.
- **`sync`.** A one-way copy of setup from the default profile into a profile, with `--dry-run`, `--prune`, `--only`, and `--all`.
- **Opt-in session sharing.** Off by default. `mcc share work` (or starting a profile with `work --mcc-share`) lets that profile's `--resume` see the sessions of every other sharing profile. Credentials and account files are never shared.
- **Per-project profiles.** `mcc project set work` makes `work` the profile `mcc run` starts in this repository, so a project stays on one account. mcc never switches accounts on its own: if that account is at its 5-hour or weekly limit, `mcc run` asks whether to wait (the default) or start it anyway, and offers another profile only when the project allows it (`--allow`).
- **`delete`.** Logs the profile out (clearing its keychain entry), then removes its directory, launcher, shell alias, and registry entry. It refuses the old name of a renamed profile, so that profile's files are never removed by mistake.
- **Clear progress everywhere.** Steps are numbered with a status and timing. Every run is logged to `~/.mcc/logs/mcc.log`.
- **Safe by design.** Changes are rolled back if `add` fails. Your rc file is backed up before mcc edits it, and `sync` backs up every file it overwrites or removes. mcc only removes what it created.

Supports **macOS and Linux**.

## Install

You need [Claude Code](https://docs.claude.com/en/docs/claude-code).

### Homebrew (macOS and Linux)

```sh
brew install luongnv89/tap/mcc
```

Homebrew builds mcc from source with its own Rust, so you don't need Rust installed. It also installs bash, zsh and fish completions. Update with `brew upgrade mcc`.

### From source

You need Rust (`cargo`).

```sh
# one-liner
curl -fsSL https://raw.githubusercontent.com/luongnv89/mcc/main/install.sh | sh

# or from a clone
git clone https://github.com/luongnv89/mcc && cd mcc && ./install.sh

# or directly with cargo
cargo install --git https://github.com/luongnv89/mcc --locked
```

## Quick start

```sh
mcc init                    # detect your environment (also runs automatically the first time)
mcc add work --clone        # new profile with your default setup → then log in
work                        # start Claude Code as the "work" account
mcc list                    # see all profiles
mcc usage                   # how much of each account's limits is used
mcc sync work               # later: pull new skills/plugins/settings from the default
mcc rename work job         # rename the profile: its command becomes `job`
mcc project set job         # in a repository: `mcc run` starts `job` there
mcc run                     # start Claude Code with this project's profile
mcc delete job              # remove it completely
```

## Commands

| Command | What it does | Key options |
|---|---|---|
| `mcc init` | Detect OS, arch, terminal, shell, rc file, the `claude` binary, and whether `~/.local/bin` is on PATH (and offer to add it) | `--force`, `--shell zsh\|bash\|fish` |
| `mcc add [ALIAS]` | Create the default profile, or a new profile with its own command | `--clone`, `--fresh`, `--no-login`, `-y` |
| `mcc list` (`ls`) | Show every profile with metrics and 5-hour/weekly usage | `--json`, `-l/--long`, `--no-usage` |
| `mcc usage [PROFILE]` | Show each profile's 5-hour and weekly usage in a table (`default` or `claude` for the default profile) | `--json`, `--show-email`, `--hide-email` |
| `mcc sync [ALIAS]` | Copy the default's setup into a profile (default → profile only) | `--all`, `--dry-run`, `--prune`, `--only ITEMS`, `-y` |
| `mcc rename ALIAS NEW` (`mv`) | Rename a profile and its command; directory and login are kept | |
| `mcc share ALIAS` | Share the profile's sessions with your other sharing profiles (off by default; see [Sharing sessions](#sharing-sessions-between-profiles)) | `--off` |
| `mcc project set PROFILE` | Make PROFILE the one `mcc run` starts in this project (see [Per-project profiles](#per-project-profiles-mcc-project-and-mcc-run)); also `unset` and `list` | `--path DIR`, `--allow PROFILE`, `list --json` |
| `mcc run [-- CLAUDE_ARGS]` | Start Claude Code with the current project's profile; at its limit, ask whether to wait (the default) or start it anyway | `--profile NAME`, `--dry-run` |
| `mcc refresh-launchers [ALIAS...]` | Rewrite launchers and shell aliases from the current templates (all profiles when none are given); needed once so `<alias> --mcc-share` works for profiles made by an older mcc | |
| `mcc delete ALIAS` (`rm`) | Log out and remove a profile | `-y/--yes`, `--keep-auth` |
| `mcc completions SHELL` | Print a completion script | `bash`, `zsh`, `fish`, `elvish`, `powershell` |
| `mcc help [COMMAND]` | Usage and examples | |

Global flags: `-v/--verbose`, `-q/--quiet`, `--no-color` (mcc also honors `NO_COLOR`), `--version`, `-h/--help`.

### Examples

```sh
mcc add                               # interactive: asks for the alias and clone/fresh
mcc add lab --fresh --no-login        # empty profile; log in later by running `lab`
mcc list --long                       # one detailed card per profile
mcc list --json | jq '.[] | {name, account, skills, plugins_installed}'
mcc list --json | jq '.[] | {name, five_hour: .usage.five_hour.percent, weekly: .usage.weekly.percent}'
mcc list --json | jq '.[] | {name, project_defaults}'   # the projects each profile starts in
mcc list --no-usage                   # offline: skip the usage lookup
mcc usage                             # every profile's limits, the default included
mcc usage work                        # only the `work` profile
mcc usage --show-email                # with each profile's account email
mcc usage --json | jq '.[] | {name, weekly: .usage.weekly.percent}'
mcc sync work --dry-run               # preview what would change
mcc sync work --only skills,agents    # only some items
mcc sync --all --prune --yes          # make every profile mirror the default exactly
mcc rename lab sandbox                # new command `sandbox`, same directory and login
mcc share work                        # share work's sessions with the other sharing profiles
work --mcc-share --resume             # the same from the launcher, then pick any shared session
mcc share work --off                  # stop sharing; work keeps a copy of the sessions
mcc project set work                  # `mcc run` starts `work` in this git repository
mcc project set work --allow lab      # …and may offer `lab` when `work` is at its limit
mcc project set default --path ~/site # …and the default profile in ~/site
mcc project list                      # every project, its profile and allowed profiles
mcc run -- --resume                   # start this project's profile; arguments after `--` go to claude
mcc run --profile lab                 # start `lab` here this time; the project's default stays
mcc run --dry-run                     # print the profile `mcc run` would start, and why
mcc refresh-launchers                 # update launchers made by an older mcc
mcc delete sandbox --yes
```

### Shell completions

Homebrew installs completions for you. Otherwise, write the script for your shell:

```sh
mcc completions zsh  > ~/.zfunc/_mcc        # with fpath+=~/.zfunc in ~/.zshrc
mcc completions bash > ~/.local/share/bash-completion/completions/mcc
mcc completions fish > ~/.config/fish/completions/mcc.fish
```

## How it works

| What | Where |
|---|---|
| Default profile | `~/.claude` + `~/.claude.json` (command: `claude`) |
| mcc profiles | `~/.mcc/profiles/<alias>/` (named after the alias the profile was created with; `mcc rename` keeps the directory), used as `CLAUDE_CONFIG_DIR` |
| Launcher | `~/.local/bin/<alias>` (works in any shell as soon as it's written; sets `CLAUDE_CONFIG_DIR` and starts `claude`, and handles `--mcc-share`) |
| Shell alias | a managed block in your rc file, between `# >>> mcc:<alias> >>>` and `# <<< mcc:<alias> <<<`, that runs the launcher |
| Shared sessions | `~/.mcc/shared/projects`, only for profiles that opted in with `mcc share` (their `projects/` links there) |
| Project defaults | `[projects."<dir>"]` tables in `~/.mcc/config.toml`, written by `mcc project set` |
| mcc state | `~/.mcc/config.toml` (environment + registry), `~/.mcc/logs/mcc.log`, `~/.mcc/backups/` (rc file backups, session-merge conflicts) |

### What clone and sync copy

**Copied** from the default profile:

| Item (`--only` name) | Paths |
|---|---|
| `settings` | `settings.json`, `settings.local.json` |
| `claude-md` | `CLAUDE.md` |
| `keybindings` | `keybindings.json` |
| `statusline` | `statusline.conf` |
| `agents`, `skills`, `commands`, `hooks`, `output-styles` | those directories |
| `plugins` | `plugins/installed_plugins.json`, `plugins/known_marketplaces.json`, `plugins/marketplaces/`, `plugins/cache/` |
| `mcp` | the `mcpServers` entry from `~/.claude.json` (merged; the rest of the profile's state is kept) |

**Never copied:** credentials, account identity (`oauthAccount`, `userID`), history, projects/sessions, todos, telemetry, caches, file history. (Sessions can be *shared* instead, opt-in, with `mcc share`; see below.)

**Paths are rewritten** so each profile uses its own copies. In settings, plugin manifests, and MCP server entries, any `~/.claude/…`, `$HOME/.claude/…`, or `/Users/you/.claude/…` path that points at a *copied* item (for example `~/.claude/hooks/notify.sh`) is rewritten to the profile directory. Paths to things that aren't copied, such as `~/.claude/statusline/run.sh`, keep pointing at the default profile, so nothing dangles. Symlinks stay symlinks: a link into the default profile is re-pointed into the copy, and a relative link elsewhere (such as `skills/x -> ../../.agents/skills/x`) is resolved so it still works from the new location.

**`sync` is one-way** (default → profile), because most tools assume the default config path. Before writing, `sync` copies every file or folder it is about to overwrite or remove (such as `CLAUDE.md`, skills, agents, `settings.json`, and `.claude.json`) to `<profile>/.mcc-backup/<timestamp>/`, at the same paths, so your own edits can be restored from there. `--dry-run` changes nothing in the profile and makes no backup. Files that exist only in the profile are kept unless you pass `--prune`, and pruning asks for confirmation (or needs `--yes`).

### Customizing sync

In `~/.mcc/config.toml`:

```toml
[sync]
exclude = ["plugins"]          # built-in items to skip
extra = ["my-tool/config.yml"] # additional paths (relative to the config dir) to copy
```

### Showing account emails in `mcc usage`

Also in `~/.mcc/config.toml` (emails are hidden by default):

```toml
[usage]
show_email = true
```

### Sharing sessions between profiles

Each profile keeps its sessions to itself unless you turn sharing on. Sharing is a per-profile setting and is **off by default**. Once two or more profiles share, `claude --resume` and `/resume` in any of them list the sessions of all of them.

```sh
mcc share work               # turn sharing on for `work`
work --mcc-share             # the same, while starting `work` (the flag must come first)
mcc share work --off         # turn it off again
```

The setting is stored per profile in `~/.mcc/config.toml`:

```toml
[profiles.work]
share_sessions = true  # default: false; set it with `mcc share`
```

Editing the key by hand does nothing on its own. `mcc share <alias>` does the actual work, and it is the only supported way to change the setting:

- **What is shared:** the profile's `projects/` directory, where Claude Code keeps the session transcripts `--resume` lists. It is replaced with a link to `~/.mcc/shared/projects`. The sessions the profile already had are moved into that store without overwriting anything. If a file of the same name with different contents is already there, the store's copy is kept and the profile's copy goes to `~/.mcc/backups/shared-projects-<alias>-<time>/`, with a warning.
- **What is never shared:** credentials (`.credentials.json`, the keychain login), the account (`.claude.json`), `policy-limits.json`, `remote-settings.json`, prompt history (`history.jsonl`), live session state (`sessions/`), `shell-snapshots/`, `file-history/` and `todos/`. Each profile keeps its own login and `CLAUDE_CONFIG_DIR`.
- **Every time sharing is turned on**, by `mcc share` or by `--mcc-share`, mcc prints this note on stderr:

  > ⚠ Session sharing makes conversation history available across your Claude Code accounts. Use it only between accounts you own. Don't use it to get around usage limits, and never let other people use your account or credentials — doing so can violate Anthropic's Consumer Terms (https://www.anthropic.com/legal/consumer-terms) and Usage Policy (https://www.anthropic.com/legal/aup), including the updated Usage Policy that takes effect November 12, 2026.

- **`--off`** replaces the link with a directory of the profile's own, holding a copy of the shared sessions. The store stays for the other sharing profiles; mcc never deletes it, so remove `~/.mcc/shared` by hand once no profile shares.
- **`mcc delete`** of a sharing profile removes only its link. The sessions stay in the store.
- **`mcc rename`** keeps sharing as it is.
- **The default profile** (`~/.claude`) can't share.
- **Caveats.** In `mcc list`, sharing profiles are marked `(shared)` (`"share_sessions": true` in `mcc list --json`), and their session counts include every sharing profile's sessions. Claude Code removes old transcripts after `cleanupPeriodDays` (30 days unless set in `settings.json`): with sharing, the shortest setting among the sharing profiles applies to all of them.

**Upgrading from an older mcc:** existing launchers and shell aliases are left exactly as they were. `<alias> --mcc-share` needs the current launcher, and an alias that runs it. To get both, run `mcc refresh-launchers` once (or `mcc refresh-launchers work` for a single profile). `CLAUDE_CONFIG_DIR` stays the same, so logins are kept. `mcc share <alias>` works without it, and `mcc list` reminds you while a launcher is out of date (`"launcher_current": false` in `mcc list --json`).

### Per-project profiles: `mcc project` and `mcc run`

To keep a project on one account, give it a default profile and start Claude Code there with `mcc run` instead of `claude` or the profile's command:

```sh
cd ~/code/app
mcc project set work     # `mcc run` starts `work` anywhere in this repository
mcc run                  # starts Claude Code as `work`
mcc run -- --resume      # arguments after `--` go to claude
```

A project is a directory. `mcc project set` uses the top of the git repository you are in (or the current directory outside one; `--path DIR` picks another). It covers everything below it, and when projects are nested, the deepest one containing the current directory applies. Outside every project, `mcc run` treats the default profile (`~/.claude`) as the default.

`mcc run` starts the project's default. **It never switches to another account on its own.** When the project's profile is at its 5-hour or weekly limit (100% used), it shows when the limit resets and asks what to do. Waiting is preselected: it starts nothing and exits. Starting the profile anyway comes next. Other profiles are offered only when the project allows them, by name and without their usage:

```console
$ mcc project set work --allow lab
$ mcc run
  ⚠ `work` (the default for ~/code/app) is at its 5-hour limit, resets in 35m
? Wait, or start a profile? ›
❯ Wait — `work` resets in 35m
  Start `work` anyway
  Start `lab` instead
```

- **Allowed profiles:** `mcc project set work --allow lab` lets the prompt offer `lab` (repeat `--allow`, or separate names with commas). Running `mcc project set` again without `--allow` clears the list. Picking a profile other than the project's prints this note on stderr:

  > ⚠ You chose to start an account other than this project's. Use only accounts you own, and don't use several accounts to get around usage limits — doing so can violate Anthropic's Consumer Terms (https://www.anthropic.com/legal/consumer-terms) and Usage Policy (https://www.anthropic.com/legal/aup), including the updated Usage Policy that takes effect November 12, 2026.

- **`mcc run --profile NAME`** starts NAME without checking usage (`default` or `claude` for `~/.claude`). It is a one-off choice: the project's default doesn't change. Inside a project, a profile other than the project's prints the same note.
- **Without a terminal**, `mcc run` can't ask. At the limit it stops with exit code 2 and names `mcc run --profile <profile>` to start it anyway.
- **Unknown usage:** only the project profile's usage is ever fetched. If it can't be read (offline, an expired login, `MCC_NO_USAGE=1`), that profile starts. Unknown usage never counts as being at the limit.
- **`mcc run --dry-run`** prints the project's profile on stdout and why on stderr (at its limit, that `mcc run` would ask and wait by default), and starts nothing.
- **Sessions:** Claude Code keeps sessions per profile. If you start another profile, `--resume` and `-c` there only see that profile's sessions, unless both profiles [share sessions](#sharing-sessions-between-profiles).
- **Why there's no automatic switching:** Anthropic's seat rules are worded differently for each plan, and both rule it out:
  - [Team](https://support.claude.com/en/articles/12004354-purchase-and-manage-seats-on-team-plans): "Users can't route their work through another user's account to use that user's remaining usage."
  - [Enterprise](https://support.claude.com/en/articles/13393991-purchase-and-manage-seats-on-enterprise-plans): "Users can't use another user's account to get around their own spend limits or usage limits."

  Its [Consumer Terms](https://www.anthropic.com/legal/consumer-terms) also forbid bypassing its systems or protective measures, and its [Usage Policy](https://www.anthropic.com/legal/aup), updated with effect from November 12, 2026, applies to every account. So which account to use when one is at its limit is always your call.
- **`mcc list`** shows your projects under the profile table, with the profile and account each one starts with (and `← here` for the current directory). `mcc list --long` adds a `Default for` row to each profile's card, and `mcc list --json` has a `project_defaults` array per profile.
- `mcc project list` (`--json`) shows every project with its profile and allowed profiles (an `allow` array in the JSON), and `mcc project unset` removes the one that applies here (or the one given with `--path`).
- `mcc rename` updates the project defaults and allow lists that name the profile. `mcc delete` removes the project defaults naming it, and takes it off the allow lists.
- The usage lookup is the same as for `mcc usage`, described below.

Project defaults live in `~/.mcc/config.toml`:

```toml
[projects."/Users/me/code/app"]
profile = "work"  # an mcc profile, or "default" for ~/.claude
allow = ["lab"]   # optional: other profiles `mcc run` may offer at the limit
```

### Usage limits: `mcc list` and `mcc usage`

The `5H` and `WEEK` columns of `mcc list` show how much of each account's 5-hour and weekly limit is used, and how long until it resets, for every profile including the default:

```console
NAME       COMMAND  ACCOUNT         5H                     WEEK                   SKILLS  …
default ★  claude   me@example.com  ████░░░░░░ 42% (2h)    ██░░░░░░░░ 18% (3d)         2  …
work       work     work@corp.com   █████████░ 85%! (35m)  ██████████ 96%!! (3d)       2  …
lab        lab      —               unavailable            unavailable                 0  …
```

`mcc list --long` spells it out (`5h usage: ████████░░░░░░░░░░░░ 42% (resets in 2h 14m)`), or says why it is unavailable (`Usage: unavailable (not logged in)`).

`mcc usage` shows only usage, as a compact table with one row per profile: for each window a bar with the percent used, the time until it resets, and the reset time in your local time zone. `mcc usage work` shows one profile (`mcc usage default` for `~/.claude`); an unknown name is a usage error (exit 2). Account emails are hidden by default, so the output is safe to share on screen; `--show-email` adds an `ACCOUNT` column for one run, and `show_email = true` under `[usage]` in `~/.mcc/config.toml` shows it every time (`--hide-email` then hides it again).

```console
$ mcc usage
PROFILE    5-HOUR           RESETS IN                  WEEKLY            RESETS IN
default ★  ████░░░░░░ 42%   2h 14m (Sat Oct 10 16:00)  ██░░░░░░░░ 18%    3d 4h (Wed Oct 14 00:00)
lab        unavailable (not logged in)
work       █████████░ 85%!  35m (Sat Oct 10 14:21)     ██████████ 96%!!  3d 4h (Wed Oct 14 00:00)
```

- **Reading the bars.** Each bar is the share of the limit used. `!` after the percent marks 80% or more and `!!` 95% or more. In a colour terminal the bar is also green, yellow (80%+) or red (95%+), but the markers carry the same meaning, so nothing depends on colour alone (`--no-color`, `NO_COLOR`, or piped output).
- **Narrow terminals.** When the output would not fit the terminal's width, bars shrink (`mcc usage` first drops the absolute reset time, keeping the time until the reset, then shrinks its bar in the same 10/5/0 steps as the `mcc list` table). From 80 columns up a compact bar always stays, even if the `mcc list` table has to wrap; below 80 columns bars disappear (leaving just the percent, such as `42%`) when nothing fits. Set `COLUMNS` to override the detected width. Piped output gets full-size bars unless `COLUMNS` is set, and `--json` never has bars or colour.
- **Where it comes from.** mcc reads each profile's own Claude Code login (`<config dir>/.credentials.json`, or on macOS the login Keychain item Claude Code created for that config dir) and asks Anthropic's usage endpoint (`https://api.anthropic.com/api/oauth/usage`, the one behind Claude Code's `/usage` screen). The endpoint is **undocumented** and may change; when it does, mcc shows "unavailable" rather than failing.
- **Never fatal.** If usage can't be determined (logged out, expired login, offline, API error), the profile shows `unavailable` with the reason (in `mcc usage` and `mcc list --long`), the command still exits 0, and no warning is printed. Requests run in parallel with short timeouts (2 s to connect, 4 s total).
- **Tokens stay private.** The access token is only sent to that endpoint, for its own profile. It is never printed, logged, or written anywhere. mcc never refreshes tokens: if a login has expired, start the profile in Claude Code once (`work`, `claude`, …) and it refreshes itself.
- **Turn it off** with `mcc list --no-usage` or `MCC_NO_USAGE=1` (which also applies to `mcc usage`); usage then shows `—` and nothing touches the network or the keychain.

In `mcc list --json`, every profile has a `usage` object. `mcc usage --json` prints one object per profile with `name`, `command`, `is_default`, `account` (`null` when logged out, or when emails are hidden, as they are by default since v0.2.0; scripts that read `account` need `--show-email` or `[usage] show_email = true`) and the same `usage` object:

```json
"usage": {
  "status": "ok",
  "reason": null,
  "five_hour": { "percent": 42.0, "resets_at": "2026-10-10T16:00:00Z" },
  "weekly":    { "percent": 18.0, "resets_at": "2026-10-14T00:00:00Z" }
}
```

`status` is `ok` or `unavailable`. `five_hour` and `weekly` are `null` when there is no data (`resets_at` can be `null` too). When `status` is `unavailable`, `reason` is one of:

| `reason` | Meaning |
|---|---|
| `not_logged_in` | no Claude Code OAuth login found for the profile |
| `token_expired` | the stored login has expired, or the API rejected it (HTTP 401); start the profile to refresh it |
| `unauthorized` | the API refused the token (HTTP 403), e.g. a login without the profile scope |
| `rate_limited` | HTTP 429 |
| `network` | offline, timeout, or connection failure |
| `http_error` | any other HTTP status |
| `parse_error` | the response wasn't the expected JSON |
| `disabled` | not fetched (`--no-usage` / `MCC_NO_USAGE`) |

### Alias validation

An alias must be 1–32 characters: lowercase letters, digits, `-` or `_`, starting with a letter. It is rejected if any of these already uses the name:

- mcc's reserved names: `claude`, `mcc`, `default`, `help`, `all`, `path`, `share`, `refresh-launchers`, `project`, `run`
- shell keywords and builtins
- existing mcc profiles, and the old name of a renamed profile (its directory still uses that name; the error names the profile)
- executables on `$PATH`
- aliases and functions in your rc files (`.zshrc`, `.bashrc`, `.bash_profile`, `.profile`, fish config, …)
- anything your interactive shell resolves with `type <name>`, which catches plugin-defined aliases

## Environment variables

| Variable | Effect |
|---|---|
| `MCC_HOME` | Use this directory instead of `$HOME` for everything mcc reads and writes (great for testing) |
| `MCC_CLAUDE_BIN` | Path to the `claude` binary |
| `MCC_NON_INTERACTIVE` | Set to any value (even empty) to never prompt, as when stdin or stderr isn't a terminal: required prompts stop and name the flag to pass instead, and optional ones take their default (useful in scripts and CI) |
| `MCC_NO_USAGE` | Any value except empty or `0`: same as `mcc list --no-usage`; `mcc usage` shows usage as not fetched, and `mcc run` starts the project's profile without checking its usage |
| `COLUMNS` | Terminal width used to size the usage bars in `mcc list` and `mcc usage` (default: the detected width; unlimited when piped) |
| `MCC_USAGE_URL` | Test/debug override for the usage endpoint. It receives bearer tokens, so only `https://` URLs and `http://` on loopback (`127.0.0.1`, `localhost`, `[::1]`) are accepted; any other value is refused and usage is not fetched at all |
| `MCC_SECURITY_BIN` | Test/debug override for the macOS `security` binary used to read Keychain logins (default `/usr/bin/security`, macOS only) |
| `NO_COLOR` | Disable colors |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Runtime error (I/O failure, `claude` missing, …) |
| 2 | Usage error (bad arguments, invalid or taken alias, confirmation needed without a terminal) |
| 3 | Invalid input (corrupt `config.toml` or JSON) |
| 130 | Interrupted (Ctrl+C) |

## Uninstall

```sh
for p in $(mcc list --json | jq -r '.[] | select(.is_default | not) | .name'); do mcc delete "$p" --yes; done
brew uninstall mcc    # if you installed with Homebrew
cargo uninstall mcc   # if you installed from source
rm -rf ~/.mcc
```

## Development

```sh
cargo test       # unit + integration tests; all run in a temp HOME with a fake `claude`
cargo clippy --all-targets
```

The tests never touch your real `~/.claude`, rc files, or keychain, and never reach the network. The integration suite sets `MCC_HOME` to a temp dir, puts a fake `claude` script on `PATH`, points `MCC_SECURITY_BIN` at a fake `security` script, and points `MCC_USAGE_URL` at a refused loopback port or a local stub server.

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, checks, and pull request process, and follow the [Code of Conduct](CODE_OF_CONDUCT.md). Please report security vulnerabilities privately as described in [SECURITY.md](SECURITY.md), not in a public issue.

## License

mcc is licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in mcc by you, as defined in the Apache-2.0 license, shall be licensed as above, without any additional terms or conditions.
