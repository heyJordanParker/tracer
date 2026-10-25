//! `--tracer-only`: a shell command that reads, searches, or lists a file or
//! folder without tracer is refused when a `trace` command gives Claude the
//! same answer, and the refusal names that command. So is `trace` output
//! piped into a filter, which throws its context away.
//!
//! A command tracer has no answer for is left alone: one with a flag no
//! `trace` command matches, such as `tail -f` or `grep -m`, an `awk` program
//! or a `sed` script other than printing lines, a path that is neither a
//! file nor a folder, such as a device, and one whose output or exit status
//! something other than Claude takes: a redirect, a pipe into another
//! program, a command substitution, or a condition. `cat <file> | <reader>`
//! is read as the reader given the file.

use crate::shell::{self, Command};
use std::path::{Path, PathBuf};

/// The programs that read the files they name, whose files a Codex shell
/// read reaches.
pub(super) const READERS: &[&str] = &["cat", "bat", "head", "tail", "sed", "awk"];
const FILTERS: &[&str] = &[
    "grep", "egrep", "fgrep", "rg", "sed", "awk", "head", "tail", "cut", "sort", "uniq", "wc", "column", "fold", "tr", "jq",
];

/// The refusal for `line` run in `cwd`, `None` when it reads nothing raw
/// that tracer answers the same.
pub fn refusal(line: &str, cwd: &Path) -> Option<String> {
    let commands = shell::commands(line);
    let feeds = |at: usize| commands.iter().any(|other| other.input == Some(at));
    commands.iter().enumerate().find_map(|(at, command)| {
        let source = command.input.and_then(|at| commands.get(at));
        let base = cwd.join(&command.folder);
        let replacement = match source {
            Some(trace) if trace.program() == "trace" && FILTERS.contains(&command.program()) => {
                Some(filtered(&trace.argv, command.program(), command.args()))
            }
            // A command `xargs` runs gets its paths from its input, so no
            // path given is not the working folder.
            _ if command.fed || command.redirected || feeds(at) => None,
            Some(source) => match plain_file(source) {
                Some(file) => {
                    let mut given = command.clone();
                    given.argv.push(file);
                    Scope { base, piped: false }.replacement(&given)
                }
                None => Scope { base, piped: true }.replacement(command),
            },
            None => Scope { base, piped: false }.replacement(command),
        };
        replacement.map(|run| format!("tracer: Claude reads files only through tracer.\nRun this instead: {run}"))
    })
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

struct Scope {
    base: PathBuf,
    /// Whether the command reads another command's output, so a search given
    /// no path searches that output, not the folder.
    piped: bool,
}

impl Scope {
    fn replacement(&self, command: &Command) -> Option<String> {
        let args = command.args();
        match command.program() {
            program @ ("cat" | "bat") => self.read(program, args),
            "head" => self.head(args),
            "sed" => self.sed(args),
            program @ ("grep" | "egrep" | "fgrep" | "rg") => self.search(program, args),
            program @ ("ls" | "eza") => self.list(program, args),
            "tree" => self.tree(args),
            "find" => self.find(args),
            "git" => self.git(args),
            _ => None,
        }
    }

    fn path(&self, word: &str) -> PathBuf {
        let path = crate::cache::absolutize(&self.base.join(word));
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

    /// `trace read --raw` of `files`, every one a file, within `lines`:
    /// the text as it is, with no line `trace read` would clean away.
    fn read_files(&self, files: &[String], lines: Option<(usize, usize)>) -> Option<String> {
        if files.is_empty() || !files.iter().all(|file| self.names(file, false)) {
            return None;
        }
        let mut run = vec!["trace".to_string(), "read".into()];
        run.extend(files.iter().map(|file| path_word(file)));
        run.push("--raw".into());
        if let Some((from, to)) = lines {
            run.extend(["--lines".into(), format!("{from}:{to}")]);
        }
        Some(run.join(" "))
    }

    fn read(&self, program: &str, args: &[String]) -> Option<String> {
        let parsed = Parsed::of(args, &["--paging", "--style", "--color", "--theme", "-l", "--language", "-r", "--line-range"]);
        let mut lines = None;
        for (flag, value) in &parsed.flags {
            match (program, flag.as_str()) {
                ("cat", "-n") => {}
                ("bat", "-p" | "--plain" | "-n" | "--number" | "-P" | "--no-paging" | "--paging" | "--style" | "--color" | "--theme" | "-l" | "--language") => {}
                ("bat", "-r" | "--line-range") if lines.is_none() => {
                    let (from, to) = value.as_deref()?.split_once(':')?;
                    lines = Some((number(from)?, number(to)?));
                }
                _ => return None,
            }
        }
        self.read_files(&parsed.words, lines)
    }

    fn head(&self, args: &[String]) -> Option<String> {
        let parsed = Parsed::of(args, &["-n", "--lines"]);
        let mut count = 10;
        for (flag, value) in &parsed.flags {
            count = match flag.as_str() {
                "-n" | "--lines" => number(value.as_deref()?)?,
                _ => number(&flag[1..])?,
            };
        }
        (count > 0).then(|| self.read_files(&parsed.words, Some((1, count))))?
    }

    /// `sed -n 'Ap'` and `sed -n 'A,Bp'` on one file, the `sed` scripts
    /// that print lines as they are.
    fn sed(&self, args: &[String]) -> Option<String> {
        let parsed = Parsed::of(args, &["-e", "--expression"]);
        let mut quiet = false;
        let mut scripts = Vec::new();
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-n" | "--quiet" | "--silent" => quiet = true,
                "-e" | "--expression" => scripts.push(value.clone()?),
                _ => return None,
            }
        }
        let mut words = parsed.words.into_iter();
        let script = match scripts.as_slice() {
            [] => words.next()?,
            [script] => script.clone(),
            _ => return None,
        };
        let [file]: [String; 1] = words.collect::<Vec<_>>().try_into().ok()?;
        let printed = script.strip_suffix('p').filter(|_| quiet)?;
        let (from, to) = printed.split_once(',').unwrap_or((printed, printed));
        let (from, to) = (number(from)?, number(to)?);
        (from >= 1 && to >= from).then(|| self.read_files(&[file], Some((from, to))))?
    }

    /// `grep`, `egrep`, `fgrep`, `rg`, and `git grep` as the `trace grep`
    /// that matches the same lines.
    fn search(&self, program: &str, args: &[String]) -> Option<String> {
        let rg = program == "rg";
        let grep = !rg && program != "git";
        let valued: &[&str] = match rg {
            true => &["-e", "--regexp", "-g", "--glob", "-t", "--type", "-A", "--after-context", "-B", "--before-context", "-C", "--context", "-M", "--max-columns"],
            false => &["-e", "--regexp", "-A", "--after-context", "-B", "--before-context", "-C", "--context", "--include", "--exclude", "--exclude-dir"],
        };
        let parsed = Parsed::of(args, valued);
        let mut fixed = program == "fgrep";
        let mut extended = rg || program == "egrep";
        let mut recursive = !grep;
        let mut smart = false;
        let mut patterns = Vec::new();
        let mut flags: Vec<String> = Vec::new();
        for (flag, value) in &parsed.flags {
            match (flag.as_str(), value) {
                ("-e" | "--regexp", Some(value)) => patterns.push(value.clone()),
                ("-F" | "--fixed-strings", _) => fixed = true,
                ("-E" | "--extended-regexp", _) if !rg => extended = true,
                ("-r" | "--recursive", _) if grep => recursive = true,
                ("-S" | "--smart-case", _) if rg => smart = true,
                ("-n" | "--line-number" | "-H" | "--with-filename" | "--color" | "--colour", _) => {}
                ("-N" | "--no-line-number" | "--heading" | "--no-heading" | "--column" | "--hidden" | "-M" | "--max-columns", _) if rg => {}
                ("-I", _) if program == "git" => {}
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
                _ => return None,
            }
        }
        // grep's and git grep's `-c` counts the lines that match, whatever
        // `-o` says; ripgrep's `-c -o` counts the matches, as trace's does.
        if !rg && flags.iter().any(|flag| flag == "-c") {
            flags.retain(|flag| flag != "-o");
        }
        let mut words = parsed.words.into_iter();
        let pattern = match patterns.as_slice() {
            [] => words.next()?,
            [pattern] => pattern.clone(),
            _ => return None,
        };
        let pattern = match (fixed, extended) {
            (true, _) => regex::escape(&pattern),
            (false, true) if rg => pattern,
            (false, true) => (!has_backreference(&pattern)).then_some(pattern)?,
            (false, false) => basic_as_extended(&pattern)?,
        };
        if smart && !has_uppercase_literal(&pattern) {
            flags.push("-i".into());
        }
        let mut paths: Vec<String> = words.collect();
        // git's pathspec glob matches at any depth, a shell's in one folder.
        if program == "git" && paths.iter().any(|path| is_glob(path)) {
            return None;
        }
        if paths.is_empty() {
            if !recursive || (self.piped && program != "git") {
                return None;
            }
            paths.push(".".into());
        }
        if !paths.iter().all(|path| self.names(path, recursive)) {
            return None;
        }
        let mut run = vec!["trace".to_string(), "grep".into()];
        run.extend(flags.iter().map(|flag| shell::quote(flag)));
        if pattern.starts_with('-') {
            run.push("-e".into());
        }
        run.push(shell::quote(&pattern));
        run.extend(paths.iter().map(|path| path_word(path)));
        Some(run.join(" "))
    }

    /// `ls` and `eza` as `trace list`, and `eza --tree` as `tree` reads.
    fn list(&self, program: &str, args: &[String]) -> Option<String> {
        let eza = program == "eza";
        let parsed = Parsed::of(args, if eza { &["-L", "--level"] } else { &[] });
        let mut all = false;
        let mut tree = false;
        let mut depth = None;
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-a" | "-A" | "--all" | "--almost-all" => all = true,
                "-T" | "--tree" if eza => tree = true,
                "-L" | "--level" if eza => depth = Some(number(value.as_deref()?)?),
                "-l" | "-h" | "-1" | "-F" | "-G" | "--long" | "--header" | "--oneline" | "--classify" | "--grid" | "--color" | "--colour" => {}
                "-p" | "-o" | "-g" | "-C" if !eza => {}
                "--git" | "--icons" | "--group-directories-first" if eza => {}
                flag if eza && flag.starts_with("--no-") => {}
                _ => return None,
            }
        }
        if tree {
            return self.tree_of(parsed.words, depth, all);
        }
        if depth.is_some() {
            return None;
        }
        self.list_of(if parsed.words.is_empty() { vec![".".into()] } else { parsed.words }, all)
    }

    fn tree(&self, args: &[String]) -> Option<String> {
        let parsed = Parsed::of(args, &["-L"]);
        let mut depth = None;
        let mut all = false;
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-L" => depth = Some(number(value.as_deref()?)?),
                "-a" => all = true,
                "-F" | "-C" | "-n" | "-h" | "--noreport" => {}
                _ => return None,
            }
        }
        self.tree_of(parsed.words, depth, all)
    }

    /// `trace list` of `folders`, unless one holds a folder `trace list`
    /// leaves out, such as `vendor/`, or with `all` `.git/`.
    fn list_of(&self, folders: Vec<String>, all: bool) -> Option<String> {
        let left_out = crate::repo_files::skip_dirs();
        let hides = |folder: &String| {
            std::fs::read_dir(self.path(folder)).map_or(true, |entries| {
                entries.flatten().any(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    (all || !name.starts_with('.')) && left_out.contains(name.as_str()) && entry.path().is_dir()
                })
            })
        };
        if !folders.iter().all(|folder| self.is_folder(folder) && !hides(folder)) {
            return None;
        }
        let mut run = vec!["trace".to_string(), "list".into()];
        run.extend(folders.iter().map(|folder| shell::quote(folder)));
        run.extend(all.then(|| "--all".to_string()));
        Some(run.join(" "))
    }

    /// A tree of one folder: one level deep is the folder's own entries,
    /// `trace list`'s answer, and every level is `trace tree`'s. `trace
    /// tree` names a folder only when a file sits below it, so a tree cut
    /// at a deeper level is left alone, and so is a folder git ignores,
    /// which it lists empty.
    fn tree_of(&self, words: Vec<String>, depth: Option<usize>, all: bool) -> Option<String> {
        let folder = match words.as_slice() {
            [] => ".".to_string(),
            [folder] => folder.clone(),
            _ => return None,
        };
        match depth {
            Some(1) => self.list_of(vec![folder], all),
            Some(_) => None,
            None if !self.is_folder(&folder) || self.is_ignored(&folder) => None,
            None => Some(format!("trace tree {}", shell::quote(&folder))),
        }
    }

    fn is_ignored(&self, word: &str) -> bool {
        let path = self.path(word);
        let Some(root) = crate::cache::worktree_root_for(&path) else {
            return false;
        };
        crate::git_activity::git_output(&root, [std::ffi::OsStr::new("check-ignore"), std::ffi::OsStr::new("-q"), path.as_os_str()])
            .is_ok_and(|output| output.status.success())
    }

    /// `find` with `-name`, `-type f` or `-type d`, and `-print`. `trace
    /// find` lists files or folders, never both, so a `find` without
    /// `-type` is left alone.
    fn find(&self, args: &[String]) -> Option<String> {
        let end = args.iter().position(|arg| arg.starts_with('-') || arg == "(" || arg == "!").unwrap_or(args.len());
        let bases = match &args[..end] {
            [] => vec![".".to_string()],
            bases => bases.to_vec(),
        };
        if !bases.iter().all(|base| self.is_folder(base)) {
            return None;
        }
        let mut name = None;
        let mut kind = None;
        let mut rest = args[end..].iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "-name" if name.is_none() => name = Some(rest.next()?.clone()),
                "-type" if kind.is_none() => kind = Some(rest.next().filter(|kind| *kind == "f" || *kind == "d")?.clone()),
                "-print" => {}
                _ => return None,
            }
        }
        let mut run = vec!["trace".to_string(), "find".into(), shell::quote(name.as_deref().unwrap_or("*"))];
        run.extend(bases.iter().map(|base| shell::quote(base)));
        if kind? == "d" {
            run.extend(["--type".into(), "d".into()]);
        }
        Some(run.join(" "))
    }

    fn git(&self, args: &[String]) -> Option<String> {
        let (subcommand, rest) = args.split_first()?;
        match subcommand.as_str() {
            "blame" | "annotate" => self.blame(rest),
            "grep" => self.search("git", rest),
            "show" => self.show(rest),
            "cat-file" => self.cat_file(rest),
            "log" => self.log(rest),
            _ => None,
        }
    }

    fn blame(&self, args: &[String]) -> Option<String> {
        let parsed = Parsed::of(args, &["-L"]);
        let mut lines = None;
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-L" if lines.is_none() => {
                    let (from, to) = value.as_deref()?.split_once(',')?;
                    lines = Some(format!("{}:{}", number(from)?, number(to)?));
                }
                _ => return None,
            }
        }
        let [file] = parsed.words.as_slice() else {
            return None;
        };
        if is_glob(file) || !self.names(file, false) {
            return None;
        }
        let mut run = vec!["trace".to_string(), "blame".into(), shell::quote(file)];
        if let Some(lines) = lines {
            run.extend(["--lines".into(), lines]);
        }
        Some(run.join(" "))
    }

    /// `git show <rev>:<file>` as `trace read --revision`, and `git show
    /// <rev>` as the commit in full.
    fn show(&self, args: &[String]) -> Option<String> {
        let parsed = Parsed::of(args, &[]);
        if !parsed.flags.is_empty() {
            return None;
        }
        match parsed.words.as_slice() {
            [] => Some("trace history --commit HEAD".into()),
            [object] => match object.contains(':') {
                true => self.blob(object),
                false => Some(format!("trace history --commit {}", shell::quote(object))),
            },
            _ => None,
        }
    }

    fn cat_file(&self, args: &[String]) -> Option<String> {
        let parsed = Parsed::of(args, &[]);
        match (parsed.flags.as_slice(), parsed.words.as_slice()) {
            ([(flag, None)], [object]) if flag == "-p" && object.contains(':') => self.blob(object),
            _ => None,
        }
    }

    /// `<rev>:<path>` naming a file, whose path git reads from the
    /// repository's root unless it starts with `./` or `../`. `:<path>`,
    /// the staged file, and a folder have no `trace read` answer.
    fn blob(&self, object: &str) -> Option<String> {
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
            .then(|| shell::join(&["trace".into(), "read".into(), path.to_string_lossy().into_owned(), "--revision".into(), revision.into()]))
    }

    /// `git log` of one file, of a function (`-L :<name>:<file>`), or of the
    /// commits that change how often a string occurs (`-S`, `-G`).
    fn log(&self, args: &[String]) -> Option<String> {
        let parsed = Parsed::of(args, &["-S", "-G", "-L", "-n", "--max-count"]);
        let mut contains = None;
        let mut function = None;
        for (flag, value) in &parsed.flags {
            match flag.as_str() {
                "-S" if contains.is_none() => contains = Some((value.clone()?, false)),
                "-G" if contains.is_none() => contains = Some((value.clone()?, true)),
                "-L" if function.is_none() => function = Some(value.clone()?),
                "--oneline" | "--follow" | "--format" | "--pretty" | "-n" | "--max-count" => {}
                flag if number(&flag[1..]).is_some() => {}
                _ => return None,
            }
        }
        match (contains, function, parsed.words.as_slice()) {
            (Some((text, regex)), None, []) => {
                let mut run = vec!["trace".to_string(), "history".into(), "--contains".into(), shell::quote(&text)];
                run.extend(regex.then(|| "--regex".to_string()));
                Some(run.join(" "))
            }
            (None, Some(function), []) => {
                let (name, file) = function.strip_prefix(':')?.split_once(':')?;
                (!name.is_empty() && self.names(file, false) && !is_glob(file))
                    .then(|| shell::join(&["trace".into(), "history".into(), file.into(), name.into()]))
            }
            (None, None, [file]) if !is_glob(file) && self.names(file, false) => {
                Some(shell::join(&["trace".into(), "history".into(), file.clone()]))
            }
            _ => None,
        }
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
}
