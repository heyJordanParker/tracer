//! The two inversions, and the on-demand reference resolver.
//!
//! Per-file entries (`file_facts::FileFacts::extraction`) already hold every
//! declaration, import and reference, content-addressed and invalidated per
//! file. They are the source of truth. Anything answerable from the file in
//! hand therefore needs no stored structure: `file_to_module` is a pure
//! function of a path, so which file a module path names is derivable from
//! the file list alone, and a file's own imports and references are in its
//! own entry.
//!
//! What cannot be derived is the inversion — who references this name, and
//! who imports this file — because computing it means reading every file.
//! Exactly two relations invert, so exactly two maps are stored and nothing
//! else:
//!
//!   symbols    name -> { defined_in: [file], used_in: [file] }
//!   importers  file -> [importing file]
//!
//! Neither carries payload. Line, kind, container and language stay in the
//! per-file entry, because the files holding them are exactly the files an
//! answer already loads.
//!
//! This replaces a materialized graph whose reference edges were 98% of its
//! mass (733,105 of 747,199 on laravel-framework, 261,747 of 262,425 on
//! WordPress) and which cost 194 MB and 0.90 s to decode for a question whose
//! answer was 382 rows drawn from 82 files. Cost is now proportional to the
//! answer instead of to the repository.
//!
//! Storage follows `file_facts`'s `mtime_index_v2__` precedent exactly: one
//! mutable index in the `file/` namespace over immutable content-addressed
//! entries. Each row records the file it came from, so a changed file's symbol
//! rows are filtered out and re-added from its fresh extraction. The index
//! also keeps each file's import rows, so when a file or declaration change
//! can alter resolution the importer inversion is re-derived in memory from
//! those rows: an added file used to force every entry to be read back, which
//! eight concurrent calls repeated eight times. Nothing is re-extracted, and
//! there is no global fingerprint.

mod index;
mod lineage;
mod manifests;
mod modules;
mod reach;
mod resolve;
mod types;

use crate::extraction::{self, Declaration, RefShape};
use crate::{cache, memo};
use index::RelationsIndex;
use manifests::{CargoManifests, Roots};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub use reach::{ranked_by_reach, reachable, Reach, Reached};
pub use resolve::{candidates, declarations, modules_named, use_sites, Candidates, UseSite};

pub const CONFIDENCE_EXTRACTED: &str = "EXTRACTED";
pub const CONFIDENCE_INFERRED: &str = "INFERRED";
pub const CONFIDENCE_AMBIGUOUS: &str = "AMBIGUOUS";

/// An inline `mod` a file declares: its name, and its header and end lines.
type InlineModule = (String, (i64, i64));

/// A Rust struct, enum, union, or trait a file declares: its name, and the
/// inline `mod` that holds it.
type DeclaredType = (String, Option<String>);

/// What the index keeps of one file's modules: its import rows, the inline
/// `mod`s it declares, and the Rust types it declares.
type ModuleRows = (Vec<extraction::Import>, Vec<InlineModule>, Vec<DeclaredType>);

/// A `mod` item a Rust file declares with `#[path]`: its name and the file
/// the attribute names.
type ModuleFile = (String, String);

/// A type a PHP file declares, with each type it inherits from by its last
/// segment, nearest first, and whether a `use` of the file binds that name.
type Inherits = (String, Vec<(String, bool)>);

/// One [`Built`] as the edges entry stores it, at its file's position in the
/// file table: its key, language, annotation counts, `#[path]` modules, and
/// the PHP types it declares with what each inherits from.
type StoredBuilt = (String, Option<String>, Vec<(u32, u32)>, Vec<ModuleFile>, Vec<Inherits>);

/// [`StoredBuilt`] borrowed for a save.
type BuiltEntry<'a> = (&'a str, Option<&'a str>, &'a [(u32, u32)], &'a [ModuleFile], &'a [Inherits]);

