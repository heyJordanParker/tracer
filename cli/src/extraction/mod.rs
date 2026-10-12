//! Per-language extraction.
//!
//! Each extractor returns an `ExtractionResult` of module-level imports and
//! exports via fixed tree-sitter query strings. The relations index and
//! `structure` command consume this. Per-function CCN is a separate
//! concern — see `crate::ccn`.

pub mod c;
pub mod go;
pub mod header;
pub mod java;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod typescript;

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

#[allow(non_upper_case_globals)]
const ctags_denied_languages: &[&str] = &[
    "Json",
    "Yaml",
    "Iniconf",
    "Diff",
    "XML",
    "DTD",
    "SVG",
    "XSLT",
    "RelaxNG",
    "PlistXML",
    "Maven2",
    "Glade",
    "DBusIntrospect",
    "Ant",
    "Toml",
];

#[allow(non_upper_case_globals)]
const ctags_denied_kinds: &[&str] = &["heredoc"];

/// One imported name. A Rust `type Name = path;` imports `path` bound as
/// `Name`, the way `use path as Name;` does. `line` and `end_line` are the
/// first and last lines of the statement that imports it, which several
/// imports share when one statement binds several names. `block` is the
/// first and last line of the block that holds the import, a Rust `mod` body
/// or block, and `None` for the whole file. A re-export, which makes the name
/// part of this file's API too, carries its `visibility`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Import {
    pub module: String,
    pub symbol: Option<String>,
    #[serde(default)]
    pub locals: Vec<String>,
    pub line: i64,
    pub end_line: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<(i64, i64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

impl Import {
    /// The name the import binds: its alias, else its symbol.
    pub fn binding(&self) -> Option<&str> {
        self.locals.first().or(self.symbol.as_ref()).map(String::as_str)
    }

    /// Whether the import binds its name at `line`: its block holds the
    /// line, and no import of `imports` whose block holds it is smaller and
    /// binds the same name.
    pub fn in_force(&self, line: i64, imports: &[Import]) -> bool {
        let holds = |import: &Import| import.block.is_none_or(|(first, last)| (first..=last).contains(&line));
        let size = |import: &Import| import.block.map_or(i64::MAX, |(first, last)| last - first);
        holds(self)
            && !self.binding().is_some_and(|name| {
                imports
                    .iter()
                    .any(|inner| inner.binding() == Some(name) && holds(inner) && size(inner) < size(self))
            })
    }
}

/// Who can reach a declaration, by its language's own rule. A row's value is
/// its own declared visibility; `exported` applies its parents' too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Reachable only from its own file.
    FileLocal,
    /// Reachable from other files, but not part of the public API.
    Restricted,
    /// The rule can't be read.
    #[default]
    Unknown,
    /// Part of the file's public API.
    Public,
}

/// The visibility of `declarations[index]` as part of the public API: the
/// narrowest along its parent chain, so a public method of a file-local
/// class is file-local.
pub fn exported(declarations: &[Declaration], index: usize) -> Visibility {
    let mut visibility = declarations[index].visibility;
    let mut parent = declarations[index].parent;
    while let Some(at) = parent.and_then(|at| declarations.get(at as usize)) {
        visibility = visibility.min(at.visibility);
        parent = at.parent;
    }
    visibility
}

/// The words before `name` on the header's line `offset`, its modifiers.
pub fn modifiers<'a>(header: &'a str, offset: i64, name: &str) -> Vec<&'a str> {
    let name = name.trim_start_matches(['$', '#']);
    header
        .lines()
        .nth(offset.max(0) as usize)
        .unwrap_or("")
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
        .take_while(|word| *word != name)
        .collect()
}

/// The index of the narrowest span that holds `line`, the later one on a tie.
pub fn innermost(spans: impl IntoIterator<Item = (i64, i64)>, line: i64) -> Option<usize> {
    spans
        .into_iter()
        .enumerate()
        .filter(|(_, (start, end))| (*start..=*end).contains(&line))
        .min_by_key(|(index, (start, end))| (end - start, std::cmp::Reverse(*index)))
        .map(|(index, _)| index)
}

