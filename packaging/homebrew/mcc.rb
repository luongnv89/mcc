# Homebrew formula for mcc. This file is the source of truth; the copy users
# install from lives in the tap repository luongnv89/homebrew-tap at
# Formula/mcc.rb, so `brew install luongnv89/tap/mcc` works on macOS and Linux.
#
# Publishing a release (keep `version` in Cargo.toml and the tag in sync):
#   1. In the release commit, point `url` below at the new tag (tests/metadata.rs
#      checks it matches Cargo.toml) and set `sha256` to 64 zeros.
#   2. Tag and push the release: git tag vX.Y.Z && git push origin vX.Y.Z
#      (don't publish a GitHub release for the tag by hand before the Release
#      workflow finishes: it would be public without binaries until then)
#   3. Get the checksum of the source tarball:
#        curl -fsSL https://github.com/luongnv89/mcc/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
#   4. Set `sha256` below, then copy this file to
#      Formula/mcc.rb in luongnv89/homebrew-tap and push it.
#   5. Check it: brew audit --strict --online luongnv89/tap/mcc
#                brew install --build-from-source luongnv89/tap/mcc && brew test mcc
#
# Prebuilt binaries: pushing the tag also runs .github/workflows/release.yml,
# which creates the tag's release as a draft, attaches mcc-<target>.tar.gz, a
# .sha256 per archive and SHA256SUMS, then publishes it (targets:
# {x86_64,aarch64}-apple-darwin and {x86_64,aarch64}-unknown-linux-musl). A
# re-run never replaces a published release's assets, so its SHA256SUMS
# values stay the same. This formula still builds from source.
# Once a release has those assets, the tap's copy can install them instead,
# so users never download Rust: replace `url`/`sha256` and `depends_on "rust"`
# with `on_macos`/`on_linux` + `on_arm`/`on_intel` blocks whose `url` is
#   https://github.com/luongnv89/mcc/releases/download/vX.Y.Z/mcc-<target>.tar.gz
# and whose `sha256` comes from SHA256SUMS, and make `install` run
# `bin.install "mcc"` before generate_completions_from_executable. (Then update
# tests/metadata.rs, which checks this file's source-build url.)
class Mcc < Formula
  desc "Manage multiple Claude Code profiles (accounts) from one CLI"
  homepage "https://github.com/luongnv89/mcc"
  url "https://github.com/luongnv89/mcc/archive/refs/tags/v0.4.0.tar.gz"
  sha256 "c60960a7afb303505888e79bf8cb4fafe4b8338cbc48cf95d62af9f47d2e57fa"
  license "Apache-2.0"
  head "https://github.com/luongnv89/mcc.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    generate_completions_from_executable(bin/"mcc", "completions")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/mcc --version")
    assert_match "#compdef mcc", shell_output("#{bin}/mcc completions zsh")
  end
end