/// The stored index: the two inversions plus the provenance that makes an
/// incremental update possible.
///
/// `built_from` is `relative path -> content-hash key`, the exact set the
/// maps were computed from. Comparing it against the current per-file hashes
/// names the files whose rows are stale, which is the whole update: drop
/// those paths from every list, then add what their fresh extraction says.
#[derive(Debug, Default)]
pub struct Relations {
    files: Vec<Arc<str>>,
    names: Vec<String>,
    file_table_hash: String,
    symbols: OnceLock<HashMap<String, (Vec<u32>, Vec<u32>)>>,
    importers: HashMap<Arc<str>, Vec<Importer>>,
    /// Each file's import rows as extracted, the source the importer
    /// inversion is derived from, beside the inline `mod`s and the Rust types
    /// it declares. Stored in their own entry and loaded only for an update
    /// or a Rust path that reaches a module, the way the symbol map is loaded
    /// only for a symbol query. Files with none of them are absent.
    imports: OnceLock<HashMap<Arc<str>, ModuleRows>>,
    built_from: BTreeMap<Arc<str>, Built>,
    roots: Roots,
    cargo_manifests: CargoManifests,
    repo_root: PathBuf,
}

/// What the index knows about a file without opening it: the content key its
/// rows were absorbed from, and its language. The language is stored because
/// every module path is spelled from it, and reading it back out of 3,030
/// per-file entries cost 0.12s on laravel-framework — on `callers`, `usages`,
/// `dependencies`, and the primer alike. `module_files` are the `mod` items
/// of a Rust file whose `#[path]` names their file, each with that file,
/// because every Rust path reads its file's module from them. `inherits` are
/// the PHP types the file declares with what each inherits from, because a
/// PHP call on a class reads the methods of its whole lineage.
#[derive(Debug, Clone)]
struct Built {
    key: String,
    language: Option<String>,
    annotations: Vec<(u32, u32)>,
    module_files: Vec<ModuleFile>,
    inherits: Vec<Inherits>,
}

/// One directory's place in the import graph, counted over its direct files.
/// An `AMBIGUOUS` edge and an edge between two files of the same directory
/// count for neither side.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct DirectoryMetrics {
    pub files: u32,
    /// Distinct files outside the directory that import one of its files.
    pub imported_by: u32,
    /// Distinct files outside the directory that its files import.
    pub imports: u32,
    /// The directories those imported files live in.
    pub imported_directories: BTreeSet<String>,
    pub annotations: BTreeMap<String, usize>,
}

/// The graph around one file, as its facts show it.
pub struct ModuleCounts {
    /// Files that import this one.
    pub imported_by: usize,
    /// Files this one imports.
    pub imports: u32,
}

#[derive(Default, Deserialize, Serialize)]
pub struct DirectoryIndex {
    pub directories: BTreeMap<String, DirectoryMetrics>,
    /// Files each file imports, keyed by path; a file importing nothing is absent.
    pub imports_per_file: BTreeMap<String, u32>,
}

fn normalized_path(mut base: PathBuf, path: &str) -> Option<PathBuf> {
    for segment in Path::new(path).components() {
        match segment {
            std::path::Component::Normal(segment) => base.push(segment),
            std::path::Component::ParentDir if !base.pop() => return None,
            _ => {}
        }
    }
    Some(base)
}

/// One file that imports another, and how surely the import resolved.
///
/// Confidence is the one thing here that is not derivable from the importing
/// file alone: an import whose module path does not resolve but whose symbol
/// does is INFERRED, and deciding that needs the whole symbol map. So it is
/// an inversion fact and it is stored, unlike kind, line and language, which
/// the importing file already carries.
#[derive(Debug, Clone)]
pub struct Importer {
    pub file: Arc<str>,
    pub confidence: &'static str,
    /// The symbol the import named, when it named one. `from util import
    /// helper` depends on `helper`, not merely on `util`, and a row that
    /// says the module loses which part of it is actually depended on.
    /// It is an edge fact, so it rides on the edge.
    pub symbol: Option<String>,
}

impl Relations {
    /// Files declaring `name`, matched case-insensitively the way the
    /// extractors index it.
    pub fn defined_in(&self, name: &str) -> impl Iterator<Item = &str> {
        self.symbols
            .get_or_init(|| self.load_symbols())
            .get(&name.to_lowercase())
            .into_iter()
            .flat_map(|(defined_in, _)| defined_in)
            .filter_map(|index| self.files.get(*index as usize))
            .map(|path| &**path)
    }

