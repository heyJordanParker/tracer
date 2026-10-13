//! `--tracer-only`: a shell command that reads this project's code without
//! tracer is refused, naming the `trace` command that answers it. So is
//! `trace` output piped into a filter, which throws its context away.
//!
//! "This project" is the repository the hook's working folder is in. A path
//! outside it, a command that writes (`sed -i`, `find -exec`), and a git
//! command tracer has no answer for are left alone.

use crate::shell::{self, Command};
use std::path::{Path, PathBuf};

pub(super) const READERS: &[&str] = &["cat", "head", "tail", "sed", "awk"];
const SEARCHERS: &[&str] = &["grep", "egrep", "fgrep", "rg"];
const LISTERS: &[&str] = &["ls", "tree"];
const FILTERS: &[&str] = &[
    "grep", "egrep", "fgrep", "rg", "sed", "awk", "head", "tail", "cut", "sort", "uniq", "wc", "column", "fold", "tr", "jq",
];
const SEARCH_VALUED: &[&str] = &["-e", "-f", "-g", "--glob", "-t", "--type", "-T", "--type-not", "-m", "--max-count", "-A", "-B", "-C", "-M"];
const FIND_ACTIONS: &[&str] = &["-delete", "-exec", "-execdir", "-ok", "-okdir"];

/// The refusal for `line` run in `cwd`, `None` when it reads no code raw.
pub fn refusal(line: &str, cwd: &Path) -> Option<String> {
    let project = crate::cache::worktree_root_for(cwd)?;
    let commands = shell::commands(line);
    // A command `xargs` runs gets its paths from its input, so no path given
    // is not the working folder, and it is left alone.
    commands.iter().filter(|command| !command.fed).find_map(|command| {
        let source = command.input.and_then(|at| commands.get(at));
        let replacement = match source {
            Some(trace) if trace.program() == "trace" && FILTERS.contains(&command.program()) => {
                Some(filtered(&trace.argv, command.program(), command.args()))
            }
            _ => Scope { project: &project, base: cwd.join(&command.folder) }.replacement(command),
        };
        replacement.map(|run| format!("tracer: Claude reads this project's code only through tracer.\nRun this instead: {run}"))
    })
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

struct Scope<'a> {
    project: &'a Path,
    base: PathBuf,
}

