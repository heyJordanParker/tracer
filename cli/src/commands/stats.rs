//! `trace stats` — complexity distribution under a path, read from the scc
//! snapshot every call shares: per-language LOC/complexity aggregates, a
//! per-file complexity distribution (median/p75/p90/p95/max), and the
//! top-10 most-complex files.

use crate::repo_context::{FileMetrics, LanguageRow, Payload};
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::thread;

struct Distribution {
    median: i64,
    p75: i64,
    p90: i64,
    p95: i64,
    max: i64,
}

/// `int(statistics.median(values))` — average of the two middles for even
/// counts, truncated toward zero. `complexities` is sorted in place.
fn distribution(complexities: &mut [i64]) -> Option<Distribution> {
    if complexities.is_empty() {
        return None;
    }
    complexities.sort_unstable();
    let n = complexities.len();
    let median = if n % 2 == 1 {
        complexities[n / 2]
    } else {
        ((complexities[n / 2 - 1] + complexities[n / 2]) as f64 / 2.0) as i64
    };
    let percentile = |p: f64| -> i64 {
        let index = (((n as f64) * p) as i64 - 1).max(0) as usize;
        complexities[index]
    };
    Some(Distribution {
        median,
        p75: percentile(0.75),
        p90: percentile(0.90),
        p95: percentile(0.95),
        max: complexities[n - 1],
    })
}

/// The ten most complex files, most complex first, then by path.
fn top_complex(payload: &Payload) -> Vec<(&String, &FileMetrics)> {
    let mut files: Vec<(&String, &FileMetrics)> = payload.per_file.iter().collect();
    files.sort_by(|left, right| right.1.ccn.cmp(&left.1.ccn).then_with(|| left.0.cmp(right.0)));
    files.truncate(10);
    files
}

fn directory_scope(path: &Path, repo_root: &Path) -> String {
    let path = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    match path.strip_prefix(repo_root).ok().and_then(Path::to_str) {
        Some("") | None => "./".to_string(),
        Some(relative) => format!("{}/", relative.trim_end_matches('/')),
    }
}

fn annotations(metrics: &crate::relations::DirectoryMetrics) -> Vec<(&str, usize)> {
    let mut names: Vec<(&str, usize)> = metrics
        .annotations
        .iter()
        .map(|(name, count)| (name.as_str(), *count))
        .collect();
    names.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    names
}

fn directory_value(metrics: &crate::relations::DirectoryMetrics) -> Value {
    json!({
        "files": metrics.files,
        "imported_by": metrics.imported_by,
        "imports": metrics.imports,
        "imported_directories": metrics.imported_directories,
        "annotations": metrics.annotations,
    })
}

/// Most depended-on first: a change there reaches the most files.
fn compare_directories(
    left: (&String, &crate::relations::DirectoryMetrics),
    right: (&String, &crate::relations::DirectoryMetrics),
) -> Ordering {
    right.1.imported_by.cmp(&left.1.imported_by).then_with(|| left.0.cmp(right.0))
}

