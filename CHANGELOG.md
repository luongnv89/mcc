# Changelog

All notable changes to mcc are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and mcc uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The same history is published at <https://luongnv89.github.io/mcc/changelog.html>.

## [Unreleased]

### Added

- Per-project profiles. `mcc project set PROFILE` makes PROFILE the one `mcc run` starts in the current git repository (or directory, or `--path DIR`). `--allow NAME` (repeatable or comma-separated) names the other profiles `mcc run` may offer when PROFILE is at its limit; setting the project again without it clears the list. `mcc project unset` and `mcc project list [--json]` manage them. They are kept as `[projects."<dir>"]` tables in `~/.mcc/config.toml`. `mcc rename` updates them, and `mcc delete` removes the ones naming the deleted profile and takes it off the allow lists.
- `mcc run [-- CLAUDE_ARGS]` starts Claude Code with the current project's default profile, or the default profile outside every project. mcc never switches accounts on its own. When that profile is at its 5-hour or weekly limit, `mcc run` asks what to do: waiting (starting nothing) is the default, starting it anyway comes next, and other profiles are offered only when the project allows them, by name and without their usage. Only that profile's usage is fetched. Without a terminal it stops (exit 2). `--profile NAME` starts NAME without checking usage. Starting a profile other than the project's, from the prompt or with `--profile`, prints a terms-of-service note on stderr. `--dry-run` prints the profile it would start. If the profile's usage can't be read, it starts. ([#31](https://github.com/luongnv89/mcc/issues/31))
- `mcc list` shows the project defaults under the profile table (project, profile and account, with `← here` for the current directory). `--long` cards have a `Default for` row, and `--json` has a `project_defaults` array per profile.
- `project` and `run` are reserved and can't be used as aliases.
- A GitHub Actions CI workflow runs `cargo test` on Linux and macOS, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `shellcheck install.sh` and `brew audit` on the Homebrew formula for every push and pull request. ([#32](https://github.com/luongnv89/mcc/issues/32))

### Fixed

- The `mcc project` and `mcc list` tests expected absolute project paths, but mcc shows a project under `$HOME` as `~/…`; they now expect what mcc prints, so they pass on Linux as well as macOS. ([#32](https://github.com/luongnv89/mcc/issues/32))
- `mcc sync` backs up every file or folder it overwrites or removes (such as `CLAUDE.md`, skills and agents) to `<profile>/.mcc-backup/<timestamp>/`, not only `settings.json`, `settings.local.json` and `.claude.json`, so edits made in a profile can be restored. A symlink is backed up as the link itself. `sync.extra` entries that are empty, `.` or inside `.mcc-backup` are rejected. ([#30](https://github.com/luongnv89/mcc/issues/30))

## [0.3.0] - 2026-10-13

Profiles can now opt in to session sharing with `mcc share ALIAS` or `ALIAS --mcc-share`, so `--resume` lists the sessions of every sharing profile, and `mcc refresh-launchers` updates launchers and shell aliases made by an older mcc.

### Added

- Opt-in session sharing between profiles, off by default. `mcc share ALIAS` (or starting a profile with `ALIAS --mcc-share`) links the profile's `projects/` to `~/.mcc/shared/projects`, so `--resume` lists the sessions of every sharing profile. `mcc share ALIAS --off` gives the profile its own copy again. The setting is `share_sessions` under `[profiles.ALIAS]` in `~/.mcc/config.toml`. Only sessions are shared: never credentials, the account, history or live session state. Every time sharing is turned on, mcc prints a note about Anthropic's Consumer Terms and Usage Policy. `mcc list` marks sharing profiles, and `--json` has `share_sessions` and `launcher_current`. ([#25](https://github.com/luongnv89/mcc/issues/25), [#26](https://github.com/luongnv89/mcc/pull/26))
- `mcc refresh-launchers [ALIAS...]` rewrites launchers and shell aliases from the current templates. ([#25](https://github.com/luongnv89/mcc/issues/25), [#26](https://github.com/luongnv89/mcc/pull/26))

### Changed

- New launchers handle `--mcc-share`, and new shell aliases run the launcher instead of calling `claude` directly. `CLAUDE_CONFIG_DIR` is unchanged, so logins are kept. **Upgrade note:** existing launchers and aliases are not modified; run `mcc refresh-launchers` once to use `ALIAS --mcc-share` with profiles created before this release. ([#25](https://github.com/luongnv89/mcc/issues/25), [#26](https://github.com/luongnv89/mcc/pull/26))
- `mcc list` prints a hint to run `mcc refresh-launchers` while a launcher predates session sharing. ([#25](https://github.com/luongnv89/mcc/issues/25), [#26](https://github.com/luongnv89/mcc/pull/26))
- `share` and `refresh-launchers` are reserved and can't be used as aliases. ([#25](https://github.com/luongnv89/mcc/issues/25), [#26](https://github.com/luongnv89/mcc/pull/26))

## [0.2.0] - 2026-10-13

`mcc usage` is now a compact table that hides account emails by default, and `mcc rename` renames a profile and its command while keeping its login.

### Added

- `mcc rename ALIAS NEW` (alias `mv`): renames a profile and its command. It installs the new launcher and alias block, renames the registry entry, then removes the old launcher and alias. The new name is checked like an alias in `mcc add`, and a failure rolls the changes back. The profile directory, and so its login, is kept. ([#24](https://github.com/luongnv89/mcc/issues/24), [#23](https://github.com/luongnv89/mcc/pull/23))
- `[usage] show_email` in `~/.mcc/config.toml`, plus `--show-email` / `--hide-email` on `mcc usage`, to choose whether account emails are shown. ([#24](https://github.com/luongnv89/mcc/issues/24), [#23](https://github.com/luongnv89/mcc/pull/23))

### Changed

- `mcc usage` shows a compact table, one row per profile, instead of a block per profile: `PROFILE`, then the bar and percent used, time until reset and reset time for the 5-hour and weekly windows. On narrow terminals the reset time goes first, then bars shrink 10 → 5 → 0. ([#24](https://github.com/luongnv89/mcc/issues/24), [#23](https://github.com/luongnv89/mcc/pull/23))
- `mcc usage` hides account emails by default, in the table and in `--json` (`account` is `null`). Use `--show-email` or `[usage] show_email = true` to show them. ([#24](https://github.com/luongnv89/mcc/issues/24), [#23](https://github.com/luongnv89/mcc/pull/23))
- `mcc add <old name>` names the renamed profile that still uses that name's directory, instead of only saying that the directory exists. ([#24](https://github.com/luongnv89/mcc/issues/24), [#23](https://github.com/luongnv89/mcc/pull/23))

### Fixed

- `mcc delete <old name>` refuses to delete a directory that a renamed profile still uses. ([#24](https://github.com/luongnv89/mcc/issues/24), [#23](https://github.com/luongnv89/mcc/pull/23))

## [0.1.0] - 2026-10-10

The first version of mcc.

### Added

- Homebrew formula for macOS and Linux: `brew install luongnv89/tap/mcc` builds mcc from source and installs bash, zsh and fish completions. `brew upgrade mcc` and `brew uninstall mcc` work as usual. The formula lives in `packaging/homebrew/mcc.rb`. ([#14](https://github.com/luongnv89/mcc/issues/14))
- `mcc usage [PROFILE] [--json]`: every profile's 5-hour and weekly usage in detail, with a bar, percent used and left, time until reset, and the reset time in local time. `default` or `claude` selects `~/.claude`; an unknown name exits 2. ([#16](https://github.com/luongnv89/mcc/issues/16), [#20](https://github.com/luongnv89/mcc/pull/20))
- Usage meters in `mcc list`: a coloured bar (green, yellow at 80%+, red at 95%+) and colour-independent `!` / `!!` markers, sized to the terminal width. ([#17](https://github.com/luongnv89/mcc/issues/17), [#20](https://github.com/luongnv89/mcc/pull/20))
- 5-hour and weekly usage in `mcc list`: new `5H` and `WEEK` columns, lines in `--long` cards, and a `usage` object in `--json`, for every profile including the default. Each profile's own login is used; failures show `unavailable` with a reason and never change the exit status. ([#13](https://github.com/luongnv89/mcc/issues/13), [#15](https://github.com/luongnv89/mcc/pull/15))
- `--no-usage` and `MCC_NO_USAGE` to skip the usage fetch, and the `MCC_USAGE_URL` / `MCC_SECURITY_BIN` test overrides. ([#15](https://github.com/luongnv89/mcc/pull/15))
- `mcc completions` for bash, zsh, fish, elvish and PowerShell, plus `install.sh`. ([#12](https://github.com/luongnv89/mcc/pull/12))
- `mcc list` (table, `--long` cards, `--json`), `mcc sync` (plan, backup, apply; `--dry-run`, `--prune`, `--only`, `--all`) and `mcc delete` (type-to-confirm or `--yes`, logout, removes only what mcc created). ([#12](https://github.com/luongnv89/mcc/pull/12))
- `mcc init` (OS, terminal, shell, rc file and `claude` detection, run automatically on first use) and `mcc add` (alias validation, clone with path rewriting, launcher, managed rc block, login, rollback on failure). ([#12](https://github.com/luongnv89/mcc/pull/12))
- Apache-2.0 license, contributing guide, code of conduct, security policy, and issue and pull request templates. ([#18](https://github.com/luongnv89/mcc/issues/18), [#19](https://github.com/luongnv89/mcc/pull/19))

### Fixed

- `mcc usage` keeps the full 20-cell bar at 80 columns: it drops the absolute reset time before shrinking the bar, and shrinks in the same 20/10/0 steps as `mcc list --long`. ([#21](https://github.com/luongnv89/mcc/pull/21))
- Relative symlinks (such as `skills/x -> ../../.agents/skills/x`) are re-resolved from their source location, so cloned profiles don't get broken links. ([#12](https://github.com/luongnv89/mcc/pull/12))

### Security

- Access tokens are kept in memory only, sent only to the usage endpoint for their own profile, and never printed, logged or written to disk. Expired tokens are never sent. ([#15](https://github.com/luongnv89/mcc/pull/15))
- GitHub private vulnerability reporting is enabled, and `contact@luongnv.com` is listed as the security contact. ([#21](https://github.com/luongnv89/mcc/pull/21))

[Unreleased]: https://github.com/luongnv89/mcc/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/luongnv89/mcc/releases/tag/v0.3.0
[0.2.0]: https://github.com/luongnv89/mcc/releases/tag/v0.2.0
[0.1.0]: https://github.com/luongnv89/mcc/releases/tag/v0.1.0
