//! A shell command line read as the commands it runs, through the bash
//! grammar `trace` already carries. `trace hook` reads every command a line
//! runs from it: the `trace` calls a Subagent's shell makes, the files a Codex
//! shell read reaches, and the raw reads `--tracer-only` refuses.
//!
//! A regex over the text matches `trace` inside a quoted argument, as a grep
//! pattern holding `|trace` does. The grammar sees only command position.

use tree_sitter::{Node, Parser};

/// One command the line runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The program, then its arguments, unquoted. A word holding an expansion
    /// keeps its text as written.
    pub argv: Vec<String>,
    /// The byte offset in the line just after the program word.
    pub name_end: usize,
    /// The folder an earlier `cd` on the line moved to, as written, empty for
    /// the folder the line starts in. Later `cd`s join onto earlier ones.
    pub folder: String,
    /// The command whose output this one reads, through a pipe.
    pub input: Option<usize>,
}

impl Command {
    /// The program's own name, without its folder.
    pub fn program(&self) -> &str {
        self.argv.first().map_or("", |program| program.rsplit('/').next().unwrap_or(program))
    }

    pub fn args(&self) -> &[String] {
        self.argv.get(1..).unwrap_or(&[])
    }
}

/// Every command `line` runs, in the order they appear.
pub fn commands(line: &str) -> Vec<Command> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_bash::LANGUAGE.into()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(line, None) else {
        return Vec::new();
    };
    let mut reader = Reader { line, commands: Vec::new(), folder: String::new() };
    reader.walk(tree.root_node(), None);
    reader.commands
}

/// `line` with `--agent <agent>` written after every `trace` program word
/// that does not already carry it. `None` when the line runs no such `trace`.
pub fn with_agent(line: &str, agent: &str) -> Option<String> {
    let mut at: Vec<usize> = commands(line)
        .into_iter()
        .filter(|command| {
            command.program() == "trace"
                && !command.args().iter().any(|arg| arg == "--agent" || arg.starts_with("--agent="))
        })
        .map(|command| command.name_end)
        .collect();
    if at.is_empty() {
        return None;
    }
    at.sort_unstable();
    let mut out = line.to_string();
    for offset in at.into_iter().rev() {
        out.insert_str(offset, &format!(" --agent {}", quote(agent)));
    }
    Some(out)
}

/// `word` quoted for a POSIX shell, bare when it needs no quotes.
pub fn quote(word: &str) -> String {
    let plain = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
    match plain {
        true => word.to_string(),
        false => format!("'{}'", word.replace('\'', r"'\''")),
    }
}

/// `words` as one shell line, each quoted.
pub fn join(words: &[String]) -> String {
    words.iter().map(|word| quote(word)).collect::<Vec<_>>().join(" ")
}

struct Reader<'a> {
    line: &'a str,
    commands: Vec<Command>,
    folder: String,
}