/// A declaration — every named definition in the file, including
/// non-exported top-levels, methods on classes, and nested definitions.
/// `container` is the enclosing class/interface/trait/enum name for a method
/// declaration, `None` for a free function or a top-level type. Reference
/// resolution uses it to tell a method from a free function of the same name.
/// `self_type` is a Rust method's `Self`, the type its `impl` names or its
/// trait, read the way a call's receiver is read. `module_file` is a Rust
/// `mod` item's `#[path]` value. `supertypes` are the types a PHP class,
/// interface, trait, or enum names after `extends` and `implements` and in
/// its body's trait `use`, each by its last segment, nearest first: its
/// traits, then its parent, then its interfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Declaration {
    pub name: String,
    pub kind: String,
    pub header_line: i64,
    pub line: i64,
    pub end_line: i64,
    pub container: Option<String>,
    pub parent: Option<u32>,
    pub header: String,
    #[serde(default)]
    pub annotations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_file: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supertypes: Vec<String>,
    #[serde(default)]
    pub visibility: Visibility,
}

/// Lines `first` through `last` of `source`, 1-based, without the
/// indentation they share.
pub fn source_lines(source: &[u8], first: i64, last: i64) -> String {
    let mut builder = header::Builder::new(source);
    builder.slice(line_bytes(source, first).start, line_bytes(source, last.max(first)).end);
    builder.finish()
}

/// `declarations` in source order, each with its parent: the nearest
/// declaration `container` accepts whose source holds it. `ranges` holds each
/// declaration's byte range, in the same order, so a container and its
/// members on one line still nest, the way `pub enum Mode { On, Off }` does,
/// and so do the fields of `typedef struct { … } Name;`, named after them.
pub fn nest(declarations: Vec<Declaration>, ranges: Vec<std::ops::Range<usize>>, container: impl Fn(&Declaration) -> bool) -> Vec<Declaration> {
    let mut paired: Vec<(Declaration, std::ops::Range<usize>)> = declarations.into_iter().zip(ranges).collect();
    paired.sort_by(|(_, at), (_, bt)| at.start.cmp(&bt.start).then(bt.end.cmp(&at.end)));
    for index in 0..paired.len() {
        let held = paired[index].1.clone();
        paired[index].0.parent = (0..index)
            .rev()
            .find(|&at| container(&paired[at].0) && paired[at].1.start <= held.start && held.end <= paired[at].1.end)
            .map(|at| at as u32);
    }
    paired.into_iter().map(|(declaration, _)| declaration).collect()
}

/// The byte range of 1-based `line` in `source`, without its newline.
fn line_bytes(source: &[u8], line: i64) -> std::ops::Range<usize> {
    let start = source.split_inclusive(|byte| *byte == b'\n').take(line.saturating_sub(1) as usize).map(<[u8]>::len).sum::<usize>();
    let end = source[start..].iter().position(|byte| *byte == b'\n').map_or(source.len(), |at| start + at);
    start..end
}

/// The lines of a file's leading comment its facts keep.
const COMMENT_LINES: usize = 25;

