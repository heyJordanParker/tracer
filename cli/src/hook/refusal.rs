//! `--tracer-only`: a shell command that reads, searches, or lists a file or
//! folder without tracer is refused, whatever its flags, and the refusal
//! names the nearest `trace` command and what it leaves out. So is `trace`
//! output piped into a filter, which throws its context away.
//!
//! A command whose output Claude never reads runs: one whose output goes to
//! a file, a command substitution, or a condition, and one piped into a
//! program that is not a text filter, such as `curl` or `git apply`. So does
//! one tracer has no answer for: `tail -f`, `sed -i`, a binary file, a path
//! that is neither a file nor a folder, `find -exec`, and a back-reference.

use crate::shell::{self, Command};
use std::path::{Path, PathBuf};

/// The programs that read the files they name, whose files a Codex shell
/// read reaches.
pub(super) const READERS: &[&str] = &["cat", "bat", "head", "tail", "sed", "awk"];
const FILTERS: &[&str] = &[
    "grep", "egrep", "fgrep", "rg", "sed", "awk", "head", "tail", "cut", "sort", "uniq", "wc", "column", "fold", "tr", "jq", "cat", "bat",
    "tee", "less", "more", "nl", "tac", "rev", "paste", "xxd", "od",
];

struct Instead {
    run: Vec<String>,
    left_out: Vec<String>,
}

impl Instead {
    fn new(run: Vec<String>, left_out: Vec<String>) -> Instead {
        Instead { run, left_out }
    }

    fn under(mut self, folder: &str) -> Instead {
        self.run.splice(1..1, ["-C".to_string(), shell::quote(folder)]);
        self
    }
}

/// The refusal for `line` run in `cwd`, `None` when it reads nothing raw
/// that tracer answers.
pub fn refusal(line: &str, cwd: &Path) -> Option<String> {
    let commands = shell::commands(line);
    let consumer = |at: usize| commands.iter().find(|other| other.input == Some(at));
    let read_by_claude = |mut at: usize| {
        while let Some(next) = commands.iter().position(|other| other.input == Some(at)) {
            let reader = &commands[next];
            if reader.redirected || reader.program() == "jq" || !FILTERS.contains(&reader.program()) {
                return false;
            }
            at = next;
        }
        true
    };
    commands.iter().enumerate().find_map(|(at, command)| {
        let source = command.input.and_then(|at| commands.get(at));
        if let Some(trace) = source.filter(|source| source.program() == "trace") {
            return FILTERS
                .contains(&command.program())
                .then(|| message(&[filtered(&trace.argv, command.program(), command.args())], &[]));
        }
        if command.fed || command.redirected || !read_by_claude(at) {
            return None;
        }
        let base = cwd.join(&command.folder);
        let given = |reader: &Command, file: String| {
            let mut given = reader.clone();
            given.argv.push(file);
            given
        };
        if let (Some(file), Some(next)) = (plain_file(command), consumer(at)) {
            if (Scope { base: base.clone(), piped: false }).replacement(&given(next, file)).is_some() {
                return None;
            }
        }
        let mut instead = match source.and_then(plain_file) {
            Some(file) => Scope { base, piped: false }.replacement(&given(command, file)),
            None => Scope { base, piped: source.is_some() }.replacement(command),
        }?;
        instead.left_out.extend(downstream(&commands, at));
        Some(message(&instead.run, &instead.left_out))
    })
}

fn message(run: &[String], left_out: &[String]) -> String {
    let mut text = format!("tracer: Claude reads files only through tracer.\nRun this instead: {}", run.join(" "));
    if !left_out.is_empty() {
        let command = run.get(if run.get(1).is_some_and(|word| word == "-C") { 3 } else { 1 });
        text.push_str(&format!(
            "\nLeft out: {}. trace numbers every line and fits every answer to the 30,000 characters Claude Code shows whole; `trace {} --help` lists what it takes.",
            left_out.join(", "),
            command.map_or("read", String::as_str),
        ));
    }
    text
}

fn downstream(commands: &[Command], mut at: usize) -> Vec<String> {
    let mut left = Vec::new();
    while let Some(next) = commands.iter().position(|other| other.input == Some(at)) {
        left.push(format!("| {}", shell::join(&commands[next].argv)));
        at = next;
    }
    left
}

/// The one file `cat <file>` prints, which a reader it pipes into reads.
fn plain_file(command: &Command) -> Option<String> {
    match command.args() {
        [file] if command.program() == "cat" && !file.starts_with('-') && !command.fed && !command.redirected => Some(file.clone()),
        _ => None,
    }
}

/// The `trace` call itself, with jq's expression as its in-binary `--filter`.
fn filtered(trace: &[String], filter: &str, args: &[String]) -> String {
    let expression = (filter == "jq").then(|| args.iter().find(|arg| !arg.starts_with('-'))).flatten();
    let Some(expression) = expression else {
        return shell::join(trace);
    };
    let mut words = trace.to_vec();
    if !words.iter().any(|word| word == "--json") {
        words.push("--json".into());
    }
    words.extend(["--filter".into(), expression.clone()]);
    shell::join(&words)
}

/// A command's arguments: its flags, each with the value it takes, and its
/// other words. A run of short flags such as `-rn` is each flag, `-C3` and
/// `--context=3` carry their value, `-20` is one flag, and every word after
/// `--` is a word.
struct Parsed {
    flags: Vec<(String, Option<String>)>,
    words: Vec<String>,
}

