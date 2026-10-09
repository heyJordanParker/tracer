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

tracer has one setting, in Claude Code's `/config` under tracer:

```text
Claude reads code only through tracer                    [ off ]
Claude can't read or search this project with grep, cat, find,
or git blame. It uses tracer instead, so every read comes with
the file's callers, history, and docs.
```

With it on, a Bash command that reads the project's code without `trace` is refused, and Claude runs the `trace` command the refusal names:

```text
● Bash(grep -rn "Cart" src)
  ⎿ tracer: Claude reads this project's code only through tracer.
    Run this instead: trace grep Cart src
```

| Claude runs, on a file in the project | The refusal names |
|---|---|
| `grep -r`, `rg` | `trace grep` |
| `cat`, `head`, `tail`, `sed`, `awk` | `trace read` |
| `find` | `trace find` |
| `ls`, `tree` | `trace list`, `trace tree` |
| `git blame`, `git grep`, `git show <ref>:<path>`, `git log <file>`, `git log -S` | `trace blame`, `trace grep`, `trace read --at`, `trace history` |
| A `trace` command piped into `head`, `grep`, `sed`, `wc`, `jq`, or another filter | The `trace` command alone, or with `--json --filter <expression>` in place of `jq` |

Read, Grep, and Glob stay allowed, because tracer adds its facts to each of them. A command that changes files, such as `sed -i` or `find -delete`, runs, and so does every read outside the project and every git command `trace` has no answer for, such as `git status` or `git log -p`.

A repository turns it on for everyone working in it with `.claude/cmods/tracer/options.json`, committed with the repository:

```json
{ "tracerOnly": true }
```

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
