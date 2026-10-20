# mcc — Implementation Plan

> **Historical document.** This is the original implementation plan for mcc and is no longer maintained. Its task list and command tree are out of date: the tree has none of the commands added since, such as `mcc usage`, `mcc rename`, `mcc share`, `mcc project`, `mcc run`, `mcc refresh-launchers` and `mcc completions`. For current behavior, see the [README](../README.md), `mcc --help` and the [docs site](https://luongnv89.github.io/mcc/docs.html).

`mcc` (Multiple Claude Code) is a Rust CLI for managing multiple Claude Code profiles (accounts) on macOS and Linux. Each profile gets its own `CLAUDE_CONFIG_DIR` and launcher command.

## Approved design (summary)

| Item | Location |
|---|---|
| Default profile | `~/.claude` + `~/.claude.json`, command `claude` |
| New profiles | `~/.mcc/profiles/<alias>/` (`CLAUDE_CONFIG_DIR`) |
| mcc state | `~/.mcc/config.toml` (env + profile registry), `~/.mcc/logs/mcc.log` |
| Launcher | `~/.local/bin/<alias>` wrapper **and** `# >>> mcc:<alias> >>>` alias block in the detected rc file |

### Command tree

```
mcc
├── init                  Detect OS, arch, terminal, shell, rc file, claude binary, PATH (auto on first run)
├── add [ALIAS]           Add a profile (creates the default if ~/.claude is missing)
├── list | ls             List all profiles with metrics
├── delete | rm <ALIAS>   Remove a profile completely
├── sync [ALIAS] | --all  One-way sync: default → profile
└── help [CMD]            Usage guide with examples
```

### Options

| Command | Args / options |
|---|---|
| `init` | `--force`, `--shell <zsh\|bash\|fish>` |
| `add` | `[ALIAS]`, `--clone` / `--fresh`, `--no-login`, `-y/--yes` |
| `list` | `--json`, `-l/--long` |
| `delete` | `<ALIAS>`, `-y/--yes`, `--keep-auth` |
| `sync` | `[ALIAS]` or `--all`, `--dry-run`, `--prune`, `--only <items>` |
| Global | `-v/--verbose`, `-q/--quiet`, `--no-color` (+ `NO_COLOR`), `--version`, `-h/--help` |

### Key behaviors

- **Alias validation**: `^[a-z][a-z0-9_-]{0,31}$`; rejects `claude`/`mcc`/`default`, shell reserved words/builtins, existing mcc profiles, executables on `$PATH`, aliases/functions in rc files, anything `$SHELL -ic 'type <name>'` resolves (3s timeout), and existing target dir/wrapper. Each rejection explains why.
- **Clone/sync whitelist** (configurable): `settings.json`, `settings.local.json`, `CLAUDE.md`, `keybindings.json`, `statusline.conf`, `agents/`, `skills/`, `commands/`, `hooks/`, `output-styles/`, `plugins/{installed_plugins.json,known_marketplaces.json,marketplaces/,cache/}`, plus `mcpServers` merged from `~/.claude.json`. Never copies credentials, `oauthAccount`, `userID`, history, projects, sessions, telemetry, caches. Sessions (`projects/` only) can instead be shared, opt-in and off by default, with `mcc share <alias>` / `<alias> --mcc-share` (a link to `~/.mcc/shared/projects`; credentials and account files are never shared). Symlinks preserved. `~/.claude/…`, `$HOME/.claude/…`, `<abs home>/.claude/…` rewritten to the profile dir in JSON/conf files.
- **add**: numbered steps `[n/7]` (validate → create dir → clone/fresh → rewrite paths → wrapper → rc block → `claude auth login` + `claude auth status` verification), rollback on failure.
- **sync**: plan (added/updated/removed/unchanged), progress bar, backups of `settings.json`/`.claude.json` to `<profile>/.mcc-backup/<ts>/`, last-sync recorded.
- **delete**: refuses `default`; shows size; type-to-confirm; `claude auth logout` for the profile (clears keychain entry); removes wrapper (only if mcc-marked), rc block, directory, registry entry.
- **list**: NAME · COMMAND · ACCOUNT · SKILLS · AGENTS · COMMANDS · PLUGINS (enabled/installed) · MCP · HOOKS · PROJECTS · SIZE · LAST USED · LAST SYNC.
- **I/O**: data → stdout; progress/logs/prompts/errors → stderr; also logged to `~/.mcc/logs/mcc.log`. Non-TTY requires flags (exit 2 otherwise).
- **Config precedence**: CLI args > env (`MCC_HOME`, `MCC_CLAUDE_BIN`) > `~/.mcc/config.toml` > defaults.
- **Exit codes**: 0 ok · 1 runtime · 2 usage/invalid alias · 3 bad config/input · 130 Ctrl+C.
- **Test safety**: all tests/demos use `MCC_HOME=<tempdir>` and a fake `claude` binary; never the real `~/.claude`, rc files, or keychain.

**Test command of record:** `cargo test`

## Phase P1 — Foundation

**Goal:** a working `mcc` skeleton with environment detection and the core `add` command · **Milestone M1:** `mcc add work --clone` creates a working profile in a sandboxed HOME and `cargo test` passes

### Task 1.1: Scaffold the Cargo project, command tree, and global flags

**Description**: Create the Cargo project with a clap (derive) command tree for `init`, `add`, `list|ls`, `delete|rm`, `sync`, `help`; global flags `-v/--verbose`, `-q/--quiet`, `--no-color` (and `NO_COLOR`), `--version`; color handling; and exit-code mapping (0 ok, 1 runtime, 2 usage, 3 input/config, 130 Ctrl+C). Files: `Cargo.toml`, `src/main.rs`, `src/cli.rs`, `src/ui.rs`, `src/error.rs`.

**Acceptance Criteria**:
- [ ] `mcc --help` and `mcc --version` exit 0 with useful output
- [ ] `--help` works on every subcommand
- [ ] `--no-color` and `NO_COLOR` disable ANSI colors
- [ ] Unknown arguments exit 2 with an error on stderr

**Dependencies**: None

**Effort**: S

**Verify**: `cargo test` (assert_cmd help/version tests)

### Task 1.2: Add path resolution, config registry, logger, and progress reporter

**Description**: Resolve every path from `MCC_HOME` (falling back to `HOME`); persist `~/.mcc/config.toml` (detected environment + profile registry); append timestamped logs to `~/.mcc/logs/mcc.log`; provide a step/progress reporter (`[n/N] step … ✓/✗`) that writes to stderr. Files: `src/paths.rs`, `src/config.rs`, `src/log.rs`.

**Acceptance Criteria**:
- [ ] All paths derive from `MCC_HOME`/`HOME`; nothing is hardcoded
- [ ] `config.toml` round-trips (write then read yields the same registry)
- [ ] Each command run appends to `~/.mcc/logs/mcc.log`
- [ ] Progress output goes to stderr only

**Dependencies**: 1.1

**Effort**: M

**Verify**: `cargo test` (unit tests for config round-trip and path resolution)

### Task 1.3: Implement `mcc init` environment detection

**Description**: Detect OS, arch, terminal (`TERM_PROGRAM`/`TERM`), login shell (`$SHELL`), the rc file to manage (zsh → `~/.zshrc`; bash → `~/.bash_profile` on macOS, `~/.bashrc` on Linux; fish → `~/.config/fish/config.fish`), the `claude` binary path and version (`MCC_CLAUDE_BIN` override), and whether `~/.local/bin` is on `PATH`. Save to `config.toml`; auto-run on first use of any command. Options `--force`, `--shell <zsh|bash|fish>`. Files: `src/env_detect.rs`, `src/commands/init.rs`.

**Acceptance Criteria**:
- [ ] `mcc init` prints a detection report and writes it to `config.toml`
- [ ] Any other command auto-runs init when `config.toml` is missing
- [ ] `--shell` overrides the detected shell and rc file
- [ ] A missing `claude` binary is reported with an install hint, not a crash

**Dependencies**: 1.2

**Effort**: M

**Verify**: `cargo test` (fake env vars + temp HOME)

### Task 1.4: Implement `mcc add` with validation, clone/fresh, launcher, and login

**Description**: If `~/.claude` is missing, create the default profile by running `claude auth login`. Otherwise prompt for or accept an alias; validate it (`^[a-z][a-z0-9_-]{0,31}$`; reject `claude`/`mcc`/`default`, shell reserved words and builtins, existing mcc profiles, executables on `PATH`, aliases/functions in rc files, anything `$SHELL -ic 'type <name>'` resolves with a 3s timeout, and existing target dir/wrapper). Create `~/.mcc/profiles/<alias>/`; `--clone` copies the whitelist (settings, CLAUDE.md, agents, skills, commands, hooks, output-styles, plugin manifests/marketplaces/cache, statusline.conf, `mcpServers` from `~/.claude.json`) with no auth or identity, preserving symlinks and rewriting `~/.claude` paths to the profile dir; `--fresh` creates an empty dir. Write the `~/.local/bin/<alias>` wrapper and the `# >>> mcc:<alias> >>>` rc block, then run `CLAUDE_CONFIG_DIR=… claude auth login` and verify with `claude auth status`. Steps shown as `[n/7]`; roll back on failure. Options `--clone`, `--fresh`, `--no-login`, `-y`. Files: `src/validate.rs`, `src/copy.rs`, `src/shell.rs`, `src/claude.rs`, `src/commands/add.rs`.

**Acceptance Criteria**:
- [ ] `mcc add work --clone` creates the profile dir, the wrapper, and the rc block, then triggers login
- [ ] Cloned profiles contain no credentials, `oauthAccount`, or `userID`
- [ ] `~/.claude/...` paths in cloned JSON/conf files point to the profile dir
- [ ] Conflicting or invalid aliases are rejected with exit 2 and the reason
- [ ] A failure mid-way rolls back the dir, wrapper, and rc block
- [ ] Without a TTY, a missing alias or clone/fresh choice exits 2

**Dependencies**: 1.3

**Effort**: L

**Verify**: `cargo test` (fake `claude` binary + temp HOME, including rejection cases)

## Phase P2 — Complete

**Goal:** full feature set — list, sync, delete, help — with robust errors · **Milestone M2:** all commands work end to end in a sandboxed HOME and the integration suite passes

### Task 2.1: Implement `mcc list` with profile metrics

**Description**: For the default profile and every registered profile, collect: command, account email (from `.claude.json` `oauthAccount`), counts of skills, agents, commands, plugins (enabled/installed), MCP servers, hooks, projects, directory size, last used, last sync. Render a table (default), `-l/--long` per-profile cards, or `--json` to stdout. Files: `src/metrics.rs`, `src/commands/list.rs`.

**Acceptance Criteria**:
- [ ] The default profile is always the first row
- [ ] Counts match fixture profiles exactly
- [ ] `mcc list --json` output parses as JSON and contains every metric
- [ ] Missing or corrupt profile dirs are shown with a warning, not a crash

**Dependencies**: 1.4

**Effort**: M

**Verify**: `cargo test` (fixture profiles; JSON parse)

### Task 2.2: Implement `mcc sync` from default to profiles

**Description**: One-way sync of the whitelist from the default profile to `<alias>` or `--all`. Compute a plan (added/updated/removed/unchanged), show it, and apply it with a progress bar; `--dry-run` prints the plan only; `--prune` removes profile items absent from the default; `--only <items>` limits scope. Back up `settings.json` and `.claude.json` to `<profile>/.mcc-backup/<ts>/` before writing; merge only `mcpServers` into the profile's `.claude.json`; rewrite paths; record last sync. Files: `src/commands/sync.rs` (reuses `src/copy.rs`).

**Acceptance Criteria**:
- [ ] After sync, whitelisted items in the profile match the default (with paths rewritten)
- [ ] `--dry-run` changes nothing on disk
- [ ] `--prune` removes only whitelisted items absent from the default
- [ ] Profile auth and identity fields are never touched
- [ ] A backup is written before any overwrite

**Dependencies**: 1.4

**Effort**: M

**Verify**: `cargo test` (before/after fixture comparison; dry-run tree unchanged)

### Task 2.3: Implement `mcc delete` with guardrails

**Description**: Refuse `default`. Show the profile size, require typing the alias to confirm (or `-y`), run `CLAUDE_CONFIG_DIR=… claude auth logout` (skip with `--keep-auth`), remove the wrapper only if it carries the mcc marker, remove the rc block, the directory, and the registry entry, with step-by-step progress. Files: `src/commands/delete.rs`.

**Acceptance Criteria**:
- [ ] All profile artifacts (dir, wrapper, rc block, registry entry) are removed
- [ ] `mcc delete default` is refused with exit 2
- [ ] Without a TTY and without `--yes`, it exits 2 and deletes nothing
- [ ] A wrapper without the mcc marker is left untouched with a warning

**Dependencies**: 1.4

**Effort**: S

**Verify**: `cargo test`

### Task 2.4: Polish help, errors, non-TTY behavior, and Ctrl+C handling

**Description**: Add a help guide with examples (`mcc help`, long `about` text), make every error print what went wrong and how to fix it on stderr, make non-TTY runs require flags, and map Ctrl+C to exit 130. Files: `src/cli.rs`, `tests/`.

**Acceptance Criteria**:
- [ ] `mcc help` and `mcc help <cmd>` show usage with examples
- [ ] Every error path writes to stderr with a fix hint
- [ ] Ctrl+C during a prompt exits 130
- [ ] The full integration suite passes

**Dependencies**: 2.1, 2.2, 2.3

**Effort**: M

**Verify**: `cargo test`

## Phase P3 — Polish

**Goal:** distribution and developer ergonomics · **Milestone M3:** a new user can install mcc from the README and get shell completions

### Task 3.1: Add shell completions

**Description**: Add `mcc completions <bash|zsh|fish>` using clap_complete, printing the script to stdout. Files: `src/cli.rs`, `src/commands/completions.rs`.

**Acceptance Criteria**:
- [ ] Each supported shell prints a non-empty completion script and exits 0
- [ ] An unsupported shell exits 2

**Dependencies**: 2.4

**Effort**: S

**Verify**: `cargo test`

### Task 3.2: Write the README and install script

**Description**: Write a README (features, install, usage, how profiles/sync work, safety notes) and an `install.sh` that builds/installs via `cargo install --path .`. Files: `README.md`, `install.sh`.

**Acceptance Criteria**:
- [ ] `cargo install --path . --root <tmp>` succeeds
- [ ] `install.sh` passes shellcheck
- [ ] The README covers every command with an example

**Dependencies**: 2.4

**Effort**: S

**Verify**: `cargo install --path . --root "$(mktemp -d)"` and `shellcheck install.sh`

## Dependency table

| Task | Depends on | Blocks | Wave |
|---|---|---|---|
| 1.1 | — | 1.2 | 1 |
| 1.2 | 1.1 | 1.3 | 2 |
| 1.3 | 1.2 | 1.4 | 3 |
| 1.4 | 1.3 | 2.1, 2.2, 2.3 | 4 |
| 2.1 | 1.4 | 2.4 | 5 |
| 2.2 | 1.4 | 2.4 | 5 |
| 2.3 | 1.4 | 2.4 | 5 |
| 2.4 | 2.1, 2.2, 2.3 | 3.1, 3.2 | 6 |
| 3.1 | 2.4 | — | 7 |
| 3.2 | 2.4 | — | 7 |

## Milestones

| ID | Phase | Exit condition | Verify with |
|---|---|---|---|
| M1 | P1 | `mcc add work --clone` creates a working profile in a sandboxed HOME | `cargo test` |
| M2 | P2 | All commands work end to end in a sandboxed HOME | `cargo test` |
| M3 | P3 | A new user can install from the README and get completions | `cargo install --path . --root <tmp>` |

**Critical path:** 1.1 → 1.2 → 1.3 → 1.4 → 2.2 → 2.4 → 3.2
