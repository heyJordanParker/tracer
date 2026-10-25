//! A shell command line read as the commands it runs, through the bash
//! grammar `trace` already carries. `trace hook` reads every command a line
//! runs from it: the `trace` calls a Subagent's shell makes, the files a Codex
//! shell read reaches, and the raw reads `--tracer-only` refuses.
//!
//! A regex over the text matches `trace` inside a quoted argument, as a grep
//! pattern holding `|trace` does. The grammar sees only command position.
//!
//! A command another command runs counts too: the script of a shell's `-c`,
//! read as a line of its own, and the program `xargs`, `find -exec`, `env`,
//! `timeout`, `nice`, `nohup`, `command`, or `exec` starts.

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
    /// Whether it sits inside a shell's `-c` script, where a word written at
    /// `name_end` lands inside that script's quotes.
    pub scripted: bool,
    /// Whether `xargs` runs it, adding arguments it reads from its input, so
    /// `argv` is not every argument the command gets.
    pub fed: bool,
    /// Whether something other than the terminal takes its input, output,
    /// or exit status: a redirect of its input or output, a command
    /// substitution its output becomes, or a condition, `&&`, or `||` that
    /// tests how it exits.
    pub redirected: bool,
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
    let mut reader = Reader { line, commands: Vec::new(), folder: String::new(), redirected: false };
    reader.walk(tree.root_node(), None);
    reader.commands
}

