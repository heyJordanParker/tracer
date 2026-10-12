//! `trace hook <event>`: everything tracer does on a harness's hook event,
//! once, for every harness. It reads the hook's input as JSON on stdin and
//! prints its answer as JSON on stdout: extra context, a rewritten tool
//! input, or a refusal.
//!
//! - `harness.rs` reads each harness's input: its tool names and fields.
//! - `events.rs` is the event table: what each event does.
//! - `refusal.rs` is `--tracer-only`'s refusal of raw code reads.
//!
//! The answer fits `BUDGET`, the size Claude Code shows whole, and is
//! recorded as shown only once it is flushed before `--deadline-ms`. A
//! repository whose relations the answer left out gets a background build.

mod events;
mod harness;
mod refusal;

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

pub use harness::Harness;

/// The characters one hook answer holds: Claude Code shows a longer one as
/// a file path and a preview.
const BUDGET: usize = 10_000;

/// One hook event, read from the harness's input.
pub struct Input {
    pub event: String,
    pub harness: Harness,
    pub cwd: PathBuf,
    /// The Subagent the event comes from, `None` for the main agent.
    pub agent: Option<String>,
    /// SessionStart's `startup`, `resume`, `clear`, or `compact`.
    pub source: String,
    /// The doc InstructionsLoaded names.
    pub loaded: Option<PathBuf>,
    pub tool: Option<harness::Tool>,
    pub tracer_only: bool,
}

/// What a hook answers.
#[derive(Default)]
pub struct Answer {
    pub context: String,
    pub updated_input: Option<Value>,
    pub refusal: Option<String>,
}

pub fn run(event: &str, harness: Harness, deadline_ms: u64, tracer_only: bool) -> Result<()> {
    crate::process::deadline(Duration::from_millis(deadline_ms));
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    // Exit 1 is a hook error the harness reports without blocking the tool.
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| crate::process::fail(1, format!("trace hook {event}: the input is not JSON: {error}")))?;
    if let Some(session) = value["session_id"].as_str() {
        crate::session::set_session(session.to_string());
    }
    let agent = value["agent_id"].as_str().filter(|agent| !agent.is_empty()).map(str::to_string);
    if let Some(agent) = &agent {
        crate::session::set_agent(agent.clone());
    }
    let cwd = value["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let _ = std::env::set_current_dir(&cwd);
    let input = Input {
        event: event.to_string(),
        harness,
        cwd: std::env::current_dir().unwrap_or(cwd),
        agent,
        source: value["source"].as_str().unwrap_or_default().to_string(),
        loaded: value["file_path"].as_str().map(PathBuf::from),
        tool: harness.tool(&value),
        tracer_only,
    };
    let mut delivery = crate::session::Delivery::default();
    // The notice of an index part left out follows the answer, so its room
    // is kept from the start.
    let notice = crate::cache::worktree_root_for(&input.cwd).map_or(0, |root| crate::relations::notice(&root).len());
    let mut answer = crate::output::within(Some(BUDGET - notice), || events::answer(&input, &mut delivery))?;
    let notice = crate::output::index_state(&mut delivery);
    if !notice.is_empty() {
        answer.context = format!("{}\n{notice}", answer.context.trim_end_matches('\n'));
    }
    // A failure the answer reports reaches the agent in the answer, and the
    // hook exits 0: a harness drops the answer of a hook that exits 2.
    let failures = crate::process::take_reported();
    if !failures.is_empty() {
        answer.context = format!("{}\n{}\n", answer.context.trim_end_matches('\n'), failures.join("\n"));
    }
    if let Some(printed) = answered(event, &answer) {
        let mut stdout = std::io::stdout();
        writeln!(stdout, "{printed}")?;
        stdout.flush()?;
    }
    delivery.commit();
    build_left_out();
    Ok(())
}

/// Starts a `trace cache build` apart from this hook for each repository
/// whose relations this answer left out, so a later event finds them built.
/// One build runs per user at a time; a start that finds one running is
/// dropped, and a later event that still leaves relations out starts it. A
/// repository whose cache this user cannot save gets none, because its build
/// would keep nothing and run again on every event.
fn build_left_out() {
    let roots = crate::relations::left_out_roots();
    if roots.is_empty() {
        return;
    }
    let (Ok(trace), Some(folder)) = (std::env::current_exe(), crate::session::user_folder()) else {
        return;
    };
    let lock = folder.join("build.lock");
    for root in roots.into_iter().filter(|root| crate::cache::can_persist(root)) {
        let mut build = std::process::Command::new(&trace);
        build.arg("-C").arg(&root).args(["cache", "build"]);
        let _ = crate::timing::phase("detach cache build", || crate::process::detach(&mut build, &lock));
    }
}

/// The JSON a harness reads for `answer`, `None` when there is nothing to say.
fn answered(event: &str, answer: &Answer) -> Option<Value> {
    let mut output = serde_json::Map::new();
    output.insert("hookEventName".into(), event.into());
    if let Some(reason) = &answer.refusal {
        output.insert("permissionDecision".into(), "deny".into());
        output.insert("permissionDecisionReason".into(), reason.clone().into());
    }
    if let Some(input) = &answer.updated_input {
        output.insert("updatedInput".into(), input.clone());
    }
    let context = answer.context.trim_matches('\n');
    if !context.is_empty() {
        output.insert("additionalContext".into(), context.into());
    }
    (output.len() > 1).then(|| json!({ "hookSpecificOutput": output }))
}
