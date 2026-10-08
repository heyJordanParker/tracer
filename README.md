# tracer

**Code intelligence for coding agents.** `trace` answers the questions an agent asks about a repository — where is this defined, who calls it, what changed, what does this directory do — in one call. Every answer carries the facts an agent needs to judge what it found: size, complexity, importers, git history, deploy branches, and the project docs it has not read yet.

tracer ships two ways:

- **The `trace` command**, a single static binary for macOS and Linux. It works in any terminal, script, or agent harness.
- **The tracer mod for Claude Code**, which installs `trace` and wires it into Claude Code, so every file Claude reads, edits, or searches arrives with its facts, and every project doc arrives once, the first time Claude's work reaches it.

```text
$ trace grep 'render_rows\(' cli/src
cli/src/commands/structure.rs  {imported_by: 0, cyclomatic_complexity: 35, lines: 204, matches: 1, nearest_doc: cli/Claude.md}
  L62   pub fn run(paths: &[PathBuf], as_json: bool) -> Result<Value> { … }  // complexity 8
  L145:   let cut = surface::render_rows(&structure.rows, &structure.relative, None, within).text;

cli/src/surface.rs  {imported_by: 10, cyclomatic_complexity: 104, lines: 377, matches: 3, nearest_doc: cli/Claude.md}
  L103  pub fn render_within( rows: &[Row], file: &str, window: Option<(i64, i64)>, budget: Option<usize>, ) -> String { … }  // complexity 3
  L109:   let whole = render_rows(rows, file, window, None);
  L113:           let fitted = render_rows(rows, file, window, Some(budget.saturating_sub(closing)));
  L130: pub fn render_rows(rows: &[Row], file: &str, window: Option<(i64, i64)>, budget: Option<usize>) -> Fitted {

4 matches in 2 files
```

Each match sits under the function that holds it, and each file carries how many files import it, its complexity, and its size, so the agent knows what it found before it opens anything.

## Install

### Claude Code

tracer is a [cmod](https://github.com/heyJordanParker/cmod) mod. Install cmod once, then the mod:

```bash
npm install -g @cmodjs/cli
cmod install heyJordanParker/tracer
```

The mod downloads the `trace` release for your machine and checks it against the release's `SHA256SUMS`. Start a new Claude Code session and tracer is on. Claude Code runs the mod's `trace` even on macOS, where `/usr/bin/trace` is Apple's own tool.

### Homebrew

For the `trace` command on its own:

```bash
brew tap heyJordanParker/tracer https://github.com/heyJordanParker/tracer
brew install heyJordanParker/tracer/tracer
```

Homebrew installs `ripgrep`, `ast-grep`, `scc`, and `universal-ctags` beside it. See [Installation](docs/installation.md) for Linux and building from source.

### Requirements

`trace` runs `git`, `ripgrep`, `ast-grep`, `scc`, and `universal-ctags`. `trace doctor` checks each one and prints the install command for anything missing.

## What an agent gets

- **One call per question.** `trace defines`, `callers`, `usages`, and `dependencies` resolve symbols through a relations index built from tree-sitter, with PHP PSR-4, TypeScript `paths`, and Rust crate resolution. `grep`, `pattern`, and `find` group each match under the declarations that enclose it.
- **Facts with every file.** Each file prints as YAML front matter: lines, cyclomatic complexity and its rank in the repository, importers, git status, commit counts, the files usually changed with it, and the deploy branches it is on. A file's facts print once per agent context, and after an edit only the lines that changed.
- **Output that fits.** Every text answer fits `--budget`, 30,000 characters by default. The budget cuts detail, never coverage, and the last line names the command for the rest.
- **Project docs once.** `Claude.md`, `AGENTS.md`, and `.claude/rules/` files arrive the first time a command reaches their directory, nearest first, and never twice in one context.
- **Git archaeology.** `trace history --contains` finds every commit that added or removed a string in about a second, with the lines and the declaration each one changed. `trace blame` collapses by symbol. `trace diff` names the declarations a change touched.

## Documentation

- [Installation](docs/installation.md): the mod, Homebrew, building from source, and checking an install
- [Commands](docs/commands.md): every command, its options, and its JSON shape
- [Claude Code mod](docs/claude-code.md): the Hooks, the settings, and what each event adds to Claude's context
- [How it works](docs/how-it-works.md): the cache, the relations index, the session record, and the output budget
- [Releasing](docs/releasing.md): publishing a version to the mod and to Homebrew

## License

[MIT](LICENSE)
