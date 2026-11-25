//! `.sharp` extraction: `mago outline` over a batch of working-tree files,
//! or one file's bytes on stdin, its version 1 JSON, the Language Server
//! Protocol's document-symbol tree, read into the
//! declarations, imports, references, and per-function complexity a PHP file
//! gets from its tree.

use super::{Declaration, Extraction, ExtractionResult, Failure, Import, Reader, RefShape, Reference, Visibility};
use crate::ccn::FunctionFact;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

/// The extension `mago outline` reads (lowercase, no leading dot).
pub const EXTENSION: &str = "sharp";

/// The one outline version this reader reads.
const VERSION: u64 = 1;

/// How long one `mago outline` may take over one file. Generous, so a machine
/// busy with parallel agents never reaches it; a run that does is stuck.
const OUTLINE_LIMIT: Duration = Duration::from_secs(30);

/// How much longer a batch may take for each file it reads: twenty times
/// `mago`'s goal of 50 ms a file on one thread, where a batch runs on every
/// core.
const OUTLINE_FILE_LIMIT: Duration = Duration::from_secs(1);

/// How long one `mago outline` over `files` files may take: a start's
/// `start` and `per_file` more for each file.
fn batch_limit(files: usize, start: Duration, per_file: Duration) -> Duration {
    start + per_file * files as u32
}

/// Whether `path` is a `.sharp` file.
pub fn reads(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
}

/// The `mago` a repository finds.
#[derive(Clone)]
pub enum Mago {
    /// No `mago` is installed.
    Missing,
    /// A `mago` with no `outline` command, as in every PHP# release before
    /// the one that added it.
    WithoutOutline(Reader),
    /// A `mago` that reads `.sharp` files.
    Outlines(Reader),
}

/// The `mago` `path`'s repository finds: the first with `outline` of its
/// `vendor/bin/mago`, which the Composer package `heyjordanparker/mago-sharp`
/// installs at the project's Sharp version, and the `mago` on `PATH`, else
/// the first of them. So a `vendor/bin/mago` that can't run here, such as one
/// installed for another platform, gives way to the one on `PATH`. Found once
/// per repository per run, and each build is asked once, stored beside its
/// stamp, so a warm call spawns nothing.
pub fn find(path: &Path) -> Mago {
    static MAGO: crate::memo::Memo<Mago> = OnceLock::new();
    let root = crate::cache::worktree_root_for(path);
    let key = root.clone().unwrap_or_default();
    let found = crate::memo::get_or_build(&MAGO, &key, || {
        let mut binaries: Vec<Reader> = root
            .as_ref()
            .and_then(|root| Reader::at(&root.join("vendor").join("bin").join("mago")))
            .into_iter()
            .chain(Reader::on_path("mago"))
            .collect();
        binaries.dedup();
        let probes = match &root {
            Some(root) => crate::cache::index(&OutlineIndex { binaries: binaries.clone() }, root).ready().unwrap_or_default(),
            None => binaries.iter().cloned().map(probe).collect(),
        };
        let answered = |binary: &Reader| probes.iter().find(|probe| &probe.binary == binary).map(|probe| probe.answer);
        let chosen = binaries
            .iter()
            .find(|binary| answered(binary) == Some(Answer::Outlines))
            .or(binaries.first());
        match chosen {
            None => Mago::Missing,
            Some(binary) if answered(binary) == Some(Answer::WithoutOutline) => Mago::WithoutOutline(binary.clone()),
            Some(binary) => Mago::Outlines(binary.clone()),
        }
    });
    found.as_ref().clone()
}

/// The `mago` that reads `path`, or `None` when its repository finds none
/// with `outline`, and its `.sharp` files are then unsupported.
pub fn mago(path: &Path) -> Option<Reader> {
    match find(path) {
        Mago::Outlines(binary) => Some(binary),
        Mago::Missing | Mago::WithoutOutline(_) => None,
    }
}

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
enum Answer {
    Outlines,
    WithoutOutline,
    Fails,
}

