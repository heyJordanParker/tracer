//! `.sharp` files, read through `mago outline`: rows, complexity, call
//! sites, failures, the build that read them, and older revisions, each
//! against a stub `mago` that prints a version 1 outline, the Language
//! Server Protocol's document-symbol tree.

use serde_json::{json, Value};
use std::path::Path;
use tracer_cli_tests::Fixture;

const STORE: &str = "namespace Paging;

import Lib.Probe;

public class Store
{
    private int count = 0;

    public TItem pick<TItem : DatabaseEntity>(string key)
    {
        return key == \"\" ? null : Probe.run(key);
    }
}
";

const OLDER_STORE: &str = "namespace Paging;

public class Store
{
    public void take()
    {
    }
}
";

/// An LSP range over the 0-based lines `start` through `end`.
fn range(start: i64, end: i64) -> Value {
    json!({"start": {"line": start, "character": 4}, "end": {"line": end, "character": 5}})
}

/// A document symbol of `kind` spanning the 0-based lines `lines`, named on
/// its first line, holding `children`.
fn symbol(name: &str, kind: &str, detail: &str, lines: (i64, i64), children: Vec<Value>) -> Value {
    json!({
        "name": name, "kind": kind, "detail": detail,
        "range": range(lines.0, lines.1), "selectionRange": range(lines.0, lines.0),
        "visibility": "public", "supertypes": [], "attributes": [], "children": children
    })
}

/// `symbol` for a function that carries `complexity`.
fn function(name: &str, kind: &str, detail: &str, lines: (i64, i64), complexity: i64, children: Vec<Value>) -> Value {
    let mut function = symbol(name, kind, detail, lines, children);
    function["complexity"] = complexity.into();
    function
}

/// A version 1 outline of `symbols`, `imports`, and `references`.
fn document(symbols: Vec<Value>, imports: Vec<Value>, references: Vec<Value>) -> Value {
    json!({
        "version": 1, "language": "sharp", "path": "Store.sharp", "namespace": "Paging", "errors": [],
        "symbols": symbols, "imports": imports, "references": references
    })
}

/// A call reference on the 0-based `line`.
fn call(name: &str, line: i64, receiver_type: Option<&str>, target: Option<&str>) -> Value {
    json!({"name": name, "kind": "call", "range": range(line, line), "receiverType": receiver_type, "target": target})
}

/// The version 1 outline of `STORE`, its `pick` at `complexity`, with the
/// type parameter `TItem` as `pick`'s child.
fn outline(complexity: i64) -> Value {
    let mut count = symbol("count", "property", "private int count = 0;", (6, 6), vec![]);
    count["visibility"] = "private".into();
    let pick = function(
        "pick",
        "method",
        "public TItem pick<TItem : DatabaseEntity>(string key)",
        (8, 11),
        complexity,
        vec![symbol("TItem", "typeParameter", "TItem : DatabaseEntity", (8, 8), vec![])],
    );
    document(
        vec![symbol(
            "Paging",
            "namespace",
            "namespace Paging;",
            (0, 12),
            vec![symbol("Store", "class", "public class Store", (4, 12), vec![count, pick])],
        )],
        vec![json!({"path": "Lib.Probe", "alias": null, "range": range(2, 2)})],
        vec![call("run", 10, Some("Lib.Probe"), Some("Lib.Probe.run"))],
    )
}

/// The version 1 outline of `OLDER_STORE`.
fn older_outline() -> Value {
    document(
        vec![symbol(
            "Store",
            "class",
            "public class Store",
            (2, 7),
            vec![function("take", "method", "public void take()", (4, 6), 1, vec![])],
        )],
        vec![],
        vec![],
    )
}

/// Writes `body` as an executable shell script at `path`.
fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755)).unwrap();
}

/// A `mago` that refuses any call but `mago outline --help`, `mago outline
/// --json --stdin-input Store.sharp`, and `mago outline --json Store.sharp`,
/// then prints `newer` for `STORE`, on its stdin or on disk, and `older` for
/// any other content.
fn mago(path: &Path, newer: &Value, older: &Value) {
    script(
        path,
        &format!(
            "emit() {{\n  case \"$1\" in\n  *pick*) cat <<'JSON'\n{newer}\nJSON\n  ;;\n  *) cat <<'JSON'\n{older}\nJSON\n  ;;\n  esac\n}}\n\
             case \"$*\" in\n  'outline --help') exit 0 ;;\n  'outline --json --stdin-input Store.sharp') emit \"$(cat)\" ;;\n  'outline --json Store.sharp') emit \"$(cat Store.sharp)\" ;;\n  \
             *) echo \"called as: $*\" >&2; exit 64 ;;\nesac\n"
        ),
    );
}

/// A `mago` that refuses any call but `mago outline --help`, `mago outline
/// --json --stdin-input <file>`, and `mago outline --json <file>...`, then
/// prints the outline `outlines` pairs with each file, labelled with its path.
fn mago_for(path: &Path, outlines: &[(&str, Value)]) {
    let cases: String = outlines
        .iter()
        .map(|(file, outline)| {
            let mut outline = outline.clone();
            outline["path"] = (*file).into();
            format!("  {file}) cat <<'JSON'\n{outline}\nJSON\n  ;;\n")
        })
        .collect();
    script(
        path,
        &format!(
            "emit() {{\n  case \"$1\" in\n{cases}  *) echo \"no outline for $1\" >&2; exit 64 ;;\n  esac\n}}\n\
             [ \"$*\" = 'outline --help' ] && exit 0\n\
             [ \"$1 $2\" = 'outline --json' ] || {{ echo \"called as: $*\" >&2; exit 64; }}\n\
             if [ \"$3\" = --stdin-input ]; then\n  [ $# -eq 4 ] || {{ echo \"called as: $*\" >&2; exit 64; }}\n  cat >/dev/null\n  emit \"$4\"\n  exit\nfi\n\
             shift 2\nfor file in \"$@\"; do\n  emit \"$file\"\ndone\n"
        ),
    );
}

/// The inherited `PATH` with `directory` first.
fn path_with(directory: &Path) -> String {
    format!("{}:{}", directory.display(), std::env::var("PATH").unwrap_or_default())
}

/// A repository holding `STORE`, with a `mago` that prints its outline at
/// `complexity` 2 in the fixture's scratch folder, and the `PATH` that finds
/// it.
fn store() -> (Fixture, String) {
    let f = Fixture::new();
    f.write("Store.sharp", STORE);
    f.commit("store");
    let bin = f.tmp.join("bin");
    mago(&bin.join("mago"), &outline(2), &older_outline());
    let path = path_with(&bin);
    (f, path)
}

/// The `context.files[file].surface` rows of `trace read file --json`.
fn surface(f: &Fixture, path: &str, file: &str) -> Vec<Value> {
    let json = f.trace_env(&["read", file, "--json"], &[("PATH", path)]);
    json.ok();
    json.json()["context"]["files"][file]["surface"].as_array().expect("surface rows").clone()
}

