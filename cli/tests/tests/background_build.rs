//! The background index build a hook starts for a repository whose relations
//! it left out: one per user at a time, ended with its Claude Code session.
//! `build.lock` is one per user, so the cases run in order inside one test,
//! in a test binary of their own.

use serde_json::{json, Value};
use std::fs::File;
use std::process::Command;
use std::time::{Duration, Instant};
use tracer_cli_tests::{hold_index_lock, schema_directory, sessions_root, Fixture, PUBLISHED_SCHEMA_VERSION};

fn repo() -> Fixture {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\n");
    f.write("src/lib.rs", "pub mod shop;\n\npub fn total() -> u32 {\n    1\n}\n");
    f.write("src/shop.rs", "pub fn sum() -> u32 {\n    crate::total()\n}\n");
    f.commit("shop");
    f
}

fn read(f: &Fixture) -> Value {
    json!({
        "session_id": "build",
        "cwd": f.root.to_string_lossy(),
        "tool_name": "Read",
        "tool_input": {"file_path": f.path("src/lib.rs")},
        "tool_response": {},
    })
}

/// Waits until no build holds `build.lock`, then holds it.
fn take_build_lock() -> File {
    let path = sessions_root().parent().unwrap().join("build.lock");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = File::options().create(true).truncate(false).write(true).open(path).unwrap();
    let until = Instant::now() + Duration::from_secs(120);
    while lock.try_lock().is_err() {
        assert!(Instant::now() < until, "another build held build.lock for two minutes");
        std::thread::sleep(Duration::from_millis(100));
    }
    lock
}

/// Runs a hook that leaves the relations out until it starts the build, since
/// a live agent's build may take `build.lock` between the wait and the hook.
fn start_build(f: &Fixture, env: &[(&str, &str)]) {
    eventually(Duration::from_secs(120), "a hook starts the build", || {
        drop(take_build_lock());
        f.hook("PostToolUse", "claude", &read(f), &[], env).ok();
        f.building()
    });
}

/// Whether the running build has the lowest scheduling priority, niceness 19.
fn at_lowest_priority(f: &Fixture) -> bool {
    let found = Command::new("pgrep").args(["-f", &f.build_pattern()]).output().unwrap();
    let pid = String::from_utf8(found.stdout).unwrap().lines().next().expect("a build is running").to_string();
    let shown = Command::new("ps").args(["-o", "ni=", "-p", &pid]).output().unwrap();
    String::from_utf8(shown.stdout).unwrap().trim() == "19"
}

fn eventually(within: Duration, what: &str, condition: impl Fn() -> bool) {
    let until = Instant::now() + within;
    while !condition() {
        assert!(Instant::now() < until, "{what} within {within:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_hook_that_leaves_the_relations_out_starts_one_build_that_ends_with_its_session() {
    // A hook that leaves the relations out starts their build, which
    // finishes them with no further call.
    let f = repo();
    let index = schema_directory(&f.root, PUBLISHED_SCHEMA_VERSION).join("relations_edges_v1.json");
    let relations = hold_index_lock(&f.root, "relations_edges");
    start_build(&f, &[]);
    assert!(at_lowest_priority(&f), "the build runs at the lowest priority");
    drop(relations);
    eventually(Duration::from_secs(60), "the build stores the relations index", || index.is_file());
    eventually(Duration::from_secs(30), "the build ends", || !f.building());

    // A hook whose relations are ready starts nothing.
    let warm = f.hook("PostToolUse", "claude", &read(&f), &[], &[("TRACE_TIMING", "1")]);
    assert!(!warm.stderr.contains("timing detach cache build"), "a warm hook starts no build");

    // While another build runs, a hook that leaves the relations out starts
    // none of its own.
    let other = repo();
    let running = take_build_lock();
    let relations = hold_index_lock(&other.root, "relations_edges");
    let blocked = other.hook("PostToolUse", "claude", &read(&other), &[], &[("TRACE_TIMING", "1")]);
    assert!(blocked.stderr.contains("timing detach cache build"), "the hook tried to start the build");
    assert!(!other.building(), "one build runs per user");
    drop(relations);
    drop(running);

    // The build ends once the Claude Code session that started it ends.
    let last = repo();
    let mut claude = Command::new("sleep").arg("60").spawn().unwrap();
    let relations = hold_index_lock(&last.root, "relations_edges");
    start_build(&last, &[("CLAUDE_PID", &claude.id().to_string())]);
    claude.kill().unwrap();
    claude.wait().unwrap();
    eventually(Duration::from_secs(2), "the build ends with its session", || !last.building());
    drop(relations);
    drop(take_build_lock());
}
