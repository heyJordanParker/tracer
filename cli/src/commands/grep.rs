//! `trace grep` — text search with rich per-match context.
//! Wraps `rg --json` and takes ripgrep's own flags (`-i`, `-l`, `-c`, `-C`,
//! `-A`, `-B`, `-n`, `-U`, `-t`, `-g`); each match is enriched with per-file facts and grouped
//! under the declarations that enclose it.
//!
//! ripgrep's output is consumed as it is written rather than through
//! `Command::output`, which buffers every byte of the search before the first
//! match is looked at. The document is serialized from borrowed structures
//! through `output::Sink`, so the result never exists as a `serde_json::Value`
//! tree on the way out.

use crate::matches::{self, Match};
use crate::output::Sink;
use crate::{cache, repo_context};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// ripgrep's own flags, as `trace grep` takes them.
#[derive(Default)]
pub struct Options {
    pub ignore_case: bool,
    /// `-l` and `-c`: each matching file once, with its facts and match count.
    pub files_only: bool,
    /// `-B` and `-A`, both `-C` when unset: lines shown before and after each match.
    pub before: usize,
    pub after: usize,
    pub multiline: bool,
    pub types: Vec<String>,
    pub globs: Vec<String>,
}

/// A snippet stays a snippet on minified or generated lines. The matched
/// line is the unit for ordinary source, but a 27KB single-line bundle is
/// not "context around the match" — so past `MAX_SNIPPET_CHARS` the snippet
/// becomes a character window positioned by the submatch byte offset
/// `rg --json` already reports, ellipsized on the cut side(s).
const MAX_SNIPPET_CHARS: usize = 240;
const WINDOW_BEFORE_CHARS: usize = 80;
const DIAGNOSTIC_BUDGET_BYTES: usize = 4 * 1024;

fn stderr_reader(
    mut stderr: impl Read + Send + 'static,
) -> thread::JoinHandle<Result<(Vec<u8>, usize)>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut total = 0;
        let mut chunk = [0; 8192];
        loop {
            let read = stderr.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            total += read;
            let remaining = DIAGNOSTIC_BUDGET_BYTES.saturating_sub(kept.len());
            kept.extend_from_slice(&chunk[..read.min(remaining)]);
        }
        Ok((kept, total))
    })
}

fn backend_error(
    name: &str,
    status: std::process::ExitStatus,
    stderr: Vec<u8>,
    total: usize,
) -> anyhow::Error {
    let mut diagnostic = String::from_utf8_lossy(&stderr).trim().to_string();
    if total > stderr.len() {
        diagnostic.push_str(&format!(
            " [stderr truncated at {} of {} bytes]",
            stderr.len(),
            total
        ));
    }
    if diagnostic.is_empty() {
        diagnostic = "no diagnostic output".to_string();
    }
    anyhow!("{name} failed with {status}: {diagnostic}")
}

fn window_snippet(line: &str, match_byte_start: usize) -> String {
    let total_chars = line.chars().count();
    if total_chars <= MAX_SNIPPET_CHARS {
        return line.to_string();
    }
    let prefix_chars = line
        .get(..match_byte_start.min(line.len()))
        .map(|p| p.chars().count())
        .unwrap_or(0);
    let begin = prefix_chars.saturating_sub(WINDOW_BEFORE_CHARS);
    let window: String = line.chars().skip(begin).take(MAX_SNIPPET_CHARS).collect();
    let mut out = String::new();
    if begin > 0 {
        out.push('\u{2026}');
    }
    out.push_str(&window);
    if begin + MAX_SNIPPET_CHARS < total_chars {
        out.push('\u{2026}');
    }
    out
}

type Lines = HashMap<String, BTreeMap<i64, String>>;
type Groups = Vec<(PathBuf, Vec<Match>)>;

struct Event {
    file: String,
    line: i64,
    text: String,
    match_start: Option<usize>,
}

