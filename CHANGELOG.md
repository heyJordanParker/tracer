# Changelog

## 0.10.0

- A `.sharp` file, written in PHP#, reads like a PHP file: `trace read`, `structure`, `defines`, `info`, and `api` show its classes, methods, properties, constants, and enum cases as rows, each function with its `// complexity N`, and its imports and call sites reach the relations commands. tracer reads the working tree's `.sharp` files through one `mago outline --json <file>...` per 512 files, and a file at a revision, or one that changed while `mago` read it, through `mago outline --json --stdin-input <file>`, once per file content. It reads its version 1 JSON, the Language Server Protocol's document-symbol tree. It runs the project's `vendor/bin/mago`, the one the Composer package `heyjordanparker/mago-sharp` installs, else the `mago` on `PATH`, which also takes over when the project's has no `outline` or can't run on this machine, as when Composer installed it in a Linux container.
- A cold `trace cache build` of 2,921 `.sharp` files starts `mago` 7 times, where one start per file took 2,922. A `.sharp` file's declarations at a revision are stored, so a second `trace diff` or `trace read --revision` starts no `mago` for the committed side it already read.
- A `mago` that fails, runs past 30 seconds, or prints an outline tracer can't read is named once per run as `mago outline failed on Store.sharp: <reason>`. One unreadable outline in a batch fails only its own file. A batch that fails, or prints another number of outlines than files, is named the same way, and each of its files is then read on its own. The file still prints its lines, every other file answers, and the run exits 2. Installing or replacing `mago` reads every `.sharp` file again, the way a new `ctags` does.
- A `mago` from a PHP# release before 0.8.1, which added `mago outline`, reads like no `mago`: a `.sharp` file prints its lines with no declarations, and nothing fails. tracer asks each `mago` build once whether it has `outline`, so a warm call starts no `mago`.
- `trace doctor` lists `mago` as optional, with its install command, or names the `mago` it found that has no `outline` command and the update that brings it. A missing `mago` never fails the check.
- `trace callers`, `usages`, `dependencies`, `imported_by`, and a window's `calls:` reach across PHP and PHP#, because a PHP# class is the engine class PHP calls. PHP's `use Paging\Store;` and PHP#'s `import Paging.Store;` both resolve through `composer.json`'s PSR-4 map to `Store.php` or `Store.sharp`, so `Store::make()` in PHP lists as an `EXTRACTED` caller of the PHP# method, and the PHP# importer counts in the PHP file's `imported_by`. A class inherits methods from its parent in the other language, as two PHP classes do. Every other language still resolves only with itself.
- A call chain through PHP# methods lists under `trace callers json` as the one `json` it calls, at the confidence an all-PHP chain gets, whether PHP writes it, `Client::make()->post()->json()`, or PHP# does, `Client.make().post().json()`. tracer reads each step's return type from the `returnType` that `mago outline` gives, and which call a PHP# call is made on from its `receiverCall`. A class in no namespace takes a chain too. A method that returns a built-in type or nothing, and a type from outside the repository, name no class, so the call stays a call on a value.
- A batch of `.sharp` files gets one more second for each file it reads, past the 30 seconds one file gets, so a slow but healthy `mago` still reads 512 files in one start.
- `trace callers` keeps a PHP static call on a class whose short name a class in another namespace shares, as `App\Models\User` and `App\Http\Resources\User` do in most Laravel apps. `Report::same()` and `self::same()` resolve to the `Report` the calling file's `use` items name, else the one in its own directory, where two `Report` classes left `same` with no callers. `\PB\Report::same()`, and `Sub\Report::same()` after `use PB\Sub;`, resolve to the class PSR-4 maps that full name to, the way a `use` of it resolves. A static call nothing tells apart lists each candidate `AMBIGUOUS` instead of none.
- A `trace` answer made while the relations index is still building fits in 30,000 characters with its `index: still building` line, one per repository it left out, so Claude Code no longer cuts its end. A hook answer fits its 10,000 the same way. A `trace read` of a long window spent 30,170, of a file in another repository 30,171, of two repositories 30,106, and a hook answer 10,175.

## 0.9.0

- With "Claude reads files only through tracer" on, Claude reads every file through tracer. A raw read, search, or listing of a file or folder is refused whatever its flags, so `grep -m 20`, `head -c 600`, `tail -5`, `awk`, a `sed` script, `find -mtime`, `git log`, `git diff`, `git status`, and a reader piped into `head` or `wc` no longer get past it. The refusal names the nearest `trace` command and what it leaves out: `Left out: -m 30, | head -5`. In the last week's sessions, 1,102 of 1,231 raw reads got past the old rule, which refused a command only when a `trace` command gave the exact same answer.
- A command whose output Claude never reads still runs: one whose output goes to a file, a variable, an `if`, or a program that is not a text filter, such as `curl`, `jq`, or `git apply`. So does one no `trace` command answers: `tail -f`, `sed -i`, `find -exec`, a binary file, and `git diff --no-index`.
- `trace read` has one mode: the file as written, every line numbered, separators and blank lines included. A whole file's leading license header or generated-file notice prints as one line naming the window that shows it, `L1-L14: license header (trace read src/Engine.java --lines 1:14)`. `--raw` is gone, and so is the `--json` query's `raw` key. Agents passed `--raw` on 23% of reads, learned from tracer's own refusals, its help, and its trim markers, because the default read dropped lines without saying so.
- A new session no longer shows tracer error toasts while cmod is still installing, such as `Could not read the tasks it listens under: cmod is installing` and `Changes nothing in session …`. The mod runs on `@cmodjs/core` 6.1.0, so a refusal from an installing cmod counts as a wait, a call nothing answered goes to the Log as a warning, and every toast uses cmod's aligned layout.
- The mod needs cmod 6.1.0 or newer.

