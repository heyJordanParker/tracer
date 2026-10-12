//! `trace context` — two modes off one command.
//!
//! No-args: the session-start primer, which `crate::primer` owns.
//! File-arg: single-file enrichment — the file's facts, rows, and `calls:`
//! block.

use crate::session::{self, Delivery, Kind, Offer};
use crate::summary::Facts;
use crate::{cache, file_facts, git_activity, relations, summary, repo_files, surface};
use anyhow::{bail, Result};
use serde_json::{Map, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};

// --- File-enrichment helper --------------------------------------------

/// Translate the read tool's `offset`/`limit` into the 1-based inclusive
/// `(start, end)` span `record_read` accumulates, or `None` for a whole-file
/// read (no offset, no limit — the shell-read path). `offset` defaults to line
/// 1; an open-ended `limit` reaches end-of-file (`record_read` clamps the span
/// to the file's real line count).
pub(crate) fn read_range(offset: Option<usize>, limit: Option<usize>) -> Option<(usize, usize)> {
    match (offset, limit) {
        (None, None) => None,
        (Some(o), Some(l)) => Some((o, o.saturating_add(l).saturating_sub(1))),
        (Some(o), None) => Some((o, usize::MAX)),
        (None, Some(l)) => Some((1, l)),
    }
}

type FileContext = (String, Option<Map<String, Value>>, Vec<surface::Row>, Delivery);

/// The context `trace hook` adds to a native Read of `lines` of `p`, once
/// per agent context, with the delivery that records the lines as `seen`.
pub(crate) fn read_context(p: &Path, lines: Option<(usize, usize)>, seen: bool) -> Result<(String, Delivery)> {
    let (text, _, _, shown) = file_mode(p, lines, seen, crate::output::budget(), true)?;
    Ok((text, shown))
}

/// The context `trace hook` adds after the agent's own edit of `p`, whose
/// `changed` line ranges it wrote: its rows are recorded as shown, since the
/// agent wrote them, its facts print only where they changed, and the
/// callers of each changed function follow, so it can update them. Empty
/// outside a repository.
pub(crate) fn edit_context(p: &Path, changed: &[(i64, i64)], delivery: &mut Delivery) -> String {
    let Some(repo_root) = cache::worktree_root_for(p) else {
        return String::new();
    };
    let Some(facts) = file_facts::get(p, &repo_root) else {
        return String::new();
    };
    let relative = cache::relative_to_root(p, &repo_root);
    let _ = surface::delivered(&surface::rows(&facts, None), &relative, None, None, p, delivery);
    let shown = Facts::of(&facts, relations::ready(&repo_root).and_then(|index| index.module_counts(&relative)).as_ref());
    let mut out = summary::offer_facts(p, &shown, delivery);
    let mut callers = crate::calls::callers_of(&facts, &relative, changed, &repo_root);
    let room = crate::output::budget().map(|budget| budget.saturating_sub(out.len()));
    out.push_str(&crate::calls::render_calls("callers", &mut callers, &relative, room, Some(delivery)));
    match out.is_empty() {
        true => out,
        false => format!("# {relative}\n{out}"),
    }
}

fn file_mode(
    p: &Path,
    lines: Option<(usize, usize)>,
    record: bool,
    budget: Option<usize>,
    once: bool,
) -> Result<FileContext> {
    crate::timing::phase("render", || file_mode_inner(p, lines, record, budget, once))
}