/// A file's leading comment as plain text: its purpose, as the comment above
/// its code states it. A `tree` parsed by the grammar `language` names, as
/// `ccn::lang_for_path` names it, is read node by node, so a shebang,
/// `<?php`, a Rust `#![…]` attribute, a JavaScript directive, a license
/// header, and a magic comment such as `# frozen_string_literal: true` never
/// pass for it, a Python module's docstring is it, and a Rust `///` comment
/// documents the item below it, not the file. A file with no tree is read as
/// text by `leading_text`.
pub fn leading_comment(source: &[u8], tree: Option<(&tree_sitter::Tree, &str)>) -> Option<String> {
    let Some((tree, language)) = tree else {
        return leading_text(source);
    };
    let text = |node: tree_sitter::Node| node.utf8_text(source).unwrap_or_default();
    let purpose = |block: &[tree_sitter::Node]| -> Option<String> {
        let item_doc = language == "rust"
            && block.iter().all(|node| {
                let raw = text(*node);
                (raw.starts_with("///") && !raw.starts_with("////")) || (raw.starts_with("/**") && !raw.starts_with("/**/"))
            });
        let lines: Vec<String> = block.iter().flat_map(|node| uncommented(text(*node))).skip_while(|line| magic(line)).collect();
        let joined = lines.join("\n").to_lowercase();
        let license = ["copyright", "spdx-license-identifier", "licensed under", "all rights reserved"]
            .iter()
            .any(|marker| joined.contains(marker));
        (!item_doc && !license).then(|| kept(&lines)).flatten()
    };
    // A line comment's node takes its newline, so it ends on the next row.
    let last_row = |node: tree_sitter::Node| {
        let end = node.end_position();
        if end.column == 0 && end.row > node.start_position().row { end.row - 1 } else { end.row }
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut block: Vec<tree_sitter::Node> = Vec::new();
    for node in root.named_children(&mut cursor) {
        let after_gap = block.last().is_some_and(|last| node.start_position().row > last_row(*last) + 1);
        if after_gap || !node.kind().ends_with("comment") {
            if let Some(found) = purpose(&block) {
                return Some(found);
            }
            block.clear();
        }
        if node.kind().ends_with("comment") {
            block.push(node);
            continue;
        }
        let string_statement = node.kind() == "expression_statement"
            && node.named_child_count() == 1
            && node.named_child(0).is_some_and(|child| child.kind() == "string");
        match node.kind() {
            "shebang" | "hash_bang_line" | "php_tag" | "text" | "inner_attribute_item" => {}
            _ if string_statement && language == "python" => return docstring(text(node)),
            _ if string_statement => {}
            _ => return None,
        }
    }
    purpose(&block)
}

/// A comment's lines with their comment markers removed.
fn uncommented(comment: &str) -> Vec<String> {
    comment
        .lines()
        .map(|line| {
            let line = line.trim();
            let line = line.strip_suffix("*/").unwrap_or(line);
            let line = ["///", "//!", "//", "/**", "/*!", "/*", "#", "--", "*"]
                .iter()
                .find_map(|marker| line.strip_prefix(marker))
                .unwrap_or(line);
            line.trim().to_string()
        })
        .collect()
}

/// A line that tells a tool how to read the file rather than a reader what
/// it does: an encoding or editor setting, a linter directive, a shebang.
fn magic(line: &str) -> bool {
    line.is_empty()
        || line.contains("-*-")
        || line.starts_with('!')
        || ["frozen_string_literal:", "encoding:", "coding:", "coding=", "@ts-", "eslint", "prettier-ignore", "vim:", "@flow", "jshint", "rubocop:", "noqa", "pylint:", "type: ignore"]
            .iter()
            .any(|marker| line.starts_with(marker))
}

/// A Python docstring statement's text without its quotes.
fn docstring(statement: &str) -> Option<String> {
    let body = statement.trim().trim_start_matches(|c: char| "rRuUbBfF".contains(c));
    let inner = ["\"\"\"", "'''", "\"", "'"]
        .iter()
        .find_map(|quote| body.strip_prefix(quote).and_then(|rest| rest.strip_suffix(quote)))?;
    kept(&inner.lines().map(|line| line.trim().to_string()).collect::<Vec<_>>())
}

/// A comment's lines without the blank ones around them, at most
/// `COMMENT_LINES`; `None` when nothing is left.
fn kept(lines: &[String]) -> Option<String> {
    let start = lines.iter().position(|line| !line.is_empty())?;
    let end = lines.iter().rposition(|line| !line.is_empty())? + 1;
    Some(lines[start..end].iter().take(COMMENT_LINES).cloned().collect::<Vec<_>>().join("\n"))
}

const BLOCK_OPEN: &[&str] = &["/*", "/**"];
const BLOCK_CLOSE: &str = "*/";
const TRIPLE_QUOTES: &[&str] = &["\"\"\"", "'''"];

/// The leading comment block of a file read as text, for a file no grammar
/// parses: a block comment, a triple-quoted docstring, or a run of `#` or
/// `//` lines at the top, after a shebang or `<?php`.
fn leading_text(source: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(source);
    let lines: Vec<String> = text.split_inclusive('\n').take(COMMENT_LINES).map(|s| s.to_string()).collect();
    if lines.is_empty() {
        return None;
    }

    let mut i = 0;
    while i < lines.len() && (lines[i].trim().is_empty() || lines[i].starts_with("#!")) {
        i += 1;
    }
    if i < lines.len() && lines[i].trim_start().starts_with("<?") {
        i += 1;
    }
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    if i >= lines.len() {
        return None;
    }

    let head = lines[i].trim().to_string();

    // Block comment.
    if BLOCK_OPEN.iter().any(|t| head.starts_with(t)) {
        let mut body: Vec<String> = Vec::new();
        for line in &lines[i..] {
            let stripped = line.trim();
            let mut cleaned = stripped.to_string();
            for token in ["/**", "/*"] {
                if cleaned.starts_with(token) {
                    cleaned = cleaned[token.len()..].trim_start().to_string();
                }
            }
            if cleaned.ends_with(BLOCK_CLOSE) {
                cleaned = cleaned[..cleaned.len() - BLOCK_CLOSE.len()].trim_end().to_string();
                if cleaned.starts_with('*') {
                    cleaned = cleaned[1..].trim_start().to_string();
                }
                if !cleaned.is_empty() {
                    body.push(cleaned);
                }
                break;
            }
            if cleaned.starts_with('*') {
                cleaned = cleaned[1..].trim_start().to_string();
            }
            body.push(cleaned);
        }
        let joined: Vec<String> = body.into_iter().filter(|l| !l.is_empty()).collect();
        return if joined.is_empty() { None } else { Some(joined.join("\n")) };
    }

    // Triple-quoted docstring.
    for quote in TRIPLE_QUOTES {
        if let Some(first_after) = head.strip_prefix(quote) {
            let mut body: Vec<String> = Vec::new();
            if let Some(idx) = first_after.find(quote) {
                let single = first_after[..idx].trim();
                return if single.is_empty() { None } else { Some(single.to_string()) };
            }
            if !first_after.is_empty() {
                body.push(first_after.to_string());
            }
            for line in &lines[i + 1..] {
                let stripped = line.trim_end();
                if let Some(idx) = stripped.find(quote) {
                    body.push(stripped[..idx].trim_end().to_string());
                    break;
                }
                body.push(stripped.to_string());
            }
            let joined = body.join("\n");
            return if joined.is_empty() { None } else { Some(joined) };
        }
    }

    // Run of // or # line comments.
    let line_tokens = ["# ", "#", "// ", "//", "*", "* "];
    if line_tokens.iter().any(|t| head.starts_with(t)) {
        let mut body: Vec<String> = Vec::new();
        for line in &lines[i..] {
            let stripped = line.trim_end();
            if stripped.is_empty() {
                break;
            }
            let ls = stripped.trim_start();
            let Some(rest) = line_tokens.iter().find_map(|token| ls.strip_prefix(token)) else {
                break;
            };
            body.push(rest.trim().to_string());
        }
        let joined: Vec<String> = body.into_iter().filter(|l| !l.is_empty()).collect();
        return if joined.is_empty() { None } else { Some(joined.join("\n")) };
    }

    None
}

/// The syntactic shape of a call/use site. `Free` is a bare call
/// (`foo()`), `Member` is a call on a receiver (`$x->foo()`, `obj.foo()`),
/// `Static` names the class at the site (`Foo::bar()`, `new Foo`,
/// `Foo::class`, a type hint). Resolution narrows candidates by shape: a
/// `Free` call resolves only to free functions, a `Member` call only to
/// methods, a `Static` use to the named class (and its exact member).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefShape {
    Free,
    Member,
    Static,
}

