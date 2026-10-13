# Homebrew formula for mcc. This file is the source of truth; the copy users
# install from lives in the tap repository luongnv89/homebrew-tap at
# Formula/mcc.rb, so `brew install luongnv89/tap/mcc` works on macOS and Linux.
#
# Publishing a release (keep `version` in Cargo.toml and the tag in sync):
#   1. In the release commit, point `url` below at the new tag (tests/metadata.rs
#      checks it matches Cargo.toml) and set `sha256` to 64 zeros.
#   2. Tag and push the release: git tag vX.Y.Z && git push origin vX.Y.Z
#   3. Get the checksum of the source tarball:
#        curl -fsSL https://github.com/luongnv89/mcc/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
#   4. Set `sha256` below, then copy this file to
#      Formula/mcc.rb in luongnv89/homebrew-tap and push it.
#   5. Check it: brew audit --strict --online luongnv89/tap/mcc
#                brew install --build-from-source luongnv89/tap/mcc && brew test mcc
class Mcc < Formula
  desc "Manage multiple Claude Code profiles (accounts) from one CLI"
  homepage "https://github.com/luongnv89/mcc"
  url "https://github.com/luongnv89/mcc/archive/refs/tags/v0.2.0.tar.gz"
  sha256 "50712585a280f8ee15f025db0adfbcf8f4a6285f5b746b0335586104854d524b"
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
