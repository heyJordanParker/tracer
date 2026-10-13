# The Claude Code mod

The tracer mod puts `trace` to work inside Claude Code. It adds three things:

- **Hooks** that run `trace hook` on Claude Code's own events, so the facts arrive with the tools Claude already uses.
- **The `trace` Skill**, which teaches Claude every command, how to read its output, and when to reach for it instead of `grep` or a raw read.
- **The `trace` program**, installed for your machine.

## What each event adds

Every event runs `trace hook <event>`, which adds its context after the tool answers:

| Event | Claude gets |
|---|---|
| Session start, `/clear`, compaction | The repository primer: languages, layout, test and script folders, git state, and the most-depended-on code. A resume that holds it gets nothing, and a repository that changed since gets only the sections that changed. |
| Read | The file's facts and declarations, and the source of each function the lines it read call |
| Edit, Write | The facts the edit changed and the callers of each function it changed, never the lines it wrote |
| Grep | Each matching file's facts and its matches under the declarations that hold them. A Grep that printed its lines gets only the declarations. |
| Glob | Each matched file's facts |
| Bash running `trace` | The command's own answer carries the project docs it reaches that this context has not read |

A file's facts arrive once per context. A later read of the same file carries one heading line, and after an edit only the facts that changed. A listing that already showed a file's facts names the file alone the next time, and a read after it prints only the facts the listing did not show. When another agent changes a file, the next read shows the declarations it added or changed and one `moved:` line for the rest.

## Project docs, once

Claude Code loads `CLAUDE.md` files itself. tracer records every doc Claude Code loads, through `InstructionsLoaded`, and every doc it sends, so a doc never arrives twice:

- A `trace` command sends the docs of each directory it reaches, nearest first, the first time it reaches them.
- A doc too long for the message arrives in parts, each part continuing where the last one stopped.
- A doc that changes after it arrived comes back as only its changed lines.
- A compaction drops the docs from Claude's context, so tracer forgets them on `PreCompact` and sends them again when Claude's work reaches them. Claude Code never reloads a user rule with `paths:` after a compaction, so tracer sends each one Claude had loaded again when the session restarts.

## Subagents

Every Subagent keeps its own record, so a Subagent gets the facts and docs its own context lacks, whatever the main session has seen. The mod writes `--agent <id>` into each `trace` command a Subagent runs, because a Subagent's shell carries no agent id. Each record is keyed by its agent id, so a resumed Subagent picks up its record where it left off.

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
| `git blame`, `git grep`, `git show <ref>:<path>`, `git log <file>`, `git log -S` | `trace blame`, `trace grep`, `trace read --revision`, `trace history` |
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
| Run `trace` | Every Hook runs `trace hook`. |
| Add text Claude reads | Every fact, primer, and project doc tracer sends. |
| Change Claude's tool calls | Writes `--agent <id>` into a Subagent's `trace` commands. |

## When `trace` cannot run

A Hook that cannot run `trace` adds nothing, and Claude's tool call goes ahead unchanged. On every other event cmod logs `tracer: the <event> hook failed: trace hook <event> exited <code>: <reason>`. A Hook that runs past its 9-second deadline ends there and records nothing, so its context arrives on a later call.

## Develop the mod

```bash
cmod link .        # load this checkout in every new Claude Code session
cmod check .       # layout, imports, types, lint, plugin validation, and tests
cmod unlink .      # stop loading it
```

`cmod link` builds `trace` for this machine alone. The mod's code is in `src/`, its tests in `tests/`, and `hooks/register.ts` registers it with Claude Code.
