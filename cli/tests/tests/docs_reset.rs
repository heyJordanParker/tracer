//! `trace docs reset` — clears the current session's surfaced-docs state so a
//! subsequent `trace docs <path>` re-surfaces docs as new. This is the seam the
//! Codex compaction/clear hook drives: after a context reset drops injected
//! rule text from the model, the surfaced-docs state must reset so the rules
//! re-inject instead of being skipped as already-loaded.
//!
//! Pins the contract:
//!   - after surfacing docs then resetting, a follow-up surfacing returns the
//!     docs as new again (not in `already_loaded`)
//!   - the reset is a clean no-op when no session is active
//!   - the reset empties the agent's whole record: its docs, its read lines,
//!     and the facts and listings it was shown
//!
//! No tracer internals are linked — the only contract is the CLI's observable
//! surface plus the documented on-disk shape.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracer_cli_tests::Fixture;

#[allow(non_upper_case_globals)] // project naming rule bans ALL_CAPS for our own identifiers
static reset_seq: AtomicU64 = AtomicU64::new(0);

fn fresh_session_id(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = reset_seq.fetch_add(1, Ordering::SeqCst);
    format!("trace-docs-reset-{tag}-{nanos}-{seq}")
}

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

// --- the core re-surface contract -----------------------------------------

#[test]
fn surfacing_after_reset_returns_docs_as_new_again() {
    // The defining behavior: surface docs, reset, surface again → the second
    // surfacing must return the full chain as `docs` (new), with nothing in
    // `already_loaded`. This is what makes rules re-inject after a Codex
    // compaction drops them from context.
    let f = docs_repo();
    let sid = fresh_session_id("re-surface");
    let env = [("CLAUDE_CODE_SESSION_ID", sid.as_str())];

    // 1. First surfacing: the full chain is new.
    let first = f.trace_env(&["docs", "sub/util.py", "--json"], &env);
    first.ok();
    assert_eq!(first.view()["docs"].as_i64().unwrap(), 2);

    // 2. Second surfacing without a reset: nothing new, both already_loaded.
    let pre_reset = f.trace_env(&["docs", "sub/util.py", "--json"], &env);
    pre_reset.ok();
    let v = pre_reset.view();
    assert_eq!(
        v["docs"].as_i64().unwrap(),
        0,
        "without a reset the chain is already in the log: {v}"
    );
    assert_eq!(v["already_loaded"].as_array().unwrap().len(), 2);

    // 3. Reset.
    let reset = f.trace_env(&["docs", "reset", "--json"], &env);
    reset.ok();
    let rv = reset.view();
    assert_eq!(rv["scope"].as_str().unwrap(), "reset", "{rv}");
    assert_eq!(rv["session_active"].as_bool().unwrap(), true, "{rv}");
    assert_eq!(
        rv["cleared"].as_i64().unwrap(),
        2,
        "reset must report the two surfaced docs it cleared: {rv}"
    );
    assert_eq!(rv["source"].as_str().unwrap(), "trace_docs_reset", "{rv}");

    // 4. Surfacing after the reset: the full chain is new AGAIN — nothing
    //    skipped as already_loaded.
    let after = f.trace_env(&["docs", "sub/util.py", "--json"], &env);
    after.ok();
    let av = after.view();
    assert_eq!(
        av["docs"].as_i64().unwrap(),
        2,
        "after a reset the chain must re-surface as new: {av}"
    );
    assert!(
        av["already_loaded"].as_array().unwrap().is_empty(),
        "after a reset nothing must be reported already_loaded: {av}"
    );
}

#[test]
fn custom_source_lands_on_the_reset_response() {
    // The Codex hook stamps the calling surface via `--source`. The value
    // round-trips into the response so the hook can confirm its own call.
    let f = docs_repo();
    let sid = fresh_session_id("source");
    let env = [("CLAUDE_CODE_SESSION_ID", sid.as_str())];

    f.trace_env(&["docs", "sub/util.py", "--json"], &env).ok();

    let r = f.trace_env(
        &["docs", "reset", "--source", "codex_compact_hook", "--json"],
        &env,
    );
    r.ok();
    let v = r.view();
    assert_eq!(v["source"].as_str().unwrap(), "codex_compact_hook", "{v}");
    assert_eq!(v["cleared"].as_i64().unwrap(), 2, "{v}");
}

