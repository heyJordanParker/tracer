//! `trace hook <event>`: every row of the event table, driven through the
//! hook's own stdin and stdout, the way a harness drives it.

use serde_json::{json, Value};
use tracer_cli_tests::Fixture;

const SESSION: &str = "hook-session";

const LIB: &str = "pub mod shop;

pub fn total(prices: &[u32]) -> u32 {
    prices.iter().sum()
}

pub fn discounted(prices: &[u32], percent: u32) -> u32 {
    let sum = total(prices);
    if percent > 100 {
        return 0;
    }
    sum - sum * percent / 100
}
";

const SHOP: &str = "use crate::{discounted, total};

pub struct Cart {
    pub prices: Vec<u32>,
}

impl Cart {
    pub fn sum(&self) -> u32 {
        total(&self.prices)
    }

    pub fn sale(&self) -> u32 {
        discounted(&self.prices, 20)
    }
}
";

fn shop() -> Fixture {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\n");
    f.write("Claude.md", "# Shop\n\nPrices are whole cents.\n");
    f.write("src/lib.rs", LIB);
    f.write("src/shop.rs", SHOP);
    f.commit("shop");
    f
}

/// A hook input for `event` in the fixture, with `extra` fields merged in.
fn input(f: &Fixture, extra: Value) -> Value {
    let mut base = json!({"session_id": SESSION, "cwd": f.root.to_string_lossy()});
    base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    base
}

fn read(f: &Fixture, rel: &str) -> Value {
    input(f, json!({"tool_name": "Read", "tool_input": {"file_path": f.path(rel)}, "tool_response": {}}))
}

fn held(f: &Fixture, agent: &str) -> Vec<String> {
    let record = f.record(SESSION, agent).unwrap_or(Value::Null);
    record["files"].as_object().map(|files| files.keys().cloned().collect()).unwrap_or_default()
}

/// A separate HOME, so its `.claude/rules` are user-global rules and not
/// the project's.
fn home_with_rules(f: &Fixture, rules: &[(&str, &str)]) -> String {
    let home = f.tmp.join("home");
    for (name, body) in rules {
        let path = home.join(".claude/rules").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    std::fs::create_dir_all(&home).unwrap();
    home.to_string_lossy().into_owned()
}

#[test]
fn session_start_offers_the_primer_and_a_resume_that_holds_it_gets_nothing() {
    let f = shop();
    let start = f.hook("SessionStart", "claude", &input(&f, json!({"source": "startup"})), &[], &[]);
    start.ok();
    assert!(start.context().contains("## Environment"), "startup prints the primer: {}", start.stdout);
    let resume = f.hook("SessionStart", "claude", &input(&f, json!({"source": "resume"})), &[], &[]);
    resume.ok();
    assert_eq!(resume.stdout, "", "a resume finds the primer the same and says nothing");
}

#[test]
fn session_start_clear_forgets_what_the_agent_held_and_sends_the_primer_again() {
    let f = shop();
    f.hook("SessionStart", "claude", &input(&f, json!({"source": "startup"})), &[], &[]).ok();
    f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[]).ok();
    let clear = f.hook("SessionStart", "claude", &input(&f, json!({"source": "clear"})), &[], &[]);
    assert!(clear.context().contains("## Environment"), "{}", clear.stdout);
    assert!(!held(&f, "root").iter().any(|path| path.ends_with("src/lib.rs")), "the read is forgotten");
    assert!(held(&f, "root").iter().any(|path| path.ends_with("Claude.md")), "the start docs are recorded again");
}

