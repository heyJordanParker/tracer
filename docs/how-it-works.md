# How it works

`trace` is one Rust binary. It parses code with tree-sitter grammars compiled into it, and runs `git`, `ripgrep`, `ast-grep`, `scc`, and `universal-ctags` for what they do best. Everything it learns about a repository lives in `.tracer-cache/` at the repository's root, which writes its own `.gitignore`.

## Declarations

Each file's declarations come from a tree-sitter extractor for C, Go, Java, PHP, Python, Ruby, Rust, and TypeScript, TSX, and JSX, plus Bash and Lua for complexity. A declaration's row is its own source text, attributes, modifiers, name, parameters, return type, and heritage, with every body elided as `{ … }`. A language with no extractor falls back to `universal-ctags`.

Cyclomatic complexity is counted per function by walking its tree for decision points: `if`, each loop, each `case` and `catch`, `&&`, `||`, `??`, ternaries, and comprehension clauses. `scc` counts the repository's lines and languages.

## The cache

`.tracer-cache/file/schema<N>/` holds everything one version of the cache format writes:

- **Per-file facts**, keyed by the file's contents and path: its declarations, imports, exports, and complexity. A changed file gets a new entry, and the superseded one is removed.
- **The relations index**: every file's imports, the files that import each file, and where each name is declared and mentioned. A call rewrites it in place, re-reading only the files whose contents moved.
- **The commit index**: the newest 4,000 commits, with every file each one changed. A call after `HEAD` moves reads only the new commits.
- **The tracked-file listing**, with the stamps of every directory and ignore file it read, so a warm call checks stamps instead of walking the tree.

A process holds `.maintain.lock` while it updates an index, so concurrent calls wait for one update and read its result. A cache directory of another format version is removed once nothing has written to it for seven days.

## Resolving a symbol

`trace callers` resolves each call site when you ask, from the files the relations index says mention the name. It reads imports the way the language does: PHP's PSR-4 from `composer.json`, TypeScript's `paths` and `baseUrl` from `tsconfig.json`, and Rust's crates, modules, `use` items, and re-exports from `Cargo.toml` and the code. Each site carries a confidence:

- `EXTRACTED`: the code names the declaration, through an import or in the same file.
- `INFERRED`: the name and shape match, and nothing proves another target.
- `AMBIGUOUS`: several declarations could be the target, such as a method called on a value whose type the file never states.

## The session record

When `AGENT_SESSION_ID` is set, `trace` keeps a record per agent under `.tracer-cache/sessions/<session>/<agent>/`: the docs it sent, the files it read and which lines, and the facts and directory listings it showed. That record is why a file's facts print once per context and a doc arrives once.

- A record is saved only after its output is flushed, so a call cut off before its output arrives records nothing as shown.
- `trace docs reset` clears what a context has seen, after a compaction or `/clear`.
- `trace docs archive` moves a stopped subagent's record aside, and its first write after a resume takes it back.

## The budget

Every text answer fits `--budget`, 30,000 characters by default, the longest Bash result Claude Code shows whole. The budget cuts detail, never coverage:

1. The files the fewest others import lose detail first.
2. A declaration keeps at least `L<line> name`.
3. A listing too long even for bare paths prints one line per directory, and names how many directories it left out.

A cut answer ends with the command that prints the rest.
