#!/bin/sh
# Install mcc — manage multiple Claude Code profiles.
#
#   One-liner:      curl -fsSL https://raw.githubusercontent.com/luongnv89/mcc/main/install.sh | sh
#   From a clone:   ./install.sh
#
# The one-liner downloads the prebuilt binary for your platform from the
# latest GitHub release, checks its SHA-256 checksum, and installs `mcc` into
# ~/.local/bin (override with MCC_INSTALL_DIR). No Rust toolchain needed.
# When no prebuilt binary is available it falls back to building with cargo.
#
# From a clone, or with MCC_FROM_SOURCE=1, it builds with cargo and installs
# into ~/.cargo/bin (override with CARGO_INSTALL_ROOT).
#
# Other settings:
#   MCC_VERSION=v0.4.0    install that release instead of the latest one
#   MCC_RELEASE_URL=URL   download from URL/<asset> (a mirror) instead of
#                         the GitHub release
#
# Supports macOS and Linux (x86_64 and arm64).
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
arch="$(uname -m)"
case "$os" in
  Darwin) target_os="apple-darwin" ;;
  Linux) target_os="unknown-linux-musl" ;;
  *) fail "unsupported platform: $os" "mcc currently supports macOS and Linux" ;;
esac
case "$arch" in
  x86_64 | amd64) target_arch="x86_64" ;;
  arm64 | aarch64) target_arch="aarch64" ;;
  *) target_arch="" ;;
esac
ok "platform: $os $arch"

# Prints the SHA-256 of file $1, or nothing when no tool is available.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

# 2a. Prebuilt binary. Returns 1 (so the caller builds from source) when there
# is no binary to download; exits on anything that looks like tampering.
install_prebuilt() {
  [ -n "$target_arch" ] || return 1
  command -v curl >/dev/null 2>&1 || return 1
  command -v tar >/dev/null 2>&1 || return 1

  target="$target_arch-$target_os"
  asset="mcc-$target.tar.gz"
  version="${MCC_VERSION:-}"
  if [ -n "${MCC_RELEASE_URL:-}" ]; then
    base="$MCC_RELEASE_URL"
  elif [ -n "$version" ]; then
    case "$version" in
      v*) ;;
      *) version="v$version" ;;
    esac
    case "$version" in
      *[!A-Za-z0-9._-]*) fail "invalid MCC_VERSION: $version" "use a release tag such as v0.4.0" ;;
    esac
    base="$REPO_URL/releases/download/$version"
  else
    base="$REPO_URL/releases/latest/download"
  fi

  tmp="$(mktemp -d 2>/dev/null || mktemp -d -t mcc)"
  # shellcheck disable=SC2064 # expand $tmp now: it is local to this run
  trap "rm -rf '$tmp'" EXIT

  info "downloading $asset"
  if ! curl -fsSL --retry 2 -o "$tmp/$asset" "$base/$asset" 2>/dev/null; then
    info "no prebuilt binary for $target at $base"
    return 1
  fi
  if ! curl -fsSL --retry 2 -o "$tmp/$asset.sha256" "$base/$asset.sha256" 2>/dev/null; then
    fail "downloaded $asset but not its checksum ($asset.sha256)" "try again, or set MCC_FROM_SOURCE=1 to build with cargo"
  fi

  expected="$(cut -d ' ' -f 1 <"$tmp/$asset.sha256")"
  actual="$(sha256_of "$tmp/$asset")"
  if [ -z "$actual" ]; then
    fail "can't verify the download: neither sha256sum nor shasum was found" "install one of them, or set MCC_FROM_SOURCE=1 to build with cargo"
  fi
  if [ "$expected" != "$actual" ]; then
    fail "checksum mismatch for $asset (expected $expected, got $actual)" "the download is corrupt or was tampered with; nothing was installed"
  fi
  ok "checksum: sha256 $actual"

  tar -xzf "$tmp/$asset" -C "$tmp" mcc || fail "could not unpack $asset"
  install_dir="${MCC_INSTALL_DIR:-$HOME/.local/bin}"
  mkdir -p "$install_dir" || fail "could not create $install_dir" "set MCC_INSTALL_DIR to a writable directory"
  # Copy then rename, so a running mcc is never left half-written.
  staged="$install_dir/.mcc.tmp.$$"
  if ! { cp "$tmp/mcc" "$staged" && chmod 755 "$staged" && mv -f "$staged" "$install_dir/mcc"; }; then
    rm -f "$staged"
    fail "could not write $install_dir/mcc" "set MCC_INSTALL_DIR to a writable directory"
  fi
  bin_dir="$install_dir"
}

# 2b. Build with cargo (local checkout if we're in one, else from GitHub).
install_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo (the Rust toolchain) is required to build mcc from source" "install it from https://rustup.rs, then re-run this script"
  fi
  ok "cargo: $(cargo --version)"
  if [ -n "$in_checkout" ]; then
    info "building from $script_dir"
    cargo install --path "$script_dir" --locked --force
  else
    info "building from $REPO_URL"
    cargo install --git "$REPO_URL" --locked --force
  fi
  bin_dir="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin"
}

script_dir="$(cd "$(dirname "$0")" 2>/dev/null && pwd)" || script_dir=""
in_checkout=""
if [ -n "$script_dir" ] && [ -f "$script_dir/Cargo.toml" ] && grep -q '^name = "mcc"' "$script_dir/Cargo.toml"; then
  in_checkout=1
fi

bin_dir=""
if [ -n "${MCC_FROM_SOURCE:-}" ] || [ -n "$in_checkout" ]; then
  install_from_source
elif ! install_prebuilt; then
  info "building from source instead"
  install_from_source
fi

# 3. Verify
if ! "$bin_dir/mcc" --version >/dev/null 2>&1; then
  fail "mcc was not installed to $bin_dir" "check the output above"
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