/// A reference — an identifier use site (a call or qualified-name access).
/// Resolved into edges at graph-build time. `shape` is the call form;
/// `receiver` is the class named at the site for a `Static` use (e.g. `Foo`
/// in `Foo::bar()` / `new Foo`), in Rust the whole path written before the
/// name (`crate::summary` in `crate::summary::front_matter()`), for a
/// `Member` call the receiver's type where PHP or Rust states it, else the
/// receiver's text in TypeScript, Python, Java, and Ruby, and `None`
/// otherwise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub line: i64,
    pub shape: RefShape,
    pub receiver: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub language: String,
    pub imports: Vec<Import>,
    pub declarations: Vec<Declaration>,
    pub references: Vec<Reference>,
}

impl ExtractionResult {
    /// The file's exported declarations: its top-level rows whose visibility
    /// is public, or that the rule couldn't read.
    pub fn exports(&self) -> impl Iterator<Item = &Declaration> {
        self.declarations
            .iter()
            .filter(|declaration| declaration.parent.is_none() && declaration.visibility >= Visibility::Unknown)
    }
}

#[derive(Default, Deserialize, Serialize)]
struct CtagsMap {
    binary: Option<CtagsBinary>,
    extensions: HashMap<String, String>,
    /// Why `ctags --list-maps` failed, stored for this build of `ctags`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

/// One build of `ctags`: the file on `PATH`, its size, and when it changed.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CtagsBinary {
    path: String,
    size: u64,
    modified_nanos: u128,
}