    /// Files with a use site naming `name`. A candidate set for the
    /// resolver, not an answer: a use site is a match by name only until
    /// `shape_matches` has seen it.
    pub fn used_in(&self, name: &str) -> impl Iterator<Item = &str> {
        self.symbols
            .get_or_init(|| self.load_symbols())
            .get(&name.to_lowercase())
            .into_iter()
            .flat_map(|(_, used_in)| used_in)
            .filter_map(|index| self.files.get(*index as usize))
            .map(|path| &**path)
    }

    /// Files whose imports resolve to `file` — the import inversion.
    pub fn importers_of(&self, file: &str) -> &[Importer] {
        self.importers
            .get(file)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// The importers of `file` whose import resolved to it: an `AMBIGUOUS`
    /// row names several candidate files, so it is evidence for none of them.
    /// Every count of a file's importers reads this, the way the directory
    /// counts skip the same rows.
    pub fn resolved_importers_of(&self, file: &str) -> impl Iterator<Item = &Importer> {
        self.importers_of(file)
            .iter()
            .filter(|importer| importer.confidence != CONFIDENCE_AMBIGUOUS)
    }

    /// Every file that imports something, with what it imports. The ranking
    /// queries — the primer Spine, `usages --path`, `dependencies --path` —
    /// walk the whole import graph, and it is 14,094 edges on
    /// laravel-framework and 678 on WordPress, so they walk it directly.
    pub fn import_edges(&self) -> impl Iterator<Item = (&str, &Importer)> {
        self.importers
            .iter()
            .flat_map(|(target, list)| list.iter().map(move |i| (&**target, i)))
    }

    pub fn directory_metrics(&self) -> Arc<DirectoryIndex> {
        memo::get_or_build(&DIRECTORY_METRICS_MEMO, &self.repo_root, || {
            self.load_or_build_directory_metrics()
        })
    }

    pub fn directory_metrics_for(&self, directory: &str) -> Option<DirectoryMetrics> {
        self.directory_metrics().directories.get(directory).cloned()
    }

    /// The graph around a file that its facts show. One owner — every command
    /// that prints a file's facts reads it here.
    pub fn module_counts(&self, file: &str) -> Option<ModuleCounts> {
        // Only the tree-sitter extractors read imports: a Markdown file's
        // "imported by 0" would read as a fact about code.
        self.built_from.get(file)?;
        let extension = Path::new(file).extension()?.to_str()?.to_lowercase();
        if !extraction::supported_extensions().contains(&extension.as_str()) {
            return None;
        }
        Some(ModuleCounts {
            imported_by: self.resolved_importers_of(file).count(),
            imports: self
                .directory_metrics()
                .imports_per_file
                .get(file)
                .copied()
                .unwrap_or(0),
        })
    }

    /// Every file the index covers, in sorted order. The module-path
    /// resolution both the index build and the resolver use is a pure
    /// function of this list.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.built_from.keys().map(|p| &**p)
    }

    /// The indexed language for one file.
    pub fn language(&self, path: &str) -> Option<&str> {
        self.built_from
            .get(path)
            .and_then(|built| built.language.as_deref())
    }

    /// How many distinct names the index carries.
    pub fn name_count(&self) -> usize {
        self.symbols.get_or_init(|| self.load_symbols()).len()
    }

    /// Whether the index knows this name at all — the search signpost's
    /// whole question, answered without loading a file.
    pub fn knows(&self, name: &str) -> bool {
        self.symbols
            .get_or_init(|| self.load_symbols())
            .contains_key(&name.to_lowercase())
    }

    /// The import rows `file` was extracted with.
    fn imports_of(&self, file: &str) -> &[extraction::Import] {
        self.imports
            .get_or_init(|| self.load_imports())
            .get(file)
            .map_or(&[], |(imports, _, _)| imports.as_slice())
    }

    /// The inline `mod`s `file` declares.
    fn inline_modules_of(&self, file: &str) -> &[InlineModule] {
        self.imports
            .get_or_init(|| self.load_imports())
            .get(file)
            .map_or(&[], |(_, inline_modules, _)| inline_modules.as_slice())
    }