/// What one build of `mago` answers `outline --help`: a `mago` from before
/// `outline` answers with clap's `unrecognized subcommand`, and one that
/// can't run here fails any other way. A `mago` that fails is still named on
/// the files it fails when no other has `outline`.
fn probe(binary: Reader) -> OutlineProbe {
    let output = crate::timing::phase("mago outline probe", || {
        crate::process::output(Command::new(&binary.path).args(["outline", "--help"]))
    });
    let answer = match output {
        Ok(output) if output.status.success() => Answer::Outlines,
        Ok(output) if String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand") => Answer::WithoutOutline,
        _ => Answer::Fails,
    };
    OutlineProbe { binary, answer }
}

/// What `probe` answered for one build of `mago`.
#[derive(Clone, Deserialize, Serialize)]
struct OutlineProbe {
    binary: Reader,
    answer: Answer,
}

/// `file/schema<N>/mago_outline_v2.json`: what each `mago` build a
/// repository finds answered, asked again only for a build not yet asked.
struct OutlineIndex {
    binaries: Vec<Reader>,
}

impl crate::cache::Index for OutlineIndex {
    type Stored = Vec<OutlineProbe>;
    type Change = ();

    fn key(&self) -> String {
        "mago_outline_v2".to_string()
    }

    fn read(&self, bytes: &[u8]) -> Option<Vec<OutlineProbe>> {
        serde_json::from_slice(bytes).ok()
    }

    fn change(&self, stored: Option<&Vec<OutlineProbe>>) -> Option<()> {
        let asked: Option<Vec<&Reader>> = stored.map(|probes| probes.iter().map(|probe| &probe.binary).collect());
        (asked != Some(self.binaries.iter().collect())).then_some(())
    }

    fn apply(&self, stored: Option<Vec<OutlineProbe>>, _change: ()) -> Option<Vec<OutlineProbe>> {
        let stored = stored.unwrap_or_default();
        Some(
            self.binaries
                .iter()
                .map(|binary| stored.iter().find(|probe| &probe.binary == binary).cloned().unwrap_or_else(|| probe(binary.clone())))
                .collect(),
        )
    }
}

/// A `.sharp` file's declarations and per-function complexity from
/// `mago outline --stdin-input`, `source` on its stdin: `Unsupported` with
/// no `mago`, and `Failed` when `mago`
/// exits with an error, runs past `OUTLINE_LIMIT`, or prints an outline this
/// reader can't read, reported once per run.
pub fn extract(source: &[u8], path: &str) -> (Extraction, Vec<FunctionFact>) {
    let Some(mago) = mago(Path::new(path)) else {
        return (Extraction::Unsupported, Vec::new());
    };
    let label = match crate::cache::worktree_root_for(Path::new(path)) {
        Some(root) => crate::cache::relative_to_root(Path::new(path), &root),
        None => path.to_string(),
    };
    let outlined = outline(Command::new(&mago.path).args(["outline", "--json", "--stdin-input", &label]), source, OUTLINE_LIMIT);
    extraction(&label, outlined.and_then(|stdout| read(&stdout)))
}

