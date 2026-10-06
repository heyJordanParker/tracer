# Changelog

## 0.1.0

The first release of tracer as its own project.

- `trace` ships as one binary for macOS and Linux, arm64 and x86-64, attached to each GitHub release.
- The tracer mod for Claude Code installs `trace` through cmod and runs it on Claude Code's own events: the repository primer at session start, file facts on Read, Edit, Write, Grep, and Glob, and each project doc once, the first time a `trace` command reaches it.
- `brew install heyJordanParker/tracer/tracer` installs `trace` from this repository's tap.
- `trace docs archive` moves a stopped subagent's record aside, so the mod keeps it without a script of its own.