#[test]
fn a_compaction_forgets_every_rule_and_sends_none_because_claude_code_loads_them_back() {
    let f = shop();
    let home = home_with_rules(&f, &[("rust.md", "---\npaths: [\"**/*.rs\"]\n---\n\nReturn early.\n")]);
    let env = [("HOME", home.as_str())];
    f.hook("SessionStart", "claude", &input(&f, json!({"source": "startup"})), &[], &env).ok();
    let rule = input(&f, json!({"file_path": format!("{home}/.claude/rules/rust.md"), "load_reason": "path_glob_match"}));
    f.hook("InstructionsLoaded", "claude", &rule, &[], &env).ok();
    f.hook("PreCompact", "claude", &input(&f, json!({"trigger": "auto"})), &[], &env).ok();
    let after = f.hook("SessionStart", "claude", &input(&f, json!({"source": "compact"})), &[], &env);
    assert!(!after.context().contains("Return early."), "{}", after.context());
    let reloaded = f.hook("InstructionsLoaded", "claude", &rule, &[], &env);
    reloaded.ok();
    assert_eq!(reloaded.stdout, "", "Claude Code's own reload of the rule goes through");
}

#[test]
fn a_doc_the_agent_holds_whole_is_refused_when_claude_code_loads_it_again() {
    let f = shop();
    let doc = f.write("src/Claude.md", "# Source\n\nTotals round half up.\n");
    let loaded = input(&f, json!({"file_path": doc.to_string_lossy(), "load_reason": "nested_traversal"}));
    f.trace_env(&["read", "src/lib.rs"], &[("AGENT_SESSION_ID", SESSION)]).ok();
    let refused = f.hook("InstructionsLoaded", "claude", &loaded, &[], &[]);
    refused.ok();
    assert_eq!(refused.json()["hookSpecificOutput"]["permissionDecision"], "deny", "{}", refused.stdout);
    f.write("src/Claude.md", "# Source\n\nTotals round down.\n");
    let changed = f.hook("InstructionsLoaded", "claude", &loaded, &[], &[]);
    changed.ok();
    assert_eq!(changed.stdout, "", "a doc changed since it was sent loads whole");
}

