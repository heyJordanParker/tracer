//! Nested Claude.md loading on file-read — command-local helper.
//! Consumed by `trace read` and `trace docs`. Replicates Claude Code's
//! Claude.md walk-up that a subprocess `read` would otherwise bypass.
//!
//! Limits matched: `@path` imports four hops deep, pass + session dedupe by
//! resolved path, visited-dir tracking in rules recursion, nested traversal
//! bounded to file dir..repo_root, a project doc's imports kept inside the
//! repository, 40k-char large warning.
//!
//! Session-scoped dedupe across invocations lives in `crate::session`.

use pulldown_cmark::{Event, Options, Parser, TagEnd};
use regex::Regex;
use std::collections::BTreeSet;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MAX_INCLUDE_DEPTH: i32 = 4;
const LARGE_MEMORY_THRESHOLD: usize = 40_000;
const TEXT_EXTENSIONS: &[&str] = &[
    "md", "txt", "text", "json", "yaml", "yml", "toml", "xml", "csv", "html", "htm", "css", "scss",
    "sass", "less", "js", "ts", "tsx", "jsx", "mjs", "cjs", "mts", "cts", "py", "pyi", "pyw", "rb",
    "erb", "rake", "go", "rs", "java", "kt", "kts", "scala", "c", "cpp", "cc", "cxx", "h", "hpp",
    "hxx", "cs", "swift", "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd", "env", "ini", "cfg",
    "conf", "config", "properties", "sql", "graphql", "gql", "proto", "vue", "svelte", "astro",
    "ejs", "hbs", "pug", "jade", "php", "pl", "pm", "lua", "r", "dart", "ex", "exs", "erl", "hrl",
    "clj", "cljs", "cljc", "edn", "hs", "lhs", "elm", "ml", "mli", "f", "f90", "f95", "for",
    "cmake", "make", "makefile", "gradle", "sbt", "rst", "adoc", "asciidoc", "org", "tex", "latex",
    "lock", "log", "diff", "patch",
];

#[derive(Clone)]
pub struct LoadedMemory {
    pub relative_path: String,
    pub kind: String,
    pub content: String,
    pub size: usize,
    pub large: bool,
    pub path: String,
}

fn resolve(p: &Path) -> Option<PathBuf> {
    p.canonicalize().ok()
}

