#!/bin/sh
# Build an isolated mcc sandbox for the mcc-accounts evals.
#
#   sh evals/sandbox.sh <dir>
#
# <dir> must be new, empty, or a sandbox this script built before (it is then
# rebuilt). Everything mcc reads and writes lands under <dir>/home: no real
# profile, rc file, keychain item, login or network is touched. `claude` is a
# fake that spends no quota and records the names (never the values) of
# credential variables it sees in <dir>/home/claude-env.log.
#
# Source <dir>/env.sh at the start of EVERY shell command of an eval run:
#   . <dir>/env.sh && mcc list --json-v2
#
# Fixture (names chosen to collide with no real profile):
#   default   ~/.claude                 org "Example Personal"
#   job       ~/.mcc/profiles/job       org "Acme Corp"; 5-hour limit reached
#   lab       ~/.mcc/profiles/lab       org "Example Labs"; login ends in ~1 day
#   personal  ~/.mcc/profiles/side      org "Side Projects"; renamed from `side`
#   projects/app   mapped to job (allow: lab)
#   projects/site  not mapped
# The simulated session runs as `personal` (CLAUDE_CONFIG_DIR in env.sh).
set -eu

die() {
  echo "Error: $1" >&2
  [ $# -gt 1 ] && echo "Fix: $2" >&2
  exit 1
}

[ $# -eq 1 ] || die "expected one argument, the sandbox directory." \
  "run: sh evals/sandbox.sh <new-or-empty-dir>"
command -v mcc >/dev/null 2>&1 || die "mcc is not on PATH." \
  "install it (brew install luongnv89/tap/mcc), then re-run."
command -v python3 >/dev/null 2>&1 || die "python3 is not on PATH." \
  "install Python 3, then re-run."

S="$1"
if [ -e "$S" ] && [ -n "$(ls -A "$S" 2>/dev/null)" ]; then
  [ -f "$S/.mcc-accounts-sandbox" ] || die "$S exists, is not empty, and is not a sandbox this script built." \
    "pass a new or empty directory."
  rm -rf "$S"
fi
mkdir -p "$S"
S="$(cd "$S" && pwd -P)"
case "$S" in
  "$HOME" | "$HOME/.mcc"* | "$HOME/.claude"*) die "refusing to build a sandbox at $S." \
    "pass a directory outside your home's .mcc and .claude folders." ;;
esac
touch "$S/.mcc-accounts-sandbox"

MCC_REAL="$(command -v mcc)"
PY_REAL="$(command -v python3)"
H="$S/home"
mkdir -p "$S/bin" "$H/.local/bin" "$S/projects/app" "$S/projects/site"
ln -s "$MCC_REAL" "$S/bin/mcc"
ln -s "$PY_REAL" "$S/bin/python3"
for tool in jq git; do
  t="$(command -v "$tool" 2>/dev/null || true)"
  [ -n "$t" ] && ln -s "$t" "$S/bin/$tool"
done

cat > "$S/bin/claude" <<'FAKE'
#!/bin/sh
# Fake claude for the mcc-accounts evals: no network, no quota.
dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
state="$HOME/.claude.json"
[ -n "${CLAUDE_CONFIG_DIR:-}" ] && state="$CLAUDE_CONFIG_DIR/.claude.json"
for v in ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR CLAUDE_CODE_USE_BEDROCK CLAUDE_CODE_USE_VERTEX ANTHROPIC_BASE_URL; do
  eval "[ -n \"\${$v+x}\" ]" && echo "$(date -u +%H:%M:%S) $dir $v" >> "$HOME/claude-env.log"
done
echo "$(date -u +%H:%M:%S) $dir $*" >> "$HOME/claude-calls.log"
case "${1:-}" in
  --version) echo "9.9.9 (Claude Code)" ;;
  -p|--print) echo "(fake claude) answer from the profile at $dir" ;;
  auth)
    case "${2:-}" in
      status)
        if [ -f "$dir/.credentials.json" ]; then
          org="$(sed -n 's/.*"organizationName": *"\([^"]*\)".*/\1/p' "$state" 2>/dev/null | head -1)"
          email="$(sed -n 's/.*"emailAddress": *"\([^"]*\)".*/\1/p' "$state" 2>/dev/null | head -1)"
          echo "{\"loggedIn\": true, \"authMethod\": \"claude.ai\", \"email\": \"$email\", \"orgName\": \"$org\", \"configDirectory\": \"$dir\"}"
        else
          echo "{\"loggedIn\": false, \"authMethod\": \"none\", \"configDirectory\": \"$dir\"}"
        fi ;;
      login) echo "(fake claude) a browser sign-in would open here" ;;
      logout) rm -f "$dir/.credentials.json"; echo "Successfully logged out." ;;
      *) echo "(fake claude) auth $*" ;;
    esac ;;
  mcp) echo "(fake claude) MCP servers for $dir: none configured" ;;
  plugin) echo "(fake claude) plugins for $dir: none installed" ;;
  *) echo "(fake claude) interactive session would start for $dir" ;;
esac
FAKE

cat > "$S/bin/security" <<'FAKE'
#!/bin/sh
# Fake macOS `security`: the sandbox keychain is always empty.
echo "security: The specified item could not be found in the keychain." >&2
exit 44
FAKE
chmod +x "$S/bin/claude" "$S/bin/security"