impl Parsed {
    /// `args` read with `valued` naming the flags that take a value.
    fn of(args: &[String], valued: &[&str]) -> Parsed {
        let mut flags = Vec::new();
        let mut words = Vec::new();
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if arg == "--" {
                words.extend(rest.by_ref().cloned());
            } else if let Some(long) = arg.strip_prefix("--") {
                let (name, value) = match long.split_once('=') {
                    Some((name, value)) => (format!("--{name}"), Some(value.to_string())),
                    None => (arg.clone(), None),
                };
                let value = match value {
                    None if valued.contains(&name.as_str()) => rest.next().cloned(),
                    value => value,
                };
                flags.push((name, value));
            } else if arg.len() < 2 || !arg.starts_with('-') {
                words.push(arg.clone());
            } else if number(&arg[1..]).is_some() {
                flags.push((arg.clone(), None));
            } else {
                for (at, letter) in arg[1..].char_indices() {
                    let flag = format!("-{letter}");
                    if valued.contains(&flag.as_str()) {
                        let attached = &arg[1 + at + letter.len_utf8()..];
                        let value = match attached.is_empty() {
                            true => rest.next().cloned(),
                            false => Some(attached.to_string()),
                        };
                        flags.push((flag, value));
                        break;
                    }
                    flags.push((flag, None));
                }
            }
        }
        Parsed { flags, words }
    }
}

fn flag_text(flag: &str, value: &Option<String>) -> String {
    match value {
        Some(value) => format!("{flag} {}", shell::quote(value)),
        None => flag.to_string(),
    }
}

enum Tail {
    Last(usize),
    From(usize),
}

struct Scope {
    base: PathBuf,
    /// Whether the command reads another command's output, so a search given
    /// no path searches that output, not the folder.
    piped: bool,
}

impl Scope {
    fn replacement(&self, command: &Command) -> Option<Instead> {
        let args = command.args();
        match command.program() {
            program @ ("cat" | "bat") => self.read(program, args),
            "head" => self.head(args),
            "tail" => self.tail(args),
            "sed" => self.sed(args),
            "awk" => self.awk(args),
            program @ ("grep" | "egrep" | "fgrep" | "rg") => self.search(program, args),
            program @ ("ls" | "eza") => self.list(program, args),
            "tree" => self.tree(args),
            "find" => self.find(args),
            "git" => self.git(args),
            _ => None,
        }
    }

    fn path(&self, word: &str) -> PathBuf {
        let home = ["~/", "$HOME/", "${HOME}/"].iter().find_map(|prefix| word.strip_prefix(prefix));
        let path = match (home, std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
            _ => crate::cache::absolutize(&self.base.join(word)),
        };
        path.canonicalize().unwrap_or(path)
    }

    /// Whether `word` names a file, or with `folders` a folder too. A glob
    /// names what the folder before its first wildcard holds.
    fn names(&self, word: &str, folders: bool) -> bool {
        match word.find(['*', '?', '[']) {
            Some(glob) => self.path(&format!("{}x", &word[..glob])).parent().is_some_and(Path::is_dir),
            None => {
                let path = self.path(word);
                path.is_file() || (folders && path.is_dir())
            }
        }
    }

    /// Whether `word` names a folder, written without a wildcard.
    fn is_folder(&self, word: &str) -> bool {
        !is_glob(word) && self.path(word).is_dir()
    }

    fn is_binary(&self, word: &str) -> bool {
        use std::io::Read;
        let mut head = [0u8; 8000];
        !is_glob(word)
            && std::fs::File::open(self.path(word))
                .and_then(|mut file| file.read(&mut head))
                .is_ok_and(|read| head[..read].contains(&0))
    }

    fn line_count(&self, word: &str) -> Option<usize> {
        use std::io::Read;
        let mut file = std::fs::File::open(self.path(word)).ok()?;
        let mut chunk = vec![0u8; 1 << 16];
        let (mut lines, mut last) = (0, b'\n');
        loop {
            let read = file.read(&mut chunk).ok()?;
            if read == 0 {
                return Some(lines + usize::from(last != b'\n'));
            }
            lines += chunk[..read].iter().filter(|byte| **byte == b'\n').count();
            last = chunk[read - 1];
        }
    }

    /// `trace read` of `files`, every one a text file, within `lines`.
    fn read_files(&self, files: &[String], lines: Option<(usize, usize)>, left_out: Vec<String>) -> Option<Instead> {
        if files.is_empty() || !files.iter().all(|file| self.names(file, false)) || files.iter().any(|file| self.is_binary(file)) {
            return None;
        }
        let mut run = vec!["trace".to_string(), "read".into()];
        run.extend(files.iter().map(|file| path_word(file)));
        if let Some((from, to)) = lines {
            run.extend(["--lines".into(), format!("{from}:{to}")]);
        }
        Some(Instead::new(run, left_out))
    }

    fn read(&self, program: &str, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["--paging", "--style", "--color", "--theme", "-l", "--language", "-r", "--line-range", "-H", "--highlight-line"]);
        let mut lines = None;
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match (program, flag.as_str()) {
                ("cat", "-n") => {}
                ("bat", "-p" | "--plain" | "-n" | "--number" | "-P" | "--no-paging" | "--paging" | "--style" | "--color" | "--theme" | "-l" | "--language") => {}
                ("bat", "-r" | "--line-range") if lines.is_none() => match value.as_deref().and_then(range) {
                    Some(range) => lines = Some(range),
                    None => left_out.push(flag_text(flag, value)),
                },
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        self.read_files(&parsed.words, lines, left_out)
    }