impl Reader<'_> {
    /// Reads `node`'s commands. `input` is the command a pipe feeds into the
    /// first command `node` holds.
    fn walk(&mut self, node: Node, input: Option<usize>) {
        match node.kind() {
            "command" => self.command(node, input),
            "pipeline" => {
                let mut feeding = input;
                let mut cursor = node.walk();
                for part in node.named_children(&mut cursor) {
                    let before = self.commands.len();
                    self.walk(part, feeding);
                    if self.commands.len() > before {
                        feeding = Some(self.commands.len() - 1);
                    }
                }
            }
            "subshell" => {
                let folder = self.folder.clone();
                self.children(node, input);
                self.folder = folder;
            }
            _ => self.children(node, input),
        }
    }

    fn children(&mut self, node: Node, input: Option<usize>) {
        let mut cursor = node.walk();
        for (index, child) in node.named_children(&mut cursor).enumerate() {
            self.walk(child, if index == 0 { input } else { None });
        }
    }

    fn command(&mut self, node: Node, input: Option<usize>) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let mut argv = vec![self.word(name)];
        let mut cursor = node.walk();
        for argument in node.children_by_field_name("argument", &mut cursor) {
            argv.push(self.word(argument));
        }
        let command = Command { argv, name_end: name.end_byte(), folder: self.folder.clone(), input };
        if command.program() == "cd" {
            if let Some(target) = command.args().iter().find(|arg| !arg.starts_with('-')) {
                self.folder = match (self.folder.is_empty(), target.starts_with('/')) {
                    (true, _) | (_, true) => target.clone(),
                    (false, false) => format!("{}/{target}", self.folder),
                };
            }
        }
        self.commands.push(command);
        // A command substitution in an argument runs commands of its own.
        let mut cursor = node.walk();
        for argument in node.children_by_field_name("argument", &mut cursor) {
            self.substitutions(argument);
        }
    }

    fn substitutions(&mut self, node: Node) {
        if node.kind() == "command_substitution" || node.kind() == "process_substitution" {
            let folder = self.folder.clone();
            self.children(node, None);
            self.folder = folder;
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.substitutions(child);
        }
    }

    /// A word's value: quotes removed from literal text, the text as written
    /// for anything the shell expands.
    fn word(&self, node: Node) -> String {
        let text = &self.line[node.byte_range()];
        match node.kind() {
            "raw_string" => text.trim_start_matches('\'').trim_end_matches('\'').to_string(),
            "string" if !has_expansion(node) => unescape(&text[1..text.len().saturating_sub(1)], "\"\\$`"),
            "command_name" => node.named_child(0).map_or_else(|| text.to_string(), |inner| self.word(inner)),
            "word" => unescape(text, ""),
            "concatenation" => {
                let mut cursor = node.walk();
                node.named_children(&mut cursor).map(|part| self.word(part)).collect()
            }
            _ => text.to_string(),
        }
    }
}

fn has_expansion(node: Node) -> bool {
    let mut cursor = node.walk();
    let expanded = node
        .named_children(&mut cursor)
        .any(|child| !matches!(child.kind(), "string_content"));
    expanded
}

/// `text` with each backslash escape removed. `only` names the characters a
/// backslash escapes inside double quotes; empty means every character.
fn unescape(text: &str, only: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some(&next)) if only.is_empty() || only.contains(next) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argvs(line: &str) -> Vec<Vec<String>> {
        commands(line).into_iter().map(|command| command.argv).collect()
    }

    #[test]
    fn quoted_arguments_lose_their_quotes() {
        assert_eq!(
            argvs(r#"trace grep 'a|trace b' "x \"y\"" plain\ word"#),
            vec![vec!["trace", "grep", "a|trace b", "x \"y\"", "plain word"]]
        );
    }

    #[test]
    fn a_pipe_names_the_command_it_reads_from() {
        let found = commands("trace grep x | head -5 && cat a.rs");
        assert_eq!(found.iter().map(Command::program).collect::<Vec<_>>(), ["trace", "head", "cat"]);
        assert_eq!(found[1].input, Some(0));
        assert_eq!(found[2].input, None);
    }

    #[test]
    fn a_cd_moves_the_commands_after_it() {
        let found = commands("cd cli && cd src; cat main.rs");
        assert_eq!(found[2].folder, "cli/src");
        assert_eq!(found[0].folder, "");
    }

    #[test]
    fn trace_inside_a_quoted_argument_is_never_a_command() {
        assert_eq!(with_agent("rg 'x|trace y'", "a1"), None);
    }

    #[test]
    fn every_trace_call_gets_the_agent_once() {
        assert_eq!(
            with_agent("trace read a.rs && FOO=1 trace --agent a1 grep x | trace find '*.rs' $(trace tree .)", "sub 1").unwrap(),
            "trace --agent 'sub 1' read a.rs && FOO=1 trace --agent a1 grep x | trace --agent 'sub 1' find '*.rs' $(trace --agent 'sub 1' tree .)"
        );
    }

    #[test]
    fn a_call_naming_its_agent_anywhere_is_left_alone() {
        assert_eq!(with_agent("trace -C cli read a.rs --agent a1 && trace --agent=a1 tree .", "sub 1"), None);
    }
}
