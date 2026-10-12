//! Each harness's hook input: its tool names and the fields each tool's
//! input and response carry, read into one `Tool`. Neither harness names
//! itself in its input, so `--harness` names it.
//!
//! The dotfiles' `lib/event.py` keeps a copy of these names for its own
//! gates, so a harness renaming a tool changes both.

use crate::session::Hunk;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Harness {
    Claude,
    Codex,
}

/// A tool call, as its hook input carries it.
pub enum Tool {
    /// `lines` are the lines the tool returned, 1-based inclusive, `None`
    /// for the whole file.
    Read { path: PathBuf, lines: Option<(usize, usize)> },
    /// Every file an edit changed. A file whose changed lines can't be
    /// placed is left out of `edited` and named in `touched` alone.
    Edit { edited: Vec<Edited>, touched: Vec<PathBuf> },
    Grep { pattern: String, path: Option<String>, ignore_case: bool, multiline: bool, glob: Option<String>, kind: Option<String>, content: bool },
    Glob { files: Vec<PathBuf> },
    Shell { command: String, input: Value },
}

/// One file an edit changed: its changes, `None` when it wrote the whole file.
pub struct Edited {
    pub path: PathBuf,
    pub hunks: Option<Vec<Hunk>>,
}

impl Harness {
    pub fn tool(self, input: &Value) -> Option<Tool> {
        let name = input["tool_name"].as_str()?;
        let tool = &input["tool_input"];
        let response = &input["tool_response"];
        match (self, name) {
            // Claude Code's Read returns at most its default line count, or
            // fewer under its token cap, and names the lines it returned.
            (_, "Read") => Some(Tool::Read {
                path: PathBuf::from(text(tool, "file_path").or_else(|| text(tool, "path"))?),
                lines: match (count(&response["file"], "startLine"), count(&response["file"], "numLines")) {
                    (Some(start), Some(read)) if read > 0 => Some((start, start + read - 1)),
                    _ => crate::commands::context::read_range(count(tool, "offset"), count(tool, "limit")),
                },
            }),
            (Harness::Claude, "Edit" | "MultiEdit" | "Write" | "NotebookEdit") => {
                let path = PathBuf::from(text(tool, "file_path").or_else(|| text(tool, "notebook_path"))?);
                let hunks = match response["type"].as_str() {
                    Some("create") => None,
                    _ => match structured(&response["structuredPatch"]) {
                        Some(hunks) => Some(hunks),
                        None if name == "Edit" || name == "MultiEdit" => {
                            return Some(Tool::Edit { edited: Vec::new(), touched: vec![path] });
                        }
                        None => None,
                    },
                };
                Some(Tool::Edit { edited: vec![Edited { path: path.clone(), hunks }], touched: vec![path] })
            }
            (Harness::Codex, "apply_patch") => Some(patched(&patch_body(tool)?)),
            (Harness::Claude, "Grep") => Some(Tool::Grep {
                pattern: text(tool, "pattern")?.to_string(),
                path: text(tool, "path").map(str::to_string),
                ignore_case: tool["-i"].as_bool().unwrap_or(false),
                multiline: tool["multiline"].as_bool().unwrap_or(false),
                glob: text(tool, "glob").map(str::to_string),
                kind: text(tool, "type").map(str::to_string),
                content: tool["output_mode"].as_str() == Some("content"),
            }),
            (Harness::Claude, "Glob") => Some(Tool::Glob {
                files: response["filenames"]
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(PathBuf::from)
                    .collect(),
            }),
            (Harness::Claude, "Bash") | (Harness::Codex, "Bash" | "exec_command" | "shell_command") => {
                Some(Tool::Shell { command: command(&tool["command"])?, input: tool.clone() })
            }
            _ => None,
        }
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str().filter(|text| !text.is_empty())
}

fn count(value: &Value, key: &str) -> Option<usize> {
    value[key].as_u64().map(|n| n as usize)
}

/// A shell command as one line. Codex may send it as argv, such as
/// `["/bin/zsh", "-lc", "<line>"]`, whose line is the shell's `-c` argument.
fn command(value: &Value) -> Option<String> {
    match value {
        Value::String(line) => Some(line.clone()),
        Value::Array(argv) => {
            let words: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
            match words.iter().position(|word| *word == "-c" || *word == "-lc") {
                Some(at) => words.get(at + 1).map(|line| line.to_string()),
                None => Some(crate::shell::join(&words.iter().map(|word| word.to_string()).collect::<Vec<_>>())),
            }
        }
        _ => None,
    }
}

/// Claude Code's `structuredPatch`, unified hunks with their context lines,
/// as the changes alone: each run of `-` and `+` lines is one `Hunk`.
fn structured(patch: &Value) -> Option<Vec<Hunk>> {
    let mut hunks = Vec::new();
    for hunk in patch.as_array()? {
        let lines = hunk["lines"].as_array()?.iter().filter_map(Value::as_str);
        hunks.extend(changes(lines, hunk["oldStart"].as_u64()? as usize, hunk["newStart"].as_u64()? as usize));
    }
    Some(hunks)
}

/// Unified diff lines, starting at line `old` of the old file and `new` of
/// the new, as their changes: each run of `-` and `+` lines is one `Hunk`.
fn changes<'a>(lines: impl Iterator<Item = &'a str>, mut old: usize, mut new: usize) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut current: Option<Hunk> = None;
    for line in lines {
        match line.as_bytes().first() {
            Some(b'-') => {
                current.get_or_insert(Hunk { old_start: old, old_count: 0, new_start: new, new_count: 0 }).old_count += 1;
                old += 1;
            }
            Some(b'+') => {
                current.get_or_insert(Hunk { old_start: old, old_count: 0, new_start: new, new_count: 0 }).new_count += 1;
                new += 1;
            }
            Some(b'\\') => {}
            _ => {
                hunks.extend(current.take());
                old += 1;
                new += 1;
            }
        }
    }
    hunks.extend(current);
    hunks
}