fn parse_event(line: &str) -> Result<Option<Event>> {
    let event: Value = serde_json::from_str(line).context("ripgrep wrote malformed JSON")?;
    let is_match = match event.get("type").and_then(Value::as_str) {
        Some("match") => true,
        Some("context") => false,
        _ => return Ok(None),
    };
    let data = event.get("data").context("ripgrep match is missing data")?;
    let Some(text) = data
        .get("lines")
        .and_then(|lines| lines.get("text"))
        .and_then(Value::as_str)
    else {
        return if is_match { Err(anyhow!("ripgrep match is missing line text")) } else { Ok(None) };
    };
    let match_start = if is_match {
        Some(
            data.get("submatches")
                .and_then(Value::as_array)
                .and_then(|matches| matches.first())
                .and_then(|matched| matched.get("start"))
                .and_then(Value::as_u64)
                .context("ripgrep match is missing a submatch start")? as usize,
        )
    } else {
        None
    };
    let file = data
        .get("path")
        .and_then(|path| path.get("text"))
        .and_then(Value::as_str)
        .context("ripgrep match is missing a file path")?;
    let line = data
        .get("line_number")
        .and_then(Value::as_i64)
        .context("ripgrep match is missing a line number")?;
    Ok(Some(Event {
        file: file.to_string(),
        line,
        text: text.trim_end_matches('\n').to_string(),
        match_start,
    }))
}

fn snippet(text: &str, match_start: usize) -> String {
    text.split('\n')
        .enumerate()
        .map(|(index, text)| window_snippet(text, if index == 0 { match_start } else { 0 }))
        .collect::<Vec<_>>()
        .join("\n")
}

fn keep_lines(lines: &mut Lines, file: &str, first: i64, text: &str) {
    let kept = lines.entry(file.to_string()).or_default();
    for (line, text) in (first..).zip(text.split('\n')) {
        kept.insert(line, text.to_string());
    }
}

fn ripgrep(pattern: &str, paths: &[String], options: &Options, types: &[String]) -> Result<(Vec<Match>, Lines)> {
    let mut cmd = Command::new("rg");
    cmd.args(["--json", "--threads", &rayon::current_num_threads().to_string()]);
    cmd.args(crate::lang::RIPGREP_SCOPE);
    if options.ignore_case {
        cmd.arg("--ignore-case");
    }
    if options.multiline {
        cmd.arg("--multiline");
    }
    let with_context = options.before + options.after > 0;
    if with_context {
        cmd.args(["--before-context", &options.before.to_string()]);
        cmd.args(["--after-context", &options.after.to_string()]);
    }
    for rg_type in types {
        cmd.args(["--type", rg_type]);
    }
    for glob in &options.globs {
        cmd.args(["--glob", glob]);
    }
    cmd.arg("--")
        .arg(pattern)
        .args(paths)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::process::spawn(&mut cmd).context("failed to start ripgrep")?;
    let stderr = stderr_reader(child.stderr.take().expect("piped ripgrep stderr"));
    let mut matches = Vec::new();
    let mut lines = Lines::new();
    let mut parse_result = Ok(());
    for line in std::io::BufReader::new(child.stdout.take().expect("piped ripgrep stdout")).lines()
    {
        match line
            .context("failed to read ripgrep output")
            .and_then(|line| parse_event(&line))
        {
            Ok(Some(event)) => {
                if with_context {
                    keep_lines(&mut lines, &event.file, event.line, &event.text);
                }
                if let Some(start) = event.match_start {
                    matches.push(Match {
                        snippet: snippet(&event.text, start),
                        file: event.file,
                        line: event.line,
                        ..Match::default()
                    });
                }
            }
            Ok(None) => {}
            Err(error) => {
                parse_result = Err(error);
                let _ = child.kill();
                break;
            }
        }
    }
    let status = child.wait().context("failed to wait for ripgrep")?;
    let (stderr, total) = stderr
        .join()
        .map_err(|_| anyhow!("ripgrep stderr reader panicked"))??;
    parse_result?;
    if status.success() || status.code() == Some(1) {
        Ok((matches, lines))
    } else {
        Err(backend_error("ripgrep", status, stderr, total))
    }
}

