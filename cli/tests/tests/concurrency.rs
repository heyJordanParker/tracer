//! Many agents call `trace` against one repository at the same time. Every
//! call must answer from the bytes on disk, and the calls must not each pay
//! for the same index update: one process updates, the rest read its result.

use std::fs;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};
use tracer_cli_tests::{
    hold_index_lock, schema_directory, standard_repo, trace, trace_env, Fixture, PUBLISHED_SCHEMA_VERSION,
};

/// An answer never leaves a relation out: `callers` waits while another
/// call builds the relations index and answers in full once it lands.
#[test]
fn an_answer_waits_for_the_index_another_call_builds() {
    let f = standard_repo();
    let building = hold_index_lock(&f.root, "relations_edges");
    let root = f.root.clone();
    let callers = thread::spawn(move || trace(&root, ["callers", "helper"]));
    thread::sleep(Duration::from_millis(1500));
    assert!(!callers.is_finished(), "callers answered without the index");
    drop(building);
    let run = callers.join().unwrap();
    run.ok();
    assert!(run.stdout.contains("src/app.py") && !run.stdout.contains("still building"), "{}", run.stdout);
}

fn mtime_index(fixture: &Fixture) -> String {
    fs::read_to_string(
        schema_directory(&fixture.root, PUBLISHED_SCHEMA_VERSION).join("mtime_index_v2.json"),
    )
    .expect("the mtime index exists after a call")
}

/// An agent runs several `trace` calls side by side, each with its own docs
/// hook, so one session's doc deliveries take turns: no two calls send the
/// same lines of a long doc.
#[test]
fn concurrent_doc_deliveries_never_send_the_same_lines_twice() {
    let fixture = Fixture::new();
    let rules: String = (1..=600)
        .map(|n| format!("- Rule {n}: one line of a doc too long for one message.\n"))
        .collect();
    fixture.write("Claude.md", &format!("# Rules\n{rules}"));
    fixture.write("x.py", "x = 1\n");
    fixture.commit("long doc");
    let env = [("CLAUDE_CODE_SESSION_ID", "concurrent-docs")];

    let mut sent: Vec<usize> = Vec::new();
    for _ in 0..4 {
        let runs = thread::scope(|scope| {
            let calls: Vec<_> = (0..6)
                .map(|_| scope.spawn(|| fixture.trace_env(&["docs", "x.py", "--budget", "2000"], &env)))
                .collect();
            calls.into_iter().map(|call| call.join().unwrap()).collect::<Vec<_>>()
        });
        for run in runs {
            run.ok();
            sent.extend(
                run.stdout
                    .lines()
                    .filter_map(|line| line.strip_prefix("- Rule "))
                    .filter_map(|rest| rest.split(':').next()?.parse::<usize>().ok()),
            );
        }
    }
    let mut unique = sent.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(sent.len(), unique.len(), "a rule was sent to two calls");
    assert_eq!(unique, (1..=unique.len()).collect::<Vec<_>>(), "the doc arrives in order, with no gap");
}

/// The same for the docs a command sends after its answer, through
/// `history`, which offers nothing else that would take the lock first.
#[test]
fn concurrent_commands_never_send_the_same_doc_lines_twice() {
    let fixture = Fixture::new();
    let rules: String = (1..=600)
        .map(|n| format!("- Rule {n}: one line of a doc too long for one message.\n"))
        .collect();
    fixture.write("Claude.md", &format!("# Rules\n{rules}"));
    fixture.write("x.py", "x = 1\n");
    fixture.commit("long doc");
    let env = [("CLAUDE_CODE_SESSION_ID", "concurrent-command-docs")];

    let mut sent: Vec<usize> = Vec::new();
    for _ in 0..4 {
        let runs = thread::scope(|scope| {
            let calls: Vec<_> = (0..6)
                .map(|_| scope.spawn(|| fixture.trace_env(&["history", "x.py", "--budget", "2000"], &env)))
                .collect();
            calls.into_iter().map(|call| call.join().unwrap()).collect::<Vec<_>>()
        });
        for run in runs {
            run.ok();
            sent.extend(
                run.stdout
                    .lines()
                    .filter_map(|line| line.strip_prefix("- Rule "))
                    .filter_map(|rest| rest.split(':').next()?.parse::<usize>().ok()),
            );
        }
    }
    let mut unique = sent.clone();
    unique.sort();
    unique.dedup();
    assert!(!unique.is_empty(), "the doc is sent");
    assert_eq!(sent.len(), unique.len(), "a rule was sent to two calls");
    assert_eq!(unique, (1..=unique.len()).collect::<Vec<_>>(), "the doc arrives in order, with no gap");
}

