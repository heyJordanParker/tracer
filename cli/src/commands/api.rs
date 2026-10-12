//! `trace api` — the public API of the files and folders named, laid out the
//! way `tree` lays out a listing: each file's facts and purpose, then every
//! declaration and re-export the visibility asked for reaches, each header
//! whole.

use crate::extraction::Visibility;
use crate::file_facts::FileFacts;
use crate::summary::Facts;
use crate::surface::{self, Row};
use crate::{cache, extraction, file_facts, output, pathval, relations};
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// How far a row must reach to be listed. Each level lists itself and every
/// wider one, the way log levels do.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Level {
    /// The public API, and the rows whose visibility is unknown.
    Public,
    /// Everything other files can reach.
    Restricted,
    /// Every row, with each file's imports.
    All,
}

impl Level {
    fn lists(self, visibility: Visibility) -> bool {
        visibility
            >= match self {
                Level::Public => Visibility::Unknown,
                Level::Restricted => Visibility::Restricted,
                Level::All => Visibility::FileLocal,
            }
    }

    fn flag(self) -> &'static str {
        match self {
            Level::Public => "",
            Level::Restricted => " --visibility restricted",
            Level::All => " --visibility all",
        }
    }
}

/// What `api` lists.
pub struct Scope<'a> {
    pub visibility: Level,
    /// Only files another file imports.
    pub imported: bool,
    pub depth: usize,
    /// ripgrep's `-g` globs, matched against repository-relative paths.
    pub globs: &'a [String],
}

/// One file with API to list.
struct Listed {
    /// The path typed, joined with the file's path under it: the path a
    /// follow-up command names.
    named: String,
    /// Its path under its listing's root.
    under: String,
    full: PathBuf,
    facts: Facts,
    purpose: Option<String>,
    rows: Vec<Row>,
}

/// The files one path argument names, under the root its listing prints.
struct Group {
    root: String,
    files: Vec<Listed>,
}

pub fn run(paths: &[PathBuf], scope: &Scope, as_json: bool) -> Result<Value> {
    let globbed = super::grep::globbed(scope.globs)?;
    let mut groups = Vec::new();
    for path in paths {
        if !path.exists() {
            pathval::report(path, "PATH", "does not exist");
            continue;
        }
        let full = path.canonicalize().unwrap_or_else(|_| cache::absolutize(path));
        let base = if full.is_dir() { full.clone() } else { full.parent().map_or_else(|| full.clone(), Path::to_path_buf) };
        let repo_root = cache::worktree_root_for(&full).unwrap_or_else(|| cache::display_root(&full));
        let canonical_root = repo_root.canonicalize().unwrap_or_else(|_| repo_root.clone());
        let index = if scope.imported { Some(relations::get(&repo_root)) } else { relations::ready(&repo_root) };
        let mut chosen: Vec<(PathBuf, String, String, String)> = Vec::new();
        for file in pathval::files_under(std::slice::from_ref(path), "PATH") {
            let file = file.canonicalize().unwrap_or(file);
            let (Ok(under), Ok(relative)) = (file.strip_prefix(&base), file.strip_prefix(&canonical_root)) else { continue };
            let relative = relative.to_string_lossy().into_owned();
            let imported = || index.as_ref().is_some_and(|index| index.resolved_importers_of(&relative).next().is_some());
            if under.components().count() > scope.depth || !globbed(Path::new(&relative)) || (scope.imported && !imported()) {
                continue;
            }
            let named = match full.is_dir() {
                true => path.join(under).to_string_lossy().into_owned(),
                false => path.to_string_lossy().into_owned(),
            };
            let under = under.to_string_lossy().into_owned();
            chosen.push((file, relative, named, under));
        }
        let mut files = Vec::new();
        for chunk in chosen.chunks(file_facts::RESOLVE_CHUNK) {
            let fulls: Vec<PathBuf> = chunk.iter().map(|(file, ..)| file.clone()).collect();
            let facts = file_facts::get_batch(&fulls, &repo_root);
            for (file, relative, named, under) in chunk {
                let Some(facts) = facts.get(relative) else { continue };
                let rows = rows(facts, scope.visibility, file);
                if rows.is_empty() {
                    continue;
                }
                let graph = index.as_ref().and_then(|index| index.module_counts(relative));
                files.push(Listed {
                    named: named.clone(),
                    under: under.clone(),
                    full: file.clone(),
                    facts: Facts::of(facts, graph.as_ref()),
                    purpose: facts.leading_comment.as_deref().map(first_sentence),
                    rows,
                });
            }
        }
        groups.push(Group { root: format!("{}/", base.to_string_lossy()), files });
    }

    let value = document(paths, scope, &groups);
    if as_json {
        return Ok(value);
    }
    let mut delivery = crate::session::Delivery::default();
    let answer = render(&groups, scope.visibility, &mut delivery);
    let bases: Vec<PathBuf> = groups.iter().map(|group| PathBuf::from(group.root.trim_end_matches('/'))).collect();
    super::docs::print_with_docs(&answer, &bases, "trace_api", delivery)?;
    Ok(value)
}