#[test]
fn codex_starts_with_the_folder_docs_and_rules_it_never_loads_itself() {
    let f = shop();
    f.write("AGENTS.md", "# Agents\n");
    let home = home_with_rules(&f, &[("always.md", "Short sentences.\n")]);
    let codex_home = f.tmp.join("codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let env = [("HOME", home.as_str()), ("CODEX_HOME", codex_home.to_str().unwrap())];
    let start = f.hook("SessionStart", "codex", &input(&f, json!({"source": "startup"})), &[], &env);
    let context = start.context();
    assert!(context.contains("Short sentences."), "the user rule is sent: {context}");
    assert!(context.contains("## Claude.md"), "Codex loads AGENTS.md, never Claude.md: {context}");
    assert!(!context.contains("## AGENTS.md"), "AGENTS.md is what Codex loaded: {context}");
}

#[test]
fn a_subagent_starts_with_its_own_record_of_the_start_docs() {
    let f = shop();
    f.hook("SubagentStart", "claude", &input(&f, json!({"agent_id": "sub-1", "agent_type": "explorer"})), &[], &[]).ok();
    assert!(held(&f, "sub-1").iter().any(|path| path.ends_with("Claude.md")), "{:?}", held(&f, "sub-1"));
    assert!(held(&f, "root").is_empty(), "the main agent's record is untouched");
}

#[test]
fn a_doc_the_harness_loads_is_recorded_under_the_agent_that_loaded_it() {
    let f = shop();
    let doc = f.write("src/Claude.md", "# Source\n");
    let loaded = input(&f, json!({"agent_id": "sub-2", "file_path": doc.to_string_lossy(), "load_reason": "nested_traversal"}));
    f.hook("InstructionsLoaded", "claude", &loaded, &[], &[]).ok();
    assert!(held(&f, "sub-2").iter().any(|path| path.ends_with("src/Claude.md")));
    assert!(held(&f, "root").is_empty());
}

#[test]
fn a_subagents_trace_calls_carry_its_agent_and_a_quoted_trace_stays_text() {
    let f = shop();
    let line = "trace read src/lib.rs && rg 'x|trace y' src | trace grep total";
    let call = |agent: Option<&str>| {
        let mut extra = json!({"tool_name": "Bash", "tool_input": {"command": line, "description": "d"}});
        if let Some(agent) = agent {
            extra["agent_id"] = agent.into();
        }
        f.hook("PreToolUse", "claude", &input(&f, extra), &[], &[])
    };
    let rewritten = call(Some("sub-3")).json();
    assert_eq!(
        rewritten["hookSpecificOutput"]["updatedInput"],
        json!({"command": "trace --agent sub-3 read src/lib.rs && rg 'x|trace y' src | trace --agent sub-3 grep total", "description": "d"})
    );
    assert_eq!(call(None).stdout, "", "the main agent's commands are left alone");
}

/// The `trace` command a tracer-only hook names for `command`, `None` when
/// it lets the command run.
fn instead(f: &Fixture, command: &str) -> Option<String> {
    let run = f.hook("PreToolUse", "claude", &input(f, json!({"tool_name": "Bash", "tool_input": {"command": command}})), &["--tracer-only"], &[]);
    run.ok();
    if run.stdout.is_empty() {
        return None;
    }
    let answer = run.json();
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{}", run.stdout);
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
    let run = reason.strip_prefix("tracer: Claude reads files only through tracer.\nRun this instead: ").unwrap_or_else(|| panic!("{reason}"));
    Some(run.to_string())
}

#[test]
fn tracer_only_refuses_a_raw_read_of_any_file_and_names_the_trace_command() {
    let f = shop();
    let other = Fixture::new();
    other.write("ledger.rs", "fn total() {}\n");
    other.commit("other");
    let notes = f.tmp.join("notes.md");
    std::fs::write(&notes, "# Notes\n").unwrap();
    let (notes, tmp, clone) = (notes.to_string_lossy(), f.tmp.to_string_lossy(), other.root.to_string_lossy());

    assert_eq!(instead(&f, "bat src/lib.rs").as_deref(), Some("trace read src/lib.rs --raw"));
    assert_eq!(instead(&f, &format!("cat {notes}")), Some(format!("trace read {notes} --raw")), "a file outside every repository");
    assert_eq!(instead(&f, &format!("grep -rn total {clone}")), Some(format!("trace grep total {clone}")), "another repository");
    assert_eq!(instead(&f, &format!("eza -la {tmp}")), Some(format!("trace list {tmp} --all")));
    assert_eq!(instead(&f, &format!("eza --tree {tmp}")), Some(format!("trace tree {tmp}")));
    assert_eq!(instead(&f, "trace grep total --json | jq .counts").as_deref(), Some("trace grep total --json --filter .counts"));
    let bash = input(&f, json!({"tool_name": "Bash", "tool_input": {"command": "cat src/lib.rs"}}));
    assert_eq!(f.hook("PreToolUse", "claude", &bash, &[], &[]).stdout, "", "without --tracer-only nothing is refused");
}

#[test]
fn tracer_only_names_the_trace_command_that_gives_the_same_answer() {
    let f = shop();
    let lib = std::fs::canonicalize(f.path("src/lib.rs")).unwrap().to_string_lossy().into_owned();
    for (command, run) in [
        ("grep -oE 'id.{0,4}' src/lib.rs", "trace grep -o 'id.{0,4}' src/lib.rs".to_string()),
        ("grep -o 'id.{0,4}' src/lib.rs", r"trace grep -o 'id.\{0,4\}' src/lib.rs".into()),
        ("grep -vi TODO src/lib.rs", "trace grep -v -i TODO src/lib.rs".into()),
        ("grep -co total src/lib.rs", "trace grep -c total src/lib.rs".into()),
        ("grep -rwn --include='*.rs' total .", "trace grep -w -g '*.rs' total .".into()),
        (r"grep 'total\|cents(' src/lib.rs", r"trace grep 'total|cents\(' src/lib.rs".into()),
        ("grep -rn '*const' src", r"trace grep '\*const' src".into()),
        ("grep -F 'a.b' src/lib.rs", r"trace grep 'a\.b' src/lib.rs".into()),
        ("rg -S total", "trace grep -i total .".into()),
        (r"rg -S '\Wtotal' src", r"trace grep -i '\Wtotal' src".into()),
        ("cat src/lib.rs | grep total", "trace grep total src/lib.rs".into()),
        ("cd src && cat lib.rs | head -5", "trace read lib.rs --raw --lines 1:5".into()),
        ("head -20 src/lib.rs", "trace read src/lib.rs --raw --lines 1:20".into()),
        ("sed -n '3,5p' src/lib.rs", "trace read src/lib.rs --raw --lines 3:5".into()),
        ("ls -la src", "trace list src --all".into()),
        ("tree -L 1 src", "trace list src".into()),
        ("tree src", "trace tree src".into()),
        ("find src -name '*.rs' -type f", "trace find '*.rs' src".into()),
        ("git blame -L 2,4 src/lib.rs", "trace blame src/lib.rs --lines 2:4".into()),
        ("git show HEAD:src/lib.rs", format!("trace read {lib} --revision HEAD")),
        ("git log -S total", "trace history --contains total".into()),
        ("git log --oneline -n 5 -- src/lib.rs", "trace history src/lib.rs".into()),
        ("git show HEAD~1", "trace history --commit 'HEAD~1'".into()),
    ] {
        assert_eq!(instead(&f, command), Some(run), "{command}");
    }
}

#[test]
fn tracer_only_lets_a_command_tracer_cannot_answer_the_same_run() {
    let f = shop();
    f.write(".gitignore", "build/\n");
    f.write("build/out.txt", "built\n");
    f.write("vendor/kept.txt", "kept\n");
    for command in [
        "tail -f src/lib.rs",
        "tail -n 2 src/lib.rs",
        "grep -m 1 total src/lib.rs",
        "grep -R total src",
        r"grep '\(t\)\1' src/lib.rs",
        "grep -P total src/lib.rs",
        "awk '{print $1}' src/lib.rs",
        "sed 's/a/b/' src/lib.rs",
        "sed -i s/a/b/ src/lib.rs",
        "sed -n '2,+3p' src/lib.rs",
        "head -c 10 src/lib.rs",
        "cat /dev/null",
        "cat src/lib.rs > copy.rs",
        "cat src/lib.rs | jq .",
        "grep -rl zzqq src || echo none",
        "n=$(grep -c total src/lib.rs)",
        "rg total < src/lib.rs",
        r#"rg total <<< "$x""#,
        "echo total | grep total",
        "echo total | rg total",
        "ls src/lib.rs",
        "ls -R src",
        "ls -t src",
        "ls",
        "tree build",
        "tree -L 2 src",
        "find src -name '*.rs'",
        "find src -mtime 1",
        "git blame -L 2,+3 src/lib.rs",
        "git grep total -- '*.rs'",
        "git show :src/lib.rs",
        "git show HEAD:src",
        "git diff --name-status",
        "git log",
    ] {
        assert_eq!(instead(&f, command), None, "{command}");
    }
}

#[test]
fn a_read_gets_its_context_once_and_its_lines_are_seen() {
    let f = shop();
    let first = f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[]);
    let context = first.context();
    assert!(context.contains("file: src/lib.rs") && context.contains("pub fn discounted"), "{context}");
    let again = f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[]).context();
    assert!(again.starts_with("# src/lib.rs  {") && !again.contains("pub fn"), "facts shrink to a heading, rows are not repeated: {again}");
    let record = f.record(SESSION, "root").unwrap();
    let lib = record["files"].as_object().unwrap().iter().find(|(path, _)| path.ends_with("src/lib.rs")).unwrap().1;
    assert!(lib["coverage"].is_object(), "the read's lines are recorded as seen: {lib}");
}

