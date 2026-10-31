---
name: trace
description: Code intelligence for the local codebase — search, callers, definitions, symbols, complexity, and file or method reads, each carrying the file's facts and the project docs not yet loaded. TRIGGER whenever you need to find, understand, or trace relationships in code instead of raw grep or unfiltered file reads.
---

# trace

- `trace` returns code intelligence: matches plus each file's facts — size, complexity, importers, git history, deploy branches, and the project docs not yet loaded.
- Text output is for Agents. `--json` output is for scripts and hooks: every `--json` result is one document, `{query, context, results, counts}`, with rows in `results`, each file's facts in `context.files`, and totals in `counts`.
- Every path argument takes several paths, and a directory names the files under it.
- `trace -C <dir> <command>` runs as if trace started in `<dir>`, the way `git -C` does: the commands answer for that repository, and relative paths resolve from `<dir>`. `-C` goes before the subcommand, because `trace grep -C <n>` is ripgrep's context.

## 1. Respect the execution Rules

### Use `trace` instead of raw grep, find, and file reads
Inside the local codebase, every search, listing, and read goes through the matching `trace` command.

### Do not pipe or redirect trace output
Use `trace <cmd> --json --filter '<jq expr>'` to narrow the rows of a search or listing. The filter requires `--json`, and it keeps `context` for the files its result names. Read code as `trace read` text: it prints a file's facts once per context, so the next window of that file costs one heading line.
Never: pipe `trace` into `grep`, `rg`, `head`, `tail`, `sed`, `awk`, `cut`, `sort`, `uniq`, `wc`, `jq`, or redirect it into a repository file.
Never: `--json` or `--filter` on `trace read` to get bare content.

### Do not use raw file-search or listing commands on repository paths
Use the matching `trace` subcommand instead of raw `cat`, `grep`, `rg`, `find`, `sed`, `awk`, `head`, `tail`, `ls`, and `tree` on repository paths.
Example: `trace list src/` replaces `ls src/`; `trace tree src/` replaces `tree src/`.

### Do not read repository code with git
Use the matching `trace` subcommand for the git forms trace already answers.
Example: `trace read <path> --revision <rev>` replaces `git show <rev>:<path>` and `git cat-file -p`; `trace grep <pattern>` replaces `git grep <pattern>`; `trace grep <pattern> --revision <rev>` replaces `git grep <pattern> <rev>`; `trace blame <file>` replaces `git blame`; `trace history <file>` replaces `git log -- <file>`; `trace history <file> <symbol>` replaces `git log -L`; `trace history --contains <pattern>` replaces `git log -S`; `trace diff` replaces `git diff --name-status`.
Every other git command passes, including `git status`, `git branch`, `git tag`, `git rev-parse`, `git reflog`, `git stash`, `git merge-base`, `git describe`, `git ls-files`, plain `git diff`, `git log -p`, and `git log -G`.

### Bound a search before sorting it
A search piped into `sort` or `uniq` holds its whole output in memory. Count with `rg -c`, cap with `rg --max-count <n>`, or put `head -<n>` before the sort.

### Find the newest artifact with `trace list --recent`
`trace list` lists the filesystem, so gitignored artifact directories (test runs, logs, builds) list too. `--recent` orders newest-first by mtime, `--limit N` caps the rows, and `entries=N` always carries the full count.
Example: `trace list tests/.runs --recent --limit 5` replaces `ls -t tests/.runs | head -5`.

## 2. Read an output cut to fit

### Expect every file and declaration, with less detail
Text output fits 30,000 characters, the longest Bash result Claude Code shows whole. The fit cuts detail, never coverage: the files the fewest others import lose their detail first, down to their path, and a listing too long even for bare paths names its files one line per directory (`dir/: a.php, b.php`).
A window's `calls:` block keeps a share of the budget and leaves the rest to the code: past one head per call it prints one head per function with every line that calls it (`L52, L60 Ledger::record  ledger.php:3`), then one line per file the calls reach (`ledger.php  {calls: 12}`).

