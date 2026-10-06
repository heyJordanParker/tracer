# trace cache

Use this Reference when a trace command is slow, stale, missing dependencies, or needs prebuild.

## 1. Know the cache namespaces

### `file/` stores per-file facts
It stores complexity, lines of code, language, imports, exports, and git activity. It invalidates per file when content changes.

### `sessions/<session_id>/<agent_id>/` stores Context logs
It stores `events.jsonl` and `view.json` for the per-session, per-Agent docs Context log. It no-ops without a session id.

## 2. Prebuild before heavy use

### Build the cache explicitly
The first Architecture command in a fresh repository builds the graph, typically five to thirty seconds for about one thousand files while respecting `.gitignore`. Later commands return well under a second.

Template:
  ```bash
  trace cache build [<path>]
  ```

## 3. Inspect or clear cache state

### Use cache verbs instead of deleting files
`trace cache stats` reports entries and bytes. `trace cache clear` empties the `file/` namespace, and `--all` removes the whole tree including session logs.

Template:
  ```bash
  trace cache stats
  trace cache clear
  trace cache clear --all
  ```

## 4. Verify installation

IF a trace command errors with missing dependencies:
### Run `trace doctor`
`trace doctor` verifies ast-grep, scc, universal-ctags, ripgrep, and git, then prints per-platform install instructions.

### The mod installs `trace` on PATH
Installing the tracer mod puts the `trace` release for the machine at `~/.local/bin/trace`. Homebrew installs it with `brew install heyJordanParker/tracer/tracer`, and a source build is `cargo build --release` in `cli/`, which writes `cli/.target/release/trace`.