#[test]
fn a_sharp_file_reads_its_rows_and_complexity_from_mago_outline() {
    let (f, path) = store();
    let read = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path)]);
    read.ok();
    assert!(read.stdout.contains("cyclomatic_complexity: 2"), "the front matter carries the outline's complexity: {}", read.stdout);
    assert!(read.stdout.contains("L5    Store\n"), "the class has its row: {}", read.stdout);
    assert!(read.stdout.contains("L9      pick()  // complexity 2\n"), "the method's row carries its complexity: {}", read.stdout);
    assert!(read.stdout.contains("L11:         return key"), "the lines still print: {}", read.stdout);
    assert!(!read.stdout.contains("L1    Paging"), "a namespace gives no row: {}", read.stdout);

    let surface = surface(&f, &path, "Store.sharp");
    let row = |name: &str| surface.iter().find(|row| row["name"] == name).unwrap_or_else(|| panic!("no {name} row: {surface:?}"));
    assert_eq!(row("Store")["kind"], "class", "{surface:?}");
    assert_eq!(row("Store")["parent"], Value::Null, "what a namespace holds is top-level: {surface:?}");
    assert_eq!(row("pick")["kind"], "function", "a method reads as a PHP method does: {surface:?}");
    assert_eq!(row("pick")["container"], "Store", "{surface:?}");
    assert_eq!(row("pick")["header"], "public TItem pick<TItem : DatabaseEntity>(string key)", "the header is the symbol's detail: {surface:?}");
    assert_eq!(row("pick")["cyclomatic_complexity"], 2, "{surface:?}");
    assert_eq!(row("count")["kind"], "property", "{surface:?}");
    assert!(surface.iter().all(|row| row["name"] != "Paging"), "a namespace gives no row: {surface:?}");
}

#[test]
fn a_symbol_tree_gives_rows_only_for_declarations_each_under_its_nearest_row() {
    let f = Fixture::new();
    f.write("Store.sharp", STORE);
    f.commit("store");
    let mut tree = outline(2);
    let members = tree["symbols"][0]["children"][0]["children"].as_array_mut().unwrap();
    members.retain(|member| member["name"] != "count");
    let bin = f.tmp.join("bin");
    mago(&bin.join("mago"), &tree, &older_outline());
    let surface = surface(&f, &path_with(&bin), "Store.sharp");
    let names: Vec<&Value> = surface.iter().map(|row| &row["name"]).collect();
    assert_eq!(names, ["Store", "pick"], "a namespace and a type parameter give no row: {surface:?}");
    assert_eq!(surface[0]["parent"], Value::Null, "{surface:?}");
    assert_eq!(surface[1]["parent"], 0, "the method nests under the class, past the namespace: {surface:?}");
    assert_eq!(surface[1]["header_line"], 9, "{surface:?}");
    assert_eq!(surface[1]["line"], 9, "{surface:?}");
    assert_eq!(surface[1]["end_line"], 12, "{surface:?}");
}

#[test]
fn defines_and_structure_answer_a_sharp_file_from_its_outline() {
    let (f, path) = store();
    let defines = f.trace_env(&["defines", "pick"], &[("PATH", &path)]);
    defines.ok();
    assert!(defines.stdout.contains("Store.sharp"), "defines names the file: {}", defines.stdout);
    assert!(defines.stdout.contains("pick"), "defines names the method: {}", defines.stdout);

    let structure = f.trace_env(&["structure", "Store.sharp"], &[("PATH", &path)]);
    structure.ok();
    assert!(structure.stdout.contains("public class Store"), "structure lists the class: {}", structure.stdout);
    assert!(structure.stdout.contains("pick"), "structure lists the method: {}", structure.stdout);
}

const PROBE: &str = "namespace Lib;

public class Probe
{
    public void run()
    {
    }
}
";

const OTHER: &str = "namespace Lib;

public class Other
{
    public void run()
    {
    }
}
";

const HELPERS: &str = "namespace Lib;

public void helper()
{
}
";

const CALLER: &str = "namespace Lib;

public class Caller
{
    public void go(Probe probe, mixed value)
    {
        probe.run();
        helper();
        value.run();
    }
}
";

/// The outline of a file that declares class `class` with one `run` method.
fn runner(class: &str) -> Value {
    document(
        vec![symbol(
            class,
            "class",
            &format!("public class {class}"),
            (2, 7),
            vec![function("run", "method", "public void run()", (4, 6), 1, vec![])],
        )],
        vec![],
        vec![],
    )
}

