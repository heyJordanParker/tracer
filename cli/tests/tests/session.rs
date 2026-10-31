//! The agent's session record: `tracer-<uid>/sessions/<session>/` in the
//! user's own temporary folder, with one `<agent>/record.json` per agent and
//! the session-wide `directories.json`.
//!
//! Black-box: assertions go through the CLI surface (`trace docs`,
//! `trace context`, `trace read`) and the documented on-disk shape of
//! `record.json`. Each fixture suffixes the session ids it passes, so every
//! test starts with no records.

use std::collections::BTreeSet;
use std::thread;

use tracer_cli_tests::Fixture;

const SESSION: &str = "session-test";

fn docs_repo() -> Fixture {
    let f = Fixture::new();
    f.write("Claude.md", "# Root rules\n\nProject root.\n");
    f.write("sub/Claude.md", "# Sub rules\n\nThis dir has rules.\n");
    f.write(
        "sub/util.py",
        "def helper(v):\n    if v > 0:\n        return v + 1\n    return 0\n",
    );
    f.commit("init docs repo");
    f
}

fn record(f: &Fixture, agent: &str) -> serde_json::Value {
    f.record(SESSION, agent)
        .unwrap_or_else(|| panic!("no record for agent {agent}"))
}

/// The record's files, as `visible_as -> entry`.
fn files(record: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    record["files"]
        .as_object()
        .expect("record.files is an object")
        .values()
        .map(|entry| (entry["visible_as"].as_str().unwrap().to_string(), entry.clone()))
        .collect()
}

// --- docs ------------------------------------------------------------------

#[test]
fn docs_command_records_each_doc_it_sends() {
    let f = docs_repo();
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION)];

    f.trace_env(&["docs", "sub/util.py"], &env).ok();

    let record = record(&f, "root");
    assert_eq!(record["version"], 1, "{record}");
    let files = files(&record);
    assert_eq!(
        files.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["Claude.md", "sub/Claude.md"]),
        "{record}"
    );
    for entry in files.values() {
        assert_eq!(entry["kind"], "doc_injection", "{entry}");
        assert_eq!(entry["source"], "trace_docs", "{entry}");
        assert_eq!(entry["whole"], true, "{entry}");
        assert!(
            entry["hash"].as_str().unwrap().starts_with("sha256:"),
            "{entry}"
        );
    }
}

#[test]
fn a_second_docs_call_leaves_the_record_as_it_was() {
    let f = docs_repo();
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION)];

    f.trace_env(&["docs", "sub/util.py"], &env).ok();
    let first = record(&f, "root");

    let second = f.trace_env(&["docs", "sub/util.py"], &env);
    second.ok();
    assert!(second.stdout.is_empty(), "{}", second.stdout);
    assert_eq!(record(&f, "root"), first);
}

#[test]
fn outside_a_repository_a_session_still_keeps_its_record() {
    let f = Fixture::new();
    let outside = f.tmp.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("Claude.md"), "# Outside rules\n").unwrap();
    std::fs::write(outside.join("util.py"), "def f():\n    return 1\n").unwrap();
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION)];

    let first = f.trace_in(&outside, ["docs", "util.py"], &env);
    first.ok();
    assert!(first.stdout.contains("Outside rules"), "{}", first.stdout);
    let again = f.trace_in(&outside, ["docs", "util.py"], &env);
    again.ok();
    assert!(again.stdout.is_empty(), "{}", again.stdout);
}

// --- agents ----------------------------------------------------------------

#[test]
fn different_agents_in_one_session_keep_separate_records() {
    let f = docs_repo();
    let alpha = [("CLAUDE_CODE_SESSION_ID", SESSION), ("TRACER_AGENT_ID", "alpha")];
    let beta = [("CLAUDE_CODE_SESSION_ID", SESSION), ("TRACER_AGENT_ID", "beta")];

    let alpha_first = f.trace_env(&["docs", "sub/util.py"], &alpha);
    alpha_first.ok();
    assert!(alpha_first.stdout.contains("Root rules"), "{}", alpha_first.stdout);

    // Beta's record starts empty, so it is sent the docs too.
    let beta_first = f.trace_env(&["docs", "sub/util.py"], &beta);
    beta_first.ok();
    assert!(beta_first.stdout.contains("Root rules"), "{}", beta_first.stdout);

    assert_eq!(files(&record(&f, "alpha")).len(), 2);
    assert_eq!(files(&record(&f, "beta")).len(), 2);
    assert!(
        f.record(SESSION, "root").is_none(),
        "agent-scoped runs must not write the root record"
    );

    let alpha_again = f.trace_env(&["docs", "sub/util.py"], &alpha);
    alpha_again.ok();
    assert!(alpha_again.stdout.is_empty(), "{}", alpha_again.stdout);
}

