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
| `mcc default <p>` | Plain `claude` in **new** shells, and `mcc run` outside projects, start `<p>`. Writes `config.toml` and a block in the rc file. `mcc default default` undoes it | run | `mcc default`; tell the user to open a new shell |
| `mcc sync <p>` | Copies the default's setup into `<p>`, keeps local edits, backs up what it replaces | run `--dry-run` first and show it, then run | `mcc list --json-v2` → `sync` |
| `mcc sync <p> --prune`, `mcc sync <p> --force`, `mcc sync --all` | Removes profile-only files, overwrites conflicts, or syncs every profile (all backed up first) | confirm | `mcc diff <p>` |
| `mcc diff <p>` | Nothing (read-only) | run | — |
| `mcc rename <old> <new>` | The command becomes `<new>`. The folder keeps its old name; login, mappings, and the default move to `<new>` | confirm (the user's command changes) | `mcc list --json-v2 --hide-email` |
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
| `mcc delete <p>` | Logs out and removes the profile folder: its sessions, history, launcher, and alias. Mappings naming it go too | confirm, naming what is lost | `mcc list --json-v2` |
| `mcc init` | Detects the shell and may offer to edit the rc file | hand off | `mcc doctor --json-v2` |

`default` (or `claude`) always names `~/.claude`. mcc refuses to rename, delete, or share it.

## After a change

1. Run the verify command from the row.
2. If it shows the expected state, report the change as complete, with that output as evidence.
3. If it does not, report the change as failed, quote mcc's stderr, and suggest `mcc doctor`.