/// Codex's `apply_patch` body, which it sends under `command`.
fn patch_body(tool: &Value) -> Option<String> {
    ["command", "input", "patch"]
        .iter()
        .find_map(|key| tool[key].as_str().filter(|body| body.contains("*** ")).map(str::to_string))
}

/// The files an `apply_patch` body changed. Its grammar carries no line
/// numbers, so each chunk's new side, its context and `+` lines, is found in
/// the file as written, the way Codex applies it. `*** Move to:` names the
/// file the changes landed in.
fn patched(body: &str) -> Tool {
    let mut edited = Vec::new();
    let mut touched = Vec::new();
    let mut sections: Vec<(String, PathBuf, Vec<&str>)> = Vec::new();
    for line in body.lines() {
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            sections.push(("update".into(), PathBuf::from(path.trim()), Vec::new()));
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            sections.push(("add".into(), PathBuf::from(path.trim()), Vec::new()));
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            sections.push(("delete".into(), PathBuf::from(path.trim()), Vec::new()));
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            if let Some(section) = sections.last_mut() {
                section.1 = PathBuf::from(path.trim());
            }
        } else if let Some(section) = sections.last_mut() {
            section.2.push(line);
        }
    }
    for (action, path, lines) in sections {
        match action.as_str() {
            "add" => edited.push(Edited { path: path.clone(), hunks: None }),
            "update" => {
                if let Some(hunks) = std::fs::read_to_string(&path).ok().and_then(|written| placed(&written, &lines)) {
                    edited.push(Edited { path: path.clone(), hunks: Some(hunks) });
                }
            }
            _ => continue,
        }
        touched.push(path);
    }
    Tool::Edit { edited, touched }
}

/// An update's chunks placed in the file as written: `None` when a chunk's
/// new side is not found in order.
fn placed(written: &str, lines: &[&str]) -> Option<Vec<Hunk>> {
    let file: Vec<&str> = written.lines().collect();
    let mut hunks = Vec::new();
    let mut from = 0;
    let mut delta: isize = 0;
    for chunk in lines.split(|line| line.starts_with("@@")).filter(|chunk| !chunk.is_empty()) {
        let new_side: Vec<&str> = chunk
            .iter()
            .filter(|line| !line.starts_with('-') && !line.starts_with("*** "))
            .map(|line| line.get(1..).unwrap_or(""))
            .collect();
        // A chunk of only removed lines leaves nothing in the file to find.
        if new_side.is_empty() {
            return None;
        }
        let at = from + file.get(from..)?.windows(new_side.len()).position(|window| window == new_side.as_slice())?;
        let lines = chunk.iter().copied().filter(|line| !line.starts_with("*** "));
        hunks.extend(changes(lines, (at + 1).checked_add_signed(-delta)?, at + 1));
        delta = hunks.iter().map(|hunk| hunk.new_count as isize - hunk.old_count as isize).sum();
        from = at + new_side.len();
    }
    Some(hunks)
}

/// `path` resolved from the hook's working folder.
pub fn resolved(path: &Path, cwd: &Path) -> PathBuf {
    match path.is_absolute() {
        true => path.to_path_buf(),
        false => cwd.join(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_structured_patch_becomes_its_changes_without_the_context() {
        let patch = json!([{
            "oldStart": 10, "oldLines": 7, "newStart": 10, "newLines": 8,
            "lines": [" a", " b", "-c", "+C", "+D", " e", " f", "-g", " h"]
        }]);
        assert_eq!(
            structured(&patch).unwrap(),
            vec![
                Hunk { old_start: 12, old_count: 1, new_start: 12, new_count: 2 },
                Hunk { old_start: 15, old_count: 1, new_start: 16, new_count: 0 },
            ]
        );
    }

    #[test]
    fn an_apply_patch_chunk_is_placed_by_its_new_side() {
        let written = "one\ntwo\nTHREE\nfour\nfive\nsix\nSEVEN\nEIGHT\n";
        let body = ["@@", " two", "-three", "+THREE", " four", "@@", " six", "-seven", "+SEVEN", "+EIGHT"];
        assert_eq!(
            placed(written, &body).unwrap(),
            vec![
                Hunk { old_start: 3, old_count: 1, new_start: 3, new_count: 1 },
                Hunk { old_start: 7, old_count: 1, new_start: 7, new_count: 2 },
            ]
        );
    }

    #[test]
    fn a_chunk_longer_than_the_file_or_with_no_new_side_is_not_placed() {
        assert_eq!(placed("one\n", &["@@", " one", "+two"]), None);
        assert_eq!(placed("one\n", &["@@", "-gone"]), None);
    }

    #[test]
    fn a_codex_argv_command_reads_as_its_shell_line() {
        assert_eq!(command(&json!(["/bin/zsh", "-lc", "cat a.rs | head"])).unwrap(), "cat a.rs | head");
    }
}
