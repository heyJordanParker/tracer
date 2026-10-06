# Releasing

One command publishes a version everywhere: the Claude Code mod, the `trace` builds, and the Homebrew formula.

## Before you release

You need a Mac with:

- a stable Rust toolchain from `rustup`, with the four release targets:

  ```bash
  rustup target add aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu
  ```

- `zig` and `cargo-zigbuild`, which cross-compile the Linux builds:

  ```bash
  brew install zig
  cargo install cargo-zigbuild
  ```

- `cmod`, and `gh` logged in with push access to the repository.

## Release

1. Raise `version` in `.claude-plugin/plugin.json` and in `cli/Cargo.toml` to the same number, and add the version to `CHANGELOG.md`.
2. Run `cmod check .` and the Rust suites, then commit.
3. Run `cmod publish --dry-run .` to build everything without pushing, and read what it would release.
4. Discard the `.claude-plugin/marketplace.json` the dry run wrote, then run `cmod publish .`.

`cmod publish`:

1. Builds the release from the committed files listed in `package.json` `files`, bundles the Hooks, and validates it with `claude plugin validate --strict`.
2. Runs `cargo xtask dist` in `cli/`, which builds `trace-darwin-arm64`, `trace-darwin-x64`, `trace-linux-arm64`, and `trace-linux-x64` into `cli/dist/`.
3. Writes `SHA256SUMS` for the release archive and every build, and `.claude-plugin/marketplace.json` pointing at the archive.
4. Commits the marketplace, tags `v<version>`, pushes the commit, the tag, and the `release` branch, and creates the GitHub release with every file attached.

## Homebrew follows

Publishing the GitHub release runs the `Homebrew` workflow. It downloads the release's `SHA256SUMS`, runs `scripts/update-formula.sh` to point `Formula/tracer.rb` at the new version and its four builds, and commits the formula. `brew upgrade tracer` picks it up from there.

To point the formula at a release by hand:

```bash
gh release download v0.1.0 --pattern SHA256SUMS --dir /tmp/tracer-release
scripts/update-formula.sh 0.1.0 /tmp/tracer-release/SHA256SUMS
```

## Check a release

```bash
gh release view v0.1.0
brew update
brew upgrade tracer
trace --version
```