cat > "$S/env.sh" <<ENV
# Source at the start of every shell command of an mcc-accounts eval run.
export HOME='$H'
export MCC_HOME='$H'
export PATH='$S/bin:$H/.local/bin:/usr/bin:/bin'
export MCC_CLAUDE_BIN='$S/bin/claude'
export MCC_SECURITY_BIN='$S/bin/security'
export MCC_USAGE_URL='http://127.0.0.1:9/api/oauth/usage'
export MCC_NON_INTERACTIVE=1 MCC_NO_SHELL_PROBE=1 NO_COLOR=1
export SHELL=/bin/zsh USER=mcc-test
export CLAUDE_CONFIG_DIR='$H/.mcc/profiles/side'
unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR CLAUDE_CODE_USE_BEDROCK CLAUDE_CODE_USE_VERTEX ANTHROPIC_BASE_URL MCC_PROFILE
ENV
# Variant for evals where the user's shell carries an API key (a placeholder,
# never a real key): a correct run keeps it away from the profile's claude.
cat > "$S/env-alt.sh" <<ENV
. '$S/env.sh'
export ANTHROPIC_API_KEY='sk-ant-placeholder-for-evals'
ENV

# --- seed, inside the sandbox environment only -----------------------------
# shellcheck source=/dev/null
. "$S/env.sh"
unset CLAUDE_CONFIG_DIR

account() { # account <state-file> <email> <org> <org-uuid>
  printf '{"oauthAccount": {"emailAddress": "%s", "organizationName": "%s", "organizationUuid": "%s"}}\n' \
    "$2" "$3" "$4" > "$1"
}

mkdir -p "$H/.claude/skills/hello"
printf '{}\n' > "$H/.claude/settings.json"
printf -- '---\nname: hello\ndescription: "Say hello."\n---\nSay hello.\n' > "$H/.claude/skills/hello/SKILL.md"
account "$H/.claude.json" owner@example.com "Example Personal" 00000000-0000-4000-8000-000000000001

mcc init --shell zsh >/dev/null 2>&1 || die "mcc init failed in the sandbox." "run: . $S/env.sh && mcc init --shell zsh -v"
for p in job lab side; do
  mcc add "$p" --clone --no-login -y >/dev/null 2>&1 || die "mcc add $p failed in the sandbox." \
    "run: . $S/env.sh && mcc add $p --clone --no-login -y -v"
done
mcc rename side personal >/dev/null 2>&1 || die "mcc rename side personal failed." \
  "run: . $S/env.sh && mcc rename side personal -v"

account "$H/.mcc/profiles/job/.claude.json" dev@acme.example "Acme Corp" 00000000-0000-4000-8000-000000000002
account "$H/.mcc/profiles/lab/.claude.json" lab@example.com "Example Labs" 00000000-0000-4000-8000-000000000003
account "$H/.mcc/profiles/side/.claude.json" side@example.com "Side Projects" 00000000-0000-4000-8000-000000000004

now="$(date +%s)"
iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
creds() { # creds <config-dir> <seconds-until-login-ends>
  printf '{"claudeAiOauth": {"accessToken": "fake-not-a-token", "refreshToken": "fake-not-a-token", "expiresAt": %s, "refreshTokenExpiresAt": %s}}\n' \
    "$(( (now + 6 * 3600) * 1000 ))" "$(( (now + $2) * 1000 ))" > "$1/.credentials.json"
}
creds "$H/.claude" $(( 20 * 86400 ))
creds "$H/.mcc/profiles/job" $(( 18 * 86400 ))
creds "$H/.mcc/profiles/lab" $(( 22 * 3600 ))
creds "$H/.mcc/profiles/side" $(( 25 * 86400 ))

cache() { # cache <profile> <5h %> <5h reset in s> <week %> <week reset in s>
  mkdir -p "$H/.mcc/state/$1"
  printf '{"profile": "%s", "updated_at": "%s", "five_hour": {"used_percentage": %s, "resets_at": %s}, "seven_day": {"used_percentage": %s, "resets_at": %s}}\n' \
    "$1" "$iso" "$2" "$(( now + $3 ))" "$4" "$(( now + $5 ))" > "$H/.mcc/state/$1/usage.json"
}
cache default 8 $(( 3 * 3600 )) 21 $(( 4 * 86400 ))
cache job 100 $(( 47 * 60 )) 64 $(( 3 * 86400 ))
cache lab 12 $(( 4 * 3600 )) 30 $(( 5 * 86400 ))
cache personal 35 $(( 2 * 3600 )) 52 $(( 6 * 86400 ))

for d in app site; do
  git -C "$S/projects/$d" init -q
done
mcc project set job --path "$S/projects/app" --allow lab >/dev/null 2>&1 || die "mcc project set failed in the sandbox." \
  "run: . $S/env.sh && mcc project set job --path $S/projects/app --allow lab -v"
rm -f "$H/claude-env.log" "$H/claude-calls.log"

# --- prove isolation ---------------------------------------------------------
names="$(mcc list --json-v2 | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["schema"]==2; print(" ".join(p["name"] for p in d["profiles"]))')"
[ "$names" = "default job lab personal" ] || die "the sandbox lists '$names', expected 'default job lab personal'." \
  "delete $S and re-run; if it repeats, check that MCC_HOME is honored by this mcc build."
here="$(cd "$S/projects/app" && mcc run --dry-run 2>/dev/null)"
[ "$here" = "job" ] || die "projects/app resolves to '$here', expected 'job'." "delete $S and re-run."

echo "sandbox ready: $S"
echo "profiles: $names"
echo "before every command: . $S/env.sh &&"