IF a `calls:` line ends in `→ trace callers <name>`:
### Run the `trace callers` command it names
A window resolves a name only while it fits a fixed work budget, spent on the cheapest names first. A name too many files mention still names the function its line calls, with its source, but no `callers` count and no other call sites: `L4 nextTestSetup  test/lib/e2e-utils/index.ts:315 → trace callers nextTestSetup`. A name too many files declare prints `{defined_in: N}`, the number of files that declare it, and no file or source. `--whole` resolves neither.

### Run the command the last line names for the rest
A cut output ends with `[N of M files shortened to fit — whole: <command> --whole]`. Run that command, or narrow the paths, when the cut detail is what you need.

## 3. Orient before deep reads

### Start broad, then narrow
Run `trace context` first. Then use `trace stats`, `trace list`, `trace tree`, `trace info`, or `trace api` before reading code deeply. `trace api` prints a folder's public API with every header whole, so it answers what a module offers without reading its files.

Template:
  ```bash
  trace context
  trace stats
  trace list src
  trace info src/Entity/Loan.php --brief
  trace api src/Service
  ```

### Compare two versions' API with `trace api -o`
`trace api <paths> --revision <rev> -o <file>` writes the API a commit, branch, or tag holds, and `trace api <paths> -o <file>` the working tree's, with no line numbers or facts. Two such files differ only where the API changed, so compare them line by line.

## 4. Choose the smallest matching command

### Use relationship commands for symbols
Use `defines`, `callers`, `dependencies`, and `usages` when the question is about a symbol or dependency direction. A symbol takes a qualifier, `Class::method` or `Class.method`, to pick one of several same-named declarations.

Template:
  ```bash
  trace defines <symbol>
  trace callers <symbol>
  trace callers Loan::renew
  trace dependencies <symbol|--path P>
  trace usages <symbol|--path P>
  ```

### Use search commands for unknown names
Use `trace grep` for text in code, `trace logs` for text in a log file, `trace pattern` for structural search, and `trace find` for basenames and full paths. `trace grep` takes ripgrep's own flags and paths, and `-e <pattern>` for a pattern that starts with `-`, such as PHP's `->method(`.

Template:
  ```bash
  trace grep <pattern> [paths...] [-i] [-t <type>] [-g <glob>] [-C <n>] [-A <n>] [-B <n>] [-U] [-l | -c] [--revision <rev>]
  trace pattern <pattern> -t <type> [paths...]
  trace find <pattern> [bases...]
  ```

### Search a commit with `trace grep --revision <rev>`
`--revision` searches that commit instead of the working tree, which is what `git grep <pattern> <rev>` would answer. `-t` and `-g` select the same files on a commit as on the working tree; `-U` needs the working tree.

IF a search result names a `nested repository (its own search scope)`:
### Re-run the search with the base inside the nested repository
A vendored checkout carries its own `.git`, and enumeration never crosses into it from above.
Example: `trace find "*.min.js" themes` returns no matches and names `themes/vendortheme`; `trace find "*.min.js" themes/vendortheme` returns `bundle.min.js`.

### Use state commands for change review
Use `trace diff`, `trace status`, `trace history`, and `trace blame` to understand change scope, file history, and ownership.

Template:
  ```bash
  trace diff [paths...] [--base <rev>] [--symbols]
  trace status
  trace history [<file>] [<symbol>]
  trace history --contains <pattern> [--regex] [--whole]
  trace history --commit <rev>
  trace blame <file> [<symbol>] [--lines L1:L2]
  ```

### Find when code entered or left with `trace history --contains`
It lists the newest 29 commits that added or removed the string, then the oldest one, where it entered; `--whole` lists every commit between them. Each file shows `+` for the lines a commit added and `-` for the lines it removed, with the first line and its declaration. `trace history --commit <sha>` then gives that commit's reason.

### Read a commit's full body with `trace history --commit`
The subject says what changed and the body says why. `--commit <rev>` returns the message, author, parents, changed files, and changed lines in one call.
Never: `git show` for a commit message.

## 5. Read with trace

