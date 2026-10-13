# trace cache

Use this Reference when a trace command is slow, stale, missing dependencies, or needs prebuild.

## 1. Know what the cache holds

### `file/` stores per-file facts
It stores complexity, lines of code, language, declarations with their visibility, imports and re-exports, and git activity. It invalidates per file when content changes.

### Session records live outside the cache
Each Agent's record of what it was shown is `tracer-<uid>/sessions/<session_id>/<agent_id>/record.json` in the user's own temporary folder, never under `TMPDIR`. Without a session id nothing is recorded. `trace docs reset` clears it, and no cache verb touches it.

## 2. Prebuild before heavy use

### Build the cache explicitly
The first Architecture command in a fresh repository builds the graph, typically five to thirty seconds for about one thousand files while respecting `.gitignore`. Later commands return well under a second.

### A context line naming `cache build` means the index is still building
`index: still building, so imported_by, imports, and calls are left out.` follows a read or search whose relations weren't ready within a second. The facts it printed are correct, and answers such as `trace callers` wait for the index instead. Run the `trace cache build` it names, or keep working and later calls finish the index.

Template:
  ```bash
  trace cache build [<path>]
  ```

## 3. Inspect or clear cache state

### Use cache verbs instead of deleting files
`trace cache stats` reports entries and bytes. `trace cache clear` empties the `file/` namespace, and `--all` removes the whole `.tracer-cache/` tree.

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
Installing the tracer mod puts the `trace` release for the machine in cmod's programs folder, `~/.local/share/cmod/programs/`, which Claude Code's Bash finds first. Homebrew installs it with `brew install heyJordanParker/tracer/tracer` after tapping `https://github.com/heyJordanParker/tracer`, and a source build is `cargo build --release` in `cli/`, which writes `cli/.target/release/trace`.