## 0.8.3

- A Claude.md reaches Claude through `trace` without the HTML comments Claude Code removes, so a note written for people costs Claude no context. As in Claude Code, a comment on its own lines goes with the blank lines after it, and a comment inside a paragraph or a list item stays.

## 0.8.2

- tracer finds a Claude.md's `@path` imports by reading the Markdown the way Claude Code does, so Claude gets the same imported files through `trace` as through Claude Code. 0.8.1 scanned the lines and disagreed on less common Markdown. It loaded imports Claude Code skips, in an indented code block, an HTML block such as `<details>`, a double-backtick code span, a fence inside a longer fence, and the frontmatter. It missed imports Claude Code loads, such as `**@x.md**`, `~~@x.md~~`, `[@x.md](…)`, `<b>@x.md</b>`, an import after a code span holding `<!--`, and a list item's `` `cat @x.md now` ``. A table, even one without a leading `|`, an image's text, an escaped `\@x.md`, and the lines after a lone tag such as `<checklist>` under a list item import nothing, as in Claude Code.

## 0.8.1

- A Claude.md's `@path` imports reach Claude when it reads code through `trace`. tracer read only its own `@include <path>` form, so a nested doc arrived without the files it imports, such as `@docs/architecture.md`. tracer now follows Claude Code's rules: an `@` at the start of a word outside code and HTML comments, a path relative to the importing doc or from `~/`, `\ ` for a space, text files only, and four hops deep. A project doc's imports stay inside the repository, and `~/.claude/` docs import from anywhere. `@include <path>` is no longer read.
- Pressing Esc no longer shows a tracer error toast. A `trace hook` run that Claude Code stopped because you cancelled the turn showed `The beforeToolResult listener failed` with `trace hook PostToolUse: tracer: $.process.run(…) aborted`. The mod runs on `@cmodjs/core` 5.2.0, so a cancelled run ends quietly, and a run that fails on its own still shows why.
- The mod needs cmod 5.2.0 or newer.

## 0.8.0

- `--no-trim` prints any command's answer untrimmed, in place of `--whole`. Every trimmed answer says so with one word: `[3 of 40 files trimmed to fit — untrimmed: trace grep Cart src --no-trim]`, `[trimmed at L1 of 1 — untrimmed: trace read bundle.js --no-trim]` for a line longer than the budget, and `… 212 commits trimmed — untrimmed: …` in `history --contains`. The `--json` query key is `no_trim`.
- The `TRACER_BUDGET` environment variable is gone. Nothing sizes the 30,000 characters an answer is trimmed to.
- A tracer hook that fails or is cancelled names its real cause, such as `trace hook PostToolUseFailure: tracer: $.process.run(…) aborted`, instead of a made-up `exited 1`.

## 0.7.0

- Claude no longer guesses how much of an answer to read. Every text answer fits the 30,000 characters Claude Code shows whole, and `--budget` is gone, so a `trace` command reads like a `grep`, with no size to choose.
- `--whole` prints any command's answer uncut, and every cut answer ends with the same command with `--whole`. It replaces `--budget 0`, `read --all`, and `history --contains --all`. `list --all` and `cache clear --all` keep their meaning.

## 0.6.3

- The mod runs on cmod 5.0.0 and every newer cmod, and needs at least 5.0.0. Installing or updating tracer asks you to grant nothing, because cmod 5.0 has no permissions. tracer does the same work as before.
- tracer installs on a computer without a GitHub SSH key. Its marketplace lists cmod by its HTTPS address, so Claude Code no longer fails with `Dependency "cmod@tracer" has no git tag satisfying >=5.0.0`.

## 0.6.2

- Reading a Markdown file whose heading repeats its chapter's name, such as `## Usage` under `# Usage`, no longer runs until the CPU limit stops it. It reads at once, with every section under the chapter.

## 0.6.1

- The mod runs on cmod 4.3.0 and every newer cmod, and needs at least 4.3.0. Since 4.3.0, a session reloads tracer by itself after an update, once its running turn ends.
- Every `trace` call stops once it has spent 10 minutes of CPU time, and says so on stderr. A cold build of a 9,000-file repository spends under a minute.
- The index build a hook starts in the background runs at the lowest priority, so it takes only the CPU nothing else is waiting for.
- In a clone that lacks part of its history, such as a partial clone missing a tree, `trace` walks the history once per HEAD instead of on every call. In one 9,000-file partial clone, that took 2.7 seconds off every call.

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
