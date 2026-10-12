# How it works

`trace` is one Rust binary. It parses code with tree-sitter grammars compiled into it, and runs `git`, `ripgrep`, `ast-grep`, `scc`, and `universal-ctags` for what they do best. Everything it learns about a repository lives in `.tracer-cache/` at the repository's root, which writes its own `.gitignore`.

## Declarations

Each file's declarations come from a tree-sitter extractor for C, Go, Java, PHP, Python, Ruby, Rust, and TypeScript, TSX, and JSX, plus Bash and Lua for complexity. A declaration's row is its own source text, attributes, modifiers, name, parameters, return type, and heritage, with every body elided as `{ … }`. A language with no extractor falls back to `universal-ctags`.

Cyclomatic complexity is counted per function by walking its tree for decision points: `if`, each loop, each `case` and `catch`, `&&`, `||`, `??`, ternaries, and comprehension clauses. `scc` counts the repository's lines and languages once, into a snapshot that a later call recounts only for the files that changed, and `trace stats` reads it.

## The cache

`.tracer-cache/file/schema<N>/` holds everything one version of the cache format writes:

- **Per-file facts**, keyed by the file's contents and path: its declarations with their visibility, its imports and re-exports, and its complexity. A changed file gets a new entry, and the superseded one is removed.
- **The relations index**: every file's imports, the files that import each file, and where each name is declared and mentioned. A call rewrites it in place, re-reading only the files whose contents moved.
- **The commit index**: the newest 4,000 commits, with every file each one changed. A call after `HEAD` moves reads only the new commits.
- **The tracked-file listing**, with the stamps of every directory and ignore file it read, so a warm call checks stamps instead of walking the tree.

A process holds an index's own lock while it updates that index, so concurrent calls wait for one update and read its result. A cache directory of another format version is removed once nothing has written to it for seven days.

## Answers and context

An answer, such as `trace callers` or `trace reach`, waits for every index it needs and is always complete.

Context, such as the facts a read or a search hit carries, waits at most a second for an index that has more than 100 files left to read. A `--json` document waits for every index, as an answer does.

- An index one update behind answers as it is.
- An index nobody has built yet is left out, along with `imported_by`, `imports`, and calls. The call then ends with one line, once per context, naming `trace cache build` to finish it.
- What the call extracted before the second ran out is kept, so the next calls finish the index.

A session's first answer, the primer, builds the index until shortly before its hook deadline.

## Resolving a symbol

`trace callers` resolves each call site when you ask, from the files the relations index says mention the name. It reads imports the way the language does: PHP's PSR-4 from `composer.json`, TypeScript's `paths` and `baseUrl` from `tsconfig.json`, and Rust's crates, modules, `use` items, and re-exports from `Cargo.toml` and the code. Each site carries a confidence:

- `EXTRACTED`: the code names the declaration, through an import or in the same file.
- `INFERRED`: the name and shape match, and nothing proves another target.
- `AMBIGUOUS`: several declarations could be the target, such as a method called on a value whose type the file never states.

A declaration only its own file can reach, such as a C `static` function, a TypeScript module's unexported function, or a `private` method, is never the target of a call in another file. A file's exports are its top-level declarations its language makes public.

## The session record

When a session id is set, `AGENT_SESSION_ID`, Codex's `CODEX_THREAD_ID`, or Claude Code's `CLAUDE_CODE_SESSION_ID`, `trace` keeps a record per agent at `$TMPDIR/tracer-<uid>/sessions/<session>/<agent>/record.json`: the docs it sent, the files it read and which lines, and a hash of each fact and directory listing line it showed. That record is why a file's facts print once per context and a doc arrives once.

- The record sits outside every repository, so a session that reads several repositories keeps one record.
- A record is saved only after its output is flushed, so a call cut off before its output arrives records nothing as shown.
- Session folders untouched for 7 days are removed.
- `trace docs reset` clears what a context has seen, after a compaction or `/clear`.
- Each subagent's record stays under its own id, so a resumed subagent finds it where it left it.

## The budget

Every text answer fits `--budget`, 30,000 characters by default, the longest Bash result Claude Code shows whole. The budget cuts detail, never coverage:

1. The files the fewest others import lose detail first.
2. A declaration keeps at least `L<line> name`.
3. A listing too long even for bare paths prints one line per directory, and names how many directories it left out.

A cut answer ends with the command that prints the rest.
