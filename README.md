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
- **`list`.** One row per profile: command, account, 5-hour and weekly usage (with a usage bar), skills, agents, commands, plugins (enabled/installed), MCP servers, hooks, size, last used, last sync, and a `SYNC` column saying how the profile compares with the default (`✓`, `3 behind`, `2 local edits`, `1 conflict`). Profiles that share sessions are marked `(shared)`, and a hint names `mcc refresh-launchers` while a launcher is out of date. A second table lists your projects and the profile (and account) `mcc run` starts in each. `--long` and `--json` give more detail.
- **`usage`.** Every profile's 5-hour and weekly limits in one compact table: a usage bar with the percent used, time until reset and the reset time. Near-limit accounts stand out with `!` / `!!` markers and colour. Account emails are hidden unless you ask for them. `--json` for scripts.
- **`rename`.** Gives a profile a new name, and with it a new command. The new name is checked like an alias in `add`. The profile's directory, login and setup stay as they are.
- **`sync`.** A one-way copy of setup from the default profile into a profile, with `--dry-run`, `--prune`, `--only`, and `--all`.
- **Opt-in session sharing.** Off by default. `mcc share work` (or starting a profile with `work --mcc-share`) lets that profile's `--resume` see the sessions of every other sharing profile. Credentials and account files are never shared.
- **Per-project profiles.** `mcc project set work` makes `work` the profile `mcc run` starts in this repository, so a project stays on one account. mcc never switches accounts on its own: if that account is at its 5-hour or weekly limit, `mcc run` asks whether to wait (the default) or start it anyway, and offers another profile only when the project allows it (`--allow`).
- **`delete`.** Logs the profile out (clearing its keychain entry), then removes its directory, launcher, shell alias, and registry entry. It refuses the old name of a renamed profile, so that profile's files are never removed by mistake.
- **Clear progress everywhere.** Steps are numbered with a status and timing. Every run is logged to `~/.mcc/logs/mcc.log`, which is rotated to `mcc.log.1` at about 1 MB.
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
| `mcc add [ALIAS]` | Create the default profile, or a new profile with its own command; records the account and its organization, warns when another profile already uses that account, and asks for an optional label. A clone shares the default's plugins instead of copying them | `--clone`, `--fresh`, `--link ITEMS`, `--copy-plugins`, `--no-login`, `--label LABEL`, `-y` |
| `mcc list` (`ls`) | Show every profile with metrics and 5-hour/weekly usage | `--json`, `--json-v2`, `-l/--long`, `--no-usage` |
| `mcc usage [PROFILE]` | Show each profile's 5-hour and weekly usage in a table (`default` or `claude` for the default profile) | `--json`, `--json-v2`, `--show-email`, `--hide-email` |
| `mcc sync [ALIAS]` | Copy the default's setup into a profile (default → profile only), keeping edits made in the profile | `--all`, `--dry-run`, `--prune`, `--force`, `--only ITEMS`, `-y` |
| `mcc diff ALIAS` | Show how a profile differs from the default's setup, path by path, as `sync` classifies it | `--only ITEMS` |
| `mcc rename ALIAS NEW` (`mv`) | Rename a profile and its command; directory and login are kept | |
| `mcc share ALIAS` | Share the profile's sessions with the other sharing profiles of its group (off by default; see [Sharing sessions](#sharing-sessions-between-profiles)) | `--off`, `--group NAME`, `--allow-cross-org` |
| `mcc link ALIAS [ITEMS]` | Link read-mostly items (`skills`, `agents`, `commands`, `output-styles`, `claude-md`) to the default's copy instead of copying them; edits write through to the default (off by default; see [Linking read-mostly items](#linking-read-mostly-items-mcc-link)). `plugins` shares the default's plugins (see [Sharing plugins](#sharing-plugins-mcc-link-alias-plugins)) | `--off` |
| `mcc project set PROFILE` | Make PROFILE the one `mcc run` starts in this project (see [Per-project profiles](#per-project-profiles-mcc-project-and-mcc-run)); also `unset` and `list` | `--path DIR`, `--allow PROFILE`, `list --json`, `list --json-v2` |
| `mcc run [-- CLAUDE_ARGS]` | Start Claude Code with the current project's profile; at its limit, ask whether to wait (the default) or start it anyway | `--profile NAME`, `--dry-run` |
| `mcc env [PROFILE]` | Print the shell line that sets `CLAUDE_CONFIG_DIR` for the project's profile (or PROFILE), for `eval` or direnv (see [Using a project's profile everywhere](#using-a-projects-profile-everywhere-mcc-env-and-mcc-exec)) | `--shell zsh\|bash\|fish` |
| `mcc exec PROFILE -- CMD...` | Run a command with a profile's `CLAUDE_CONFIG_DIR` and return its exit status; `--all` runs it once per profile and reports each result | `--all` |
| `mcc config get\|set\|edit\|path` | Read or change `~/.mcc/config.toml` without hand-editing TOML; `set` and `edit` check values and leave the file unchanged when one is invalid (see [Changing settings](#changing-settings-mcc-config)) | `get KEY`, `set KEY VALUE`, `edit`, `path` |
| `mcc statusline install` | Opt-in: show the active profile in Claude Code's status line and cache each profile's rate limits (see [Status line](#status-line-mcc-statusline)); it edits the default profile's `settings.json`; also `uninstall` | `--prefix`, `-y/--yes` |
| `mcc refresh-launchers [ALIAS...]` | Rewrite launchers and shell aliases from the current templates (all profiles when none are given); needed once so `<alias> --mcc-share` works for profiles made by an older mcc | |
| `mcc doctor [PROFILE]` | Check every profile's setup (launcher, alias, PATH, login, config directory, sharing, permissions, drift, a global `CLAUDE_CONFIG_DIR`) and print a fix for each problem; read-only, exits 1 when it finds one (see [Checking your setup](#checking-your-setup-mcc-doctor)) | `--json`, `--json-v2` |
| `mcc delete ALIAS` (`rm`) | Log out and remove a profile | `-y/--yes`, `--keep-auth` |
| `mcc completions SHELL` | Print a completion script | `bash`, `zsh`, `fish`, `elvish`, `powershell` |
| `mcc help [COMMAND]` | Usage and examples | |

Global flags: `-v/--verbose`, `-q/--quiet`, `--no-color` (mcc also honors `NO_COLOR`), `--version`, `-h/--help`.

### Examples

```sh
mcc add                               # interactive: asks for the alias and clone/fresh
mcc add lab --fresh --no-login        # empty profile; log in later by running `lab`
mcc add side --clone --label personal # label the account instead of being asked
mcc list --long                       # one detailed card per profile
mcc list --json | jq '.[] | {name, account, skills, plugins_installed}'
mcc list --json | jq '.[] | {name, organization, organization_uuid, label}'
mcc list --json | jq '.[] | {name, five_hour: .usage.five_hour.percent, weekly: .usage.weekly.percent}'
mcc list --json | jq '.[] | {name, project_defaults}'   # the projects each profile starts in
mcc list --json-v2 | jq '.profiles[] | .name'   # versioned: {"schema": 2, "profiles": [...]}
mcc list --no-usage                   # offline: skip the usage lookup
mcc usage                             # every profile's limits, the default included
mcc usage work                        # only the `work` profile
mcc usage --show-email                # with each profile's account email
mcc usage --json | jq '.[] | {name, weekly: .usage.weekly.percent}'
mcc sync work --dry-run               # preview what would change
mcc sync work --only skills,agents    # only some items
mcc sync --all --prune --yes          # make every profile mirror the default exactly
mcc sync work --force                 # also overwrite paths edited in both (conflicts)
mcc diff work                         # what differs from the default, with line diffs
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
eval "$(mcc env)"                     # point CLAUDE_CONFIG_DIR at this project's profile (also in .envrc)
mcc exec --all -- claude plugin list  # run a command once per profile
mcc refresh-launchers                 # update launchers made by an older mcc
mcc doctor                            # check every profile's setup; prints a fix for each problem
mcc delete sandbox --yes
```

### JSON output and its schema version

`mcc list`, `mcc usage`, `mcc project list` and `mcc doctor` print JSON for scripts in two shapes:

| Flag | Shape | Schema | Read it with |
|---|---|---|---|
| `--json` | a bare array, one object per profile (per project for `project list`, per check for `doctor`) | 1, implicit | `jq '.[]'` |
| `--json-v2` | `{"schema": 2, "profiles": [...]}` (`"projects"` for `project list`, `"checks"` for `doctor`), the same objects as `--json` | 2, in the `schema` field | `jq '.profiles[]'`, `jq '.projects[]'`, `jq '.checks[]'` |

The contract: within a schema, fields may be added to the objects but are never renamed, removed or given another type. A change that breaks a shape gets a new schema number and a new flag, and the old flag keeps working. Scripts that should notice such a change can use `--json-v2` and check `schema` first, e.g. `mcc list --json-v2 | jq -e '.schema == 2' >/dev/null`.

**Deprecation path:** `--json` (schema 1) stays as it is in every 0.x release. If a future major release makes `--json` print the versioned object, the CHANGELOG will say so in an upgrade note, and `--json-v2` will keep printing schema 2 unchanged, so scripts written for it need no change.

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
| Shared sessions | `~/.mcc/shared/projects` (the `default` group) and `~/.mcc/shared/groups/<name>/projects` (`--group <name>`), only for profiles that opted in with `mcc share` (their `projects/` links there) |
| Project defaults | `[projects."<dir>"]` tables in `~/.mcc/config.toml`, written by `mcc project set` |
| Statusline state | `~/.mcc/state/<profile>/usage.json` (rate limits cached by `mcc statusline`) and `statusline.json` (the `statusLine` it replaced), only after `mcc statusline install` |
| mcc state | `~/.mcc/config.toml` (environment + registry; safe to edit by hand, mcc keeps your comments and formatting), `~/.mcc/.lock` (held while a command writes the config), `~/.mcc/logs/mcc.log` (rotated to `mcc.log.1` at about 1 MB), `~/.mcc/backups/` (rc file backups, session-merge conflicts) |

### What clone and sync copy

**Copied** from the default profile:

| Item (`--only` name) | Paths |
|---|---|
| `settings` | `settings.json`, `settings.local.json` |
| `claude-md` | `CLAUDE.md` |
| `keybindings` | `keybindings.json` |
| `statusline` | `statusline.conf` |
| `agents`, `skills`, `commands`, `hooks`, `output-styles` | those directories |
| `plugins` | `plugins/installed_plugins.json`, `plugins/known_marketplaces.json`, `plugins/marketplaces/`, `plugins/cache/` (only for a profile that keeps its own copy: a clone [shares the default's plugins](#sharing-plugins-mcc-link-alias-plugins) instead) |
| `mcp` | the `mcpServers` entry from `~/.claude.json` (merged; the rest of the profile's state is kept) |

**Never copied:** credentials, account identity (`oauthAccount`, `userID`), history, projects/sessions, todos, telemetry, caches, file history. (Sessions can be *shared* instead, opt-in, with `mcc share`; see below.)

**Paths are rewritten** so each profile uses its own copies. In settings, plugin manifests, and MCP server entries, any `~/.claude/…`, `$HOME/.claude/…`, or `/Users/you/.claude/…` path that points at a *copied* item (for example `~/.claude/hooks/notify.sh`) is rewritten to the profile directory. Paths to things that aren't copied, such as `~/.claude/statusline/run.sh`, keep pointing at the default profile, so nothing dangles. Symlinks stay symlinks: a link into the default profile is re-pointed into the copy, and a relative link elsewhere (such as `skills/x -> ../../.agents/skills/x`) is resolved so it still works from the new location.

**`sync` is one-way** (default → profile), because most tools assume the default config path. Before writing, `sync` copies every file or folder it is about to overwrite or remove (such as `CLAUDE.md`, skills, agents, `settings.json`, and `.claude.json`) to `<profile>/.mcc-backup/<timestamp>/`, at the same paths, so your own edits can be restored from there. `--dry-run` changes nothing in the profile and makes no backup. Files that exist only in the profile are kept unless you pass `--prune`, and pruning asks for confirmation (or needs `--yes`).

**`sync` is three-way.** `add --clone` and every `sync` record what they wrote in `<profile>/.mcc-sync.json` (each path and MCP server with a hash of its content). The next `sync` compares the default, the profile and that record for every path:

- changed only in the default: updated (or added) in the profile;
- edited, or deleted, only in the profile: kept, and reported as a **local edit**;
- changed in both, or added on both sides: a **conflict**, kept and reported; `--force` overwrites it (after backing it up).

`--dry-run` shows the same classification and writes nothing, not even the record. A profile without a record yet (created `--fresh` or by an older mcc) is compared two-way once: every difference is taken from the default, as before, and the record starts with that sync. The record belongs to its profile and is never copied. `mcc diff ALIAS` lists each differing path with its class and a line diff (`---` default, `+++` profile), and `mcc list` sums it up in its `SYNC` column (`--json`: `sync.behind`, `sync.local_edits`, `sync.conflicts`).

### Changing settings: `mcc config`

`mcc config` reads and changes `~/.mcc/config.toml` so you don't have to edit TOML by hand:

```sh
mcc config get usage.live                         # false (lists print one item per line)
mcc config set usage.live true
mcc config set sync.exclude plugins,mcp           # comma-separated; "" excludes nothing
mcc config set profiles.work.label "Acme (work)"  # "" removes the label
mcc config edit                                   # open the file in $VISUAL or $EDITOR
mcc config path                                   # print the file's path
```

`set` accepts `sync.exclude`, `sync.extra`, `usage.show_email`, `usage.live` and `profiles.ALIAS.label`, and checks each value as the command that uses it would: an unknown sync item, a path outside the config dir, a value that isn't `true` or `false`, or an invalid label is refused with exit 2 or 3, and the file is left unchanged. Keys that other commands manage point you to them (`mcc share`, `mcc link`, `mcc project`, `mcc init --force`). `get` reads any key, dotted as in TOML (`projects."/Users/me/app".profile`); a key that isn't set exits 1. Comments and formatting are kept.

`edit` opens a copy of the file and saves it only when it is valid mcc config (it parses, and sync items, `link` items, labels and `share_group` names pass the same checks); otherwise it offers to edit again, or leaves the file unchanged. mcc holds no lock while the editor is open, so other mcc commands, launches included, keep working; if one of them changes the file meanwhile, nothing is saved and your edited copy is kept next to it. `edit` also works on a file that no longer parses, so it can fix one. `get` and `path` never take the lock; `set` holds it like any other write.

### Customizing sync

In `~/.mcc/config.toml`:

```toml
[sync]
exclude = ["plugins"]          # built-in items to skip
extra = ["my-tool/config.yml"] # additional paths (relative to the config dir) to copy
```

### Linking read-mostly items: `mcc link`

Clone and sync give every profile its own copy of the default's `skills/`, `agents/`, `commands/`, `output-styles/` and `CLAUDE.md`, so the copies drift apart until the next sync. **Link mode** (opt-in per item, off by default) replaces a profile's copy with a symlink to the default's, so every linking profile sees the same files with nothing duplicated:

```sh
mcc link work skills,agents            # link work's skills/ and agents/ to ~/.claude's
mcc link work                          # show which items work links
mcc link work skills --off             # give work its own copy of skills/ again
mcc add lab --clone --link skills      # clone, linking skills/ instead of copying it
```

> **Write-through:** a linked item *is* the default's. Editing it in the profile, by hand or by Claude Code (installing a skill, say), changes the default's real files, and so every other profile that links it. mcc prints this warning every time link mode is turned on.

- Turning link mode on backs up the profile's own copy first (it may hold local edits) to `<profile>/.mcc-backup/<timestamp>/`, and drops its entries from the sync record (`.mcc-sync.json`).
- `mcc sync` and `mcc diff` leave linked items alone: they are never compared, copied, pruned (not even with `--prune`) or recorded, so they never show up as behind, local edits or conflicts in `mcc list`. A sync warns when a linked item is no longer the link.
- `--off` removes only the symlink, never the default's files it points at, copies the default's item in its place, and records it so the next sync is three-way again.
- Each run is all or nothing: if a step fails, what was already changed is undone. `plugins` (see below) is applied as a separate step after the other items, so a run like `mcc link work skills plugins` that fails while sharing plugins keeps `skills` linked.
- `settings` can't be linked (each profile's `settings.json` is merged with its [overlay](#per-profile-settings-overlays)), and neither can `keybindings`, `statusline`, `hooks` or `mcp`. `plugins` is shared a different way, described next.

The setting is kept per profile in `~/.mcc/config.toml`; set it with `mcc link`, which also creates the symlinks (editing the key by hand does nothing on its own):

```toml
[profiles.work]
link = ["skills", "agents"]   # default: none
```

### Sharing plugins: `mcc link ALIAS plugins`

A full copy of the default's plugin files in every profile adds up (about 88 MB per profile on one real setup). Instead, a profile can use the default's plugins directly. **`mcc add --clone` does this by default** (`--copy-plugins` gives the clone its own copy, as before); profiles made by an older mcc keep their copy until you opt in:

```sh
mcc link work plugins          # share ~/.claude/plugins with work; its own copy goes to a backup
mcc link work plugins --off    # give work its own copy of the plugins again
mcc add lab --clone --copy-plugins   # clone with a full copy of the plugins
```

**How it works.** Claude Code reads every plugin file from one *plugins root*, by default `<CLAUDE_CONFIG_DIR>/plugins`. The documented `CLAUDE_CODE_PLUGIN_CACHE_DIR` variable ([env vars](https://code.claude.com/docs/en/env-vars)) replaces that root; despite its name it is the whole root, not only the cache. mcc sets it to `~/.claude/plugins` for a sharing profile, in its launcher (`~/.local/bin/<alias>`), and in `mcc run`, `mcc exec` and `mcc env`. No symlink is made, and `~/.claude/plugins` is never moved or rewritten by mcc.

**Exactly what is shared** (checked against Claude Code 2.1.286; other versions may treat `CLAUDE_CODE_PLUGIN_CACHE_DIR` differently, see the caveat below): everything in the plugins root, which is `installed_plugins.json` (which plugins are installed, and where), `known_marketplaces.json` and `marketplaces/` (the registered marketplaces), `cache/` (the plugin files), `data/` (each plugin's persistent `${CLAUDE_PLUGIN_DATA}`), `store/`, and `synced/` (plugins synced from claude.ai, kept per account and organization). **What stays per profile:** which plugins are *enabled*, because `enabledPlugins` lives in the profile's `settings.json` (so an [overlay](#per-profile-settings-overlays) can turn a plugin on or off for one profile), plus everything outside the plugins root.

Trade-offs:

- **Verified on one Claude Code version.** The behavior above was verified on Claude Code 2.1.286; other versions may treat `CLAUDE_CODE_PLUGIN_CACHE_DIR` differently. If a sharing profile shows no plugins, `mcc link ALIAS plugins --off` gives it its own copy again (or clone with `--copy-plugins`).
- **Write-through.** Installing, updating or uninstalling a plugin or marketplace in a sharing profile does it for the default and every sharing profile. A plugin installed from one profile is installed (not enabled) everywhere; one uninstalled from one profile is gone for every profile that still has it enabled. Plugin data is shared too.
- **Only when mcc starts it.** The variable reaches Claude Code through the profile's command, its shell alias (which runs the launcher), `mcc run`, `mcc exec` and `eval "$(mcc env)"`. A profile started any other way, such as `CLAUDE_CONFIG_DIR=~/.mcc/profiles/work claude` or an IDE configured with only `CLAUDE_CONFIG_DIR`, uses its own `plugins/`, which is empty: set `CLAUDE_CODE_PLUGIN_CACHE_DIR` there as well. For a profile with its own plugins, `mcc run`, `mcc exec`, `mcc env` and launchers written by this version (`mcc refresh-launchers` updates older ones) clear the variable only when it holds `~/.claude/plugins` (mcc's own value, e.g. left by `eval "$(mcc env work)"`); a value you set yourself is left alone.
- **`sync` and `diff` leave plugins alone** for a sharing profile, like linked items: nothing under `plugins/` is copied, compared, pruned or recorded, and paths to `~/.claude/plugins/…` in its settings are not rewritten. `mcc list` counts the default's installed plugins for it.

**Moving an existing profile over is opt-in and keeps a backup.** `mcc link work plugins` moves the profile's own `plugins/` (it may hold plugins or plugin data the default lacks) to `<profile>/.mcc-backup/<timestamp>/plugins` with a rename, so nothing is copied or deleted, then rewrites the launcher and records `"plugins"` in `link`. The space is freed when you remove that backup folder; mcc prints its path and size. If a step fails, the move and the launcher are undone. Profiles whose shell alias predates launchers need `mcc refresh-launchers <alias>` once (mcc says so). `--off` moves any `plugins/` the profile has to a backup, copies the default's plugins in (paths rewritten, recorded for three-way sync), and removes the variable from the launcher. Restart the profile's running sessions after either.

### Per-profile settings overlays

`settings.json` is copied from the default, so on its own a profile can't keep a `model`, `env`, `permissions` or `statusLine` of its own. An **overlay** fixes that: clone and every `sync` write the default's `settings.json` (paths rewritten first), then deep-merge the profile's overlay onto it. Give a profile an overlay in either or both places:

```toml
# ~/.mcc/config.toml (or `mcc config edit`)
[profiles.work.settings]
model = "sonnet"

[profiles.work.settings.env]
ANTHROPIC_DEFAULT_SONNET_MODEL = "claude-sonnet-5"
```

```json
// ~/.mcc/profiles/work/settings.mcc.json
{ "permissions": { "allow": ["Bash(npm test)"] }, "statusLine": null }
```

- **Merge rules.** Objects merge key by key, recursively. A string, number, boolean or **array** replaces the default's value whole: arrays are never concatenated, so an overlay `permissions.allow` is the profile's whole list (copy the default's entries into it to extend them). A `null` removes the key (JSON only, since TOML has no null). Overlay values are written as they are: their paths are not rewritten.
- **Order.** The default's `settings.json`, then `[profiles.<alias>.settings]`, then `<profile>/settings.mcc.json`, so the file wins where both set a key. Only `settings.json` is overlaid; `settings.local.json` is still copied as it is. The overlay needs the default's `settings.json` to merge onto: while the default has none, nothing is written and `sync` warns. With an overlay, `settings.json` is written as formatted JSON (and as a file, even when the default's is a symlink); without one it is copied byte for byte, as before.
- **Sync keeps it.** `sync` recomputes the merge every time and records the merged result in `.mcc-sync.json`, so overlay keys never show up as local edits or conflicts in `sync`, `mcc diff` (which labels the source `default/settings.json + <alias> overlay`) or `mcc list`. Changing the overlay is a change the next `sync` applies; an edit made directly in the profile's `settings.json` is still a local edit, and `--force` replaces it with the merged result. As with every overwrite, the previous `settings.json` is backed up to `.mcc-backup/` first.
- **Excluded settings.** When `[sync] exclude` has `"settings"`, the profile owns its `settings.json` and sync never writes it, so the overlay is not applied (`sync` warns that it is ignored). Edit that profile's `settings.json` directly instead.
- **Status line.** An overlay `statusLine` replaces the default's, the `mcc statusline` shim included, so that profile's rate limits are no longer cached. To keep the shim with a profile-specific command, leave `statusLine` out of the overlay, or exclude `settings` and use `mcc statusline install`, which wraps each profile's own `statusLine`. `mcc sync` warns when an overlay replaces the shim this way.
- `settings.mcc.json` belongs to its profile: it is never copied, pruned or backed up by sync, and a `[sync] extra` entry naming it is rejected. An overlay that isn't a JSON object, or a default `settings.json` that isn't valid JSON while an overlay is set, stops the sync before anything is written.

### Showing account emails in `mcc usage`

Also in `~/.mcc/config.toml` (emails are hidden by default):

```toml
[usage]
show_email = true
```

### Live usage lookup (opt-in)

By default `mcc list`, `mcc usage` and `mcc run` read usage only from what `mcc statusline` cached, and never read a login. To have them look usage up live with each profile's login instead, opt in:

```toml
[usage]
live = true   # read each profile's OAuth token and ask Anthropic's usage endpoint
```

The first command that looks usage up live prints a one-time disclosure on stderr and records when as `live_disclosed_at` under `[usage]` (it takes `~/.mcc/.lock` for that one write, like every command that saves the config). See [Usage limits](#usage-limits-mcc-list-and-mcc-usage) for what it sends where.

### Sharing sessions between profiles

Each profile keeps its sessions to itself unless you turn sharing on. Sharing is a per-profile setting and is **off by default**. Sharing profiles are put in groups: once two or more profiles of a group share, `claude --resume` and `/resume` in any of them list the sessions of all of them, and of no profile outside the group.

```sh
mcc share work               # turn sharing on for `work`, in the `default` group
mcc share work --group acme  # …or in group `acme`, with a store of its own
work --mcc-share             # the same, while starting `work` (the flag must come first)
mcc share work --off         # turn it off again
```

The setting is stored per profile in `~/.mcc/config.toml`:

```toml
[profiles.work]
share_sessions = true  # default: false; set it with `mcc share`
share_group = "acme"   # left out for the `default` group
```

Editing the key by hand does nothing on its own. `mcc share <alias>` does the actual work, and it is the only supported way to change the setting:

- **What is shared:** the profile's `projects/` directory, where Claude Code keeps the session transcripts `--resume` lists. It is replaced with a link to its group's store: `~/.mcc/shared/groups/<name>/projects` for `--group <name>`, else `~/.mcc/shared/projects` (the `default` group). The sessions the profile already had are moved into that store without overwriting anything. If a file of the same name with different contents is already there, the store's copy is kept and the profile's copy goes to `~/.mcc/backups/shared-projects-<alias>-<time>/`, with a warning.
- **What is never shared:** credentials (`.credentials.json`, the keychain login), the account (`.claude.json`), `policy-limits.json`, `remote-settings.json`, prompt history (`history.jsonl`), live session state (`sessions/`), `shell-snapshots/`, `file-history/` and `todos/`. Each profile keeps its own login and `CLAUDE_CONFIG_DIR`.
- **The first time sharing is turned on for a profile**, by `mcc share` or by `--mcc-share`, mcc prints this note on stderr and records when as `share_note_acknowledged_at` under `[profiles.<alias>]`:

  > ⚠ Session sharing makes conversation history available across your Claude Code accounts. Use it only between accounts you own. Don't use it to get around usage limits, and never let other people use your account or credentials — doing so can violate Anthropic's Consumer Terms (https://www.anthropic.com/legal/consumer-terms) and Usage Policy (https://www.anthropic.com/legal/aup), including the updated Usage Policy that takes effect November 12, 2026.

  After that, every `mcc share` or `--mcc-share` for that profile prints a one-line reminder instead:

  > ⚠ Session sharing is on: use it only between accounts you own, never to get around usage limits or to let other people use your account (Anthropic's Consumer Terms and Usage Policy; full note: `mcc share --help`).

- **Groups.** A profile shares in one group at a time. `mcc share <alias>` and `--mcc-share` reuse the group recorded as `share_group` (the `default` group when there is none). To move a profile to another group, run `mcc share <alias> --off`, then `mcc share <alias> --group <name>`. A group name is 1–64 letters, digits, `-`, `_` or `.`, starting with a letter or digit; `--group default` is the default group.
- **Organizations.** A work transcript (commercial terms, a customer's confidential information) resumed under a personal account goes to Anthropic under consumer terms. So `mcc share` compares the profile's account (as `mcc add` recorded it under `[profiles.<alias>.account]`, or as its `.claude.json` says now) with every other sharing profile of the group, by organization UUID, else by organization name. Claude Code gives personal plans an organization of their own, so the organization type tells them apart: Free, Pro and Max (or no organization at all) are personal, Team and Enterprise are work organizations, and an organization whose type mcc doesn't know is treated as a possible work organization.
  - Another work organization: refused, unless you pass `--allow-cross-org`.
  - An account mcc can't identify (`--no-login`, never logged in): refused the same way, so unknown never counts as "the same".
  - A work organization (or one of unknown type) and a personal or unidentified account, whichever joins: the strictest case. `--allow-cross-org` must also be confirmed at a prompt in a terminal (the default answer is no); without a terminal, and from `<alias> --mcc-share`, it is always refused.
  - A refusal changes nothing, and `<alias> --mcc-share` then exits without starting Claude Code. When `--allow-cross-org` is accepted, mcc records when as `share_cross_org_allowed_at`; `--off` clears it. Keeping work and personal profiles in separate groups (`--group`) needs no override.
- **`--off`** replaces the link with a directory of the profile's own, holding a copy of the shared sessions. The group's store stays for its other sharing profiles; mcc never deletes it, so remove it (or `~/.mcc/shared`) by hand once no profile uses it. The group stays recorded, so the next `mcc share` joins it again.
- **`mcc delete`** of a sharing profile removes only its link. The sessions stay in the store.
- **`mcc rename`** keeps sharing as it is.
- **The default profile** (`~/.claude`) can't share.
- **Caveats.** In `mcc list`, sharing profiles are marked `(shared)`, or `(shared: <group>)` in a named group (`"share_sessions": true` and `"share_group"` in `mcc list --json`), and their session counts include every sharing profile's sessions in the group. Claude Code removes old transcripts after `cleanupPeriodDays` (30 days unless set in `settings.json`): with sharing, the shortest setting among the sharing profiles applies to all of them.

**Upgrading from an older mcc:** profiles that already share keep using `~/.mcc/shared/projects`, which is now the `default` group's store: nothing is moved, and the config is not rewritten. If they mix organizations (or a work and a personal account), `mcc share` and `--mcc-share` warn on every run but never cut them off; to separate them, run `mcc share <alias> --off` (the profile keeps a copy of every shared session) and then `mcc share <alias> --group <name>`, or keep them together with `mcc share <alias> --allow-cross-org`. Existing launchers and shell aliases are left exactly as they were. `<alias> --mcc-share` needs the current launcher, and an alias that runs it. To get both, run `mcc refresh-launchers` once (or `mcc refresh-launchers work` for a single profile). `CLAUDE_CONFIG_DIR` stays the same, so logins are kept. `mcc share <alias>` works without it, and `mcc list` reminds you while a launcher is out of date (`"launcher_current": false` in `mcc list --json`).

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

- The usage it checks is the same as for `mcc usage`, described below: the statusline cache by default, so a profile at its limit is only recognized once its rate limits have been cached (or with `[usage] live = true`).

Project defaults live in `~/.mcc/config.toml`:

```toml
[projects."/Users/me/code/app"]
profile = "work"  # an mcc profile, or "default" for ~/.claude
allow = ["lab"]   # optional: other profiles `mcc run` may offer at the limit
```

### Using a project's profile everywhere: `mcc env` and `mcc exec`

`mcc run` only covers Claude Code started through it. `mcc env` prints the shell line that points `CLAUDE_CONFIG_DIR` at the profile `mcc run` would start here (the project's default, else the default profile), so plain `claude`, IDE extensions and scripts use it too:

```sh
eval "$(mcc env)"        # this shell now uses the project's profile
mcc env work             # export CLAUDE_CONFIG_DIR='/Users/me/.mcc/profiles/work'
mcc env default          # unset CLAUDE_CONFIG_DIR (the default profile, ~/.claude)
mcc env | source         # fish: set -gx CLAUDE_CONFIG_DIR '/Users/me/.mcc/profiles/work'
```

The shell is `--shell zsh|bash|fish`, else bash inside a direnv `.envrc`, else your `$SHELL` (pass `--shell bash` when evaluating it in another shell than your login shell). Paths are single-quoted, so spaces and quotes in them are safe. It never checks usage and never writes anything; the choice stays the one you made with `mcc project set`, and naming a profile other than the project's prints the same note as `mcc run --profile`.

**With [direnv](https://direnv.net/)**, the project's profile applies whenever you `cd` into it. Put this in the project's `.envrc`, then run `direnv allow`:

```sh
eval "$(mcc env)"
```

direnv evaluates `.envrc` with bash (fish users included), and `mcc env` prints the bash form there. Leaving the directory restores the previous `CLAUDE_CONFIG_DIR`. `mcc project set` later changes what the `.envrc` prints, after `direnv reload`.

`mcc exec` runs any command with a profile's `CLAUDE_CONFIG_DIR`, which makes Claude Code's own [plugin](https://code.claude.com/docs/en/plugins/cli-reference) and [MCP](https://code.claude.com/docs/en/mcp) commands work per profile:

```sh
mcc exec work -- claude mcp list                         # as `work`; exits with the command's status
mcc exec --all -- claude plugin marketplace update       # once per profile, the default first
```

With `--all`, each profile's output comes under its name followed by its exit status, and mcc exits 0 only if the command succeeded everywhere (else with the first failing status). A command that can't be found counts as exit 127 (126 when it can't be run), and Ctrl+C stops at the current profile. mcc holds no lock while the command runs, so the command may run mcc itself.

### Accounts, organizations and labels

After `mcc add` logs a profile in, it records the account under `[profiles.<alias>.account]` in `~/.mcc/config.toml`: its email, Claude Code's account UUID, and its organization's name and UUID, each when Claude Code reports it. If another profile (the default included) is already logged in to the same account in the same organization, `mcc add` warns: profiles on one account share its usage limits, so this is usually a mistake. The same login in another organization, such as a personal plan and a work seat, has limits of its own, so it only gets a note. It then asks for an optional label, such as `personal` or the account's organization; `--label LABEL` sets it without asking, and `-y` or a non-interactive run skips it.

```toml
[profiles.work]
label = "Corp (work)"

[profiles.work.account]
email = "work@corp.com"
organization = "Corp"
organization_uuid = "…"
recorded_at = "2026-10-20T09:00:00+02:00"
```

`mcc list` shows the organization in its `ORG` column; `--long` adds `Organization ID` and `Label` rows, and `--json` has `organization`, `organization_uuid` and `label` per profile. What the profile's `.claude.json` says now wins over what was recorded. Profiles added before mcc recorded accounts, or with `--no-login`, have no `account` table and show only what `.claude.json` says.

### Usage limits: `mcc list` and `mcc usage`

The `5H` and `WEEK` columns of `mcc list` show how much of each account's 5-hour and weekly limit is used, and how long until it resets, for every profile including the default. `UPDATED` says how old those numbers are:

```console
NAME       COMMAND  ACCOUNT         ORG   5H                     WEEK                   UPDATED  SKILLS  …
default ★  claude   me@example.com  —     ████░░░░░░ 42% (2h)    ██░░░░░░░░ 18% (3d)    12m ago       2  …
work       work     work@corp.com   Corp  █████████░ 85%! (35m)  ██████████ 96%!! (3d)  just now      2  …
lab        lab      —               —     —                      —                      —             0  …
```

`mcc list --long` spells it out (`5h usage: ████████░░░░░░░░░░░░ 42% (resets in 2h 14m)`, `Usage updated: 12m ago (statusline cache)`), or says why there is none (`Usage: — (no recent usage cached; run /usage in Claude Code)`).

`mcc usage` shows only usage, as a compact table with one row per profile: for each window a bar with the percent used, the time until it resets, and the reset time in your local time zone. `mcc usage work` shows one profile (`mcc usage default` for `~/.claude`); an unknown name is a usage error (exit 2). Account emails are hidden by default, so the output is safe to share on screen; `--show-email` adds an `ACCOUNT` column for one run, and `show_email = true` under `[usage]` in `~/.mcc/config.toml` shows it every time (`--hide-email` then hides it again).

```console
$ mcc usage
PROFILE    5-HOUR           RESETS IN                  WEEKLY            RESETS IN                 UPDATED
default ★  ████░░░░░░ 42%   2h 14m (Sat Oct 10 16:00)  ██░░░░░░░░ 18%    3d 4h (Wed Oct 14 00:00)  12m ago
lab        — (no recent usage cached; run /usage in Claude Code)
work       █████████░ 85%!  35m (Sat Oct 10 14:21)     ██████████ 96%!!  3d 4h (Wed Oct 14 00:00)  just now
```

- **Where it comes from (default).** The rate limits [`mcc statusline`](#status-line-mcc-statusline) caches in `~/.mcc/state/<profile>/usage.json` while you use Claude Code. No login is read and nothing touches the network. `UPDATED` (and `· 12m ago` in `mcc run`) is how old the cached numbers are. A window whose reset time has passed is obsolete and shown as `—`. A profile with nothing current cached (an idle profile, a Team or Enterprise seat that reports no rate limits, or no `mcc statusline install` yet) shows `—` and a hint to run `/usage` inside Claude Code.

- **Reading the bars.** Each bar is the share of the limit used. `!` after the percent marks 80% or more and `!!` 95% or more. In a colour terminal the bar is also green, yellow (80%+) or red (95%+), but the markers carry the same meaning, so nothing depends on colour alone (`--no-color`, `NO_COLOR`, or piped output).
- **Narrow terminals.** When the output would not fit the terminal's width, bars shrink (`mcc usage` first drops the absolute reset time, keeping the time until the reset, then shrinks its bar in the same 10/5/0 steps as the `mcc list` table). From 80 columns up a compact bar always stays, even if the `mcc list` table has to wrap; below 80 columns bars disappear (leaving just the percent, such as `42%`) when nothing fits. Set `COLUMNS` to override the detected width. Piped output gets full-size bars unless `COLUMNS` is set, and `--json` never has bars or colour.
- **Live lookup (opt-in, `[usage] live = true`).** mcc reads each profile's own Claude Code login (`<config dir>/.credentials.json`, or on macOS the login Keychain item Claude Code created for that config dir, under your login name: `$USER`, or the system's name for your user when `USER` is unset, as under launchd or cron) and asks Anthropic's usage endpoint (`https://api.anthropic.com/api/oauth/usage`, the one behind Claude Code's `/usage` screen). It is off by default: Claude Code's [legal and compliance page](https://code.claude.com/docs/en/legal-and-compliance) says developers may not collect or intermediate Claude.ai credentials, and the endpoint is **undocumented**, rate-limited and may change; when it does, mcc shows "unavailable" rather than failing. The first live lookup prints a one-time disclosure.
- **Never fatal.** If usage can't be determined (nothing cached; or, live: logged out, expired login, offline, API error), the profile shows `—` or `unavailable` with the reason (in `mcc usage` and `mcc list --long`), and the command still exits 0. Live requests run in parallel with short timeouts (2 s to connect, 4 s total).
- **Tokens stay private.** With the live lookup, the access token is only sent to that endpoint, for its own profile. It is never printed, logged, or written anywhere. mcc never refreshes tokens: if a login has expired, start the profile in Claude Code once (`work`, `claude`, …) and it refreshes itself.
- **Turn it off** with `mcc list --no-usage` or `MCC_NO_USAGE=1` (which also applies to `mcc usage` and `mcc run`); usage then shows `—` and nothing reads the cache, the network or the keychain.

In `mcc list --json`, every profile has a `usage` object. `mcc usage --json` prints one object per profile with `name`, `command`, `is_default`, `account` (`null` when logged out, or when emails are hidden, as they are by default since v0.2.0; scripts that read `account` need `--show-email` or `[usage] show_email = true`) and the same `usage` object:

```json
"usage": {
  "status": "ok",
  "reason": null,
  "five_hour": { "percent": 42.0, "resets_at": "2026-10-10T16:00:00Z" },
  "weekly":    { "percent": 18.0, "resets_at": "2026-10-14T00:00:00Z" },
  "source": "cache",
  "updated_at": "2026-10-10T11:48:00Z"
}
```

`status` is `ok` or `unavailable`. `five_hour` and `weekly` are `null` when there is no data (`resets_at` can be `null` too). `source` is `cache` (the statusline cache), `live` (`[usage] live = true`) or `null` (not looked up); `updated_at` is when the statusline cached the numbers (`null` for live lookups). When `status` is `unavailable`, `reason` is one of:

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
| `no_data` | nothing current in the statusline cache: never cached, or every cached window has reset since; run `/usage` in Claude Code |

The other reasons only come from the live lookup.

### Status line: `mcc statusline`

Opt-in. Claude Code passes the session's 5-hour and weekly rate limits (`rate_limits.five_hour` and `rate_limits.seven_day`, each with `used_percentage` and `resets_at`) to its [status-line command](https://code.claude.com/docs/en/statusline) on stdin. `mcc statusline` is a small shim for that slot: it records them per profile, without reading any login, and can show which profile you're in.

```sh
mcc statusline install            # wrap your statusLine in ~/.claude/settings.json
mcc statusline install --prefix   # …and start it with the profile's name, e.g. [work]
mcc statusline uninstall          # put the previous statusLine back
```

- **Install writes to the default profile.** Apart from `mcc add` creating a missing `~/.claude`, `install` and `uninstall` are the only mcc commands that change your default profile. A symlinked `settings.json` (from a dotfiles manager, say) stays a symlink: its target is written. It copies `~/.claude/settings.json` to `~/.mcc/backups/settings.json.default.<timestamp>` first, records your current `statusLine` in `~/.mcc/state/default/statusline.json`, and sets `statusLine` to `mcc statusline` (keeping its other fields, such as `padding`). Running it again changes nothing: the shim is never wrapped twice. `--prefix` on an installed shim only switches the prefix.
- **Profiles.** Profiles get the shim with their settings on the next `mcc sync` (e.g. `mcc sync --all --only settings`), unless their [settings overlay](#per-profile-settings-overlays) sets a `statusLine` of its own, which wins. When `[sync] exclude` has `"settings"`, profiles never receive the default's settings, so `install` offers to add the shim to each profile's own `settings.json` (with a backup, and that profile's own `statusLine` recorded in `~/.mcc/state/<alias>/statusline.json`); `--yes` adds it without asking, and without a terminal it is skipped with a hint.
- **On every refresh** Claude Code runs the shim with the session's JSON on stdin. The shim works out the profile from `CLAUDE_CONFIG_DIR`, which Claude Code passes on to the status-line command (unset means the default profile), saves the rate limits to `~/.mcc/state/<profile>/usage.json`, runs your previous `statusLine` command with the same stdin and `CLAUDE_CONFIG_DIR`, and prints its output (after `[<profile>] ` with `--prefix`). A profile that got the shim through sync runs the default's previous command, with paths rewritten as sync rewrites them. The shim never takes mcc's lock, never writes `~/.mcc/config.toml` or the log, and a failure to cache never blanks the status line.
- **Uninstall** restores the recorded `statusLine` (or removes it, if there was none) in the default profile and in every profile where the shim is set, backing each file up first. A `statusLine` you changed by hand since is left alone, with a warning.
- **Limits of the data.** Claude Code sends rate limits only to Pro and Max subscribers, and only after a session's first response. A profile you don't use keeps its last values, so check `updated_at`. A `statusLine` in a project's own `.claude/settings.json` overrides the user one, so the shim doesn't run (or cache) in such projects:

```json
{
  "profile": "work",
  "updated_at": "2026-10-20T10:00:00Z",
  "five_hour": { "used_percentage": 23.5, "resets_at": 1792500000 },
  "seven_day": { "used_percentage": 41.2, "resets_at": 1792900000 }
}
```

`mcc rename` moves a profile's `~/.mcc/state/<alias>/` with it and `mcc delete` removes it.

### Checking your setup: `mcc doctor`

`mcc doctor` checks every profile, the default included, and prints a fix for each problem it finds. It only reads: it never writes `~/.mcc/config.toml` (not even the first-run setup), takes no lock, and changes no file. `mcc doctor work` checks one profile.

| Check | Fix it prints |
|---|---|
| mcc is set up, `claude` is found | `mcc init`; install Claude Code or set `MCC_CLAUDE_BIN` |
| `~/.local/bin` is on PATH | `source ~/.zshrc` when the rc file already adds it, else `mcc init --force` (or the line to add) |
| No rc file sets `CLAUDE_CONFIG_DIR` for every shell (`export`, a bare assignment, fish's `set -gx`); comments, aliases and mcc's own blocks are ignored. Such a line makes plain `claude` stop starting the default profile | remove line N from the rc file |
| `CLAUDE_CONFIG_DIR` set in the current shell (a warning: expected inside `mcc exec` or after `eval "$(mcc env)"`) | `unset CLAUDE_CONFIG_DIR` (fish: `set -e CLAUDE_CONFIG_DIR`) |
| The profile's directory exists, with mode 0700 | `mcc delete ALIAS`; `chmod 700 DIR` |
| Its launcher is there, written by mcc, executable, starts the profile's directory, sets the plugins root exactly when the profile shares plugins, and is current | `mcc refresh-launchers ALIAS`; `chmod +x`; move a file mcc didn't write out of the way |
| Its alias block is in the rc file it was added to | the block to paste (no command adds it back) |
| `claude auth status` reports the profile's directory as `configDirectory` (Claude Code 2.1.268 or later; skipped before), run through the profile's launcher | `mcc refresh-launchers ALIAS` |
| It is logged in | `mcc exec ALIAS -- claude auth login` (`claude auth login` for the default) |
| Its OAuth token hasn't expired (a warning), checked only with `[usage] live = true` and `MCC_NO_USAGE` unset, the setting under which mcc reads tokens | `mcc exec ALIAS -- claude auth login` |
| Its `projects/` links to its sharing group's store exactly when sharing is on, and the store exists | `mcc share ALIAS`, `mcc share ALIAS --off` |
| No `.projects.mcc-share-*` or `.projects.mcc-unshare-*` left by an interrupted `mcc share` | `mcc share ALIAS` for a sharing profile, else what to move or remove |
| Drift against the default, as `mcc list` shows it (a warning; local edits are kept by design and linked items, overlays and shared plugins never count) | `mcc sync ALIAS`, or `mcc sync ALIAS --force` for conflicts |
| With the `mcc statusline` shim installed, each profile's `settings.json` has it too unless its overlay sets a `statusLine` (a warning) | `mcc sync ALIAS --only settings`, or `mcc statusline install --yes` when settings aren't synced |

Each finding is `ok`, `warn`, `problem` or `skipped` (with the reason). mcc exits 1 when there is at least one problem, after printing them all; warnings don't fail it. `--json` prints the findings as a bare array of `{"profile", "check", "status", "message", "fix"}` objects (`profile` is `null` for the checks every profile shares, `fix` is `null` when there is nothing to do), and `--json-v2` wraps it as `{"schema": 2, "checks": [...]}`.

### Alias validation

An alias must be 1–32 characters: lowercase letters, digits, `-` or `_`, starting with a letter. It is rejected if any of these already uses the name:

- mcc's reserved names: `claude`, `mcc`, `default`, `help`, `all`, `path`, `share`, `refresh-launchers`, `project`, `run`, `statusline`, `doctor`
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
| `MCC_USAGE_URL` | Test/debug override for the live usage endpoint (`[usage] live = true`). It receives bearer tokens, so only `https://` URLs and `http://` on loopback (`127.0.0.1`, `localhost`, `[::1]`) are accepted; any other value is refused and usage is not fetched at all |
| `MCC_SECURITY_BIN` | Test/debug override for the macOS `security` binary used to read Keychain logins with `[usage] live = true` (default `/usr/bin/security`, macOS only) |
| `NO_COLOR` | Disable colors |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Runtime error (I/O failure, `claude` missing, …), or `mcc doctor` found a problem |
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