/// The declarations and per-function complexity of the working-tree files
/// `paths` names, relative to `root`, from one `mago outline --json
/// <path>...` that reads them from disk, in the order given: a malformed
/// outline fails only its own file. A file is `None`, to be read on its own,
/// when `unchanged` says it changed on disk after its bytes were read, and
/// every file is `None` when no `mago` outlines, or when the run fails or
/// prints another number of outlines than files, which is reported once per
/// run.
pub fn extract_on_disk(
    root: &Path,
    paths: &[&str],
    unchanged: impl Fn(usize) -> bool,
) -> Vec<Option<(Extraction, Vec<FunctionFact>)>> {
    let unread = || paths.iter().map(|_| None).collect();
    let Some(mago) = mago(root) else {
        return unread();
    };
    let limit = batch_limit(paths.len(), OUTLINE_LIMIT, OUTLINE_FILE_LIMIT);
    let failed = |reason: &str| {
        let named = match paths {
            [path] => path.to_string(),
            _ => format!("{} files, {} first", paths.len(), paths.first().unwrap_or(&"")),
        };
        report_failure(&named, reason);
        unread()
    };
    let stdout = match outline(Command::new(&mago.path).current_dir(root).args(["outline", "--json"]).args(paths), &[], limit) {
        Ok(stdout) => stdout,
        Err(reason) => return failed(&reason),
    };
    let lines: Vec<&[u8]> = stdout.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).collect();
    if lines.len() != paths.len() {
        return failed(&format!("printed {} outlines for {} files", lines.len(), paths.len()));
    }
    lines
        .iter()
        .zip(paths)
        .enumerate()
        .map(|(at, (line, path))| unchanged(at).then(|| extraction(path, read(line))))
        .collect()
}

/// One file's extraction from what its outline read gave, a failure
/// reported.
fn extraction(path: &str, read: Result<(ExtractionResult, Vec<FunctionFact>), String>) -> (Extraction, Vec<FunctionFact>) {
    match read {
        Ok((result, functions)) => (Extraction::Extracted(result), functions),
        Err(reason) => {
            report_failure(path, &reason);
            (Extraction::Failed(Failure { reason }), Vec::new())
        }
    }
}

/// Names a `mago outline` failure once per run: a broken `mago` fails every
/// `.sharp` file, and the first names the cause.
pub(super) fn report_failure(path: &str, reason: &str) {
    static REPORTED: std::sync::Once = std::sync::Once::new();
    REPORTED.call_once(|| crate::process::report(&format!("mago outline failed on {path}: {reason}")));
}

/// The JSON `command`, a `mago outline` with `input` on its stdin, prints
/// within `limit`, or why it printed none.
fn outline(command: &mut Command, input: &[u8], limit: Duration) -> Result<Vec<u8>, String> {
    let output = crate::timing::phase("mago outline", || crate::process::output_within(command, input, limit))
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("ran past {}s", limit.as_secs()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(match stderr.is_empty() {
            true => format!("exited with {}", output.status),
            false => stderr,
        });
    }
    Ok(output.stdout)
}

/// The declarations and per-function complexity of one outline, or why this
/// reader can't read it: another version, JSON that is not version 1, or a
/// `receiverCall` that names no earlier call. A version 1 outline is parsed
/// once; only one that fails is read again for the version it names.
fn read(stdout: &[u8]) -> Result<(ExtractionResult, Vec<FunctionFact>), String> {
    #[derive(Deserialize)]
    struct Versioned {
        version: u64,
    }
    let parsed = serde_json::from_slice::<v1::Outline>(stdout);
    let version = match &parsed {
        Ok(outline) => outline.version,
        Err(_) => serde_json::from_slice::<Versioned>(stdout).map_or(VERSION, |versioned| versioned.version),
    };
    if version != VERSION {
        return Err(format!("outline version {version}, and this trace reads version {VERSION}"));
    }
    let outline = parsed.map_err(|error| format!("unreadable outline: {error}"))?;
    let mut walked = Walked::default();
    walked.symbols(&outline.symbols, &Enclosing::default());
    let result = ExtractionResult {
        language: EXTENSION.to_string(),
        imports: outline.imports.iter().map(import).collect(),
        declarations: walked.declarations,
        references: references(&outline.references)?,
    };
    Ok((result, walked.functions))
}

/// What the symbols above a symbol give it: the row it nests under, the type
/// a member names as its container, and the qualified name a function's
/// name continues.
#[derive(Default, Clone)]
struct Enclosing {
    parent: Option<u32>,
    container: Option<String>,
    qualified: Option<Qualified>,
}

/// The enclosing types and functions, joined the way `ccn` qualifies a PHP
/// function, and whether the innermost is a type.
#[derive(Clone)]
struct Qualified {
    name: String,
    is_type: bool,
}