/// The `ctags` build a file's declarations come from: the one on `PATH`,
/// `None` when there is none, for a file whose extension no tree-sitter
/// extractor reads, and `None` for every other file.
pub fn ctags_build(path: &str) -> Option<CtagsBinary> {
    reads_through_ctags(path).then(|| ctags_binary().cloned()).flatten()
}

/// Whether facts extracted under `stored`, the `ctags_build` of their time,
/// still hold: a file `ctags` reads holds only while that build is on
/// `PATH`, so installing or replacing `ctags` extracts it again.
pub fn ctags_build_holds(path: &str, stored: Option<&CtagsBinary>) -> bool {
    !reads_through_ctags(path) || stored == ctags_binary()
}

fn reads_through_ctags(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| !supported_extensions().contains(&extension.to_lowercase().as_str()))
}

/// Extensions with a tree-sitter extractor (lowercase, no leading dot).
pub fn supported_extensions() -> &'static [&'static str] {
    &[
        "py", "ts", "tsx", "js", "jsx", "php", "rs", "go", "rb", "java", "c", "h",
    ]
}

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_lowercase();
            supported_extensions().contains(&extension.as_str())
                || ctags_languages(path).contains_key(&extension)
        })
}

/// What extracting one file's declarations produced.
pub enum Extraction {
    /// No extractor knows the file's language.
    Unsupported,
    /// `ctags` exited with an error or ran past `CTAGS_LIMIT`. The failure is
    /// already reported; it is stored with the file's facts, so the same
    /// content is never retried and every later answer names it again.
    Failed(Failure),
    Extracted(ExtractionResult),
}

/// Why `ctags` gave a file no declarations. Stored with the file's
/// `ctags_build`, it holds only while that build is the one on `PATH`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub reason: String,
}

impl Extraction {
    /// The declarations, when there are any to read.
    pub fn extracted(self) -> Option<ExtractionResult> {
        match self {
            Extraction::Extracted(result) => Some(result),
            Extraction::Unsupported | Extraction::Failed(_) => None,
        }
    }
}

/// Dispatch to the per-language extractor.
pub fn extract(source: &[u8], path: &str) -> Extraction {
    let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()).map(str::to_lowercase) else {
        return Extraction::Unsupported;
    };
    Extraction::Extracted(match ext.as_str() {
        "py" => python::extract(source, path),
        "ts" | "js" | "tsx" | "jsx" => typescript::extract(source, path),
        "php" => php::extract(source),
        "rs" => rust::extract(source),
        "go" => go::extract(source),
        "rb" => ruby::extract(source),
        "java" => java::extract(source),
        "c" | "h" => c::extract(source, path),
        _ => return ctags_extract(source, path, &ext),
    })
}

/// Names a `ctags` failure, with the file it failed on when there is one.
/// One line per run: a broken `ctags` fails every file in its languages, and
/// the first names the cause.
pub fn report_failure(path: Option<&str>, reason: &str) {
    static REPORTED: std::sync::Once = std::sync::Once::new();
    REPORTED.call_once(|| match path {
        Some(path) => crate::process::report(&format!("ctags failed on {path}: {reason}")),
        None => crate::process::report(&format!("ctags failed: {reason}")),
    });
}

/// How long one file's `ctags` run may take. Generous, so a machine busy
/// with parallel agents never reaches it; a run that does is stuck.
const CTAGS_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The `ctags` language map for `path`'s repository: empty when there is no
/// `ctags`, and carrying the failure when `ctags --list-maps` failed. A
/// failure is reported the first time the map is read in a run.
fn ctags_map(path: &Path) -> &'static CtagsMap {
    static MAP: OnceLock<CtagsMap> = OnceLock::new();
    static EMPTY: OnceLock<CtagsMap> = OnceLock::new();
    if let Some(map) = MAP.get() {
        return map;
    }
    // A file outside any repository has no cache to read the map from, and
    // never decides the map for the run's other files.
    let (Some(root), Some(binary)) = (crate::cache::worktree_root_for(path), ctags_binary()) else {
        return EMPTY.get_or_init(CtagsMap::default);
    };
    MAP.get_or_init(|| {
        let map = crate::cache::index(&CtagsIndex { binary: binary.clone() }, &root).ready().unwrap_or_default();
        if let Some(failure) = &map.failure {
            report_failure(None, failure);
        }
        map
    })
}

