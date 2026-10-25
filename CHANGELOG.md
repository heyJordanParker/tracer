# Changelog

## 0.6.0

- The mod runs on cmod 4.1.1 and every newer cmod, and needs at least 4.1.1. Since 4.1.0, cmod keeps the notes tracer sends after a compaction out of the conversation you see.
- "Claude reads code only through tracer" is now "Claude reads files only through tracer". It refuses a raw read, search, or listing of any file or folder, in another repository or worktree or outside every repository, not only in the project. It refuses a command only when a `trace` command gives the same answer, and the refusal names that command with the flags that match. A command tracer has no answer for now runs instead of being sent to a command that answers something else: one with a flag such as `tail -f` or `grep -m 1`, an `awk` program, a `sed` substitution, and one whose output goes to a file, a variable, another program, or a test. `eza` and `bat` are refused like `ls` and `cat`, and `cat <file> | grep` like `grep` on the file.
- `trace grep` takes `-o`, each match alone, `-v`, the lines that do not match, `-w`, whole words, and `-x`, whole lines, on the working tree and with `--revision`.
- A subagent's compaction re-sends that subagent's own rules into its own context. Before, it re-sent the main session's rules into the subagent's context.
- A failing `trace hook` shows cmod's error toast, such as `The beforeSessionStart listener failed`, with `trace hook <event> exited <code>: <reason>` as its detail.
- Claude no longer gets the same context twice:
  - The primer leaves out what Claude Code already tells Claude: the working folder, git repository, worktree, platform, shell, operating system, date, the file count the `repo_context:` line carries, and the project rules Claude Code loaded. Its Environment keeps the repository root, when the working folder is below it, and the git user with their email.
  - A compaction no longer re-sends the user rules with `paths:`, because Claude Code loads them back itself at the next file they match.
  - A doc a `trace` command sent is not loaded again by Claude Code when Claude reads a file beside it.
  - A declaration whose header Claude just read, through a Read or in `trace read`'s own lines, shows by line and name.
  - A called function Claude already read whole shows as its file, line, and callers, without its header again.
  - A command that pages a long doc names the docs queued behind it once per context, instead of on every call.

## 0.5.1

- The mod runs on cmod 1.5 and needs it. Its failures show in cmod 1.5's toasts with Retry, Report, and Hide, and a failed install retries on its own.

## 0.5.0

- A hook and a shell `trace` call keep one session record whatever `TMPDIR` each sees, so a fact or a doc one of them showed Claude is not shown again by the other.
- A session in a large repository starts with every primer section that needs no index, before the hook's deadline, instead of an empty primer. The index then builds in the background, one build per user at a time, and the build ends when it finishes or its Claude Code session ends.
- Extra context waits about a second for the relations index and never longer, so one large file still being read no longer holds a hook past its deadline.
- A subagent's `trace` call inside `bash -c`, `xargs`, `find -exec`, `env`, `timeout`, `nice`, `nohup`, `command`, or `exec` is credited to the subagent.
- `trace docs archive`, `--triggering-tool`, and `--triggering-command` are gone. They did nothing.
- `trace api -o <file>` writes the API to a file without line numbers or facts, and `--revision <rev>` reads it from a commit, branch, or tag, so the files of two versions compare line by line.
- `trace api --visibility internal` replaces `restricted`. It adds what the language keeps inside its module or package, and what the author marks internal: an attribute, annotation, or decorator named `Internal`, an `@internal` doc tag, Rust's `#[doc(hidden)]`, or Ruby's `@api private`. Protected members move to `all`. The marks apply wherever tracer reads a file's exports, `read --diff` and `diff --symbols` included.
- `trace api` and `trace tree` list every depth unless `--max-depth N` limits it. `usages` and `dependencies` take `--max-depth N`, still 3 by default. `--depth` is gone.
- `grep --at` and `read --at` are now `--revision`.
- `trace api`, `read`, and `structure` list the files of a folder git ignores when you name it, the way ripgrep does.
- Reading the declarations of a large file through `ctags`, such as a 4 MB stylesheet, takes seconds instead of minutes.
- A clone made with `--filter=blob:none` stores its history the first time instead of walking it on every call, and reads a folder at an old revision in one fetch per 512 files instead of one per file.

## 0.4.0

- With "Claude reads code only through tracer" on, a `trace` command piped into `head`, `grep`, `sed`, `wc`, `jq`, or another filter is refused, because the filter cuts the context tracer fitted. The refusal names the `trace` command alone, or with `--json --filter <expression>` in place of `jq`.
- The mod needs cmod 0.3.0 or later.

## 0.3.0

- One setting, "Claude reads code only through tracer", off by default. With it on, Claude's `grep`, `cat`, `find`, `ls`, and `git blame` reads of the project are refused, and Claude runs the `trace` command the refusal names. A repository turns it on with `{ "tracerOnly": true }` in `.claude/cmods/tracer/options.json`.
- The `budget`, `primer`, `enrich`, and `projectDocs` settings are gone. tracer always sends its context, fitted to the 10,000 characters Claude Code shows of a hook message.

## 0.2.0

- The mod installs on macOS, where `/usr/bin/trace` is Apple's own tool. Claude Code runs the mod's `trace` from cmod's programs folder, and the install no longer stops.
- tracer's settings live in Claude Code's `/config`, and a repository sets them in `.claude/cmods/tracer/options.json`. The `state.json` settings are gone.
- The mod asks for the permissions it uses, and `/mods` turns each one off.
- A `trace` command after a `cd`, such as `cd src && trace read cart.ts`, gets the project docs of the folder it runs in.
- The mod needs cmod 0.2.2 or later.

## 0.1.0

The first release of tracer as its own project.

- `trace` ships as one binary for macOS and Linux, arm64 and x86-64, attached to each GitHub release.
- The tracer mod for Claude Code installs `trace` through cmod and runs it on Claude Code's own events: the repository primer at session start, file facts on Read, Edit, Write, Grep, and Glob, and each project doc once, the first time a `trace` command reaches it.
- `brew install heyJordanParker/tracer/tracer` installs `trace` from this repository's tap.
- `trace docs archive` moves a stopped subagent's record aside, so the mod keeps it without a script of its own.