/// Whether ripgrep's `-g` `globs` select a repository-relative `file`, matched
/// as ripgrep matches them: a `!` glob excludes, and a glob that excludes a
/// directory excludes every file under it.
pub(super) fn globbed(globs: &[String]) -> Result<impl Fn(&Path) -> bool> {
    let mut builder = ignore::overrides::OverrideBuilder::new(".");
    for glob in globs {
        builder.add(glob)?;
    }
    let globs = builder.build()?;
    Ok(move |file: &Path| {
        !globs.matched(file, false).is_ignore()
            && !file
                .ancestors()
                .skip(1)
                .take_while(|parent| !parent.as_os_str().is_empty())
                .any(|parent| globs.matched(parent, true).is_ignore())
    })
}

/// Search a commit rather than the working tree. ripgrep reads files on
/// disk, so the tool for a past state is `git grep`, which reads the tree
/// object directly — no checkout, no temp files. Its files then pass through
/// the type and glob matchers ripgrep itself builds, so `-t` and `-g` select
/// the same files on a commit as on the working tree.
fn git_grep(
    pattern: &str,
    revision: &str,
    repo_root: &Path,
    scope: &[(&str, String)],
    options: &Options,
    types: &ignore::types::Types,
) -> Result<Vec<(String, Match)>> {
    if options.multiline {
        bail!("-U needs ripgrep, and --revision searches with git grep, which has no multiline mode");
    }
    let globbed = globbed(&options.globs)?;
    let selected = |file: &str| {
        let file = Path::new(file);
        !types.matched(file, false).is_ignore() && globbed(file)
    };
    let mut args = vec!["grep", "-z", "--text", "-n", "--no-color", "--perl-regexp"];
    if options.ignore_case {
        args.push("--ignore-case");
    }
    args.extend(["-e", pattern, revision, "--"]);
    args.extend(scope.iter().map(|(_, within)| within.as_str()));
    let mut matches = crate::git_activity::git_command(repo_root, args, |command| {
        let mut child = crate::process::spawn(command.stdout(Stdio::piped()).stderr(Stdio::piped()))
            .context("failed to start git grep")?;
        let stderr = stderr_reader(child.stderr.take().expect("piped git grep stderr"));
        let mut stdout = Vec::new();
        let read_result = child
            .stdout
            .take()
            .expect("piped git grep stdout")
            .read_to_end(&mut stdout)
            .context("failed to read git grep output");
        if read_result.is_err() {
            let _ = child.kill();
        }
        let status = child.wait().context("failed to wait for git grep")?;
        let (stderr, total) = stderr
            .join()
            .map_err(|_| anyhow!("git grep stderr reader panicked"))??;
        read_result?;
        if !status.success() && status.code() != Some(1) {
            bail!("{}", backend_error("git grep", status, stderr, total));
        }
        let prefix = format!("{revision}:").into_bytes();
        let mut matches = Vec::new();
        let mut remaining = stdout.as_slice();
        while !remaining.is_empty() {
            let path_end = remaining
                .iter()
                .position(|byte| *byte == 0)
                .context("git grep wrote a record without a path delimiter")?;
            let framed_path = &remaining[..path_end];
            let file = framed_path
                .strip_prefix(prefix.as_slice())
                .context("git grep wrote a malformed revision/path field")?;
            remaining = &remaining[path_end + 1..];

            let line_end = remaining
                .iter()
                .position(|byte| *byte == 0)
                .context("git grep wrote a record without a line delimiter")?;
            let line = std::str::from_utf8(&remaining[..line_end])
                .context("git grep wrote a non-text line number")?
                .parse()
                .context("git grep wrote an invalid line number")?;
            remaining = &remaining[line_end + 1..];

            let text_end = remaining
                .iter()
                .position(|byte| *byte == b'\n')
                .context("git grep wrote a record without a text delimiter")?;
            let text = String::from_utf8_lossy(&remaining[..text_end]);
            remaining = &remaining[text_end + 1..];
            let file = String::from_utf8_lossy(file).into_owned();
            matches.push((
                file.clone(),
                Match {
                    file: shown(&file, scope),
                    line,
                    snippet: window_snippet(&text, text.find(pattern).unwrap_or(0)),
                    ..Match::default()
                },
            ));
        }
        Ok::<_, anyhow::Error>(matches)
    })?;
    let cwd = std::env::current_dir().ok();
    matches.retain(|(within, _)| {
        let found = repo_root.join(within);
        match cwd.as_deref().and_then(|cwd| found.strip_prefix(cwd).ok()) {
            Some(here) => selected(&here.to_string_lossy()),
            None => selected(within),
        }
    });
    Ok(matches)
}

