//! Shared black-box harness for the `trace` CLI test suite.
//!
//! Every integration test drives the CLI as a subprocess via [`trace`]. The
//! binary under test is resolved from the `TRACE_BIN` environment variable,
//! defaulting to `trace` on `PATH`, so the suite can run against any built
//! `trace` binary without changing the assertions.
//!
//! Nothing here imports or links tracer internals. The only contract is the
//! CLI's observable surface: exit code, stdout, stderr, the `--json`
//! document shape, and wall-clock latency.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Resolve the `trace` binary under test.
///
/// `TRACE_BIN` overrides the default (`trace` on PATH), so the suite can be
/// pointed at a specific build, e.g.
/// `TRACE_BIN=/path/to/target/release/trace`.
pub fn trace_bin() -> String {
    std::env::var("TRACE_BIN").unwrap_or_else(|_| "trace".to_string())
}

/// A stdout no one reads: the write end of a pipe whose read end is already
/// closed, so the child's first write fails with a broken pipe every time.
pub fn closed_stdout() -> Stdio {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    Stdio::from(writer)
}

/// SCHEMA_VERSION as published in the tracer Claude.md and `cache.rs`.
pub const PUBLISHED_SCHEMA_VERSION: u32 = 54;

/// The directory under `.tracer-cache/file/` holding every entry `schema`
/// writes in the worktree at `root`.
pub fn schema_directory(root: &Path, schema: u32) -> PathBuf {
    root.join(format!(".tracer-cache/file/schema{schema}"))
}

/// Holds the current schema's lock on the index `family` in the worktree at
/// `root` until dropped, the way another call building that index holds it.
pub fn hold_index_lock(root: &Path, family: &str) -> fs::File {
    let path = schema_directory(root, PUBLISHED_SCHEMA_VERSION).join(format!(".{family}.lock"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = fs::File::options().create(true).truncate(false).write(true).open(path).unwrap();
    lock.lock().unwrap();
    lock
}

/// Outcome of one CLI invocation.
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Wall-clock duration of the subprocess, start to exit.
    pub elapsed: Duration,
}

impl Run {
    /// Assert the process exited 0; panic with full diagnostics otherwise.
    pub fn ok(&self) -> &Self {
        assert_eq!(
            self.code, 0,
            "expected exit 0, got {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.code, self.stdout, self.stderr
        );
        self
    }

    /// Assert a specific non-zero exit code.
    pub fn code_is(&self, want: i32) -> &Self {
        assert_eq!(
            self.code, want,
            "expected exit {}, got {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            want, self.code, self.stdout, self.stderr
        );
        self
    }

    /// Combined stdout+stderr — error text lands on either stream across
    /// commands (click prints usage to stdout, explicit errors to stderr).
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// Assert stdout parses as JSON; return the value.
    /// A hook answer's extra context, empty when it has none.
    pub fn context(&self) -> String {
        if self.stdout.trim().is_empty() {
            return String::new();
        }
        self.json()["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout was not valid JSON ({e})\n--- stdout ---\n{}\n--- stderr ---\n{}",
                self.stdout, self.stderr
            )
        })
    }

    /// The document's four slots flattened into one object, so an assertion
    /// names the fact rather than the slot holding it.
    ///
    /// Every `--json` result carries `query`, `context`, `results`, `counts`.
    /// Which slot a given fact sits in is the document's business, not each
    /// test's: `view()` merges `query`, `context`, and `counts` at the top
    /// level, and merges `results` too when it is an object (a command whose
    /// result is named sections rather than a row list). A row list stays
    /// under `results`.
    pub fn view(&self) -> serde_json::Value {
        let document = self.json();
        let mut merged = serde_json::Map::new();
        // Counts merge before context and results, so where a count shares a
        // name with a section the section wins and the count stays reachable
        // as `json()["counts"][name]`.
        for slot in ["query", "counts", "context"] {
            if let Some(object) = document.get(slot).and_then(|v| v.as_object()) {
                for (key, value) in object {
                    merged.insert(key.clone(), value.clone());
                }
            }
        }
        match document.get("results") {
            Some(serde_json::Value::Object(sections)) => {
                for (key, value) in sections {
                    merged.insert(key.clone(), value.clone());
                }
            }
            Some(rows) => {
                merged.insert("results".to_string(), rows.clone());
            }
            None => {}
        }
        serde_json::Value::Object(merged)
    }

    /// Assert the run finished within `budget`; panic with the measured time
    /// otherwise. Thresholds are deliberately generous (see suite README) so
    /// they catch gross regressions without flaking on a loaded CI box.
    pub fn within(&self, budget: Duration) -> &Self {
        assert!(
            self.elapsed <= budget,
            "command took {:?}, budget was {:?}\n--- stdout ---\n{}",
            self.elapsed,
            budget,
            self.stdout
        );
        self
    }
}

