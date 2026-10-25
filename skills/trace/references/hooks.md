# trace Hooks

Use this Reference when diagnosing tracer's Hooks, docs injection, identity propagation, or session records.

## 1. Identify the Hook surface

### Every Hook is `trace hook <event>`
`trace hook <event> --harness claude|codex --deadline-ms <n>` reads the Hook's input as JSON on stdin and prints its answer as JSON on stdout. Everything tracer does on a Hook event lives in it, for every Harness.

- The tracer mod is a pass-through: `src/mod.ts` runs `trace hook` for each event and returns its answer. `--tracer-only` turns on the refusal.
- Codex wires one line per event to the same command, with `--harness codex`.
- An answer fits 10,000 characters, because Claude Code saves a longer hook message to a file and shows the Agent its path and a 2,000-character preview.
- Past `--deadline-ms` the Harness throws the answer away, so `trace hook` ends itself there and records nothing as shown.
- A Hook that cannot run `trace` adds nothing on a tool call, so the call goes on. On every other event cmod logs the failure.

## 2. Match the Hook to the event

### SessionStart records the start docs and offers the primer
- It records the docs the Harness loaded at start. Claude Code loads its user-global doc, the doc chain from the repository root to the working folder, and every rule file with no `paths:`. Codex loads its own discovery: `$CODEX_HOME/AGENTS.override.md` or `AGENTS.md`, then per folder the first of `AGENTS.override.md`, `AGENTS.md`, and the config's `project_doc_fallback_filenames`, within `project_doc_max_bytes`.
- It offers the primer once per context: a resume that holds it gets nothing, and a repository that changed gets only the sections that changed.
- `clear` forgets the record first.
- The primer leaves out what Claude Code already tells Claude: the working folder, git repository, worktree, platform, shell, operating system, date, and the rules it loaded.
- The primer answers before the deadline with every section ready by then. The sections that need no relations index are read beside its build. When the index is not ready, the Spine and the import counts and complexity it stores are left out.
- Any Hook that leaves the relations index out starts `trace cache build` in the background after it answers. One build runs per user at a time, and it ends when it finishes or when the Claude Code session in `CLAUDE_PID` ends.
- On Codex it also sends the working folder's docs and rules Codex never loads itself.

### PreCompact forgets the record
A compaction drops what the Agent held. Claude Code loads every doc and rule back itself when the Agent's work reaches it.

### SubagentStart and InstructionsLoaded record what the Harness loaded
- `SubagentStart` records the start docs under the Subagent's agent id.
- `InstructionsLoaded` records each doc Claude Code loads, under the agent id in the input. A doc the Agent holds whole, as a `trace` command sent it, is refused, so Claude Code does not load it again.

### PreToolUse on a shell command names the Subagent and refuses raw reads
- Inside a Subagent it writes `--agent <agent_id>` after each `trace` program word through `updatedInput`, because the Subagent's shell carries no agent id. `cli/src/shell.rs` reads the command's structure, so a `trace` inside a quoted argument stays text, and a `trace` that `bash -c`, `xargs`, `find -exec`, `env`, `timeout`, `nice`, `nohup`, `command`, or `exec` runs gets the flag too.
- With `--tracer-only`, a command that reads, searches, or lists any file or folder with `grep`, `rg`, `git grep`, `cat`, `bat`, `head`, `sed -n`, `find`, `ls`, `eza`, `tree`, `git blame`, `git show`, or `git log <file>`/`-S`/`-G` is refused when a `trace` command gives the same answer, and so is a `trace` command piped into a filter such as `head`, `grep`, `wc`, or `jq`. The refusal reads `tracer: Claude reads files only through tracer.` and names the `trace` command that answers it. Run that command. A command with a flag no `trace` command matches, such as `grep -m 1`, runs, and so does one whose output goes to a file, a variable, another program, or a test.

### PostToolUse adds the context around what a tool returned or wrote
- Read: the file's facts, its declaration rows, and the calls in the lines read, once per context. A row whose header the Read returned shows as its line and name. The lines read are recorded as seen. A Read refused for its size still gets the context, with nothing seen.
- Grep: each matching file's facts and its matches under their declarations. In `content` mode, whose lines the Grep printed, only the declarations.
- Glob: each returned file's facts.
- Edit, MultiEdit, Write, NotebookEdit, and Codex's `apply_patch`: the Agent's own edit. Its lines are seen and its rows recorded as shown, so nothing it wrote comes back. Facts the edit changed print, and so do the callers of each function it changed.
- Codex loads no docs of its own, so a file its `apply_patch`, Read, or `cat`-style shell read reaches also gets its docs.
- Claude Code loads a file's docs itself when it reads it, so `trace hook` never sends docs on a Claude Code Read.

### SubagentStop has no handler
Each Agent's record is keyed by its own agent id, so a resumed Subagent finds it where it left it.

## 3. Preserve identity propagation

### `trace hook` takes identity from the Hook input
- The input's `session_id` names the session, and its `agent_id` names the Subagent.
- A session's records live at `tracer-<uid>/sessions/<session>/` in the user's own temporary folder, outside every repository and never under `TMPDIR`, so every call in the session writes the same record whatever its `cwd` or `-C`. A sandboxed shell that cannot write there reads the record and records nothing.

### `AGENT_SESSION_ID` is the Harness-neutral carrier
Outside a Hook, `trace` resolves `AGENT_SESSION_ID` first for the session record, so the same binary serves Claude Code, Codex, and any other Harness that sets it.

### Missing session id records nothing
Without a session id, docs dedupe and read coverage cannot work.

## 4. Read the environment variables

### Session identity variables are ordered
`trace` resolves `AGENT_SESSION_ID`, then `CODEX_THREAD_ID`, then `CLAUDE_CODE_SESSION_ID`.

### Agent identity defaults to root
`--agent`, else `TRACER_AGENT_ID`, identifies the Agent within the session, and it defaults to `root`.