/// The `[file, calling declaration, line, confidence]` rows of
/// `trace callers <query>`, over every declaration it names.
fn callers(f: &Fixture, path: &str, query: &str) -> Vec<(String, String, i64, String)> {
    let run = f.trace_env(&["callers", query, "--json"], &[("PATH", path)]);
    run.ok();
    let document = run.json();
    document["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results: {document}"))
        .iter()
        .flat_map(|declaration| declaration["callers"].as_array().cloned().unwrap_or_default())
        .map(|caller| {
            (
                caller["source_file"].as_str().unwrap_or_default().to_string(),
                caller["declaration"]["name"].as_str().unwrap_or_default().to_string(),
                caller["source_line"].as_i64().unwrap_or_default(),
                caller["confidence"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn a_sharp_call_takes_its_shape_from_the_receiver_type_and_target_it_names() {
    let f = Fixture::new();
    f.write("Lib/Probe.sharp", PROBE);
    f.write("Lib/Other.sharp", OTHER);
    f.write("Lib/helpers.sharp", HELPERS);
    f.write("Lib/Caller.sharp", CALLER);
    f.commit("calls");
    let caller = document(
        vec![symbol(
            "Caller",
            "class",
            "public class Caller",
            (2, 10),
            vec![function("go", "method", "public void go(Probe probe, mixed value)", (4, 9), 1, vec![])],
        )],
        vec![],
        vec![
            call("run", 6, Some("Lib.Probe"), Some("Lib.Probe.run")),
            call("helper", 7, None, Some("Lib.helper")),
            call("run", 8, None, None),
        ],
    );
    let helpers = document(vec![function("helper", "function", "public void helper()", (2, 4), 1, vec![])], vec![], vec![]);
    let bin = f.tmp.join("bin");
    mago_for(
        &bin.join("mago"),
        &[
            ("Lib/Probe.sharp", runner("Probe")),
            ("Lib/Other.sharp", runner("Other")),
            ("Lib/helpers.sharp", helpers),
            ("Lib/Caller.sharp", caller),
        ],
    );
    let path = path_with(&bin);
    let go = |line: i64, confidence: &str| ("Lib/Caller.sharp".to_string(), "go".to_string(), line, confidence.to_string());

    assert_eq!(
        callers(&f, &path, "helper"),
        [go(8, "INFERRED")],
        "a call with a target and no receiver type is a free call"
    );
    assert_eq!(
        callers(&f, &path, "Probe::run"),
        [go(7, "INFERRED"), go(9, "AMBIGUOUS")],
        "a call on a receiver type reaches that type's method, and a call on a value reaches every method of its name"
    );
    assert_eq!(
        callers(&f, &path, "Other::run"),
        [go(9, "AMBIGUOUS")],
        "a call on a receiver type reaches no other type's method"
    );
}

#[test]
fn a_call_whose_target_is_a_method_reaches_only_that_class_even_without_a_receiver_type() {
    let f = Fixture::new();
    f.write("Lib/Probe.sharp", PROBE);
    f.write("Lib/Other.sharp", OTHER);
    f.write("Lib/helpers.sharp", HELPERS);
    f.write("Lib/Caller.sharp", CALLER);
    f.commit("calls");
    let targeting = |name: &str, line: i64, target: &str, kind: &str| {
        let mut reference = call(name, line, None, Some(target));
        reference["targetKind"] = kind.into();
        reference
    };
    let caller = document(
        vec![symbol(
            "Caller",
            "class",
            "public class Caller",
            (2, 10),
            vec![function("go", "method", "public void go(Probe probe, mixed value)", (4, 9), 1, vec![])],
        )],
        vec![],
        vec![
            targeting("run", 6, "Lib.Probe.run", "method"),
            targeting("run", 7, "Sharp.MapMembers.run", "method"),
            targeting("helper", 8, "Lib.helper", "function"),
        ],
    );
    let helpers = document(vec![function("helper", "function", "public void helper()", (2, 4), 1, vec![])], vec![], vec![]);
    let bin = f.tmp.join("bin");
    mago_for(
        &bin.join("mago"),
        &[
            ("Lib/Probe.sharp", runner("Probe")),
            ("Lib/Other.sharp", runner("Other")),
            ("Lib/helpers.sharp", helpers),
            ("Lib/Caller.sharp", caller),
        ],
    );
    let path = path_with(&bin);
    let go = |line: i64, confidence: &str| ("Lib/Caller.sharp".to_string(), "go".to_string(), line, confidence.to_string());

    assert_eq!(
        callers(&f, &path, "Probe::run"),
        [go(7, "INFERRED")],
        "a method target reaches its class's method, and a built-in's method reaches no class of the repository"
    );
    assert_eq!(callers(&f, &path, "Other::run"), [], "a method target reaches no other class's method");
    assert_eq!(callers(&f, &path, "helper"), [go(9, "INFERRED")], "a function target is a free call");
}

#[test]
fn a_failing_mago_is_named_while_the_lines_still_print() {
    let f = Fixture::new();
    f.write("Store.sharp", STORE);
    f.commit("store");
    let bin = f.tmp.join("bin");
    let path = path_with(&bin);
    let cases = [
        ("cat >/dev/null\necho 'mago: boom' >&2\nexit 3\n".to_string(), "mago outline failed on Store.sharp: mago: boom"),
        (
            format!("cat >/dev/null\ncat <<'JSON'\n{}\nJSON\n", json!({"version": 2, "symbols": []})),
            "mago outline failed on Store.sharp: outline version 2",
        ),
    ];
    for (body, named) in cases {
        script(&bin.join("mago"), &body);
        for call in ["first call", "stored call"] {
            let read = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path)]);
            assert_eq!(read.code, 2, "{call} exits 2 for the failure: {}", read.combined());
            assert!(read.stderr.contains(named), "{call} names `{named}`: {}", read.combined());
            assert!(read.stdout.contains("L 5: public class Store"), "{call} still prints the lines: {}", read.combined());
        }
    }
}

#[test]
fn a_symbol_kind_outside_the_outlines_list_fails_the_file_with_the_reason() {
    for kind in ["trait", "variable"] {
        let f = Fixture::new();
        f.write("Store.sharp", STORE);
        f.commit("store");
        let mut tree = outline(2);
        tree["symbols"][0]["children"][0]["kind"] = kind.into();
        let bin = f.tmp.join("bin");
        mago(&bin.join("mago"), &tree, &older_outline());
        let read = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path_with(&bin))]);
        assert_eq!(read.code, 2, "{kind}: {}", read.combined());
        assert!(
            read.stderr.contains(&format!("mago outline failed on Store.sharp: unreadable outline: unknown variant `{kind}`, expected one of")),
            "the reason names the kind `{kind}`, which `mago outline` never prints: {}",
            read.combined()
        );
        assert!(read.stdout.contains("L 5: public class Store"), "{kind}: the lines still print: {}", read.combined());
        assert!(!read.stdout.contains("pick()"), "{kind}: no row of the outline is kept: {}", read.combined());
    }
}

/// A `mago` from before `outline`, as PHP# 0.7.4's: every `outline` call
/// fails the way clap refuses an unknown command, and each call is written
/// to `log`.
fn mago_without_outline(path: &Path, log: &Path) {
    script(
        path,
        &format!(
            "echo \"$*\" >> '{}'\ncat >/dev/null\necho \"error: unrecognized subcommand 'outline'\" >&2\necho >&2\necho 'Usage: mago [OPTIONS] <COMMAND>' >&2\nexit 2\n",
            log.display()
        ),
    );
}

#[test]
fn a_mago_without_outline_leaves_sharp_files_unsupported_and_every_other_fact_intact() {
    let f = Fixture::new();
    f.write("composer.json", r#"{"autoload": {"psr-4": {"App\\": "app/"}}}"#);
    f.write("app/Foo.php", "<?php\nnamespace App;\n\nclass Foo\n{\n    public function run(): void\n    {\n    }\n}\n");
    f.write(
        "app/Bar.php",
        "<?php\nnamespace App;\n\nuse App\\Foo;\n\nclass Bar\n{\n    public function go(Foo $foo): void\n    {\n        $foo->run();\n    }\n}\n",
    );
    f.write("Store.sharp", STORE);
    f.commit("app");
    let bin = f.tmp.join("bin");
    let log = f.tmp.join("mago.log");
    mago_without_outline(&bin.join("mago"), &log);
    let path = path_with(&bin);
    let quiet = |run: &tracer_cli_tests::Run, call: &str| {
        assert_eq!(run.code, 0, "{call} exits 0: {}", run.combined());
        assert!(!run.stderr.contains("mago"), "{call} names no mago failure: {}", run.combined());
    };

    let read = f.trace_env(&["read", "app/Foo.php", "--json"], &[("PATH", &path)]);
    quiet(&read, "read of a PHP file");
    assert_eq!(read.json()["context"]["files"]["app/Foo.php"]["imported_by"], 1, "the PHP file's importer still counts: {}", read.stdout);

    let sharp = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path)]);
    quiet(&sharp, "read of a .sharp file");
    assert!(sharp.stdout.contains("L 5: public class Store"), "the lines still print: {}", sharp.stdout);
    assert!(!sharp.stdout.contains("pick()"), "an unsupported file has no rows: {}", sharp.stdout);

    let find = f.trace_env(&["find", "*.sharp", "."], &[("PATH", &path)]);
    quiet(&find, "find");
    assert!(find.stdout.contains("Store.sharp"), "{}", find.stdout);

    f.write("Store.sharp", &STORE.replace("count = 0", "count = 1"));
    f.write("app/Foo.php", "<?php\nnamespace App;\n\nclass Foo\n{\n    public function run(): void\n    {\n        echo 1;\n    }\n}\n");
    let diff = f.trace_env(&["diff", "--json"], &[("PATH", &path)]);
    quiet(&diff, "diff");
    assert_eq!(diff.json()["context"]["files"]["app/Foo.php"]["imported_by"], 1, "diff keeps the importer: {}", diff.stdout);

    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "outline --help\n",
        "the mago build is asked about outline once and spawned for no file"
    );

    let doctor = f.trace_env(&["doctor"], &[("PATH", &path)]);
    doctor.ok();
    assert!(doctor.stdout.contains("\u{2717} mago, for .sharp files: ") && doctor.stdout.contains("has no outline command"), "{}", doctor.stdout);
    assert!(doctor.stdout.contains("mago 0.8.1 or later, which has `mago outline`"), "{}", doctor.stdout);
    assert!(doctor.stdout.contains("All required binaries installed."), "{}", doctor.stdout);

    f.write("Store.sharp", STORE);
    mago(&bin.join("mago"), &outline(2), &older_outline());
    let updated = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path)]);
    updated.ok();
    assert!(updated.stdout.contains("L9      pick()  // complexity 2\n"), "a mago with outline reads the file: {}", updated.stdout);
}