#[test]
fn the_agent_flag_names_the_record_a_subagents_shell_writes() {
    let f = docs_repo();
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION)];

    f.trace_env(&["--agent", "worker", "docs", "sub/util.py"], &env).ok();

    assert_eq!(files(&record(&f, "worker")).len(), 2);
    assert!(f.record(SESSION, "root").is_none());
}

#[test]
fn missing_agent_id_defaults_to_the_root_record() {
    let f = docs_repo();
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION)];

    f.trace_env(&["docs", "sub/util.py"], &env).ok();

    assert_eq!(files(&record(&f, "root")).len(), 2);
}

// --- concurrent writers ------------------------------------------------------

#[test]
fn concurrent_writers_record_each_doc_once() {
    let f = docs_repo();
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION)];

    let outputs: Vec<String> = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| f.trace_env(&["docs", "sub/util.py"], &env)))
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                let run = handle.join().unwrap();
                run.ok();
                run.stdout
            })
            .collect()
    });

    assert_eq!(
        outputs.iter().filter(|stdout| stdout.contains("Root rules")).count(),
        1,
        "exactly one call sends the docs: {outputs:?}"
    );
    let files = files(&record(&f, "root"));
    assert_eq!(
        files.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["Claude.md", "sub/Claude.md"])
    );
}

// --- reads -------------------------------------------------------------------

#[test]
fn context_of_a_file_records_it_as_read() {
    let f = docs_repo();
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION)];

    f.trace_env(&["context", "sub/util.py"], &env).ok();

    let record = record(&f, "root");
    let read: Vec<&serde_json::Value> = record["files"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(path, _)| path.ends_with("sub/util.py"))
        .map(|(_, entry)| entry)
        .collect();
    assert_eq!(read.len(), 1, "{record}");
    assert_eq!(read[0]["kind"], "read_file", "{record}");
    assert_eq!(read[0]["source"], "agent_read", "{record}");
    assert_eq!(read[0]["coverage"]["read"], serde_json::json!([[1, 4]]), "{record}");
}

#[test]
fn a_read_writes_the_record_once_and_reuses_the_directory_baseline() {
    let f = Fixture::new();
    let source = format!(
        "from outside.a import a\n{}",
        (1..=30)
            .map(|line| format!("line_{line} = a()\n"))
            .collect::<String>()
    );
    f.write("outside/a.py", "def a():\n    return 1\n");
    f.write("inside/ranges.py", &source);
    f.commit("read ranges");
    let env = [
        ("CLAUDE_CODE_SESSION_ID", SESSION),
        ("TRACER_AGENT_ID", "a"),
        ("TRACE_TIMING", "1"),
    ];

    f.trace_env(&["cache", "build"], &env).ok();
    let first = f.trace_env(&["read", "inside/ranges.py", "--lines", "1:5"], &env);
    first.ok();
    assert_eq!(
        first.stderr.matches("timing session record ").count(),
        1,
        "{}",
        first.stderr
    );
    f.trace_env(&["status", "--json"], &env).ok();
    let directories = f.session_dir(SESSION).join("directories.json");
    let before = std::fs::read(&directories).expect("directory baseline written");

    let second = f.trace_env(&["read", "inside/ranges.py", "--lines", "10:20"], &env);
    second.ok();
    assert_eq!(
        second.stderr.matches("timing session record ").count(),
        1,
        "{}",
        second.stderr
    );
    assert!(
        !second.stderr.contains("timing lock session directories "),
        "a directory baseline hit must not lock: {}",
        second.stderr
    );
    assert_eq!(std::fs::read(&directories).unwrap(), before);
    let record = record(&f, "a");
    let entry = record["files"]
        .as_object()
        .unwrap()
        .values()
        .find(|entry| entry["kind"] == "read_file")
        .expect("the read is recorded");
    assert_eq!(entry["coverage"]["read"], serde_json::json!([[1, 5], [10, 20]]));
}

// --- the session-wide directory baseline ---------------------------------------