/// Two processes that each learn a file at the same moment both land in the
/// mtime index: the update merges onto what is on disk under the mtime
/// index's lock, so neither write erases the other's.
#[test]
fn two_concurrent_calls_both_land_in_the_mtime_index() {
    let f = standard_repo();
    f.trace(&["context"]).ok();
    f.write("src/one.py", "def one():\n    return 1\n");
    f.write("src/two.py", "def two():\n    return 2\n");

    let root_a = f.root.clone();
    let root_b = f.root.clone();
    let a = thread::spawn(move || trace(&root_a, ["structure", "src/one.py", "--json"]));
    let b = thread::spawn(move || trace(&root_b, ["structure", "src/two.py", "--json"]));
    a.join().unwrap().ok();
    b.join().unwrap().ok();

    let index = mtime_index(&f);
    assert!(
        index.contains("src/one.py") && index.contains("src/two.py"),
        "one concurrent write erased the other's entry:\n{index}"
    );
}

/// Two installed builds at different schemas update their own indexes side by
/// side: an update holds only the locks in its own schema's directory, so a
/// build of a neighbor schema, or one from before per-index locks, holding
/// its lock mid-update never stalls it.
#[test]
fn an_index_update_never_waits_on_another_schemas_lock() {
    let f = standard_repo();
    f.trace(&["cache", "build", "."]).ok();
    let file_namespace = f.root.join(".tracer-cache/file");
    let below = schema_directory(&f.root, PUBLISHED_SCHEMA_VERSION - 1);
    let above = schema_directory(&f.root, PUBLISHED_SCHEMA_VERSION + 1);
    let held: Vec<fs::File> = [
        file_namespace.join(".maintain.lock"),
        file_namespace.join(format!(".maintain__schema{PUBLISHED_SCHEMA_VERSION}.lock")),
        below.join(".maintain.lock"),
        above.join(".maintain.lock"),
        below.join(".relations_edges.lock"),
        above.join(".relations_edges.lock"),
        below.join(".mtime_index.lock"),
        above.join(".mtime_index.lock"),
    ]
    .iter()
    .map(|path| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let lock = fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .unwrap();
        lock.lock().unwrap();
        lock
    })
    .collect();
    f.write("src/added.py", "def added():\n    return 1\n");

    let mut child = f
        .command(&f.root, ["grep", "added", "src"], &[("TRACE_TIMING", "1")])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() >= Duration::from_secs(10) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the index update waited 10 seconds on another schema's lock");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let run = child.wait_with_output().unwrap();
    drop(held);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "the call failed: {stderr}");
    assert!(
        stderr.contains("timing lock relations_edges "),
        "the call took no index lock, so it proved nothing about waiting:\n{stderr}"
    );
}

/// Eight calls arriving right after a file appears cost about one update,
/// not eight: the first holder updates the indexes and the others wait for
/// it and read the result. Every call answers cleanly, with nothing on stderr.
#[test]
fn eight_concurrent_calls_after_an_added_file_cost_about_one_call() {
    let f = standard_repo();
    f.trace(&["context"]).ok();
    f.write("src/first.py", "def first():\n    return 1\n");
    let one = f.trace(&["grep", "zzqq_nomatch", "src"]);
    one.ok();

    f.write("src/second.py", "def second():\n    return 2\n");
    let started = std::time::Instant::now();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let root = f.root.clone();
            thread::spawn(move || trace(&root, ["grep", "zzqq_nomatch", "src"]))
        })
        .collect();
    let runs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let eight = started.elapsed();

    for run in &runs {
        run.ok();
        assert!(
            run.stderr.is_empty(),
            "stderr must stay empty: {}",
            run.stderr
        );
    }
    let budget = one.elapsed * 3 + Duration::from_millis(500);
    assert!(
        eight <= budget,
        "eight concurrent calls took {eight:?}; one call after an added file took {:?}",
        one.elapsed
    );
}

/// Eight `history --contains` calls on a repository nobody indexed walk its
/// commits once: the first holder walks them, and the rest read its index.
#[test]
fn eight_concurrent_history_searches_walk_the_commits_once() {
    let f = standard_repo();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let root = f.root.clone();
            thread::spawn(move || {
                trace_env(&root, ["history", "--contains", "def", "--json"], &[("TRACE_TIMING", "1")])
            })
        })
        .collect();
    let runs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let mut walks = 0;
    for run in &runs {
        run.ok();
        walks += run.stderr.matches("timing git log --no-walk=unsorted ").count();
    }
    assert_eq!(walks, 1, "the commits were walked {walks} times");
}

#[test]
fn concurrent_agents_share_one_parseable_directory_baseline() {
    let f = standard_repo();
    let f = &f;
    thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|agent| {
                scope.spawn(move || {
                    let agent_id = format!("agent-{agent}");
                    f.trace_env(
                        &["context", "src/app.py"],
                        &[("AGENT_SESSION_ID", "directory-baseline"), ("TRACER_AGENT_ID", agent_id.as_str())],
                    )
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap().ok();
        }
    });

    let path = f.session_dir("directory-baseline").join("directories.json");
    let text = fs::read_to_string(&path).expect("directories.json exists");
    let directories: serde_json::Value =
        serde_json::from_str(&text).expect("directories.json parses");
    assert_eq!(
        directories.as_object().unwrap().len(),
        1,
        "all agents must share the one directory entry: {directories}"
    );
    let key = format!("{}/src/", f.root.canonicalize().unwrap().display());
    assert!(directories.get(&key).is_some(), "{directories}");
}

