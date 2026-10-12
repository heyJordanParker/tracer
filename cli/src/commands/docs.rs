//! `trace docs` — project-docs surface: a `--graph` flag off the noun plus
//! the `status`, `reset`, `archive` and `prime` sub-verbs.
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
//!   default. `--triggering-tool` and `--triggering-command` are only echoed
//!   back in the JSON query.
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
//! - `archive`: does nothing. Each agent's record is keyed by its own agent
//!   id, so a resumed Subagent finds its record where it left it.

use super::nested_memory;
use crate::session;
use crate::{cache, docs_graph};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Path-mode, the default `trace docs <paths>`. Each path's chain is walked
/// with no dedupe, then partitioned against the live log: new entries land in
/// `docs[]` (with content); already-loaded entries land in `already_loaded[]`
/// (without content, with per-entry source). `skip` names the docs the
/// triggering command delivers itself. Only what was sent gets recorded, and
/// `record` is false under `--filter`, whose projection may drop the docs.
#[allow(clippy::too_many_arguments)]
pub fn run(
    targets_raw: &[PathBuf],
    skip: &[PathBuf],
    directory_mode: bool,
    source: &str,
    triggering_tool: Option<&str>,
    triggering_command: Option<&str>,
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
            "triggering_tool": triggering_tool,
            "triggering_command": triggering_command,
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

/// The most a command's own docs take after its answer.
pub const REACHED_ROOM: usize = 10_000;

/// The project docs `paths` reach that this agent does not hold, nearest
/// first, as a command prints them after its answer: only under a session,
/// fit to `room` and at most `REACHED_ROOM`, the rest named under the
/// `trace docs` command that sends them. What prints is queued in
/// `delivery`, which the caller commits once the text is flushed.
pub fn reached(paths: &[PathBuf], room: Option<usize>, source: &str, delivery: &mut session::Delivery) -> String {
    if !session::active() {
        return String::new();
    }
    crate::timing::phase("docs", || reached_text(paths, room, source, delivery))
}

/// Print a command's `answer` with the docs `anchors` reach after it, in the
/// room the budget leaves, then commit `delivery` once the text is flushed,
/// so a call that dies before its answer arrives records nothing.
pub fn print_with_docs(answer: &str, anchors: &[PathBuf], source: &str, mut delivery: session::Delivery) -> Result<()> {
    let room = crate::output::budget().map(|budget| budget.saturating_sub(crate::output::width(answer)));
    print!("{answer}{}", reached(anchors, room, source, &mut delivery));
    std::io::stdout().flush()?;
    delivery.commit();
    Ok(())
}

fn reached_text(paths: &[PathBuf], room: Option<usize>, source: &str, delivery: &mut session::Delivery) -> String {
    // Held from reading what the agent holds until the delivery commits, so
    // calls side by side never send the same lines.
    delivery.hold();
    let mut held = crate::timing::phase("docs held", session::loaded_paths);
    let mut chain = Vec::new();
    for path in paths {
        if let Ok((target, repo_root, scope_dir)) = resolve_target(path, false) {
            chain.extend(crate::timing::phase("docs walk", || {
                nested_memory::load_for_file(&target, &repo_root, &mut held, scope_dir)
            }));
        }
    }
    let again = shell_command(&format!("{} docs", crate::output::trace_command()), paths);
    match deliver(chain, room, &again, source, delivery) {
        text if text.is_empty() => text,
        text => format!("\n{text}"),
    }
}

/// `docs`, each from its first unread line, fit to `room` and at most
/// `REACHED_ROOM`, the rest named under `again`, queued in `delivery`.
pub fn deliver(
    docs: Vec<nested_memory::LoadedMemory>,
    room: Option<usize>,
    again: &str,
    source: &str,
    delivery: &mut session::Delivery,
) -> String {
    if docs.is_empty() {
        return String::new();
    }
    let room = room.map_or(REACHED_ROOM, |room| room.min(REACHED_ROOM));
    let fitted = fit(unread(docs), Some(room), again);
    fitted.record(delivery, source);
    fitted.text
}

/// A doc the agent does not hold whole, from its first unread line. `start`
/// is its first line below its frontmatter, where the agent's view begins.
#[derive(Clone)]
pub struct Unread {
    pub memory: nested_memory::LoadedMemory,
    name: String,
    pub start: usize,
    pub from: usize,
    pub total: usize,
    /// Set when the agent was sent the doc whole before it changed.
    changed: Option<Changed>,
}

/// What changed in a doc since it was sent whole: its added or changed
/// lines, 1-based, and how many lines were removed.
#[derive(Clone)]
struct Changed {
    lines: Vec<usize>,
    removed: usize,
}

impl Unread {
    fn from_start(&self) -> bool {
        self.from == self.start
    }
}

/// Each doc from the line the agent stopped at, when it read part of the
/// same content before; only its changed lines, when it was sent whole
/// before it changed; from the top of its text otherwise.
pub fn unread(memories: Vec<nested_memory::LoadedMemory>) -> Vec<Unread> {
    let states = session::doc_states();
    memories
        .into_iter()
        .map(|memory| {
            let total = memory.content.lines().count();
            let start = body_start(&memory.content, total);
            let hash = session::content_hash(&memory.content);
            let (from, changed) = match states.get(&memory.path) {
                Some(session::DocState::Partial { hash: read, coverage }) if *read == hash && coverage.total_lines == total => {
                    (coverage.first_unread().clamp(start, total), None)
                }
                Some(session::DocState::Whole { hash: sent, lines }) if *sent != hash => {
                    (start, Some(changed_lines(lines, &memory.content, start)))
                }
                _ => (start, None),
            };
            let name = shown(&memory);
            Unread { memory, name, start, from, total, changed }
        })
        .collect()
}

/// The lines of `content` from `start` on whose hash is not among the lines
/// sent, each sent line matching once, and how many sent lines are gone.
fn changed_lines(sent: &[String], content: &str, start: usize) -> Changed {
    let mut left: HashMap<&str, usize> = HashMap::new();
    for line in sent {
        *left.entry(line.as_str()).or_default() += 1;
    }
    let mut lines = Vec::new();
    for (at, line) in session::line_hashes(content).iter().enumerate() {
        match left.get_mut(line.as_str()).filter(|count| **count > 0) {
            Some(count) => *count -= 1,
            None if at + 1 >= start => lines.push(at + 1),
            None => {}
        }
    }
    Changed { lines, removed: left.values().sum() }
}

/// A doc's path as the agent reads it: relative to the repository it works
/// in, else under `~`, else absolute, so a doc from another checkout never
/// reads as one of its own.
fn shown(memory: &nested_memory::LoadedMemory) -> String {
    let path = Path::new(&memory.path);
    let here = std::env::current_dir()
        .ok()
        .and_then(|cwd| cache::worktree_root_for(&cwd))
        .and_then(|root| root.canonicalize().ok());
    if let Some(relative) = here.as_deref().and_then(|root| path.strip_prefix(root).ok()) {
        return relative.to_string_lossy().to_string();
    }
    let home = nested_memory::home_dir().map(|home| home.canonicalize().unwrap_or(home));
    match home.and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(relative) => format!("~/{}", relative.to_string_lossy()),
        None => memory.path.clone(),
    }
}

