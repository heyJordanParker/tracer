//! Which file a written module path names, the inline `mod`s a file
//! declares, and the walk a Rust path takes through the modules and `use`
//! items it reaches.

use crate::extraction::{self, Declaration, ExtractionResult};
use crate::relations::manifests::Roots;
use crate::relations::{
    confidence_code, file_to_module, normalized_path, Candidate, Relations, CONFIDENCE_AMBIGUOUS,
    CONFIDENCE_EXTRACTED, CONFIDENCE_INFERRED,
};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

/// Module path -> file, derived from the file list and nothing else.
///
/// `file_to_module` is a pure function of a path and its language, so this
/// map is rebuilt from the paths on every call rather than stored. It is what
/// turns a written import (`Illuminate\Support\Str`, `./helpers`) into the
/// repo-relative file it names.
pub(super) struct ModulePaths<'a> {
    /// Normalized module path, relative file, and language in discovery order.
    entries: Vec<(String, String, Option<String>)>,
    /// Exact module path to positions in `entries`. Multiple positions retain
    /// honest same-language ambiguity without making exact imports scan the
    /// repository.
    exact: HashMap<String, Vec<usize>>,
    /// Final path segment to positions in `entries`. A suffix match ends at
    /// the entry's end, so the segments agree, and the fallback reads this
    /// list instead of every entry: re-resolving 37,000 imports against
    /// 2,600 files cost 880ms per update through the full scan.
    tails: HashMap<String, Vec<usize>>,
    roots: &'a Roots,
    /// Each file a `#[path]` names, with the file whose `mod` item names it
    /// and that item's name.
    declared_by: HashMap<String, (String, String)>,
    /// The same `#[path]` modules keyed the other way, by the declaring
    /// file and the item's name.
    declared_files: HashMap<(String, String), String>,
    /// Each directory that holds a Rust crate root, with the roots it holds.
    crate_directories: HashMap<&'a Path, Vec<String>>,
    crate_members: OnceLock<HashMap<String, Vec<String>>>,
    relations: &'a Relations,
}

/// The final segment of a module path, past the last `.`, `/`, or `::`.
pub(super) fn tail(module: &str) -> &str {
    module.rsplit(['.', '/', ':']).next().unwrap_or(module)
}

/// Of several readings of one Rust path, each the files it reaches and the
/// segments past them, the files of every reading that reaches a file with
/// the fewest segments left, and those segments.
fn deepest(readings: Vec<(Vec<String>, Vec<String>)>) -> Option<(Vec<String>, Vec<String>)> {
    let shortest = readings
        .iter()
        .filter(|(files, _)| !files.is_empty())
        .map(|(_, rest)| rest.len())
        .min()?;
    let (mut files, rest): (Vec<String>, Vec<String>) = readings
        .into_iter()
        .filter(|(files, rest)| !files.is_empty() && rest.len() == shortest)
        .fold((Vec::new(), Vec::new()), |(mut files, _), (reached, rest)| {
            files.extend(reached);
            (files, rest)
        });
    files.sort();
    files.dedup();
    Some((files, rest))
}

impl<'a> ModulePaths<'a> {
    /// The module paths of every file `relations` indexes, read through its
    /// root mappings, its `#[path]` modules, and its stored import rows.
    pub(super) fn new(relations: &'a Relations) -> Self {
        let files = &relations.built_from;
        let mut entries = Vec::with_capacity(files.len());
        let mut exact: HashMap<String, Vec<usize>> = HashMap::with_capacity(files.len());
        let mut tails: HashMap<String, Vec<usize>> = HashMap::with_capacity(files.len());
        let mut declared_by = HashMap::new();
        let mut declared_files = HashMap::new();
        for (path, built) in files {
            let language = built.language.as_deref();
            let mut module = file_to_module(path, language);
            if language == Some("php") {
                module = module.to_lowercase();
            }
            let position = entries.len();
            exact.entry(module.clone()).or_default().push(position);
            tails
                .entry(tail(&module).to_string())
                .or_default()
                .push(position);
            entries.push((module, path.to_string(), built.language.clone()));
            for (name, file) in &built.module_files {
                declared_by.insert(file.clone(), (path.to_string(), name.clone()));
                declared_files.insert((path.to_string(), name.clone()), file.clone());
            }
        }
        let mut crate_directories: HashMap<&Path, Vec<String>> = HashMap::new();
        for root in relations.roots.rust_crates.keys() {
            if let Some(directory) = Path::new(root).parent() {
                crate_directories.entry(directory).or_default().push(root.clone());
            }
        }
        Self {
            entries,
            exact,
            tails,
            roots: &relations.roots,
            declared_by,
            declared_files,
            crate_directories,
            crate_members: OnceLock::new(),
            relations,
        }
    }

