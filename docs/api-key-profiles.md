# API-key profiles

An API-key profile is a Claude Code profile that uses an Anthropic API key
(API usage billing) instead of a Claude subscription login. mcc creates it,
starts it, keeps its key out of your shell's environment, and shows it apart
from the subscription profiles with an estimate of what it spent this month.
This page explains how the key is delivered and stored, and what mcc can and
can't track for such a profile.

Claude Code only. Other harnesses are out of scope.

## Creating one

```console
$ mcc add api --fresh --api-key-stdin --budget 50 < key.txt
$ mcc add api --clone --api-key-prompt
$ mcc add api --clone --api-key-helper "op read op://Private/anthropic/key"
```

Each sets [`apiKeyHelper`](https://code.claude.com/docs/en/settings-reference#apikeyhelper)
in the profile's `settings.json`. Claude Code runs that command to get the
key, so every way of starting the profile uses it: its command (the
launcher), `mcc run`, `mcc exec`, or `CLAUDE_CONFIG_DIR` set by hand. There
is no sign-in: mcc marks Claude Code's first-run setup as done
(`hasCompletedOnboarding` in the profile's `.claude.json`), so the first
start goes straight to the prompt.

The key is never taken as an argument, where it would land in your shell
history and the process list. A value that contains `sk-ant-` is refused as
an alias, and mcc replaces it with `<redacted>` in `~/.mcc/logs/mcc.log`
and with `[redacted]` in its argument-parsing (usage) errors. The
`--api-key-helper` command is redacted from the log too. Other commands'
errors (an unknown profile name, say) may still repeat a mistyped value, so
never paste a key as an argument.

`--budget USD` records a monthly budget to compare the spend with; change or
remove it later with `mcc config set profiles.ALIAS.api_budget_usd 75` (`""`
removes it). It must be a number of US dollars, 0 or more.

The default profile (`~/.claude`) is never an API-key profile: `mcc add`
refuses the API-key flags when it would create the default profile.

## Where the key is kept

There are two modes.

- **Helper mode** (`--api-key-helper CMD`): `apiKeyHelper` is your command,
  for example a password manager's CLI. mcc stores no key, and never runs or
  prints the command.
- **Managed mode** (`--api-key-stdin`, `--api-key-prompt`): mcc reads the key
  from stdin (a pipe; a terminal is refused) or from a hidden prompt (a
  terminal is needed), and writes it **once** to
  `~/.mcc/keys/<profile dir name>-<8 hex>.key`. The 8 hex characters are the
  start of the SHA-256 of the profile's directory, the same way Claude Code
  names a profile's Keychain item. The file is created `0600` (a temporary
  file opened with `O_EXCL`, then renamed into place) in a `0700`
  directory. `apiKeyHelper` is then `cat '<that file>'`. mcc never reads the
  file again: Claude Code does, when it needs the key.

This is the one credential mcc stores. It never reads or moves a Claude
subscription login.

The file is named after the profile's directory, not its alias, so `mcc
rename` keeps it working. `mcc delete` removes it, but only when the
profile's `apiKeyHelper` is exactly the `cat` command mcc wrote for that
file. A helper of your own, and whatever it reads, are left alone. With
`mcc delete --keep-auth` the file stays and mcc prints where it is. Adding
the profile again then replaces it, and puts the old one back if the add
fails.

## Starting it without your shell's key

Claude Code uses `ANTHROPIC_API_KEY` from the environment before an
`apiKeyHelper` ([authentication precedence](https://code.claude.com/docs/en/authentication)).
A key exported in your shell would therefore replace the profile's. So for an
API-key profile:

- its launcher (`~/.local/bin/ALIAS`) runs
  `unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN` right after setting
  `CLAUDE_CONFIG_DIR`;
- `mcc run` and `mcc exec` start it without those two variables;
- `mcc env ALIAS` prints the same `unset` line (`set -e …` for fish).

Subscription profiles' launchers don't have the line. A launcher written
before the profile was an API-key profile is listed as out of date until
`mcc refresh-launchers` rewrites it.

## What mcc can track

| What | Available? | How |
|---|---|---|
| Spend this month, estimated | Yes, on this machine | the status line's `cost.total_cost_usd`, below |
| A budget to compare with | Yes, entered by you | `--budget`, `profiles.ALIAS.api_budget_usd` |
| Usage and cost by API key | Only with an Admin key; not implemented | the Admin API's usage and cost reports |
| Credit balance, spend limit | No | Console only |
| 5-hour and weekly windows | No | subscription logins only |

### Available: a local spend estimate

Claude Code sends the [status line](https://code.claude.com/docs/en/statusline)
each session's `session_id` and its running cost, `cost.total_cost_usd`. That
cost is Claude Code's own estimate at list prices, not the billed amount.
`mcc add` installs the mcc statusline shim in every API-key profile, through
the profile's settings overlay (`settings.mcc.json`), and wraps any
statusLine the profile already had, as `mcc statusline install` does. An
overlay key belongs to the profile: `mcc sync` and the launch-time catch-up
never replace it, and never take it into the default or another profile.

For an API-key profile the shim keeps one ledger per session in
`~/.mcc/state/<profile>/cost/<session_id>.json`: the last total it saw and
what the session added in each UTC month. The first time it sees a session
is a baseline that adds nothing, because a new session starts at 0 and a
forked session may carry its parent's total over. After that, a higher total
adds the difference, the same total adds nothing (`--continue` keeps the
session), and a lower total (Claude Code counting again) counts in full. A
session id that isn't a plain file name is ignored. Ledgers left alone for
62 days are removed when the next new session starts.

`mcc list`, `mcc usage` and bare `mcc` add up the current UTC month across
the profile's ledgers and show it as `est. $X.XX this month`, then
`of $Y.YY budget (Z%)` when a budget is set. Before the first session they
say `no cost recorded yet` and suggest starting one.

The estimate covers only Claude Code sessions in this profile on this
machine. Sessions on other machines, other tools using the same key, and
anything else billed to the organization are not in it.

### Available with an Admin key (not implemented)

The Admin API's [usage and cost reports](https://platform.claude.com/docs/en/manage-claude/usage-cost-api)
(`/v1/organizations/usage_report/messages` and `/v1/organizations/cost_report`)
give an organization's usage and cost, filterable by `api_key_id`. They need
an Admin key (`sk-ant-admin…`), a second and more powerful credential, and
are not available to individual Console accounts. mcc doesn't ask for one.

### Not available

- **Credit balance and spend limit.** They are shown and set only in the
  Console. There is no API endpoint for them
  ([anthropics/claude-code#47574](https://github.com/anthropics/claude-code/issues/47574)).
  A request over the limit fails, which is the only signal
  ([spend limits](https://platform.claude.com/docs/en/api/rate-limits#spend-limits)).
- **`anthropic-ratelimit-*` response headers.** They report per-minute
  throughput limits, not spend, and reading them takes a billed request.
- **Claude Code's "usage credits".** They are a subscription's extra usage,
  and don't apply to API-key authentication.

## How mcc lists them

`mcc list`, bare `mcc` and `mcc usage` show API-key profiles in an `API KEY`
block after the subscription profiles, with no 5-hour or weekly window, no
"run /usage" hint and no login expiry:

```text
# API KEY  LAST ACTIVE  EST. SPEND (THIS MONTH, THIS MACHINE)
3 api      unknown      est. $1.50 this month of $50.00 budget (3%)
```

In `--json` and `--json-v2`, an API-key profile's object has `"auth":
"api_key"`, and its `usage` object adds `est_cost_usd` and `budget_usd` (each
left out when unknown). Its `status` and `reason` are codes older mcc
versions already know: `unavailable` with `no_data` from the cache, or
`not_logged_in` with a live lookup. So another machine's older mcc
(`mcc usage` across [machines](../README.md#multiple-machines)) still reads
the row. Other profiles' objects are unchanged.

## Commands that don't apply

- `mcc login` (and `mcc renew`) refuse an API-key profile in both modes,
  before running `claude`: there is no Claude login to renew. They refuse
  `mcc login default` too when `~/.claude`'s settings give Claude Code a key
  (`apiKeyHelper` or `env.ANTHROPIC_API_KEY`).
- `mcc usage --all` never refreshes an API-key profile's token: the
  one-turn `claude -p` it would start would be billed to the key.
- `mcc default PROFILE` refuses to trade with an API-key profile, or when
  `~/.claude`'s settings give Claude Code a key: there is no Claude account
  to trade.
- `mcc share` can't compare an API-key profile's account with the group's,
  so it asks for `--allow-cross-org` or a group of its own.
- `mcc delete` still runs `claude auth logout` for the profile, as for any
  other.

## Security notes

- The key file is readable only by you, and its directory only by you.
  Anyone who can read your files can read it, as they can read a key in
  `settings.json` or a shell profile. Helper mode keeps it in a secret store
  of your choice.
- `mcc add` installs the statusline (which backs up the profile's
  `settings.json` first) before it sets `apiKeyHelper`, so no settings backup
  mcc makes contains the helper command.
- Claude Code caches the helper's output for 5 minutes by default
  (`CLAUDE_CODE_API_KEY_HELPER_TTL_MS`).
- `claude auth status` does not run `apiKeyHelper`, so mcc's status checks
  never touch the key.

## Sources

- Claude Code authentication: <https://code.claude.com/docs/en/authentication>
- `apiKeyHelper` setting: <https://code.claude.com/docs/en/settings-reference#apikeyhelper>
- Status line input: <https://code.claude.com/docs/en/statusline>
- Admin usage and cost API: <https://platform.claude.com/docs/en/manage-claude/usage-cost-api>
- Spend limits: <https://platform.claude.com/docs/en/api/rate-limits#spend-limits>