/// The first line below a doc's YAML frontmatter and the blank lines after
/// it. Claude Code shows the agent a doc without its frontmatter.
fn body_start(content: &str, total: usize) -> usize {
    let lines: Vec<&str> = content.lines().collect();
    if lines.first().map(|line| line.trim_end()) != Some("---") {
        return 1;
    }
    let Some(close) = lines.iter().skip(1).position(|line| line.trim_end() == "---") else {
        return 1;
    };
    let body = (close + 2..lines.len())
        .find(|&index| !lines[index].trim().is_empty())
        .map(|index| index + 1);
    body.filter(|&line| line <= total).unwrap_or(1)
}

/// What one budget carries: each sent doc with the last whole line it
/// reached, and the printed text, the queued docs' names included.
pub struct Fitted {
    sent: Vec<(Unread, usize)>,
    pub text: String,
}

/// Nearest first. A doc's unread lines go whole while they fit; the first doc
/// that does not fit is cut at a whole line to fill the room and ends in
/// `read`'s trim marker; every doc after it is queued by name with `again`,
/// the command that sends them. Sizes are measured as Claude Code measures.
pub fn fit(docs: Vec<Unread>, budget: Option<usize>, again: &str) -> Fitted {
    let mut pending: std::collections::VecDeque<Unread> = docs.into();
    let mut sent: Vec<(Unread, usize)> = Vec::new();
    let mut sections: Vec<String> = Vec::new();
    let mut used = 0usize;
    while let Some(doc) = pending.pop_front() {
        let whole = section(&doc, doc.total);
        let separator = usize::from(!sections.is_empty());
        let Some(budget) = budget else {
            sections.push(whole);
            sent.push((doc.clone(), doc.total));
            continue;
        };
        let room = budget.saturating_sub(used + separator + queue_width(pending.iter(), again));
        if crate::output::width(&whole) <= room {
            used += separator + crate::output::width(&whole);
            sections.push(whole);
            sent.push((doc.clone(), doc.total));
            continue;
        }
        match cut(&doc, room) {
            Some((through, text)) => {
                used += separator + crate::output::width(&text);
                sections.push(text);
                sent.push((doc, through));
            }
            None => pending.push_front(doc),
        }
        break;
    }
    let queued: Vec<Unread> = pending.into();
    let mut text = sections.join("\n");
    let room = budget.map(|budget| budget.saturating_sub(used));
    text.push_str(&queue_block(&queued, again, room));
    Fitted { sent, text }
}