/// Instruction files for a path, nearest first: the target directory's docs,
/// then each parent's up to the repo root (each doc followed by its
/// imports), then the user-global rules. `directory_mode` skips
/// path-conditional rules.
pub fn load_for_file(
    file_path: &Path,
    repo_root: &Path,
    session_dedupe: &mut BTreeSet<String>,
    directory_mode: bool,
) -> Vec<LoadedMemory> {
    let file_path = match resolve(file_path) {
        Some(p) => p,
        None => return vec![],
    };
    let repo_root = match resolve(repo_root) {
        Some(p) => p,
        None => return vec![],
    };
    if file_path.strip_prefix(&repo_root).is_err() {
        return vec![];
    }
    let target_dir = if file_path.is_file() {
        file_path.parent().unwrap_or(&file_path).to_path_buf()
    } else {
        file_path.clone()
    };

    // Chain: repo_root down to target_dir inclusive.
    let mut chain: Vec<PathBuf> = Vec::new();
    let mut current = target_dir.clone();
    loop {
        if current.strip_prefix(&repo_root).is_err() {
            break;
        }
        chain.push(current.clone());
        if current == repo_root || current.parent() == Some(current.as_path()) {
            break;
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    chain.reverse();

    let mut pass_dedupe: BTreeSet<String> = BTreeSet::new();
    // One group per directory, walked root first so dedupe keeps the
    // outermost copy, then emitted nearest first.
    let mut groups: Vec<Vec<LoadedMemory>> = Vec::new();

    for directory in &chain {
        let mut results: Vec<LoadedMemory> = Vec::new();
        // Project-rules markdown candidates per ancestor. Two harness
        // conventions are recognized: Claude Code's `CLAUDE.md` / `Claude.md`
        // (plus the `.local.md` personal-overrides peer), and OpenAI's
        // `AGENTS.md` — the cross-harness convention adopted by Codex,
        // Cursor, Aider, Jules, Amp et al. — with its dual casing and a
        // matching `.local.md` peer. Both casings of every name are probed;
        // on case-insensitive filesystems `try_load` collapses duplicate
        // physical files via canonical-path dedupe.
        let candidates: [(PathBuf, &str); 10] = [
            (directory.join("CLAUDE.md"), "claude_md"),
            (directory.join("Claude.md"), "claude_md"),
            (directory.join(".claude").join("CLAUDE.md"), "claude_md"),
            (directory.join(".claude").join("Claude.md"), "claude_md"),
            (directory.join("CLAUDE.local.md"), "local_md"),
            (directory.join("Claude.local.md"), "local_md"),
            (directory.join("AGENTS.md"), "agents_md"),
            (directory.join("Agents.md"), "agents_md"),
            (directory.join("AGENTS.local.md"), "agents_local_md"),
            (directory.join("Agents.local.md"), "agents_local_md"),
        ];
        for (candidate, kind) in &candidates {
            if let Some(mem) = try_load(
                candidate,
                &repo_root,
                kind,
                &mut pass_dedupe,
                session_dedupe,
            ) {
                let inc = load_includes(
                    Path::new(&mem.path),
                    &mem.content,
                    &repo_root,
                    false,
                    &mut pass_dedupe,
                    session_dedupe,
                    0,
                );
                results.push(mem);
                results.extend(inc);
            }
        }

        let rules_dir = directory.join(".claude").join("rules");
        if rules_dir.is_dir() {
            for rule_mem in scan_rules_dir(
                &rules_dir,
                &repo_root,
                &file_path,
                &mut pass_dedupe,
                session_dedupe,
                false,
                directory_mode,
            ) {
                let inc = load_includes(
                    Path::new(&rule_mem.path),
                    &rule_mem.content,
                    &repo_root,
                    false,
                    &mut pass_dedupe,
                    session_dedupe,
                    0,
                );
                results.push(rule_mem);
                results.extend(inc);
            }
        }
        groups.push(results);
    }
    groups.reverse();
    let mut results: Vec<LoadedMemory> = groups.into_iter().flatten().collect();

    // User-global rules (`~/.claude/rules`) split by scope. In directory mode
    // — the session-start `trace docs <cwd>` call — only unconditional rules
    // load: there is no file to match a `paths:` glob against, and the harness
    // loads Claude's unconditional user-global rules at session start. In file
    // mode — a file touch — only conditional rules load, matched against the
    // touched file; each file's unconditional rules already surfaced at session
    // start (session dedupe keeps them from surfacing twice).
    if let Some(user_rules) = user_rules_dir() {
        if user_rules.is_dir() {
            let (conditional_only, unconditional_only) = if directory_mode {
                (false, true)
            } else {
                (true, false)
            };
            for rule_mem in scan_rules_dir(
                &user_rules,
                &repo_root,
                &file_path,
                &mut pass_dedupe,
                session_dedupe,
                conditional_only,
                unconditional_only,
            ) {
                let inc = load_includes(
                    Path::new(&rule_mem.path),
                    &rule_mem.content,
                    &repo_root,
                    true,
                    &mut pass_dedupe,
                    session_dedupe,
                    0,
                );
                results.push(rule_mem);
                results.extend(inc);
            }
        }
    }

    results
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `~/.claude/rules`, the user-global rule files.
fn user_rules_dir() -> Option<PathBuf> {
    Some(home_dir()?.join(".claude").join("rules"))
}

pub fn try_load(
    path: &Path,
    repo_root: &Path,
    kind: &str,
    pass_dedupe: &mut BTreeSet<String>,
    session_dedupe: &mut BTreeSet<String>,
) -> Option<LoadedMemory> {
    // Dedupe key + displayed path come from the file's real on-disk
    // identity (canonicalize). On a case-insensitive filesystem this
    // collapses `CLAUDE.md` / `Claude.md` probes of the same physical file
    // to one entry (no double-print); on a case-sensitive filesystem two
    // genuinely distinct files keep distinct keys and both still load.
    let resolved = resolve(path)?;
    if !resolved.is_file() {
        return None;
    }
    let normalized = resolved.to_string_lossy().to_string();
    if pass_dedupe.contains(&normalized) || session_dedupe.contains(&normalized) {
        return None;
    }
    let content = fs::read_to_string(&resolved).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    pass_dedupe.insert(normalized.clone());
    session_dedupe.insert(normalized.clone());

    let relative = match resolved.strip_prefix(repo_root) {
        Ok(r) => r.to_string_lossy().to_string(),
        Err(_) => match home_dir().and_then(|h| {
            resolved
                .strip_prefix(&h)
                .ok()
                .map(|r| format!("~/{}", r.to_string_lossy()))
        }) {
            Some(r) => r,
            None => normalized.clone(),
        },
    };
    let size = content.chars().count();
    Some(LoadedMemory {
        relative_path: relative,
        kind: kind.to_string(),
        large: size >= LARGE_MEMORY_THRESHOLD,
        size,
        content,
        path: normalized,
    })
}

#[allow(clippy::too_many_arguments)]
fn scan_rules_dir(
    rules_dir: &Path,
    repo_root: &Path,
    file_path: &Path,
    pass_dedupe: &mut BTreeSet<String>,
    session_dedupe: &mut BTreeSet<String>,
    conditional_only: bool,
    unconditional_only: bool,
) -> Vec<LoadedMemory> {
    let mut results: Vec<LoadedMemory> = Vec::new();
    let mut visited_dirs: BTreeSet<String> = BTreeSet::new();
    walk_rules(
        rules_dir,
        repo_root,
        file_path,
        pass_dedupe,
        session_dedupe,
        conditional_only,
        unconditional_only,
        &mut visited_dirs,
        &mut results,
    );
    results
}

#[allow(clippy::too_many_arguments)]
fn walk_rules(
    d: &Path,
    repo_root: &Path,
    file_path: &Path,
    pass_dedupe: &mut BTreeSet<String>,
    session_dedupe: &mut BTreeSet<String>,
    conditional_only: bool,
    unconditional_only: bool,
    visited_dirs: &mut BTreeSet<String>,
    results: &mut Vec<LoadedMemory>,
) {
    let d_real = match resolve(d) {
        Some(p) => p.to_string_lossy().to_string(),
        None => return,
    };
    if visited_dirs.contains(&d_real) {
        return;
    }
    visited_dirs.insert(d_real);

    let mut entries: Vec<PathBuf> = match fs::read_dir(d) {
        Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
        Err(_) => return,
    };
    entries.sort();

    for entry in entries {
        let is_symlink = entry
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_symlink {
            if let Some(t) = resolve(&entry) {
                if visited_dirs.contains(&t.to_string_lossy().to_string()) {
                    continue;
                }
            }
        }
        if entry.is_dir() {
            walk_rules(
                &entry,
                repo_root,
                file_path,
                pass_dedupe,
                session_dedupe,
                conditional_only,
                unconditional_only,
                visited_dirs,
                results,
            );
        } else if entry.is_file() && entry.extension().and_then(|e| e.to_str()) == Some("md") {
            let preview = match fs::read_to_string(&entry) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let paths_globs = extract_paths_frontmatter(&preview);
            let kind = match &paths_globs {
                Some(globs) => {
                    if unconditional_only {
                        continue;
                    }
                    if !super::paths_match::matches_paths(file_path, globs, repo_root) {
                        continue;
                    }
                    "rules_conditional"
                }
                None => {
                    if conditional_only {
                        continue;
                    }
                    "rules_unconditional"
                }
            };
            if let Some(mem) = try_load(&entry, repo_root, kind, pass_dedupe, session_dedupe) {
                results.push(mem);
            }
        }
    }
}

/// The `paths:` globs from a file's YAML frontmatter. None when absent.
pub(crate) fn extract_paths_frontmatter(content: &str) -> Option<Vec<String>> {
    if !content.starts_with("---") {
        return None;
    }
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.len() < 2 {
        return None;
    }
    let mut end: Option<usize> = None;
    for (i, l) in lines.iter().enumerate().take(lines.len().min(100)).skip(1) {
        if l.trim_end() == "---" {
            end = Some(i);
            break;
        }
    }
    let end = end?;

    let mut paths: Vec<String> = Vec::new();
    let mut capturing_list = false;
    for ln in &lines[1..end] {
        let stripped = ln.trim_end();
        if stripped.is_empty() {
            if capturing_list {
                break;
            }
            continue;
        }
        let lstripped = stripped.trim_start();
        if let Some(rest) = parse_paths_key(lstripped) {
            let rest = rest.trim();
            if !rest.is_empty() {
                let unquoted = |item: &str| item.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                let globs: Vec<String> = match rest.strip_prefix('[').and_then(|list| list.strip_suffix(']')) {
                    Some(list) => list.split(',').map(unquoted).filter(|glob| !glob.is_empty()).collect(),
                    None => vec![unquoted(rest)],
                };
                return (!globs.is_empty()).then_some(globs);
            }
            capturing_list = true;
            continue;
        }
        if capturing_list {
            let ls = ln.trim_start();
            if let Some(item) = ls.strip_prefix("- ") {
                paths.push(
                    item.trim()
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string(),
                );
            } else if ls.starts_with('#') {
                continue;
            } else {
                break;
            }
        }
    }
    if paths.is_empty() {
        None
    } else {
        Some(paths)
    }
}

/// `^paths\s*:\s*(.*)$` applied to the lstripped line.
fn parse_paths_key(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("paths")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?;
    Some(rest.trim_start())
}

pub fn load_includes(
    including_file: &Path,
    content: &str,
    repo_root: &Path,
    user_scope: bool,
    pass_dedupe: &mut BTreeSet<String>,
    session_dedupe: &mut BTreeSet<String>,
    depth: i32,
) -> Vec<LoadedMemory> {
    if depth >= MAX_INCLUDE_DEPTH {
        return vec![];
    }
    let mut results: Vec<LoadedMemory> = Vec::new();
    let parent = including_file.parent().unwrap_or(Path::new("."));
    for refpath in find_includes(content) {
        let named = match refpath.strip_prefix("~/") {
            Some(rest) => match home_dir() {
                Some(home) => home.join(rest),
                None => continue,
            },
            None => parent.join(&refpath),
        };
        if !is_text_file(&named) {
            continue;
        }
        let included = match resolve(&named) {
            Some(p) => p,
            None => continue,
        };
        if !user_scope && included.strip_prefix(repo_root).is_err() {
            continue;
        }
        if let Some(mem) = try_load(&included, repo_root, "include", pass_dedupe, session_dedupe) {
            let inc = load_includes(
                &included,
                &mem.content,
                repo_root,
                user_scope,
                pass_dedupe,
                session_dedupe,
                depth + 1,
            );
            results.push(mem);
            results.extend(inc);
        }
    }
    results
}

fn is_text_file(path: &Path) -> bool {
    match path.extension() {
        Some(extension) => TEXT_EXTENSIONS.contains(&extension.to_string_lossy().to_lowercase().as_str()),
        None => true,
    }
}

const JS_SPACE: &str = r"\t\n\x0B\f\r \x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}";

fn import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(?:^|[{JS_SPACE}])@((?:[^{JS_SPACE}\\]|\\ )+)")).unwrap())
}

fn frontmatter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\A---\s*\n(?s:.*?)---\s*\n?").unwrap())
}

fn comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").unwrap())
}