#[test]
fn directory_baseline_is_shared_across_agents_and_survives_context_reset() {
    let f = Fixture::new();
    f.write("outside/one.py", "def one():\n    return 1\n");
    f.write("outside/two.py", "def two():\n    return 2\n");
    f.write("inside/main.py", "class Main:\n    pass\n");
    f.write("consumer.py", "from inside.main import Main\n");
    f.commit("initial directory graph");

    let agent_a = [("AGENT_SESSION_ID", SESSION), ("TRACER_AGENT_ID", "a")];
    let agent_b = [("AGENT_SESSION_ID", SESSION), ("TRACER_AGENT_ID", "b")];

    // A normal read, not a context command, establishes the session baseline.
    f.trace_env(&["read", "inside/main.py"], &agent_a).ok();
    let directories = f.session_dir(SESSION).join("directories.json");
    let first_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&directories).expect("trace read writes directories.json"))
            .expect("directories.json is valid JSON");
    let inside_key = format!("{}/inside/", f.root.canonicalize().unwrap().display());
    assert_eq!(first_json[&inside_key]["imports"], 0, "{first_json}");

    // The same directory now imports two outside files. A second agent sees
    // Agent A's session-level first touch, not a baseline of its own.
    f.write(
        "inside/main.py",
        "from outside.one import one\nfrom outside.two import two\n\nclass Main:\n    def run(self):\n        return one() + two()\n",
    );
    let second = f.trace_env(&["read", "inside/main.py"], &agent_b);
    second.ok();
    assert!(
        second
            .stdout
            .contains("at_session_start: {imports: 0, now_imports_from: [outside/]}"),
        "second agent must render the shared baseline:\n{}",
        second.stdout
    );

    let status = f.trace_env(&["status", "--json"], &agent_b);
    status.ok();
    let status_json = status.json();
    let inside = &status_json["context"]["directories"]["inside/"];
    assert_eq!(inside["at_session_start"]["imports"], 0, "{status_json}");
    assert_eq!(inside["imports"], 2, "{status_json}");

    let before_reset = std::fs::read(&directories).unwrap();
    f.trace_env(&["docs", "reset"], &agent_b).ok();
    assert_eq!(
        std::fs::read(&directories).unwrap(),
        before_reset,
        "trace docs reset must not rewrite the directory baseline"
    );
}

#[test]
fn missing_no_record_target_records_its_parent_directory_baseline() {
    let f = Fixture::new();
    f.write("inside/main.py", "def main():\n    return 0\n");
    f.commit("initial directory");
    let env = [("AGENT_SESSION_ID", SESSION)];

    let missing = f.trace_env(&["context", "inside/new.py", "--no-record"], &env);
    missing.code_is(2);
    assert!(
        missing.stdout.contains("path: inside/"),
        "a missing target renders its parent directory context:\n{}",
        missing.stdout
    );
    assert!(
        f.session_dir(SESSION).join("directories.json").is_file(),
        "a missing target establishes the parent directory baseline"
    );
}

// --- no session ---------------------------------------------------------------

#[test]
fn no_session_id_means_no_record_is_written() {
    let f = docs_repo();

    let r = f.trace_env(&["docs", "sub/util.py"], &[("TRACE_TIMING", "1")]);
    r.ok();
    assert!(r.stdout.contains("Root rules"), "{}", r.stdout);
    assert!(
        !r.stderr.contains("timing lock session") && !r.stderr.contains("timing session record"),
        "a run with no session id takes no session lock and writes no record:\n{}",
        r.stderr
    );
}

// --- resumed Subagents and context resets ----------------------------------------

#[test]
fn a_listing_shows_each_files_facts_once_and_a_read_after_it_the_rest() {
    let f = docs_repo();
    let env = [("AGENT_SESSION_ID", SESSION)];

    let first = f.trace_env(&["find", "*.py"], &env);
    first.ok();
    assert!(first.stdout.contains("  sub/util.py  {"), "{}", first.stdout);
    let again = f.trace_env(&["find", "*.py"], &env);
    again.ok();
    assert!(again.stdout.contains("  sub/util.py\n"), "{}", again.stdout);
    // Another listing shares the record: the facts stay shown.
    let grep = f.trace_env(&["grep", "helper", "sub"], &env);
    grep.ok();
    assert!(grep.stdout.contains("sub/util.py  {matches: 1}"), "{}", grep.stdout);

    // The read prints the facts the listings never did, not the ones they did.
    let read = f.trace_env(&["read", "sub/util.py"], &env);
    read.ok();
    assert!(read.stdout.contains("complexity_rank: "), "{}", read.stdout);
    assert!(!read.stdout.contains("\nlines: "), "{}", read.stdout);
    let listed = f.trace_env(&["find", "*.py"], &env);
    listed.ok();
    assert!(listed.stdout.contains("  sub/util.py\n"), "{}", listed.stdout);

    // Without a session every listing shows every file's facts.
    let standalone = f.trace(&["find", "*.py"]);
    standalone.ok();
    assert!(standalone.stdout.contains("  sub/util.py  {"), "{}", standalone.stdout);
}

