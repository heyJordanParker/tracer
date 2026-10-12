//! The event table: what `trace hook` does on each harness event.
//!
//! - SessionStart records the docs the harness loaded at start, re-sends
//!   the rules a compaction dropped, and offers the primer. Codex also gets
//!   the working folder's rules, which it never loads itself.
//! - PreCompact forgets what the agent holds, keeping the rules to re-send.
//! - SubagentStart and InstructionsLoaded record what the harness loaded.
//! - PreToolUse on a shell command names a Subagent in its `trace` calls and,
//!   with `--tracer-only`, refuses raw code reads.
//! - PostToolUse adds the context around what a tool returned or wrote.
//!   Codex loads no docs of its own, so a file it touches gets its docs too.

use super::harness::{resolved, Tool};
use super::{Answer, Harness, Input, BUDGET};
use crate::commands::docs_prime::{self, Reason};
use crate::commands::{context, find, grep};
use crate::{docs, nested_memory};
use crate::session::{self, Delivery};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub fn answer(input: &Input, delivery: &mut Delivery) -> Result<Answer> {
    let context = match input.event.as_str() {
        "SessionStart" => session_start(input, delivery),
        "PreCompact" => {
            session::compact(nested_memory::conditional_user_rule);
            String::new()
        }
        "SubagentStart" => {
            docs_prime::record_start(input.harness == Harness::Codex, Reason::SessionStart);
            String::new()
        }
        "InstructionsLoaded" => {
            if let Some(doc) = &input.loaded {
                docs_prime::run(Reason::SessionStart, std::slice::from_ref(doc), true)?;
            }
            String::new()
        }
        "PreToolUse" => return Ok(pre_tool_use(input)),
        "PostToolUse" => post_tool_use(input, false, delivery)?,
        "PostToolUseFailure" => post_tool_use(input, true, delivery)?,
        _ => String::new(),
    };
    Ok(Answer { context, ..Answer::default() })
}

/// The start docs recorded, then in one budget, kept whole in this order:
/// the rules a compaction dropped, Codex's working-folder rules, and the
/// primer, which is cut first.
fn session_start(input: &Input, delivery: &mut Delivery) -> String {
    let codex = input.harness == Harness::Codex;
    if input.source == "clear" {
        session::reset();
    }
    let compact = input.source == "compact";
    docs_prime::record_start(codex, if compact { Reason::PostCompact } else { Reason::SessionStart });
    // The room the hook left this answer, less the blank lines between its parts.
    let room = crate::output::budget().unwrap_or(BUDGET).saturating_sub(SEPARATOR.len() * 2);
    let rules = if compact { resent_rules(room, delivery) } else { String::new() };
    let folder = match codex {
        true => docs::reached(std::slice::from_ref(&input.cwd), Some(room.saturating_sub(rules.len())), SOURCE, delivery),
        false => String::new(),
    };
    let primer = crate::cache::worktree_root_for(&input.cwd)
        .map(|root| crate::primer::delivered(&root, room.saturating_sub(rules.len() + folder.len()), delivery))
        .unwrap_or_default();
    [primer, folder, rules].iter().filter(|part| !part.trim().is_empty()).map(|part| part.trim_matches('\n')).collect::<Vec<_>>().join(SEPARATOR)
}

const SEPARATOR: &str = "\n\n";

const SOURCE: &str = "tracer_session_start";

/// The rule files the last compaction kept, sent again from their first line.
fn resent_rules(room: usize, delivery: &mut Delivery) -> String {
    let Some(boundary) = nested_memory::user_rules_dir() else {
        return String::new();
    };
    let (mut pass, mut held) = (BTreeSet::new(), BTreeSet::new());
    let rules: Vec<nested_memory::LoadedMemory> = session::kept()
        .iter()
        .filter_map(|path| nested_memory::try_load(Path::new(path), &boundary, "rule", &mut pass, &mut held))
        .collect();
    let again = format!("{} read", crate::output::trace_command());
    docs::deliver(rules, Some(room), &again, "tracer_post_compact", delivery)
}