fn tag_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r#"(?i)^(?:<([a-z][a-z0-9_-]*)(?: +[a-z:_][a-z0-9_.:-]*(?: *= *"[^"\n]*"| *= *'[^'\n]*'| *= *[^\s"'=<>`]+)?)* */?>"#,
            r"|</([a-z][a-z0-9_-]*)\s*>)[ \t]*$",
        ))
        .unwrap()
    })
}

fn table_delimiter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(?:\| *)?:?-+:? *(?:\| *:?-+:? *)*(?:\| *)?$").unwrap())
}

fn without_frontmatter(content: &str) -> &str {
    let plain = content.strip_prefix('\u{feff}').unwrap_or(content);
    match frontmatter_re().find(plain) {
        Some(frontmatter) => &plain[frontmatter.end()..],
        None => content,
    }
}

fn scan(text: &str, paths: &mut Vec<String>) {
    for found in import_re().captures_iter(text) {
        let written = found[1].split('#').next().unwrap_or_default().replace("\\ ", " ");
        let is_path = written.starts_with("~/")
            || (written.starts_with('/') && written != "/")
            || written.starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if is_path && !paths.contains(&written) {
            paths.push(written);
        }
    }
}

fn scan_html(html: &str, paths: &mut Vec<String>) {
    let start = html.trim_start();
    if start.starts_with("<!--") && start.contains("-->") {
        let rest = comment_re().replace_all(html, "");
        if !rest.trim().is_empty() {
            scan(&rest, paths);
        }
    }
}

