#!/bin/sh
# Install mcc — manage multiple Claude Code profiles.
#
#   From a clone:   ./install.sh
#   One-liner:      curl -fsSL https://raw.githubusercontent.com/luongnv89/mcc/main/install.sh | sh
#
# Builds with cargo and installs the `mcc` binary into ~/.cargo/bin
# (override with CARGO_INSTALL_ROOT). Supports macOS and Linux.
set -eu

REPO_URL="https://github.com/luongnv89/mcc"

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  blue='\033[34m' green='\033[32m' red='\033[31m' reset='\033[0m'
else
  blue='' green='' red='' reset=''
fi
info() { printf "  ${blue}●${reset} %s\n" "$1"; }
ok() { printf "  ${green}✓${reset} %s\n" "$1"; }
fail() {
  printf "${red}✗ error:${reset} %s\n" "$1" >&2
  if [ $# -gt 1 ]; then printf '  hint: %s\n' "$2" >&2; fi
  exit 1
}

printf '\n◆ Installing mcc\n'

# 1. Platform
os="$(uname -s)"
case "$os" in
  Darwin | Linux) ok "platform: $os $(uname -m)" ;;
  *) fail "unsupported platform: $os" "mcc currently supports macOS and Linux" ;;
esac

# 2. Toolchain
if ! command -v cargo >/dev/null 2>&1; then
  fail "cargo (the Rust toolchain) is required" "install it from https://rustup.rs, then re-run this script"
fi
ok "cargo: $(cargo --version)"

# 3. Build + install (local checkout if we're in one, else from GitHub)
script_dir="$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)"
if [ -n "$script_dir" ] && [ -f "$script_dir/Cargo.toml" ] && grep -q '^name = "mcc"' "$script_dir/Cargo.toml"; then
  info "building from $script_dir"
  cargo install --path "$script_dir" --locked --force
else
  info "building from $REPO_URL"
  cargo install --git "$REPO_URL" --locked --force
fi

# 4. Verify
bin_dir="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin"
if ! "$bin_dir/mcc" --version >/dev/null 2>&1; then
  fail "mcc was not installed to $bin_dir" "check the cargo output above"
fi
ok "installed $("$bin_dir/mcc" --version) → $bin_dir/mcc"

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) info "add $bin_dir to your PATH to run \`mcc\` directly" ;;
esac

if ! command -v claude >/dev/null 2>&1; then
  info "Claude Code (\`claude\`) was not found — install it before adding profiles"
fi

printf '\n  Next: run %s, then %s (see %s)\n\n' "\`mcc init\`" "\`mcc add <alias>\`" "\`mcc --help\`"