/// A read while another call builds the relations index answers in about a
/// second with what it has: the file's rows without its relations, and once
/// per agent context the notice naming the command that finishes the index.
#[test]
fn a_read_while_the_index_builds_leaves_its_relations_out_and_says_so_once() {
    let f = shop();
    let building = tracer_cli_tests::hold_index_lock(&f.root, "relations_edges");
    let started = std::time::Instant::now();
    let first = f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[]).context();
    assert!(started.elapsed() < std::time::Duration::from_secs(4), "the read waited on the build: {:?}", started.elapsed());
    assert!(first.contains("pub fn discounted") && !first.contains("imported_by:"), "{first}");
    assert!(first.contains("index: still building") && first.contains("cache build"), "{first}");
    let second = f.hook("PostToolUse", "claude", &read(&f, "src/shop.rs"), &[], &[]).context();
    assert!(second.contains("pub fn sum") && !second.contains("index: still building"), "the notice shows once: {second}");

    drop(building);
    let built = f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[]).context();
    assert!(built.contains("imported_by: 1") && !built.contains("index: still building"), "{built}");
}

/// A session that starts while the relations index can't be built in time
/// still gets every primer section that needs no index, before its deadline.
#[test]
fn a_session_start_before_the_index_is_ready_gets_every_section_that_needs_none() {
    let f = shop();
    let _building = tracer_cli_tests::hold_index_lock(&f.root, "relations_edges");
    let started = std::time::Instant::now();
    let start = f.hook("SessionStart", "claude", &input(&f, json!({"source": "startup"})), &["--deadline-ms", "3000"], &[]);
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
    start.ok();
    let context = start.context();
    assert!(context.contains("## Environment") && context.contains("## Identity"), "{context}");
    let layout = context.lines().find(|line| line.starts_with("  📁 src/")).unwrap_or_else(|| panic!("{context}"));
    assert!(layout.contains("files: 2") && !layout.contains("cyclomatic_complexity"), "Layout leaves out the complexity the index stores: {layout}");
    assert!(!context.contains("## Spine"), "the Spine reads the index: {context}");
}

