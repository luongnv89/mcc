# Contributing to mcc

Thanks for taking the time to contribute. Bug reports, feature ideas, documentation fixes and code changes are all welcome.

By participating in this project you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Reporting bugs and requesting features

- Search the [existing issues](https://github.com/luongnv89/mcc/issues) first, in case it has already been reported.
- Open a new issue with the **Bug report** or **Feature request** template and fill in every section you can.
- When you paste output or logs, redact access tokens, email addresses and other personal data.
- **Do not report security vulnerabilities in a public issue.** Follow [SECURITY.md](SECURITY.md) instead.

## Development setup

You need Rust 1.88 or newer (install it with [rustup](https://rustup.rs)) and git.

```sh
git clone https://github.com/luongnv89/mcc
cd mcc
cargo build
```

## Running checks

Run these before you open a pull request:

```sh
cargo test                                   # unit + integration tests
cargo fmt --all -- --check                   # formatting
cargo clippy --all-targets -- -D warnings    # lints
shellcheck install.sh                        # installer script
```

CI (`.github/workflows/ci.yml`) runs the same checks on every push and pull request: `cargo test` on Linux and macOS, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `shellcheck install.sh`, and `brew audit --strict` on `packaging/homebrew/mcc.rb`.

The tests never touch your real `~/.claude`, rc files, or keychain, and never reach the network. The integration suite sets `MCC_HOME` to a temp dir, puts a fake `claude` script on `PATH`, points `MCC_SECURITY_BIN` at a fake `security` script, and points `MCC_USAGE_URL` at a refused loopback port or a local stub server. Keep new tests sandboxed the same way.

## Making a change

1. Fork the repository and create a branch named `<type>/<issue>-<short-description>`, for example `fix/42-sync-dry-run-output`.
2. Keep each pull request to one logical change.
3. Add or update tests for any behavior change.
4. Update `README.md` when the change is visible to users (commands, options, output, environment variables).
5. Make sure `cargo test`, `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings` all pass.

### Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org): `type(scope): description (#N)`, where `type` is one of `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `style` or `perf`, and `#N` is the issue number. Write the description in the imperative mood, in lowercase, without a trailing period. For example:

```text
fix(sync): keep profile-only files when --prune is not given (#42)
```

### Pull requests

- Use a Conventional Commit style title.
- Link the issue it resolves with `Closes #N` in the description.
- Fill in the pull request template checklist.
- Be ready to respond to review feedback; a maintainer will review it as soon as they can.

## License of contributions

mcc is licensed under the [Apache License, Version 2.0](LICENSE). Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in mcc is licensed under the same terms, without any additional terms or conditions.
