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
- **`list`.** One row per profile: command, account, 5-hour and weekly usage, skills, agents, commands, plugins (enabled/installed), MCP servers, hooks, size, last used, last sync. `--long` and `--json` give more detail.
- **`sync`.** A one-way copy of setup from the default profile into a profile, with `--dry-run`, `--prune`, `--only`, and `--all`.
- **`delete`.** Logs the profile out (clearing its keychain entry), then removes its directory, launcher, shell alias, and registry entry.
- **Clear progress everywhere.** Steps are numbered with a status and timing. Every run is logged to `~/.mcc/logs/mcc.log`.
- **Safe by design.** Changes are rolled back if `add` fails. Your rc file is backed up before mcc edits it. mcc only removes what it created.

Supports **macOS and Linux**.

## Install

You need Rust (`cargo`) and [Claude Code](https://docs.claude.com/en/docs/claude-code).

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
mcc sync work               # later: pull new skills/plugins/settings from the default
mcc delete work             # remove it completely
```

## Commands

| Command | What it does | Key options |
|---|---|---|
| `mcc init` | Detect OS, arch, terminal, shell, rc file, the `claude` binary, and whether `~/.local/bin` is on PATH (and offer to add it) | `--force`, `--shell zsh\|bash\|fish` |
| `mcc add [ALIAS]` | Create the default profile, or a new profile with its own command | `--clone`, `--fresh`, `--no-login`, `-y` |
| `mcc list` (`ls`) | Show every profile with metrics and 5-hour/weekly usage | `--json`, `-l/--long`, `--no-usage` |
| `mcc sync [ALIAS]` | Copy the default's setup into a profile (default → profile only) | `--all`, `--dry-run`, `--prune`, `--only ITEMS`, `-y` |
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
mcc list --no-usage                   # offline: skip the usage lookup
mcc sync work --dry-run               # preview what would change
mcc sync work --only skills,agents    # only some items
mcc sync --all --prune --yes          # make every profile mirror the default exactly
mcc delete lab --yes
```

### Shell completions

```sh
mcc completions zsh  > ~/.zfunc/_mcc        # with fpath+=~/.zfunc in ~/.zshrc
mcc completions bash > ~/.local/share/bash-completion/completions/mcc
mcc completions fish > ~/.config/fish/completions/mcc.fish
```

## How it works

| What | Where |
|---|---|
| Default profile | `~/.claude` + `~/.claude.json` (command: `claude`) |
| mcc profiles | `~/.mcc/profiles/<alias>/`, used as `CLAUDE_CONFIG_DIR` |
| Launcher | `~/.local/bin/<alias>` (works in any shell as soon as it's written) |
| Shell alias | a managed block in your rc file, between `# >>> mcc:<alias> >>>` and `# <<< mcc:<alias> <<<` |
| mcc state | `~/.mcc/config.toml` (environment + registry), `~/.mcc/logs/mcc.log`, `~/.mcc/backups/` (rc file backups) |

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

**Never copied:** credentials, account identity (`oauthAccount`, `userID`), history, projects/sessions, todos, telemetry, caches, file history.

**Paths are rewritten** so each profile uses its own copies. In settings, plugin manifests, and MCP server entries, any `~/.claude/…`, `$HOME/.claude/…`, or `/Users/you/.claude/…` path that points at a *copied* item (for example `~/.claude/hooks/notify.sh`) is rewritten to the profile directory. Paths to things that aren't copied, such as `~/.claude/statusline/run.sh`, keep pointing at the default profile, so nothing dangles. Symlinks stay symlinks: a link into the default profile is re-pointed into the copy, and a relative link elsewhere (such as `skills/x -> ../../.agents/skills/x`) is resolved so it still works from the new location.

**`sync` is one-way** (default → profile), because most tools assume the default config path. Before writing, `sync` backs up the profile's `settings.json`, `settings.local.json`, and `.claude.json` to `<profile>/.mcc-backup/<timestamp>/`. Files that exist only in the profile are kept unless you pass `--prune`, and pruning asks for confirmation (or needs `--yes`).

### Customizing sync

In `~/.mcc/config.toml`:

```toml
[sync]
exclude = ["plugins"]          # built-in items to skip
extra = ["my-tool/config.yml"] # additional paths (relative to the config dir) to copy
```

### Usage limits in `mcc list`

The `5H` and `WEEK` columns show how much of each account's 5-hour and weekly limit is used, and how long until it resets, for every profile including the default:

```console
NAME       COMMAND  ACCOUNT         5H           WEEK         SKILLS  …
default ★  claude   me@example.com  42% (2h)     18% (3d)          2  …
work       work     work@corp.com   7% (35m)     64% (3d)          2  …
lab        lab      —               unavailable  unavailable       0  …
```

`mcc list --long` spells it out (`5h usage: 42% (resets in 2h 14m)`, `Weekly usage: 18% (resets in 3d 4h)`), or says why it is unavailable (`Usage: unavailable (not logged in)`).

- **Where it comes from.** mcc reads each profile's own Claude Code login (`<config dir>/.credentials.json`, or on macOS the login Keychain item Claude Code created for that config dir) and asks Anthropic's usage endpoint (`https://api.anthropic.com/api/oauth/usage`, the one behind Claude Code's `/usage` screen). The endpoint is **undocumented** and may change; when it does, mcc shows "unavailable" rather than failing.
- **Never fatal.** If usage can't be determined (logged out, expired login, offline, API error), the row shows `unavailable`, `mcc list` still exits 0, and no warning is printed. Requests run in parallel with short timeouts (2 s to connect, 4 s total).
- **Tokens stay private.** The access token is only sent to that endpoint, for its own profile. It is never printed, logged, or written anywhere. mcc never refreshes tokens: if a login has expired, start the profile in Claude Code once (`work`, `claude`, …) and it refreshes itself.
- **Turn it off** with `--no-usage` or `MCC_NO_USAGE=1`; the columns then show `—` and nothing touches the network or the keychain.

In `--json`, every profile has a `usage` object:

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

- mcc's reserved names: `claude`, `mcc`, `default`, `help`, `all`
- shell keywords and builtins
- existing mcc profiles
- executables on `$PATH`
- aliases and functions in your rc files (`.zshrc`, `.bashrc`, `.bash_profile`, `.profile`, fish config, …)
- anything your interactive shell resolves with `type <name>`, which catches plugin-defined aliases

## Environment variables

| Variable | Effect |
|---|---|
| `MCC_HOME` | Use this directory instead of `$HOME` for everything mcc reads and writes (great for testing) |
| `MCC_CLAUDE_BIN` | Path to the `claude` binary |
| `MCC_NO_USAGE` | Any value except empty or `0`: same as `mcc list --no-usage` |
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
cargo uninstall mcc
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
