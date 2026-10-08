# trace Hooks

Use this Reference when diagnosing the tracer mod's Hooks, docs injection, identity propagation, or session logs.

## 1. Identify the Hook surface

### The Hooks are the tracer mod
The tracer mod registers its Hooks through cmod in `hooks/register.ts`, and `src/mod.ts` wires each event to its handler. Every handler runs `trace` and adds its output to the Agent's context.

- Each handler passes `--budget 10000`, because Claude Code saves a longer hook message to a file and shows the Agent its path and a 2,000-character preview.
- A handler that cannot run `trace` adds nothing, so the session goes on without tracer's context.

## 2. Match the Hook to the event

### SessionStart loads the repository primer
- Inside a git work tree it runs `trace context` and adds the primer.- `clear` resets the record first with `trace docs reset`.
- Every SessionStart records the docs Claude Code loads at session start, which it puts back after a compaction without reporting them: `trace docs prime` for the `Claude.md` chain, and `trace docs <cwd> --json` for the working directory's docs.

### InstructionsLoaded and PreCompact mirror Claude Code's own doc loads
- `InstructionsLoaded` runs `trace docs prime <file>`, so every doc Claude Code loads while the session runs is recorded as loaded.
- `PreCompact` resets the record with `trace docs reset`, because a compaction drops the docs from context.

### PreToolUse on Read, Edit, Write, Grep, and Glob attaches facts
One `trace` call per tool call.

- Read runs `trace context <file>` with the Read's offset and limit.
- Edit and Write add `--no-record`, and Edit reads only the lines its `old_string` replaces.
- Grep runs `trace grep` with the native Grep's pattern, `-i`, `glob`, `type`, and `multiline`, so it names the same files.
- Glob runs `trace find` with the same pattern and path.
- A failed call on an existing file adds `[trace context unavailable: <reason>]` under its path.

### PreToolUse on Bash sends the docs a trace command reaches
- Before a `trace` subcommand that takes paths, it runs `trace docs <paths> --source tracer_project_docs --triggering-tool Bash --triggering-command <cmd>`.
- The paths are every argument of the command that exists as a path, else the working directory; a `trace read` passes its files as `--skip`, because the read prints them. A `trace -C <dir>` call resolves its paths from `<dir>`.
- It adds the docs not yet loaded as Markdown, a doc too long for the message cut at a whole line and continued on the next trace command.
- Inside a Subagent it writes `--agent <agent_id>` into each `trace` call through `updatedInput`, because the Subagent's shell carries no agent id.

### PreToolUse on Bash refuses raw reads when the person turned tracerOnly on
With the `tracerOnly` setting on, a Bash command that reads the project's code with `grep -r`, `rg`, `cat`, `head`, `tail`, `sed`, `awk`, `find`, `ls`, `tree`, `git blame`, `git grep`, `git show <ref>:<path>`, or `git log <file>`/`-S` is refused. The refusal reads `tracer: Claude reads this project's code only through tracer.` and names the `trace` command that answers it. Run that command.

### SubagentStop archives the stopped Subagent's log
It runs `trace docs archive`, which moves `<repo>/.tracer-cache/sessions/<sid>/<aid>/` into `<repo>/.tracer-cache/sessions/<sid>/archived/<aid>/`. Trace reads fall back to the archived directory, and a resumed Subagent's first write takes it back.

## 3. Preserve identity propagation

### Hooks pass identity on the subprocess environment
- Every handler runs `trace` with `AGENT_SESSION_ID` set to the session id and, inside a Subagent, `TRACER_AGENT_ID` set to its agent id.
- Every handler runs `trace` from the event's `cwd`, so the session log lands in the event's repository. An Agent's `trace -C <dir>` call writes the same log, because `-C` keeps the session log in the directory the call started in.

### `AGENT_SESSION_ID` is the Harness-neutral carrier
Trace resolves `AGENT_SESSION_ID` first for the session log, so the same binary serves Claude Code, Codex, and any other Harness that sets it.

### Missing session id makes the log a no-op
Without a session id, docs dedupe and read coverage cannot work.

## 4. Read the environment variables

### Session identity variables are ordered
`trace` resolves `AGENT_SESSION_ID`, then `CODEX_THREAD_ID`, then `CLAUDE_CODE_SESSION_ID`.

### Agent identity defaults to root
`TRACER_AGENT_ID` identifies the Agent within the session and defaults to `root`.

### Trigger variables stamp log events
`TRACER_TRIGGERING_TOOL` and `TRACER_TRIGGERING_COMMAND` are written on log events. `trace docs` sets them from `--triggering-tool` and `--triggering-command`.

### The complexity backend variable is explicit
`TRACER_CCN_BACKEND` selects the complexity backend; the AST tree-sitter walker is the only supported backend.