fn opens_html_block(line: &str) -> bool {
    tag_line_re().captures(line).is_some_and(|tag| {
        let name = tag.get(1).or(tag.get(2)).map_or(String::new(), |name| name.as_str().to_lowercase());
        !["script", "pre", "style", "textarea"].iter().any(|raw| name.starts_with(raw))
    })
}

fn cell_count(row: &str) -> usize {
    let mut cells = Vec::new();
    let mut start = 0;
    let mut backslashes = 0;
    for (index, c) in row.char_indices() {
        if c == '|' && backslashes % 2 == 0 {
            cells.push(&row[start..index]);
            start = index + 1;
        }
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
    }
    cells.push(&row[start..]);
    let leading = usize::from(cells[0].trim().is_empty());
    let trailing = usize::from(cells.len() > leading && cells[cells.len() - 1].trim().is_empty());
    cells.len() - leading - trailing
}

fn opens_table(header: &str, delimiter: &str) -> bool {
    let aligns = delimiter.strip_prefix('|').unwrap_or(delimiter).trim_end();
    let aligns = aligns.strip_suffix('|').unwrap_or(aligns);
    !header.trim().is_empty()
        && delimiter.contains([':', '|'])
        && table_delimiter_re().is_match(delimiter)
        && cell_count(header) == aligns.split('|').count()
}