    /// The compatible-language files an import may name. Explicit relative
    /// imports are first made repository-relative from the importing file,
    /// and a Rust path tries the importing file's `imports` globs before it
    /// names no file; suffix fallback retains every honest candidate
    /// rather than choosing whichever file happened to be discovered first.
    fn resolve(
        &self,
        module_path: &str,
        importer_path: &str,
        language: Option<&str>,
        imports: &[extraction::Import],
    ) -> Option<Vec<String>> {
        if language == Some("php") && self.roots.has_mapping(language) {
            let mut prefixes: Vec<&String> = self.roots.php.keys().collect();
            prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.len()));
            for prefix in prefixes {
                if let Some(rest) = module_path.strip_prefix(prefix.as_str()) {
                    let base = self.roots.php[prefix].as_str();
                    let wanted = format!("{base}/{}.php", rest.replace('\\', "/"));
                    return Some(self.files_named(&wanted, language));
                }
            }
            return Some(Vec::new());
        }
        if language == Some("typescript") && self.roots.has_mapping(language) {
            if module_path.starts_with("./") || module_path.starts_with("../") {
                return Some(self.typescript_path(module_path, importer_path));
            }
            let mut aliases: Vec<&String> = self.roots.javascript_paths.keys().collect();
            aliases.sort_by_key(|alias| std::cmp::Reverse(alias.len()));
            for alias in aliases {
                let Some(rest) = alias_match(module_path, alias) else {
                    continue;
                };
                let mut resolved = Vec::new();
                for target in &self.roots.javascript_paths[alias] {
                    let target = target.replace('*', rest);
                    resolved.extend(self.typescript_path_from(&target));
                }
                resolved.sort();
                resolved.dedup();
                return Some(resolved);
            }
            if let Some(base_url) = &self.roots.javascript_base_url {
                return Some(self.typescript_path_from(&format!("{base_url}/{module_path}")));
            }
            return Some(Vec::new());
        }
        if language == Some("rust") && self.roots.has_mapping(language) {
            return Some(self.rust_item_path(module_path, importer_path, imports).0);
        }
        let absolute;
        let module_path = match language {
            Some("python") if module_path.starts_with('.') => {
                absolute = python_absolute(module_path, importer_path)?;
                absolute.as_str()
            }
            _ => module_path,
        };
        let mut wanted = module_path.to_string();
        if module_path.starts_with("./") || module_path.starts_with("../") {
            let mut path = Path::new(importer_path)
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf();
            for segment in module_path.split('/') {
                match segment {
                    "" | "." => {}
                    ".." => {
                        if !path.pop() {
                            return None;
                        }
                    }
                    part => path.push(part),
                }
            }
            wanted = file_to_module(&path.to_string_lossy(), language);
        }

        let compared = if language == Some("php") {
            wanted.replace('\\', "/").to_lowercase()
        } else {
            wanted.clone()
        };
        let compatible = |entry_language: &Option<String>| entry_language.as_deref() == language;
        if let Some(positions) = self.exact.get(&compared) {
            let mut exact: Vec<String> = positions
                .iter()
                .filter_map(|position| self.entries.get(*position))
                .filter(|(_, _, entry_language)| compatible(entry_language))
                .map(|(_, file, _)| file.clone())
                .collect();
            if !exact.is_empty() {
                exact.sort();
                exact.dedup();
                return Some(exact);
            }
        }

        let mut suffixes: Vec<String> = self
            .tails
            .get(tail(&compared))
            .into_iter()
            .flatten()
            .filter_map(|position| self.entries.get(*position))
            .filter(|(module, _, entry_language)| {
                if !compatible(entry_language) {
                    return false;
                }
                module.strip_suffix(&compared).is_some_and(|prefix| {
                    prefix.is_empty() || prefix.ends_with('.') || prefix.ends_with('/')
                })
            })
            .map(|(_, file, _)| file.clone())
            .collect();
        suffixes.sort();
        suffixes.dedup();
        Some(suffixes)
    }

    fn files_named(&self, path: &str, language: Option<&str>) -> Vec<String> {
        self.relations
            .built_from
            .get(path)
            .filter(|built| built.language.as_deref() == language)
            .map(|_| vec![path.to_string()])
            .unwrap_or_default()
    }

    fn typescript_path(&self, module_path: &str, importer_path: &str) -> Vec<String> {
        let base = Path::new(importer_path)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        let Some(path) = normalized_path(base, module_path) else {
            return Vec::new();
        };
        self.typescript_path_from(&path.to_string_lossy())
    }

    fn typescript_path_from(&self, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.files_named(path, Some("typescript")));
        for extension in ["ts", "tsx", "js", "jsx"] {
            out.extend(self.files_named(&format!("{path}.{extension}"), Some("typescript")));
        }
        for extension in ["ts", "tsx", "js", "jsx"] {
            out.extend(self.files_named(&format!("{path}/index.{extension}"), Some("typescript")));
        }
        out
    }

    /// The files a Rust path names from `file`, whose `use` items are
    /// `imports`, and the segments of it past the deepest module file it
    /// reaches, so it names a module when none are left. `crate`,
    /// `self`, and `super` start in the file's own crate; another first
    /// segment is a child module of the file, else the library a crate name
    /// of its package reaches, or for a file no package holds a library of
    /// the repository by name, else a module `X::<first>` that a `use X::*`
    /// brings in, else no file at all, which leaves every segment. The path
    /// descends module by module, `a/b.rs` or `a/b/mod.rs`, and names the
    /// deepest module file it reaches, so `crate::summary::Facts` names
    /// `summary.rs` and leaves `Facts`. When the first segment left names no
    /// module file, a `use` at the top level of a file reached that binds it
    /// leads one hop on, the way the walk follows a last segment, so after
    /// `pub use engine::config;` in `lib.rs`, `crate::config::Builder` and
    /// the module path `crate::config` both name `engine/config.rs`.
    pub(super) fn rust_path(&self, path: &str, file: &str, imports: &[extraction::Import]) -> (Vec<String>, Vec<String>) {
        let (files, rest) = self.descend(path, file, imports);
        self.through_use(files, rest)
    }

    /// [`ModulePaths::rust_path`] for a path whose last segment is an item,
    /// a `use` row's or a type path's, which the walk follows itself, so
    /// only a segment with another after it takes the hop.
    pub(super) fn rust_item_path(&self, path: &str, file: &str, imports: &[extraction::Import]) -> (Vec<String>, Vec<String>) {
        let (files, rest) = self.descend(path, file, imports);
        if rest.len() < 2 {
            return (files, rest);
        }
        self.through_use(files, rest)
    }

    /// The files and the segments left past them, one hop on through a
    /// `use` at the top level of a file of `files` that binds the first
    /// segment of `rest`, when the hop leaves fewer segments or reaches
    /// another file, such as a module another crate declares inline.
    fn through_use(&self, files: Vec<String>, rest: Vec<String>) -> (Vec<String>, Vec<String>) {
        let Some(first) = rest.first() else {
            return (files, rest);
        };
        let mut hops = Vec::new();
        for reached in &files {
            for import in self.relations.imports_of(reached) {
                let Some(symbol) = import.symbol.as_deref().filter(|_| import.block.is_none()) else {
                    continue;
                };
                if import.binding() != Some(first.as_str()) {
                    continue;
                }
                let bound: Vec<&str> = [import.module.as_str(), symbol]
                    .into_iter()
                    .chain(rest[1..].iter().map(String::as_str))
                    .filter(|segment| !segment.is_empty())
                    .collect();
                let hop = self.descend(&bound.join("::"), reached, &[]);
                if hop.1.len() < rest.len() || hop.0.iter().any(|target| target != reached) {
                    hops.push(hop);
                }
            }
        }
        deepest(hops).unwrap_or((files, rest))
    }

    /// The files a Rust path names by its module files alone, before any
    /// `use` partway along it, and the segments past the deepest one. A
    /// first segment that is no child module of `file` reads through each
    /// `use` of `imports` that binds it, one hop, before a crate name,
    /// because in Rust a name in scope outranks the extern prelude.
    fn descend(&self, path: &str, file: &str, imports: &[extraction::Import]) -> (Vec<String>, Vec<String>) {
        let segments: Vec<String> = path
            .split("::")
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect();
        let own = self.crate_roots(file);
        let module = self.rust_module(file, &own);
        let (roots, base, rest) = match segments.first().map(String::as_str) {
            None => return (Vec::new(), Vec::new()),
            Some("crate") => (own, Vec::new(), &segments[1..]),
            Some("self") => (own, module, &segments[1..]),
            Some("super") => {
                let supers = segments.iter().take_while(|segment| *segment == "super").count();
                let Some(depth) = module.len().checked_sub(supers) else {
                    return (Vec::new(), segments);
                };
                (own, module[..depth].to_vec(), &segments[supers..])
            }
            Some(first) => {
                if !self.rust_module_files(&own, &[module.as_slice(), &segments[..1]].concat()).is_empty() {
                    (own, module, &segments[..])
                } else {
                    let bound = path_after_binding(path, imports)
                        .iter()
                        .map(|bound| self.descend(bound, file, &[]))
                        .collect();
                    if let Some(found) = deepest(bound) {
                        return found;
                    }
                    let named = self.crates_named(first, file);
                    if named.is_empty() {
                        return imports
                            .iter()
                            .filter(|import| import.symbol.is_none())
                            .find(|glob| {
                                let (files, rest) = self.rust_path(&format!("{}::{first}", glob.module), file, &[]);
                                files.iter().any(|reached| named_module(reached, &rest, self.relations).is_some())
                            })
                            .map_or((Vec::new(), segments), |glob| {
                                self.descend(&format!("{}::{path}", glob.module), file, &[])
                            });
                    }
                    (named, Vec::new(), &segments[1..])
                }
            }
        };
        for taken in (0..=rest.len()).rev() {
            let files = self.rust_module_files(&roots, &[base.as_slice(), &rest[..taken]].concat());
            if !files.is_empty() {
                return (files, rest[taken..].to_vec());
            }
        }
        (Vec::new(), segments)
    }

    /// The crate roots `crate::` names from `file`: the file itself when it
    /// is a root, else the roots whose directory holds it most closely. A
    /// file no root's directory holds, such as a test, an example, or a
    /// build script, is a crate root itself.
    fn crate_roots(&self, file: &str) -> Vec<String> {
        if self.roots.rust_crates.contains_key(file) {
            return vec![file.to_string()];
        }
        Path::new(file)
            .ancestors()
            .skip(1)
            .find_map(|directory| self.crate_directories.get(directory))
            .cloned()
            .unwrap_or_else(|| vec![file.to_string()])
    }

    pub(super) fn crate_files(&self, file: &str) -> Vec<String> {
        let members = self.crate_members.get_or_init(|| {
            let mut members: HashMap<String, Vec<String>> = HashMap::new();
            for (_, member, language) in &self.entries {
                if language.as_deref() == Some("rust") {
                    for root in self.crate_roots(member) {
                        members.entry(root).or_default().push(member.clone());
                    }
                }
            }
            members
        });
        self.crate_roots(file)
            .iter()
            .filter_map(|root| members.get(root))
            .flatten()
            .cloned()
            .collect()
    }

    /// The library roots `name` reaches from `file`: the root its package's
    /// name `name` reaches, and none for a name its package does not give or
    /// that reaches a crate outside the repository, because Cargo gives a
    /// package's code no other crate. For a file no package holds, the
    /// libraries of the repository named `name` nearest `file`.
    fn crates_named(&self, name: &str, file: &str) -> Vec<String> {
        if let Some(names) = self.package_names(file) {
            return names.get(name).cloned().flatten().into_iter().collect();
        }
        let shared = |root: &str| {
            Path::new(root)
                .components()
                .zip(Path::new(file).components())
                .take_while(|(root, file)| root == file)
                .count()
        };
        let named: Vec<&String> = self
            .roots
            .rust_crates
            .iter()
            .filter(|(_, library)| library.as_deref() == Some(name))
            .map(|(root, _)| root)
            .collect();
        let nearest = named.iter().map(|root| shared(root)).max();
        named
            .into_iter()
            .filter(|root| Some(shared(root)) == nearest)
            .cloned()
            .collect()
    }

    /// Whether a Rust module file of the repository is named `name`: a file
    /// whose stem is `name`, or the `mod.rs` of a directory named `name`.
    pub(super) fn has_module_file(&self, name: &str) -> bool {
        let rust = |position: &usize| {
            self.entries
                .get(*position)
                .filter(|(_, _, language)| language.as_deref() == Some("rust"))
        };
        self.tails.get(name).into_iter().flatten().any(|position| rust(position).is_some())
            || self
                .tails
                .get("mod")
                .into_iter()
                .flatten()
                .filter_map(rust)
                .any(|(module, _, _)| module.strip_suffix("/mod").is_some_and(|directory| tail(directory) == name))
    }

    /// Whether a Rust path written in `file` is proven to live outside the
    /// repository: its first segment is a crate of the standard library, or
    /// a name of `file`'s package that reaches no library of the repository.
    pub(super) fn is_external(&self, path: &str, file: &str) -> bool {
        let Some(first) = path.split("::").find(|segment| !segment.is_empty()) else {
            return false;
        };
        matches!(first, "std" | "core" | "alloc" | "proc_macro" | "test")
            || self
                .package_names(file)
                .and_then(|names| names.get(first))
                .is_some_and(Option::is_none)
    }

    /// The crate names of the Cargo package whose directory holds `file`
    /// most closely.
    fn package_names(&self, file: &str) -> Option<&BTreeMap<String, Option<String>>> {
        nearest_package(&self.roots.rust_dependencies, file)
    }

    /// The library roots a Rust path written in `file` may reach when its
    /// first segment is a name `file`'s package leaves out.
    pub(super) fn left_out_crates(&self, path: &str, file: &str) -> Option<&[String]> {
        let first = path.split("::").find(|segment| !segment.is_empty())?;
        nearest_package(&self.roots.rust_left_out, file)?.get(first).map(Vec::as_slice)
    }

    /// The files a Rust path names inside the library whose root is
    /// `library`, its first segment read as that library's `crate`, and the
    /// segments of it past the deepest module file it reaches there.
    pub(super) fn rust_path_within(&self, path: &str, library: &str) -> (Vec<String>, Vec<String>) {
        let rest = path.trim_start_matches("::").split_once("::").map_or("", |(_, rest)| rest);
        self.rust_path(&format!("crate::{rest}"), library, &[])
    }

    /// The files of module `segments` below the directory of `roots`: the
    /// file a `#[path]` names for the last segment in the files of the ones
    /// before it, else the file its path below their directory names.
    fn rust_module_files(&self, roots: &[String], segments: &[String]) -> Vec<String> {
        let Some((last, parent)) = segments.split_last() else {
            return roots.to_vec();
        };
        if !self.declared_files.is_empty() {
            let declared: Vec<String> = self
                .rust_module_files(roots, parent)
                .into_iter()
                .filter_map(|declaring| self.declared_files.get(&(declaring, last.clone())).cloned())
                .collect();
            if !declared.is_empty() {
                return declared;
            }
        }
        let mut files: Vec<String> = roots
            .iter()
            .map(|root| Path::new(root).parent().unwrap_or(Path::new("")).join(segments.join("/")))
            .flat_map(|module| [module.join("mod"), module])
            .filter_map(|module| self.exact.get(module.to_str()?))
            .flatten()
            .filter_map(|position| self.entries.get(*position))
            .filter(|(_, _, language)| language.as_deref() == Some("rust"))
            .map(|(_, file, _)| file.clone())
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// The module path of `file` in the crate whose roots are `roots`. A
    /// file a `#[path]` names is a child of the file that declares it, by
    /// that `mod` item's name. Any other file is its path below their
    /// directory, where `mod.rs` names its own directory.
    fn rust_module(&self, file: &str, roots: &[String]) -> Vec<String> {
        let mut names = Vec::new();
        let mut file = file;
        while let Some((declaring, name)) = self.declared_by.get(file).filter(|_| names.len() < self.declared_by.len()) {
            names.push(name.clone());
            file = declaring;
        }
        let mut module = if roots.iter().any(|root| root == file) {
            Vec::new()
        } else {
            let directory = roots
                .first()
                .and_then(|root| Path::new(root).parent())
                .unwrap_or(Path::new(""));
            let mut module: Vec<String> = Path::new(file)
                .strip_prefix(directory)
                .unwrap_or(Path::new(file))
                .with_extension("")
                .iter()
                .map(|segment| segment.to_string_lossy().to_string())
                .collect();
            if module.last().map(String::as_str) == Some("mod") {
                module.pop();
            }
            module
        };
        module.extend(names.into_iter().rev());
        module
    }
}

