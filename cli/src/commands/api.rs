//! `trace api` — the public API of the files and folders named, laid out the
//! way `tree` lays out a listing: each file's facts and purpose, then every
//! declaration and re-export the visibility asked for reaches, each header
//! whole. `--revision` reads the files at a commit, and `--output` writes the
//! API to a file instead, without line numbers or facts.

use crate::extraction::Visibility;
use crate::file_facts::FileFacts;
use crate::summary::Facts;
use crate::surface::{self, Row};
use crate::{cache, extraction, file_facts, git_activity, output, pathval, process, relations};
use anyhow::Result;
use rayon::prelude::*;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// How far a row must reach to be listed. Each level lists itself and every
/// wider one, the way log levels do.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Level {
    /// The public API, and the rows whose visibility is unknown.
    Public,
    /// Also what the language keeps inside its module or package, and what
    /// an `Internal` attribute or an `@internal` doc tag marks.
    Internal,
    /// Every row, with each file's imports.
    All,
}

impl Level {
    fn lists(self, visibility: Visibility) -> bool {
        visibility
            >= match self {
                Level::Public => Visibility::Unknown,
                Level::Internal => Visibility::Internal,
                Level::All => Visibility::FileLocal,
            }
    }

    fn name(self) -> &'static str {
        match self {
            Level::Public => "public",
            Level::Internal => "internal",
            Level::All => "all",
        }
    }

}

impl Scope<'_> {
    /// The flags a follow-up command needs to list what this one listed.
    fn flags(&self) -> String {
        let mut flags = String::new();
        if self.visibility != Level::Public {
            flags.push_str(&format!(" --visibility {}", self.visibility.name()));
        }
        if let Some(revision) = self.revision {
            flags.push_str(&format!(" --revision {}", crate::shell::quote(revision)));
        }
        flags
    }
}

/// What `api` lists.
pub struct Scope<'a> {
    pub visibility: Level,
    /// Only files another file imports.
    pub imported: bool,
    /// How many folders below a directory argument a file may sit, as
    /// ripgrep counts `--max-depth`; none lists every depth.
    pub max_depth: Option<usize>,
    /// ripgrep's `-g` globs, matched against repository-relative paths.
    pub globs: &'a [String],
    /// The commit the files are read at, instead of the working tree.
    pub revision: Option<&'a str>,
}

/// One file with API to list.
struct Listed {
    /// The path typed, joined with the file's path under it: the path a
    /// follow-up command names.
    named: String,
    /// Its path under its listing's root.
    under: String,
    /// Its path in its repository.
    relative: String,
    /// The key its facts are offered under: its full path, qualified by the
    /// revision it was read at, so a past version never stands in for the
    /// working tree's.
    key: PathBuf,
    facts: Facts,
    purpose: Option<String>,
    rows: Vec<Row>,
}

/// The files one path argument names, under the root its listing prints.
struct Group {
    root: String,
    files: Vec<Listed>,
}

/// A file a path argument names that the listing keeps.
struct Chosen {
    full: PathBuf,
    relative: String,
    named: String,
    under: String,
}

pub fn run(paths: &[PathBuf], scope: &Scope, written: Option<&Path>, as_json: bool) -> Result<Value> {
    if scope.imported && scope.revision.is_some() {
        return Err(process::fail(2, "Error: --imported reads the working tree's imports, so it can't combine with --revision"));
    }
    let globbed = super::grep::globbed(scope.globs)?;
    let mut groups = Vec::new();
    for path in paths {
        let group = match scope.revision {
            Some(revision) => at_revision(path, revision, scope, &globbed)?,
            None => in_worktree(path, scope, written.is_none(), &globbed),
        };
        groups.extend(group);
    }

    let value = document(paths, scope, &groups);
    if as_json {
        return Ok(value);
    }
    let mut delivery = crate::session::Delivery::default();
    let answer = match written {
        Some(file) => {
            std::fs::write(file, plain(&groups))
                .map_err(|error| process::fail(2, format!("Error: could not write {}: {error}", file.display())))?;
            let files = groups.iter().map(|group| group.files.len()).sum();
            format!("wrote the API of {} to {}\n", output::counted(files, "file", "files"), file.display())
        }
        None => render(&groups, scope, &mut delivery),
    };
    // The docs on disk describe the working tree, never a past revision.
    let bases: Vec<PathBuf> = match scope.revision {
        None => groups.iter().map(|group| PathBuf::from(group.root.trim_end_matches('/'))).collect(),
        Some(_) => Vec::new(),
    };
    crate::docs::print_with_docs(&answer, &bases, "trace_api", delivery)?;
    Ok(value)
}