/// A file's context: its facts as YAML front matter — with the docs governing
/// it that the session has not loaded, and its directory — then its rows.
/// `once` prints the front matter once per Agent context, the way text output
/// does. The returned delivery holds what to record, the caller commits it
/// once the text is flushed; the returned facts stay whole.
fn file_mode_inner(
    p: &Path,
    lines: Option<(usize, usize)>,
    record: bool,
    budget: Option<usize>,
    once: bool,
) -> Result<FileContext> {
    // The agent's Read tool just showed it these lines, so they are seen.
    // `record` is false for an Edit/Write: the agent gets the full summary
    // but the touch is not a read, so read coverage stays a function of
    // genuine reads alone.
    let mut shown = Delivery::default();
    let read_failed = match std::fs::read(p) {
        Ok(content) if record => {
            let content = String::from_utf8_lossy(&content).into_owned();
            let hash = session::content_hash(&content);
            shown.seen(p, "agent_read", &hash, content.len(), content.lines().count(), &[lines]);
            false
        }
        Ok(_) => false,
        Err(_) => true,
    };

    let window = lines.map(|(start, end)| (start as i64, end as i64));
    let rows_for = |facts: &file_facts::FileFacts| surface::rows(facts, window);
    // Outside any git repository there is no history and no import graph, so
    // there are no facts to show — only the rows.
    let Some(repo_root) = cache::worktree_root_for(p) else {
        let rows = file_facts::get(p, &cache::display_root(p))
            .as_ref()
            .map(rows_for)
            .unwrap_or_default();
        let file = p.to_string_lossy();
        let text = match once {
            true => surface::delivered(&rows, &file, window, budget, p, &mut shown),
            false => surface::render_within(&rows, &file, window, budget),
        };
        return Ok((text, None, rows, shown));
    };
    let relative = cache::relative_to_root(p, &repo_root);
    let facts = file_facts::get(p, &repo_root);
    let rows = facts.as_ref().map(rows_for).unwrap_or_default();
    let shown_facts = facts
        .as_ref()
        .map(|facts| Facts::of(facts, relations::ready(&repo_root).and_then(|index| index.module_counts(&relative)).as_ref()));
    let mut map = match &shown_facts {
        Some(shown_facts) => shown_facts.to_map(),
        None => {
            let mut map = Map::new();
            map.insert("file".into(), relative.clone().into());
            if read_failed {
                if let Some(activity) = git_activity::bulk_cached(&repo_root).get(&relative) {
                    map.insert("git".into(), Value::Object(summary::activity_git(activity)));
                }
            }
            map
        }
    };
    let mut calls = match (window, facts.as_ref()) {
        (Some(window), Some(facts)) => crate::calls::calls(facts, &relative, window, &repo_root),
        _ => Vec::new(),
    };
    if let Some(directory) = p.parent().and_then(|directory| directory_facts(directory, &mut shown, once)) {
        map.insert("directory".into(), Value::Object(directory));
    }
    // The rows fit in what the front matter leaves, each still named.
    let mut out = summary::front_matter_once(None, p, shown_facts.as_ref(), &map, &mut shown, once);
    let rows_budget = budget.map(|budget| budget.saturating_sub(out.len()));
    out.push_str(&match once {
        true => surface::delivered(&rows, &relative, window, rows_budget, p, &mut shown),
        false => surface::render_within(&rows, &relative, window, rows_budget),
    });
    let calls_budget = budget.map(|budget| budget.saturating_sub(out.len()));
    out.push_str(&crate::calls::render_calls("calls", &mut calls, &relative, calls_budget, once.then_some(&mut shown)));
    if !calls.is_empty() {
        map.insert("calls".into(), serde_json::json!(calls));
    }
    Ok((out, Some(map), rows, shown))
}

/// Most entries a directory's facts list before naming the total instead.
const DIRECTORY_ENTRY_LIMIT: usize = 40;