/// Invoke the `trace` binary with `args`, working dir `cwd`, returning the
/// captured outcome. Inherits the parent environment so the external binaries
/// (`scc`, `rg`, `ctags`, `ast-grep`, `git`) resolve on PATH.
pub fn trace<I, S>(cwd: &Path, args: I) -> Run
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    trace_env(cwd, args, &[])
}

/// Same as [`trace`] but with extra environment variables — used for the
/// `TRACER_CCN_BACKEND` toggle coverage.
pub fn trace_env<I, S>(cwd: &Path, args: I, env: &[(&str, &str)]) -> Run
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    trace_with_input(cwd, args, env, None)
}

/// [`trace_env`] with `input` written to the binary's stdin.
pub fn trace_with_input<I, S>(cwd: &Path, args: I, env: &[(&str, &str)], input: Option<&str>) -> Run
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(command(cwd, args, env), input)
}

/// The `trace` command for `args` from `cwd`: the invoking agent's session
/// identity removed, the test process as its `CLAUDE_PID`, `env` applied,
/// and `HOME` at `cwd` unless `env` names one.
pub fn command<I, S>(cwd: &Path, args: I, env: &[(&str, &str)]) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(trace_bin());
    cmd.current_dir(cwd);
    for a in args {
        cmd.arg(a);
    }
    // Session identity belongs to the fixture, never to the invoking agent.
    // Remove inherited values before applying explicit fixture overrides.
    for key in [
        "AGENT_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "TRACER_AGENT_ID",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        cmd.env_remove(key);
    }
    // The test process stands in for the Claude Code session, so a build a
    // test's hook starts ends with the test run.
    cmd.env("CLAUDE_PID", std::process::id().to_string());
    for (k, v) in env {
        cmd.env(k, v);
    }
    // The binary walks $HOME/.claude/rules, so an inherited developer HOME leaks live Rules into doc counts.
    if !env.iter().any(|(k, _)| *k == "HOME") {
        cmd.env("HOME", cwd);
    }
    cmd
}

/// How long one `trace` call may run before the suite kills it and fails.
pub const HUNG_AFTER: Duration = Duration::from_secs(60);