impl Qualified {
    /// `name` inside `outer`: joined by `::` when either side is a type, else
    /// by `.`.
    fn within(outer: Option<&Qualified>, name: &str, is_type: bool) -> Qualified {
        let name = match outer {
            None => name.to_string(),
            Some(outer) if outer.is_type || is_type => format!("{}::{name}", outer.name),
            Some(outer) => format!("{}.{name}", outer.name),
        };
        Qualified { name, is_type }
    }
}

/// The rows and per-function complexity of a symbol tree, in source order.
#[derive(Default)]
struct Walked {
    declarations: Vec<Declaration>,
    functions: Vec<FunctionFact>,
}

impl Walked {
    /// Walks `symbols` depth-first, each sibling list in source order: a
    /// symbol of a row kind gives its row, a symbol that carries
    /// `complexity` its function, and a symbol PHP gives no row passes
    /// through to its children when it is a scope, such as a namespace.
    fn symbols(&mut self, symbols: &[v1::Symbol], enclosing: &Enclosing) {
        let mut ordered: Vec<&v1::Symbol> = symbols.iter().collect();
        ordered.sort_by_key(|symbol| symbol.range.start.line);
        for symbol in ordered {
            let kind = symbol.kind;
            let qualified = Qualified::within(enclosing.qualified.as_ref(), &symbol.name, kind.is_type());
            if let Some(complexity) = symbol.complexity {
                self.functions.push(FunctionFact {
                    name: qualified.name.clone(),
                    start_line: symbol.range.start.line + 1,
                    nloc: symbol.range.end.line - symbol.range.start.line + 1,
                    cyclomatic_complexity: complexity,
                });
            }
            let mut inner = enclosing.clone();
            if kind.is_type() || kind.is_function() {
                inner.qualified = Some(qualified);
            }
            if kind.is_type() {
                inner.container = Some(symbol.name.clone());
            }
            match kind.row() {
                v1::Row::Of(row) => {
                    inner.parent = Some(self.declarations.len() as u32);
                    self.declarations.push(Declaration {
                        name: symbol.name.clone(),
                        kind: row.to_string(),
                        header_line: symbol.range.start.line + 1,
                        line: symbol.selection_range.start.line + 1,
                        end_line: symbol.range.end.line + 1,
                        container: enclosing.container.clone().filter(|_| kind.is_member()),
                        parent: enclosing.parent,
                        header: symbol.detail.clone().unwrap_or_else(|| symbol.name.clone()),
                        annotations: symbol.attributes.clone(),
                        self_type: None,
                        module_file: None,
                        supertypes: symbol.supertypes.iter().map(|supertype| last_segment(supertype).to_string()).collect(),
                        returned_class: symbol.return_type.as_ref().map(|declared| declared.replace('.', "\\")),
                        visibility: symbol.visibility.map_or(Visibility::Unknown, v1::Visibility::row),
                    });
                    self.symbols(&symbol.children, &inner);
                }
                v1::Row::Scope => self.symbols(&symbol.children, &inner),
                v1::Row::None => {}
            }
        }
    }
}

/// An outline import as PHP's `use` gives it: the module, each of Sharp's
/// dots one of PHP's backslashes, and the symbol it binds, the path's last
/// segment.
fn import(import: &v1::Import) -> Import {
    let path = import.path.replace('.', "\\");
    let (module, symbol) = match path.rsplit_once('\\') {
        Some((module, symbol)) => (module.to_string(), Some(symbol.to_string())),
        None => (path, None),
    };
    Import {
        module,
        symbol,
        locals: import.alias.iter().cloned().collect(),
        line: import.range.start.line + 1,
        end_line: import.range.end.line + 1,
        block: None,
        visibility: None,
    }
}

