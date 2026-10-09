# mcc — Implementation Plan

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
- **Clone/sync whitelist** (configurable): `settings.json`, `settings.local.json`, `CLAUDE.md`, `keybindings.json`, `statusline.conf`, `agents/`, `skills/`, `commands/`, `hooks/`, `output-styles/`, `plugins/{installed_plugins.json,known_marketplaces.json,marketplaces/,cache/}`, plus `mcpServers` merged from `~/.claude.json`. Never copies credentials, `oauthAccount`, `userID`, history, projects, sessions, telemetry, caches. Symlinks preserved. `~/.claude/…`, `$HOME/.claude/…`, `<abs home>/.claude/…` rewritten to the profile dir in JSON/conf files.
- **add**: numbered steps `[n/7]` (validate → create dir → clone/fresh → rewrite paths → wrapper → rc block → `claude auth login` + `claude auth status` verification), rollback on failure.
- **sync**: plan (added/updated/removed/unchanged), progress bar, backups of `settings.json`/`.claude.json` to `<profile>/.mcc-backup/<ts>/`, last-sync recorded.
- **delete**: refuses `default`; shows size; type-to-confirm; `claude auth logout` for the profile (clears keychain entry); removes wrapper (only if mcc-marked), rc block, directory, registry entry.
- **list**: NAME · COMMAND · ACCOUNT · SKILLS · AGENTS · COMMANDS · PLUGINS (enabled/installed) · MCP · HOOKS · PROJECTS · SIZE · LAST USED · LAST SYNC.
- **I/O**: data → stdout; progress/logs/prompts/errors → stderr; also logged to `~/.mcc/logs/mcc.log`. Non-TTY requires flags (exit 2 otherwise).
- **Config precedence**: CLI args > env (`MCC_HOME`, `MCC_CLAUDE_BIN`) > `~/.mcc/config.toml` > defaults.
- **Exit codes**: 0 ok · 1 runtime · 2 usage/invalid alias · 3 bad config/input · 130 Ctrl+C.
- **Test safety**: all tests/demos use `MCC_HOME=<tempdir>` and a fake `claude` binary; never the real `~/.claude`, rc files, or keychain.

## Phase 1 — Foundation

### 1.1 Scaffold, command tree, global flags
- **Goal**: Cargo project with clap command tree, global flags, color handling, and exit-code mapping.
- **Files**: `Cargo.toml`, `src/main.rs`, `src/cli.rs`, `src/ui.rs`, `src/error.rs`
- **Expected behavior**: `mcc --help`, `mcc --version`, and `--help` on every subcommand work.
- **Test**: assert_cmd tests for help/version at every level.
- **Effort**: S

### 1.2 Paths, registry, logger, progress reporter
- **Goal**: Resolve all paths from `MCC_HOME`/`HOME`, persist `config.toml` registry, file logger, step/progress reporter.
- **Files**: `src/paths.rs`, `src/config.rs`, `src/log.rs`
- **Expected behavior**: state persisted under `~/.mcc`; logs written to `~/.mcc/logs/mcc.log`.
- **Test**: unit tests for config round-trip and path resolution.
- **Effort**: M

### 1.3 `init` — environment detection
- **Goal**: Detect OS, arch, terminal, shell, rc file, claude binary + version, `~/.local/bin` on PATH; auto-run on first use.
- **Files**: `src/env_detect.rs`, `src/commands/init.rs`
- **Expected behavior**: prints a detection report and saves it to `config.toml`.
- **Test**: tests with fake env vars and temp HOME.
- **Effort**: M

### 1.4 `add` — core command
- **Goal**: Alias validation, clone/fresh whitelist copy, path rewrite, wrapper, rc block, auth login, rollback; default-profile case.
- **Files**: `src/validate.rs`, `src/copy.rs`, `src/shell.rs`, `src/claude.rs`, `src/commands/add.rs`
- **Expected behavior**: `mcc add work --clone` creates the profile dir, wrapper, and rc block, then triggers login.
- **Test**: fake `claude` + temp HOME; rejection cases for invalid/conflicting aliases.
- **Effort**: L

## Phase 2 — Complete

### 2.1 `list` — metrics
- **Goal**: Collect per-profile metrics; render table, `--long` cards, `--json`.
- **Files**: `src/metrics.rs`, `src/commands/list.rs`
- **Expected behavior**: accurate counts per profile; pipeable JSON.
- **Test**: fixture profiles; JSON parses.
- **Effort**: M

### 2.2 `sync` — default → profile
- **Goal**: Plan/diff, `--dry-run`, `--prune`, `--only`, `--all`, backups, `mcpServers` merge.
- **Files**: `src/commands/sync.rs` (reuses `src/copy.rs`)
- **Expected behavior**: profile matches default after sync; dry-run changes nothing.
- **Test**: before/after fixture comparison; dry-run leaves tree untouched.
- **Effort**: M

### 2.3 `delete` — remove a profile
- **Goal**: Guardrails, confirmation, logout, wrapper/rc/dir/registry removal.
- **Files**: `src/commands/delete.rs`
- **Expected behavior**: everything belonging to the profile is removed.
- **Test**: all artifacts gone; `default` refused; non-TTY without `--yes` exits 2.
- **Effort**: S

### 2.4 `help`, error polish, non-TTY, Ctrl+C
- **Goal**: Help with examples, errors to stderr with fix hints, non-TTY behavior, Ctrl+C → 130.
- **Files**: `src/cli.rs`, `tests/`
- **Expected behavior**: every error path prints what went wrong and how to fix it.
- **Test**: full integration suite green.
- **Effort**: M

## Phase 3 — Polish

### 3.1 Shell completions
- **Goal**: `mcc completions <bash|zsh|fish>` via clap_complete.
- **Files**: `src/cli.rs`, `src/commands/completions.rs`
- **Expected behavior**: completion script printed to stdout.
- **Test**: each shell's output is non-empty and exits 0.
- **Effort**: S

### 3.2 Distribution and docs
- **Goal**: README, `install.sh`, `cargo install --path .` instructions.
- **Files**: `README.md`, `install.sh`
- **Expected behavior**: a new user can install and run mcc from the README.
- **Test**: `cargo install --path .` into a temp root succeeds; `install.sh` shellcheck-clean.
- **Effort**: S