fn ctags_languages(path: &Path) -> &'static HashMap<String, String> {
    &ctags_map(path).extensions
}

struct CtagsIndex {
    binary: CtagsBinary,
}

impl crate::cache::Index for CtagsIndex {
    type Stored = CtagsMap;
    type Change = ();

    fn key(&self) -> String {
        "ctags_maps_v1".to_string()
    }

    fn read(&self, bytes: &[u8]) -> Option<CtagsMap> {
        serde_json::from_slice(bytes).ok()
    }

    fn change(&self, stored: Option<&CtagsMap>) -> Option<()> {
        (stored.and_then(|map| map.binary.as_ref()) != Some(&self.binary)).then_some(())
    }

    fn apply(&self, _stored: Option<CtagsMap>, _change: ()) -> Option<CtagsMap> {
        let (extensions, failure) = match list_ctags_maps() {
            Ok(extensions) => (extensions, None),
            Err(failure) => (HashMap::new(), Some(failure)),
        };
        Some(CtagsMap { binary: Some(self.binary.clone()), extensions, failure })
    }
}

/// The `ctags` on `PATH`, or `None` when there is none: its languages are
/// then unsupported, and `trace doctor` names the install command.
fn ctags_binary() -> Option<&'static CtagsBinary> {
    static BINARY: OnceLock<Option<CtagsBinary>> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let path = std::env::var_os("PATH")
                .into_iter()
                .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
                .map(|directory| directory.join("ctags"))
                .find(|candidate| candidate.is_file())
                .and_then(|candidate| candidate.canonicalize().ok())?;
            let metadata = fs::metadata(&path).ok()?;
            let modified_nanos = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
            Some(CtagsBinary {
                path: path.to_string_lossy().into_owned(),
                size: metadata.len(),
                modified_nanos,
            })
        })
        .as_ref()
}

/// Every extension `ctags` maps to a language, or why `ctags` couldn't
/// list them.
fn list_ctags_maps() -> Result<HashMap<String, String>, String> {
    let maps = crate::timing::phase("ctags list maps", || {
        crate::process::output(Command::new("ctags").arg("--list-maps"))
    })
    .map_err(|error| error.to_string())?;
    if !maps.status.success() {
        return Err(String::from_utf8_lossy(&maps.stderr).trim().to_string());
    }
    let mut extensions = HashMap::new();
    for line in String::from_utf8_lossy(&maps.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some(language) = fields.next() else {
            continue;
        };
        if ctags_denied_languages
            .iter()
            .any(|denied_language| language.eq_ignore_ascii_case(denied_language))
        {
            continue;
        }
        for pattern in fields {
            let Some(extension) = pattern.strip_prefix("*.") else {
                continue;
            };
            if !extension.contains(['*', '[', ']']) {
                extensions
                    .entry(extension.to_lowercase())
                    .or_insert_with(|| language.to_string());
            }
        }
    }
    Ok(extensions)
}

/// `ctags`'s declarations for a language it maps. While `ctags --list-maps`
/// fails, no language is known, so every file a working `ctags` might
/// extract fails with it, and extracts again once `ctags` is replaced.
fn ctags_extract(source: &[u8], path: &str, extension: &str) -> Extraction {
    let map = ctags_map(Path::new(path));
    if let Some(failure) = &map.failure {
        return Extraction::Failed(Failure { reason: failure.clone() });
    }
    let (Some(language), Some(root)) = (map.extensions.get(extension), crate::cache::worktree_root_for(Path::new(path)))
    else {
        return Extraction::Unsupported;
    };
    let tags = run_ctags(source, extension, language, &root)
        .and_then(|tags| ctags_declarations(source, language, &tags).ok_or_else(|| "unreadable output".to_string()));
    match tags {
        Ok(result) => Extraction::Extracted(result),
        Err(reason) => {
            report_failure(Some(&crate::cache::relative_to_root(Path::new(path), &root)), &reason);
            Extraction::Failed(Failure { reason })
        }
    }
}