/// The outline's references as PHP's extractor gives them, each call on
/// another call's result read through the call its `receiverCall` names, or
/// why they can't be read: a `receiverCall` that names no earlier call.
fn references(references: &[v1::Reference]) -> Result<Vec<Reference>, String> {
    let mut read: Vec<Reference> = Vec::with_capacity(references.len());
    for (at, written) in references.iter().enumerate() {
        let on = match written.receiver_call {
            None => None,
            Some(call) if call < at && matches!(references[call].kind, v1::ReferenceKind::Call) => Some(&read[call]),
            Some(call) => return Err(format!("reference {at}'s receiverCall {call} names no earlier call")),
        };
        read.push(reference(written, on));
    }
    Ok(read)
}

/// An outline reference as PHP's extractor gives it, each name by its last
/// segment. A call with a receiver type is a member call on that type, one
/// on the result of the call `on` is a member call on `Class->method()`, the
/// receiver PHP's extractor writes for that chain, one with only a target
/// that is a method, as on a built-in, a member call on the target's class,
/// one with only another target a free call, and one with none of them a
/// member call on a value; `new T` and `T::class` are a static use whose
/// receiver is `T`.
fn reference(reference: &v1::Reference, on: Option<&Reference>) -> Reference {
    let name = last_segment(&reference.name).to_string();
    let segment = |name: &str| last_segment(name).to_string();
    let (shape, receiver) = match (reference.kind, &reference.receiver_type, on, &reference.target) {
        (v1::ReferenceKind::Call, Some(receiver), _, _) => (RefShape::Member, Some(segment(receiver))),
        (v1::ReferenceKind::Call, None, Some(call), _) => {
            (RefShape::Member, call.receiver.as_ref().map(|class| format!("{class}->{}()", call.name)))
        }
        (v1::ReferenceKind::Call, None, None, Some(target)) if matches!(reference.target_kind, Some(v1::Kind::Method)) => {
            (RefShape::Member, target.rsplit_once('.').map(|(class, _)| segment(class)))
        }
        (v1::ReferenceKind::Call, None, None, Some(_)) => (RefShape::Free, None),
        (v1::ReferenceKind::Call, None, None, None) => (RefShape::Member, None),
        (v1::ReferenceKind::New | v1::ReferenceKind::Type, _, _, target) => {
            (RefShape::Static, Some(segment(target.as_deref().unwrap_or(&name))))
        }
    };
    Reference {
        name,
        line: reference.range.start.line + 1,
        shape,
        receiver,
    }
}

/// The last segment of a dotted name, as PHP's extractor keeps a name.
fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// The JSON `mago outline` prints, version 1: the Language Server Protocol's
/// `DocumentSymbol` tree, positions 0-based, beside the file's imports and
/// references. A value outside each list below fails the whole outline.
mod v1 {
    use serde::Deserialize;

    #[derive(Deserialize)]
    pub struct Outline {
        pub version: u64,
        pub symbols: Vec<Symbol>,
        pub imports: Vec<Import>,
        pub references: Vec<Reference>,
    }

    #[derive(Deserialize)]
    pub struct Position {
        pub line: i64,
    }

    #[derive(Deserialize)]
    pub struct Range {
        pub start: Position,
        pub end: Position,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Symbol {
        pub name: String,
        pub kind: Kind,
        pub detail: Option<String>,
        pub range: Range,
        pub selection_range: Range,
        pub visibility: Option<Visibility>,
        #[serde(default)]
        pub supertypes: Vec<String>,
        #[serde(default)]
        pub attributes: Vec<String>,
        pub complexity: Option<i64>,
        pub return_type: Option<String>,
        #[serde(default)]
        pub children: Vec<Symbol>,
    }

    /// The 13 Language Server Protocol `SymbolKind` names `mago outline`
    /// prints.
    #[derive(Deserialize, Clone, Copy)]
    #[serde(rename_all = "camelCase")]
    pub enum Kind {
        Namespace,
        Class,
        Interface,
        Enum,
        Struct,
        Method,
        Constructor,
        Function,
        Property,
        Field,
        Constant,
        EnumMember,
        TypeParameter,
    }

