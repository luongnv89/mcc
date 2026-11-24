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
| `is_global_default` | mcc 0.10.0+: `true` for `default` (`~/.claude`) only, the profile plain `claude` starts. mcc 0.7.0 to 0.9.x: `true` for the profile `mcc default` chose, which plain `claude` in new shells started |
| `account` | Email; `null` with `--hide-email`. `[privacy] show_email = true` shows it unless you pass `--hide-email` |
| `organization`, `organization_uuid`, `label`, `color` | Who the account belongs to; `label` is the user's own name for it |
| `project_defaults` | Directories whose `mcc run` starts this profile |
| `share_sessions`, `share_group` | Session sharing (`mcc share`) |
| `sync` | `{behind, local_edits, conflicts}` against the default; `null` for `default` |
| `launcher_current` | `false` → suggest `mcc refresh-launchers` |
| `usage` | See below |
| `provider` | mcc 0.11.0+: `claude` for a Claude Code profile, `devin` for Devin's row (see below). Missing before 0.11.0 |
| `auth` | mcc 0.12.0+: `"api_key"` for an API-key profile (see below); missing for every other row |

`mcc list` measures every profile on disk, so it can take several seconds. `mcc usage --json-v2` is fast but has no `dir`.

mcc 0.11.0+: when the Devin CLI is signed in, `profiles[]` in `list` and `usage` also holds a row with `provider: "devin"` and `name: "devin"`. It is not an mcc profile or a Claude Code account: its `dir` is Devin's login folder, not a `CLAUDE_CONFIG_DIR`; its Claude Code counts are `null`; and `mcc exec`, `mcc run --profile`, `mcc project set` and `mcc sync` do not take it. Leave it out when you list or resolve Claude Code accounts with `select(.provider != "devin")`, which also keeps every row of an older mcc that has no `provider`. Report its usage as Devin's, apart from the accounts.

### `usage` object (list and usage)

`{"status": "ok"|"unavailable", "reason", "five_hour": {"percent", "resets_at"}, "weekly": {...}, "source": "cache"|"live"|null, "updated_at"}`

- 100% in either window means the account is at its limit. `!` in tables marks 80%+, `!!` marks 95%+.
- A window whose reset time has passed is dropped (`null`). `null` is unknown, not 0%.
- `source: cache` comes from the opt-in `mcc statusline` cache. Report `updated_at` as the age. Team and Enterprise seats may report no rate limits at all.
- Devin's row (mcc 0.11.0+) has `five_hour: null` and may have a `daily` window (same shape as `weekly`; absent when Devin's plan hides it or the saved one has reset) and `overage_usd` (dollars; absent when not reported). Both keys are left out when the row is unavailable. Claude Code profiles' `usage` never has either key.
- An API-key profile's row (`auth: "api_key"`, mcc 0.12.0+) is `unavailable` with `no_data` (or `not_logged_in` with a live lookup) and never has a window: those codes don't mean it needs a login. Its `usage` adds `est_cost_usd` (US dollars spent this UTC month, Claude Code's own list-price estimate for sessions on this machine; absent before the first session is recorded) and `budget_usd` (the monthly budget the user set; absent when none). It is an estimate, not the bill: mcc can't read the Console balance or spend limit.

| `reason` | Meaning |
| --- | --- |
| `no_data` | Nothing current cached; the user can run `/usage` in Claude Code as that account |
| `disabled` | `--no-usage` or `MCC_NO_USAGE` |
| `not_logged_in` | No login found (live lookup only) |
| `token_expired` | Login expired or rejected (live lookup only) |
| `unauthorized`, `rate_limited`, `network`, `http_error`, `parse_error` | Live lookup failures |
| `not_looked_up`, `no_organizations`, `missing_token`, `permission_denied`, `decode_failed`, `missing_quota` | Devin's row only (mcc 0.11.0+). `not_looked_up`: nothing current from an earlier Devin lookup (none saved, or every saved window has reset) and none made now (`[usage] live` is off, or bare `mcc`); `mcc usage --all` looks it up. `no_organizations`: the Devin login has no organization; the user picks one at app.devin.ai. `missing_token`, `decode_failed`: Devin's `credentials.toml` holds no usable token (`decode_failed`: it is not text or can't be read for another reason); the user runs `devin auth login`. `permission_denied`: the user makes Devin's `credentials.toml` readable. `missing_quota`: Devin's API reported neither a daily nor a weekly window; nothing for the user to fix |

### `mcc usage --json-v2` extra fields

mcc 0.6.0+: `last_activity`, `login_expires_at`, `login_expires_source` (`credentials_file`, `keychain`, or `null`), and `login_warning`. `login_warning: true` means the login ends inside the warning window (2 days by default). The fix is `mcc login <profile>`, which the user runs, because it opens a browser. `login_expires_at: null` is unknown: on macOS, plain `mcc usage` cannot read a Keychain-only login.

mcc 0.12.0+, only when other machines are set up under `[machines.NAME]`: a top-level `updated_at` (this machine's own last update) and a `machines` key after `profiles`: `[{"name", "host", "updated_at", "error", "profiles": [...]}]`. `error` is `null` or a code (`unreachable`, `timed_out`, `mcc_not_found`, `outdated`, `incompatible`, `failed`, `no_ssh`, `invalid_config`); one machine's error leaves the others readable. Each profile row, here and under `machines`, then also has `account_id` (a SHA-256 of the account's email and organization, the same on every machine) and `also_on` (the other machines that account is on). Rows under `machines` always have `account: null`. Every row of an account shows its newest numbers from any machine.

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
  mcc list --json-v2 --hide-email | jq '.profiles[] | select(.provider != "devin") | {name, organization, label, five_hour: .usage.five_hour.percent, weekly: .usage.weekly.percent}'
mcc usage --json-v2 --hide-email | jq '.profiles[] | select(.login_warning) | .name'
mcc project list --json-v2 | jq '.projects[] | {path, profile, allow}'
```
