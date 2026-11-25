#!/usr/bin/env bash
# scripts/e2e_test.sh: end-to-end smoke test of every mcc command.
#
# Builds the debug binary, then runs each command and subcommand in a throwaway
# sandbox built the way tests/common/mod.rs builds one: a temporary HOME and
# MCC_HOME, fake claude, security and ssh commands first on PATH, and a
# refused loopback port as the usage endpoint. Nothing here touches the real
# ~/.claude, ~/.mcc, rc files or keychain, and nothing reaches the network.
#
# It fails when a command listed by "mcc --help" has no check below, so a new
# command cannot ship without one.
#
# Runs on git push (the e2e-cli pre-push hook) and in CI's hook guard.
# Usage: bash scripts/e2e_test.sh [-v]     -v prints each command's output
set -euo pipefail

VERBOSE=0
[ "${1:-}" = "-v" ] && VERBOSE=1

cd "$(dirname "$0")/.."
cargo build --locked --quiet
target="${CARGO_TARGET_DIR:-target}"
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
MCC="$target/debug/mcc"
[ -x "$MCC" ] || { echo "e2e: no binary at $MCC" >&2; exit 1; }

S="$(mktemp -d)"
trap 'rm -rf "$S"' EXIT
S="$(cd "$S" && pwd -P)"
mkdir -p "$S/fakebin" "$S/.local/bin" "$S/proj" "$S/.claude/skills/hello"

cat > "$S/fakebin/claude" <<'FAKE'
#!/bin/sh
# Fake claude: no network, no quota. Login writes a credentials file.
dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
printf '%s\n' "$*" >> "$HOME/claude-calls.log"
case "${1:-} ${2:-}" in
  "--version "*) echo "9.9.9 (Claude Code)" ;;
  "auth login")
    [ "${3:-}" = "--help" ] && { echo "Options:"; echo "  --email <email>"; exit 0; }
    mkdir -p "$dir" && cp "$HOME/fake-login.json" "$dir/.credentials.json" ;;
  "auth status")
    if [ -f "$dir/.credentials.json" ]; then
      echo '{"loggedIn": true, "authMethod": "claude.ai", "email": "fake@example.com"}'
    else
      echo '{"loggedIn": false, "authMethod": "none"}'
    fi ;;
  "auth logout") rm -f "$dir/.credentials.json"; echo "Successfully logged out." ;;
  *) echo "fake claude $*" ;;
esac
FAKE
cat > "$S/fakebin/security" <<'FAKE'
#!/bin/sh
# Fake macOS security: the sandbox keychain is always empty.
echo "security: The specified item could not be found in the keychain." >&2
exit 44
FAKE
cat > "$S/fakebin/ssh" <<'FAKE'
#!/bin/sh
# Fake ssh: no machine can be reached.
echo "ssh: connect to host: Connection refused" >&2
exit 255
FAKE
chmod +x "$S/fakebin/claude" "$S/fakebin/security" "$S/fakebin/ssh"

# A default profile (~/.claude) to copy from.
printf '{"model":"opus"}\n' > "$S/.claude/settings.json"
printf -- '---\nname: hello\ndescription: "Say hello."\n---\nSay hello.\n' > "$S/.claude/skills/hello/SKILL.md"
printf '{"oauthAccount": {"emailAddress": "owner@example.com"}}\n' > "$S/.claude.json"
# What the fake claude writes on login: a made-up token that expires in 30 days.
printf '{"claudeAiOauth":{"accessToken":"fake-access","refreshToken":"fake-refresh","expiresAt":%s000,"scopes":["user:inference","user:profile"],"subscriptionType":"max"}}\n' \
  "$(($(date +%s) + 2592000))" > "$S/fake-login.json"
cp "$S/fake-login.json" "$S/.claude/.credentials.json"

# mcc with the sandboxed environment only (env -i), run from the sandbox project.
mcc() {
  (cd "$S/proj" && env -i \
    HOME="$S" MCC_HOME="$S" PATH="$S/fakebin:$S/.local/bin:/usr/bin:/bin" \
    SHELL=/bin/zsh TERM_PROGRAM=TestTerm USER=mcc-test EDITOR=true \
    MCC_CLAUDE_BIN="$S/fakebin/claude" MCC_SECURITY_BIN="$S/fakebin/security" \
    MCC_SSH_BIN="$S/fakebin/ssh" MCC_DEVIN_BIN="$S/fakebin/no-devin" \
    MCC_USAGE_URL=http://127.0.0.1:9/api/oauth/usage \
    MCC_DEVIN_API_URL=http://127.0.0.1:9/api/oauth/usage \
    MCC_NO_SHELL_PROBE=1 MCC_NON_INTERACTIVE=1 NO_COLOR=1 \
    "$MCC" "$@")
}

PASSED=0
FAILED=0
COVERED=" help "

report() { # report <ok|fail> <description> <output>
  if [ "$1" = ok ]; then
    PASSED=$((PASSED + 1))
    echo "  ok    $2"
    [ "$VERBOSE" -eq 0 ] || printf '%s\n' "$3" | sed 's/^/        | /'
  else
    FAILED=$((FAILED + 1))
    echo "  FAIL  $2"
    printf '%s\n' "$3" | sed 's/^/        | /'
  fi
}