/// `line` with `--agent <agent>` written after every `trace` program word
/// that does not already carry it. `None` when the line runs no such `trace`.
/// Inside a `-c` script the word is written only when `agent` needs no
/// quotes, as every harness's agent id does, so the script's own quoting
/// stays whole.
pub fn with_agent(line: &str, agent: &str) -> Option<String> {
    let word = quote(agent);
    let mut at: Vec<usize> = commands(line)
        .into_iter()
        .filter(|command| {
            command.program() == "trace"
                && !command.args().iter().any(|arg| arg == "--agent" || arg.starts_with("--agent="))
                && (!command.scripted || word == agent)
        })
        .map(|command| command.name_end)
        .collect();
    if at.is_empty() {
        return None;
    }
    at.sort_unstable();
    let mut out = line.to_string();
    for offset in at.into_iter().rev() {
        out.insert_str(offset, &format!(" --agent {word}"));
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
    /// Whether the commands read now are `Command::redirected`.
    redirected: bool,
}

impl Reader<'_> {
    /// Reads `node`'s commands. `input` is the command a pipe feeds into the
    /// first command `node` holds.
    fn walk(&mut self, node: Node, input: Option<usize>) {
        match node.kind() {
            "command" => self.command(node, input),
            "redirected_statement" => self.marked(redirects_terminal(node, self.line), |reader| reader.children(node, input)),
            "command_substitution" | "process_substitution" => self.marked(true, |reader| reader.children(node, None)),
            "list" => {
                let mut cursor = node.walk();
                let operands: Vec<Node> = node.named_children(&mut cursor).collect();
                let last = operands.len().saturating_sub(1);
                for (index, operand) in operands.into_iter().enumerate() {
                    self.marked(index < last, |reader| reader.walk(operand, if index == 0 { input } else { None }));
                }
            }
            "if_statement" | "elif_clause" | "while_statement" => {
                let mut cursor = node.walk();
                let conditions: Vec<usize> = node.children_by_field_name("condition", &mut cursor).map(|condition| condition.id()).collect();
                let mut cursor = node.walk();
                let parts: Vec<Node> = node.named_children(&mut cursor).collect();
                for part in parts {
                    self.marked(conditions.contains(&part.id()), |reader| reader.walk(part, None));
                }
            }
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

    /// Runs `read` with the commands it reads `redirected` when `mark` is.
    fn marked(&mut self, mark: bool, read: impl FnOnce(&mut Self)) {
        let outer = self.redirected;
        self.redirected |= mark;
        read(self);
        self.redirected = outer;
    }

    fn command(&mut self, node: Node, input: Option<usize>) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let mut cursor = node.walk();
        let words: Vec<Node> = std::iter::once(name).chain(node.children_by_field_name("argument", &mut cursor)).collect();
        let argv: Vec<String> = words.iter().map(|word| self.word(*word)).collect();
        let ends: Vec<usize> = words.iter().map(Node::end_byte).collect();
        // A here-string is the command's own redirect.
        let redirected = self.redirected || redirects_terminal(node, self.line);
        let command = Command { argv, name_end: name.end_byte(), folder: self.folder.clone(), input, scripted: false, fed: false, redirected };
        if command.program() == "cd" {
            if let Some(target) = command.args().iter().find(|arg| !arg.starts_with('-')) {
                self.folder = match (self.folder.is_empty(), target.starts_with('/')) {
                    (true, _) | (_, true) => target.clone(),
                    (false, false) => format!("{}/{target}", self.folder),
                };
            }
        }
        let argv = command.argv.clone();
        self.commands.push(command);
        let script = self.nested(&argv, &ends, &words, false);
        // A command substitution in an argument runs commands of its own. A
        // `-c` script was read whole, its substitutions with it.
        for argument in &words[1..] {
            if script.is_none_or(|script| script.id() != argument.id()) {
                self.substitutions(*argument);
            }
        }
    }

    /// Reads the command `argv` runs in its own arguments, and returns the
    /// `-c` script word it read, if any. `ends` and `words` are each word's
    /// end offset and node, the program's first. `fed` is whether `xargs`
    /// adds arguments to `argv`.
    fn nested<'t>(&mut self, argv: &[String], ends: &[usize], words: &[Node<'t>], fed: bool) -> Option<Node<'t>> {
        let program = argv.first()?.rsplit('/').next().unwrap_or_default();
        if SHELLS.contains(&program) {
            let script = *words.get(script_flag(argv)? + 1)?;
            return self.script(script, fed).then_some(script);
        }
        let (start, end) = runs(argv)?;
        let fed = fed || program == "xargs";
        self.commands.push(Command {
            argv: argv[start..end].to_vec(),
            name_end: ends[start],
            folder: self.folder.clone(),
            input: None,
            scripted: false,
            fed,
            redirected: self.redirected,
        });
        self.nested(&argv[start..end], &ends[start..end], &words[start..end], fed)
    }

    /// A shell's `-c` script, read as a line of its own at its place in this
    /// line, and whether it was read. A script holding a backslash is left
    /// unread, because its escapes shift the offsets a word written into it
    /// needs, and the caller reads its substitutions as the outer line's.
    /// `fed` is whether `xargs` runs the shell, so the script's commands may
    /// get its arguments through `"$@"`.
    fn script(&mut self, node: Node, fed: bool) -> bool {
        let range = match node.kind() {
            "raw_string" | "string" => node.start_byte() + 1..node.end_byte().saturating_sub(1),
            _ => return false,
        };
        let Some(text) = self.line.get(range.clone()) else {
            return false;
        };
        if text.contains('\\') {
            return false;
        }
        let base = self.commands.len();
        for mut inner in commands(text) {
            inner.name_end += range.start;
            inner.input = inner.input.map(|at| base + at);
            inner.scripted = true;
            inner.fed |= fed;
            inner.redirected |= self.redirected;
            inner.folder = match (self.folder.is_empty(), inner.folder.is_empty(), inner.folder.starts_with('/')) {
                (_, true, _) => self.folder.clone(),
                (true, _, _) | (_, _, true) => inner.folder,
                (false, false, false) => format!("{}/{}", self.folder, inner.folder),
            };
            self.commands.push(inner);
        }
        true
    }

    fn substitutions(&mut self, node: Node) {
        if node.kind() == "command_substitution" || node.kind() == "process_substitution" {
            let folder = self.folder.clone();
            self.marked(true, |reader| reader.children(node, None));
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

/// The shells whose `-c` script runs as a line of its own.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash"];

/// Whether a redirected statement or a command takes its input or output
/// from the terminal: a here-document, a here-string, or a file redirect of standard
/// input or output. A redirect of another descriptor, such as `2>/dev/null`,
/// and a copy of one descriptor onto another, such as `>&2`, leave it.
fn redirects_terminal(node: Node, line: &str) -> bool {
    let mut cursor = node.walk();
    let redirects: Vec<Node> = node.children_by_field_name("redirect", &mut cursor).collect();
    redirects.into_iter().any(|redirect| match redirect.kind() {
        "heredoc_redirect" | "herestring_redirect" => true,
        "file_redirect" => {
            let descriptor = redirect.child_by_field_name("descriptor").map(|descriptor| &line[descriptor.byte_range()]);
            let mut cursor = redirect.walk();
            let copies = redirect.children(&mut cursor).any(|part| !part.is_named() && matches!(part.kind(), ">&" | "<&" | ">&-" | "<&-"));
            matches!(descriptor, None | Some("0" | "1")) && !copies
        }
        _ => false,
    })
}

/// Where a shell's `-c` flag sits in its `argv`, among the options before
/// its first other word: a `-c` after a script file is that script's own.
fn script_flag(argv: &[String]) -> Option<usize> {
    let mut at = 1;
    while let Some(arg) = argv.get(at) {
        match arg.as_str() {
            "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file" => at += 2,
            long if long.starts_with("--") => at += 1,
            short if short.starts_with('-') && short.contains('c') => return Some(at),
            short if short.starts_with(['-', '+']) && short.len() > 1 => at += 1,
            _ => return None,
        }
    }
    None
}

/// Where the command a runner starts sits in its `argv`: the first word past
/// the runner's options, the values those options take, and the words that
/// aren't the command, as `env`'s `NAME=value` and `timeout`'s duration; for
/// `find`, the words from `-exec` to its `;` or `+`. `None` for any other
/// program, and for `command -v`, which names a program without running it.
fn runs(argv: &[String]) -> Option<(usize, usize)> {
    let past_options = |valued: &[&str]| {
        let mut at = 1;
        while let Some(arg) = argv.get(at) {
            if arg == "--" {
                return at + 1;
            }
            if !arg.starts_with('-') || arg == "-" {
                return at;
            }
            at += if valued.contains(&arg.as_str()) { 2 } else { 1 };
        }
        at
    };
    let program = argv.first()?.rsplit('/').next().unwrap_or_default();
    let start = match program {
        "xargs" => past_options(&["-n", "-I", "-L", "-P", "-s", "-d", "-E", "-a"]),
        "env" => {
            let mut at = past_options(&["-u", "-C", "-S"]);
            while argv.get(at).is_some_and(|word| assignment(word)) {
                at += 1;
            }
            at
        }
        "timeout" => past_options(&["-s", "-k"]) + 1,
        "nice" => past_options(&["-n"]),
        "nohup" => past_options(&[]),
        "exec" => past_options(&["-a"]),
        "command" if argv[1..].iter().take_while(|arg| arg.starts_with('-')).any(|arg| arg.contains(['v', 'V'])) => return None,
        "command" => past_options(&[]),
        "find" => argv.iter().position(|arg| matches!(arg.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))? + 1,
        _ => return None,
    };
    if start >= argv.len() {
        return None;
    }
    let end = match program {
        "find" => argv[start..].iter().position(|arg| arg == ";" || arg == "+").map_or(argv.len(), |at| start + at),
        _ => argv.len(),
    };
    (start < end).then_some((start, end))
}

/// Whether `word` is a `NAME=value` assignment, as `env` takes before the
/// command it runs.
fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.starts_with(|c: char| c.is_ascii_digit())
    })
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

    #[test]
    fn a_shell_script_gets_the_agent_inside_its_quotes() {
        assert_eq!(with_agent("bash -c 'trace read a.rs'", "a1").unwrap(), "bash -c 'trace --agent a1 read a.rs'");
        assert_eq!(
            with_agent(r#"sh -c "trace grep x | head""#, "a1").unwrap(),
            r#"sh -c "trace --agent a1 grep x | head""#
        );
    }

    #[test]
    fn a_double_quoted_script_keeps_what_the_outer_shell_expands() {
        assert_eq!(
            with_agent(r#"f=a.rs; bash -c "trace read $f""#, "a1").unwrap(),
            r#"f=a.rs; bash -c "trace --agent a1 read $f""#
        );
    }

    #[test]
    fn a_substitution_inside_a_script_gets_the_agent_once() {
        assert_eq!(
            with_agent(r#"bash -c "echo $(trace read a.rs)""#, "a1").unwrap(),
            r#"bash -c "echo $(trace --agent a1 read a.rs)""#
        );
    }

    #[test]
    fn a_substitution_inside_a_script_left_unread_is_still_read() {
        assert_eq!(
            with_agent(r#"bash -c "echo $(trace read a.rs) \"done\"""#, "a1").unwrap(),
            r#"bash -c "echo $(trace --agent a1 read a.rs) \"done\"""#
        );
        assert!(argvs(r#"bash -c "wc $(cat lib.rs) \"x\"""#).contains(&vec!["cat".to_string(), "lib.rs".to_string()]));
    }

    #[test]
    fn a_command_xargs_runs_is_fed_its_arguments() {
        let found = commands("git diff --name-only | xargs ls -la");
        let ls = found.iter().find(|command| command.program() == "ls").unwrap();
        assert!(ls.fed);
        assert!(!found[0].fed);
    }

    #[test]
    fn a_script_xargs_runs_is_fed_its_arguments() {
        let found = commands("git ls-files | xargs sh -c 'cat \"$@\"' _");
        assert!(found.iter().find(|command| command.program() == "cat").unwrap().fed);
    }

    #[test]
    fn a_bare_word_script_is_left_unread() {
        assert_eq!(with_agent("bash -c trace", "a1"), None);
    }

    #[test]
    fn a_c_flag_after_a_script_file_is_the_scripts_own() {
        assert_eq!(with_agent("bash notify.sh -c 'trace read a.rs'", "a1"), None);
        assert_eq!(with_agent("bash -o pipefail -c 'trace read a.rs'", "a1").unwrap(), "bash -o pipefail -c 'trace --agent a1 read a.rs'");
    }

    #[test]
    fn a_script_already_naming_its_agent_is_left_alone() {
        assert_eq!(with_agent("bash -c 'trace --agent a1 read a.rs'", "a1"), None);
    }

    #[test]
    fn a_script_needing_a_quoted_agent_is_left_alone() {
        assert_eq!(with_agent("bash -c 'trace read a.rs'", "sub 1"), None);
    }

    #[test]
    fn the_program_a_runner_starts_gets_the_agent() {
        assert_eq!(with_agent("ls *.rs | xargs -n 1 trace read", "a1").unwrap(), "ls *.rs | xargs -n 1 trace --agent a1 read");
        assert_eq!(
            with_agent(r"find . -name '*.rs' -exec trace read {} \;", "a1").unwrap(),
            r"find . -name '*.rs' -exec trace --agent a1 read {} \;"
        );
        assert_eq!(with_agent("env X=1 trace read a.rs", "a1").unwrap(), "env X=1 trace --agent a1 read a.rs");
        assert_eq!(with_agent("timeout -k 5 60 trace read a.rs", "a1").unwrap(), "timeout -k 5 60 trace --agent a1 read a.rs");
        assert_eq!(with_agent("nice -n 5 nohup trace read a.rs", "a1").unwrap(), "nice -n 5 nohup trace --agent a1 read a.rs");
    }

    #[test]
    fn a_word_a_runner_only_passes_on_is_never_a_command() {
        assert_eq!(with_agent("xargs grep trace", "a1"), None);
        assert_eq!(with_agent("command -v trace", "a1"), None);
    }

    #[test]
    fn a_read_inside_a_script_is_read() {
        let found = commands("bash -lc 'cd src && cat lib.rs'");
        let cat = found.iter().find(|command| command.program() == "cat").unwrap();
        assert_eq!((cat.argv.clone(), cat.folder.as_str()), (vec!["cat".to_string(), "lib.rs".to_string()], "src"));
    }

    #[test]
    fn a_command_something_else_takes_the_input_output_or_exit_of_is_redirected() {
        let redirected = |line: &str| -> Vec<(String, bool)> {
            commands(line).iter().map(|command| (command.program().to_string(), command.redirected)).collect()
        };
        let pair = |program: &str, redirected: bool| (program.to_string(), redirected);
        assert_eq!(redirected("cat a > b"), [pair("cat", true)]);
        assert_eq!(redirected("rg x < a"), [pair("rg", true)]);
        assert_eq!(redirected(r#"rg x <<< "$y""#), [pair("rg", true)]);
        assert_eq!(redirected("n=$(grep -c x a)"), [pair("grep", true)]);
        assert_eq!(redirected("grep -l x a || echo none"), [pair("grep", true), pair("echo", false)]);
        assert_eq!(redirected("if grep -q x a; then cat a; fi"), [pair("grep", true), pair("cat", false)]);
        assert_eq!(redirected("cd src && cat a"), [pair("cd", true), pair("cat", false)]);
        assert_eq!(redirected("cat a 2>/dev/null"), [pair("cat", false)]);
        assert_eq!(redirected("cat a >&2"), [pair("cat", false)]);
        assert_eq!(redirected("cat a | head"), [pair("cat", false), pair("head", false)]);
    }
}
