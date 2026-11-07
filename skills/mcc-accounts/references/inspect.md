# Inspect: fields, codes, and manual resolution

Read this when you need a JSON field, a `reason` code, an exit code, or when `scripts/mcc_whoami.py` cannot run. Every command here is read-only.

## JSON shapes (schema 2)

`--json-v2` wraps the same objects as `--json`: `{"schema": 2, "profiles": [...]}` for `list` and `usage`, `"projects"` for `project list`, `"checks"` for `doctor`. Within a schema, fields are only ever added. Check `.schema == 2` first; if it differs, stop and tell the user that the skill needs an update.

### `mcc list --json-v2` → `profiles[]`

| Field | Meaning |
| --- | --- |
| `name`, `command` | Profile name and the command that starts it (`claude` for `default`) |
| `dir` | Config dir, abbreviated with `~` (= `$MCC_HOME`, else `$HOME`). Expand it before comparing paths |
| `is_default` | `true` only for `~/.claude` itself |
| `is_global_default` | `true` for the profile `mcc default` chose (mcc 0.7.0+). Plain `claude` in new shells starts it |
| `account` | Email; `null` with `--hide-email`. `[privacy] show_email = true` shows it unless you pass `--hide-email` |
| `organization`, `organization_uuid`, `label`, `color` | Who the account belongs to; `label` is the user's own name for it |
| `project_defaults` | Directories whose `mcc run` starts this profile |
| `share_sessions`, `share_group` | Session sharing (`mcc share`) |
| `sync` | `{behind, local_edits, conflicts}` against the default; `null` for `default` |
| `launcher_current` | `false` → suggest `mcc refresh-launchers` |
| `usage` | See below |

`mcc list` measures every profile on disk, so it can take several seconds. `mcc usage --json-v2` is fast but has no `dir`.

### `usage` object (list and usage)

`{"status": "ok"|"unavailable", "reason", "five_hour": {"percent", "resets_at"}, "weekly": {...}, "source": "cache"|"live"|null, "updated_at"}`

- 100% in either window means the account is at its limit. `!` in tables marks 80%+, `!!` marks 95%+.
- A window whose reset time has passed is dropped (`null`). `null` is unknown, not 0%.
- `source: cache` comes from the opt-in `mcc statusline` cache. Report `updated_at` as the age. Team and Enterprise seats may report no rate limits at all.

| `reason` | Meaning |
| --- | --- |
| `no_data` | Nothing current cached; the user can run `/usage` in Claude Code as that account |
| `disabled` | `--no-usage` or `MCC_NO_USAGE` |
| `not_logged_in` | No login found (live lookup only) |
| `token_expired` | Login expired or rejected (live lookup only) |
| `unauthorized`, `rate_limited`, `network`, `http_error`, `parse_error` | Live lookup failures |

### `mcc usage --json-v2` extra fields

mcc 0.6.0+: `last_activity`, `login_expires_at`, `login_expires_source` (`credentials_file`, `keychain`, or `null`), and `login_warning`. `login_warning: true` means the login ends inside the warning window (2 days by default). The fix is `mcc login <profile>`, which the user runs, because it opens a browser. `login_expires_at: null` is unknown: on macOS, plain `mcc usage` cannot read a Keychain-only login.

### `mcc project list --json-v2` → `projects[]`

`{"path", "profile", "allow"}`. `path` covers everything below it; the deepest matching path wins. `allow` lists the profiles `mcc run` may offer at a limit.

### `mcc doctor --json-v2` → `checks[]`

`{"profile", "check", "status": "ok"|"warn"|"problem"|"skipped", "message", "fix"}`. `profile` is `null` for shared checks. Exit 1 means at least one `problem`. Report every `problem` with its `fix`.

## `mcc run --dry-run`

Prints the profile `mcc run` would start on stdout, and the reason on stderr. At a limit, stderr says so, and names the reset time and the project's allowed profiles. It starts nothing and exits 0.

## Resolve the session account by hand (no python3)

1. Check that mcc is set up: `~/.mcc/config.toml` (`$MCC_HOME/.mcc/config.toml` when `MCC_HOME` is set) must exist, load, and have its `[env]` table. Run `mcc doctor --json-v2`, which is read-only and never sets mcc up. Its exit code says nothing about setup (exit 1 means any `problem`), so read the `init` check whose `profile` is `null`. If that check is not `ok`, mcc is not set up: hand off `mcc init` and stop. If doctor prints no JSON, config.toml does not load (stderr names the error): hand off fixing it by hand, or moving it away and running `mcc init --force`, and stop. Either way, never run `mcc list` here: it would set mcc up.
2. Run `echo "${CLAUDE_CONFIG_DIR:-unset}"`.
3. If it prints `unset`, the session account is `default`. Skip to step 7.
4. Run `mcc list --json-v2 --no-usage --hide-email`.
5. For each profile, replace a leading `~` in `dir` with `$MCC_HOME` if set, else `$HOME`. Compare each expanded `dir` with the value from step 2, after resolving symlinks (`cd <dir> && pwd -P`). If one matches, that profile is the session account. If none matches, report that this config dir is not an mcc profile.
6. If `CLAUDECODE` is not `1`, call the result "this shell's account": no Claude Code session is involved.
7. List credential variables by name only: `env | cut -d= -f1 | grep -E '^(ANTHROPIC_API_KEY|ANTHROPIC_AUTH_TOKEN|ANTHROPIC_BASE_URL|CLAUDE_CODE_OAUTH_TOKEN.*|CLAUDE_CODE_USE_BEDROCK|CLAUDE_CODE_USE_VERTEX)$'`.

## Exit codes (every mcc command)

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Runtime error, or `mcc doctor` found a problem |
| 2 | Usage error: bad arguments, unknown profile, confirmation needed without a terminal, `mcc run` at a limit without a terminal |
| 3 | Invalid input: a corrupt `~/.mcc/config.toml` or JSON file (run `mcc doctor`), or a request mcc refuses, such as sharing `default` |
| 130 | Interrupted |

## jq recipes

```sh
mcc list --json-v2 --hide-email | jq -e '.schema == 2' >/dev/null && \
  mcc list --json-v2 --hide-email | jq '.profiles[] | {name, organization, label, five_hour: .usage.five_hour.percent, weekly: .usage.weekly.percent}'
mcc usage --json-v2 --hide-email | jq '.profiles[] | select(.login_warning) | .name'
mcc project list --json-v2 | jq '.projects[] | {path, profile, allow}'
```