/// Calls surfacing one directory at the same moment show its listing to the
/// Agent exactly once: the record is re-read under the session lock, so no two
/// calls both claim the first showing.
#[test]
fn concurrent_calls_show_a_directory_listing_once() {
    let f = standard_repo();
    f.trace(&["cache", "build", "."]).ok();
    let env = [("AGENT_SESSION_ID", "listing-once")];
    let runs: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| f.trace_env(&["context", "src/app.py"], &env)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for run in &runs {
        run.ok();
    }
    let listed = runs.iter().filter(|run| run.stdout.contains("  entries: ")).count();
    assert_eq!(listed, 1, "the listing must show exactly once across concurrent calls");
}

/// Claude Code sends one Agent's `Read`s in parallel, and each runs a windowed
/// `context` on its own file. Each call resolves its `calls:` block before it
/// takes the Agent's lock, so a call waits on the others only for their
/// compare, print, and save, never for their resolution. `TRACE_TIMING` prints
/// a phase's line when the phase ends, so each call's `calls` line comes before
/// its `lock session` line. A wait measured against render time failed on a slow
/// runner once render got faster than the locked compare, print, and save.
#[test]
fn one_agents_parallel_windowed_calls_never_wait_on_each_others_calls() {
    let f = Fixture::new();
    let methods: Vec<String> = (0..16).map(|m| format!("record{m}")).collect();
    let declared: String = methods
        .iter()
        .map(|method| format!("    public function {method}(int $cents): int\n    {{\n        return $cents;\n    }}\n"))
        .collect();
    f.write("ledger.php", &format!("<?php\nclass Ledger {{\n{declared}}}\n"));
    let called: String = methods
        .iter()
        .map(|method| format!("        $this->ledger->{method}($cents);\n"))
        .collect();
    for n in 0..6 {
        f.write(
            &format!("checkout{n}.php"),
            &format!("<?php\nclass Checkout{n} {{\n    public function __construct(private Ledger $ledger) {{}}\n    public function pay(int $cents): void\n    {{\n{called}    }}\n}}\n"),
        );
    }
    for n in 0..100 {
        let called: String = methods
            .iter()
            .map(|method| format!("        $ledger->{method}(-$cents - {n});\n"))
            .collect();
        f.write(
            &format!("refund{n}.php"),
            &format!("<?php\nclass Refund{n} {{\n    public function back(Ledger $ledger, int $cents): void\n    {{\n{called}    }}\n}}\n"),
        );
    }
    f.commit("many callers");
    f.trace(&["cache", "build", "."]).ok();
    let env = [("AGENT_SESSION_ID", "parallel-windows"), ("TRACE_TIMING", "1")];

    let f = &f;
    let runs: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..6)
            .map(|n| {
                scope.spawn(move || {
                    let file = format!("checkout{n}.php");
                    f.trace_env(&["context", file.as_str(), "--offset", "6", "--limit", "16", "--no-record"], &env)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for run in &runs {
        run.ok();
        assert!(run.stdout.contains("Ledger::record15"), "the window resolves its calls:\n{}", run.stdout);
        let lines: Vec<&str> = run.stderr.lines().collect();
        let resolved = lines.iter().position(|line| line.starts_with("timing calls "));
        let locked = lines
            .iter()
            .position(|line| line.strip_prefix("timing lock session ").is_some_and(|wait| wait.parse::<u128>().is_ok()));
        assert!(
            matches!((resolved, locked), (Some(resolved), Some(locked)) if resolved < locked),
            "each call resolves its calls: block before it takes the Agent's lock:\n{}",
            run.stderr
        );
    }
}

/// A primer must finish when its warm cache learns a newly tracked directory.
#[test]
fn warm_context_avoids_deadlock() {
    let f = standard_repo();

    for directory in 0..40 {
        for source in 0..15 {
            let path = format!("layer_{directory}/source_{source}.py");
            let body = if directory == 0 && source == 0 {
                "value = 1\n".to_owned()
            } else {
                let previous_directory = if source == 0 {
                    directory - 1
                } else {
                    directory
                };
                let previous_source = if source == 0 { 14 } else { source - 1 };
                format!(
                    "from layer_{previous_directory}.source_{previous_source} import value\nvalue = value + 1\n"
                )
            };
            f.write(&path, &body);
        }
    }
    f.git(&["add", "."]);
    f.commit("add parallel layout sources");

    for attempt in 1..=20 {
        f.trace(&["cache", "build", "."]).ok();
        let path = format!("added-{attempt}/source.py");
        f.write(&path, "def source():\n    return 1\n");
        f.git(&["add", &path]);

        let mut child = f
            .command(&f.root, ["context"], &[("RAYON_NUM_THREADS", "2")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() >= Duration::from_secs(10) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("context attempt {attempt} exceeded the 10-second deadline");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "context attempt {attempt} failed: {status}"
        );
    }
}
