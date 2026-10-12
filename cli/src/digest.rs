//! File digest helpers.
//! top_callers, immediate_dependencies, nearest_doc.

use crate::relations;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// The docs tracer sends an agent itself under a session. With no session
/// nothing sends them, so `nearest_doc` names them too.
const DELIVERED_DOCS: &[&str] = &["Claude.md", "CLAUDE.md"];

/// The docs no tracer delivery sends, which `nearest_doc` always names.
const DOC_FILENAMES: &[&str] = &["Readme.md", "README.md", "ARCHITECTURE.md", "architecture.md"];

/// Walk up from the file to find the nearest project doc tracer does not
/// deliver: `DOC_FILENAMES`, and with no session `DELIVERED_DOCS` before
/// them. Any match in a directory wins, and list order is the tiebreak.
///
/// The doc is named relative to `repo_root` where it sits: its directory is
/// resolved, its own name is not, so a `Claude.md` symlinked into a
/// directory is named in that directory, not at the file it points to.
pub fn nearest_doc(path: &Path, repo_root: &Path) -> Option<String> {
    let delivered: &[&str] = if crate::session::active() { &[] } else { DELIVERED_DOCS };
    let abs = crate::cache::absolutize(path);
    let mut current = if abs.is_file() {
        abs.parent().map(|p| p.to_path_buf())
    } else {
        Some(abs.clone())
    };
    while let Some(dir) = current {
        if dir == Path::new("/") {
            // The filesystem root itself is not probed.
            break;
        }
        for name in delivered.iter().chain(DOC_FILENAMES) {
            let candidate = dir.join(name);
            if candidate.exists() {
                let directory = crate::cache::relative_to_root(&dir, repo_root);
                return Some(if directory.is_empty() {
                    (*name).to_string()
                } else {
                    format!("{directory}/{name}")
                });
            }
        }
        current = dir.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Up to `limit` files that import `relative_file`, each with the first line
/// of its leading comment so the reader sees why it depends on this.
pub fn top_callers(
    index: &relations::Relations,
    relative_file: &str,
    repo_root: Option<&Path>,
    limit: usize,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut importers: Vec<&str> = index
        .resolved_importers_of(relative_file)
        .map(|importer| &*importer.file)
        .collect();
    importers.sort_by(|a, b| {
        index
            .resolved_importers_of(b)
            .count()
            .cmp(&index.resolved_importers_of(a).count())
            .then_with(|| a.cmp(b))
    });
    importers.dedup();
    importers.truncate(limit);
    let facts = repo_root.map_or_else(Default::default, |root| {
        let paths: Vec<std::path::PathBuf> = importers.iter().map(|importer| root.join(importer)).collect();
        crate::file_facts::get_batch(&paths, root)
    });
    for importer in importers {
        let language = index.language(importer);
        let purpose = facts
            .get(importer)
            .and_then(|facts| facts.leading_comment.as_deref())
            .and_then(|comment| comment.lines().next());
        out.push(json!({
            "source_file": importer,
            "source_line": Value::Null,
            "label": relations::file_to_module(importer, language),
            "kind": "module",
            "purpose": purpose,
        }));
    }
    out
}

/// Immediate (one-hop) dependencies of a file. The stored map is the
/// inversion, so this reads the edges whose importer is this file.
pub fn immediate_dependencies(
    index: &relations::Relations,
    relative_file: &str,
    limit: usize,
) -> Vec<Value> {
    let mut dependencies: BTreeMap<&str, &str> = BTreeMap::new();
    for (target, importer) in index.import_edges() {
        if &*importer.file == relative_file {
            dependencies
                .entry(target)
                .and_modify(|confidence| {
                    if relations::confidence_code(importer.confidence)
                        < relations::confidence_code(confidence)
                    {
                        *confidence = importer.confidence;
                    }
                })
                .or_insert(importer.confidence);
        }
    }
    dependencies
        .into_iter()
        .take(limit)
        .map(|(target, confidence)| {
            let language = index.language(target);
            json!({
                "module": relations::file_to_module(target, language),
                "confidence": confidence,
            })
        })
        .collect()
}