impl Scope<'_> {
    fn replacement(&self, command: &Command) -> Option<String> {
        let program = command.program();
        let args = command.args();
        match program {
            "git" => self.git(args),
            "find" => self.find(args),
            _ if SEARCHERS.contains(&program) => self.search(program, args),
            _ if READERS.contains(&program) => self.read(program, args),
            _ if LISTERS.contains(&program) => self.list(program, args),
            _ => None,
        }
    }

    /// Whether `arg` names something in the project, a glob by the folder
    /// before its first wildcard.
    fn in_project(&self, arg: &str) -> bool {
        if arg.is_empty() || arg.starts_with('-') {
            return false;
        }
        let named = match arg.find(['*', '?', '[']) {
            Some(glob) => Path::new(&format!("{}x", &arg[..glob])).parent().map(Path::to_path_buf).unwrap_or_default(),
            None => PathBuf::from(arg),
        };
        let path = crate::cache::absolutize(&self.base.join(named));
        let path = path.canonicalize().unwrap_or(path);
        path.starts_with(self.project) && path.exists()
    }

    fn paths(&self, args: &[String]) -> Vec<String> {
        args.iter().filter(|arg| self.in_project(arg)).cloned().collect()
    }

    fn search(&self, program: &str, args: &[String]) -> Option<String> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut pattern = None;
        let mut words = args.iter();
        while let Some(arg) = words.next() {
            if SEARCH_VALUED.contains(&arg.as_str()) {
                let value = words.next();
                match (arg.as_str(), value) {
                    ("-e", Some(value)) => pattern = Some(value.clone()),
                    ("-g" | "--glob" | "-t" | "--type", Some(value)) => {
                        let short = if arg.len() == 2 { arg.clone() } else { format!("-{}", &arg[2..3]) };
                        flags.extend([short, value.clone()]);
                    }
                    _ => {}
                }
            } else if arg == "-i" || arg == "--ignore-case" {
                flags.push("-i".into());
            } else if !arg.starts_with('-') {
                positional.push(arg.clone());
            }
        }
        let pattern = pattern.or_else(|| (!positional.is_empty()).then(|| positional.remove(0)))?;
        let paths = self.paths(&positional);
        let recursive = program == "rg" || args.iter().any(|arg| arg == "-r" || arg == "-R" || arg == "--recursive");
        if paths.is_empty() && !(recursive && positional.is_empty() && self.in_project(".")) {
            return None;
        }
        Some(shell::join(&[vec!["trace".into(), "grep".into(), pattern], paths, flags].concat()))
    }

    fn read(&self, program: &str, args: &[String]) -> Option<String> {
        if program == "sed" && args.iter().any(|arg| arg.starts_with("-i") || arg == "--in-place") {
            return None;
        }
        let paths = self.paths(args);
        (!paths.is_empty()).then(|| shell::join(&[vec!["trace".into(), "read".into()], paths].concat()))
    }

    fn list(&self, program: &str, args: &[String]) -> Option<String> {
        let named: Vec<String> = args.iter().filter(|arg| !arg.starts_with('-')).cloned().collect();
        let paths = match named.is_empty() {
            true if self.in_project(".") => vec![".".to_string()],
            true => Vec::new(),
            false => self.paths(&named),
        };
        let first = paths.first()?.clone();
        Some(match program {
            "tree" => shell::join(&["trace".into(), "tree".into(), first]),
            _ => shell::join(&[vec!["trace".into(), "list".into()], paths].concat()),
        })
    }

    fn find(&self, args: &[String]) -> Option<String> {
        if args.iter().any(|arg| FIND_ACTIONS.contains(&arg.as_str())) {
            return None;
        }
        let end = args.iter().position(|arg| arg.starts_with('-') || arg == "(" || arg == "!").unwrap_or(args.len());
        let bases = match end {
            0 if self.in_project(".") => vec![".".to_string()],
            0 => Vec::new(),
            _ => self.paths(&args[..end]),
        };
        if bases.is_empty() {
            return None;
        }
        let name = args
            .iter()
            .position(|arg| arg == "-name" || arg == "-iname")
            .and_then(|at| args.get(at + 1))
            .cloned()
            .unwrap_or_else(|| "*".into());
        Some(shell::join(&[vec!["trace".into(), "find".into(), name], bases].concat()))
    }

    fn git(&self, args: &[String]) -> Option<String> {
        let at = args.iter().position(|arg| !arg.starts_with('-'))?;
        let subcommand = args[at].as_str();
        let rest = &args[at + 1..];
        let separator = rest.iter().position(|arg| arg == "--").unwrap_or(rest.len());
        let flags: Vec<&String> = rest[..separator].iter().filter(|arg| arg.starts_with('-')).collect();
        let positional: Vec<String> = rest[..separator]
            .iter()
            .filter(|arg| !arg.starts_with('-'))
            .chain(rest.get(separator + 1..).unwrap_or(&[]))
            .cloned()
            .collect();
        let has = |flag: &str| flags.iter().any(|given| *given == flag);
        match subcommand {
            "blame" | "annotate" => {
                let file = self.paths(&positional).into_iter().next()?;
                let range = rest
                    .iter()
                    .position(|arg| arg == "-L")
                    .and_then(|at| rest.get(at + 1))
                    .filter(|range| range.split_once(',').is_some_and(|(a, b)| is_number(a) && is_number(b)));
                let mut words = vec!["trace".to_string(), "blame".into(), file];
                if let Some(range) = range {
                    words.extend(["--lines".into(), range.replace(',', ":")]);
                }
                Some(shell::join(&words))
            }
            "grep" => {
                let (pattern, paths) = positional.split_first()?;
                Some(shell::join(&[vec!["trace".into(), "grep".into(), pattern.clone()], self.paths(paths)].concat()))
            }
            "show" | "cat-file" => {
                if subcommand == "cat-file" && !has("-p") {
                    return None;
                }
                let (revision, file) = positional.iter().find_map(|arg| arg.split_once(':'))?;
                let revision = if revision.is_empty() { "HEAD" } else { revision };
                Some(shell::join(&["trace".into(), "read".into(), file.into(), "--revision".into(), revision.into()]))
            }
            "log" => {
                if flags.iter().any(|flag| flag.starts_with("-G") || *flag == "-p" || *flag == "--patch") {
                    return None;
                }
                if let Some(at) = rest.iter().position(|arg| arg == "-S") {
                    return Some(match rest.get(at + 1).filter(|text| !text.starts_with('-')) {
                        Some(text) => shell::join(&["trace".into(), "history".into(), "--contains".into(), text.clone()]),
                        None => "trace history --contains <pattern>".into(),
                    });
                }
                if has("-L") {
                    return Some("trace history <file> <symbol>".into());
                }
                let paths = self.paths(&positional);
                (!paths.is_empty() && paths.len() == positional.len())
                    .then(|| shell::join(&["trace".into(), "history".into(), paths[0].clone()]))
            }
            "diff" if has("--name-status") => Some("trace diff".into()),
            _ => None,
        }
    }
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}
