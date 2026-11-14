# The Claude Code mod

The tracer mod puts `trace` to work inside Claude Code. It adds three things:

- **Hooks** that run `trace hook` on Claude Code's own events, so the facts arrive with the tools Claude already uses.
- **The `trace` Skill**, which teaches Claude every command, how to read its output, and when to reach for it instead of `grep` or a raw read.
- **The `trace` program**, installed for your machine.

## What each event adds

Every event runs `trace hook <event>`, which adds its context after the tool answers:

| Event | Claude gets |
|---|---|
| Session start, `/clear`, compaction | The repository primer: languages, layout, test and script folders, git state, the rules that apply to your changes, and the most-depended-on code. It leaves out what Claude Code already tells Claude, such as the working folder, platform, and date. A resume that holds it gets nothing, and a repository that changed since gets only the sections that changed. |
| Read | The file's facts and declarations, and the source of each function the lines it read call. A declaration whose header the Read returned shows by name. |
| Edit, Write | The facts the edit changed and the callers of each function it changed, never the lines it wrote |
| Grep | Each matching file's facts and its matches under the declarations that hold them. A Grep that printed its lines gets only the declarations. |
| Glob | Each matched file's facts |
| Bash running `trace` | The command's own answer carries the project docs it reaches that this context has not read |

A file's facts arrive once per context. A later read of the same file carries one heading line, and after an edit only the facts that changed. A listing that already showed a file's facts names the file alone the next time, and a read after it prints only the facts the listing did not show. When another agent changes a file, the next read shows the declarations it added or changed and one `moved:` line for the rest.

## Project docs, once

Claude Code loads `CLAUDE.md` files itself. tracer records every doc Claude Code loads, through `InstructionsLoaded`, and every doc it sends, so a doc never arrives twice:

- A `trace` command sends the docs of each directory it reaches, nearest first, the first time it reaches them.
- A doc a `trace` command sent is not loaded again when Claude reads a file beside it.
- A doc too long for the message arrives in parts, each part continuing where the last one stopped, and the docs queued behind it are named once.
- A doc that changes after it arrived comes back as only its changed lines.
- A compaction drops the docs from Claude's context, so tracer forgets them on `PreCompact`. Claude Code loads each one back when Claude's work reaches it, and so does the next `trace` command that reaches it.

## Subagents

Every Subagent keeps its own record, so a Subagent gets the facts and docs its own context lacks, whatever the main session has seen. The mod writes `--agent <id>` into each `trace` command a Subagent runs, because a Subagent's shell carries no agent id. Each record is keyed by its agent id, so a resumed Subagent picks up its record where it left off.

## Settings

tracer has one setting, in Claude Code's `/config` under tracer:

```text
Claude reads files only through tracer                   [ off ]
Claude can't read, search, or list files with grep, cat, ls,
find, or git, whatever their flags. It uses tracer instead,
so every read comes with what tracer knows about the file.
```

With it on, a Bash command that reads, searches, or lists a file or folder without `trace` is refused, whatever its flags, wherever the file is: in the project, in another repository or worktree, or outside every repository. Claude runs the `trace` command the refusal names, and the refusal says what that command leaves out:

```text
● Bash(grep -n -m 30 "Cart" src/Cart.php | head -5)
  ⎿ tracer: Claude reads files only through tracer.
    Run this instead: trace grep Cart src/Cart.php
    Left out: -m 30, | head -5. trace numbers every line and fits every answer to the 30,000 characters Claude Code shows whole; `trace grep --help` lists what it takes.
```

| Claude runs | The refusal names |
|---|---|
| `grep`, `egrep`, `fgrep`, `rg`, `git grep` | `trace grep`, with the flags it shares: `-i`, `-o`, `-v`, `-w`, `-x`, `-l`, `-c`, `-A`, `-B`, `-C`, `-F`, `-g`, `-t`, `--include`, `--exclude` |
| `cat`, `bat`, `head`, `tail`, `sed`, `awk` | `trace read`, with `--lines` for the lines `head -n`, `tail -n`, and `sed -n 'A,Bp'` print |
| `find` | `trace find` of its `-name` or `-path` pattern |
| `ls`, `eza`, `tree -L 1` | `trace list`, with `--all` for `-a` and `--recent` for `ls -t`, or `trace info` for a file |
| `tree`, `ls -R`, `eza --tree` | `trace tree`, with `--max-depth` for `-L` |
| `git blame`, `git show <ref>:<file>`, `git show <ref>`, `git log`, `git log -S`, `git log -G`, `git diff`, `git status` | `trace blame`, `trace read --revision`, `trace history --commit`, `trace history`, `trace diff`, `trace status` |
| Any of them piped into `head`, `grep`, `sed`, `wc`, or another text filter | The same `trace` command, with the filter named as left out |
| A `trace` command piped into `head`, `grep`, `sed`, `wc`, `jq`, or another filter | The `trace` command alone, or with `--json --filter <expression>` in place of `jq` |

Read, Grep, and Glob stay allowed, because tracer adds its facts to each of them. A command whose output Claude never reads, or that tracer has no answer for, runs:

- one whose output goes to a file, a variable, or the condition of an `if`, and one piped into a program that is not a text filter, such as `curl`, `jq`, or `git apply`
- `tail -f` on a growing file, `sed -i`, `find -exec`, and a search with a back-reference
- a binary file, a device such as `/dev/null`, and `tree` on a folder git ignores
- every git command `trace` has no answer for, such as `git branch` or `git diff --no-index`

A repository turns it on for everyone working in it with `.claude/cmods/tracer/options.json`, committed with the repository:

```json
{ "tracerOnly": true }
```

## When `trace` cannot run

A Hook that cannot run `trace` adds nothing, and Claude's tool call goes ahead unchanged. On every other event cmod shows an error toast, such as `The beforeSessionStart listener failed`, whose detail reads `trace hook <event> exited <code>: <reason>`. A Hook that runs past its 9-second deadline ends there and records nothing, so its context arrives on a later call.

## Develop the mod

```bash
cmod link .        # load this checkout in every new Claude Code session
cmod check .       # layout, imports, types, lint, plugin validation, and tests
cmod unlink .      # stop loading it
```

`cmod link` builds `trace` for this machine alone. The mod's code is in `src/`, its tests in `tests/`, and `hooks/register.ts` registers it with Claude Code.