### Use `trace read` instead of raw reads
`trace read` strips Fluff and keeps line numbers. Read whole files, several files, methods, line ranges, anchor ranges, or a file at a git revision.

Template:
  ```bash
  trace read <paths...>
  trace read <file> --method <name>
  trace read <file> --lines L1:L2
  trace read <file> --between START END
  trace read <file> --revision <rev>
  trace read <file> --revision <rev> --diff
  trace read <file> --docs
  trace read <file> --whole
  ```

IF `--method` names a function the file does not declare:
### Read the lines the refusal names
The refusal lists the lines where the word appears and the `trace read <file> --lines a:b` command for them.

### Scope the read by `lines:` before reading a whole file
Every file's facts carry `lines:`. `trace read` fits the file to 30,000 characters: it keeps every declaration row and cuts the content at a whole line, ending with `[trimmed at L<n> of <total> — continue: trace read <file> --lines <n+1>:<end>]`. A trimmed read still costs a second call, so scope the first one: read the rows, then `--method <name>` or `--lines L1:L2`.
Never: `--whole` to skim a large file. It returns every line and takes the whole cost.

### Calibrate read depth by complexity
Use `trace stats` for the complexity distribution. Read whole the files past the repository's p95. Skim uniformly-low files only when the task does not need every line.

IF reading or searching a log file:
### Use `trace logs`
`trace grep` does not search a gitignored path and `trace read` fits the file to its budget. `trace logs` reads the files the ignore rules skip, returns one entry per line with stack traces kept whole, and windows by time so a rotated 80 MB directory returns a window.

Template:
  ```bash
  trace logs [<pattern>] [--path P] [--file GLOB] [--since WHEN] [--until WHEN] [--around N] [--limit N]
  trace logs <pattern> --path storage/logs
  trace logs <pattern> --since "2026-08-15 10:52" --until "2026-08-15 10:55"
  trace logs <pattern> --around 2
  trace logs <pattern> --file "laravel-*.log"
  trace logs --path /var/log/nginx --limit 5
  ```

## 6. Read the facts and rows instead of opening the file

- `trace context <file>`, `read`, file `info`, and `structure` print the file's facts as YAML front matter, then one row per declaration. `blame <file> <symbol>` prints the rows alone.
- A windowed `read --method`, `read --lines`, `read --between`, or `context --offset/--limit` prints only the rows whose span intersects the window, plus the rows that hold them.
- A list of many files prints each file's facts on one line: `path  {imported_by: 63, cyclomatic_complexity: 66, lines: 318, git: modified}`.

### Read the front matter as the file's facts
Keys use git and GitHub words; dates are ages. `directory` names the file's directory, its importers and imports, and its entries the first time you see that directory or after its entries change. Entries leave out what git ignores.

Template:
  ```yaml
  ---
  file: src/Entity/Loan.php
  lines: 164
  cyclomatic_complexity: 18
  complexity_rank: high
  imported_by: 21
  imports: 6
  git:
    status: modified
    commits: 41
    commits_last_30_days: 6
    first_commit: 4 months ago
    last_commit: "3 days ago by Jordan Parker: fix: …"
    on_deploy_branches: [main, production]
    usually_changed_with: [src/Service/Notifier.php]
    main_author: Jordan Parker
  directory:
    path: src/Entity/
    imported_by: 54
    imports: 9
    annotations: {Column: 61, Entity: 8, PrePersist: 3, PreUpdate: 3}
    entries: [Author.php, Book.php, …]
  ---
  ```

### Read a heading's headline as facts you already hold
A file's facts print once per context, whichever command showed them, and whole again after a compaction or `/clear`. A repeat carries the headline on its heading line. After an edit or another Agent's commit, a repeat shows only the lines that changed, and a fact that went away shows as `null`. Its block keeps the command's own answer, such as `info`'s `language` and `functions` or `structure`'s `declarations`. A `directory` you already hold prints only the lines that changed, such as a new `imported_by`, and none when nothing changed.
Example: `# src/Entity/Loan.php :: L24-L40  {imported_by: 21, cyclomatic_complexity: 18, lines: 164, git: modified}`