/// The rows of `facts` that `level` lists, with its re-exports, and under
/// `Level::All` its imports, each in line order among the top-level rows.
fn rows(facts: &FileFacts, level: Level, full: &Path) -> Vec<Row> {
    let Some(extraction) = &facts.extraction else {
        return Vec::new();
    };
    // A row is as visible as its narrowest parent, so the listed rows hold
    // every listed row's parent.
    let mut at: HashMap<u32, u32> = HashMap::new();
    let mut declared = Vec::new();
    for (index, row) in surface::rows(facts, None).into_iter().enumerate() {
        if level.lists(row.visibility) {
            at.insert(index as u32, declared.len() as u32);
            declared.push(row);
        }
    }
    for row in &mut declared {
        row.parent = row.parent.and_then(|parent| at.get(&parent).copied());
    }
    // One row per statement, whose imports share its lines: its widest
    // visibility, named by the names it binds, else by its module.
    let mut statements: BTreeMap<(i64, i64), (Visibility, Vec<&str>, &str)> = BTreeMap::new();
    for import in &extraction.imports {
        let visibility = import.visibility.unwrap_or(Visibility::FileLocal);
        if level.lists(visibility) {
            let (widest, names, _) = statements.entry((import.line, import.end_line)).or_insert((visibility, Vec::new(), &import.module));
            *widest = visibility.max(*widest);
            names.extend(import.binding());
        }
    }
    let source = match statements.is_empty() {
        true => Vec::new(),
        false => std::fs::read(full).unwrap_or_default(),
    };
    let imports = statements.into_iter().map(|((line, end_line), (visibility, names, module))| Row {
        header_line: line,
        line,
        end_line,
        kind: if visibility > Visibility::FileLocal { "reexport" } else { "import" }.to_string(),
        name: if names.is_empty() { module.to_string() } else { names.join(", ") },
        container: None,
        parent: None,
        header: extraction::source_lines(&source, line, end_line),
        annotations: Vec::new(),
        cyclomatic_complexity: None,
        visibility,
    });
    let mut merged = Vec::with_capacity(declared.len() + imports.len());
    let mut moved: HashMap<u32, u32> = HashMap::new();
    let mut pending = imports.into_iter().peekable();
    for (index, row) in declared.into_iter().enumerate() {
        if row.parent.is_none() {
            while let Some(import) = pending.next_if(|import| import.line < row.line) {
                merged.push(import);
            }
        }
        moved.insert(index as u32, merged.len() as u32);
        merged.push(row);
    }
    merged.extend(pending);
    for row in merged.iter_mut().filter(|row| row.parent.is_some()) {
        row.parent = row.parent.and_then(|parent| moved.get(&parent).copied());
    }
    merged
}

/// A comment's first sentence: its first paragraph up to the first period
/// that ends a sentence, on one line.
fn first_sentence(comment: &str) -> String {
    let paragraph = comment.lines().take_while(|line| !line.trim().is_empty()).map(str::trim).collect::<Vec<_>>().join(" ");
    match paragraph.match_indices(". ").next() {
        Some((at, _)) => paragraph[..=at].to_string(),
        None => paragraph,
    }
}

