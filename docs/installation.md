# Installation

tracer installs two ways, and both can live on one machine: Claude Code runs the mod's `trace`, and a terminal runs the first `trace` its `PATH` finds.

| You want | Install |
|---|---|
| tracer in Claude Code, with its Hooks and Skill | The Claude Code mod |
| `trace` in a terminal, a script, or another agent harness | Homebrew, or a source build |

## The Claude Code mod

tracer is a [cmod](https://github.com/heyJordanParker/cmod) mod: a Claude Code plugin that installs its own program.

```bash
npm install -g @cmodjs/cli
cmod install heyJordanParker/tracer
```

`cmod install` adds tracer's marketplace to Claude Code, installs the plugin and the cmod plugin it depends on, and fetches `trace`:

1. It downloads `trace-<os>-<arch>` from the GitHub release that matches the plugin's version.
2. It checks the file's SHA-256 against the release's `SHA256SUMS`, and runs `trace --version`.
3. It keeps the file under `~/.local/share/cmod/bin/trace/<version>/`, and links it into cmod's programs folder, `~/.local/share/cmod/programs/`, and into `~/.local/bin/trace`.

Start a new Claude Code session to load the mod. The mod's Hooks and Claude's Bash commands run the `trace` in cmod's programs folder, so macOS's own `/usr/bin/trace`, or a Homebrew `trace`, never stands in for it.

A terminal runs the first `trace` its `PATH` finds. On macOS that is `/usr/bin/trace`, Apple's system tracing tool, unless `~/.local/bin` comes before `/usr/bin`, or Homebrew installed tracer too. cmod logs which `trace` a terminal runs when it installs the mod.

```bash
cmod update tracer   # move to the latest release
cmod remove tracer   # uninstall the plugin and its trace
```

## Homebrew

The formula lives in this repository, so the repository is its own tap:

```bash
brew tap heyJordanParker/tracer https://github.com/heyJordanParker/tracer
brew install heyJordanParker/tracer/tracer
```

Homebrew installs the `trace` release for your machine, macOS or Linux, arm64 or x86-64, with `ripgrep`, `ast-grep`, `scc`, and `universal-ctags` beside it. `brew upgrade tracer` moves to the latest release.

Homebrew loads a formula from a tap outside its own only once you trust it. Installing by the full name, `heyJordanParker/tracer/tracer`, trusts that one formula. The tap needs its URL because the repository is named `tracer`, not `homebrew-tracer`.

## Build from source

`trace` builds with a stable Rust toolchain:

```bash
git clone https://github.com/heyJordanParker/tracer
cd tracer/cli
cargo build --release
```

The binary lands at `cli/.target/release/trace`. Copy it onto your `PATH`, then install the programs it runs:

```bash
brew install ripgrep ast-grep scc universal-ctags   # macOS
```

On Linux, install `ripgrep` and `universal-ctags` from your distribution, and `ast-grep` and `scc` from their release pages, or use Homebrew on Linux.

## Check the install

```bash
trace --version
trace doctor
```

`trace doctor` checks `git`, `ripgrep`, `ast-grep`, `scc`, and `universal-ctags`, and prints the install command for each one that is missing. Every other command fails with the same message while one is missing.

## Platforms

Every release ships four builds:

| Machine | File |
|---|---|
| macOS, Apple silicon | `trace-darwin-arm64` |
| macOS, Intel | `trace-darwin-x64` |
| Linux, arm64 | `trace-linux-arm64` |
| Linux, x86-64 | `trace-linux-x64` |

The Linux builds link against glibc 2.17, so they run on every distribution still in service.