/// `ctags`'s JSON tags for `source`, or why it gave none.
fn run_ctags(source: &[u8], extension: &str, language: &str, root: &Path) -> Result<String, String> {
    let temporary_dir = root.join(crate::cache::CACHE_DIR_NAME).join(crate::cache::TEMPORARY_DIR_NAME);
    std::fs::create_dir_all(&temporary_dir).map_err(|error| error.to_string())?;
    let mut source_file = tempfile::Builder::new()
        .suffix(&format!(".{extension}"))
        .tempfile_in(temporary_dir)
        .map_err(|error| error.to_string())?;
    source_file.write_all(source).map_err(|error| error.to_string())?;
    let output = crate::timing::phase("ctags", || {
        crate::process::output_within(
            Command::new("ctags")
                .args([
                    "--output-format=json",
                    &format!("--language-force={language}"),
                    "--fields=+nezKSta",
                    "--sort=no",
                    "-f",
                    "-",
                ])
                .arg(source_file.path()),
            CTAGS_LIMIT,
        )
    })
    .map_err(|error| error.to_string())?
    .ok_or_else(|| format!("ran past {}s", CTAGS_LIMIT.as_secs()))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The declarations in `ctags`'s JSON tags, `None` when a tag is malformed.
fn ctags_declarations(source: &[u8], language: &str, tags: &str) -> Option<ExtractionResult> {
    let mut declarations = Vec::new();
    let mut missing_ends = HashSet::new();
    for tag in tags.lines() {
        let tag: serde_json::Value = serde_json::from_str(tag).ok()?;
        let raw_kind = tag.get("kind")?.as_str()?;
        if ctags_denied_kinds.contains(&raw_kind) {
            continue;
        }
        let name = tag.get("name")?.as_str()?.to_string();
        let line = tag.get("line")?.as_i64()?;
        let scope = tag
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let end = tag.get("end").and_then(serde_json::Value::as_i64);
        if end.is_none() {
            missing_ends.insert((name.clone(), line));
        }
        let header = source_lines(source, line, line).split('{').next().unwrap_or("").trim().to_string();
        let visibility = match tag.get("access").and_then(serde_json::Value::as_str) {
            Some("public") => Visibility::Public,
            Some("private") => Visibility::FileLocal,
            Some(_) => Visibility::Restricted,
            None if modifiers(&header, 0, &name).contains(&"private") => Visibility::FileLocal,
            None => Visibility::Unknown,
        };
        declarations.push(Declaration {
            name,
            kind: ctags_kind(raw_kind),
            header_line: line,
            line,
            end_line: end.unwrap_or(line),
            container: scope,
            parent: None,
            header,
            annotations: Vec::new(),
            self_type: None,
            module_file: None,
            supertypes: Vec::new(),
            visibility,
        });
    }
    declarations.sort_by_key(|declaration| declaration.line);
    let last_line = (source.iter().filter(|byte| **byte == b'\n').count() + 1) as i64;
    for index in 0..declarations.len() {
        if !missing_ends.contains(&(declarations[index].name.clone(), declarations[index].line)) {
            continue;
        }
        let depth = declarations[index]
            .container
            .as_deref()
            .map(|scope| (scope.matches("::").count() + scope.matches("\"\"").count()) as i64 + 1)
            .unwrap_or(1);
        declarations[index].end_line = declarations
            .iter()
            .skip(index + 1)
            .find(|next| {
                next.container
                    .as_deref()
                    .map(|scope| {
                        (scope.matches("::").count() + scope.matches("\"\"").count()) as i64 + 1
                    })
                    .unwrap_or(1)
                    <= depth
            })
            .map(|next| next.line - 1)
            .unwrap_or(last_line);
    }
    let mut parents = HashMap::new();
    for (index, declaration) in declarations.iter().enumerate() {
        parents.insert(declaration.name.clone(), index as u32);
        if let Some(scope) = declaration.container.as_deref() {
            parents.insert(format!("{scope}::{}", declaration.name), index as u32);
            parents.insert(format!("{scope}\"\"{}", declaration.name), index as u32);
        }
    }
    for declaration in &mut declarations {
        declaration.parent = declaration
            .container
            .as_ref()
            .and_then(|scope| parents.get(scope).copied());
    }
    Some(ExtractionResult {
        language: language.to_lowercase(),
        imports: Vec::new(),
        declarations,
        references: Vec::new(),
    })
}

fn ctags_kind(raw: &str) -> String {
    match raw {
        "c" => "class",
        "f" => "function",
        "m" => "method",
        "v" => "variable",
        "p" => "property",
        "F" => "field",
        "i" | "I" => "import",
        "n" => "namespace",
        "s" => "struct",
        "e" | "g" => "enum",
        "t" => "trait",
        "u" => "union",
        other => other,
    }
    .to_string()
}