/// A hook and a shell `trace` call see different `TMPDIR`s under Claude
/// Code's sandbox, and still keep one record for the session.
#[test]
fn a_hook_and_a_trace_call_with_different_temp_folders_share_one_record() {
    let f = shop();
    let hook_temp = f.tmp.join("hook");
    let shell_temp = f.tmp.join("shell");
    f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[("TMPDIR", hook_temp.to_str().unwrap())]).ok();
    f.trace_env(&["read", "src/shop.rs"], &[("AGENT_SESSION_ID", SESSION), ("TMPDIR", shell_temp.to_str().unwrap())]).ok();
    let held = held(&f, "root");
    assert!(held.iter().any(|path| path.ends_with("src/lib.rs")) && held.iter().any(|path| path.ends_with("src/shop.rs")), "{held:?}");
}

/// A Subagent's `trace` call inside `bash -c` is credited to the Subagent.
#[test]
fn a_subagents_trace_call_inside_a_shell_script_lands_in_its_own_record() {
    let f = shop();
    let extra = json!({"agent_id": "sub-4", "tool_name": "Bash", "tool_input": {"command": "bash -c \"trace read src/lib.rs\""}});
    let rewritten = f.hook("PreToolUse", "claude", &input(&f, extra), &[], &[]).json();
    let line = rewritten["hookSpecificOutput"]["updatedInput"]["command"].as_str().unwrap().to_string();
    assert_eq!(line, "bash -c \"trace --agent sub-4 read src/lib.rs\"");

    let trace = f.command(&f.root, ["--version"], &[("AGENT_SESSION_ID", SESSION)]);
    let mut shell = std::process::Command::new("bash");
    shell.arg("-c").arg(&line).current_dir(&f.root);
    for (key, value) in trace.get_envs() {
        match value {
            Some(value) => shell.env(key, value),
            None => shell.env_remove(key),
        };
    }
    let bin = std::path::Path::new(trace.get_program()).canonicalize().unwrap_or_else(|_| trace.get_program().into());
    let path = std::env::var("PATH").unwrap_or_default();
    shell.env("PATH", format!("{}:{path}", bin.parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default()));
    let ran = shell.output().unwrap();
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert!(held(&f, "sub-4").iter().any(|path| path.ends_with("src/lib.rs")), "{:?}", held(&f, "sub-4"));
    assert!(!held(&f, "root").iter().any(|path| path.ends_with("src/lib.rs")), "{:?}", held(&f, "root"));
}