/// The text listing, fitted to the budget: full headers are never cut. Past
/// the budget the files' facts go first, then their purposes, then the
/// least-imported files collapse to one line naming the command that lists
/// them. A single file larger than the budget prints whole rows up to it.
fn render(groups: &[Group], level: Level, delivery: &mut crate::session::Delivery) -> String {
    let files: Vec<(&Group, &Listed)> = groups.iter().flat_map(|group| group.files.iter().map(move |file| (group, file))).collect();
    if files.is_empty() {
        let none = match level {
            Level::Public => "(no public declarations)",
            _ => "(no declarations)",
        };
        return groups.iter().map(|group| format!("{}\n  {none}\n", group.root)).collect();
    }
    let offered: Vec<Option<String>> = files.iter().map(|(_, file)| crate::summary::offer_headline(delivery, &file.full, &file.facts)).collect();
    let mut leads = Vec::with_capacity(files.len());
    let mut open: Vec<&str> = Vec::new();
    let mut root = "";
    for (group, file) in &files {
        if group.root != root {
            open.clear();
            root = &group.root;
        }
        leads.push(super::tree::lead(&mut open, &file.under));
    }
    let head = |at: usize, stage: usize| {
        let ((heading, indent, name), file) = (&leads[at], files[at].1);
        let mut line = format!("{heading}{indent}{} {name}", super::tree::rank_marker(&file.facts.complexity_rank));
        if stage == 0 && offered[at].is_some() {
            line.push_str(&format!("  {}", file.facts.headline()));
        }
        if let Some(purpose) = file.purpose.as_ref().filter(|_| stage <= 1) {
            line.push_str(&format!("  — {purpose}"));
        }
        line
    };
    let indented = |at: usize, text: &str| {
        let indent = format!("{}  ", leads[at].1);
        text.lines().map(|line| format!("{indent}{line}")).collect::<Vec<_>>().join("\n")
    };
    let rows = |at: usize, budget: Option<usize>| {
        let file = files[at].1;
        surface::render_rows(&file.rows, &file.under, None, budget, surface::Fit::Whole)
    };
    let entry = |at: usize, stage: usize| format!("{}\n{}", head(at, stage), indented(at, &rows(at, None).text));
    let roots = |texts: &[String]| {
        let mut out = String::new();
        let mut root = "";
        for ((group, _), text) in files.iter().zip(texts) {
            if group.root != root {
                root = &group.root;
                out.push_str(&format!("{root}\n"));
            }
            out.push_str(&format!("{text}\n"));
        }
        out
    };
    let budget = output::budget();
    let fixed: usize = groups.iter().map(|group| output::width(&group.root) + 1).sum::<usize>() + output::closing_room(files.len(), "files");
    let size = |texts: &[String]| fixed + texts.iter().map(|text| output::width(text) + 1).sum::<usize>();
    for stage in 0..3 {
        let texts: Vec<String> = (0..files.len()).map(|at| entry(at, stage)).collect();
        if budget.is_none_or(|budget| size(&texts) <= budget) {
            crate::summary::withdraw_cut(delivery, &offered, &vec![stage; files.len()], 1);
            return roots(&texts);
        }
    }
    crate::summary::withdraw_cut(delivery, &offered, &vec![2; files.len()], 1);
    let budget = budget.unwrap_or(usize::MAX);
    if files.len() == 1 {
        let (group, head) = (files[0].0, head(0, 2));
        let closing = output::closing_room(files[0].1.rows.len(), "declarations");
        let room = budget.saturating_sub(output::width(&group.root) + output::width(&head) + 2 + closing);
        let fitted = rows(0, Some(room));
        return format!(
            "{}\n{head}\n{}\n{}\n",
            group.root,
            indented(0, &fitted.text),
            output::shortened_line(fitted.cut, fitted.of, "declarations")
        );
    }
    let entries: Vec<output::Entry> = (0..files.len())
        .map(|at| {
            let ((heading, indent, name), file) = (&leads[at], files[at].1);
            let listed = match level {
                Level::Public => output::counted(file.rows.len(), "public declaration", "public declarations"),
                _ => output::counted(file.rows.len(), "declaration", "declarations"),
            };
            let collapsed = format!(
                "{heading}{indent}{} {name}  … {listed} → {} api {}{}",
                super::tree::rank_marker(&file.facts.complexity_rank),
                output::trace_command(),
                crate::shell::quote(&file.named),
                level.flag()
            );
            output::Entry { rank: file.facts.imported_by.unwrap_or(0) as i64, levels: vec![entry(at, 2), collapsed] }
        })
        .collect();
    let named: Vec<&str> = files.iter().map(|(_, file)| file.named.as_str()).collect();
    let output::Fitted { texts, shortened, levels } = output::fit_listing(&entries, &named, fixed);
    let listing = match levels.contains(&usize::MAX) {
        true => texts.iter().map(|text| format!("{text}\n")).collect(),
        false => roots(&texts),
    };
    format!("{listing}{}\n", output::shortened_line(shortened, files.len(), "files"))
}

/// The `--json` document: one result per listed row, in `surface`'s row
/// shape with its `file` and `visibility`, and each file's facts in
/// `context`.
fn document(paths: &[PathBuf], scope: &Scope, groups: &[Group]) -> Value {
    let mut facts = Map::new();
    let mut results = Vec::new();
    for file in groups.iter().flat_map(|group| &group.files) {
        facts.insert(file.named.clone(), Value::Object(file.facts.to_map()));
        for row in &file.rows {
            let mut result = serde_json::to_value(row).unwrap_or_default();
            result["file"] = file.named.clone().into();
            result["visibility"] = serde_json::to_value(row.visibility).unwrap_or_default();
            results.push(result);
        }
    }
    let files = facts.len();
    output::document(
        json!({
            "paths": paths,
            "visibility": match scope.visibility { Level::Public => "public", Level::Restricted => "restricted", Level::All => "all" },
            "imported": scope.imported,
            "depth": scope.depth,
            "globs": scope.globs,
        }),
        json!({"files": facts}),
        json!(results),
        json!({"files": files, "rows": results.len()}),
    )
}