    fn head(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["-n", "--lines", "-c", "--bytes"]);
        let mut count = Some(10);
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match (flag.as_str(), value.as_deref().and_then(number)) {
                ("-n" | "--lines", Some(lines)) => count = Some(lines),
                (flag, _) if number(&flag[1..]).is_some() => count = number(&flag[1..]),
                ("-n" | "--lines" | "-c" | "--bytes", _) => {
                    count = None;
                    left_out.push(flag_text(flag, value));
                }
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        let lines = match count {
            Some(0) => return None,
            count => count.map(|count| (1, count)),
        };
        self.read_files(&parsed.words, lines, left_out)
    }

    fn tail(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["-n", "--lines", "-c", "--bytes", "-s", "--sleep-interval", "--pid"]);
        let mut window = Some(Tail::Last(10));
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-f" | "-F" | "--follow" | "--retry" => return None,
                "-n" | "--lines" => match value.as_deref().and_then(tail_window) {
                    Some(given) => window = Some(given),
                    None => {
                        window = None;
                        left_out.push(flag_text(flag, value));
                    }
                },
                flag if number(&flag[1..]).is_some() => window = number(&flag[1..]).map(Tail::Last),
                "-c" | "--bytes" => {
                    window = None;
                    left_out.push(flag_text(flag, value));
                }
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        let lines = match (window, parsed.words.as_slice()) {
            (Some(window), [file]) if self.names(file, false) && !is_glob(file) => {
                let total = self.line_count(file)?;
                let from = match window {
                    Tail::Last(count) => total.saturating_sub(count) + 1,
                    Tail::From(line) => line.max(1),
                };
                (from <= total).then_some((from, total))
            }
            (Some(Tail::Last(10)), _) => None,
            (Some(Tail::Last(count)), _) => {
                left_out.push(format!("-n {count}"));
                None
            }
            (Some(Tail::From(line)), _) => {
                left_out.push(format!("-n +{line}"));
                None
            }
            (None, _) => None,
        };
        self.read_files(&parsed.words, lines, left_out)
    }

    fn sed(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["-e", "--expression", "-f", "--file", "-l", "--line-length"]);
        let mut quiet = false;
        let mut scripts = Vec::new();
        let mut from_file = false;
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match (flag.as_str(), value) {
                ("-i" | "--in-place", _) => return None,
                ("-n" | "--quiet" | "--silent", _) => quiet = true,
                ("-e" | "--expression", Some(script)) => scripts.push(script.clone()),
                ("-f" | "--file", _) => {
                    from_file = true;
                    left_out.push(flag_text(flag, value));
                }
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        let mut words = parsed.words.into_iter();
        if scripts.is_empty() && !from_file {
            scripts.push(words.next()?);
        }
        let files: Vec<String> = words.collect();
        let last = || match files.as_slice() {
            [file] => self.line_count(file),
            _ => None,
        };
        let lines = (quiet && files.len() == 1).then(|| printed_lines(&scripts, last)).flatten();
        if lines.is_none() {
            left_out.extend(scripts.iter().map(|script| shell::quote(script)));
        }
        self.read_files(&files, lines, left_out)
    }

    fn awk(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["-F", "-v", "-f", "-i", "--include"]);
        if parsed.flags.iter().any(|(flag, value)| matches!(flag.as_str(), "-i" | "--include") && value.as_deref() == Some("inplace")) {
            return None;
        }
        let mut left_out: Vec<String> = parsed.flags.iter().map(|(flag, value)| flag_text(flag, value)).collect();
        let mut words = parsed.words.into_iter();
        if !parsed.flags.iter().any(|(flag, _)| flag == "-f") {
            let program = words.next()?;
            if writes_a_file(&program) {
                return None;
            }
            left_out.push(shell::quote(&program));
        }
        let files: Vec<String> = words.filter(|word| !word.contains('=')).collect();
        self.read_files(&files, None, left_out)
    }

    /// `grep`, `egrep`, `fgrep`, `rg`, and `git grep` as the `trace grep`
    /// that matches the same lines.
    fn search(&self, program: &str, args: &[String]) -> Option<Instead> {
        let rg = program == "rg";
        let git = program == "git";
        let grep = !rg && !git;
        let valued: &[&str] = match rg {
            true => &[
                "-e", "--regexp", "-g", "--glob", "--iglob", "-t", "--type", "-T", "--type-not", "--type-add", "-A", "--after-context", "-B",
                "--before-context", "-C", "--context", "-M", "--max-columns", "-m", "--max-count", "-d", "--max-depth", "-r", "--replace", "-j",
                "--threads", "-E", "--encoding", "-f", "--file", "--max-filesize", "--sort", "--sortr", "--engine", "--colors", "--path-separator",
            ],
            false => &[
                "-e", "--regexp", "-A", "--after-context", "-B", "--before-context", "-C", "--context", "--include", "--exclude", "--exclude-dir",
                "-m", "--max-count", "-f", "--file", "-d", "--directories", "-D", "--devices", "--binary-files", "--label",
            ],
        };
        let parsed = Parsed::of(args, valued);
        if rg && parsed.flags.iter().any(|(flag, _)| flag == "--files") {
            return self.files(parsed);
        }
        let mut fixed = program == "fgrep";
        let mut extended = rg || program == "egrep";
        let mut recursive = !grep;
        let mut smart = false;
        let mut patterns = Vec::new();
        let mut flags: Vec<String> = Vec::new();
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match (flag.as_str(), value) {
                ("-q" | "--quiet" | "--silent" | "-f" | "--file", _) => return None,
                ("-e" | "--regexp", Some(value)) => patterns.push(value.clone()),
                ("-F" | "--fixed-strings", _) => fixed = true,
                ("-E" | "--extended-regexp", _) if !rg => extended = true,
                ("-r" | "--recursive" | "-R" | "--dereference-recursive", _) if grep => recursive = true,
                ("-S" | "--smart-case", _) if rg => smart = true,
                ("-n" | "--line-number" | "-H" | "--with-filename" | "--color" | "--colour" | "-s" | "--no-messages", _) => {}
                ("-N" | "--no-line-number" | "--heading" | "--no-heading" | "--column" | "--hidden" | "-M" | "--max-columns", _) if rg => {}
                ("-I", _) if git => {}
                ("-i" | "--ignore-case", _) => flags.push("-i".into()),
                ("-o" | "--only-matching", _) => flags.push("-o".into()),
                ("-v" | "--invert-match", _) => flags.push("-v".into()),
                ("-w" | "--word-regexp", _) => flags.push("-w".into()),
                ("-x" | "--line-regexp", _) => flags.push("-x".into()),
                ("-l" | "--files-with-matches", _) => flags.push("-l".into()),
                ("-c" | "--count", _) => flags.push("-c".into()),
                ("-U" | "--multiline", _) if rg => flags.push("-U".into()),
                ("-A" | "--after-context", Some(value)) if number(value).is_some() => flags.extend(["-A".into(), value.clone()]),
                ("-B" | "--before-context", Some(value)) if number(value).is_some() => flags.extend(["-B".into(), value.clone()]),
                ("-C" | "--context", Some(value)) if number(value).is_some() => flags.extend(["-C".into(), value.clone()]),
                ("-g" | "--glob", Some(value)) if rg => flags.extend(["-g".into(), value.clone()]),
                ("-t" | "--type", Some(value)) if rg => flags.extend(["-t".into(), value.clone()]),
                ("--include", Some(value)) if grep => flags.extend(["-g".into(), value.clone()]),
                ("--exclude" | "--exclude-dir", Some(value)) if grep => flags.extend(["-g".into(), format!("!{value}")]),
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        // grep's and git grep's `-c` counts the lines that match, whatever
        // `-o` says; ripgrep's `-c -o` counts the matches, as trace's does.
        if !rg && flags.iter().any(|flag| flag == "-c") {
            flags.retain(|flag| flag != "-o");
        }
        let mut words = parsed.words.into_iter();
        if patterns.is_empty() {
            patterns.push(words.next()?);
        }
        let translated: Option<Vec<String>> = patterns
            .iter()
            .map(|pattern| match (fixed, extended) {
                (true, _) => Some(regex::escape(pattern)),
                (false, true) if rg => Some(pattern.clone()),
                (false, true) => (!has_backreference(pattern)).then(|| pattern.clone()),
                (false, false) => basic_as_extended(pattern),
            })
            .collect();
        let pattern = translated?.join("|");
        if smart && !has_uppercase_literal(&pattern) {
            flags.push("-i".into());
        }
        let mut paths: Vec<String> = words.collect();
        // git's pathspec glob matches at any depth, as ripgrep's `-g` matches
        // a glob with no folder in it, and a word that names a commit is the
        // revision it searches.
        if git {
            let (globs, named): (Vec<String>, Vec<String>) = paths.into_iter().partition(|path| is_glob(path));
            if globs.iter().any(|glob| glob.contains('/')) {
                return None;
            }
            flags.extend(globs.into_iter().flat_map(|glob| ["-g".to_string(), glob]));
            let (revisions, named): (Vec<String>, Vec<String>) = named.into_iter().partition(|word| !self.names(word, true) && self.is_commit(word));
            let mut revisions = revisions.into_iter();
            if let Some(revision) = revisions.next() {
                flags.extend(["--revision".to_string(), revision]);
                left_out.extend(revisions.map(|revision| shell::quote(&revision)));
            }
            paths = named;
        }
        if paths.is_empty() {
            if !recursive || (self.piped && !git) {
                return None;
            }
            paths.push(".".into());
        }
        if !paths.iter().all(|path| self.names(path, recursive)) || paths.iter().all(|path| self.is_binary(path)) {
            return None;
        }
        let mut run = vec!["trace".to_string(), "grep".into()];
        run.extend(flags.iter().map(|flag| shell::quote(flag)));
        if pattern.starts_with('-') {
            run.push("-e".into());
        }
        run.push(shell::quote(&pattern));
        run.extend(paths.iter().map(|path| path_word(path)));
        Some(Instead::new(run, left_out))
    }

    fn files(&self, parsed: Parsed) -> Option<Instead> {
        let mut pattern = None;
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match (flag.as_str(), value) {
                ("--files", _) => {}
                ("-g" | "--glob", Some(glob)) if pattern.is_none() => pattern = Some(glob.clone()),
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        let bases = match parsed.words.is_empty() {
            true => vec![".".to_string()],
            false => parsed.words,
        };
        if !bases.iter().all(|base| self.is_folder(base)) {
            return None;
        }
        let mut run = vec!["trace".to_string(), "find".into(), shell::quote(pattern.as_deref().unwrap_or("*"))];
        run.extend(bases.iter().map(|base| shell::quote(base)));
        Some(Instead::new(run, left_out))
    }

    fn is_commit(&self, word: &str) -> bool {
        crate::cache::worktree_root_for(&self.path(".")).is_some_and(|root| {
            crate::git_activity::git_output(&root, ["rev-parse", "--verify", "--quiet", &format!("{word}^{{commit}}")])
                .is_ok_and(|output| output.status.success())
        })
    }

    /// `ls` and `eza` as `trace list`, a recursive or tree listing as `trace
    /// tree`, and a listing of files as `trace info`.
    fn list(&self, program: &str, args: &[String]) -> Option<Instead> {
        let eza = program == "eza";
        let valued: &[&str] = match eza {
            true => &["-L", "--level", "-s", "--sort", "-t", "--time", "--time-style", "-I", "--ignore-glob"],
            false => &["-I", "--ignore", "-w", "--width", "--hide", "--sort", "--time-style"],
        };
        let parsed = Parsed::of(args, valued);
        let mut all = false;
        let mut tree = false;
        let mut recent = false;
        let mut depth = None;
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-a" | "-A" | "--all" | "--almost-all" => all = true,
                "-T" | "--tree" if eza => tree = true,
                "-R" | "--recurse" | "--recursive" => tree = true,
                "-L" | "--level" if eza => match value.as_deref().and_then(number) {
                    Some(level) => depth = Some(level),
                    None => left_out.push(flag_text(flag, value)),
                },
                "-t" if !eza => recent = true,
                "-s" | "--sort" if matches!(value.as_deref(), Some("modified" | "date" | "time" | "newest" | "mod")) => recent = true,
                "-l" | "-h" | "-1" | "-F" | "-G" | "--long" | "--header" | "--oneline" | "--classify" | "--grid" | "--color" | "--colour" => {}
                "-p" | "-o" | "-g" | "-C" if !eza => {}
                "-t" | "--time" | "--time-style" | "--git" | "--icons" | "--group-directories-first" if eza => {}
                flag if eza && flag.starts_with("--no-") => {}
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        let words = match parsed.words.is_empty() {
            true => vec![".".into()],
            false => parsed.words,
        };
        if tree {
            return self.tree_of(words, depth, all, left_out);
        }
        if let Some(depth) = depth {
            left_out.push(format!("-L {depth}"));
        }
        self.list_of(words, all, recent, left_out)
    }

    fn tree(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["-L", "-P", "-I", "--charset", "--filelimit", "-o"]);
        let mut depth = None;
        let mut all = false;
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-o" => return None,
                "-L" => match value.as_deref().and_then(number) {
                    Some(level) => depth = Some(level),
                    None => left_out.push(flag_text(flag, value)),
                },
                "-a" => all = true,
                "-F" | "-C" | "-n" | "-h" | "--noreport" => {}
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        self.tree_of(parsed.words, depth, all, left_out)
    }

    /// `trace list` of `words` when each is a folder, `trace find` of one
    /// glob, and `trace info` when each is a file or a folder.
    fn list_of(&self, words: Vec<String>, all: bool, recent: bool, left_out: Vec<String>) -> Option<Instead> {
        if words.iter().all(|word| self.is_folder(word)) {
            let mut run = vec!["trace".to_string(), "list".into()];
            run.extend(words.iter().map(|word| shell::quote(word)));
            run.extend(all.then(|| "--all".to_string()));
            run.extend(recent.then(|| "--recent".to_string()));
            return Some(Instead::new(run, left_out));
        }
        if let [glob] = words.as_slice() {
            if is_glob(glob) {
                let (folder, pattern) = glob.rsplit_once('/').unwrap_or((".", glob));
                let folder = if folder.is_empty() { "/" } else { folder };
                return self
                    .is_folder(folder)
                    .then(|| Instead::new(vec!["trace".into(), "find".into(), shell::quote(pattern), shell::quote(folder)], left_out));
            }
        }
        if !words.iter().all(|word| !is_glob(word) && self.names(word, true)) {
            return None;
        }
        let mut run = vec!["trace".to_string(), "info".into()];
        run.extend(words.iter().map(|word| shell::quote(word)));
        Some(Instead::new(run, left_out))
    }

    /// A tree of one folder: one level deep is the folder's own entries,
    /// `trace list`'s answer, and deeper is `trace tree`'s. A folder git
    /// ignores is left alone, because `trace tree` lists it empty.
    fn tree_of(&self, words: Vec<String>, depth: Option<usize>, all: bool, mut left_out: Vec<String>) -> Option<Instead> {
        let folder = match words.as_slice() {
            [] => ".".to_string(),
            [folder] => folder.clone(),
            _ => return None,
        };
        if depth == Some(1) {
            return self.list_of(vec![folder], all, false, left_out);
        }
        if !self.is_folder(&folder) || self.is_ignored(&folder) {
            return None;
        }
        if all {
            left_out.push("-a".into());
        }
        let mut run = vec!["trace".to_string(), "tree".into(), shell::quote(&folder)];
        if let Some(depth) = depth {
            run.extend(["--max-depth".into(), depth.to_string()]);
        }
        Some(Instead::new(run, left_out))
    }

    fn is_ignored(&self, word: &str) -> bool {
        let path = self.path(word);
        let Some(root) = crate::cache::worktree_root_for(&path) else {
            return false;
        };
        crate::git_activity::git_output(&root, [std::ffi::OsStr::new("check-ignore"), std::ffi::OsStr::new("-q"), path.as_os_str()])
            .is_ok_and(|output| output.status.success())
    }

    /// `find` as `trace find` of its name or path pattern. A `find` that runs
    /// a command on what it finds, or deletes it, is left alone, and the
    /// command it runs is read on its own.
    fn find(&self, args: &[String]) -> Option<Instead> {
        let start = args.iter().position(|arg| !matches!(arg.as_str(), "-L" | "-H" | "-P")).unwrap_or(args.len());
        let args = &args[start..];
        let end = args.iter().position(|arg| arg.starts_with('-') || arg == "(" || arg == "!").unwrap_or(args.len());
        let bases = match &args[..end] {
            [] => vec![".".to_string()],
            bases => bases.to_vec(),
        };
        if !bases.iter().all(|base| self.is_folder(base)) {
            return None;
        }
        let mut name = None;
        let mut folders = false;
        let mut left_out = Vec::new();
        let mut rest = args[end..].iter().peekable();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprint0" | "-fprintf" | "-fls" => return None,
                test @ ("-name" | "-iname" | "-path" | "-ipath" | "-wholename" | "-iwholename") if name.is_none() => {
                    let pattern = rest.next()?;
                    name = Some(pattern.strip_prefix("./").unwrap_or(pattern).to_string());
                    if test.starts_with("-i") {
                        left_out.push(format!("{test}, which ignores case"));
                    }
                }
                "-prune" => {
                    if let Some(pruned) = name.take() {
                        left_out.push(format!("{} -prune", shell::quote(&pruned)));
                    }
                }
                "-type" => match rest.next()?.as_str() {
                    "f" => {}
                    "d" => folders = true,
                    other => left_out.push(format!("-type {other}")),
                },
                "-print" | "-print0" => {}
                flag => {
                    let value = rest.next_if(|next| !next.starts_with('-') && !matches!(next.as_str(), "(" | ")" | "!"));
                    left_out.push(match value {
                        Some(value) => format!("{} {}", shell::quote(flag), shell::quote(value)),
                        None => shell::quote(flag),
                    });
                }
            }
        }
        let mut run = vec!["trace".to_string(), "find".into(), shell::quote(name.as_deref().unwrap_or("*"))];
        run.extend(bases.iter().map(|base| shell::quote(base)));
        if folders {
            run.extend(["--type".into(), "d".into()]);
        }
        Some(Instead::new(run, left_out))
    }

    fn git(&self, args: &[String]) -> Option<Instead> {
        let mut at = 0;
        let mut base = self.base.clone();
        let mut folder: Option<PathBuf> = None;
        while let Some(arg) = args.get(at) {
            match arg.as_str() {
                "-C" => {
                    let dir = args.get(at + 1)?;
                    base = base.join(dir);
                    folder = Some(folder.map_or_else(|| PathBuf::from(dir), |folder| folder.join(dir)));
                    at += 2;
                }
                "-c" => {
                    args.get(at + 1)?;
                    at += 2;
                }
                "--no-pager" | "-P" | "--paginate" | "--no-replace-objects" | "--literal-pathspecs" | "--no-optional-locks" => at += 1,
                flag if flag.starts_with('-') => return None,
                _ => break,
            }
        }
        let (subcommand, rest) = args[at..].split_first()?;
        let scope = Scope { base, piped: self.piped };
        let instead = match subcommand.as_str() {
            "blame" | "annotate" => scope.blame(rest),
            "grep" => scope.search("git", rest),
            "show" => scope.show(rest),
            "cat-file" => scope.cat_file(rest),
            "log" => scope.log(rest),
            "diff" => scope.diff(rest),
            "status" => scope.status(rest),
            _ => None,
        }?;
        Some(match folder {
            Some(folder) => instead.under(&folder.to_string_lossy()),
            None => instead,
        })
    }

    fn blame(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["-L", "-S", "--contents", "--ignore-rev", "--ignore-revs-file"]);
        let mut lines = None;
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match (flag.as_str(), value.as_deref().and_then(|value| value.split_once(','))) {
                ("-L", Some((from, to))) if lines.is_none() && number(from).is_some() && number(to).is_some() => {
                    lines = Some(format!("{from}:{to}"));
                }
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        let (file, revisions) = parsed.words.split_last()?;
        if is_glob(file) || !self.names(file, false) {
            return None;
        }
        left_out.extend(revisions.iter().map(|revision| shell::quote(revision)));
        let mut run = vec!["trace".to_string(), "blame".into(), shell::quote(file)];
        if let Some(lines) = lines {
            run.extend(["--lines".into(), lines]);
        }
        Some(Instead::new(run, left_out))
    }

    /// `git show <rev>:<file>` as `trace read --revision`, and `git show
    /// <rev>` as the commit in full.
    fn show(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &[]);
        let mut left_out: Vec<String> = parsed.flags.iter().map(|(flag, value)| flag_text(flag, value)).collect();
        let (object, paths) = match parsed.words.split_first() {
            Some((object, paths)) => (object.clone(), paths),
            None => ("HEAD".to_string(), &[][..]),
        };
        left_out.extend(paths.iter().map(|path| shell::quote(path)));
        let run = match object.contains(':') {
            true => self.blob(&object)?,
            false => vec!["trace".into(), "history".into(), "--commit".into(), shell::quote(&object)],
        };
        Some(Instead::new(run, left_out))
    }

    fn cat_file(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &[]);
        match (parsed.flags.as_slice(), parsed.words.as_slice()) {
            ([(flag, None)], [object]) if flag == "-p" && object.contains(':') => Some(Instead::new(self.blob(object)?, Vec::new())),
            _ => None,
        }
    }

    /// `<rev>:<path>` naming a file, whose path git reads from the
    /// repository's root unless it starts with `./` or `../`. `:<path>`,
    /// the staged file, and a folder have no `trace read` answer.
    fn blob(&self, object: &str) -> Option<Vec<String>> {
        let (revision, file) = object.split_once(':')?;
        if revision.is_empty() || file.is_empty() {
            return None;
        }
        let root = crate::cache::worktree_root_for(&self.path("."))?;
        let path = match file.starts_with("./") || file.starts_with("../") {
            true => self.base.join(file),
            false => root.join(file),
        };
        let within = crate::cache::relative_to_root(&path, &root);
        let kind = crate::git_activity::git_output(&root, ["cat-file", "-t", &format!("{revision}:{within}")]).ok()?;
        (kind.status.success() && kind.stdout == b"blob\n")
            .then(|| vec!["trace".into(), "read".into(), shell::quote(&path.to_string_lossy()), "--revision".into(), shell::quote(revision)])
    }

    /// `git log` of the repository, of one file, of a function (`-L
    /// :<name>:<file>`), or of the commits that change how often a string
    /// occurs (`-S`, `-G`).
    fn log(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["-S", "-G", "-L", "-n", "--max-count", "--since", "--until", "--author", "--grep", "--skip", "--date"]);
        let mut contains = None;
        let mut function = None;
        let mut left_out = Vec::new();
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-S" if contains.is_none() && value.is_some() => contains = value.clone().map(|text| (text, false)),
                "-G" if contains.is_none() && value.is_some() => contains = value.clone().map(|text| (text, true)),
                "-L" if function.is_none() && value.is_some() => function = value.clone(),
                "--follow" => {}
                _ => left_out.push(flag_text(flag, value)),
            }
        }
        let (files, others): (Vec<&String>, Vec<&String>) = parsed.words.iter().partition(|word| !is_glob(word) && self.names(word, false));
        left_out.extend(others.iter().map(|word| shell::quote(word)));
        let traced = function.as_deref().and_then(|function| {
            let (name, file) = function.strip_prefix(':')?.split_once(':')?;
            (!name.is_empty() && !is_glob(file) && self.names(file, false)).then(|| (name.to_string(), file.to_string()))
        });
        let run = match (contains, traced) {
            (Some((text, regex)), _) => {
                left_out.extend(function.iter().map(|function| format!("-L {}", shell::quote(function))));
                left_out.extend(files.iter().map(|file| shell::quote(file)));
                let mut run = vec!["trace".to_string(), "history".into(), "--contains".into(), shell::quote(&text)];
                run.extend(regex.then(|| "--regex".to_string()));
                run
            }
            (None, Some((name, file))) => {
                left_out.extend(files.iter().map(|file| shell::quote(file)));
                vec!["trace".into(), "history".into(), shell::quote(&file), shell::quote(&name)]
            }
            (None, None) => {
                left_out.extend(function.iter().map(|function| format!("-L {}", shell::quote(function))));
                let mut run = vec!["trace".to_string(), "history".into()];
                if let Some((file, rest)) = files.split_first() {
                    run.push(shell::quote(file));
                    left_out.extend(rest.iter().map(|file| shell::quote(file)));
                }
                run
            }
        };
        Some(Instead::new(run, left_out))
    }

    fn status(&self, args: &[String]) -> Option<Instead> {
        let parsed = Parsed::of(args, &["--column", "--ignored", "--untracked-files", "-u"]);
        let mut left_out: Vec<String> = parsed
            .flags
            .iter()
            .filter(|(flag, _)| !matches!(flag.as_str(), "-s" | "--short" | "-b" | "--branch" | "--porcelain" | "--long"))
            .map(|(flag, value)| flag_text(flag, value))
            .collect();
        left_out.extend(parsed.words.iter().map(|path| shell::quote(path)));
        crate::cache::worktree_root_for(&self.path(".")).map(|_| Instead::new(vec!["trace".into(), "status".into()], left_out))
    }

    fn diff(&self, args: &[String]) -> Option<Instead> {
        let split = args.iter().position(|arg| arg == "--").unwrap_or(args.len());
        let parsed = Parsed::of(&args[..split], &["-S", "-G", "-O", "--diff-filter"]);
        if parsed.flags.iter().any(|(flag, _)| matches!(flag.as_str(), "--no-index" | "--quiet")) {
            return None;
        }
        let root = crate::cache::worktree_root_for(&self.path("."))?;
        let mut left_out: Vec<String> = parsed.flags.iter().map(|(flag, value)| flag_text(flag, value)).collect();
        let (mut paths, revisions): (Vec<String>, Vec<String>) = parsed.words.into_iter().partition(|word| self.names(word, true));
        paths.extend(args.get(split + 1..).unwrap_or(&[]).iter().cloned());
        if !paths.iter().all(|path| self.path(path).starts_with(&root)) {
            return None;
        }
        let mut revisions = revisions.into_iter();
        let base = revisions.next();
        left_out.extend(revisions.map(|revision| shell::quote(&revision)));
        let mut run = vec!["trace".to_string(), "diff".into()];
        run.extend(paths.iter().map(|path| path_word(path)));
        match base {
            Some(base) if !base.contains("..") => run.extend(["--base".into(), shell::quote(&base)]),
            Some(base) => left_out.push(shell::quote(&base)),
            None => {}
        }
        Some(Instead::new(run, left_out))
    }
}