/// A read whose stored index is one update behind, while another call
/// brings it up to date, answers from the stored index without waiting.
#[test]
fn a_read_while_the_index_updates_answers_from_the_stored_index() {
    let f = shop();
    f.hook("PostToolUse", "claude", &read(&f, "src/shop.rs"), &[], &[]).ok();
    f.write("src/lib.rs", &format!("{LIB}\npub fn added() -> u32 {{\n    1\n}}\n"));
    let _updating = tracer_cli_tests::hold_index_lock(&f.root, "relations_edges");
    let started = std::time::Instant::now();
    let context = f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[]).context();
    assert!(started.elapsed() < std::time::Duration::from_secs(4), "{:?}", started.elapsed());
    assert!(context.contains("pub fn added") && context.contains("imported_by: 1") && !context.contains("index: still building"), "{context}");
}

#[test]
fn a_read_refused_for_its_size_still_gets_the_files_context_with_no_lines_seen() {
    let f = shop();
    let failed = f.hook("PostToolUseFailure", "claude", &read(&f, "src/lib.rs"), &[], &[]);
    assert!(failed.context().contains("pub fn discounted"), "{}", failed.stdout);
    let record = f.record(SESSION, "root").unwrap();
    let lib = record["files"].as_object().unwrap().iter().find(|(path, _)| path.ends_with("src/lib.rs"));
    assert!(lib.is_none_or(|(_, held)| held["coverage"].is_null()), "nothing was read: {record}");
}

#[test]
fn a_failed_read_of_a_missing_file_gets_no_context() {
    let f = shop();
    let missing = f.hook("PostToolUseFailure", "claude", &read(&f, "src/gone.rs"), &[], &[]);
    missing.ok();
    assert!(missing.context().is_empty(), "{}", missing.stdout);
}

/// Claude Code's Read returns at most its default line count, or fewer under
/// its token cap, so only the lines it returned are held.
#[test]
fn a_read_the_tool_cut_short_holds_only_the_lines_it_returned() {
    let f = shop();
    let response = json!({"type": "text", "file": {"filePath": f.path("src/lib.rs"), "numLines": 5, "startLine": 1, "totalLines": 21}});
    let cut = input(&f, json!({"tool_name": "Read", "tool_input": {"file_path": f.path("src/lib.rs")}, "tool_response": response}));
    f.hook("PostToolUse", "claude", &cut, &[], &[]).ok();
    let record = f.record(SESSION, "root").unwrap();
    let lib = record["files"].as_object().unwrap().iter().find(|(path, _)| path.ends_with("src/lib.rs")).unwrap().1;
    assert_eq!(lib["coverage"]["read"], json!([[1, 5]]), "{lib}");
}

/// A row whose header the Read returned shows by line and name.
#[test]
fn a_read_names_the_rows_whose_headers_it_returned() {
    let f = shop();
    let response = json!({"type": "text", "file": {"filePath": f.path("src/lib.rs"), "numLines": 5, "startLine": 1, "totalLines": 21}});
    let read = input(&f, json!({"tool_name": "Read", "tool_input": {"file_path": f.path("src/lib.rs")}, "tool_response": response}));
    let context = f.hook("PostToolUse", "claude", &read, &[], &[]).context();
    assert!(context.contains("L3    total()  // complexity 1"), "{context}");
    assert!(!context.contains("pub fn total(prices: &[u32]) -> u32"), "{context}");
}

