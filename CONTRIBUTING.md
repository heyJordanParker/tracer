# Contributing

## Layout

```text
tracer/
├── cli/                  the trace program, in Rust
│   ├── src/              the commands, the cache, and the extractors
│   ├── tests/            the black-box suite that runs the trace binary
│   └── xtask/            cargo xtask dist, the release builds
├── src/                  the Claude Code mod: one handler per event
├── hooks/                registers the mod with Claude Code
├── skills/trace/         the Skill that teaches Claude every command
├── tests/                the mod's tests
├── Formula/tracer.rb     the Homebrew formula
└── docs/                 the documentation
```

## Build and test the program

```bash
cd cli
cargo build --release
cargo test --release --workspace
cd tests
TRACE_BIN="$PWD/../.target/release/trace" cargo test
```

The black-box suite in `cli/tests` drives the binary `TRACE_BIN` names as a subprocess, against a git repository each test builds. It needs `git`, `ripgrep`, `ast-grep`, `scc`, and `universal-ctags`; `trace doctor` names any that are missing.

`cli/Claude.md` and `cli/tests/Claude.md` hold the facts about how the program works. Read them before changing it, and update them in the same change.

## Build and test the mod

```bash
bun install
bun test
cmod check .
```

`cmod check` runs the layout, import, type, lint, and plugin checks, then the tests. It has Claude Code write the types in `.claude-plugin/types/` when they are missing, so it needs Claude Code installed.

## Change the output

Agents read `trace`'s output, so a change to it is a change to its users' prompts. Keep the shape every command shares:

- Text output fits `--budget` and cuts detail, never a file or a declaration.
- `--json` is one `{query, context, results, counts}` document.
- A file's facts print once per agent context.

Pin every change in output with a test in `cli/tests`, and update the Skill in `skills/trace/` when an agent should use it differently.