# ok <description> <mcc args...>: exits 0.
ok() {
  local desc="$1" out status=0
  shift
  COVERED="$COVERED${1:-} "
  out="$(mcc "$@" 2>&1)" || status=$?
  if [ "$status" -eq 0 ]; then report ok "$desc" "$out"; else report fail "$desc (exit $status)" "$out"; fi
}

# shows <description> <pattern> <mcc args...>: exits 0 and prints <pattern>.
shows() {
  local desc="$1" pattern="$2" out status=0
  shift 2
  COVERED="$COVERED$1 "
  out="$(mcc "$@" 2>&1)" || status=$?
  if [ "$status" -eq 0 ] && grep -q -e "$pattern" <<<"$out"; then
    report ok "$desc" "$out"
  else
    report fail "$desc (exit $status, expected: $pattern)" "$out"
  fi
}

# refuses <description> <pattern> <mcc args...>: exits non-zero and prints <pattern>.
refuses() {
  local desc="$1" pattern="$2" out status=0
  shift 2
  COVERED="$COVERED$1 "
  out="$(mcc "$@" 2>&1)" || status=$?
  if [ "$status" -ne 0 ] && grep -q -e "$pattern" <<<"$out"; then
    report ok "$desc" "$out"
  else
    report fail "$desc (exit $status, expected a failure with: $pattern)" "$out"
  fi
}

echo "=== mcc end-to-end ($MCC) ==="
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"

echo "--- version and help"
shows "--version matches Cargo.toml" "^mcc $version\$" --version
shows "--help lists the commands" "^Usage: mcc" --help
COMMANDS="$(mcc --help | awk '/^Commands:/ {on = 1; next} /^$/ {on = 0} on && /^  [a-z]/ {print $1}')"
for cmd in $COMMANDS; do
  [ "$cmd" = help ] || shows "help $cmd" "^Usage: mcc $cmd" help "$cmd"
done
COVERED=" help "   # --help alone does not count as a check of a command

echo "--- setup"
ok "init --shell zsh" init --shell zsh
ok "add work --fresh (signs in with the fake claude)" add work --fresh
ok "add lab --clone" add lab --clone
shows "list shows both profiles" "work" list --no-usage
shows "list --json is JSON with the profiles" '"name": *"lab"' list --json --no-usage
ok "usage --json (endpoint refused)" usage --json
ok "login lab renews the sign-in" login lab
refuses "login --refresh-only leaves a still-valid token alone" "still valid" login lab --refresh-only
ok "mcc with no command prints the status"

echo "--- a profile's setup"
ok "sync work --dry-run" sync work --dry-run
ok "sync work --yes" sync work --yes
ok "diff work" diff work
ok "rename work job" rename work job
shows "the renamed profile is listed" "job" list --no-usage
ok "share job" share job
ok "share job --off" share job --off
ok "link job skills" link job skills
ok "link job skills --off" link job skills --off
ok "refresh-launchers" refresh-launchers
ok "doctor" doctor
shows "doctor --json is JSON" "^[[{]" doctor --json
ok "default (show)" default

echo "--- projects and launching"
ok "project set job" project set job
shows "project list --json names the profile" "job" project list --json
shows "here names the project's profile" "job" here
shows "run --dry-run starts the project's profile" "job" run --dry-run
shows "env job sets CLAUDE_CONFIG_DIR" "CLAUDE_CONFIG_DIR" env job
# shellcheck disable=SC2016 # $CLAUDE_CONFIG_DIR is read by the sh that mcc exec starts
shows "exec job runs a command as the profile" "/\.mcc/profiles/" exec job -- /bin/sh -c 'echo "$CLAUDE_CONFIG_DIR"'
ok "project unset" project unset
ok "devin --dry-run" devin --dry-run
refuses "remote to an unknown machine fails" "no machine named" remote job no-such-box "free disk space"

echo "--- config, status line, completions"
shows "config path" "config.toml" config path
ok "config set profiles.job.label" config set profiles.job.label Acme
shows "config get returns the value" "Acme" config get profiles.job.label
ok "config edit (EDITOR=true)" config edit
ok "statusline install" statusline install
ok "statusline uninstall" statusline uninstall
shows "completions zsh" "#compdef mcc" completions zsh
ok "completions bash" completions bash
ok "completions fish" completions fish

echo "--- teardown and error paths"
ok "delete lab --yes" delete lab --yes
ok "delete job --yes" delete job --yes
refuses "an unknown command fails" "unrecognized subcommand" totally-invalid-command
refuses "an unknown profile fails" "ghost" diff ghost
refuses "an unknown flag fails" "unexpected argument" list --no-such-flag

echo "--- every command has a check"
for cmd in $COMMANDS; do
  case "$COVERED" in
    *" $cmd "*) ;;
    *) report fail "no end-to-end check for: mcc $cmd" "add one to scripts/e2e_test.sh" ;;
  esac
done

echo
echo "=== $PASSED passed, $FAILED failed ==="
[ "$FAILED" -eq 0 ]
