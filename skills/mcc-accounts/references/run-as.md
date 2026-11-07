# Run as a named account

Read this before you run anything as a profile with `mcc exec`. The account must be user-picked first (SKILL.md → the rule that outranks the others).

## What `mcc exec <profile> -- <command>` does

- Sets `CLAUDE_CONFIG_DIR` to the profile's directory. For `default`, it unsets the variable and sets `MCC_PROFILE=default`.
- Sets `CLAUDE_CODE_PLUGIN_CACHE_DIR` for a profile that shares the default's plugins, and clears mcc's own value for one that does not.
- Runs the command and exits with its exit status. A command that is not found exits 127; one that cannot run exits 126.
- Takes no lock, so the command may run mcc itself.
- Does **not** remove credential variables. The variables below make `claude` use another credential or provider instead of the profile's login. mcc strips the same list for its own `claude` runs, but `mcc exec` and `mcc env` keep them:

  `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `ANTHROPIC_BASE_URL`

  If any is set (`credential_env` in `mcc_whoami.py`), remove it for this one command and tell the user:

  ```sh
  env -u ANTHROPIC_API_KEY mcc exec lab -- claude mcp list
  ```

  This changes nothing outside that command.

`mcc exec --all -- <command>` runs once per profile, the default first. It exits 0 only when every run succeeds. Use it only when the user asks for every account.

## Common commands

| Task | Command |
| --- | --- |
| Check a profile's login | `mcc exec lab -- claude auth status` (prints the email: show it only if the user asked) |
| List MCP servers or plugins | `mcc exec lab -- claude mcp list`, `mcc exec lab -- claude plugin list` |
| Add an MCP server to one account | `mcc exec lab -- claude mcp add …` (a Manage action: the user asked for it) |
| Any tool that reads `CLAUDE_CONFIG_DIR` | `mcc exec lab -- <tool> …` |

## Headless Claude tasks (`claude -p`)

SKILL.md (Run as, step 4) decides whether to run at once or to wait for a yes. Then:

1. Say that the run spends the named profile's quota.
2. Run it from the directory the task is about. The child sees that project's `CLAUDE.md` and the profile's own settings, MCP servers, and hooks.
3. Use the smallest permissions that fit:
   - Answer-only task: `mcc exec lab -- claude -p "<task>"`.
   - Task that needs tools: ask the user which tools to allow, then pass `--allowedTools "Read,Grep"` (for example). Never pass `--dangerously-skip-permissions` or `--permission-mode bypassPermissions` unless the user names that flag.
   - Always pass `--max-turns <n>` (a small number for an answer, more for a task with tools). Optional: `--model <name>`, `--no-session-persistence`.
   - A task that may outlast your shell tool's time limit runs in the background. If it times out, report `git status` so the user sees any half-done edits.
4. Read the answer from stdout. Keep stderr separate (`2>"$err"`). Relay any line starting with ⚠ (mcc's terms note). Report the rest of stderr only when the exit status is non-zero: Claude Code may print settings warnings there.
5. Quote the prompt with single quotes, or pass it on stdin (`printf '%s' "$task" | mcc exec lab -- claude -p`), so the shell does not expand it.

A nested `claude -p` started this way works from inside a Claude Code session. It was verified with Claude Code 2.1.x and mcc 0.8.0.

## Never from an agent shell

These need a terminal or a browser. A non-interactive shell hangs, exits 2, or starts nothing useful. Give the user the command instead:

| Command | Why |
| --- | --- |
| `mcc run`, `<profile>` (its launcher), plain `claude` | Start an interactive Claude Code session |
| `mcc login <profile>`, `mcc exec <profile> -- claude auth login` | Open a browser sign-in |
| `mcc add <profile>` | Ends with a browser sign-in |
| `mcc config edit` | Opens an editor |

`mcc env <profile>` prints `export` lines for the user's own shell, IDE, or direnv (`eval "$(mcc env lab)"`). Your shell state does not persist between tool calls, so use `mcc exec` for your own commands.

## Other accounts than the project's

Inside a project mapped to another account, `mcc exec`, `mcc env`, and `mcc run --profile` print mcc's terms note on stderr. Relay it as SKILL.md says, once per conversation.