#[test]
fn another_mago_build_extracts_the_file_again() {
    let (f, path) = store();
    let first = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path)]);
    first.ok();
    assert!(first.stdout.contains("// complexity 2"), "{}", first.stdout);

    mago(&f.tmp.join("bin").join("mago"), &outline(5), &older_outline());
    let replaced = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path)]);
    replaced.ok();
    assert!(replaced.stdout.contains("// complexity 5"), "the new build reads the file again: {}", replaced.stdout);
}

#[test]
fn the_repositorys_vendor_mago_wins_over_the_one_on_path() {
    let f = Fixture::new();
    f.write(".gitignore", "vendor/\n");
    f.write("Store.sharp", STORE);
    f.commit("store");
    let bin = f.tmp.join("bin");
    mago(&bin.join("mago"), &outline(7), &older_outline());
    mago(&f.root.join("vendor").join("bin").join("mago"), &outline(3), &older_outline());
    let read = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path_with(&bin))]);
    read.ok();
    assert!(read.stdout.contains("// complexity 3"), "vendor/bin/mago read the file: {}", read.stdout);
}

#[test]
fn a_vendor_mago_that_cannot_run_here_gives_way_to_the_one_on_path() {
    let placeholder = "#!/bin/sh\necho \"mago's binary is not installed: run composer install\" >&2\nexit 1\n";
    let other_platform = "\u{7f}ELF\u{2}\u{1}\u{1}\u{0}";
    for (installed, why) in [(placeholder, "the Composer placeholder"), (other_platform, "a binary built for another platform")] {
        let f = Fixture::new();
        f.write(".gitignore", "vendor/\n");
        f.write("Store.sharp", STORE);
        f.commit("store");
        let bin = f.tmp.join("bin");
        mago(&bin.join("mago"), &outline(7), &older_outline());
        let vendor = f.root.join("vendor").join("bin").join("mago");
        std::fs::create_dir_all(vendor.parent().unwrap()).unwrap();
        std::fs::write(&vendor, installed).unwrap();
        std::fs::set_permissions(&vendor, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755)).unwrap();
        for call in ["first call", "stored call"] {
            let read = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path_with(&bin))]);
            assert_eq!(read.code, 0, "{why}, {call}: {}", read.combined());
            assert!(read.stdout.contains("// complexity 7"), "{why}, {call}: the mago on PATH read the file: {}", read.stdout);
        }
    }
}

#[test]
fn a_sharp_file_at_an_older_revision_shows_that_revisions_rows() {
    let f = Fixture::new();
    f.write("Store.sharp", OLDER_STORE);
    f.commit("older store");
    f.write("Store.sharp", STORE);
    f.commit("store");
    let bin = f.tmp.join("bin");
    mago(&bin.join("mago"), &outline(2), &older_outline());
    let read = f.trace_env(&["read", "--revision", "HEAD~1", "Store.sharp"], &[("PATH", &path_with(&bin))]);
    read.ok();
    assert!(read.stdout.contains("L3    Store\n"), "the older revision's class has its row: {}", read.stdout);
    assert!(read.stdout.contains("L5      take()  // complexity 1\n"), "the older revision's method has its row and its own complexity: {}", read.stdout);
    assert!(!read.stdout.contains("pick()"), "today's method is no row of the older revision: {}", read.stdout);
}

#[test]
fn a_window_of_a_sharp_file_prints_the_lines_its_outline_names() {
    let (f, path) = store();
    let method = f.trace_env(&["read", "Store.sharp", "--method", "pick"], &[("PATH", &path)]);
    method.ok();
    assert!(method.stdout.contains("public TItem pick<TItem : DatabaseEntity>(string key)"), "the method's header prints: {}", method.stdout);
    assert!(method.stdout.contains("return key == \"\""), "the method's body prints: {}", method.stdout);
    assert!(!method.stdout.contains("private int count"), "only the method's lines print: {}", method.stdout);

    for window in [vec!["--lines", "9:12"], vec!["--between", "pick", "^    }$"]] {
        let read = f.trace_env(&[&["read", "Store.sharp"], window.as_slice()].concat(), &[("PATH", &path)]);
        read.ok();
        assert!(read.stdout.contains("return key == \"\""), "{window:?} prints the lines: {}", read.stdout);
    }
}

#[test]
fn a_symbol_diff_of_a_sharp_file_names_the_function_whose_body_changed() {
    let f = Fixture::new();
    f.write("lib/helpers.sharp", HELPERS);
    f.commit("helpers");
    f.write("lib/helpers.sharp", "namespace Lib;\n\npublic void helper()\n{\n    run();\n}\n");
    let helpers = document(vec![function("helper", "function", "public void helper()", (2, 4), 1, vec![])], vec![], vec![]);
    let path = mago_path(&f, &[("lib/helpers.sharp", helpers)]);
    let diff = f.trace_env(&["read", "--revision", "HEAD", "--diff", "lib/helpers.sharp"], &[("PATH", &path)]);
    diff.ok();
    assert!(diff.stdout.contains("~ function helper (ref L3 → worktree L3)"), "the changed body is named: {}", diff.stdout);
}

#[test]
fn a_second_read_at_a_revision_starts_no_mago() {
    let f = Fixture::new();
    f.write("lib/Store.sharp", "public class Store\n");
    f.commit("store");
    f.write("lib/Store.sharp", "public class Shop\n");
    let (path, log) = logging_mago(&f);

    let first = f.trace_env(&["read", "--revision", "HEAD", "--diff", "lib/Store.sharp"], &[("PATH", &path)]);
    first.ok();
    assert!(first.stdout.contains("+ class Shop") && first.stdout.contains("- class Store"), "both sides answer: {}", first.stdout);
    let started = starts(&log);
    let committed = started.iter().filter(|start| *start == "outline --json --stdin-input lib/Store.sharp").count();
    assert_eq!(committed, 1, "the committed side is read once: {started:?}");

    f.trace_env(&["read", "--revision", "HEAD", "--diff", "lib/Store.sharp"], &[("PATH", &path)]).ok();
    assert_eq!(starts(&log), started, "a second read at the revision starts no mago");
}