impl Fitted {
    /// Queue what printed in `delivery`: a doc sent from its first line to
    /// its last as loaded, any other span as the lines seen, so a doc cut
    /// short is offered again from where it stopped.
    pub fn record(&self, delivery: &mut session::Delivery, source: &str) {
        let whole: Vec<nested_memory::LoadedMemory> = self
            .sent
            .iter()
            .filter(|(doc, through)| doc.from_start() && *through == doc.total)
            .map(|(doc, _)| doc.memory.clone())
            .collect();
        delivery.sent(&whole, source);
        for (doc, through) in &self.sent {
            if (doc.from_start() && *through == doc.total) || *through < doc.from {
                continue;
            }
            let first = if doc.from_start() { 1 } else { doc.from };
            delivery.seen(
                Path::new(&doc.memory.path),
                source,
                &session::content_hash(&doc.memory.content),
                doc.memory.content.len(),
                doc.total,
                &[Some((first, *through))],
            );
        }
    }
}

/// Lines `from..=through` of a doc as a Markdown section: the doc whole as
/// `## <path>` when that is all of it, else `## <path> (L<from>-L<through> of
/// <total>)`, ending in the trim marker while lines remain.
fn section(doc: &Unread, through: usize) -> String {
    let lines: Vec<&str> = doc.memory.content.lines().collect();
    if let Some(changed) = &doc.changed {
        let mut out = format!(
            "## {} (changed since sent: {} lines, {} removed)\n\n",
            doc.name,
            changed.lines.len(),
            changed.removed
        );
        for &line in &changed.lines {
            out.push_str(&format!("L{line}: {}\n", lines[line - 1]));
        }
        return out;
    }
    if doc.from_start() && through == doc.total {
        let text = lines[doc.start - 1..].join("\n");
        return format!("## {}\n\n{}\n", doc.name, text.trim());
    }
    let mut out = format!(
        "## {} (L{}-L{through} of {})\n\n{}\n",
        doc.name,
        doc.from,
        doc.total,
        lines[doc.from - 1..through].join("\n")
    );
    if through < doc.total {
        out.push_str(&super::read::trim_marker(
            &runnable(&doc.memory.path),
            "",
            through,
            doc.total,
            doc.total,
        ));
    }
    out
}

/// The doc's unread lines cut to `room`: the whole lines that fit, or, when
/// not even its first unread line fits, as much of that line as does.
/// `None` when the room cannot hold the section's own heading and marker.
fn cut(doc: &Unread, room: usize) -> Option<(usize, String)> {
    // A changed doc's lines are scattered, so it goes whole or queued.
    if doc.changed.is_some() {
        return None;
    }
    let frame = crate::output::width(&section_frame(doc));
    let mut left = room.checked_sub(frame)?;
    let mut through = doc.from - 1;
    for line in doc.memory.content.lines().skip(doc.from - 1) {
        let size = crate::output::width(line) + 1;
        if size > left {
            break;
        }
        left -= size;
        through += 1;
    }
    if through >= doc.from {
        return Some((through, section(doc, through)));
    }
    // A line longer than the whole room is clipped and counted as read, as
    // Claude Code's Read tool clips an over-long line, so the doc moves on.
    let line = doc.memory.content.lines().nth(doc.from - 1)?;
    let shown = crate::output::clip(line, left.checked_sub(2)?);
    if shown.is_empty() {
        return None;
    }
    let mut text = format!(
        "## {} (L{}-L{} of {})\n\n{shown}\u{2026}\n",
        doc.name, doc.from, doc.from, doc.total
    );
    if doc.from < doc.total {
        text.push_str(&super::read::trim_marker(
            &runnable(&doc.memory.path),
            "",
            doc.from,
            doc.total,
            doc.total,
        ));
    }
    Some((doc.from, text))
}

