# Commands

`trace <command> --help` prints every option of a command. This page groups the commands by the question each one answers.

## Options every command takes

| Option | What it does |
|---|---|
| `-C <dir>` | Runs as if `trace` started in `<dir>`, the way `git -C` does. It goes before the command, because `trace grep -C <n>` is ripgrep's context. |
| `--budget <chars>` | The characters the text output fits in, 30,000 by default. The budget cuts detail, never a file or a declaration, and the last line names the command for the rest. `0` is unbounded. |
| `--json` | One JSON document, `{query, context, results, counts}`: rows in `results`, each file's facts in `context.files`, totals in `counts`. |
| `--filter <jq>` | Runs a jq program over the `--json` output, in-process, in place of piping into `jq`. |
| `--agent <id>` | The agent whose session record the call reads and writes, in place of `TRACER_AGENT_ID`. |

Every path argument takes several paths, and a directory names the files under it.

## Orient

| Command | Answers |
|---|---|
| `trace context` | The repository primer: languages, layout, test and script folders, git state, and the most-depended-on code. |
| `trace context <file> [--offset N] [--limit N]` | One file's facts and the declarations of the lines read. |
| `trace stats [<path>]` | Lines of code, languages, and the complexity distribution, with one row per directory. |
| `trace list <dir> [--recent] [--limit N]` | One directory level from the disk, ignored files included, with size, code, and git columns. |
| `trace tree <path>` | The file tree with each file's complexity rank. |
| `trace info <paths> [--brief]` | A file's or directory's structure, complexity, importers, and imports. |
| `trace structure <paths>` | Every declaration of each file: classes, functions, properties, imports, exports. |

## Search

| Command | Answers |
|---|---|
| `trace grep <pattern> [paths] [-i] [-t type] [-g glob] [-C n] [-U] [-l \| -c] [--at ref]` | Text matches through ripgrep, each under the declarations that hold it. Takes ripgrep's own flags, and `-e <pattern>` for a pattern that starts with `-`. |
| `trace pattern <pattern> -t <lang> [paths]` | Structural matches through ast-grep, grouped the same way. |
| `trace find <pattern> [bases] [--type f\|d] [--sort complexity\|recent\|path]` | Files or directories by name or path glob, with their facts. |
| `trace logs [<pattern>] [--path P] [--file GLOB] [--since WHEN] [--until WHEN] [--around N]` | Timestamped log entries, ignored files included, a stack trace kept whole, `.gz` rotations read. |

## Symbols

| Command | Answers |
|---|---|
| `trace defines <symbol>` | Every declaration of the symbol. `Class::method` or `Class.method` picks one of several. |
| `trace callers <symbol>` | Every call site of the symbol, each with its confidence: `EXTRACTED`, `INFERRED`, or `AMBIGUOUS`. |
| `trace usages <symbol> [--depth N]` | What depends on the symbol, transitively. |
| `trace usages --path <P>` | The most-depended-on files in `P`. |
| `trace dependencies <symbol> [--depth N]` | What the symbol depends on, transitively. |
| `trace dependencies --path <P>` | The files in `P` that depend on the most others. |

If A imports B, B is a dependency of A, and A is a usage of B.

## Read

| Command | Answers |
|---|---|
| `trace read <paths>` | Whole files, cleaned, with line numbers, fitted to the budget. A cut read ends with the command for the next window. |
| `trace read <file> --method <name>` | One function or method. |
| `trace read <file> --lines L1:L2` | A line range. |
| `trace read <file> --between START END` | The section between two anchors. |
| `trace read <file> --at <ref> [--diff]` | The file at a commit, or its difference from that commit. |
| `trace read <file> --all` | Every line, with no budget. |

A windowed read adds a `calls:` block: each function the window calls, with its source when it is short and the session has not read it, and its other call sites.

## Change and history

| Command | Answers |
|---|---|
| `trace status [--state added\|renamed\|modified\|deleted\|untracked]` | The working tree's changed files, the most-imported first. |
| `trace diff [paths] [--base ref] [--symbols]` | What changed as lines, with the declarations each change touched: `changed`, `removed`, `added`, and `touches`. |
| `trace history <file> [<symbol>]` | A file's commits, or one function's line history. |
| `trace history --contains <text> [--regex] [--all]` | Every commit that added or removed the text, with the lines and the declaration each one changed. |
| `trace history --commit <ref>` | One commit in full: message, author, parents, files, and changed lines. |
| `trace blame <file> [<symbol>] [--lines L1:L2]` | Blame collapsed into regions, each with its commit subject. |

## Project docs

`trace docs` serves the `CLAUDE.md`, `AGENTS.md`, and `.claude/rules/` files that govern a path, and records what it sent, so a doc reaches an agent's context once.

| Command | Answers |
|---|---|
| `trace docs <paths> [--skip PATH]` | The docs of each path not yet loaded in this context, nearest first. |
| `trace docs <path> --graph` | The repository's whole docs graph, with `@include` edges and path-scoped rules. |
| `trace docs status [<path>]` | What this context has loaded, and from where. |
| `trace docs prime [<files>]` | Records docs the harness loaded itself, so tracer never sends them. |
| `trace docs reset` | Forgets what this context loaded, after a compaction or `/clear`. |
| `trace docs archive` | Moves a stopped subagent's record under `archived/`. A resumed subagent takes it back. |

## Install and cache

| Command | Answers |
|---|---|
| `trace doctor` | Checks `git`, `ripgrep`, `ast-grep`, `scc`, and `universal-ctags`, and prints the install command for each one missing. |
| `trace cache build [<path>]` | Builds the per-file facts and the relations index ahead of the first query. |
| `trace cache stats` | The cache's entries and size. |
| `trace cache clear [--all]` | Empties the per-file cache, or with `--all` the whole `.tracer-cache/`. |

## Environment

| Variable | What it does |
|---|---|
| `AGENT_SESSION_ID` | The session whose record `trace` reads and writes. `CODEX_THREAD_ID`, then `CLAUDE_CODE_SESSION_ID`, stand in when it is unset. Without a session, `trace` records nothing. |
| `TRACER_AGENT_ID` | The agent within the session, `root` by default. |
| `TRACE_TIMING` | Set to `1` to print one timing line per phase to stderr. |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | The command answered, including a search with no matches. |
| `2` | A path, symbol, or ref was not found. The message on stderr names what was missing. |
| `1` | Anything else failed. |