### Read the docs after an answer as the rules for those paths
In a session, every command that takes paths prints the project docs those paths reach after its answer, nearest first, each once per context. The docs past the budget are named under the `trace docs` command that sends them.

### Read the row as the source's own text
A row is `L<line>` and the declaration's header as written — attributes, modifiers, name, parameters, return type, heritage — with every body elided as `{ … }` and a data initializer as `= …`. A row indents two spaces under the declaration that holds it, and a function's complexity follows as a comment in the file's own syntax.

Template:
  ```text
  L10   #[ORM\Entity(repositoryClass: LoanRepository::class)]
        class Loan implements \JsonSerializable { … }
  L19     #[ORM\PrePersist]
          #[ORM\PreUpdate]
          public function stampDates(): void { … }  // complexity 1
  ```

### Read a search hit under the declarations that hold it
`grep` and `pattern` print each file's facts once, then every declaration that encloses a match once, then the matched lines, `L<n>:` for a match and `L<n>-` for a `-C` context line, as `git grep --show-function` does. A match on a declaration's own line stands in for it. The footer counts matches and files, and a searched word the graph knows names `trace callers <word>`.

Template:
  ```text
  src/Entity/Loan.php  {imported_by: 21, cyclomatic_complexity: 18, lines: 164, matches: 1}
    L10   #[ORM\Entity(repositoryClass: LoanRepository::class)] class Loan implements \JsonSerializable { … }
    L24     public function renew(int $days = 14): void { … }  // complexity 2
    L27:      $this->notifier->renewed($this);

  src/Service/Notifier.php  {imported_by: 9, cyclomatic_complexity: 4, lines: 48, matches: 1}
    L7    class Notifier { … }
    L9:     public function renewed(Loan $loan): void

  2 matches in 2 files
  renewed: 1 definition (function) · mentioned in 1 file → trace callers renewed
  ```

### Read the four declaration groups under a `diff` row
`changed:` gives the header before and after, `removed:` and `added:` the declarations the change dropped and introduced, and `touches:` every current-side declaration a hunk intersects.

Template:
  ```text
  changed: L24 public function renew(int $days = 14): void { … } → L24 public function renew(int $days = 21): void { … }
  removed: L34 fn signature_lists(files: &[String], repo_root: &Path) -> HashMap<String, Vec<Signature>> { … }
  added: L28 fn record_for(file: &str, line: i64, name: &str) -> Option<surface::Row> { … }
  touches: L99 pub fn run(symbol: &str, limit: usize, as_json: bool) -> Result<Value> { … }
  ```

## 7. Treat facts as a hypothesis

### Validate lifecycle before acting
An untracked or added file likely has no callers. A renamed file continues old code. A recent file with few commits may still be moving. An old file with many commits is a settled Precedent. A file with no `on_deploy_branches` is not deployed; `main` or `production` means Capability regression matters.

### Cross-check modify-or-stack Decisions
Before recommending modify or stack, read the nearest `Claude.md` or `Agents.md` and check `trace read <path> --revision origin/production`. A locally new file may already be deployed through worktrees, squashed baselines, or branch divergence.

### Read an annotation as a name, never a meaning
`directory.annotations` counts the attributes, decorators, and docblock tags the directory's files carry, and a row shows each one as written above its declaration. Tracer does not know what a name means; the project's Claude.md does. Read it before touching a declaration that carries one.

## 8. Keep dependency direction straight

- If A imports B, B is a dependency of A, and A is a usage of B.
- `trace usages --path P` returns the most-depended-on files in `P`.
- `trace dependencies --path P` returns the highest fan-out files in `P`.

## References

- Full command selection, flags, JSON payload shapes, `trace docs`, and `trace read` modes → [commands.md](references/commands.md)
- `trace hook`, the Hooks it serves for every Harness, identity propagation, and session environment variables → [hooks.md](references/hooks.md)
- Disk cache namespaces, invalidation, `trace doctor`, and install checks → [cache.md](references/cache.md)