#[test]
fn a_doc_that_changed_since_it_was_sent_sends_only_its_changed_lines() {
    let f = docs_repo();
    let env = [("AGENT_SESSION_ID", SESSION)];
    f.trace_env(&["docs", "sub/util.py"], &env).ok();

    f.write("sub/Claude.md", "# Sub rules\n\nThis dir has new rules.\nAnd one more.\n");
    let changed = f.trace_env(&["docs", "sub/util.py"], &env);
    changed.ok();
    assert_eq!(
        changed.stdout,
        "## sub/Claude.md (changed since sent: 2 lines, 1 removed)\n\nL3: This dir has new rules.\nL4: And one more.\n"
    );

    let again = f.trace_env(&["docs", "sub/util.py"], &env);
    again.ok();
    assert_eq!(again.stdout, "", "a delta sent is held: {}", again.stdout);
}

#[test]
fn every_command_that_takes_paths_sends_the_docs_they_reach_once() {
    let f = docs_repo();
    f.write("sub/util.py", "def helper(v):\n    if v > 0:\n        return v + 1\n    return 0\n# edited\n");
    let commands: [&[&str]; 13] = [
        &["tree", "sub"],
        &["list", "sub"],
        &["find", "*.py", "sub"],
        &["grep", "helper", "sub"],
        &["pattern", "def $NAME($$$)", "-t", "python", "sub"],
        &["info", "sub/util.py"],
        &["info", "sub"],
        &["structure", "sub/util.py"],
        &["blame", "sub/util.py"],
        &["history", "sub/util.py"],
        &["stats", "sub"],
        &["diff", "sub"],
        &["usages", "--path", "sub"],
    ];
    for (index, args) in commands.iter().enumerate() {
        let session = format!("reached-{index}");
        let env = [("AGENT_SESSION_ID", session.as_str())];
        let first = f.trace_env(args, &env);
        first.ok();
        let sub = first.stdout.find("\n## sub/Claude.md\n").unwrap_or_else(|| panic!("{args:?}: {}", first.stdout));
        let root = first.stdout.find("\n## Claude.md\n").unwrap_or_else(|| panic!("{args:?}: {}", first.stdout));
        assert!(sub < root, "{args:?} sends the nearest doc first: {}", first.stdout);
        let again = f.trace_env(args, &env);
        again.ok();
        assert!(!again.stdout.contains("## "), "{args:?} sent a held doc: {}", again.stdout);

        let mut json = args.to_vec();
        json.push("--json");
        let json = f.trace_env(&json, &[("AGENT_SESSION_ID", "reached-json")]);
        json.ok();
        assert!(!json.stdout.contains("Sub rules"), "{args:?} --json carries no docs: {}", json.stdout);
    }
}

#[test]
fn docs_after_an_answer_fit_what_the_budget_leaves_and_name_the_rest() {
    let f = docs_repo();
    f.write("sub/Claude.md", &format!("# Sub rules\n\n{}", "A long rule line.\n".repeat(200)));
    f.commit("long doc");
    let env = [("AGENT_SESSION_ID", SESSION), ("TRACER_BUDGET", "1500")];

    let small = f.trace_env(&["tree", "sub"], &env);
    small.ok();
    assert!(small.stdout.len() < 1_800, "the docs stay inside the budget: {}", small.stdout.len());
    assert!(small.stdout.contains(" docs "), "the rest is named under the command that sends it: {}", small.stdout);
}