fn pre_tool_use(input: &Input) -> Answer {
    let Some(Tool::Shell { command, input: tool_input }) = &input.tool else {
        return Answer::default();
    };
    if input.tracer_only {
        if let Some(reason) = super::refusal::refusal(command, &input.cwd) {
            return Answer { refusal: Some(reason), ..Answer::default() };
        }
    }
    let updated_input = input
        .agent
        .as_deref()
        .and_then(|agent| crate::shell::with_agent(command, agent))
        .map(|line| {
            let mut rewritten = tool_input.clone();
            rewritten["command"] = line.into();
            rewritten
        });
    Answer { updated_input, ..Answer::default() }
}

/// The context around what the tool returned or wrote. A failed call wrote
/// nothing and showed nothing, except a Read of an existing file refused for
/// its size, whose file still gets its context, with no lines seen.
fn post_tool_use(input: &Input, failed: bool, delivery: &mut Delivery) -> Result<String> {
    let codex = input.harness == Harness::Codex;
    let Some(tool) = &input.tool else {
        return Ok(String::new());
    };
    let (text, touched) = match tool {
        Tool::Read { path, lines } => {
            let path = resolved(path, &input.cwd);
            if failed && !path.is_file() {
                return Ok(String::new());
            }
            crate::output::set_command(format!("{} context {}", crate::output::trace_command(), crate::shell::quote(&path.to_string_lossy())));
            let (text, shown) = context::read_context(&path, *lines, !failed)?;
            *delivery = shown;
            (text, vec![path])
        }
        _ if failed => return Ok(String::new()),
        Tool::Edit { edited, touched } => {
            let mut text = String::new();
            for edit in edited {
                let path = resolved(&edit.path, &input.cwd);
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let changed: Vec<(i64, i64)> = match &edit.hunks {
                    Some(hunks) => hunks
                        .iter()
                        .filter(|hunk| hunk.new_count > 0)
                        .map(|hunk| (hunk.new_start as i64, (hunk.new_start + hunk.new_count - 1) as i64))
                        .collect(),
                    None => vec![(1, content.lines().count().max(1) as i64)],
                };
                delivery.edited(&path, "agent_edit", &content, edit.hunks.clone());
                text.push_str(&context::edit_context(&path, &changed, delivery));
            }
            (text, touched.iter().map(|path| resolved(path, &input.cwd)).collect())
        }
        Tool::Grep { pattern, path, ignore_case, multiline, glob, kind, content } => {
            let searched = path.clone().unwrap_or_else(|| ".".into());
            crate::output::set_command(format!(
                "{} grep {} {}",
                crate::output::trace_command(),
                crate::shell::quote(pattern),
                crate::shell::quote(&searched)
            ));
            let options = grep::Options {
                ignore_case: *ignore_case,
                multiline: *multiline,
                globs: glob.iter().cloned().collect(),
                types: kind.iter().cloned().collect(),
                ..grep::Options::default()
            };
            (grep::native_context(pattern, &[searched], &options, *content, delivery)?, Vec::new())
        }
        Tool::Glob { files } => {
            let files: Vec<PathBuf> = files.iter().map(|file| resolved(file, &input.cwd)).collect();
            (find::native_context(&files, delivery), Vec::new())
        }
        Tool::Shell { command, .. } => (String::new(), if codex { shell_reads(command, &input.cwd) } else { Vec::new() }),
    };
    let docs = match codex && !touched.is_empty() {
        true => docs::reached(&touched, Some(crate::output::budget().unwrap_or(BUDGET).saturating_sub(text.len())), "tracer_touch", delivery),
        false => String::new(),
    };
    Ok(format!("{text}{docs}"))
}

/// The files a shell command line reads with a plain reader, such as `cat`.
fn shell_reads(line: &str, cwd: &Path) -> Vec<PathBuf> {
    crate::shell::commands(line)
        .iter()
        .filter(|command| super::refusal::READERS.contains(&command.program()))
        .flat_map(|command| {
            let folder = cwd.join(&command.folder);
            command
                .args()
                .iter()
                .filter(|arg| !arg.starts_with('-'))
                .map(move |arg| folder.join(arg))
                .filter(|path| path.is_file())
                .collect::<Vec<_>>()
        })
        .collect()
}
