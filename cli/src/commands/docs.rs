//! `trace docs` — project-docs surface: a `--graph` flag off the noun plus
//! the `status`, `reset` and `prime` sub-verbs. Delivering docs is
//! `crate::docs`, which every command shares.
//!
//! - default (no flag, no sub-verb): the project docs for the paths that the
//!   agent does not hold whole yet. Walks each path's ancestor chain, nearest
//!   doc first, and partitions it against the agent's record. As text, each
//!   doc is a Markdown section (`## <path>` then its text), sent from its first
//!   unread line; the first doc that does not fit `--budget` is cut at a whole
//!   line and ends in `read`'s trim marker, and the docs after it are queued
//!   by name. A doc counts as loaded only once every line arrived. Nothing
//!   prints when every doc is already loaded. As JSON the new docs arrive
//!   whole, with the skipped slice in `context.already_loaded`.
//!
//!   `--source` names the delivering surface in the record, `trace_docs` by
//!   default.
//!
//! - `--graph`: the whole-repo docs graph (doc-file nodes + `@include`
//!   edges), built in memory per call, plus the "available but not loaded"
//!   set computed against the agent's record when a session is active.
//! - `status`: the agent-facing "what do I have right now?" query. With no
//!   path argument returns the full session manifest (every loaded doc with
//!   source attribution). With a path argument returns that path's ancestor
//!   chain partitioned into `loaded` (with source) and `not_loaded`.
//! - `reset`: clears the agent's record so a subsequent `trace docs <path>`
//!   re-surfaces docs as new. Driven by the compaction and clear hooks: a
//!   context reset drops injected rule text from the model, so the record
//!   must reset to re-inject it.

use crate::docs::{fit, resolve_target, shell_command, unread};
use crate::{cache, docs_graph, nested_memory, session};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Path-mode, the default `trace docs <paths>`. Each path's chain is walked
/// with no dedupe, then partitioned against the live log: new entries land in
/// `docs[]` (with content); already-loaded entries land in `already_loaded[]`
/// (without content, with per-entry source). `skip` names the docs the
/// triggering command delivers itself. Only what was sent gets recorded, and
/// `record` is false under `--filter`, whose projection may drop the docs.
pub fn run(
    targets_raw: &[PathBuf],
    skip: &[PathBuf],
    directory_mode: bool,
    source: &str,
    as_json: bool,
    record: bool,
    sink: &crate::output::Sink<'_>,
) -> Result<()> {
    // One delivery at a time per agent, from reading what it holds to
    // recording what was sent, so calls side by side send the next lines.
    let _delivering = session::hold();

    let pre_loaded: BTreeSet<String> = session::loaded_paths();
    let skip: BTreeSet<String> = skip
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .map(|path| path.to_string_lossy().to_string())
        .collect();

    let mut displays: Vec<String> = Vec::new();
    let mut scoped = false;
    let mut walked: BTreeSet<String> = BTreeSet::new();
    let mut new_docs: Vec<nested_memory::LoadedMemory> = Vec::new();
    let mut skipped: Vec<nested_memory::LoadedMemory> = Vec::new();
    for target_raw in targets_raw {
        let (target, repo_root, scope_dir) = resolve_target(target_raw, directory_mode)?;
        displays.push(cache::relative_to_root(&target, &repo_root));
        scoped |= scope_dir;
        let mut empty_dedupe: BTreeSet<String> = BTreeSet::new();
        for memory in nested_memory::load_for_file(&target, &repo_root, &mut empty_dedupe, scope_dir) {
            if skip.contains(&memory.path) || !walked.insert(memory.path.clone()) {
                continue;
            }
            if pre_loaded.contains(&memory.path) {
                skipped.push(memory);
            } else {
                new_docs.push(memory);
            }
        }
    }

    // Text sends each doc from its first unread line, nearest first, and
    // records exactly the lines that printed. JSON sends every new doc whole
    // and records them all. Each records only once its output is flushed.
    if !as_json {
        let again = shell_command(&format!("{} docs", crate::output::trace_command()), targets_raw);
        let fitted = fit(unread(new_docs), crate::output::budget(), &again);
        print!("{}", fitted.text);
        std::io::stdout().flush()?;
        let mut delivery = session::Delivery::default();
        fitted.record(&mut delivery, source);
        delivery.commit();
        return Ok(());
    }

    let prior_sources: BTreeMap<String, String> = source_map(&session::loaded_entries());
    let already_loaded: Vec<Value> = skipped
        .iter()
        .map(|m| {
            json!({
                "path": m.relative_path,
                "kind": m.kind,
                "size": m.size,
                "large": m.large,
                "source": prior_sources
                    .get(&m.path)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string()),
            })
        })
        .collect();

    sink.emit(&crate::output::document(
        json!({
            "path": displays.first(),
            "paths": displays,
            "directory_scoped": scoped,
            "source": source,
        }),
        json!({"already_loaded": already_loaded.clone()}),
        json!(new_docs
            .iter()
            .map(|m| json!({
                "path": m.relative_path,
                "kind": m.kind,
                "size": m.size,
                "large": m.large,
                "content": m.content,
            }))
            .collect::<Vec<_>>()),
        json!({"docs": new_docs.len(), "skipped": already_loaded.len()}),
    ))?;
    if record {
        session::record_emission(&new_docs, source);
    }
    Ok(())
}