fn marked_block_start(source: &str, span: &Range<usize>, in_item: bool) -> Option<usize> {
    let first_line = source[..span.start].rfind('\n').map_or(0, |newline| newline + 1);
    let mut lines = Vec::new();
    let mut offset = first_line;
    for line in source[first_line..span.end].split('\n') {
        lines.push((offset, line.trim_start_matches([' ', '\t', '>'])));
        offset += line.len() + 1;
    }
    lines.iter().enumerate().find_map(|(index, &(offset, line))| {
        let starts_block = (in_item && opens_html_block(line))
            || lines.get(index + 1).is_some_and(|&(_, next)| opens_table(line, next));
        starts_block.then_some(offset)
    })
}

fn scan_text(source: &str, runs: &[Range<usize>], item_text: Option<Range<usize>>, in_item: bool, paths: &mut Vec<String>) {
    let mut spans = item_text.iter().chain(runs);
    let Some(first) = spans.next() else {
        return;
    };
    let span = spans.fold(first.clone(), |span, next| span.start.min(next.start)..span.end.max(next.end));
    let cut = marked_block_start(source, &span, in_item).unwrap_or(span.end);
    for text in item_text.iter().chain(runs) {
        if text.start < cut {
            scan(&source[text.start..text.end.min(cut)], paths);
        }
    }
}

fn find_includes(content: &str) -> Vec<String> {
    let source = without_frontmatter(content);
    let mut paths = Vec::new();
    let mut blocks: Vec<TagEnd> = Vec::new();
    let mut hidden = 0usize;
    let mut runs: Vec<Range<usize>> = Vec::new();
    let mut item_text: Option<Range<usize>> = None;
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        let tag = match &event {
            Event::Start(tag) => Some(tag.to_end()),
            Event::End(tag) => Some(*tag),
            _ => None,
        };
        let is_block = matches!(event, Event::Rule)
            || tag.is_some_and(|tag| {
                !matches!(tag, TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link | TagEnd::Image)
            });
        let in_item_text = matches!(blocks.as_slice(), [.., TagEnd::Item] | [.., TagEnd::Item, TagEnd::Paragraph]);
        if is_block {
            scan_text(source, &runs, item_text.take(), in_item_text, &mut paths);
            runs.clear();
        } else if in_item_text {
            item_text = Some(item_text.map_or(range.clone(), |text| text.start..range.end));
        }
        if matches!(event, Event::Text(_)) && hidden == 0 {
            let escaped = range.start > 0 && source.as_bytes()[range.start - 1] == b'\\';
            match runs.last_mut() {
                Some(run) if !escaped && run.end == range.start => run.end = range.end,
                _ => runs.push(if escaped { range.start + 1 } else { range.start }..range.end),
            }
        }
        let Some(tag) = tag else {
            continue;
        };
        let hides = matches!(tag, TagEnd::CodeBlock | TagEnd::Table | TagEnd::Image);
        match event {
            Event::Start(_) => {
                hidden += usize::from(hides);
                if is_block {
                    blocks.push(tag);
                }
            }
            _ => {
                hidden -= usize::from(hides);
                if tag == TagEnd::HtmlBlock {
                    scan_html(&source[range], &mut paths);
                }
                if is_block {
                    blocks.pop();
                }
            }
        }
    }
    paths
}
