//! `trace tree` — recursive annotated file tree with complexity ranks.
//! Discovery via `repo_files::tracked_files` inside a git repo,
//! `walk_files` outside; both honor SKIP_DIRS. Per-file facts come from
//! bounded `file_facts::get_batch` calls, which always do real extraction.

use crate::summary::Facts;
use crate::{cache, file_facts, repo_context, repo_files};
use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A listing line's lead for the file at `rel`, a path under the listing's
/// root: a line for each directory it opens that `open`, the directories of
/// the file listed before it, did not, then its own indent and name. `open`
/// becomes `rel`'s directories.
pub(super) fn lead<'a>(open: &mut Vec<&'a str>, rel: &'a str) -> (String, String, &'a str) {
    let (directories, name) = match rel.rsplit_once('/') {
        Some((directories, name)) => (directories.split('/').collect::<Vec<_>>(), name),
        None => (Vec::new(), rel),
    };
    let kept = open.iter().zip(&directories).take_while(|(a, b)| a == b).count();
    let heading = directories
        .iter()
        .enumerate()
        .skip(kept)
        .map(|(depth, directory)| format!("{}{directory}/\n", "  ".repeat(depth + 1)))
        .collect();
    let indent = "  ".repeat(directories.len() + 1);
    *open = directories;
    (heading, indent, name)
}

pub(super) fn rank_marker(rank: &str) -> &'static str {
    match rank {
        "low" => "·",
        "medium" => "•",
        "high" => "●",
        "critical" => "⚠",
        _ => "?",
    }
}

struct Entry {
    full: PathBuf,
    facts: Option<Facts>,
    max_function_complexity: i64,
}

/// Depth-bounded tree walk under `base`, collecting entries.
fn walk(base: &Path, max_depth: usize) -> Vec<Entry> {
    let repo_root = cache::worktree_root_for(base).unwrap_or_else(|| cache::display_root(base));
    let base_abs = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let tracked = repo_files::tracked_files(&repo_root, (base_abs != repo_root).then_some(base));

    let mut entries: Vec<Entry> = Vec::new();
    // Collect the depth-filtered file set first, then ONE get_batch over
    // it (was O(N²) per-file get() — directory class, same root cause as
    // the list 62s blowup). Real extraction preserved.
    let mut selected: Vec<std::path::PathBuf> = Vec::new();
    match &tracked {
        Some(rels) => {
            for rel in rels.iter() {
                let full = repo_root.join(rel);
                let resolved = full.canonicalize().unwrap_or_else(|_| full.clone());
                let under = match resolved.strip_prefix(&base_abs) {
                    Ok(u) => u.to_string_lossy().to_string(),
                    Err(_) => continue,
                };
                let depth = under.matches('/').count() + 1;
                if depth > max_depth {
                    continue;
                }
                selected.push(full);
            }
        }
        None => {
            let mut walked = repo_files::walk_files(&base_abs);
            walked.sort();
            for full in walked {
                let depth = match full.strip_prefix(&base_abs) {
                    Ok(r) => r.components().count(),
                    Err(_) => continue,
                };
                if depth > max_depth {
                    continue;
                }
                selected.push(full);
            }
        }
    }
    // Depth filter uses a non-strict resolve: canonicalize when the target
    // exists (symlink / `..` normalization, e.g. a symlink into a deeper
    // dir), else the lexical join (keeps indexed-but-deleted files).
    // Applied above when building `selected`.
    let index = crate::relations::ready(&repo_root);
    for chunk in selected.chunks(file_facts::RESOLVE_CHUNK) {
        let facts_map = file_facts::get_batch(chunk, &repo_root);
        for full in chunk {
            let rel = cache::relative_to_root(full, &repo_root);
            let facts = facts_map.get(&rel);
            let graph = index.as_ref().and_then(|index| index.module_counts(&rel));
            entries.push(Entry {
                full: full.clone(),
                facts: facts.map(|f| Facts::of(f, graph.as_ref())),
                max_function_complexity: facts.map_or(0, |f| f.cyclomatic_complexity_max),
            });
        }
    }
    entries
}

