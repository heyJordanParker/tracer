//! Project docs as every command and hook delivers them: the docs a path
//! reaches that the agent does not hold whole, each from its first unread
//! line, nearest first, fit to the room the answer left, the rest queued
//! under the `trace docs` command that sends them. `commands::docs` is the
//! `trace docs` command itself, built on this.

use crate::{cache, nested_memory, session};
use anyhow::Result;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

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
        out.push_str(&crate::output::trim_marker(
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
        text.push_str(&crate::output::trim_marker(
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
        crate::output::trim_marker(&runnable(&doc.memory.path), "", doc.total, doc.total + 1, doc.total)
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

/// `target_raw` as the docs walk takes it: its canonical path, the
/// repository holding it, and whether its own folder is the scope.
pub fn resolve_target(target_raw: &Path, directory_mode: bool) -> Result<(PathBuf, PathBuf, bool)> {
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