/// Status-mode: the agent-facing "what do I have right now?" query.
///
/// With `target_raw` Some: returns the ancestor chain for that path
/// partitioned into `loaded` (with per-entry source) and `not_loaded`. The
/// chain is the same one path-mode would walk; the partition is computed
/// against the agent's record so the agent can immediately
/// tell whether a path's rules are in context.
///
/// With `target_raw` None: returns the full session manifest — every doc
/// the log has surfaced so far, with source attribution.
///
/// Pure read. Never records, never mutates the log.
pub fn run_status(target_raw: Option<&Path>, as_json: bool) -> Result<Value> {
    let loaded_entries = session::loaded_entries();
    let session_active = session::active();

    match target_raw {
        Some(path) => run_status_path(path, &loaded_entries, session_active, as_json),
        None => run_status_session(&loaded_entries, session_active, as_json),
    }
}

fn run_status_session(
    loaded_entries: &[session::LoadedEntry],
    session_active: bool,
    as_json: bool,
) -> Result<Value> {
    let loaded_json: Vec<Value> = loaded_entries
        .iter()
        .map(|e| {
            json!({
                "path": e.visible_as,
                "source": e.source,
                "kind": e.kind,
                "size": e.size,
                "content_hash": e.content_hash,
                "total_lines": e.total_lines,
                "lines_read": e.lines_read,
                "read_fraction": e.read_fraction,
            })
        })
        .collect();

    let by_source = group_by_source(loaded_entries);
    let out = crate::output::document(
        json!({"scope": "session"}),
        json!({"session_active": session_active, "by_source": by_source}),
        // Named the same way path-mode names its rows, so `loaded` reads as
        // the same thing in both modes of one command.
        json!({"loaded": loaded_json}),
        json!({"loaded": loaded_entries.len()}),
    );

    if as_json {
        return Ok(out);
    }
    print_status_session_human(loaded_entries, session_active, &by_source);
    Ok(out)
}

fn run_status_path(
    target_raw: &Path,
    loaded_entries: &[session::LoadedEntry],
    session_active: bool,
    as_json: bool,
) -> Result<Value> {
    let (target, repo_root, scope_dir) = resolve_target(target_raw, false)?;

    // Walk-up with NO dedupe — every doc reachable for this path. We don't
    // record anything; status is a pure read.
    let mut empty_dedupe: BTreeSet<String> = BTreeSet::new();
    let chain = nested_memory::load_for_file(&target, &repo_root, &mut empty_dedupe, scope_dir);

    // Source map for partition attribution. Same shape as path-mode uses for
    // already_loaded so consumers see consistent attribution.
    let source_map = source_map(loaded_entries);
    // The same rule the docs path sends by: a doc seen only in part is not
    // loaded, so status never calls a doc loaded that the hook would resend.
    let loaded_set: BTreeSet<String> = session::loaded_paths();

    let (loaded_chain, not_loaded_chain): (Vec<_>, Vec<_>) =
        chain.iter().partition(|m| loaded_set.contains(&m.path));

    let loaded_json: Vec<Value> = loaded_chain
        .iter()
        .map(|m| {
            json!({
                "path": m.relative_path,
                "kind": m.kind,
                "size": m.size,
                "source": source_map.get(&m.path).cloned().unwrap_or_else(|| "unknown".to_string()),
            })
        })
        .collect();
    let not_loaded_json: Vec<Value> = not_loaded_chain
        .iter()
        .map(|m| {
            json!({
                "path": m.relative_path,
                "kind": m.kind,
                "size": m.size,
            })
        })
        .collect();

    let display = cache::relative_to_root(&target, &repo_root);
    let out = crate::output::document(
        json!({"scope": "path", "path": display}),
        json!({"session_active": session_active}),
        json!({"loaded": loaded_json, "not_loaded": not_loaded_json}),
        json!({
            "loaded": loaded_chain.len(),
            "not_loaded": not_loaded_chain.len(),
            "chain": chain.len(),
        }),
    );

    if as_json {
        return Ok(out);
    }
    print_status_path_human(&display, &loaded_chain, &not_loaded_chain, &source_map);
    Ok(out)
}

/// Reset-mode: clear the agent's record so the next `trace docs <path>`
/// re-surfaces docs as new. A clean no-op when no session is active or
/// nothing was surfaced.
pub fn run_reset(source: &str, as_json: bool) -> Result<Value> {
    let session_active = session::active();
    let cleared = session::reset();

    let out = crate::output::document(
        json!({"scope": "reset", "source": source}),
        json!({"session_active": session_active}),
        Value::Array(vec![]),
        json!({"cleared": cleared}),
    );

    if as_json {
        return Ok(out);
    }
    if !session_active {
        println!("# docs reset · no active session (nothing to clear)");
    } else {
        println!("# docs reset · cleared {cleared} file(s) from the agent's record");
    }
    Ok(out)
}