/// Runs `cmd` to its exit, `input` on its stdin, and captures the outcome. A
/// call still running after [`HUNG_AFTER`] is killed with the programs it
/// started, and the test fails naming those programs and the stderr printed so
/// far, so the log says where the call stopped instead of only that it ran on.
pub fn run(mut cmd: Command, input: Option<&str>) -> Run {
    let start = Instant::now();
    let mut child = cmd
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn `{}`: {e}", trace_bin()));
    if let Some(input) = input {
        std::io::Write::write_all(&mut child.stdin.take().expect("stdin is piped"), input.as_bytes())
            .unwrap_or_else(|e| panic!("failed to write `{}`'s stdin: {e}", trace_bin()));
    }
    let stdout = Drained::start(child.stdout.take().expect("stdout is piped"));
    let stderr = Drained::start(child.stderr.take().expect("stderr is piped"));
    let status = loop {
        if let Some(status) = child.try_wait().unwrap_or_else(|e| panic!("failed to wait for `{}`: {e}", trace_bin())) {
            break status;
        }
        if start.elapsed() > HUNG_AFTER {
            let running = children_of(child.id());
            for (pid, _) in &running {
                let _ = Command::new("kill").args(["-9", pid]).status();
            }
            let _ = child.kill();
            let _ = child.wait();
            let running: Vec<String> = running.into_iter().map(|(pid, command)| format!("{pid} {command}")).collect();
            panic!(
                "`{}` ran past {HUNG_AFTER:?} and was killed\n--- programs it was waiting on ---\n{}\n--- stderr so far ---\n{}",
                trace_bin(),
                if running.is_empty() { "none".to_string() } else { running.join("\n") },
                stderr.so_far()
            );
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let elapsed = start.elapsed();
    Run {
        code: status.code().unwrap_or(-1),
        stdout: stdout.finish(),
        stderr: stderr.finish(),
        elapsed,
    }
}

/// One pipe read to its end on a thread of its own, into a buffer readable at
/// any moment, so a killed call still shows what it printed.
struct Drained {
    buffer: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: std::thread::JoinHandle<()>,
}

impl Drained {
    fn start(mut pipe: impl std::io::Read + Send + 'static) -> Self {
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let filled = buffer.clone();
        let reader = std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(read) = pipe.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                filled.lock().unwrap().extend_from_slice(&chunk[..read]);
            }
        });
        Self { buffer, reader }
    }

    fn so_far(&self) -> String {
        String::from_utf8_lossy(&self.buffer.lock().unwrap()).into_owned()
    }

    fn finish(self) -> String {
        let Self { buffer, reader } = self;
        reader.join().expect("pipe reader");
        let read = buffer.lock().unwrap();
        String::from_utf8_lossy(&read).into_owned()
    }
}

/// Each process whose parent is `pid`, as its id and command line.
fn children_of(pid: u32) -> Vec<(String, String)> {
    let listing = Command::new("ps").args(["-A", "-o", "ppid=,pid=,command="]).output().map(|out| out.stdout).unwrap_or_default();
    String::from_utf8_lossy(&listing)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let parent = fields.next()?;
            let child = fields.next()?;
            (parent == pid.to_string()).then(|| (child.to_string(), fields.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

#[allow(non_upper_case_globals)] // project naming rule bans ALL_CAPS for our own identifiers
static fixture_seq: AtomicU64 = AtomicU64::new(0);

/// The environment variables `trace` reads a session id from.
const SESSION_VARIABLES: [&str; 3] = ["AGENT_SESSION_ID", "CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID"];

/// The folder holding every session's records, where `trace` keeps them:
/// `tracer-<uid>/sessions/` in the user's own temporary folder, which no
/// `TMPDIR` moves.
pub fn sessions_root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let read = |program: &str, args: &[&str]| {
            String::from_utf8(Command::new(program).args(args).output().unwrap().stdout).unwrap().trim().to_string()
        };
        let temp = match cfg!(target_os = "macos") {
            true => read("getconf", &["DARWIN_USER_TEMP_DIR"]),
            false => "/tmp".to_string(),
        };
        PathBuf::from(temp).join(format!("tracer-{}", read("id", &["-u"]))).join("sessions")
    })
    .clone()
}

/// A throwaway git repository on disk, deleted on drop.
///
/// Commits are created with an explicit, hermetic environment (fixed
/// author/committer, isolated `HOME`) so the suite never depends on the
/// developer's git config — the same pattern the prior pytest `test_glob.py`
/// used. `git commit` is run via a direct subprocess (not a shell), which is
/// also what keeps the suite independent of any commit-gating shell hooks.
///
/// Every session id a run from the fixture passes carries the fixture's own
/// suffix, so fixtures running side by side never share a record, and the
/// fixture's sessions are deleted with it.
pub struct Fixture {
    pub root: PathBuf,
    /// A scratch folder beside the repository, deleted with it.
    pub tmp: PathBuf,
    suffix: String,
}