/// The widest heading and marker a cut of this doc prints, with no lines.
fn section_frame(doc: &Unread) -> String {
    format!(
        "## {} (part of L{} of {})\n\n\n{}",
        doc.name,
        doc.total,
        doc.total,
        super::read::trim_marker(&runnable(&doc.memory.path), "", doc.total, doc.total + 1, doc.total)
    )
}

const QUEUED: &str = "Queued, nearest first; this sends them:";

/// The queued docs by name under the command that sends them, as many names
/// as `room` holds and a count for the rest.
fn queue_block(queued: &[Unread], again: &str, room: Option<usize>) -> String {
    if queued.is_empty() {
        return String::new();
    }
    let mut out = format!("\n{QUEUED} {again}\n");
    let mut named = 0;
    for doc in queued {
        let line = queue_line(doc);
        let rest = format!("- {} more\n", queued.len() - named);
        let fits = room.is_none_or(|room| {
            crate::output::width(&out) + crate::output::width(&line) + crate::output::width(&rest) <= room
        });
        if !fits && named + 1 < queued.len() {
            out.push_str(&rest);
            return out;
        }
        out.push_str(&line);
        named += 1;
    }
    out
}

fn queue_line(doc: &Unread) -> String {
    if let Some(changed) = &doc.changed {
        return format!("- {} (changed since sent: {} lines)\n", doc.name, changed.lines.len());
    }
    if !doc.from_start() {
        return format!(
            "- {} (L{}-L{} of {} unread)\n",
            doc.name, doc.from, doc.total, doc.total
        );
    }
    format!("- {} ({} chars)\n", doc.name, doc.memory.size)
}

/// The room the queue of these docs takes when every one is named.
fn queue_width<'a>(queued: impl Iterator<Item = &'a Unread>, again: &str) -> usize {
    let lines: usize = queued.map(|doc| crate::output::width(&queue_line(doc))).sum();
    if lines == 0 {
        return 0;
    }
    crate::output::width(&format!("\n{QUEUED} {again}\n")) + lines
}

/// `path` as a shell word that runs from here: relative to the working
/// directory when under it, else absolute, quoted when it holds whitespace.
fn runnable(path: &str) -> String {
    let path = Path::new(path);
    let shown = std::env::current_dir()
        .ok()
        .and_then(|here| here.canonicalize().ok())
        .and_then(|here| path.strip_prefix(here).ok().map(Path::to_path_buf))
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or_else(|| path.to_path_buf());
    shell_word(&shown.to_string_lossy())
}

fn shell_word(word: &str) -> String {
    if word.contains(char::is_whitespace) || word.contains('\'') {
        format!("'{}'", word.replace('\'', r"'\''"))
    } else {
        word.to_string()
    }
}

/// `command` followed by each path, as it runs from here.
pub fn shell_command(command: &str, paths: &[PathBuf]) -> String {
    let mut words = vec![command.to_string()];
    for path in paths {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        words.push(runnable(&canonical.to_string_lossy()));
    }
    words.join(" ")
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

/// Archive-mode: nothing to move. Each agent's record is keyed by its own
/// agent id, so a resumed Subagent finds its record where it left it, and
/// the 7-day sweep removes old ones.
pub fn run_archive(as_json: bool) -> Result<Value> {
    let out = crate::output::document(
        json!({"scope": "archive"}),
        json!({"session_active": session::active()}),
        Value::Array(vec![]),
        json!({"archived": false}),
    );
    if !as_json {
        println!("# docs archive · each agent's record stays under its own agent id; nothing to archive");
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

fn resolve_target(target_raw: &Path, directory_mode: bool) -> Result<(PathBuf, PathBuf, bool)> {
    let target = target_raw
        .canonicalize()
        .unwrap_or_else(|_| cache::absolutize(target_raw));
    if !target.exists() {
        return Err(crate::process::fail(2, format!("Error: path not found: {}", target_raw.display())));
    }
    let repo_root =
        cache::worktree_root_for(&target).unwrap_or_else(|| cache::display_root(&target));
    let scope_dir = directory_mode || target.is_dir();
    Ok((target, repo_root, scope_dir))
}

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