// --- no-op when no session is active --------------------------------------

#[test]
fn reset_is_a_clean_noop_when_no_session_is_active() {
    // Standalone tracer use: no AGENT_SESSION_ID / CODEX_THREAD_ID /
    // CLAUDE_CODE_SESSION_ID → the log no-ops, so reset succeeds with a
    // structured "nothing cleared" answer rather than erroring or writing any
    // cache.
    let f = docs_repo();
    let r = f.trace_env(&["docs", "reset", "--json"], &[("TRACE_TIMING", "1")]);
    r.ok();
    let v = r.json();
    assert_eq!(v["query"]["scope"].as_str().unwrap(), "reset", "{v}");
    assert_eq!(
        v["context"]["session_active"].as_bool().unwrap(),
        false,
        "no session id ⇒ session_active false: {v}"
    );
    assert_eq!(
        v["counts"]["cleared"].as_i64().unwrap(),
        0,
        "no session id ⇒ nothing cleared: {v}"
    );
    assert!(
        !r.stderr.contains("timing lock session") && !r.stderr.contains("timing session record"),
        "standalone reset must write no session record:\n{}",
        r.stderr
    );
}

#[test]
fn reset_before_anything_surfaces_clears_nothing() {
    // Active session but no record yet → reset is still a clean no-op
    // reporting zero cleared, and writes no record.
    let f = docs_repo();
    let sid = fresh_session_id("empty");
    let env = [("CLAUDE_CODE_SESSION_ID", sid.as_str())];

    let r = f.trace_env(&["docs", "reset", "--json"], &env);
    r.ok();
    let v = r.view();
    assert_eq!(v["session_active"].as_bool().unwrap(), true, "{v}");
    assert_eq!(
        v["cleared"].as_i64().unwrap(),
        0,
        "an empty log clears nothing: {v}"
    );
    assert!(f.record(&sid, "root").is_none(), "reset with no record must write none");
}

#[test]
fn reset_clears_shown_facts_and_listings_when_no_docs_view_exists() {
    let f = Fixture::new();
    f.write("app/one.py", "def one():\n    return 1\n");
    f.commit("one file, no docs");
    let sid = fresh_session_id("shown-only");
    let env = [("CLAUDE_CODE_SESSION_ID", sid.as_str())];
    let whole = "---\nfile: app/one.py\n";
    let listed = "\n  entries: [one.py]\n";

    let first = f.trace_env(&["context", "app/one.py", "--no-record"], &env);
    first.ok();
    assert!(first.stdout.starts_with(whole) && first.stdout.contains(listed), "{}", first.stdout);
    let repeat = f.trace_env(&["context", "app/one.py", "--no-record"], &env);
    repeat.ok();
    assert!(repeat.stdout.starts_with("# app/one.py  {"), "{}", repeat.stdout);
    assert!(!repeat.stdout.contains(listed), "{}", repeat.stdout);
    let held = f.record(&sid, "root").expect("the facts and listing are recorded");
    assert_eq!(held["files"], serde_json::json!({}), "the Agent must hold only listings and facts: {held}");

    f.trace_env(&["docs", "reset"], &env).ok();
    let after_reset = f.trace_env(&["context", "app/one.py", "--no-record"], &env);
    after_reset.ok();
    assert!(
        after_reset.stdout.starts_with(whole) && after_reset.stdout.contains(listed),
        "{}",
        after_reset.stdout
    );
}

// --- the whole record clears -----------------------------------------------

#[test]
fn reset_empties_the_whole_record() {
    let f = docs_repo();
    let sid = fresh_session_id("whole");
    let env = [("CLAUDE_CODE_SESSION_ID", sid.as_str())];

    f.trace_env(&["docs", "sub/util.py", "--json"], &env).ok();
    f.trace_env(&["context", "sub/util.py"], &env).ok();
    let before = f.record(&sid, "root").unwrap();
    assert_eq!(before["files"].as_object().unwrap().len(), 3, "{before}");
    assert!(!before["shown"].as_object().unwrap().is_empty(), "{before}");

    f.trace_env(&["docs", "reset", "--json"], &env).ok();
    let after = f.record(&sid, "root").unwrap();
    assert_eq!(after["files"], serde_json::json!({}), "{after}");
    assert_eq!(after["shown"], serde_json::json!({}), "{after}");
}
