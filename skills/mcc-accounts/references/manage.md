# Manage: what each command changes

Read the command's row before you run it. Run a Manage command only when the user asked for that change.

- **run**: the change is reversible. Run it, then verify.
- **confirm**: state what will change or be lost, wait for an explicit yes, then run and verify.
- **hand off**: it needs a terminal, a browser, or a choice only the user can make. Give the user the command and stop.

Pass `-y`/`--yes` only after the user confirmed. With `MCC_NON_INTERACTIVE=1`, a prompt that needs an answer stops and names the flag to pass.

| Command | What it changes | Mode | Verify with |
| --- | --- | --- | --- |
| `mcc project set <p> [--path DIR] [--allow q]` | `mcc run` starts `<p>` in that project (git top, or DIR). Running it again without `--allow` clears the allow list | run | `mcc run --dry-run` in the project → `<p>` |
| `mcc project unset [--path DIR]` | Removes the mapping that applies here | run | `mcc project list --json-v2` |
| `mcc default <p>`, `mcc default --new` | Switches the account of `~/.claude` (`default`) by signing it out and in again in the browser. `<p>` trades accounts with that profile, so `<p>` is signed out and in too; `--new` signs `~/.claude` in to another account and may keep the old one as a new profile. Moves project mappings with their account unless `--keep-projects`. (mcc 0.7.0 to 0.9.x: a pointer that made plain `claude` start `<p>`; mcc 0.10.0 removes it.) | hand off: it signs accounts out and needs browser sign-ins; never run it yourself, not even with `-y` | `mcc list --json-v2 --hide-email` → each profile's `account` |
| `mcc sync <p>` | Copies the default's setup into `<p>`, keeps local edits, backs up what it replaces. mcc 0.12.0+: first takes a settings key changed in any profile into the default's `settings.json` (backed up), and asks before copying a shared secret (skipped without a terminal; `--yes` never agrees) | run `--dry-run` first and show it, then run | `mcc list --json-v2` → `sync` |
| `mcc sync <p> --prune`, `mcc sync <p> --force`, `mcc sync --all` | Removes profile-only files, overwrites conflicts, or syncs every profile (all backed up first) | confirm | `mcc diff <p>` |
| `mcc diff <p>` | Nothing (read-only) | run | — |
| `mcc rename <old> <new>` | The command becomes `<new>`. The folder keeps its old name; login and mappings move to `<new>` | confirm (the user's command changes) | `mcc list --json-v2 --hide-email` |
| `mcc config set <key> <value>` | One setting in `~/.mcc/config.toml`; checked before it is written | run | `mcc config get <key>` |
| `mcc config set usage.live true`, `mcc config set privacy.show_email true` | Every usage lookup then sends each account's token to Anthropic's usage endpoint / every listing shows emails | confirm | `mcc config get <key>` |
| `mcc share <p> [--group g]` / `--off` | Shares `<p>`'s session history with the group (off by default). Prints a terms note the first time | confirm; relay the note | `mcc list --json-v2` → `share_sessions` |
| `mcc share <p> --allow-cross-org` | Shares sessions across organizations (work and personal) | hand off: it needs the user at a prompt | — |
| `mcc link <p> [items]` / `plugins` | Items or plugins write through to the default's copy | confirm (edits then reach the default) | `mcc link <p>` |
| `mcc refresh-launchers [p…]` | Rewrites launchers from current templates | run when `launcher_current` is `false` | `mcc list --json-v2` → `launcher_current` |
| `mcc statusline install` / `uninstall` | Edits `~/.claude/settings.json` (after a backup) | confirm | `mcc doctor --json-v2` |
| `mcc login <p> --refresh-only` | Runs one tiny `claude -p` as `<p>` so Claude Code refreshes its token. Spends a little quota; usually does not move the login deadline | confirm | `mcc usage --json-v2` → `login_expires_at` |
| `mcc login <p>` | Browser sign-in that renews the login | hand off | `mcc usage --json-v2` → `login_warning` |
| `mcc add <p> --clone` | New profile, launcher, and rc alias; ends with a browser sign-in | hand off | `mcc list --json-v2` |
| `mcc add <p> --api-key-prompt` / `--api-key-stdin` / `--api-key-helper CMD` | New API-key profile: no sign-in; a managed key goes to a `0600` file in `~/.mcc/keys`. Never put a key in the command line, and never ask the user to paste one into the chat | hand off: the user types or pipes the key | `mcc list --json-v2` → `auth` |
| `mcc config set profiles.<p>.api_budget_usd <usd>` | An API-key profile's monthly budget (`""` removes it) | run | `mcc config get profiles.<p>.api_budget_usd` |
| `mcc delete <p>` | Logs out and removes the profile folder: its sessions, history, launcher, and alias. Mappings naming it go too | confirm, naming what is lost | `mcc list --json-v2` |
| `mcc init` | Detects the shell and may offer to edit the rc file | hand off | `mcc doctor --json-v2` |

`default` (or `claude`) always names `~/.claude`. mcc refuses to rename, delete, or share it.

## After a change

1. Run the verify command from the row.
2. If it shows the expected state, report the change as complete, with that output as evidence.
3. If it does not, report the change as failed, quote mcc's stderr, and suggest `mcc doctor`.