#[test]
fn under_a_session_nearest_doc_names_only_a_doc_tracer_does_not_send() {
    let f = docs_repo();
    let nearest = |env: &[(&str, &str)]| {
        let info = f.trace_env(&["info", "sub/util.py", "--brief"], env);
        info.ok();
        info.stdout.lines().find_map(|line| line.strip_prefix("nearest_doc: ")).map(str::to_string)
    };
    assert_eq!(nearest(&[("AGENT_SESSION_ID", "nearest-1")]), None);

    f.write("Readme.md", "# Readme\n");
    assert_eq!(nearest(&[("AGENT_SESSION_ID", "nearest-2")]).as_deref(), Some("Readme.md"));
    assert_eq!(nearest(&[]).as_deref(), Some("sub/Claude.md"), "with no session nothing sends the Claude docs");
}

#[test]
fn a_files_declarations_print_once_then_only_what_changed() {
    let f = docs_repo();
    let env = [("AGENT_SESSION_ID", SESSION)];
    f.write("mod.py", "def alpha():\n    return 1\n\n\ndef beta():\n    return 2\n");

    let first = f.trace_env(&["context", "mod.py"], &env);
    first.ok();
    assert!(first.stdout.contains("L1    def alpha():"), "{}", first.stdout);
    assert!(first.stdout.contains("L5    def beta():"), "{}", first.stdout);
    let again = f.trace_env(&["context", "mod.py"], &env);
    again.ok();
    assert!(!again.stdout.contains("def alpha"), "{}", again.stdout);

    // A new row prints whole; the rows it pushed down print only their line.
    f.write("mod.py", "def gamma():\n    return 3\n\n\ndef alpha():\n    return 1\n\n\ndef beta():\n    return 2\n");
    let added = f.trace_env(&["context", "mod.py"], &env);
    added.ok();
    assert!(added.stdout.contains("L1    def gamma():"), "{}", added.stdout);
    assert!(added.stdout.contains("moved: alpha() L5, beta() L9\n"), "{}", added.stdout);
    assert!(!added.stdout.contains("def alpha"), "{}", added.stdout);

    // A removed row is named by its kind and name.
    f.write("mod.py", "def gamma():\n    return 3\n\n\ndef alpha():\n    return 1\n");
    let removed = f.trace_env(&["context", "mod.py"], &env);
    removed.ok();
    assert!(removed.stdout.contains("removed: function beta\n"), "{}", removed.stdout);
    assert!(!removed.stdout.contains("def gamma"), "{}", removed.stdout);

    // Without a session the rows print whole every time.
    let standalone = f.trace(&["context", "mod.py"]);
    standalone.ok();
    assert!(standalone.stdout.contains("L1    def gamma():"), "{}", standalone.stdout);
}

#[test]
fn the_record_holds_each_listing_until_a_context_reset() {
    let f = docs_repo();
    let env = [("AGENT_SESSION_ID", SESSION)];

    let first = f.trace_env(&["context", "sub/util.py"], &env);
    first.ok();
    assert!(first.stdout.contains("  entries: [Claude.md, util.py]\n"), "{}", first.stdout);
    let shown = record(&f, "root");
    let sub = f.root.join("sub").canonicalize().unwrap();
    let block = &shown["shown"]["listing"][sub.to_string_lossy().as_ref()];
    // The record keeps a hash of each printed line, never the line.
    for line in ["directory.path", "directory.entries"] {
        assert!(
            block[line].as_str().is_some_and(|hash| hash.len() == 16),
            "{shown}"
        );
    }
    assert!(!block.to_string().contains("util.py"), "{shown}");

    let repeat = f.trace_env(&["context", "sub/util.py"], &env);
    repeat.ok();
    assert!(!repeat.stdout.contains("  entries: "), "{}", repeat.stdout);

    f.trace_env(&["docs", "reset"], &env).ok();
    let after_reset = f.trace_env(&["context", "sub/util.py"], &env);
    after_reset.ok();
    assert!(
        after_reset.stdout.contains("  entries: [Claude.md, util.py]\n"),
        "{}",
        after_reset.stdout
    );
}

#[test]
fn a_record_from_another_version_reads_as_empty() {
    let f = docs_repo();
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION)];

    f.trace_env(&["docs", "sub/util.py"], &env).ok();
    let path = f.session_dir(SESSION).join("root/record.json");
    let mut stale = record(&f, "root");
    stale["version"] = 0.into();
    std::fs::write(&path, stale.to_string()).unwrap();

    let again = f.trace_env(&["docs", "sub/util.py"], &env);
    again.ok();
    assert!(again.stdout.contains("Root rules"), "{}", again.stdout);
    assert_eq!(record(&f, "root")["version"], 1);
}