fn writes_a_file(program: &str) -> bool {
    use std::sync::OnceLock;
    static PRINT_TO: OnceLock<regex::Regex> = OnceLock::new();
    PRINT_TO.get_or_init(|| regex::Regex::new(r"\bprintf?\b[^;}]*>").unwrap()).is_match(program)
}

fn printed_lines(scripts: &[String], last: impl Fn() -> Option<usize>) -> Option<(usize, usize)> {
    let mut span: Option<(usize, usize)> = None;
    for part in scripts.iter().flat_map(|script| script.split([';', '\n'])).map(str::trim).filter(|part| !part.is_empty()) {
        let printed = part.strip_suffix('p')?;
        let (from, to) = printed.split_once(',').unwrap_or((printed, printed));
        let line = |text: &str| match text {
            "$" => last(),
            text => number(text).filter(|line| *line >= 1),
        };
        let (from, to) = (line(from)?, line(to)?);
        if to < from {
            return None;
        }
        span = Some(span.map_or((from, to), |(first, end)| (first.min(from), end.max(to))));
    }
    span
}

fn range(text: &str) -> Option<(usize, usize)> {
    let (from, to) = text.split_once(':')?;
    Some((number(from)?, number(to)?))
}

fn tail_window(text: &str) -> Option<Tail> {
    match text.strip_prefix('+') {
        Some(line) => number(line).map(Tail::From),
        None => number(text).map(Tail::Last),
    }
}