/// The entry of the Cargo package whose directory holds `file` most closely.
fn nearest_package<'a, T>(packages: &'a BTreeMap<String, T>, file: &str) -> Option<&'a T> {
    packages
        .iter()
        .filter(|(directory, _)| file.starts_with(directory.as_str()))
        .max_by_key(|(directory, _)| directory.len())
        .map(|(_, entry)| entry)
}

/// The dotted module a Python relative import names from `importer_path`:
/// one leading dot is the importer's package, and each further dot its
/// parent. `None` when the dots climb past the repository root.
fn python_absolute(module_path: &str, importer_path: &str) -> Option<String> {
    let rest = module_path.trim_start_matches('.');
    let mut package = Path::new(importer_path).parent().unwrap_or_else(|| Path::new("")).to_path_buf();
    for _ in 1..module_path.len() - rest.len() {
        if !package.pop() {
            return None;
        }
    }
    let package = file_to_module(&package.to_string_lossy(), Some("python"));
    Some([package.as_str(), rest].iter().filter(|part| !part.is_empty()).copied().collect::<Vec<_>>().join("."))
}

fn alias_match<'a>(module: &'a str, alias: &'a str) -> Option<&'a str> {
    if let Some(prefix) = alias.strip_suffix('*') {
        return module.strip_prefix(prefix);
    }
    (module == alias).then_some("")
}