fn shown(file: &str, scope: &[(&str, String)]) -> String {
    scope
        .iter()
        .filter_map(|(written, within)| {
            let rest = match within.as_str() {
                "." => file,
                _ if file == within => "",
                _ => file.strip_prefix(within.as_str())?.strip_prefix('/')?,
            };
            Some((within.len(), Path::new(written).join(rest)))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, path)| {
            path.to_string_lossy()
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string()
        })
        .unwrap_or_else(|| file.to_string())
}

fn lines_at(repo_root: &Path, revision: &str, matches: &[(String, Match)]) -> Result<Lines> {
    let mut lines = Lines::new();
    for (within, found) in matches {
        if lines.contains_key(&found.file) {
            continue;
        }
        let bytes = crate::git_activity::blob(repo_root, revision, within)
            .with_context(|| format!("failed to read {} at {revision}", found.file))?;
        keep_lines(&mut lines, &found.file, 1, String::from_utf8_lossy(&bytes).trim_end_matches('\n'));
    }
    Ok(lines)
}

fn root_of(path: &str) -> PathBuf {
    let abs = cache::absolutize(Path::new(path));
    cache::worktree_root_for(&abs).unwrap_or_else(|| cache::display_root(&abs))
}

fn at_revision(
    pattern: &str,
    revision: &str,
    paths: &[String],
    options: &Options,
    types: &ignore::types::Types,
) -> Result<(Groups, Lines)> {
    let mut scopes: Vec<(PathBuf, Vec<(&str, String)>)> = Vec::new();
    for path in paths {
        let root = root_of(path);
        let within = match cache::relative_to_root(&cache::absolutize(Path::new(path)), &root) {
            within if within.is_empty() => ".".to_string(),
            within => within,
        };
        match scopes.iter_mut().find(|(known, _)| *known == root) {
            Some((_, scope)) => scope.push((path.as_str(), within)),
            None => scopes.push((root, vec![(path.as_str(), within)])),
        }
    }
    let mut groups = Vec::new();
    let mut lines = Lines::new();
    for (root, scope) in scopes {
        let found = git_grep(pattern, revision, &root, &scope, options, types)?;
        if found.is_empty() {
            continue;
        }
        if options.before + options.after > 0 {
            lines.extend(lines_at(&root, revision, &found)?);
        }
        groups.push((root, found.into_iter().map(|(_, found)| found).collect()));
    }
    Ok((groups, lines))
}

fn by_root(matches: Vec<Match>, paths: &[String]) -> Groups {
    let roots: Vec<PathBuf> = paths.iter().map(|path| root_of(path)).collect();
    let mut groups = Groups::new();
    for found in matches {
        let root = paths
            .iter()
            .zip(&roots)
            .filter(|(path, _)| found.file.starts_with(path.as_str()))
            .max_by_key(|(path, _)| path.len())
            .map_or(&roots[0], |(_, root)| root);
        match groups.iter_mut().find(|(known, _)| known == root) {
            Some((_, group)) => group.push(found),
            None => groups.push((root.clone(), vec![found])),
        }
    }
    groups
}

fn attach_context(matches: &mut [Match], lines: &Lines, before: usize, after: usize) {
    for found in matches {
        let Some(kept) = lines.get(&found.file) else {
            continue;
        };
        let window = |from: i64, to: i64| kept.range(from..to).map(|(_, text)| window_snippet(text, 0)).collect();
        let next = found.line + found.snippet.split('\n').count() as i64;
        found.before = window(found.line - before as i64, found.line);
        found.after = window(next, next + after as i64);
    }
}

/// Every match of `pattern` under `paths`, grouped by the repository each
/// file belongs to, with `-B` and `-A` context attached.
fn searched(pattern: &str, paths: &[String], revision: Option<&str>, options: &Options) -> Result<Groups> {
    // A type is resolved before the search runs: an unknown one must fail by
    // name, never as an empty result the agent reads as "this does not
    // exist".
    let (types, matcher) = crate::lang::ripgrep_types(&options.types)
        .map_err(|message| crate::process::fail(2, format!("Error: {message}")))?;
    let (mut groups, lines) = match revision {
        Some(revision) => at_revision(pattern, revision, paths, options, &matcher)?,
        None => {
            let (matches, lines) = ripgrep(pattern, paths, options, &types)?;
            (by_root(matches, paths), lines)
        }
    };
    for (_, group) in &mut groups {
        attach_context(group, &lines, options.before, options.after);
    }
    Ok(groups)
}