/// The files `path` names in the working tree, with their facts. `shown`
/// is false when the API goes to a file, which prints no facts, so neither
/// the git facts nor the import index are read for it.
fn in_worktree(path: &Path, scope: &Scope, shown: bool, globbed: &impl Fn(&Path) -> bool) -> Option<Group> {
    if !path.exists() {
        pathval::report(path, "PATH", "does not exist");
        return None;
    }
    let full = path.canonicalize().unwrap_or_else(|_| cache::absolutize(path));
    let is_dir = full.is_dir();
    let base = if is_dir { full.clone() } else { full.parent().map_or_else(|| full.clone(), Path::to_path_buf) };
    let repo_root = cache::worktree_root_for(&full).unwrap_or_else(|| cache::display_root(&full));
    let canonical_root = repo_root.canonicalize().unwrap_or_else(|_| repo_root.clone());
    let index = match (scope.imported, shown) {
        (true, _) => Some(relations::get(&repo_root)),
        (false, true) => relations::ready(&repo_root),
        (false, false) => None,
    };
    let imported = |relative: &str| index.as_ref().is_some_and(|index| index.resolved_importers_of(relative).next().is_some());
    let chosen: Vec<Chosen> = pathval::files_under(&[path.to_path_buf()], "PATH")
        .into_iter()
        .filter_map(|file| {
            // A link inside a folder repeats the file it names, which the
            // listing holds under its own path, and a revision lists none.
            if is_dir && file.symlink_metadata().is_ok_and(|metadata| metadata.is_symlink()) {
                return None;
            }
            let file = file.canonicalize().unwrap_or(file);
            let under = file.strip_prefix(&base).ok()?.to_path_buf();
            let relative = file.strip_prefix(&canonical_root).ok()?.to_string_lossy().into_owned();
            (!scope.imported || imported(&relative)).then_some(())?;
            choose(path, is_dir, file, relative, &under, scope, globbed)
        })
        .collect();
    let mut files = Vec::new();
    for chunk in chosen.chunks(file_facts::RESOLVE_CHUNK) {
        let fulls: Vec<PathBuf> = chunk.iter().map(|chosen| chosen.full.clone()).collect();
        let facts = match shown {
            true => file_facts::get_batch(&fulls, &repo_root),
            false => file_facts::extracted(&fulls, &repo_root),
        };
        for chosen in chunk {
            let Some(facts) = facts.get(&chosen.relative) else { continue };
            let graph = index.as_ref().and_then(|index| index.module_counts(&chosen.relative));
            files.extend(listed(chosen, chosen.full.clone(), facts, Facts::of(facts, graph.as_ref()), scope.visibility, || {
                std::fs::read(&chosen.full).unwrap_or_default()
            }));
        }
    }
    Some(Group { root: format!("{}/", base.to_string_lossy()), files })
}