    /// What a symbol gives PHP's rows: a row of a kind, no row while its
    /// children are still walked, or nothing.
    pub enum Row {
        Of(&'static str),
        Scope,
        None,
    }

    impl Kind {
        pub fn row(self) -> Row {
            match self {
                Kind::Class | Kind::Struct => Row::Of("class"),
                Kind::Interface => Row::Of("interface"),
                Kind::Enum => Row::Of("enum"),
                Kind::Method | Kind::Constructor | Kind::Function => Row::Of("function"),
                Kind::Property | Kind::Field | Kind::EnumMember => Row::Of("property"),
                Kind::Constant => Row::Of("constant"),
                Kind::Namespace => Row::Scope,
                Kind::TypeParameter => Row::None,
            }
        }

        pub fn is_type(self) -> bool {
            matches!(self, Kind::Class | Kind::Struct | Kind::Interface | Kind::Enum)
        }

        pub fn is_function(self) -> bool {
            matches!(self, Kind::Method | Kind::Constructor | Kind::Function)
        }

        /// A declaration a type holds, which names the type as its
        /// container.
        pub fn is_member(self) -> bool {
            matches!(
                self,
                Kind::Method | Kind::Constructor | Kind::Property | Kind::Field | Kind::EnumMember | Kind::Constant
            )
        }
    }

    #[derive(Deserialize, Clone, Copy)]
    #[serde(rename_all = "lowercase")]
    pub enum Visibility {
        Public,
        Protected,
        Private,
        Internal,
    }

    impl Visibility {
        /// The row's visibility, a `private` member reaching only its file
        /// as a PHP one does.
        pub fn row(self) -> super::Visibility {
            match self {
                Visibility::Public => super::Visibility::Public,
                Visibility::Protected => super::Visibility::Protected,
                Visibility::Private => super::Visibility::FileLocal,
                Visibility::Internal => super::Visibility::Internal,
            }
        }
    }

    #[derive(Deserialize)]
    pub struct Import {
        pub path: String,
        pub alias: Option<String>,
        pub range: Range,
    }

    #[derive(Deserialize, Clone, Copy)]
    #[serde(rename_all = "lowercase")]
    pub enum ReferenceKind {
        Call,
        New,
        Type,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Reference {
        pub name: String,
        pub kind: ReferenceKind,
        pub range: Range,
        pub receiver_type: Option<String>,
        pub target: Option<String>,
        #[serde(default)]
        pub target_kind: Option<Kind>,
        #[serde(default)]
        pub receiver_call: Option<usize>,
    }
}

#[cfg(test)]
mod tests {
    use super::{batch_limit, outline};
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn a_batch_slower_than_one_files_limit_still_completes_in_one_start() {
        let folder = tempfile::tempdir().unwrap();
        let mago = folder.path().join("mago");
        std::fs::write(&mago, "#!/bin/sh\nshift 2\nfor file in \"$@\"; do\n  sleep 0.05\n  echo \"{\\\"path\\\": \\\"$file\\\"}\"\ndone\n").unwrap();
        std::fs::set_permissions(&mago, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755)).unwrap();
        let files: Vec<String> = (0..20).map(|at| format!("lib/C{at}.sharp")).collect();
        let batch = || {
            let mut command = Command::new(&mago);
            command.args(["outline", "--json"]).args(&files);
            command
        };
        let (start, per_file) = (Duration::from_millis(100), Duration::from_millis(100));

        let one_file = outline(&mut batch(), &[], start);
        assert!(one_file.as_ref().is_err_and(|reason| reason.starts_with("ran past")), "20 files at 50 ms each run past one file's limit: {one_file:?}");

        let printed = outline(&mut batch(), &[], batch_limit(files.len(), start, per_file)).expect("the batch completes");
        assert_eq!(printed.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).count(), files.len(), "one start prints every file's line");
    }
}