pub(super) struct ImportResolution<'a> {
    pub(super) invalid_bindings: &'a [String],
    pub(super) targets: Vec<(String, &'static str, Option<String>)>,
}

impl Relations {
    /// The internal files one file's imports resolve to, each with its
    /// confidence. An import naming a package outside the repository
    /// resolves to nothing and is dropped: the inversion answers which file
    /// in this repository imports this file, and an external package is not
    /// one.
    pub(super) fn resolve_imports(
        &self,
        importer_path: &str,
        imports: &[extraction::Import],
        language: Option<&str>,
        modules: &ModulePaths,
    ) -> Vec<(String, &'static str, Option<String>)> {
        let mut out: Vec<(String, &'static str, Option<String>)> = Vec::new();
        for import in imports {
            let resolution = self.resolve_import(importer_path, import, language, modules, imports);
            for (target, confidence, symbol) in resolution.targets {
                match out.iter_mut().find(|(file, _, _)| file == &target) {
                    Some(existing) => {
                        if confidence_code(confidence) < confidence_code(existing.1) {
                            existing.1 = confidence;
                            existing.2 = symbol;
                        } else if existing.2.is_none() && symbol.is_some() {
                            existing.2 = symbol;
                        }
                    }
                    None => out.push((target, confidence, symbol)),
                }
            }
        }
        out
    }

    pub(super) fn resolve_import<'a>(
        &self,
        importer_path: &str,
        import: &'a extraction::Import,
        language: Option<&str>,
        modules: &ModulePaths,
        imports: &[extraction::Import],
    ) -> ImportResolution<'a> {
        // `from module import symbol` names the symbol's own file when
        // one exists, and the module otherwise — Python's package form,
        // and the reason a from-imported file must not read as having no
        // importers. Rust's `use crate::a::b` names `b`'s own file the same
        // way when `b` is a module. Other languages' named imports still
        // name the module written after `from`, not a child path made from
        // the symbol.
        let separator = match language {
            Some("python") => Some("."),
            Some("rust") => Some("::"),
            _ => None,
        };
        // A bare relative module, Python's `from . import y`, already ends
        // in its separator.
        let combined = separator.and_then(|separator| {
            let separator = if import.module.ends_with(separator) { "" } else { separator };
            import
                .symbol
                .as_ref()
                .map(|symbol| format!("{}{separator}{}", import.module, symbol))
        });
        let combined_resolved = combined
            .and_then(|module| modules.resolve(&module, importer_path, language, imports))
            .unwrap_or_default();
        let php_combined = (language == Some("php"))
            .then(|| {
                import
                    .symbol
                    .as_ref()
                    .map(|symbol| format!("{}\\{}", import.module, symbol))
            })
            .flatten()
            .and_then(|module| modules.resolve(&module, importer_path, language, imports))
            .unwrap_or_default();
        let module_resolved = modules.resolve(&import.module, importer_path, language, imports);
        let Some(module_resolved) = module_resolved else {
            return ImportResolution {
                invalid_bindings: if import.locals.is_empty() {
                    import.symbol.as_slice()
                } else {
                    &import.locals
                },
                targets: Vec::new(),
            };
        };
        let mut resolved = if !combined_resolved.is_empty() {
            combined_resolved
        } else if !php_combined.is_empty() {
            php_combined
        } else {
            module_resolved
        };
        // A module path that resolved is a clean resolution. When it did
        // not, an imported symbol the map knows still names its file:
        // one declaring file is INFERRED, several are AMBIGUOUS.
        let confidence = if resolved.len() == 1 {
            CONFIDENCE_EXTRACTED
        } else if resolved.len() > 1 {
            CONFIDENCE_AMBIGUOUS
        } else {
            let written = import
                .symbol
                .as_ref()
                .map_or_else(|| import.module.clone(), |symbol| format!("{}::{symbol}", import.module));
            if let Some(libraries) = (language == Some("rust"))
                .then(|| modules.left_out_crates(&written, importer_path))
                .flatten()
            {
                resolved = libraries
                    .iter()
                    .flat_map(|library| modules.rust_path_within(&written, library).0)
                    .collect();
                CONFIDENCE_INFERRED
            } else if modules.roots.has_mapping(language) {
                let external = language != Some("rust") || modules.is_external(&written, importer_path);
                return ImportResolution {
                    invalid_bindings: if !external {
                        &[]
                    } else if import.locals.is_empty() {
                        import.symbol.as_slice()
                    } else {
                        &import.locals
                    },
                    targets: Vec::new(),
                };
            } else {
                let Some(symbol) = import.symbol.as_ref() else {
                    return ImportResolution {
                        invalid_bindings: &[],
                        targets: Vec::new(),
                    };
                };
                resolved = self
                    .defined_in(symbol)
                    .filter(|file| {
                        self.built_from
                            .get(*file)
                            .and_then(|built| built.language.as_deref())
                            == language
                    })
                    .map(str::to_string)
                    .collect();
                match resolved.len() {
                    1 => CONFIDENCE_INFERRED,
                    n if n > 1 => CONFIDENCE_AMBIGUOUS,
                    _ => {
                        return ImportResolution {
                            invalid_bindings: &[],
                            targets: Vec::new(),
                        }
                    }
                }
            }
        };
        let mut targets = Vec::new();
        for target in resolved {
            // The named symbol only stands as the depended-on thing when
            // the target file actually declares it; otherwise the module
            // is what the import reaches.
            let symbol = import
                .symbol
                .as_ref()
                .filter(|s| self.defined_in(s).any(|file| file == target))
                .cloned();
            // One edge per target file. A language that emits both a
            // module import and a named import for the same statement
            // would otherwise double its direct-edge count. The named
            // symbol wins because it says what is actually depended on.
            targets.push((target, confidence, symbol));
        }
        ImportResolution {
            invalid_bindings: &[],
            targets,
        }
    }

}

