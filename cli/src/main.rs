//! tracer — code-intelligence CLI. Binary name: `trace`.
//! Per-function cyclomatic complexity is AST-derived (tree-sitter
//! decision-node walker), the single CCN backend.
mod cache;
mod calls;
mod ccn;
mod commands;
mod digest;
mod docs;
mod docs_graph;
mod extraction;
mod file_facts;
mod filter;
mod git_activity;
mod hook;
mod jsonfmt;
mod lang;
mod matches;
mod memo;
mod nested_memory;
mod output;
mod paths_match;
mod pathval;
mod primer;
mod process;
mod relations;
mod repo_context;
mod repo_files;
mod sections;
mod session;
mod shell;
mod summary;
mod surface;
mod timing;
mod yamlfmt;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "trace",
    version,
    about = "Code intelligence CLI for mapping architectural relationships."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Run as if trace started in DIR, the way `git -C` does: every command
    /// answers for DIR's repository and relative paths resolve from DIR.
    /// Goes before the subcommand, because `grep -C` is ripgrep's context.
    #[arg(short = 'C', value_name = "DIR")]
    directory: Option<PathBuf>,

    /// Run a jq program over this command's JSON output, in-process.
    /// Requires --json. Replaces piping `trace ... --json | jq`.
    #[arg(long, global = true, value_name = "JQ")]
    filter: Option<String>,

    /// Print the whole answer, uncut. Text output otherwise fits the
    /// 30,000 characters Claude Code shows whole: detail is cut, never a
    /// file or declaration, and the last line names this flag.
    #[arg(long, global = true)]
    whole: bool,

    /// The agent whose session record this call reads and writes, in place
    /// of `TRACER_AGENT_ID`.
    #[arg(long, global = true, value_name = "ID")]
    agent: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Complexity structure + architectural overview of files or directories.
    Info {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        brief: bool,
    },
    /// Verify required external binaries are installed.
    Doctor,
    /// Repo-wide language + LOC + complexity distribution.
    Stats {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Manage the .tracer-cache/ disk cache.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// The public API of the files and folders named: each file's facts and
    /// purpose, then every declaration and re-export the visibility reaches,
    /// with its whole header.
    Api {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// The rows listed: `public`, also `internal` ones the language or
        /// the author marks internal, or `all` with each file's imports.
        #[arg(long, value_enum, default_value = "public")]
        visibility: commands::api::Level,
        /// Only files another file imports.
        #[arg(long)]
        imported: bool,
        /// Only files at most N folders below each path, as ripgrep's
        /// --max-depth.
        #[arg(long = "max-depth", value_name = "N")]
        max_depth: Option<usize>,
        /// Include or, with `!`, exclude files, as ripgrep's `-g` does.
        #[arg(short = 'g', long = "glob", value_name = "GLOB")]
        globs: Vec<String>,
        /// Read the files at a commit, branch, or tag instead of the working
        /// tree.
        #[arg(long, value_name = "REV")]
        revision: Option<String>,
        /// Write the API to FILE instead of printing it, uncut by --budget
        /// and without line numbers or file facts, so two versions compare
        /// line by line.
        #[arg(short = 'o', long, value_name = "FILE", conflicts_with = "json")]
        output: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Methods, properties, variables, imports, and exports for every file
    /// named; a directory names the files under it.
    Structure {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Text search via ripgrep, each match grouped under the declarations
    /// that enclose it. Takes ripgrep's own flags.
    Grep {
        #[arg(required_unless_present = "regexp")]
        pattern: Option<String>,
        /// The pattern, as ripgrep's `-e` takes it: it may start with `-`,
        /// and every positional argument is then a path.
        #[arg(short = 'e', long = "regexp", value_name = "PATTERN", allow_hyphen_values = true)]
        regexp: Option<String>,
        /// Match case-insensitively.
        #[arg(short = 'i', long)]
        ignore_case: bool,
        /// Each match alone, without the rest of its line.
        #[arg(short = 'o', long)]
        only_matching: bool,
        /// The lines that do not match.
        #[arg(short = 'v', long)]
        invert_match: bool,
        /// Match whole words only.
        #[arg(short = 'w', long)]
        word_regexp: bool,
        /// Match whole lines only.
        #[arg(short = 'x', long)]
        line_regexp: bool,
        /// Each matching file once, with its facts.
        #[arg(short = 'l', long)]
        files_with_matches: bool,
        /// Each matching file once, with its facts and match count.
        #[arg(short = 'c', long)]
        count: bool,
        /// Lines shown either side of each match.
        #[arg(short = 'C', long, value_name = "NUM", default_value_t = 0)]
        context: usize,
        /// Lines shown after each match, in place of `-C`'s.
        #[arg(short = 'A', long = "after-context", value_name = "NUM")]
        after_context: Option<usize>,
        /// Lines shown before each match, in place of `-C`'s.
        #[arg(short = 'B', long = "before-context", value_name = "NUM")]
        before_context: Option<usize>,
        /// Every match line is numbered; taken so ripgrep's `-n` works.
        #[arg(short = 'n', long = "line-number")]
        line_number: bool,
        /// Let a match span lines.
        #[arg(short = 'U', long)]
        multiline: bool,
        /// Only files of this type: a language name or any `rg --type-list` type.
        #[arg(short = 't', long = "type", value_name = "TYPE")]
        types: Vec<String>,
        /// Only paths matching this glob; `!` excludes them.
        #[arg(short = 'g', long = "glob", value_name = "GLOB")]
        globs: Vec<String>,
        /// Files and directories to search, as ripgrep takes them; `.` when none.
        paths: Vec<String>,
        /// Read the files at a commit, branch, or tag instead of the working
        /// tree.
        #[arg(long, value_name = "REV")]
        revision: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Timestamped entries from log files, including the ones the ignore
    /// walk skips. One line is one entry; a line with no timestamp of its
    /// own attaches to the entry above it, so a stack trace stays whole.
    Logs {
        pattern: Option<String>,
        #[arg(long, default_value = ".")]
        path: String,
        /// Filename glob under <path>. The default reaches access.log,
        /// access.log.1, access.log.2.gz, and laravel-2026-08-15.log.
        #[arg(long = "file", default_value = "*.log*")]
        file_glob: String,
        /// Window start: YYYY-MM-DD, 'YYYY-MM-DD HH:MM[:SS]', or HH:MM[:SS]
        /// on the day of the newest log selected.
        #[arg(long)]
        since: Option<String>,
        /// Window end, same three forms.
        #[arg(long)]
        until: Option<String>,
        /// Whole entries either side of each match.
        #[arg(long, default_value_t = 0)]
        around: usize,
        /// Cap on matching entries, keeping the newest.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Structural (AST) search via ast-grep, each match grouped under the
    /// declarations that enclose it.
    Pattern {
        pattern: String,
        /// The language the pattern parses as.
        #[arg(short = 't', long = "type", value_name = "TYPE")]
        language: String,
        /// Files and directories to search, as ast-grep takes them.
        #[arg(default_value = ".")]
        paths: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// All places a symbol is declared.
    Defines {
        symbol: String,
        #[arg(long)]
        json: bool,
    },
    /// Direct callers / importers of a symbol, resolved now.
    Callers {
        symbol: String,
        #[arg(long, default_value_t = 200)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// What a symbol depends on (transitive), or highest-coupling symbols in a path.
    Dependencies {
        symbol: Option<String>,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long = "max-depth", value_name = "N", default_value_t = 3)]
        max_depth: i64,
        #[arg(long, default_value_t = 10)]
        limit: i64,
        #[arg(long)]
        json: bool,
    },
    /// Where a symbol is used (transitive), or most-depended-on symbols in a path.
    Usages {
        symbol: Option<String>,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long = "max-depth", value_name = "N", default_value_t = 3)]
        max_depth: i64,
        #[arg(long, default_value_t = 10)]
        limit: i64,
        #[arg(long)]
        json: bool,
    },
    /// One-level directory listing from the filesystem: every file on disk
    /// (gitignored included) with stat, code, and git column groups.
    List {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[arg(long = "all")]
        show_hidden: bool,
        /// Order files newest-first by filesystem mtime.
        #[arg(long)]
        recent: bool,
        /// Cap the file rows after ordering (no default cap); `entries=N`
        /// always reports the pre-cap total.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Annotated file tree with per-file complexity ranks (recursive).
    Tree {
        path: PathBuf,
        /// Only files at most N folders below the path, as ripgrep's
        /// --max-depth.
        #[arg(long = "max-depth", value_name = "N")]
        max_depth: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Find files (or directories) by name pattern with code intelligence.
    Find {
        pattern: String,
        #[arg(default_value = ".")]
        bases: Vec<String>,
        #[arg(long = "path")]
        path_filter: Option<String>,
        #[arg(long = "exclude")]
        excludes: Vec<String>,
        #[arg(long = "type", value_parser = ["f", "d"], default_value = "f")]
        type_filter: String,
        #[arg(long, default_value_t = 200)]
        limit: usize,
        #[arg(long, value_parser = ["complexity", "recent", "path"], default_value = "path")]
        sort: String,
        #[arg(long)]
        json: bool,
    },
    /// What changed, as lines: staged, unstaged and untracked against HEAD by
    /// default, or the committed difference from `--base <ref>`.
    /// Load-bearing file first.
    Diff {
        /// Limit to these files and directories.
        paths: Vec<String>,
        /// Compare committed history against this ref instead of comparing
        /// the working tree against HEAD.
        #[arg(long, value_name = "REV")]
        base: Option<String>,
        #[arg(long = "symbols")]
        symbol_mode: bool,
        #[arg(long)]
        json: bool,
    },
    /// Working-tree dirty set with code intelligence, ordered by blast radius.
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long = "state", value_parser = ["added", "renamed", "modified", "deleted", "untracked"])]
        state: Option<String>,
    },
    /// Everything tracer does on a harness hook event: reads the hook's input
    /// as JSON on stdin and prints its answer as JSON on stdout.
    Hook {
        /// The hook event, as the harness names it (`SessionStart`, `PostToolUse`, …).
        event: String,
        /// The harness whose input format and tool names the input uses.
        #[arg(long, value_enum)]
        harness: hook::Harness,
        /// The harness's timeout for this hook: past it the answer is thrown
        /// away, so nothing is recorded as shown and the process ends.
        #[arg(long, value_name = "MS")]
        deadline_ms: u64,
        /// Refuse shell commands that read files without tracer when tracer gives the same answer.
        #[arg(long)]
        tracer_only: bool,
    },
    /// Project-docs surface: path-scoped deduped set (default), `--graph` for
    /// the whole-repo docs graph, `status` for the session manifest, `reset`
    /// to forget what was loaded, or `prime` to record what the harness loaded.
    #[command(args_conflicts_with_subcommands = true)]
    Docs {
        /// Paths for the default path-mode (`trace docs <paths>`), or the one
        /// path for `--graph` (optional; defaults to the cwd's repo root).
        /// Replaced by any present sub-verb (`status`, `reset`, `prime`).
        paths: Vec<PathBuf>,
        /// A doc the triggering command delivers itself, never sent here.
        #[arg(long = "skip", value_name = "PATH")]
        skip: Vec<PathBuf>,
        /// Treat <path> as a directory even when it points at a file (path-mode only).
        #[arg(long = "directory")]
        directory: bool,
        /// Whole-repo docs graph, built in memory per call, plus the
        /// available-but-not-loaded set. With this flag, <path> is optional.
        #[arg(long = "graph")]
        graph: bool,
        /// Names the calling surface (e.g. `trace_inject_hook`, `agent_read`).
        /// Recorded verbatim as each sent doc's `source`.
        #[arg(long, default_value = "trace_docs")]
        source: String,
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        command: Option<DocsCommand>,
    },
    /// Session-start primer (no args) or the one-line file briefing.
    Context {
        paths: Vec<PathBuf>,
        #[arg(long = "directory")]
        force_directory: bool,
        /// 1-based line the read started at (the read tool's `offset`); records
        /// which slice of the file the agent read for per-file read coverage.
        #[arg(long)]
        offset: Option<usize>,
        /// Number of lines the read covered (the read tool's `limit`).
        #[arg(long)]
        limit: Option<usize>,
        /// Render the file summary without recording a read. The enrich hook
        /// sets this for Edit/Write — an edit gets the file's architectural
        /// summary but is not a read, so it must not count toward per-file
        /// read coverage.
        #[arg(long = "no-record")]
        no_record: bool,
        #[arg(long)]
        json: bool,
    },
    /// Cleaned read: whole file, method, line range, or anchor section; worktree or git ref.
    Read {
        #[arg(required = true)]
        paths: Vec<String>,
        #[arg(long = "method")]
        method: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        raw: bool,
        /// Read the files at a commit, branch, or tag instead of the working
        /// tree.
        #[arg(long, value_name = "REV")]
        revision: Option<String>,
        #[arg(long = "lines", value_name = "L1:L2")]
        lines: Option<String>,
        #[arg(long, num_args = 2, value_names = ["START", "END"])]
        between: Option<Vec<String>>,
        #[arg(long = "diff")]
        as_diff: bool,
        /// Inject project-docs content (off by default).
        #[arg(long = "docs")]
        docs: bool,
    },
    /// Symbol-aware blame collapsed into per-region commit summaries.
    Blame {
        file: PathBuf,
        symbol: Option<String>,
        #[arg(long = "lines", value_name = "L1:L2")]
        lines: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Git archaeology: whole-file log, function-line history, pickaxe, or
    /// one commit in full.
    History {
        file: Option<PathBuf>,
        symbol: Option<String>,
        #[arg(long)]
        contains: Option<String>,
        /// Read `--contains` as a regular expression instead of literal text.
        #[arg(long)]
        regex: bool,
        /// One commit in full: message, author, parents, changed files and
        /// the changed lines.
        #[arg(long = "commit", value_name = "REV")]
        commit: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DocsCommand {
    /// Agent-facing "what do I have right now?" query against the agent's
    /// record. No path arg → every doc and file it holds, with source
    /// attribution. With a path arg → that path's ancestor chain
    /// partitioned into loaded (with source) and not_loaded.
    Status {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Clear the agent's record so a subsequent `trace docs <path>`
    /// re-surfaces docs as new. Driven by the compaction and clear hooks: a
    /// context reset drops injected rule text from the model, so the record
    /// must reset to re-inject it.
    Reset {
        /// Names the calling surface (e.g. `codex_compact_hook`). Echoed in
        /// the JSON query.
        #[arg(long, default_value = "trace_docs_reset")]
        source: String,
        #[arg(long)]
        json: bool,
    },
    /// Record docs the harness put in the agent's context, so later tracer
    /// output skips them: the named files (Claude Code's InstructionsLoaded
    /// hook), or with none, the chain the harness loads at session start
    /// (codex's SessionStart hook).
    Prime {
        files: Vec<PathBuf>,
        #[arg(long, value_parser = ["session_start", "post_compact"], default_value = "session_start")]
        reason: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Prebuild the cache for a repo so the first agent query is fast.
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Delete cache entries. Clears the `file` namespace; session state
    /// goes with `--all`.
    Clear {
        /// Remove .tracer-cache/ entirely.
        #[arg(long = "all")]
        clear_all: bool,
    },
    /// Show cache size and entry count.
    Stats {
        #[arg(long)]
        json: bool,
    },
}

/// The most threads one tracer process uses. Many agents call tracer at
/// once, and a call that took every core made them fight each other.
const THREADS: usize = 8;

fn main() {
    let started = timing::start();
    // rayon's pool holds the process's thread count; the file-stamping
    // workers, ripgrep and ast-grep read it back from
    // `rayon::current_num_threads()`.
    let cores = std::thread::available_parallelism().map_or(1, |count| count.get());
    let _ = rayon::ThreadPoolBuilder::new().num_threads(cores.min(THREADS)).build_global();
    let result = run();
    timing::total(started);
    let code = match result {
        Ok(()) if process::reported() => 2,
        Ok(()) => 0,
        Err(error) => match error.downcast_ref::<process::TraceError>() {
            Some(failure) => {
                if !failure.message.is_empty() {
                    eprintln!("{failure}");
                }
                failure.code
            }
            None => {
                eprintln!("Error: {error:?}");
                1
            }
        },
    };
    process::exit(code)
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let detachable = matches!(cli.command, Command::Cache { command: CacheCommand::Build { .. } });
    process::guard(!detachable);
    if let Some(dir) = &cli.directory {
        if let Err(error) = std::env::set_current_dir(dir) {
            return Err(process::fail(2, format!("-C {}: {error}", dir.display())));
        }
        output::set_directory(dir.to_string_lossy().into_owned());
    }
    output::set_whole(cli.whole);
    if let Some(agent) = cli.agent {
        session::set_agent(agent);
    }
    let filter = cli.filter.as_deref();
    match cli.command {
        Command::Info { paths, json, brief } => {
            output::run_value(json, filter, || commands::info::run(&paths, json, brief))
        }
        Command::Doctor => {
            output::guard(false, filter)?;
            commands::doctor::run()
        }
        Command::Stats { path, json } => {
            output::run_value(json, filter, || commands::stats::run(&path, json))
        }
        Command::Cache { command } => match command {
            CacheCommand::Build { path } => {
                output::guard(false, filter)?;
                commands::cache::build(&path)
            }
            CacheCommand::Clear { clear_all } => {
                output::guard(false, filter)?;
                commands::cache::clear(Path::new("."), clear_all)
            }
            CacheCommand::Stats { json } => output::run_value(json, filter, || {
                commands::cache::stats(Path::new("."), json)
            }),
        },
        Command::Api { paths, visibility, imported, max_depth, globs, revision, output, json } => output::run_value(json, filter, || {
            let scope = commands::api::Scope { visibility, imported, max_depth, globs: &globs, revision: revision.as_deref() };
            commands::api::run(&paths, &scope, output.as_deref(), json)
        }),
        Command::Structure { paths, json } => {
            output::run_value(json, filter, || commands::structure::run(&paths, json))
        }
        Command::Grep {
            pattern,
            regexp,
            ignore_case,
            only_matching,
            invert_match,
            word_regexp,
            line_regexp,
            files_with_matches,
            count,
            context,
            after_context,
            before_context,
            line_number: _,
            multiline,
            types,
            globs,
            paths,
            revision,
            json,
        } => output::run_streamed(json, filter, |sink| {
            let (pattern, mut paths) = match regexp {
                Some(regexp) => (regexp, pattern.into_iter().chain(paths).collect()),
                None => (pattern.unwrap_or_default(), paths),
            };
            if paths.is_empty() {
                paths.push(".".to_string());
            }
            let options = commands::grep::Options {
                ignore_case,
                only_matching,
                invert_match,
                word_regexp,
                line_regexp,
                files_only: files_with_matches || count,
                before: before_context.unwrap_or(context),
                after: after_context.unwrap_or(context),
                multiline,
                types,
                globs,
            };
            commands::grep::run(&pattern, &paths, revision.as_deref(), &options, json, sink)
        }),
        Command::Logs {
            pattern,
            path,
            file_glob,
            since,
            until,
            around,
            limit,
            json,
        } => output::run_value(json, filter, || {
            commands::logs::run(
                pattern.as_deref(),
                &path,
                &file_glob,
                since.as_deref(),
                until.as_deref(),
                around,
                limit,
                json,
            )
        }),
        Command::Pattern {
            pattern,
            language,
            paths,
            json,
        } => output::run_streamed(json, filter, |sink| {
            commands::pattern::run(&pattern, &language, &paths, json, sink)
        }),
        Command::Defines { symbol, json } => {
            output::run_value(json, filter, || commands::defines::run(&symbol, json))
        }
        Command::Callers {
            symbol,
            limit,
            json,
        } => output::run_value(json, filter, || {
            commands::callers::run(&symbol, limit, json)
        }),
        Command::Dependencies {
            symbol,
            path,
            max_depth,
            limit,
            json,
        } => output::run_value(json, filter, || {
            commands::reach::run(
                commands::reach::Direction::Dependencies,
                symbol.as_deref(),
                path.as_deref(),
                max_depth,
                limit,
                json,
            )
        }),
        Command::Usages {
            symbol,
            path,
            max_depth,
            limit,
            json,
        } => output::run_value(json, filter, || {
            commands::reach::run(
                commands::reach::Direction::Dependents,
                symbol.as_deref(),
                path.as_deref(),
                max_depth,
                limit,
                json,
            )
        }),
        Command::List {
            paths,
            show_hidden,
            recent,
            limit,
            json,
        } => output::run_value(json, filter, || {
            commands::list_::run(&paths, show_hidden, recent, limit, json)
        }),
        Command::Tree { path, max_depth, json } => {
            output::run_value(json, filter, || commands::tree::run(&path, max_depth, json))
        }
        Command::Find {
            pattern,
            bases,
            path_filter,
            excludes,
            type_filter,
            limit,
            sort,
            json,
        } => output::run_value(json, filter, || {
            commands::find::run(
                &pattern,
                &bases,
                path_filter,
                excludes,
                type_filter,
                limit,
                sort,
                json,
            )
        }),
        Command::Diff {
            paths,
            base,
            symbol_mode,
            json,
        } => output::run_value(json, filter, || {
            commands::diff::run(&paths, base.as_deref(), symbol_mode, json)
        }),
        Command::Status { json, state } => output::run_value(json, filter, || {
            commands::status::run(json, state.as_deref())
        }),
        Command::Hook { event, harness, deadline_ms, tracer_only } => {
            output::guard(false, filter)?;
            hook::run(&event, harness, deadline_ms, tracer_only)
        }
        Command::Docs {
            paths,
            skip,
            directory,
            graph,
            source,
            json,
            command,
        } => match command {
            Some(DocsCommand::Status { path, json }) => output::run_value(json, filter, || {
                commands::docs::run_status(path.as_deref(), json)
            }),
            Some(DocsCommand::Reset { source, json }) => {
                output::run_value(json, filter, || commands::docs::run_reset(&source, json))
            }
            Some(DocsCommand::Prime { files, reason, json }) => output::run_value(json, filter, || {
                let parsed = commands::docs_prime::parse_reason(&reason)?;
                commands::docs_prime::run(parsed, &files, json)
            }),
            None if graph => output::run_value(json, filter, || {
                commands::docs::run_graph(paths.first().map(PathBuf::as_path), json)
            }),
            None => output::run_streamed(json, filter, |sink| {
                if paths.is_empty() {
                    return Err(process::fail(
                        2,
                        "Error: <PATHS> is required (use `trace docs --graph` for the whole-repo graph)",
                    ));
                }
                commands::docs::run(
                    &paths,
                    &skip,
                    directory,
                    &source,
                    json,
                    filter.is_none(),
                    sink,
                )
            }),
        },
        Command::Context {
            paths,
            force_directory,
            offset,
            limit,
            no_record,
            json,
        } => output::run_value(json, filter, || {
            commands::context::run(&paths, force_directory, offset, limit, !no_record, json)
        }),
        Command::Read {
            paths,
            method,
            json,
            raw,
            revision,
            lines,
            between,
            as_diff,
            docs,
        } => output::run_streamed(json, filter, |sink| {
            let docs_override = if docs { Some(true) } else { None };
            commands::read::run(
                &paths,
                method.as_deref(),
                json,
                raw,
                revision.as_deref(),
                lines.as_deref(),
                between.map(|v| (v[0].clone(), v[1].clone())),
                as_diff,
                docs_override,
                filter.is_none(),
                sink,
            )
        }),
        Command::Blame {
            file,
            symbol,
            lines,
            json,
        } => output::run_value(json, filter, || {
            commands::blame::run(&file, symbol.as_deref(), lines.as_deref(), json)
        }),
        Command::History {
            file,
            symbol,
            contains,
            regex,
            commit,
            json,
        } => output::run_value(json, filter, || {
            commands::history::run(
                file.as_deref(),
                symbol.as_deref(),
                contains.as_deref(),
                regex,
                commit.as_deref(),
                json,
            )
        }),
    }
}
