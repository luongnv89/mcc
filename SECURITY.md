# Security Policy

## Supported versions

mcc is pre-1.0 (currently 0.9.0). Security fixes are made on `main` and shipped in the next release; only the latest release and `main` are supported.

| Version | Supported |
|---|---|
| latest release / `main` | yes |
| older releases | no |

## Reporting a vulnerability

**Please do not open a public GitHub issue, discussion or pull request for a security vulnerability.**

Report it privately instead:

1. **Preferred:** use GitHub private vulnerability reporting at <https://github.com/luongnv89/mcc/security/advisories/new>.
2. **Alternatively:** email the maintainer at contact@luongnv.com with the subject prefix `[mcc security]`.

### What to include

- A description of the vulnerability and its impact
- The mcc version (`mcc --version`) or commit, your OS and your shell
- Step-by-step instructions to reproduce it, or a proof of concept
- The affected command, file or source location, if you know it
- Any suggested fix or mitigation

Never include a real access token or credentials in your report. Use placeholder values.

### What to expect

mcc is maintained by volunteers, so responses are best effort:

- An acknowledgment within about 7 days
- An assessment and, if the report is confirmed, a plan for a fix
- A fix released as soon as practical, with credit in the advisory if you want it

Please give us a reasonable amount of time to fix the issue before you disclose it publicly.

## Scope

mcc manages Claude Code profiles on your machine, so it handles sensitive local data:

- **Credentials.** By default mcc reads no token: the usage in `mcc list`, `mcc usage` and `mcc run` comes from the rate limits the opt-in `mcc statusline` shim caches in `~/.mcc/state/<profile>/usage.json` from what Claude Code passes it. To warn before a login runs out, `mcc usage` reads one timestamp from each profile's `<config dir>/.credentials.json`, the login deadline (`refreshTokenExpiresAt`), into a structure that has no token field, and sends nothing over the network; it reads that deadline from the macOS Keychain only with `live = true` or `--all` (below), and reads nothing with `MCC_NO_USAGE` set. `mcc login PROFILE` (alias `mcc renew`), which you run to renew a login, reads the same deadline from the file and, on macOS, the Keychain (whatever `MCC_NO_USAGE` says) before and after it renews the login, and with `--refresh-only` also the access token's expiry (`expiresAt`): timestamps only, never a token. Only with `live = true` under `[usage]` in `~/.mcc/config.toml` (off by default), or for one run of `mcc usage --all` (every profile, the default included; it prints a risk notice each time), does mcc read each profile's Claude Code OAuth access token, from `<config dir>/.credentials.json` or, on macOS, from the login Keychain item Claude Code created for that config dir: with `live = true`, for the usage in those commands and in `mcc doctor` to check whether it has expired; with `--all`, for that one `mcc usage` run, which saves only the percentages and reset times it finds (never the token) to that same `usage.json` cache. The access token is kept in memory only, is never printed, logged or written anywhere, and is sent only to Anthropic's usage endpoint (`https://api.anthropic.com/api/oauth/usage`) for its own profile, or to the `MCC_USAGE_URL` test override (accepted only for `https://` URLs or `http://` on loopback), with no redirects followed and no proxy taken from the environment. mcc itself never refreshes a token (it never sends a refresh token or calls the OAuth endpoint): to renew a login, `mcc login` runs `claude auth login` for the profile, and `mcc login --refresh-only` runs one isolated `claude -p` request (haiku, one turn, no tools, MCP servers or hooks, only the user's own settings, in an empty directory, with credential variables removed, stopped after 2 minutes) so that Claude Code refreshes its own token. `mcc usage --all` makes the same request, announced on stderr first, for each account whose access token it finds expired: in the first existing project mapped to that profile with `mcc project set`, where Claude Code sees the project's git status and `CLAUDE.md` as in any session there, or, for a profile no project maps to, in a new empty folder. It starts none for a profile whose login has passed its deadline or whose refresh failed in the last 30 minutes, and spends at most 3 minutes on refreshes per run. Both commands hold `~/.mcc/state/<profile>/refresh.lock` while the request runs, so two never refresh one profile at once.
- **Files it writes.** Profile directories under `~/.mcc/profiles/`, launchers in `~/.local/bin/`, a managed alias block in your shell rc file, and its own state, logs and backups under `~/.mcc/`. With auto sync (`[sync] auto`, on by default), starting a profile from its launcher (which calls the hidden `mcc launch-sync <alias>`) or with `mcc run` writes to profiles without any other command: mcc copies what the default profile added or updated into that profile and the others the default changed for, after backing up what it replaces to `<profile>/.mcc-backup/<timestamp>-auto/` (it keeps the 10 latest of these per profile and removes older ones; backups of `mcc sync` and `mcc link` are never removed), and records each profile's state in `~/.mcc/state/<alias>/auto-sync.json`. For a profile that opts in to session sharing (`mcc share`), mcc moves its session transcripts into its group's store, `~/.mcc/shared/projects` or `~/.mcc/shared/groups/<name>/projects`, and replaces its `projects/` directory with a link there. `mcc statusline install` and `uninstall` (opt-in) change the `statusLine` in your default profile's `~/.claude/settings.json` (or the file it links to), after copying it to `~/.mcc/backups/`. `mcc login --refresh-only`, and `mcc usage --all` when it refreshes the token of a profile no project maps to, also create an empty, private folder (mode 0700) in the system's temporary directory for the one `claude -p` request and remove it when the request ends. Both write the lock file `~/.mcc/state/<profile>/refresh.lock`, and `mcc usage --all` records a failed refresh's time in `~/.mcc/state/<profile>/refresh-failed-at` (removed by a refresh that works).

Examples of issues that are in scope:

- Leaking an access token (to logs, output, files, or any host other than the ones above)
- Path traversal or writes outside the intended locations through profile aliases, sync paths or config values
- Unsafe file permissions on files that hold credentials or account data
- Shell injection through the rc file alias block or the generated launchers
- Deleting or overwriting files mcc did not create
- The install script installing a binary that doesn't match its release's SHA-256 checksum, downloading over anything but HTTPS, or building a directory that isn't its own clone

Out of scope: vulnerabilities in Claude Code itself or in Anthropic's services (report those to Anthropic), and issues that require an attacker who already controls your user account.