    /// The Rust types `file` declares.
    fn types_of(&self, file: &str) -> &[DeclaredType] {
        self.imports
            .get_or_init(|| self.load_imports())
            .get(file)
            .map_or(&[], |(_, _, types)| types.as_slice())
    }

}

/// The three confidence classes, stored as their ordinal. Written out in full
/// they were the second-largest thing in the index after the paths.
pub(crate) fn confidence_code(confidence: &str) -> u64 {
    match confidence {
        CONFIDENCE_EXTRACTED => 0,
        CONFIDENCE_INFERRED => 1,
        _ => 2,
    }
}

fn confidence_name(code: u64) -> &'static str {
    match code {
        0 => CONFIDENCE_EXTRACTED,
        1 => CONFIDENCE_INFERRED,
        _ => CONFIDENCE_AMBIGUOUS,
    }
}

/// The weaker of two confidences.
fn weaker(left: &'static str, right: &'static str) -> &'static str {
    if confidence_code(left) >= confidence_code(right) {
        left
    } else {
        right
    }
}

/// The addressable id for one declaration, as every command has always
/// emitted it. It is formatted from the file and the name at render time
/// rather than stored: both are already in hand wherever a row is built, so
/// keeping it as a field would be a third copy of two facts.
pub fn symbol_id(file: &str, name: &str) -> String {
    format!("{file}::{name}")
}

/// The addressable id for a file's module, on the same terms.
pub fn module_id(file: &str, language: Option<&str>) -> String {
    format!("module::{}", file_to_module(file, language))
}

