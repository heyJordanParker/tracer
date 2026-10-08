# Changelog

## 0.3.0

- One setting, "Claude reads code only through tracer", off by default. With it on, Claude's `grep`, `cat`, `find`, `ls`, and `git blame` reads of the project are refused, and Claude runs the `trace` command the refusal names. A repository turns it on with `{ "tracerOnly": true }` in `.claude/cmods/tracer/options.json`.
- The `budget`, `primer`, `enrich`, and `projectDocs` settings are gone. tracer always sends its context, fitted to the 10,000 characters Claude Code shows of a hook message.

## 0.2.0

- The mod installs on macOS, where `/usr/bin/trace` is Apple's own tool. Claude Code runs the mod's `trace` from cmod's programs folder, and the install no longer stops.
- tracer's settings live in Claude Code's `/config`, and a repository sets them in `.claude/cmods/tracer/options.json`. The `state.json` settings are gone.
- The mod asks for the permissions it uses, and `/mods` turns each one off.
- A `trace` command after a `cd`, such as `cd src && trace read cart.ts`, gets the project docs of the folder it runs in.
- The mod needs cmod 0.2.2 or later.

## 0.1.0

The first release of tracer as its own project.

- `trace` ships as one binary for macOS and Linux, arm64 and x86-64, attached to each GitHub release.
- The tracer mod for Claude Code installs `trace` through cmod and runs it on Claude Code's own events: the repository primer at session start, file facts on Read, Edit, Write, Grep, and Glob, and each project doc once, the first time a `trace` command reaches it.
- `brew install heyJordanParker/tracer/tracer` installs `trace` from this repository's tap.
- `trace docs archive` moves a stopped subagent's record aside, so the mod keeps it without a script of its own.