/// The files `path` names at `revision`, read from git's objects, with the
/// facts their contents give. A path the revision lacks is reported, and an
/// unknown revision fails the run.
fn at_revision(path: &Path, revision: &str, scope: &Scope, globbed: &impl Fn(&Path) -> bool) -> Result<Option<Group>> {
    let full = cache::absolutize(path);
    let Some(anchor) = full.ancestors().find(|ancestor| ancestor.exists()) else {
        pathval::report(path, "PATH", "does not exist");
        return Ok(None);
    };
    let Some(repo_root) = cache::worktree_root_for(anchor) else {
        return Err(process::fail(2, format!("Error: {} is not in a git repository, so it has no revision {revision}", path.display())));
    };
    let canonical_root = repo_root.canonicalize().unwrap_or_else(|_| repo_root.clone());
    let canonical_anchor = anchor.canonicalize().unwrap_or_else(|_| anchor.to_path_buf());
    let below = full.strip_prefix(anchor).unwrap_or(Path::new(""));
    let relative = canonical_anchor.join(below).strip_prefix(&canonical_root).map(Path::to_path_buf).unwrap_or_default();
    let tree = git_activity::git_output(&repo_root, ["ls-tree", "-r", "-z", revision, "--", &format!("./{}", relative.to_string_lossy())])?;
    if !tree.status.success() {
        return Err(process::fail(2, format!("Error: unknown revision '{revision}'")));
    }
    // Each entry is `<mode> <type> <object>\t<path>`; a submodule is a commit,
    // and a symbolic link, mode 120000, is a blob holding the path it names.
    let mut blobs: Vec<(String, String)> = tree
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            let (meta, name) = entry.split_once('\t')?;
            match meta.split(' ').collect::<Vec<_>>().as_slice() {
                [mode, "blob", object] if *mode != "120000" => Some((object.to_string(), name.to_string())),
                _ => None,
            }
        })
        .collect();
    if blobs.is_empty() {
        pathval::report(path, "PATH", &format!("does not exist at {revision}"));
        return Ok(None);
    }
    // git orders `a.rs` before `a/`; the working tree's listing compares
    // paths by component, and both versions of the API list in one order.
    blobs.sort_by(|a, b| Path::new(&a.1).cmp(Path::new(&b.1)));
    let is_dir = blobs.len() > 1 || Path::new(&blobs[0].1) != relative;
    let base_relative = if is_dir { relative.clone() } else { relative.parent().map(Path::to_path_buf).unwrap_or_default() };
    let base = canonical_root.join(&base_relative);
    let chosen: Vec<(String, Chosen)> = blobs
        .into_iter()
        .filter_map(|(object, name)| {
            let under = Path::new(&name).strip_prefix(&base_relative).ok()?.to_path_buf();
            Some((object, choose(path, is_dir, canonical_root.join(&name), name, &under, scope, globbed)?))
        })
        .collect();
    let mut files = Vec::new();
    for chunk in chosen.chunks(file_facts::RESOLVE_CHUNK) {
        let objects: Vec<String> = chunk.iter().map(|(object, _)| object.clone()).collect();
        let mut contents: Vec<Option<Vec<u8>>> = vec![None; chunk.len()];
        git_activity::blobs(&repo_root, &objects, |at, bytes| contents[at] = bytes.map(<[u8]>::to_vec));
        let read: Vec<Option<Listed>> = chunk
            .par_iter()
            .zip(contents)
            .map(|((_, chosen), bytes)| {
                let Some(bytes) = bytes else {
                    pathval::report(Path::new(&chosen.named), "PATH", &format!("could not be read at {revision}"));
                    return None;
                };
                let facts = file_facts::of_bytes(&bytes, &chosen.relative, &repo_root);
                let key = PathBuf::from(format!("{}@{revision}", chosen.full.to_string_lossy()));
                listed(chosen, key, &facts, Facts::of(&facts, None), scope.visibility, || bytes)
            })
            .collect();
        files.extend(read.into_iter().flatten());
    }
    Ok(Some(Group { root: format!("{}/", base.to_string_lossy()), files }))
}