/// Module path for a file: extension stripped, separators turned into `.`
/// for Python and `/` for every other language.
pub fn file_to_module(relative_path: &str, language: Option<&str>) -> String {
    let path = Path::new(relative_path);
    let stem = match path.extension() {
        Some(_) => path.with_extension("").to_string_lossy().to_string(),
        None => relative_path.to_string(),
    };
    if language == Some("python") {
        stem.replace(std::path::MAIN_SEPARATOR, ".")
    } else {
        stem.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

/// One declaration a name could resolve to, with the language of the file
/// that declares it and the inline `mod` that holds it. Language is a
/// per-file fact, so it sits beside the declaration rather than being copied
/// into every one.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub file: String,
    pub language: Option<String>,
    pub inline_module: Option<String>,
    pub is_file_local: bool,
    pub declaration: Declaration,
}

/// Does a candidate declaration structurally match a use site of this shape,
/// in this language, naming this receiver?
fn shape_matches(
    candidate: &Candidate,
    referrer_language: Option<&str>,
    shape: RefShape,
    receiver: Option<&str>,
) -> bool {
    // Same-language only — the rule that removes every cross-language edge.
    if candidate.language.as_deref() != referrer_language {
        return false;
    }
    let declaration = &candidate.declaration;
    if declaration.kind == "property" {
        return false;
    }
    let is_method = declaration.container.is_some();
    let is_type = matches!(
        declaration.kind.as_str(),
        "class" | "interface" | "trait" | "enum" | "struct"
    );
    let receiver = receiver.map(|receiver| receiver.rsplit(['\\', '.', ':']).next().unwrap_or(receiver));
    let contained_by = |named: &str| {
        declaration
            .container
            .as_deref()
            .is_some_and(|container| same_name(referrer_language, container, named))
    };
    match shape {
        // A free call resolves to a non-method declaration. A type is a valid
        // target only where calling the class constructs an instance (Python,
        // Ruby); elsewhere construction is a `new` expression, already
        // classified Static.
        RefShape::Free => !is_method && (!is_type || constructs_by_call(referrer_language)),
        // A static use names the class at the site, so it resolves exactly:
        // the named type itself, or the method of that name whose container
        // is the named type.
        RefShape::Static => match receiver {
            Some(named) => (is_type && same_name(referrer_language, &declaration.name, named)) || contained_by(named),
            None => is_type || is_method,
        },
        RefShape::Member => match receiver.filter(|_| types_member_receiver(referrer_language)) {
            Some(named) => is_method && contained_by(named),
            None => is_method,
        },
    }
}

/// Whether two names are one name in `language`: PHP ignores the case of a
/// function, class, or method name, and every other language compares it
/// exactly.
fn same_name(language: Option<&str>, left: &str, right: &str) -> bool {
    match language {
        Some("php") => left.eq_ignore_ascii_case(right),
        _ => left == right,
    }
}

/// Languages where calling a class constructs an instance, so a free call is
/// a valid reference to the class.
fn constructs_by_call(language: Option<&str>) -> bool {
    matches!(language, Some("python") | Some("ruby"))
}

fn types_member_receiver(language: Option<&str>) -> bool {
    matches!(language, Some("php") | Some("rust"))
}

static MEMO: memo::Memo<Relations> = OnceLock::new();
static DIRECTORY_METRICS_MEMO: memo::Memo<DirectoryIndex> = OnceLock::new();

/// The two inversions for a repository, current as of this call.
///
/// Loads the stored index, compares its `built_from` against the per-file
/// content hashes on disk, and re-absorbs declarations for files that moved.
/// When files or declarations move, imports are re-resolved from the stored
/// import rows so unchanged importers follow the new declarations. A first
/// call on a cold cache absorbs everything, which is the same work the graph
/// build did minus reference resolution.
pub fn get(repo_root: &Path) -> Arc<Relations> {
    built(repo_root, None).expect("an index with no limit is always built")
}

/// The relations for extra context: built until `process::context_limit`,
/// `None` when nothing is ready by then. The parts that need them are left
/// out, and the files extracted so far are kept for the next call. A whole
/// output has no limit, so it gets them built.
pub fn ready(repo_root: &Path) -> Option<Arc<Relations>> {
    match crate::process::context_limit() {
        Some(limit) => within(repo_root, limit),
        None => Some(get(repo_root)),
    }
}

/// The relations built until `limit`, as `ready` builds them.
pub fn within(repo_root: &Path, limit: std::time::Instant) -> Option<Arc<Relations>> {
    if left_out().contains(repo_root) {
        return None;
    }
    let ready = built(repo_root, Some(limit));
    if ready.is_none() {
        left_out().insert(repo_root.to_path_buf());
    }
    ready
}

/// The repositories whose relations this call left out: once one is not
/// ready by the limit, every later part of the call leaves them out too.
fn left_out() -> std::sync::MutexGuard<'static, BTreeSet<PathBuf>> {
    static LEFT_OUT: std::sync::Mutex<BTreeSet<PathBuf>> = std::sync::Mutex::new(BTreeSet::new());
    LEFT_OUT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `IndexState` notice, once per agent context, for each repository
/// this call left relations out of. It names `trace cache build`, which
/// finishes the index for an agent that only ever sees hooks.
pub fn index_state(delivery: &mut crate::session::Delivery) -> String {
    left_out()
        .iter()
        .filter_map(|root| {
            let line = notice(root);
            let offer = delivery.offer(crate::session::Kind::IndexState, &root.to_string_lossy(), &serde_json::json!(line));
            (!matches!(offer, crate::session::Offer::Same)).then_some(line)
        })
        .collect()
}

/// The `IndexState` line for `repo_root`.
pub fn notice(repo_root: &Path) -> String {
    format!(
        "index: still building, so imported_by, imports, and calls are left out. `trace -C {} cache build` finishes it.\n",
        crate::shell::quote(&repo_root.to_string_lossy()),
    )
}

/// Only an index brought up to date is memoized, so a stale one never
/// stands in for `get`'s answer later in the same call.
fn built(repo_root: &Path, limit: Option<std::time::Instant>) -> Option<Arc<Relations>> {
    let mut stale = None;
    let current = memo::get_or_try_build(&MEMO, repo_root, || {
        let index = RelationsIndex {
            repo_root,
            hashes: OnceLock::new(),
            limit,
        };
        match cache::index(&index, repo_root) {
            cache::Outcome::Ready(relations) => Some(relations),
            cache::Outcome::Empty => Some(index.empty()),
            cache::Outcome::Stale(relations) => {
                stale = Some(Arc::new(relations));
                None
            }
            cache::Outcome::NotReady => None,
        }
    });
    current.or(stale)
}