/// `text` as a count or a line number: digits alone, so `+3`, which `sed`
/// and `git blame` read as relative to the line before it, is none.
fn number(text: &str) -> Option<usize> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())).then(|| text.parse().ok())?
}

fn is_glob(word: &str) -> bool {
    word.contains(['*', '?', '['])
}

/// A path for the `trace` line: a glob bare, so the shell expands it as it
/// would have for the refused command, anything else quoted.
fn path_word(word: &str) -> String {
    match is_glob(word) && word.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_*?[]".contains(c)) {
        true => word.to_string(),
        false => shell::quote(word),
    }
}

/// Whether an extended expression refers back to a group, which ripgrep
/// has no answer for.
fn has_backreference(pattern: &str) -> bool {
    pattern.as_bytes().windows(2).any(|pair| pair[0] == b'\\' && pair[1].is_ascii_digit())
}

/// Whether `pattern` holds an uppercase letter outside an escape, the rule
/// ripgrep's `--smart-case` reads, so `\W` holds none.
fn has_uppercase_literal(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            c if c.is_uppercase() => return true,
            _ => {}
        }
    }
    false
}

/// A basic regular expression, `grep`'s default, as the extended one
/// ripgrep reads: `\(`, `\)`, `\|`, `\+`, `\?`, `\{`, and `\}` are operators
/// in the one, and the same characters bare are operators in the other. A
/// `*` that starts the expression, or follows `^`, `\(`, or `\|`, repeats
/// nothing and is a literal. A back-reference is `None`.
fn basic_as_extended(basic: &str) -> Option<String> {
    const SWAPPED: &str = "()|+?{}";
    let mut extended = String::new();
    let mut at_start = true;
    let mut chars = basic.chars();
    while let Some(c) = chars.next() {
        let starts = match c {
            '\\' => match chars.next() {
                Some(next) if next.is_ascii_digit() => return None,
                Some(next) if SWAPPED.contains(next) => {
                    extended.push(next);
                    next == '(' || next == '|'
                }
                Some(next) => {
                    extended.extend(['\\', next]);
                    false
                }
                None => {
                    extended.push_str("\\\\");
                    false
                }
            },
            '*' if at_start => {
                extended.push_str("\\*");
                false
            }
            c if SWAPPED.contains(c) => {
                extended.extend(['\\', c]);
                false
            }
            '^' => {
                extended.push('^');
                at_start
            }
            c => {
                extended.push(c);
                false
            }
        };
        at_start = starts;
    }
    Some(extended)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_basic_expression_reads_as_the_extended_one_ripgrep_takes() {
        assert_eq!(basic_as_extended(r"foo\|bar(x)").unwrap(), r"foo|bar\(x\)");
        assert_eq!(basic_as_extended(r"a\{2\}b+").unwrap(), r"a{2}b\+");
        assert_eq!(basic_as_extended(r"\.rs$").unwrap(), r"\.rs$");
        assert_eq!(basic_as_extended("*const").unwrap(), r"\*const");
        assert_eq!(basic_as_extended(r"^*a\|*b").unwrap(), r"^\*a|\*b");
        assert_eq!(basic_as_extended("a*").unwrap(), "a*");
        assert!(basic_as_extended(r"\(a\)\1").is_none());
    }

    #[test]
    fn smart_case_reads_only_the_letters_outside_an_escape() {
        assert!(!has_uppercase_literal(r"\Wparsed"));
        assert!(has_uppercase_literal("Parsed"));
    }

    #[test]
    fn a_number_is_digits_alone() {
        assert_eq!(number("12"), Some(12));
        assert_eq!(number("+3"), None);
        assert_eq!(number(""), None);
    }

    #[test]
    fn short_flags_split_and_carry_their_values() {
        let args: Vec<String> = ["-rnC3", "--include=*.rs", "-e", "x", "--", "-y"].iter().map(|arg| arg.to_string()).collect();
        let parsed = Parsed::of(&args, &["-C", "-e", "--include"]);
        assert_eq!(
            parsed.flags,
            vec![
                ("-r".into(), None),
                ("-n".into(), None),
                ("-C".into(), Some("3".into())),
                ("--include".into(), Some("*.rs".into())),
                ("-e".into(), Some("x".into())),
            ]
        );
        assert_eq!(parsed.words, vec!["-y".to_string()]);
    }

    #[test]
    fn sed_prints_the_lines_its_p_commands_name() {
        let scripts = |text: &str| vec![text.to_string()];
        assert_eq!(printed_lines(&scripts("36,42p;380,384p"), || None), Some((36, 384)));
        assert_eq!(printed_lines(&scripts("5,$p"), || Some(9)), Some((5, 9)));
        assert_eq!(printed_lines(&scripts("2,+3p"), || None), None);
        assert_eq!(printed_lines(&scripts("s/a/b/p"), || None), None);
    }
}
