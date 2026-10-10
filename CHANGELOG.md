# Changelog

All notable changes to mcc are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and mcc uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The same history is published at <https://luongnv89.github.io/mcc/changelog.html>.

## [Unreleased]

## [0.1.0] - not yet tagged

The first version of mcc. Everything below is on `main`; no release tag has been cut yet.

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

[Unreleased]: https://github.com/luongnv89/mcc/commits/main
[0.1.0]: https://github.com/luongnv89/mcc/commits/main