/// A directory's facts: its path, how many files outside it import its files
/// and how many it imports, the annotations its files carry, and its entries
/// one level deep — sub-directories first, each suffixed `/`, the ones git
/// ignores left out. `once` passes the block through the `Listing` gate, the
/// way a file's front matter carries its directory: only the lines that
/// changed since this Agent was last shown it, and the whole block with its
/// entries when it is new to the Agent or its entries changed; a directory
/// asked for by name always prints whole. `None` when the directory cannot
/// be read or is empty, or when the Agent was shown it as it is. The caller
/// commits `delivery` once the block is flushed.
pub(crate) fn directory_facts(directory: &Path, delivery: &mut Delivery, once: bool) -> Option<Map<String, Value>> {
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let entries = std::fs::read_dir(directory).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), ".git" | ".tracer-cache") {
            continue;
        }
        if entry.path().is_dir() {
            directories.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }
    directories.sort();
    files.sort();
    let mut entries = directories;
    entries.extend(files);
    if entries.is_empty() {
        return None;
    }
    let mut map = Map::new();
    let repo_root = cache::worktree_root_for(directory);
    let key = match &repo_root {
        Some(root) => match cache::relative_to_root(directory, root).as_str() {
            "" | "." => "./".to_string(),
            relative => format!("{}/", relative.trim_end_matches('/')),
        },
        None => format!("{}/", directory.to_string_lossy().trim_end_matches('/')),
    };
    let metrics = repo_root
        .as_ref()
        .and_then(|root| relations::ready(root)?.directory_metrics_for(&key));
    // The first time a session is shown a directory is its baseline; later
    // looks name the imports it gained or lost since.
    let since = repo_root
        .as_ref()
        .zip(metrics.as_ref())
        .and_then(|(root, metrics)| session::at_session_start(root, &key, metrics));
    map.insert("path".into(), key.clone().into());
    if let Some(metrics) = metrics {
        map.insert("imported_by".into(), metrics.imported_by.into());
        map.insert("imports".into(), metrics.imports.into());
        if let Some(since) = since {
            map.insert("at_session_start".into(), Value::Object(since));
        }
        let mut annotations: Vec<(String, usize)> = metrics.annotations.into_iter().collect();
        annotations.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        if !annotations.is_empty() {
            let annotations: Map<String, Value> =
                annotations.into_iter().map(|(name, count)| (name, count.into())).collect();
            map.insert("annotations".into(), Value::Object(annotations));
        }
    }
    // Whether the entries changed is judged by what is on disk, so the
    // repository listing is consulted only when the entries print.
    if once {
        let seen_at = directory.canonicalize().unwrap_or_else(|_| directory.to_path_buf());
        let mut block = map.clone();
        block.insert("entries".into(), session::content_hash(&entries.join("\n")).into());
        let printed = serde_json::json!({ "directory": block });
        match delivery.offer(Kind::Listing, &seen_at.to_string_lossy(), &printed) {
            Offer::Same => return None,
            Offer::Changed(mut lines) => match lines.remove("directory") {
                Some(Value::Object(changed)) if !changed.contains_key("entries") => return Some(changed),
                _ => {}
            },
            Offer::New => {}
        }
    }
    // A directory git ignores whole keeps every entry: the Agent is working
    // inside it.
    if let Some(unignored) = repo_root
        .as_ref()
        .and_then(|root| repo_files::unignored(root, key_prefix(&key), &entries))
        .filter(|unignored| !unignored.is_empty())
    {
        entries = unignored;
    }
    let total = entries.len();
    entries.truncate(DIRECTORY_ENTRY_LIMIT);
    map.insert("entries".into(), entries.into());
    if total > DIRECTORY_ENTRY_LIMIT {
        map.insert("total_entries".into(), total.into());
    }
    Some(map)
}

/// A directory key as the prefix its repository-relative paths share: empty
/// for the root.
fn key_prefix(key: &str) -> &str {
    if key == "./" {
        ""
    } else {
        key
    }
}

// --- Primer mode --------------------------------------------------------