pub fn run(path: &Path, as_json: bool) -> Result<Value> {
    crate::pathval::require_exists(path, "PATH")?;
    let abs = crate::cache::absolutize(path);
    let resolved = abs.canonicalize().unwrap_or(abs);
    let repo_root =
        crate::cache::worktree_root_for(&resolved).unwrap_or_else(|| PathBuf::from("."));
    let (payload, metrics) = thread::scope(|scope| {
        let relations = scope.spawn(|| {
            crate::timing::phase("relations", || {
                crate::relations::get(&repo_root).directory_metrics()
            })
        });
        let payload = crate::timing::phase("scc", || crate::repo_context::metrics_under(&resolved));
        (payload, relations.join().unwrap())
    });
    let payload = payload.map_err(|e| crate::process::fail(1, format!("Error: scc failed: {e:#}")))?;
    let (distribution, top_complex) = crate::timing::phase("summary", || {
        (distribution(&mut payload.per_file.values().map(|file| file.ccn).collect::<Vec<i64>>()), top_complex(&payload))
    });
    let scope = directory_scope(&resolved, &repo_root);

    if as_json {
        return Ok(crate::timing::phase("document", || {
            let mut languages = Map::new();
            for language in &payload.languages {
                languages.insert(
                    language.name.clone(),
                    json!({
                        "files": language.count,
                        "lines_of_code": language.code,
                        "cyclomatic_complexity": language.complexity,
                    }),
                );
            }
            let distribution = distribution.as_ref().map_or_else(
                || json!({}),
                |distribution| {
                    json!({
                        "median": distribution.median,
                        "p75": distribution.p75,
                        "p90": distribution.p90,
                        "p95": distribution.p95,
                        "max": distribution.max,
                    })
                },
            );
            let top_complex = top_complex
                .iter()
                .map(|(path, file)| {
                    json!({
                        "path": path,
                        "language": file.language,
                        "lines": file.lines,
                        "cyclomatic_complexity": file.ccn,
                    })
                })
                .collect::<Vec<_>>();
            let mut directories = Map::new();
            for (directory, metrics) in metrics
                .directories
                .iter()
                .filter(|(directory, _)| scope == "./" || directory.starts_with(scope.as_str()))
            {
                directories.insert(directory.clone(), directory_value(metrics));
            }
            crate::output::document(
                json!({"path": resolved.to_string_lossy()}),
                json!({"distribution": distribution, "directories": directories}),
                json!({"languages": languages, "top_complex": top_complex}),
                json!({
                    "files": payload.per_file.len(),
                    "directories": metrics
                        .directories
                        .keys()
                        .filter(|directory| scope == "./" || directory.starts_with(scope.as_str()))
                        .count(),
                }),
            )
        }));
    }

    crate::timing::phase("render", || {
        let mut head = format!("Files: {}\n\nLanguages:\n", payload.per_file.len());
        let mut languages: Vec<&LanguageRow> = payload.languages.iter().collect();
        languages.sort_by_key(|language| std::cmp::Reverse(language.code));
        for language in languages.iter().take(15) {
            let facts = json!({
                "files": language.count,
                "lines_of_code": language.code,
                "cyclomatic_complexity": language.complexity,
            });
            head.push_str(&format!("  {:<20} {}\n", language.name, crate::yamlfmt::flow(&facts, false)));
        }
        head.push('\n');
        if let Some(distribution) = &distribution {
            head.push_str(&format!(
                "Complexity distribution (per file):\n  median={}  p75={}  p90={}  p95={}  max={}\n",
                distribution.median,
                distribution.p75,
                distribution.p90,
                distribution.p95,
                distribution.max,
            ));
        }
        head.push_str("\nTop 10 most-complex files:\n");
        for (path, file) in &top_complex {
            let facts = json!({"cyclomatic_complexity": file.ccn, "lines": file.lines});
            head.push_str(&format!("  {path}  {}\n", crate::yamlfmt::flow(&facts, false)));
        }
        head.push_str(&format!("\nDirectories under {scope} (most imported first, direct files only):\n"));
        let columns = "  imported_by  imports  files  directory                        annotations\n";
        let mut rows: Vec<(&String, &crate::relations::DirectoryMetrics)> = metrics
            .directories
            .iter()
            .filter(|(directory, _)| scope == "./" || directory.starts_with(scope.as_str()))
            .collect();
        rows.sort_by(|left, right| compare_directories(*left, *right));
        let mut entries: Vec<crate::output::Entry> = Vec::with_capacity(rows.len());
        let mut paths: Vec<&str> = Vec::with_capacity(rows.len());
        for (directory, metrics) in rows {
            let annotations = annotations(metrics)
                .into_iter()
                .take(3)
                .map(|(name, count)| format!("{name}×{count}"))
                .collect::<Vec<_>>()
                .join(" ");
            let line = format!(
                "  {:<12} {:<8} {:<6} {:<32} {}",
                metrics.imported_by,
                metrics.imports,
                metrics.files,
                directory,
                if annotations.is_empty() {
                    "-"
                } else {
                    &annotations
                },
            );
            let lead = if entries.is_empty() { columns } else { "" };
            entries.push(crate::output::Entry {
                rank: metrics.imported_by as i64,
                levels: vec![format!("{lead}{line}"), format!("{lead}  {directory}")],
            });
            paths.push(directory);
        }
        let fixed = head.len() + crate::output::closing_room(entries.len(), "directories");
        let crate::output::Fitted { texts, shortened, .. } = crate::output::fit_listing(&entries, &paths, fixed);
        for text in texts {
            head.push_str(&format!("{text}\n"));
        }
        if shortened > 0 {
            head.push_str(&format!("{}\n", crate::output::shortened_line(shortened, entries.len(), "directories")));
        }
        super::docs::print_with_docs(&head, std::slice::from_ref(&resolved), "trace_stats", crate::session::Delivery::default())
    })?;
    Ok(Value::Null)
}