/// `file` as the listing keeps it, when `-g` and `--max-depth` leave it in.
/// A file named on the command line is always kept, the way ripgrep always
/// searches a file it is given.
fn choose(path: &Path, is_dir: bool, file: PathBuf, relative: String, under: &Path, scope: &Scope, globbed: &impl Fn(&Path) -> bool) -> Option<Chosen> {
    let deeper = is_dir && scope.max_depth.is_some_and(|max| under.components().count() > max);
    if deeper || !globbed(Path::new(&relative)) {
        return None;
    }
    let named = match is_dir {
        true => path.join(under).to_string_lossy().into_owned(),
        false => path.to_string_lossy().into_owned(),
    };
    Some(Chosen { full: file, relative, named, under: under.to_string_lossy().into_owned() })
}

/// `chosen` with the rows of `facts` its level lists, or none when it lists
/// no row.
fn listed(chosen: &Chosen, key: PathBuf, facts: &FileFacts, shown: Facts, level: Level, source: impl FnOnce() -> Vec<u8>) -> Option<Listed> {
    let rows = rows(facts, level, source);
    (!rows.is_empty()).then(|| Listed {
        named: chosen.named.clone(),
        under: chosen.under.clone(),
        relative: chosen.relative.clone(),
        key,
        facts: shown,
        purpose: facts.leading_comment.as_deref().map(first_sentence),
        rows,
    })
}

/// The rows of `facts` that `level` lists, with its re-exports, and under
/// `Level::All` its imports, each in line order among the top-level rows.
/// `source` gives the file's bytes, which an import's text is cut from.
fn rows(facts: &FileFacts, level: Level, source: impl FnOnce() -> Vec<u8>) -> Vec<Row> {
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
        false => source(),
    };
    let starts = extraction::line_starts(&source);
    let imports = statements.into_iter().map(|((line, end_line), (visibility, names, module))| Row {
        header_line: line,
        line,
        end_line,
        kind: if visibility > Visibility::FileLocal { "reexport" } else { "import" }.to_string(),
        name: if names.is_empty() { module.to_string() } else { names.join(", ") },
        container: None,
        parent: None,
        header: extraction::source_lines(&source, &starts, line, end_line),
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

/// The API as `--output` writes it: each file's path in its repository and
/// purpose, then its rows whole, indented under it, with no line number,
/// complexity, or facts, so two versions differ only where the API does.
fn plain(groups: &[Group]) -> String {
    let mut out = String::new();
    for file in groups.iter().flat_map(|group| &group.files) {
        out.push_str(&file.relative);
        if let Some(purpose) = &file.purpose {
            out.push_str(&format!("  — {purpose}"));
        }
        out.push('\n');
        for line in surface::render_plain(&file.rows).lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out
}

/// The text listing, fitted to the budget: full headers are never cut. Past
/// the budget the files' facts go first, then their purposes, then the
/// least-imported files collapse to one line naming the command that lists
/// them. A single file larger than the budget prints whole rows up to it.
fn render(groups: &[Group], scope: &Scope, delivery: &mut crate::session::Delivery) -> String {
    let level = scope.visibility;
    let files: Vec<(&Group, &Listed)> = groups.iter().flat_map(|group| group.files.iter().map(move |file| (group, file))).collect();
    if files.is_empty() {
        let none = match level {
            Level::Public => "(no public declarations)",
            _ => "(no declarations)",
        };
        return groups.iter().map(|group| format!("{}\n  {none}\n", group.root)).collect();
    }
    let offered: Vec<Option<String>> = files.iter().map(|(_, file)| crate::summary::offer_headline(delivery, &file.key, &file.facts)).collect();
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
        surface::render_rows(&file.rows, &file.under, None, budget, surface::Fit::Whole, None)
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
                scope.flags()
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
            "visibility": scope.visibility.name(),
            "imported": scope.imported,
            "max_depth": scope.max_depth,
            "globs": scope.globs,
            "revision": scope.revision,
        }),
        json!({"files": facts}),
        json!(results),
        json!({"files": files, "rows": results.len()}),
    )
}