type Enriched<'a> = (
    Vec<matches::EnrichedMatch<'a>>,
    BTreeMap<String, std::sync::Arc<matches::FileEnrichment>>,
    BTreeMap<String, Vec<crate::surface::Row>>,
);

/// Each group's matches with their files' facts and declaration rows.
fn enriched<'a>(groups: &'a Groups, revision: Option<&str>) -> Enriched<'a> {
    let mut enriched = Vec::new();
    let mut files = BTreeMap::new();
    let mut surfaces = BTreeMap::new();
    for (root, group) in groups {
        let relations = crate::relations::ready(root);
        let (group_enriched, group_files, group_surfaces) = matches::enrich(group, root, relations.as_deref(), revision);
        enriched.extend(group_enriched);
        files.extend(group_files);
        surfaces.extend(group_surfaces);
    }
    (enriched, files, surfaces)
}

/// The context `trace hook` adds to a native search that already printed
/// its own result: each matching file's facts and its matches grouped under
/// their declarations, or with `content` only the declarations, through
/// `delivery`.
pub fn native_context(
    pattern: &str,
    paths: &[String],
    options: &Options,
    content: bool,
    delivery: &mut crate::session::Delivery,
) -> Result<String> {
    let groups = searched(pattern, paths, None, options)?;
    let (enriched, _, surfaces) = enriched(&groups, None);
    Ok(matches::render_native(&enriched, &surfaces, content, delivery))
}

pub fn run(
    pattern: &str,
    paths: &[String],
    revision: Option<&str>,
    options: &Options,
    as_json: bool,
    sink: &Sink,
) -> Result<()> {
    let groups = searched(pattern, paths, revision, options)?;
    let abs = cache::absolutize(Path::new(&paths[0]));
    let first_root = root_of(&paths[0]);
    let ((enriched, files, surfaces, signpost), repo_ctx) = rayon::join(
        || {
            let signpost = matches::signpost(matches::searched_name(pattern), &first_root);
            let (enriched, files, surfaces) = enriched(&groups, revision);
            (enriched, files, surfaces, signpost)
        },
        || if as_json { repo_context::repo_context(&abs) } else { Value::Null },
    );

    let nested: Vec<String> = if enriched.is_empty() {
        paths
            .iter()
            .map(|path| cache::absolutize(Path::new(path)))
            .filter(|path| path.is_dir())
            .flat_map(|path| crate::repo_files::nested_repo_rels(&path))
            .collect()
    } else {
        Vec::new()
    };

    if !as_json {
        let mut delivery = crate::session::Delivery::default();
        let shown = matches::offer_signpost(signpost.as_deref(), matches::searched_name(pattern), &first_root, &mut delivery);
        let mut answer = matches::render_human(&enriched, &files, &surfaces, shown, options.files_only, &mut delivery);
        for r in &nested {
            answer.push_str(&format!("nested repository (its own search scope): {r}\n"));
        }
        // Docs belong to the worktree, so a search at a revision sends none.
        let reached: Vec<PathBuf> = if revision.is_none() { paths.iter().map(PathBuf::from).collect() } else { Vec::new() };
        crate::docs::print_with_docs(&answer, &reached, "trace_grep", delivery)?;
    }
    drop(surfaces);

    sink.emit(&matches::SearchDocument {
        query: json!({
            "pattern": pattern,
            "paths": paths,
            "revision": revision,
            "type": options.types,
            "glob": options.globs,
            "ignore_case": options.ignore_case,
            "multiline": options.multiline,
            "before_context": options.before,
            "after_context": options.after,
        }),
        context: matches::SearchContext {
            files: &files,
            repo: &repo_ctx,
            signpost,
        },
        results: &enriched,
        nested_repos: &nested,
    })
}