pub fn run(
    paths: &[PathBuf],
    force_directory: bool,
    offset: Option<usize>,
    limit: Option<usize>,
    record: bool,
    json: bool,
) -> Result<Value> {
    if json && paths.is_empty() {
        bail!("context --json requires at least one path");
    }
    if paths.len() > 1 && record {
        bail!("multiple paths require --no-record");
    }
    if paths.len() > 1 && (force_directory || offset.is_some() || limit.is_some()) {
        bail!("multiple paths cannot be combined with --directory, --offset, or --limit");
    }
    match paths {
        [] => {
            if !force_directory {
                crate::primer::run()?;
            }
            Ok(crate::output::document(
                serde_json::json!({"paths": []}),
                serde_json::json!({}),
                serde_json::json!([]),
                serde_json::json!({"files": 0, "unavailable": 0}),
            ))
        }
        _ => {
            let mut rows = Vec::with_capacity(paths.len());
            let mut files = serde_json::Map::new();
            let mut unavailable = 0usize;
            for requested in paths {
                let p = cache::absolutize(requested);
                // A directory's context is its facts under `directory`, the
                // same mapping a file's front matter nests.
                let directory_context = |directory: &Path| {
                    let mut map = Map::new();
                    if let Some(facts) = directory_facts(directory, &mut Delivery::default(), false) {
                        map.insert("directory".into(), Value::Object(facts));
                    }
                    let content =
                        if map.is_empty() { String::new() } else { summary::front_matter(&map) };
                    (content, Some(map).filter(|map| !map.is_empty()))
                };
                let mut shown = Delivery::default();
                let (content, error, facts, surface_rows) = if !p.exists() {
                    unavailable += 1;
                    crate::pathval::report(requested, "PATHS", "does not exist");
                    // One missing path still shows what its directory holds,
                    // so the agent sees the names it may have meant.
                    let (content, facts) = match p.parent() {
                        Some(parent) if paths.len() == 1 && !record => directory_context(parent),
                        _ => (String::new(), None),
                    };
                    (
                        content,
                        Some(format!("path is unavailable: {}", requested.display())),
                        facts,
                        Vec::new(),
                    )
                } else if force_directory || p.is_dir() {
                    let (content, facts) = directory_context(&p);
                    (content, None, facts, Vec::new())
                } else {
                    let budget = crate::output::budget().map(|budget| budget / paths.len());
                    match file_mode(&p, read_range(offset, limit), record, budget, !json) {
                        Ok((content, facts, surface_rows, delivery)) => {
                            shown = delivery;
                            let error = content
                                .is_empty()
                                .then(|| format!("context is unavailable: {}", requested.display()));
                            unavailable += usize::from(error.is_some());
                            (content, error, facts, surface_rows)
                        }
                        Err(err) => {
                            unavailable += 1;
                            (String::new(), Some(err.to_string()), None, Vec::new())
                        }
                    }
                };
                if !json {
                    if paths.len() == 1 {
                        crate::timing::phase("output", || print!("{content}"));
                    } else {
                        crate::timing::phase("output", || {
                            println!("== {} ==", requested.display())
                        });
                        if let Some(message) = &error {
                            println!("[unavailable: {message}]");
                        } else {
                            print!("{content}");
                            if !content.ends_with('\n') {
                                println!();
                            }
                        }
                    }
                    let room = crate::output::budget().map(|budget| (budget / paths.len()).saturating_sub(content.len()));
                    print!("{}", super::docs::reached(std::slice::from_ref(&p), room, "trace_context", &mut shown));
                    std::io::stdout().flush()?;
                }
                // JSON makes no offers, so its delivery holds only the read.
                shown.commit();
                rows.push(serde_json::json!({
                    "file": requested.to_string_lossy(),
                    "content": content,
                    "error": error,
                }));
                // The front matter's keys, then the rows beside them.
                let mut file = facts.unwrap_or_default();
                file.insert("surface".into(), serde_json::json!(surface_rows));
                files.insert(requested.to_string_lossy().to_string(), Value::Object(file));
            }
            Ok(crate::output::document(
                serde_json::json!({"paths": paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>() }),
                serde_json::json!({"files": files}),
                Value::Array(rows),
                serde_json::json!({"files": paths.len(), "unavailable": unavailable}),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A throwaway git worktree — `git init` makes `cache::worktree_root_for`
    /// resolve to the fixture root, the same precondition the production read
    /// path always runs under. Dropped — and removed from disk — at scope end.
    struct Fixture {
        root: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            // Per-fixture unique suffix: tests run in parallel threads of one
            // process, so the pid is shared and the clock is second-granular
            // — a monotonic counter is what keeps two fixtures from colliding.
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "tracer_ctx_test_{}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| since.as_secs()),
                SEQ.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&root).unwrap();
            let output = crate::git_activity::git_output(&root, ["init", "-q"]).expect("git init");
            assert!(output.status.success(), "git init failed in {}", root.display());
            Fixture { root }
        }

        fn write(&self, rel: &str, contents: &str) {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, contents).unwrap();
        }

        /// Stage every file so `git ls-files` (the listing's tracked-file
        /// source) sees the tree. Git cannot track an empty directory, so a
        /// sub-directory only surfaces in the listing once it holds a tracked
        /// file — the same as in a real repo.
        fn stage(&self) {
            let output =
                crate::git_activity::git_output(&self.root, ["add", "-A"]).expect("git add");
            assert!(output.status.success(), "git add failed");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    // A directory's entries list its sub-directories, each suffixed `/`,
    // ahead of its files.
    #[test]
    fn directory_entries_list_subdirectories_before_files() {
        let fx = Fixture::new();
        // Git can't track an empty directory; a tracked file inside each
        // sub-directory is what makes it surface — same as a real repo.
        fx.write("sub_one/a.rs", "fn a() {}\n");
        fx.write("sub_two/b.rs", "fn b() {}\n");
        fx.write("readme.md", "x");
        fx.write("other.rs", "fn t() {}\n");
        fx.stage();

        let facts = directory_facts(&fx.root, &mut Delivery::default(), false).expect("directory facts");

        assert_eq!(facts["path"], "./");
        assert_eq!(
            facts["entries"],
            serde_json::json!(["sub_one/", "sub_two/", "other.rs", "readme.md"])
        );
    }
}