/// Graph-mode: whole-repo docs graph + the available-but-not-loaded slice.
/// Invoked via the `--graph` flag on `trace docs`. The graph is built in
/// memory from the doc files themselves and cached nowhere, so it is always
/// current with the bytes on disk.
pub fn run_graph(path: Option<&Path>, as_json: bool) -> Result<Value> {
    let here = Path::new(".");
    let resolve_root = |p: &Path| -> PathBuf {
        cache::worktree_root_for(p).unwrap_or_else(|| cache::display_root(p))
    };
    let repo_root = match path {
        Some(p) => resolve_root(p),
        None => resolve_root(here),
    };
    // The doc graph is built directly. It used to be unpacked out of the
    // architecture entry, which meant `trace docs --graph` resolved every
    // code relationship in the repository to read a doc-tree walk.
    let docs = docs_graph::build(&repo_root);
    let graph_json = docs.to_json();

    // Diff against the agent's record to surface "available but
    // not loaded" — the agent-facing report. The record keys by
    // canonical absolute path, so re-canonicalize each node path under the
    // repo root to compare.
    let loaded: BTreeSet<String> = session::loaded_paths();
    let mut not_loaded: Vec<String> = Vec::new();
    for n in &docs.nodes {
        let abs = repo_root.join(&n.path);
        let canonical = abs
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| abs.to_string_lossy().to_string());
        if !loaded.contains(&canonical) {
            not_loaded.push(n.path.clone());
        }
    }

    let out = crate::output::document(
        json!({"scope": "graph", "path": repo_root.to_string_lossy()}),
        json!({"available_not_loaded": not_loaded}),
        graph_json,
        json!({"nodes": docs.nodes.len(), "edges": docs.edges.len()}),
    );

    if as_json {
        return Ok(out);
    }
    print_graph_human(&docs, &not_loaded);
    Ok(out)
}

// ---------- shared helpers ----------

/// path -> source map, built from the loaded entries, so `status` and
/// path-mode report the same source for any given path.
fn source_map(entries: &[session::LoadedEntry]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|e| (e.path.clone(), e.source.clone()))
        .collect()
}

/// source -> count breakdown of the session manifest. Stable sorted by
/// source string so the human-readable form is deterministic.
fn group_by_source(entries: &[session::LoadedEntry]) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for e in entries {
        *out.entry(e.source.clone()).or_insert(0) += 1;
    }
    out
}

fn print_status_session_human(
    entries: &[session::LoadedEntry],
    session_active: bool,
    by_source: &BTreeMap<String, usize>,
) {
    if !session_active {
        println!("# docs status · no active session (log is empty)");
        return;
    }
    println!(
        "# docs status · session manifest · {} loaded",
        entries.len()
    );
    if entries.is_empty() {
        println!("  (no docs loaded in this session yet)");
        return;
    }
    if !by_source.is_empty() {
        let items: Vec<String> = by_source.iter().map(|(s, n)| format!("{s}: {n}")).collect();
        println!("  by source: {}", items.join(", "));
    }
    for e in entries {
        // Read coverage is meaningful only for files the agent actually read
        // (total_lines > 0); a doc-injection-only entry omits it.
        let coverage = if e.total_lines > 0 {
            format!(
                ", read: {}/{} lines ({:.0}%)",
                e.lines_read,
                e.total_lines,
                e.read_fraction * 100.0
            )
        } else {
            String::new()
        };
        println!(
            "  · {}  (source: {}, kind: {}, {} chars{})",
            e.visible_as, e.source, e.kind, e.size, coverage
        );
    }
}

fn print_status_path_human(
    display: &str,
    loaded: &[&nested_memory::LoadedMemory],
    not_loaded: &[&nested_memory::LoadedMemory],
    source_map: &BTreeMap<String, String>,
) {
    println!(
        "# docs status · {display} · loaded {} · not_loaded {}",
        loaded.len(),
        not_loaded.len()
    );
    if !loaded.is_empty() {
        println!("## in context");
        for m in loaded {
            let source = source_map
                .get(&m.path)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            println!(
                "  · {}  (source: {source}, kind: {}, {} chars)",
                m.relative_path, m.kind, m.size
            );
        }
    }
    if !not_loaded.is_empty() {
        println!("## not loaded");
        for m in not_loaded {
            println!(
                "  · {}  (kind: {}, {} chars)",
                m.relative_path, m.kind, m.size
            );
        }
    }
}

fn print_graph_human(graph: &docs_graph::DocsGraph, not_loaded: &[String]) {
    println!(
        "# docs graph: {} nodes, {} edges (head={}, available-not-loaded={})",
        graph.nodes.len(),
        graph.edges.len(),
        graph.head,
        not_loaded.len()
    );
    for n in &graph.nodes {
        let marker = if not_loaded.contains(&n.path) {
            " [not loaded]"
        } else {
            ""
        };
        println!("  · {} ({}, {} chars){}", n.path, n.kind, n.size, marker);
    }
    if !graph.edges.is_empty() {
        println!();
        println!("## edges");
        for e in &graph.edges {
            println!("  {} --{}--> {}", e.source, e.relation, e.target);
        }
    }
}