#[test]
fn doctor_names_mago_and_its_install_when_none_is_found() {
    let f = Fixture::new();
    let inherited = std::env::var("PATH").unwrap_or_default();
    let without = std::env::join_paths(std::env::split_paths(&inherited).filter(|directory| !directory.join("mago").exists()))
        .unwrap()
        .into_string()
        .unwrap();
    let missing = f.trace_env(&["doctor"], &[("PATH", &without)]);
    missing.ok();
    assert!(missing.stdout.contains("\u{2717} mago, for .sharp files"), "{}", missing.stdout);
    assert!(missing.stdout.contains("composer require --dev heyjordanparker/mago-sharp"), "{}", missing.stdout);
    assert!(missing.stdout.contains("All required binaries installed."), "mago is optional: {}", missing.stdout);

    let bin = f.tmp.join("bin");
    mago(&bin.join("mago"), &outline(2), &older_outline());
    let found = f.trace_env(&["doctor"], &[("PATH", &path_with(&bin))]);
    found.ok();
    assert!(found.stdout.contains("\u{2713} mago, for .sharp files"), "{}", found.stdout);
}

/// The `imported_by` count `trace read file --json` gives `file`.
fn imported_by(f: &Fixture, path: &str, file: &str) -> Value {
    let read = f.trace_env(&["read", file, "--json"], &[("PATH", path)]);
    read.ok();
    read.json()["context"]["files"][file]["imported_by"].clone()
}

/// A `mago_for` stub in the fixture's scratch folder for `outlines`, and the
/// `PATH` that finds it.
fn mago_path(f: &Fixture, outlines: &[(&str, Value)]) -> String {
    let bin = f.tmp.join("bin");
    mago_for(&bin.join("mago"), outlines);
    path_with(&bin)
}

const PAGING_STORE: &str = "namespace Paging;

public class Store
{
    public static Store make()
    {
        return new Store();
    }

    public void save()
    {
    }
}
";