/// The name of the innermost inline `mod` of `extraction` that holds
/// `line`, other than `declaration` itself.
pub(super) fn inline_module(extraction: &ExtractionResult, line: i64, declaration: Option<&Declaration>) -> Option<String> {
    let modules: Vec<&Declaration> = extraction
        .declarations
        .iter()
        .filter(|module| {
            module.kind == "module" && !declaration.is_some_and(|declaration| std::ptr::eq(*module, declaration))
        })
        .collect();
    extraction::innermost(modules.iter().map(|module| (module.header_line, module.end_line)), line)
        .map(|index| modules[index].name.clone())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Module {
    Every,
    TopLevel,
    Inline(String),
}

/// Whether `candidate` counts at a position that stands for `module`: its
/// innermost inline `mod` is that module.
pub(super) fn admits(module: &Module, candidate: &Candidate) -> bool {
    in_module(module, candidate.inline_module.as_deref())
}

/// Whether a declaration whose innermost inline `mod` is `inline_module`
/// counts at a position that stands for `module`.
pub(super) fn in_module(module: &Module, inline_module: Option<&str>) -> bool {
    match module {
        Module::Every => true,
        Module::TopLevel => inline_module.is_none(),
        Module::Inline(name) => inline_module == Some(name.as_str()),
    }
}

/// The module `rest`, the segments of a Rust path past `file`, the file it
/// reaches, names there: the inline `mod` the last of them names when each
/// names an inline `mod` `file` declares, the top level when there are
/// none, and `None` when one names a type.
pub(super) fn named_module(file: &str, rest: &[String], relations: &Relations) -> Option<Module> {
    rest.iter()
        .all(|segment| relations.inline_modules_of(file).iter().any(|(name, _)| name == segment))
        .then(|| rest.last().map_or(Module::TopLevel, |name| Module::Inline(name.clone())))
}

/// What a walk from a Rust reference's start positions reached: every
/// position it counted, with the name it looks for there, each file those
/// positions sit in, with the confidence it was reached by, the dead ends,
/// positions a path or a `use` that binds the name reaches that declare
/// nothing and lead nowhere, where a `*` that brings no such name is no
/// dead end, the names each `use`
/// that binds the name and whose path is proven to live outside the
/// repository leads to, and whether a `*` it followed is proven to.
#[derive(Default)]
pub(super) struct Walk {
    pub(super) positions: Vec<(String, Module, String)>,
    pub(super) reached: HashMap<String, &'static str>,
    pub(super) dead_ends: Vec<(String, Module, String)>,
    pub(super) outside: Vec<String>,
    outside_glob: bool,
}

impl Walk {
    /// Whether the walk proves its name lives outside the repository: a
    /// `use` that binds it is external, or a `*` is external and every lead
    /// into the repository was followed to its end, because a dead end may
    /// still hold the name.
    pub(super) fn external(&self) -> bool {
        !self.outside.is_empty() || (self.outside_glob && self.dead_ends.is_empty())
    }
}

/// The walk from `starts`, each a file, the module it stands for, whether
/// it is the bare-name start at the reference, and the name it looks for,
/// until each position `declares` its name; `None` leaves the reference to
/// the name rule.
///
/// The walk moves between positions, each a file and the [`Module`] it
/// stands for there: `Module::Inline` one inline `mod` of it,
/// `Module::TopLevel` its top level, or `Module::Every` every module of it,
/// for a path that starts with `self` and stays in the file that wrote it,
/// because the extractor folds a path into an inline `mod` into `self` and
/// drops the `mod`. A bare name starts in its own file at the innermost
/// inline `mod` that holds the reference. A path or a `use` reaches the file
/// it names, then each leading segment of the rest that names an inline
/// `mod` the file declares, and stands for the last of them, else the file's
/// top level, the module [`named_module`] reads from those segments, by
/// [`position_of`]. A segment after them names a type, so
/// `engine::Thing::new()` starts at the top level of `engine.rs` as a type
/// path and `nested::imp::start()` starts at `imp` in `nested.rs` as a free
/// call. A `*` over a type adds no position, because it brings in only the
/// type's variants and associated items. A candidate counts at a position
/// only when [`admits`] accepts it there.
///
/// From each position the walk follows the `use` items of its module that
/// bind the name, a `use … as Local` by `Local` and onward by the name it
/// renames, and the modules a `*` imports, one hop at a time until a
/// position declares a target the call can reach, so a `pub use` re-export
/// and a glob reach the declaring module. Each stored `use` row is owned by
/// the module whose own body holds its block: a top level owns the rows
/// outside every block, an inline `mod` the rows whose innermost inline
/// `mod`, by the lines each stored inline `mod` spans, is that `mod` and
/// whose block ends on that `mod`'s end line, and no module owns a row in a
/// function body. Only the bare-name start, where a free call or a
/// one-segment path starts at the call, follows the `use` items in force at
/// the call, the calling function's own included, because only a bare name
/// sees them. When one of them that binds the name sits in a function body
/// and its path names no module of the repository, by [`named_module`], it
/// hides the start's own declaration of the name, because in Rust a
/// function's own `use` hides a same-named item of its module in the
/// namespace it binds, so that start neither stops on its declaration nor
/// counts it. A `use` of a module binds the module namespace alone, so it
/// hides no function. When a `*` in force at the call sits in a function
/// body, the start counts its declaration and still follows the glob,
/// because the glob's names are known only where it leads, and in Rust a
/// function's own glob outranks its module's items. Two limits are
/// accepted: a `use` of a standard-library module still hides, because
/// tracer cannot tell a `std` module from a `std` function, and a `fn`
/// declared in a nested block counts as an item of its module. Every other
/// position, a path's start and every hop, a hop back into the calling
/// module included, follows the stored module-body rows of its file that its
/// module owns, and `Module::Every` every module-body row. A hop's
/// [`ModulePaths::rust_path`] reads its first segment through the globs of
/// its position's followed rows alone, never a sibling module's. A hop into
/// a left-out name adds that name's library files as `INFERRED` with every
/// module, a `use` that binds the name and whose path is external records
/// the name it leads to as outside the repository, a `*` whose path is
/// external is recorded too, and a hop that names no other file and is not
/// external leaves the reference to the name rule.
pub(super) fn walk_modules(
    starts: Vec<(String, Module, bool, String)>,
    imports: &[extraction::Import],
    declares: impl Fn(&str, &Module, &str) -> bool,
    modules: &ModulePaths,
    relations: &Relations,
) -> Option<Walk> {
    let mut walk = Walk::default();
    let mut seen: HashSet<_> = starts.iter().cloned().collect();
    let mut bound: HashSet<_> = starts.iter().map(|(file, module, _, name)| (file.clone(), module.clone(), name.clone())).collect();
    let mut frontier = starts;
    while let Some((current, module, at_call, name)) = frontier.pop() {
        walk.reached.entry(current.clone()).or_insert(CONFIDENCE_EXTRACTED);
        let declared = declares(&current, &module, &name);
        let hidden = declared
            && at_call
            && imports.iter().any(|import| {
                import.binding() == Some(name.as_str())
                    && owner_module(relations, &current, import).is_none()
                    && !import.symbol.as_ref().is_some_and(|symbol| {
                        let path = format!("{}::{symbol}", import.module);
                        let (files, rest) = modules.rust_path(&path, &current, imports);
                        files.iter().any(|target| named_module(target, &rest, relations).is_some())
                    })
            });
        let shared = declared
            && at_call
            && imports
                .iter()
                .any(|import| import.symbol.is_none() && owner_module(relations, &current, import).is_none());
        if !hidden {
            walk.positions.push((current.clone(), module.clone(), name.clone()));
        }
        if declared && !hidden && !shared {
            continue;
        }
        let followed: Cow<[extraction::Import]> = if at_call {
            Cow::Borrowed(imports)
        } else {
            let follows = |import: &extraction::Import| {
                owner_module(relations, &current, import).is_some_and(|owner| module == Module::Every || owner == module)
            };
            Cow::Owned(relations.imports_of(&current).iter().filter(|import| follows(import)).cloned().collect())
        };
        let mut leads = false;
        for import in followed.iter() {
            let (path, next) = match &import.symbol {
                None => (import.module.clone(), name.clone()),
                Some(symbol) if import.binding() == Some(name.as_str()) => {
                    (format!("{}::{symbol}", import.module), symbol.clone())
                }
                Some(_) => continue,
            };
            let (targets, mut rest) = modules.rust_item_path(&path, &current, &followed);
            if targets.is_empty() {
                match modules.left_out_crates(&path, &current) {
                    Some(libraries) => {
                        for member in libraries.iter().flat_map(|library| modules.crate_files(library)) {
                            walk.reached.entry(member.clone()).or_insert(CONFIDENCE_INFERRED);
                            walk.positions.push((member, Module::Every, next.clone()));
                        }
                        leads = true;
                    }
                    None if !modules.is_external(&path, &current) => return None,
                    None if import.symbol.is_some() => walk.outside.push(next.clone()),
                    None => walk.outside_glob = true,
                }
            }
            if import.symbol.is_some() {
                rest.pop();
            }
            for target in targets {
                let (target, module, type_path) = position_of(&path, &current, target, &rest, relations);
                if type_path && import.symbol.is_none() {
                    continue;
                }
                leads = true;
                if import.symbol.is_some() {
                    bound.insert((target.clone(), module.clone(), next.clone()));
                }
                if seen.insert((target.clone(), module.clone(), false, next.clone())) {
                    frontier.push((target, module, false, next.clone()));
                }
            }
        }
        if !at_call && !declared && !leads && bound.contains(&(current.clone(), module.clone(), name.clone())) {
            walk.dead_ends.push((current, module, name));
        }
    }
    Some(walk)
}

/// The position a Rust path `path`, written in `writer`, reaches in
/// `target`, a file it names with `rest` left past it, and whether a type
/// segment follows the module there.
pub(super) fn position_of(path: &str, writer: &str, target: String, rest: &[String], relations: &Relations) -> (String, Module, bool) {
    let (module, type_path) = (0..=rest.len())
        .rev()
        .find_map(|end| named_module(&target, &rest[..end], relations).map(|module| (module, end < rest.len())))
        .expect("a path with no segment past its file names the file's top level");
    let folded = target == writer && path.split("::").find(|segment| !segment.is_empty()) == Some("self");
    (target, if folded { Module::Every } else { module }, type_path)
}

/// The module whose own body holds `import` in `file`: its top level when no
/// block holds it, the inline `mod` whose body is its block, and none for a
/// block inside a function.
fn owner_module(relations: &Relations, file: &str, import: &extraction::Import) -> Option<Module> {
    let (_, last) = match import.block {
        None => return Some(Module::TopLevel),
        Some(block) => block,
    };
    let inline_modules = relations.inline_modules_of(file);
    extraction::innermost(inline_modules.iter().map(|(_, span)| *span), import.line)
        .map(|index| &inline_modules[index])
        .filter(|(_, (_, end))| last == *end)
        .map(|(declared, _)| Module::Inline(declared.clone()))
}

/// The paths a Rust path `written` stands for: one through each `use` of
/// `imports` that binds its first segment, else `written` itself.
pub(super) fn bound_paths(written: &str, imports: &[extraction::Import]) -> Vec<String> {
    let bound = path_after_binding(written, imports);
    if bound.is_empty() {
        vec![written.to_string()]
    } else {
        bound
    }
}

/// The paths a Rust path `written` stands for through each `use` of
/// `imports` that binds its first segment: that `use`'s path, then the rest
/// of `written`. Several `use` items bind one name under `cfg`
/// alternatives, and each counts.
pub(super) fn path_after_binding(written: &str, imports: &[extraction::Import]) -> Vec<String> {
    let (first, rest) = written.split_once("::").map_or((written, None), |(first, rest)| (first, Some(rest)));
    imports
        .iter()
        .filter(|import| import.binding() == Some(first))
        .map(|import| {
            [import.module.as_str()]
                .into_iter()
                .chain(import.symbol.as_deref())
                .chain(rest)
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
                .join("::")
        })
        .collect()
}