impl Fixture {
    /// Create an empty initialized git repo in a unique temp directory.
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = fixture_seq.fetch_add(1, Ordering::SeqCst);
        let suffix = format!("fixture-{nanos}-{seq}");
        let root = std::env::temp_dir().join(format!("trace-{suffix}"));
        let tmp = std::env::temp_dir().join(format!("trace-{suffix}-tmp"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&tmp).unwrap();
        let f = Fixture { root, tmp, suffix };
        f.git(&["init", "--quiet"]);
        f.git(&["config", "user.email", "test@example.com"]);
        f.git(&["config", "user.name", "Tracer Test"]);
        f
    }

    /// Run a git command in the fixture with a hermetic environment.
    pub fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/local/bin")
            .env("HOME", &self.root)
            .env("GIT_AUTHOR_NAME", "Tracer Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Tracer Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap_or_else(|e| panic!("git {:?} failed to spawn: {e}", args));
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }

    /// Write `contents` to `rel` (creating parent dirs). Returns the abs path.
    pub fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, contents).unwrap();
        p
    }

    /// Write raw bytes (used for the binary-file edge case).
    pub fn write_bytes(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, bytes).unwrap();
        p
    }

    /// `git add -A && git commit -m msg`.
    pub fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", msg]);
    }

    /// `git worktree add <abs_path> -b <branch>`. Returns the linked
    /// worktree's absolute path. The linked worktree is cleaned up when
    /// the parent fixture drops (its root is removed recursively); git's
    /// `worktrees` admin dir is also gone with the main repo.
    pub fn add_worktree(&self, sub: &str, branch: &str) -> PathBuf {
        let abs = self.root.join(sub);
        self.git(&[
            "worktree",
            "add",
            "-b",
            branch,
            abs.to_str().expect("worktree path is UTF-8"),
        ]);
        abs
    }

    /// Path to a fixture-relative file as a string (for passing to `trace`).
    pub fn path(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }

    /// Run `trace` with the fixture root as the working directory.
    pub fn trace(&self, args: &[&str]) -> Run {
        self.trace_env(args, &[])
    }

    /// Run `trace` with extra env.
    pub fn trace_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        self.trace_in(&self.root, args, env)
    }

    /// Run `trace` from `cwd` with extra env.
    pub fn trace_in<I, S>(&self, cwd: &Path, args: I, env: &[(&str, &str)]) -> Run
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        run(self.command(cwd, args, env), None)
    }

    /// The `trace` command for `args` from `cwd` with `env`, every session
    /// id in `env` this fixture's own. A test that runs `trace` itself, to
    /// hold its stdin or stdout, starts it from here.
    pub fn command<I, S>(&self, cwd: &Path, args: I, env: &[(&str, &str)]) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let owned: Vec<(&str, String)> = env
            .iter()
            .map(|(key, value)| match SESSION_VARIABLES.contains(key) {
                true => (*key, self.session(value)),
                false => (*key, value.to_string()),
            })
            .collect();
        let env: Vec<(&str, &str)> = owned.iter().map(|(key, value)| (*key, value.as_str())).collect();
        command(cwd, args, &env)
    }

    /// `session` as this fixture passes it to `trace`.
    pub fn session(&self, session: &str) -> String {
        format!("{session}-{}", self.suffix)
    }

    /// The folder holding `session`'s records.
    pub fn session_dir(&self, session: &str) -> PathBuf {
        sessions_root().join(self.session(session))
    }

    /// `trace hook <event> --harness <harness> <flags>` from the fixture
    /// root, `input` on stdin, its `session_id` this fixture's own, and a
    /// 20-second deadline unless `flags` name one.
    pub fn hook(&self, event: &str, harness: &str, input: &serde_json::Value, flags: &[&str], env: &[(&str, &str)]) -> Run {
        let mut input = input.clone();
        if let Some(session) = input["session_id"].as_str() {
            input["session_id"] = self.session(session).into();
        }
        let mut args = vec!["hook", event, "--harness", harness];
        if !flags.contains(&"--deadline-ms") {
            args.extend(["--deadline-ms", "20000"]);
        }
        args.extend(flags);
        run(self.command(&self.root, args, env), Some(&input.to_string()))
    }

    /// `agent`'s record in `session`, `None` while nothing was recorded.
    pub fn record(&self, session: &str, agent: &str) -> Option<serde_json::Value> {
        let bytes = fs::read(self.session_dir(session).join(agent).join("record.json")).ok()?;
        Some(serde_json::from_slice(&bytes).expect("record.json is JSON"))
    }

    /// Whether a `trace cache build` of this repository runs.
    pub fn building(&self) -> bool {
        Command::new("pgrep").args(["-f", &self.build_pattern()]).output().unwrap().status.success()
    }

    /// What `pgrep -f` finds a build of this repository by.
    pub fn build_pattern(&self) -> String {
        format!("{} cache build", self.root.file_name().unwrap().to_string_lossy())
    }
}