#[test]
fn a_php_caller_reaches_a_sharp_class_through_its_use_at_extracted() {
    let f = Fixture::new();
    f.write("composer.json", r#"{"autoload": {"psr-4": {"Paging\\": "lib/Paging/", "App\\": "app/"}}}"#);
    f.write("lib/Paging/Store.sharp", PAGING_STORE);
    f.write(
        "app/Saver.php",
        "<?php\nnamespace App;\n\nuse Paging\\Store;\n\nclass Saver\n{\n    public function keep(): void\n    {\n        Store::make();\n        (new Store)->save();\n    }\n}\n",
    );
    f.commit("store");
    let store = document(
        vec![symbol(
            "Paging",
            "namespace",
            "namespace Paging;",
            (0, 12),
            vec![symbol(
                "Store",
                "class",
                "public class Store",
                (2, 12),
                vec![
                    function("make", "method", "public static Store make()", (4, 7), 1, vec![]),
                    function("save", "method", "public void save()", (9, 11), 1, vec![]),
                ],
            )],
        )],
        vec![],
        vec![],
    );
    let path = mago_path(&f, &[("lib/Paging/Store.sharp", store)]);
    let keep = |line: i64| ("app/Saver.php".to_string(), "keep".to_string(), line, "EXTRACTED".to_string());

    assert_eq!(callers(&f, &path, "make"), [keep(10)], "a static call reaches the Sharp method its `use` names");
    assert_eq!(callers(&f, &path, "Store::save"), [keep(11)], "a call on `new Store` reaches the Sharp method");
    assert_eq!(imported_by(&f, &path, "lib/Paging/Store.sharp"), 1, "the PHP `use` is an edge to the .sharp file");
}

#[test]
fn a_sharp_import_reaches_a_php_file_through_psr4_and_counts_as_its_importer() {
    let f = Fixture::new();
    f.write("composer.json", r#"{"autoload": {"psr-4": {"App\\": "app/"}}}"#);
    f.write(
        "app/Billing/Invoice.php",
        "<?php\nnamespace App\\Billing;\n\nclass Invoice\n{\n    public function total(): int\n    {\n        return 0;\n    }\n}\n",
    );
    f.write(
        "app/Reports/Monthly.sharp",
        "namespace App.Reports;\n\nimport App.Billing.Invoice;\n\npublic class Monthly\n{\n    public void sum(Invoice invoice)\n    {\n        invoice.total();\n    }\n}\n",
    );
    f.commit("billing");
    let monthly = document(
        vec![symbol(
            "Monthly",
            "class",
            "public class Monthly",
            (4, 10),
            vec![function("sum", "method", "public void sum(Invoice invoice)", (6, 9), 1, vec![])],
        )],
        vec![json!({"path": "App.Billing.Invoice", "alias": null, "range": range(2, 2)})],
        vec![call("total", 8, Some("App.Billing.Invoice"), Some("App.Billing.Invoice.total"))],
    );
    let path = mago_path(&f, &[("app/Reports/Monthly.sharp", monthly)]);

    assert_eq!(imported_by(&f, &path, "app/Billing/Invoice.php"), 1, "the Sharp importer counts");
    assert_eq!(
        callers(&f, &path, "total"),
        [("app/Reports/Monthly.sharp".to_string(), "sum".to_string(), 9, "EXTRACTED".to_string())],
        "a typed member call reaches the PHP method the import names"
    );
}

#[test]
fn a_psr4_use_reaches_only_the_php_or_sharp_file_its_class_names() {
    let f = Fixture::new();
    let foo = "<?php\nnamespace App;\n\nclass Foo\n{\n    public function run(): void\n    {\n    }\n}\n";
    f.write("composer.json", r#"{"autoload": {"psr-4": {"App\\": "app/"}}}"#);
    f.write("app/Foo.php", foo);
    f.write("app/Foo.phtml", foo);
    f.write("app/Bar.php", "<?php\nnamespace App;\n\nuse App\\Foo;\n\nclass Bar\n{\n}\n");
    f.commit("foo");
    let path = mago_path(&f, &[]);

    let info = f.trace_env(&["info", "app/Bar.php", "--json"], &[("PATH", &path)]);
    info.ok();
    let dependencies: Vec<(String, String)> = info.json()["context"]["files"]["app/Bar.php"]["dependencies"]
        .as_array()
        .unwrap_or_else(|| panic!("dependencies: {}", info.stdout))
        .iter()
        .map(|dependency| (dependency["module"].as_str().unwrap_or_default().to_string(), dependency["confidence"].as_str().unwrap_or_default().to_string()))
        .collect();
    assert_eq!(
        dependencies,
        [("app/Foo".to_string(), "EXTRACTED".to_string())],
        "`use App\\Foo;` names app/Foo.php alone, never the app/Foo.phtml ctags reads as PHP"
    );
}

#[test]
fn a_class_inherits_methods_across_php_and_sharp_in_either_direction() {
    let f = Fixture::new();
    f.write("composer.json", r#"{"autoload": {"psr-4": {"App\\": "app/"}}}"#);
    f.write(
        "app/Base.php",
        "<?php\nnamespace App;\n\nclass Base\n{\n    public function save(): void\n    {\n    }\n}\n",
    );
    f.write(
        "app/Sharp/Report.sharp",
        "namespace App.Sharp;\n\nimport App.Base;\n\npublic class Report : Base\n{\n    public void run()\n    {\n        this.save();\n    }\n}\n",
    );
    f.write(
        "app/Child.php",
        "<?php\nnamespace App;\n\nuse App\\Sharp\\Report;\n\nclass Child extends Report\n{\n    public function go(): void\n    {\n        $this->run();\n    }\n}\n",
    );
    f.commit("lineage");
    let mut report = symbol(
        "Report",
        "class",
        "public class Report : Base",
        (4, 10),
        vec![function("run", "method", "public void run()", (6, 9), 1, vec![])],
    );
    report["supertypes"] = json!(["App.Base"]);
    let outline = document(
        vec![report],
        vec![json!({"path": "App.Base", "alias": null, "range": range(2, 2)})],
        vec![call("save", 8, Some("App.Sharp.Report"), Some("App.Base.save"))],
    );
    let path = mago_path(&f, &[("app/Sharp/Report.sharp", outline)]);

    assert_eq!(
        callers(&f, &path, "Base::save"),
        [("app/Sharp/Report.sharp".to_string(), "run".to_string(), 9, "EXTRACTED".to_string())],
        "a call on the Sharp type reaches the method it inherits from its PHP parent"
    );
    assert_eq!(
        callers(&f, &path, "Report::run"),
        [("app/Child.php".to_string(), "go".to_string(), 10, "EXTRACTED".to_string())],
        "a call on the PHP type reaches the method it inherits from its Sharp parent"
    );
}

#[test]
fn a_typescript_or_rust_call_never_reaches_a_sharp_or_php_declaration() {
    let f = Fixture::new();
    f.write("lib/helpers.sharp", HELPERS);
    f.write("lib/Caller.sharp", "namespace Lib;\n\npublic void go()\n{\n    helper();\n}\n");
    f.write("app/run.php", "<?php\n\nfunction start(): void\n{\n    helper();\n}\n");
    f.write("web/app.ts", "export function boot(): void {\n  helper();\n}\n");
    f.write("src/lib.rs", "pub fn launch() {\n    helper();\n}\n");
    f.commit("helpers");
    let helpers = document(vec![function("helper", "function", "public void helper()", (2, 4), 1, vec![])], vec![], vec![]);
    let caller = document(
        vec![function("go", "function", "public void go()", (2, 5), 1, vec![])],
        vec![],
        vec![call("helper", 4, None, Some("Lib.helper"))],
    );
    let path = mago_path(&f, &[("lib/helpers.sharp", helpers), ("lib/Caller.sharp", caller)]);

    assert_eq!(
        callers(&f, &path, "helper"),
        [
            ("app/run.php".to_string(), "start".to_string(), 5, "INFERRED".to_string()),
            ("lib/Caller.sharp".to_string(), "go".to_string(), 5, "INFERRED".to_string()),
        ],
        "PHP and Sharp resolve with each other, and TypeScript and Rust with neither"
    );
}

/// `file_facts::RESOLVE_CHUNK`: the most files one `get_batch` resolves, so
/// the most one `mago outline` reads.
const CHUNK: usize = 512;

/// The outline the logging `mago` prints for a file: one class, `printf`'s
/// first `%s` its path and the other two its name.
const CLASS_OUTLINE: &str = r#"{"version":1,"language":"sharp","path":"%s","namespace":"","errors":[],"symbols":[{"name":"%s","kind":"class","detail":"public class %s","range":{"start":{"line":0,"character":0},"end":{"line":1,"character":1}},"selectionRange":{"start":{"line":0,"character":13},"end":{"line":0,"character":14}},"visibility":"public","supertypes":[],"attributes":[],"children":[]}],"imports":[],"references":[]}"#;

/// A `mago` with `outline` in the fixture's scratch folder that writes each
/// start's arguments to `mago.log` there and outlines each file as the class
/// its first line, `public class <Name>`, declares, read from stdin or from
/// disk. A second line holding `MALFORMED` prints an outline that is not
/// JSON. On disk, `REWRITE` rewrites the file to `public class After`
/// before it is read, `BREAK` ends the run with an error, and `DROP` prints
/// no outline for the file. The `PATH` that finds it, and the log.
fn logging_mago(f: &Fixture) -> (String, std::path::PathBuf) {
    let bin = f.tmp.join("bin");
    let log = f.tmp.join("mago.log");
    script(
        &bin.join("mago"),
        &format!(
            "echo \"$*\" >> '{log}'\n\
             outline() {{\n  case \"$3\" in *MALFORMED*) echo '{{\"version\": 1, \"symbols\": ['; return ;; esac\n  \
             name=\"${{2#public class }}\"\n  printf '{CLASS_OUTLINE}\\n' \"$1\" \"$name\" \"$name\"\n}}\n\
             [ \"$1 $2\" = 'outline --help' ] && {{ echo 'Usage: mago outline'; exit 0; }}\n\
             [ \"$1 $2\" = 'outline --json' ] || {{ echo \"called as: $*\" >&2; exit 64; }}\n\
             if [ \"$3\" = --stdin-input ]; then\n  {{ read -r first; read -r second; }}\n  outline \"$4\" \"$first\" \"$second\"\n  exit\nfi\n\
             shift 2\nfor file in \"$@\"; do\n  {{ read -r first; read -r second; }} < \"$file\"\n  \
             case \"$second\" in *REWRITE*) printf 'public class After\\n' > \"$file\"; {{ read -r first; read -r second; }} < \"$file\" ;; esac\n  \
             case \"$second\" in *BREAK*) echo 'mago: the batch broke' >&2; exit 3 ;; *DROP*) continue ;; esac\n  \
             outline \"$file\" \"$first\" \"$second\"\ndone\n",
            log = log.display()
        ),
    );
    (path_with(&bin), log)
}

/// Each `mago` start the log holds, as its arguments.
fn starts(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log).unwrap_or_default().lines().map(str::to_string).collect()
}

/// The names of the rows `trace read file --json` gives `file`.
fn row_names(f: &Fixture, path: &str, file: &str) -> Vec<String> {
    surface(f, path, file).iter().map(|row| row["name"].as_str().unwrap_or_default().to_string()).collect()
}

#[test]
fn a_cold_build_starts_mago_once_per_chunk_and_every_file_gets_its_rows() {
    let f = Fixture::new();
    let files = CHUNK + 88;
    for at in 0..files {
        f.write(&format!("lib/C{at}.sharp"), &format!("public class C{at}\n"));
    }
    f.commit("classes");
    let (path, log) = logging_mago(&f);

    f.trace_env(&["cache", "build"], &[("PATH", &path)]).ok();
    let started = starts(&log);
    let batches: Vec<&String> = started.iter().filter(|start| start.starts_with("outline --json lib/")).collect();
    assert_eq!(started.iter().filter(|start| *start == "outline --help").count(), 1, "the build is asked about outline once: {started:?}");
    assert!(batches.len() <= files.div_ceil(CHUNK), "one start per chunk of files: {} starts", batches.len());
    assert_eq!(started.len(), batches.len() + 1, "no file starts mago on its own: {started:?}");
    let outlined: std::collections::BTreeSet<&str> = batches.iter().flat_map(|batch| batch.split(' ').skip(2)).collect();
    assert_eq!(outlined.len(), files, "the batches read every file once");

    let listing = f.tmp.join("api.txt");
    f.trace_env(&["api", "lib", "--output", listing.to_str().unwrap()], &[("PATH", &path)]).ok();
    let api = std::fs::read_to_string(&listing).unwrap();
    for at in 0..files {
        assert!(api.contains(&format!("public class C{at}\n")), "C{at}.sharp has its row: {api}");
    }
    assert_eq!(starts(&log), started, "the stored facts answer with no start");
}

#[test]
fn a_file_changed_after_trace_read_it_keeps_the_facts_of_the_bytes_trace_read() {
    let f = Fixture::new();
    let before = "public class Before\n// REWRITE\n";
    f.write("lib/Moving.sharp", before);
    f.write("lib/Still.sharp", "public class Still\n");
    f.commit("moving");
    let (path, log) = logging_mago(&f);

    f.trace_env(&["cache", "build"], &[("PATH", &path)]).ok();
    assert_eq!(std::fs::read_to_string(f.root.join("lib/Moving.sharp")).unwrap(), "public class After\n", "mago rewrote the file before it read it");
    let started = starts(&log);
    assert!(started.contains(&"outline --json lib/Moving.sharp lib/Still.sharp".to_string()), "{started:?}");
    assert!(started.contains(&"outline --json --stdin-input lib/Moving.sharp".to_string()), "the changed file is read from the bytes trace read: {started:?}");
    assert!(!started.iter().any(|start| start.ends_with("--stdin-input lib/Still.sharp")), "an unchanged file keeps its batch outline: {started:?}");

    f.write("lib/Moving.sharp", before);
    assert_eq!(row_names(&f, &path, "lib/Moving.sharp"), ["Before"], "the entry those bytes key holds their own rows");
    assert_eq!(row_names(&f, &path, "lib/Still.sharp"), ["Still"]);
    assert_eq!(starts(&log), started, "the stored entries answer with no start");
}

#[test]
fn a_malformed_outline_in_a_batch_fails_only_its_own_file() {
    let f = Fixture::new();
    f.write("lib/A.sharp", "public class A\n");
    f.write("lib/B.sharp", "public class B\n// MALFORMED\n");
    f.write("lib/C.sharp", "public class C\n");
    f.commit("three");
    let (path, log) = logging_mago(&f);

    let build = f.trace_env(&["cache", "build"], &[("PATH", &path)]);
    build.code_is(2);
    assert!(build.stderr.contains("mago outline failed on lib/B.sharp: unreadable outline"), "{}", build.combined());
    let started = starts(&log);
    assert_eq!(started.len(), 2, "one probe and one batch: {started:?}");
    assert!(started.contains(&"outline --json lib/A.sharp lib/B.sharp lib/C.sharp".to_string()), "{started:?}");

    assert_eq!(row_names(&f, &path, "lib/A.sharp"), ["A"]);
    assert_eq!(row_names(&f, &path, "lib/C.sharp"), ["C"]);
    let failed = f.trace_env(&["read", "lib/B.sharp"], &[("PATH", &path)]);
    failed.code_is(2);
    assert!(failed.stderr.contains("mago outline failed on lib/B.sharp: unreadable outline"), "the stored failure is named again: {}", failed.combined());
    assert!(failed.stdout.contains("L1: public class B"), "the lines still print: {}", failed.stdout);
    assert_eq!(starts(&log), started, "the stored facts and failure answer with no start");
}

#[test]
fn a_batch_that_fails_or_prints_too_few_outlines_is_named_and_each_file_is_read_on_its_own() {
    let cases = [
        ("// BREAK", "mago outline failed on 2 files, lib/A.sharp first: mago: the batch broke"),
        ("// DROP", "mago outline failed on 2 files, lib/A.sharp first: printed 1 outlines for 2 files"),
    ];
    for (marker, named) in cases {
        let f = Fixture::new();
        f.write("lib/A.sharp", "public class A\n");
        f.write("lib/B.sharp", &format!("public class B\n{marker}\n"));
        f.commit("two");
        let (path, log) = logging_mago(&f);

        let build = f.trace_env(&["cache", "build"], &[("PATH", &path)]);
        build.code_is(2);
        assert!(build.stderr.contains(named), "{marker}: the batch's failure is named: {}", build.combined());
        let started = starts(&log);
        assert!(started.contains(&"outline --json lib/A.sharp lib/B.sharp".to_string()), "{marker}: {started:?}");
        assert!(started.contains(&"outline --json --stdin-input lib/A.sharp".to_string()), "{marker}: each file is read on its own: {started:?}");
        assert!(started.contains(&"outline --json --stdin-input lib/B.sharp".to_string()), "{marker}: {started:?}");
        assert_eq!(row_names(&f, &path, "lib/A.sharp"), ["A"], "{marker}");
        assert_eq!(row_names(&f, &path, "lib/B.sharp"), ["B"], "{marker}");
    }
}

#[test]
fn a_second_diff_starts_no_mago_for_a_revision_it_already_read() {
    let f = Fixture::new();
    f.write("lib/Store.sharp", "public class Store\n");
    f.commit("store");
    f.write("lib/Store.sharp", "public class Shop\n");
    let (path, log) = logging_mago(&f);

    f.trace_env(&["diff"], &[("PATH", &path)]).ok();
    let started = starts(&log);
    assert!(started.contains(&"outline --json --stdin-input lib/Store.sharp".to_string()), "the first diff reads the committed side: {started:?}");

    let again = f.trace_env(&["diff", "--json"], &[("PATH", &path)]);
    again.ok();
    assert_eq!(starts(&log), started, "a second diff starts no mago");
    assert!(again.stdout.contains("Shop") && again.stdout.contains("Store"), "both sides still answer: {}", again.stdout);
}

const CLIENT: &str = "namespace App.Http;

public class Client
{
    public static Client make() { return new Client(); }
    public Response post(string path) { return new Response(); }
    public void none() { }
    public Sheet sheet() { return new Sheet(); }
    public Vendor.Http.Response remote() { return null; }
}
";

/// A one-line method on the 0-based `line` whose outline states
/// `return_type`, `null` when it is `None`.
fn method(name: &str, detail: &str, line: i64, return_type: Option<&str>, children: Vec<Value>) -> Value {
    let mut method = function(name, "method", detail, (line, line), 1, children);
    method["returnType"] = return_type.into();
    method
}

/// The outline of `CLIENT`: each method's `returnType` as `mago outline`
/// gives it, a declared type by its full name, which is undotted for a
/// class in no namespace, and `null` for none.
fn client_outline() -> Value {
    document(
        vec![symbol(
            "Client",
            "class",
            "public class Client",
            (2, 9),
            vec![
                method("make", "public static Client make()", 4, Some("App.Http.Client"), vec![]),
                method("post", "public Response post(string path)", 5, Some("App.Http.Response"), vec![]),
                method("none", "public void none()", 6, None, vec![]),
                method("sheet", "public Sheet sheet()", 7, Some("Sheet"), vec![]),
                method("remote", "public Vendor.Http.Response remote()", 8, Some("Vendor.Http.Response"), vec![]),
            ],
        )],
        vec![],
        vec![],
    )
}

/// A `.sharp` class `name` in `namespace`, or in none, with one `json`
/// method that returns a built-in, and its outline.
fn json_class(namespace: Option<&str>, name: &str) -> (String, Value) {
    let opening = namespace.map_or(String::new(), |namespace| format!("namespace {namespace};\n\n"));
    let at = if namespace.is_some() { 2 } else { 0 };
    let source = format!("{opening}public class {name}\n{{\n    public Map json() {{ return []; }}\n}}\n");
    let outline = document(
        vec![symbol(name, "class", &format!("public class {name}"), (at, at + 3), vec![method("json", "public Map json()", at + 2, None, vec![])])],
        vec![],
        vec![],
    );
    (source, outline)
}

/// A repository whose `App\` classes are `.sharp` files under `app/Http/`,
/// `Client` and a `json` class for each of `Response` and `Payload`, with
/// the `json` class `Sheet` in no namespace in `lib/Sheet.sharp` and
/// `source` as `caller`, read through `outline` when it is a `.sharp` file,
/// and the `PATH` that finds a `mago` for them.
fn sharp_http(f: &Fixture, caller: &str, source: &str, outline: Option<Value>) -> String {
    f.write("composer.json", r#"{"autoload": {"psr-4": {"App\\": "app/"}}}"#);
    f.write("app/Http/Client.sharp", CLIENT);
    let mut outlines = vec![("app/Http/Client.sharp".to_string(), client_outline())];
    for (namespace, name, file) in [
        (Some("App.Http"), "Response", "app/Http/Response.sharp"),
        (Some("App.Http"), "Payload", "app/Http/Payload.sharp"),
        (None, "Sheet", "lib/Sheet.sharp"),
    ] {
        let (written, outline) = json_class(namespace, name);
        f.write(file, &written);
        outlines.push((file.to_string(), outline));
    }
    f.write(caller, source);
    outlines.extend(outline.map(|outline| (caller.to_string(), outline)));
    f.commit("http classes");
    let outlines: Vec<(&str, Value)> = outlines.iter().map(|(file, outline)| (file.as_str(), outline.clone())).collect();
    mago_path(f, &outlines)
}

const FUNNELS: &str = "namespace App.Flows;

import App.Http.Client;

public class Funnels
{
    public Map viaStatic() { return Client.make().post(\"/c\").json(); }
    public Map viaSheet() { return Client.make().sheet().json(); }
}
";

/// A call reference on the 0-based `line` made on the result of the
/// reference at `receiver_call`.
fn chained(name: &str, line: i64, receiver_call: usize) -> Value {
    let mut chained = call(name, line, None, None);
    chained["receiverCall"] = receiver_call.into();
    chained
}

/// The outline of `FUNNELS`: each chain's first call on `Client`, and each
/// later call on the call before it.
fn funnels_outline() -> Value {
    let make = |line: i64| call("make", line, Some("App.Http.Client"), Some("App.Http.Client.make"));
    document(
        vec![symbol(
            "Funnels",
            "class",
            "public class Funnels",
            (4, 8),
            vec![
                method("viaStatic", "public Map viaStatic()", 6, None, vec![]),
                method("viaSheet", "public Map viaSheet()", 7, None, vec![]),
            ],
        )],
        vec![json!({"path": "App.Http.Client", "alias": null, "range": range(2, 2)})],
        vec![make(6), chained("post", 6, 0), chained("json", 6, 1), make(7), chained("sheet", 7, 3), chained("json", 7, 4)],
    )
}

#[test]
fn a_php_chain_through_sharp_methods_takes_each_declared_return_type() {
    let f = Fixture::new();
    let path = sharp_http(
        &f,
        "app/Flows/Funnels.php",
        "<?php\nnamespace App\\Flows;\nuse App\\Http\\Client;\nclass Funnels {\n    public function viaStatic() { return Client::make()->post('/c')->json(); }\n}\n",
        None,
    );

    assert_eq!(
        callers(&f, &path, "Response::json"),
        [("app/Flows/Funnels.php".to_string(), "viaStatic".to_string(), 5, "INFERRED".to_string())],
        "the chain reaches the json of the class post returns, at the confidence the all-PHP chain gets"
    );
    assert_eq!(callers(&f, &path, "Payload::json"), [], "no other json takes the call");
}

#[test]
fn a_sharp_chain_reaches_the_one_method_each_call_returns_as_a_php_chain_does() {
    let f = Fixture::new();
    let path = sharp_http(&f, "app/Flows/Funnels.sharp", FUNNELS, Some(funnels_outline()));
    let funnels = |caller: &str, line: i64| ("app/Flows/Funnels.sharp".to_string(), caller.to_string(), line, "INFERRED".to_string());

    assert_eq!(
        callers(&f, &path, "Response::json"),
        [funnels("viaStatic", 7)],
        "Client.make().post().json() reaches the json of the class post returns, at the confidence the PHP chain gets"
    );
    assert_eq!(callers(&f, &path, "Payload::json"), [], "no other json takes the call");
}

#[test]
fn a_chain_through_a_class_in_no_namespace_reaches_its_method() {
    let f = Fixture::new();
    let path = sharp_http(&f, "app/Flows/Funnels.sharp", FUNNELS, Some(funnels_outline()));

    assert_eq!(
        callers(&f, &path, "Sheet::json"),
        [("app/Flows/Funnels.sharp".to_string(), "viaSheet".to_string(), 8, "INFERRED".to_string())],
        "the undotted returnType `Sheet` names the class Sheet"
    );
}

#[test]
fn a_receiver_call_that_names_no_earlier_call_fails_the_file_with_the_reason() {
    for receiver_call in [0, 3] {
        let f = Fixture::new();
        f.write("Store.sharp", STORE);
        f.commit("store");
        let mut tree = outline(2);
        tree["references"][0]["receiverCall"] = receiver_call.into();
        let bin = f.tmp.join("bin");
        mago(&bin.join("mago"), &tree, &older_outline());
        let read = f.trace_env(&["read", "Store.sharp"], &[("PATH", &path_with(&bin))]);
        assert_eq!(read.code, 2, "{receiver_call}: {}", read.combined());
        assert!(
            read.stderr.contains(&format!("mago outline failed on Store.sharp: reference 0's receiverCall {receiver_call} names no earlier call")),
            "the reason names the index: {}",
            read.combined()
        );
        assert!(read.stdout.contains("L 5: public class Store"), "{receiver_call}: the lines still print: {}", read.combined());
        assert!(!read.stdout.contains("pick()"), "{receiver_call}: no row of the outline is kept: {}", read.combined());
    }
}

#[test]
fn a_return_type_that_names_no_class_of_the_repository_keeps_the_call_on_a_value() {
    let f = Fixture::new();
    let path = sharp_http(
        &f,
        "app/Flows/Funnels.php",
        concat!(
            "<?php\nnamespace App\\Flows;\nuse App\\Http\\Client;\nclass Funnels {\n",
            "    public function viaNothing() { return Client::make()->none()->json(); }\n",
            "    public function viaVendor() { return Client::make()->remote()->json(); }\n",
            "}\n",
        ),
        None,
    );
    let on_a_value = |caller: &str, line: i64| ("app/Flows/Funnels.php".to_string(), caller.to_string(), line, "AMBIGUOUS".to_string());
    let every_chain = [on_a_value("viaNothing", 5), on_a_value("viaVendor", 6)];

    assert_eq!(callers(&f, &path, "Payload::json"), every_chain, "a null returnType names no class");
    assert_eq!(callers(&f, &path, "Response::json"), every_chain, "Vendor.Http.Response names no class of the repository, so never App.Http.Response");
}
