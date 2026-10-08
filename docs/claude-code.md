# The Claude Code mod

The tracer mod puts `trace` to work inside Claude Code. It adds three things:

- **Hooks** that run `trace` on Claude Code's own events, so the facts arrive with the tools Claude already uses.
- **The `trace` Skill**, which teaches Claude every command, how to read its output, and when to reach for it instead of `grep` or a raw read.
- **The `trace` program**, installed for your machine.

## What each event adds

| Event | `trace` runs | Claude gets |
|---|---|---|
| Session start, resume, `/clear`, compaction | `trace context` | The repository primer: languages, layout, test and script folders, git state, and the most-depended-on code |
| Read | `trace context <file>` | The file's facts and the declarations of the lines it reads, with the source of each function those lines call |
| Edit | `trace context <file> --no-record` | The declarations around the lines it replaces |
| Write | `trace context <file> --no-record` | The facts of the file it overwrites |
| Grep | `trace grep` | Each match under the declarations that hold it |
| Glob | `trace find` | Each matched file with its facts |
| Bash running `trace` | `trace docs <paths>` | The project docs the command reaches that this context has not read |

A file's facts arrive once per context. A later read of the same file carries one heading line, and after an edit only the facts that changed.

## Project docs, once

Claude Code loads `CLAUDE.md` files itself. tracer records every doc Claude Code loads, through `InstructionsLoaded`, and every doc it sends, so a doc never arrives twice:

- A `trace` command sends the docs of each directory it reaches, nearest first, the first time it reaches them.
- A doc too long for the message arrives in parts, each part continuing where the last one stopped.
- A compaction drops the docs from Claude's context, so tracer forgets them on `PreCompact` and sends them again when Claude's work reaches them.

## Subagents

Every Subagent keeps its own record, so a Subagent gets the facts and docs its own context lacks, whatever the main session has seen. The mod writes `--agent <id>` into each `trace` command a Subagent runs, because a Subagent's shell carries no agent id, and archives the Subagent's record when it stops. A resumed Subagent picks up its record where it left off.

## Settings

Change a setting in Claude Code's `/config`, under tracer. A repository sets them for everyone working in it with `.claude/cmods/tracer/options.json`, committed with the repository, and that file wins over `/config`:

```json
{ "budget": 6000, "primer": false }
```

| Setting | `/config` shows | Default | What it does |
|---|---|---|---|
| `budget` | Context budget | `10000` | The characters each Hook's context fits in, at least 1000. Claude Code moves a longer hook message to a file and shows a 2,000-character preview. |
| `primer` | Repository primer | `true` | Sends the repository primer at session start. |
| `enrich` | File facts | `true` | Adds facts to Read, Edit, Write, Grep, and Glob. |
| `projectDocs` | Project docs | `true` | Sends the project docs a `trace` command reaches. |

## Permissions

cmod asks for these when it installs tracer, and `/mods` turns each one off:

| Permission | What tracer does with it |
|---|---|
| Run `trace` | Every Hook runs `trace`. |
| Run `git` | The session start checks it is in a git repository before it builds the primer. |
| Add text Claude reads | Every fact, primer, and project doc tracer sends. |
| Change Claude's tool calls | Writes `--agent <id>` into a Subagent's `trace` commands. |

## When `trace` cannot run

A Hook that cannot run `trace` adds nothing, and Claude's tool call goes ahead unchanged. A Read of a file whose facts fail adds one line naming the reason, so Claude knows the facts are missing rather than empty.

## Develop the mod

```bash
cmod link .        # load this checkout in every new Claude Code session
cmod check .       # layout, imports, types, lint, plugin validation, and tests
cmod unlink .      # stop loading it
```

`cmod link` builds `trace` for this machine alone. The mod's code is in `src/`, its tests in `tests/`, and `hooks/register.ts` registers it with Claude Code.