impl Drop for Fixture {
    /// Ends a build a hook started here before the repository goes, because
    /// a build still writing would leave its `.tracer-cache/` behind.
    fn drop(&mut self) {
        let _ = Command::new("pkill").args(["-f", &self.build_pattern()]).status();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.building() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.tmp);
        let suffix = format!("-{}", self.suffix);
        for session in fs::read_dir(sessions_root()).into_iter().flatten().flatten() {
            if session.file_name().to_string_lossy().ends_with(&suffix) {
                let _ = fs::remove_dir_all(session.path());
            }
        }
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

/// A representative multi-language repo used by most correctness tests.
///
/// Layout (committed):
/// ```text
/// src/app.py        imports src.util.helper; branchy main()
/// src/util.py       helper()
/// src/front.tsx     exported component, imports a constant
/// lib/widget.php    a PHP class + function
/// docs/readme.md    non-source file
/// pyproject.toml    package-config marker for the primer
/// ```
pub fn standard_repo() -> Fixture {
    let f = Fixture::new();
    f.write(
        "src/app.py",
        concat!(
            "import os\n",
            "from src.util import helper\n",
            "\n\n",
            "def main(x):\n",
            "    \"\"\"Entry point with real branching.\"\"\"\n",
            "    if x:\n",
            "        return helper(x)\n",
            "    for i in range(10):\n",
            "        if i % 2 == 0:\n",
            "            print(i)\n",
            "    return None\n",
        ),
    );
    f.write(
        "src/util.py",
        "def helper(v):\n    if v > 0:\n        return v + 1\n    return 0\n",
    );
    f.write(
        "src/front.tsx",
        concat!(
            "import { CONST } from './consts';\n",
            "export const Widget = (p: {n: number}) => {\n",
            "  return p.n > CONST ? <div/> : <span/>;\n",
            "};\n",
        ),
    );
    f.write("src/consts.ts", "export const CONST = 5;\n");
    f.write(
        "lib/widget.php",
        concat!(
            "<?php\n",
            "function render($x) {\n",
            "  if ($x) { return 1; }\n",
            "  return 0;\n",
            "}\n",
            "class Widget {\n",
            "  public function show() { return render(1); }\n",
            "}\n",
        ),
    );
    f.write("docs/readme.md", "# Project\n\nDocs only.\n");
    f.write(
        "pyproject.toml",
        "[project]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );
    f.commit("init standard repo");
    f
}

/// Parse the human `cache stats` table into `namespace -> entry_count`.
/// Lines look like: `  file            5 entries     2.6 KB`.
pub fn parse_stats_table(stdout: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for line in stdout.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() >= 3 && toks[2] == "entries" {
            if let Ok(n) = toks[1].parse::<u64>() {
                out.insert(toks[0].to_string(), n);
            }
        }
    }
    out
}