pub fn run(path: &Path, depth: usize, as_json: bool) -> Result<Value> {
    crate::pathval::require_exists(path, "PATH")?;
    // The displayed base is the resolved path.
    let base = path
        .canonicalize()
        .unwrap_or_else(|_| cache::absolutize(path));
    let base_abs = base.clone();
    let entries = walk(&base, depth);
    let ctx = repo_context::repo_context(&base);

    // Each file's facts are per-file enrichment, so they sit in `context`
    // keyed by the same path the row carries. A `--filter` that projects rows
    // cannot take them along.
    let mut file_facts_by_path = serde_json::Map::new();
    let files: Vec<Value> = entries
        .iter()
        .map(|e| {
            // Relative path against the UNRESOLVED repo_root/rel —
            // symlinks keep their tracked name.
            let rel = e
                .full
                .strip_prefix(&base_abs)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| e.full.to_string_lossy().to_string());
            if let Some(facts) = &e.facts {
                let mut map = facts.to_map();
                map.insert("max_function_complexity".into(), e.max_function_complexity.into());
                file_facts_by_path.insert(rel.clone(), Value::Object(map));
            }
            json!({"path": rel})
        })
        .collect();
    let value = crate::output::document(
        json!({"root": base.to_string_lossy(), "depth": depth}),
        json!({"repo": ctx.clone(), "files": Value::Object(file_facts_by_path)}),
        json!(files),
        json!({"files": files.len()}),
    );

    if as_json {
        return Ok(value);
    }

    let mut delivery = crate::session::Delivery::default();
    let mut rows: Vec<crate::output::Entry> = Vec::with_capacity(entries.len());
    let mut offered: Vec<Option<String>> = Vec::with_capacity(entries.len());
    let mut paths: Vec<&str> = Vec::with_capacity(entries.len());
    let mut open: Vec<&str> = Vec::new();
    for (e, row) in entries.iter().zip(&files) {
        let rel = row["path"].as_str().unwrap_or_default();
        let (heading, indent, name) = lead(&mut open, rel);
        let marker = rank_marker(e.facts.as_ref().map_or("unknown", |f| &f.complexity_rank));
        let bare = format!("{heading}{indent}{marker} {name}");
        let offer = e.facts.as_ref().and_then(|facts| crate::summary::offer_headline(&mut delivery, &e.full, facts));
        let rank = e.facts.as_ref().and_then(|facts| facts.imported_by).unwrap_or(0) as i64;
        rows.push(match (&e.facts, &offer) {
            (Some(facts), Some(_)) => crate::output::Entry {
                rank,
                levels: vec![format!("{bare}  {}", facts.headline()), bare],
            },
            _ => crate::output::Entry { rank, levels: vec![bare] },
        });
        offered.push(offer);
        paths.push(rel);
    }
    let footer = repo_context::footer(&base, Some(&mut delivery)).map(|line| format!("\n{line}"));
    let root = format!("{}/", base.to_string_lossy());
    let fixed = root.len()
        + 1
        + footer.as_ref().map_or(0, |footer| footer.len() + 1)
        + crate::output::closing_room(rows.len(), "files");
    let crate::output::Fitted { texts, shortened, levels } = crate::output::fit_listing(&rows, &paths, fixed);
    crate::summary::withdraw_cut(&mut delivery, &offered, &levels, 1);
    let mut answer = format!("{root}\n");
    for text in texts {
        answer.push_str(&format!("{text}\n"));
    }
    if let Some(footer) = footer {
        answer.push_str(&format!("{footer}\n"));
    }
    if shortened > 0 {
        answer.push_str(&format!("{}\n", crate::output::shortened_line(shortened, rows.len(), "files")));
    }
    super::docs::print_with_docs(&answer, std::slice::from_ref(&base), "trace_tree", delivery)?;
    Ok(value)
}
