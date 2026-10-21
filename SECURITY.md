# Security Policy

## Supported versions

mcc is pre-1.0 (currently 0.4.0). Security fixes are made on `main` and shipped in the next release; only the latest release and `main` are supported.

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

- **Credentials.** By default mcc reads no login: the usage in `mcc list`, `mcc usage` and `mcc run` comes from the rate limits the opt-in `mcc statusline` shim caches in `~/.mcc/state/<profile>/usage.json` from what Claude Code passes it. Only with `live = true` under `[usage]` in `~/.mcc/config.toml` (off by default) does mcc read each profile's Claude Code OAuth login, from `<config dir>/.credentials.json` or, on macOS, from the login Keychain item Claude Code created for that config dir: for the usage in those commands, and in `mcc doctor` to check whether it has expired. `mcc usage --all` does the same for one run, for every profile (the default included), and prints a risk notice each time. The access token is kept in memory only, is never printed, logged or written anywhere, and is sent only to Anthropic's usage endpoint (`https://api.anthropic.com/api/oauth/usage`) for its own profile, or to the `MCC_USAGE_URL` test override (accepted only for `https://` URLs or `http://` on loopback), with no redirects followed and no proxy taken from the environment. mcc never refreshes tokens.
- **Files it writes.** Profile directories under `~/.mcc/profiles/`, launchers in `~/.local/bin/`, a managed alias block in your shell rc file, and its own state, logs and backups under `~/.mcc/`. For a profile that opts in to session sharing (`mcc share`), mcc moves its session transcripts into its group's store, `~/.mcc/shared/projects` or `~/.mcc/shared/groups/<name>/projects`, and replaces its `projects/` directory with a link there. `mcc statusline install` and `uninstall` (opt-in) change the `statusLine` in your default profile's `~/.claude/settings.json` (or the file it links to), after copying it to `~/.mcc/backups/`.

Examples of issues that are in scope:

- Leaking an access token (to logs, output, files, or any host other than the ones above)
- Path traversal or writes outside the intended locations through profile aliases, sync paths or config values
- Unsafe file permissions on files that hold credentials or account data
- Shell injection through the rc file alias block or the generated launchers
- Deleting or overwriting files mcc did not create
- The install script installing a binary that doesn't match its release's SHA-256 checksum, downloading over anything but HTTPS, or building a directory that isn't its own clone

Out of scope: vulnerabilities in Claude Code itself or in Anthropic's services (report those to Anthropic), and issues that require an attacker who already controls your user account.