/// A failure the hook reports reaches the agent in the answer, and the hook
/// exits 0, because a harness drops the answer of a hook that exits 2.
#[test]
fn a_failure_the_hook_reports_reaches_the_agent_in_its_answer() {
    let f = shop();
    f.write("m.lua", "function alpha()\n    return 1\nend\n");
    f.commit("lua file");
    let shim_dir = f.root.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("ctags");
    std::fs::write(&shim, "#!/bin/sh\n[ \"$1\" = --list-maps ] && { echo 'Lua *.lua'; exit 0; }\necho 'ctags: boom' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&shim, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", shim_dir.display(), std::env::var("PATH").unwrap_or_default());
    let answered = f.hook("PostToolUse", "claude", &read(&f, "m.lua"), &[], &[("PATH", &path)]);
    answered.ok();
    assert!(answered.context().contains("ctags failed on m.lua: ctags: boom"), "{}", answered.stdout);
}

#[test]
fn a_native_grep_gets_its_matches_under_their_declarations_once() {
    let f = shop();
    let grep = |mode: Option<&str>| {
        let mut tool = json!({"pattern": "total"});
        if let Some(mode) = mode {
            tool["output_mode"] = mode.into();
        }
        f.hook("PostToolUse", "claude", &input(&f, json!({"tool_name": "Grep", "tool_input": tool, "tool_response": {}})), &[], &[])
    };
    let paths = grep(None).context();
    assert!(paths.contains("pub fn discounted") && paths.contains("let sum = total(prices);"), "{paths}");
    assert_eq!(grep(None).stdout, "", "the same matches print once");
    let other = Fixture::new();
    other.write("a.rs", "fn caller() {\n    total();\n}\n");
    other.commit("a");
    let content = other
        .hook("PostToolUse", "claude", &input(&other, json!({"tool_name": "Grep", "tool_input": {"pattern": "total", "output_mode": "content"}, "tool_response": {}})), &[], &[])
        .context();
    assert!(content.contains("fn caller()") && !content.contains("total();"), "content mode adds only the declarations: {content}");
}

#[test]
fn a_native_glob_gets_each_files_facts_once() {
    let f = shop();
    let glob = || {
        let files = json!([f.path("src/lib.rs"), f.path("src/shop.rs")]);
        f.hook("PostToolUse", "claude", &input(&f, json!({"tool_name": "Glob", "tool_input": {"pattern": "**/*.rs"}, "tool_response": {"filenames": files}})), &[], &[])
    };
    let first = glob().context();
    assert!(first.contains("src/lib.rs  {") && first.contains("src/shop.rs  {"), "{first}");
    assert_eq!(glob().stdout, "");
}

#[test]
fn the_agents_own_edit_shows_only_the_facts_it_changed_and_the_callers_to_update() {
    let f = shop();
    f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[]).ok();
    f.write("src/lib.rs", &LIB.replace("percent > 100", "percent >= 100"));
    let patch = json!([{"oldStart": 8, "oldLines": 3, "newStart": 8, "newLines": 3, "lines": [
        "     let sum = total(prices);", "-    if percent > 100 {", "+    if percent >= 100 {", "         return 0;"
    ]}]);
    let edit = input(&f, json!({"tool_name": "Edit", "tool_input": {"file_path": f.path("src/lib.rs")}, "tool_response": {"structuredPatch": patch}}));
    let context = f.hook("PostToolUse", "claude", &edit, &[], &[]).context();
    assert!(context.contains("status: modified"), "a fact the edit changed prints: {context}");
    assert!(context.contains("callers:") && context.contains("src/shop.rs:13  sale"), "the edited function's callers: {context}");
    assert!(!context.contains("pub fn total"), "the agent's own rows never come back: {context}");
    let after = f.hook("PostToolUse", "claude", &read(&f, "src/lib.rs"), &[], &[]).context();
    assert!(!after.contains("pub fn"), "a read after the edit shows nothing new: {after}");
}

#[test]
fn another_agents_edit_shows_the_changed_rows_and_where_the_rest_moved() {
    let f = shop();
    f.hook("PostToolUse", "claude", &read(&f, "src/shop.rs"), &[], &[]).ok();
    f.write("src/shop.rs", &SHOP.replace("impl Cart {\n", "impl Cart {\n    pub fn count(&self) -> usize {\n        self.prices.len()\n    }\n\n"));
    let context = f.hook("PostToolUse", "claude", &read(&f, "src/shop.rs"), &[], &[]).context();
    assert!(context.contains("pub fn count(&self) -> usize"), "{context}");
    assert!(context.contains("moved: sum() L12, sale() L16"), "{context}");
    assert!(!context.contains("pub struct Cart"), "unchanged rows stay unsaid: {context}");
}

#[test]
fn a_codex_patch_is_the_agents_own_edit_and_brings_the_docs_its_file_reaches() {
    let f = shop();
    let home = home_with_rules(&f, &[("rust.md", "---\npaths: [\"**/*.rs\"]\n---\n\nReturn early.\n")]);
    let env = [("HOME", home.as_str())];
    f.write("src/lib.rs", &LIB.replace("percent > 100", "percent >= 100"));
    let body = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n     let sum = total(prices);\n-    if percent > 100 {\n+    if percent >= 100 {\n*** End Patch";
    let patch = input(&f, json!({"tool_name": "apply_patch", "tool_input": {"command": body}, "tool_response": {}}));
    let context = f.hook("PostToolUse", "codex", &patch, &[], &env).context();
    assert!(context.contains("src/shop.rs:13  sale"), "{context}");
    assert!(context.contains("Return early."), "Codex gets the rule the touched file reaches: {context}");
    let rows = f.hook("PostToolUse", "codex", &read(&f, "src/lib.rs"), &[], &env).context();
    assert!(!rows.contains("pub fn total"), "the patched file's rows were recorded as shown: {rows}");
}

#[test]
fn a_codex_shell_read_brings_the_docs_its_file_reaches() {
    let f = shop();
    let codex_home = f.tmp.join("codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let env = [("CODEX_HOME", codex_home.to_str().unwrap())];
    let shell = input(&f, json!({"tool_name": "exec_command", "tool_input": {"command": ["/bin/zsh", "-lc", "cd src && cat shop.rs"]}, "tool_response": {}}));
    let context = f.hook("PostToolUse", "codex", &shell, &[], &env).context();
    assert!(context.contains("## Claude.md") && context.contains("Prices are whole cents."), "{context}");
}

#[test]
fn a_hook_past_its_deadline_records_nothing() {
    let f = shop();
    let late = f.hook("SessionStart", "claude", &input(&f, json!({"source": "startup"})), &["--deadline-ms", "0"], &[]);
    assert_ne!(late.code, 0, "the deadline ends the process: {}", late.stderr);
    // The docs the harness loads itself are recorded whatever the hook
    // prints; what the hook printed is not.
    let record = f.record(SESSION, "root");
    let shown = record.as_ref().and_then(|record| record["shown"].as_object()).map_or(0, serde_json::Map::len);
    assert_eq!(shown, 0, "{record:?}");
}

#[test]
fn a_hook_given_input_that_is_not_json_fails_and_says_so() {
    let f = shop();
    let broken = tracer_cli_tests::trace_with_input(
        &f.root,
        ["hook", "PostToolUse", "--harness", "claude", "--deadline-ms", "5000"],
        &[],
        Some("not json"),
    );
    assert_eq!(broken.code, 1, "exit 1 is the error a harness reports without blocking the tool: {}", broken.stderr);
    assert!(broken.stderr.contains("the input is not JSON"), "{}", broken.stderr);
}
