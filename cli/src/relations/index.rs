//! The stored index: read from its entries, absorbed from each file's
//! extraction, updated for the files that changed, and saved.

use crate::cache;
use crate::extraction::{self, ExtractionResult};
use crate::file_facts::{self, FileFacts};
use crate::relations::manifests::{CargoManifests, Roots};
use crate::relations::modules::{inline_module, tail, ModulePaths};
use crate::relations::{
    confidence_code, confidence_name, normalized_path, Built, BuiltEntry, DeclaredType, DirectoryIndex,
    DirectoryMetrics, Importer, Inherits, InlineModule, ModuleFile, ModuleRows, Relations, StoredBuilt,
    CONFIDENCE_AMBIGUOUS,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// The index exactly as it sits on disk, so reading it allocates the rows
/// and nothing else. `stored_forms` writes this shape.
#[derive(Deserialize)]
struct StoredEdges {
    files: Vec<String>,
    #[serde(default)]
    names: Vec<String>,
    built: Vec<StoredBuilt>,
    importers: HashMap<String, Vec<(u32, u64, Option<String>)>>,
    #[serde(default)]
    roots: Roots,
    #[serde(default)]
    cargo_manifests: CargoManifests,
}

#[derive(Serialize, Deserialize)]
struct StoredSymbols {
    table: String,
    symbols: HashMap<String, (Vec<u32>, Vec<u32>)>,
}

#[derive(Deserialize)]
struct StoredImports {
    table: String,
    imports: Vec<ModuleRows>,
}

/// The same three shapes on the way out, borrowing the rows they write so
/// a save serializes the index once, straight to bytes: building a JSON
/// value tree first cost 86ms per update on 37,000 import rows. Sorted maps
/// keep the bytes stable across writes of an unchanged index.
#[derive(Serialize)]
struct EdgesEntry<'a> {
    files: Vec<&'a str>,
    names: &'a [String],
    built: Vec<BuiltEntry<'a>>,
    importers: BTreeMap<String, Vec<(u32, u64, Option<&'a str>)>>,
    roots: &'a Roots,
    cargo_manifests: &'a CargoManifests,
}

#[derive(Serialize)]
struct SymbolsEntry<'a> {
    table: &'a str,
    symbols: BTreeMap<&'a str, (Vec<u32>, Vec<u32>)>,
}

#[derive(Serialize)]
struct ImportsEntry<'a> {
    table: &'a str,
    imports: Vec<(&'a [extraction::Import], &'a [InlineModule], &'a [DeclaredType])>,
}

impl Relations {
    pub(super) fn load_or_build_directory_metrics(&self) -> DirectoryIndex {
        cache::index(&DirectoriesIndex { relations: self }, &self.repo_root).ready().unwrap_or_default()
    }

    fn build_directory_metrics(&self) -> DirectoryIndex {
        let files: Vec<&Arc<str>> = self.built_from.keys().collect();
        let slots: HashMap<&str, usize> = files
            .iter()
            .enumerate()
            .map(|(index, file)| (&***file, index))
            .collect();
        let mut directory_ids = Vec::with_capacity(files.len());
        let mut directories = Vec::new();
        let mut directory_slots = HashMap::new();
        let mut metrics = Vec::new();
        for file in &files {
            let directory = directory_of(file);
            let directory_id = match directory_slots.get(directory.as_str()) {
                Some(id) => *id,
                None => {
                    let id = directories.len();
                    directory_slots.insert(directory.clone(), id);
                    directories.push(directory);
                    metrics.push(DirectoryMetrics::default());
                    id
                }
            };
            directory_ids.push(directory_id);
        }
        for (index, (_, built)) in self.built_from.iter().enumerate() {
            let metrics = &mut metrics[directory_ids[index]];
            metrics.files += 1;
            for &(name, count) in &built.annotations {
                let Some(name) = self.names.get(name as usize) else {
                    continue;
                };
                *metrics.annotations.entry(name.clone()).or_default() += count as usize;
            }
        }

        let mut imports_per_file: BTreeMap<String, u32> = BTreeMap::new();
        let mut imported_by = vec![Vec::new(); directories.len()];
        let mut imports = vec![Vec::new(); directories.len()];
        let mut imported_directories = vec![Vec::new(); directories.len()];
        for (target, importer) in self.import_edges() {
            if importer.confidence == CONFIDENCE_AMBIGUOUS {
                continue;
            }
            let (Some(&target), Some(&importer)) = (slots.get(target), slots.get(&*importer.file))
            else {
                continue;
            };
            *imports_per_file.entry(files[importer].to_string()).or_default() += 1;
            let target_directory = directory_ids[target];
            let importer_directory = directory_ids[importer];
            if target_directory != importer_directory {
                imported_by[target_directory].push(importer as u32);
                imports[importer_directory].push(target as u32);
                imported_directories[importer_directory].push(target_directory as u32);
            }
        }
        for index in 0..metrics.len() {
            for list in [&mut imported_by[index], &mut imports[index], &mut imported_directories[index]] {
                list.sort_unstable();
                list.dedup();
            }
            metrics[index].imported_by = imported_by[index].len() as u32;
            metrics[index].imports = imports[index].len() as u32;
            metrics[index].imported_directories = imported_directories[index]
                .iter()
                .filter_map(|directory| directories.get(*directory as usize))
                .cloned()
                .collect();
        }
        DirectoryIndex {
            directories: directories.into_iter().zip(metrics).collect(),
            imports_per_file,
        }
    }

    /// The one shared handle for a path. Every list that names a file holds a
    /// clone of this handle, so the path's bytes exist once per index.
    fn intern(&mut self, path: &str) -> (Arc<str>, u32) {
        if let Some((index, existing)) = self
            .files
            .iter()
            .enumerate()
            .find(|(_, existing)| &***existing == path)
        {
            return (Arc::clone(existing), index as u32);
        }
        let path: Arc<str> = Arc::from(path);
        self.files.push(Arc::clone(&path));
        (path, (self.files.len() - 1) as u32)
    }

    fn intern_name(&mut self, name: &str) -> u32 {
        if let Some(index) = self.names.iter().position(|candidate| candidate == name) {
            return index as u32;
        }
        self.names.push(name.to_string());
        (self.names.len() - 1) as u32
    }

    fn symbols_mut(&mut self) -> &mut HashMap<String, (Vec<u32>, Vec<u32>)> {
        self.symbols.get_or_init(|| self.load_symbols());
        self.symbols.get_mut().expect("symbols cell initialized")
    }

    fn imports_mut(&mut self) -> &mut HashMap<Arc<str>, ModuleRows> {
        self.imports.get_or_init(|| self.load_imports());
        self.imports.get_mut().expect("imports cell initialized")
    }

    /// Remove every trace of `file`, so its fresh contribution can be added
    /// without duplicating rows. A name left with no files at all is dropped
    /// rather than kept as an empty row.
    fn forget(&mut self, file: &str) {
        let index = self
            .files
            .iter()
            .position(|candidate| &**candidate == file)
            .map(|index| index as u32);
        self.symbols_mut().retain(|_, (defined_in, used_in)| {
            defined_in.retain(|candidate| Some(*candidate) != index);
            used_in.retain(|candidate| Some(*candidate) != index);
            !(defined_in.is_empty() && used_in.is_empty())
        });
        for list in self.importers.values_mut() {
            list.retain(|i| &*i.file != file);
        }
        self.importers.retain(|_, list| !list.is_empty());
        self.imports_mut().remove(file);
        self.built_from.remove(file);
    }

    /// Pass one: this file's declarations and use sites into the symbol map.
    /// Every file's declarations must land before any file's imports are
    /// resolved, because an import that misses on module path falls back to
    /// the symbol map — so the two passes cannot be merged.
    fn absorb_symbols(&mut self, file: &str, facts: &FileFacts, key: &str) {
        let (path, index) = self.intern(file);
        let annotations = facts
            .extraction
            .as_ref()
            .map(annotation_counts)
            .unwrap_or_default()
            .into_iter()
            .map(|(name, count)| (self.intern_name(&name), count))
            .collect();
        self.built_from.insert(
            Arc::clone(&path),
            Built {
                key: key.to_string(),
                language: facts.language.clone(),
                annotations,
                module_files: facts
                    .extraction
                    .as_ref()
                    .map(|extraction| module_files(file, extraction))
                    .unwrap_or_default(),
                inherits: facts.extraction.as_ref().map(inherits).unwrap_or_default(),
            },
        );
        let extraction = match &facts.extraction {
            Some(e) => e,
            None => return,
        };
        if let Some(rows) = module_rows(extraction, facts.language.as_deref()) {
            self.imports_mut().insert(Arc::clone(&path), rows);
        }
        for declaration in &extraction.declarations {
            let row = self
                .symbols_mut()
                .entry(declaration.name.to_lowercase())
                .or_default();
            if !row.0.contains(&index) {
                row.0.push(index);
            }
        }
        for reference in &extraction.references {
            let row = self
                .symbols_mut()
                .entry(reference.name.to_lowercase())
                .or_default();
            if !row.1.contains(&index) {
                row.1.push(index);
            }
        }
    }

    /// Pass two: this file's resolved imports into the import inversion.
    fn absorb_imports(&mut self, path: &str, targets: Vec<(String, &'static str, Option<String>)>) {
        let (importer, _) = self.intern(path);
        for (target, confidence, symbol) in targets {
            if target == path {
                continue;
            }
            let (target, _) = self.intern(&target);
            let list = self.importers.entry(target).or_default();
            if !list.iter().any(|i| i.file == importer) {
                list.push(Importer {
                    file: Arc::clone(&importer),
                    confidence,
                    symbol,
                });
            }
        }
    }

    /// The stored forms, with every path written once in the edges entry.
    ///
    /// A path appears in the index once per name that mentions it. Spelling
    /// each one in full made laravel-framework's entry 7.0 MB, and parsing it
    /// cost 81 MB of resident memory before a single query ran. One path
    /// table plus integer references carries the identical index in 1.1 MB.
    fn stored_forms(&self) -> (EdgesEntry<'_>, SymbolsEntry<'_>, ImportsEntry<'_>) {
        let edges = self.edges_entry();
        let slot: HashMap<&str, u32> = edges
            .files
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, i as u32))
            .collect();
        let at = |p: &str| slot.get(p).copied();
        let imports: Vec<(&[extraction::Import], &[InlineModule], &[DeclaredType])> = edges
            .files
            .iter()
            .map(|p| (self.imports_of(p), self.inline_modules_of(p), self.types_of(p)))
            .collect();

        let old_paths: Vec<&str> = self.files.iter().map(|path| &**path).collect();
        let remap = |indexes: &[u32]| {
            indexes
                .iter()
                .filter_map(|index| old_paths.get(*index as usize).and_then(|path| at(path)))
                .collect::<Vec<_>>()
        };
        let symbols: BTreeMap<&str, (Vec<u32>, Vec<u32>)> = self
            .symbols
            .get_or_init(|| self.load_symbols())
            .iter()
            .map(|(name, (defined_in, used_in))| {
                (name.as_str(), (remap(defined_in), remap(used_in)))
            })
            .collect();

        let table = self.file_table_hash.as_str();
        (
            edges,
            SymbolsEntry { table, symbols },
            ImportsEntry { table, imports },
        )
    }

    /// The edges entry alone, which is all a manifest change that moves no
    /// root mapping rewrites.
    fn edges_entry(&self) -> EdgesEntry<'_> {
        // `built_from` holds every indexed file, and no row can name a file
        // outside it: rows are absorbed per file and `forget` drops both.
        let paths: Vec<&str> = self.built_from.keys().map(|p| &**p).collect();
        let slot: HashMap<&str, u32> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, i as u32))
            .collect();
        let at = |p: &str| slot.get(p).copied();
        let mut importers = BTreeMap::new();
        for (target, list) in &self.importers {
            let Some(t) = at(target) else { continue };
            let rows: Vec<(u32, u64, Option<&str>)> = list
                .iter()
                .filter_map(|i| {
                    Some((
                        at(&i.file)?,
                        confidence_code(i.confidence),
                        i.symbol.as_deref(),
                    ))
                })
                .collect();
            importers.insert(t.to_string(), rows);
        }
        EdgesEntry {
            files: paths,
            names: &self.names,
            built: self
                .built_from
                .values()
                .map(|b| {
                    (
                        b.key.as_str(),
                        b.language.as_deref(),
                        b.annotations.as_slice(),
                        b.module_files.as_slice(),
                        b.inherits.as_slice(),
                    )
                })
                .collect(),
            importers,
            roots: &self.roots,
            cargo_manifests: &self.cargo_manifests,
        }
    }

    fn from_bytes(bytes: &[u8], repo_root: &Path) -> Option<Self> {
        let stored: StoredEdges = serde_json::from_slice(bytes).ok()?;
        // One handle per path, made once here. Every row below clones a
        // handle rather than the path's bytes, which is the whole reason the
        // index is cheap to hold.
        let files: Vec<Arc<str>> = stored.files.into_iter().map(Arc::<str>::from).collect();
        let file_table_hash = table_hash(files.iter().map(|path| &**path));
        let mut out = Relations {
            files,
            names: stored.names,
            file_table_hash,
            repo_root: repo_root.to_path_buf(),
            roots: stored.roots,
            cargo_manifests: stored.cargo_manifests,
            ..Default::default()
        };
        for (i, (key, language, annotations, module_files, inherits)) in stored.built.into_iter().enumerate() {
            let file = Arc::clone(out.files.get(i)?);
            out.built_from.insert(
                file,
                Built {
                    key,
                    language,
                    annotations,
                    module_files,
                    inherits,
                },
            );
        }
        for (target, list) in stored.importers {
            let Some(target) = target
                .parse::<u32>()
                .ok()
                .and_then(|index| out.files.get(index as usize).cloned())
            else {
                continue;
            };
            let rows: Vec<Importer> = list
                .into_iter()
                .filter_map(|(file, confidence, symbol)| {
                    Some(Importer {
                        file: out.files.get(file as usize).cloned()?,
                        confidence: confidence_name(confidence),
                        symbol,
                    })
                })
                .collect();
            out.importers.insert(target, rows);
        }
        Some(out)
    }

    /// Every name's declaring and mentioning files: the stored entry when its
    /// table matches this index, else rebuilt from every file's extraction
    /// under the symbols lock.
    pub(super) fn load_symbols(&self) -> HashMap<String, (Vec<u32>, Vec<u32>)> {
        cache::index(&SymbolsIndex { relations: self }, &self.repo_root)
            .ready()
            .map(|stored| stored.symbols)
            .unwrap_or_default()
    }

    /// Every indexed file's import rows, inline `mod`s, and Rust types: from
    /// the stored entry when its table matches this index, otherwise read
    /// back out of every file's per-file entry, which is the cost the stored
    /// entry exists to pay once. Loaded while `files` is still in stored
    /// order.
    pub(super) fn load_imports(&self) -> HashMap<Arc<str>, ModuleRows> {
        let mut imports = HashMap::new();
        let table = &self.file_table_hash;
        if let Some(stored) =
            cache::load_bytes(cache::NAMESPACE_FILE, imports_key(), &self.repo_root)
                .as_deref()
                .and_then(|bytes| serde_json::from_slice::<StoredImports>(bytes).ok())
                .filter(|stored| stored.table == *table)
        {
            for (index, rows) in stored.imports.into_iter().enumerate() {
                let empty = rows.0.is_empty() && rows.1.is_empty() && rows.2.is_empty();
                if let (false, Some(file)) = (empty, self.files.get(index)) {
                    imports.insert(Arc::clone(file), rows);
                }
            }
            return imports;
        }
        let files: Vec<Arc<str>> = self.built_from.keys().cloned().collect();
        for chunk in files.chunks(file_facts::RESOLVE_CHUNK) {
            let paths: Vec<PathBuf> = chunk
                .iter()
                .map(|path| self.repo_root.join(&**path))
                .collect();
            let mut facts = file_facts::extracted(&paths, &self.repo_root);
            for path in chunk {
                let Some(facts) = facts.remove(&**path) else {
                    continue;
                };
                if let Some(rows) = facts
                    .extraction
                    .as_ref()
                    .and_then(|extraction| module_rows(extraction, facts.language.as_deref()))
                {
                    imports.insert(Arc::clone(path), rows);
                }
            }
        }
        imports
    }

    fn rebuild_symbols(&self) -> HashMap<String, (Vec<u32>, Vec<u32>)> {
        let mut symbols: HashMap<String, (Vec<u32>, Vec<u32>)> = HashMap::new();
        for (base, chunk) in self.files.chunks(file_facts::RESOLVE_CHUNK).enumerate() {
            let paths: Vec<PathBuf> = chunk
                .iter()
                .map(|path| self.repo_root.join(&**path))
                .collect();
            let facts = file_facts::extracted(&paths, &self.repo_root);
            for (offset, path) in chunk.iter().enumerate() {
                let index = base * file_facts::RESOLVE_CHUNK + offset;
                let Some(facts) = facts.get(&**path) else {
                    continue;
                };
                let Some(extraction) = &facts.extraction else {
                    continue;
                };
                for declaration in &extraction.declarations {
                    let row = symbols.entry(declaration.name.to_lowercase()).or_default();
                    if !row.0.contains(&(index as u32)) {
                        row.0.push(index as u32);
                    }
                }
                for reference in &extraction.references {
                    let row = symbols.entry(reference.name.to_lowercase()).or_default();
                    if !row.1.contains(&(index as u32)) {
                        row.1.push(index as u32);
                    }
                }
            }
        }
        symbols
    }
}

fn annotation_counts(extraction: &ExtractionResult) -> BTreeMap<String, u32> {
    let mut annotations = BTreeMap::new();
    for declaration in &extraction.declarations {
        for annotation in &declaration.annotations {
            *annotations.entry(annotation.clone()).or_default() += 1;
        }
    }
    annotations
}

fn directory_of(file: &str) -> String {
    match Path::new(file).parent().and_then(Path::to_str) {
        Some("") | None => "./".to_string(),
        Some(parent) => format!("{parent}/"),
    }
}

/// Stable key: the index is mutable and updated in place, so it never
/// carries a fingerprint. It lives in its schema's directory because the rows
/// describe extraction output, which a schema bump can reshape.
fn edges_key() -> &'static str {
    "relations_edges_v1"
}

fn symbols_key() -> &'static str {
    "relations_symbols_v1"
}

fn imports_key() -> &'static str {
    "relations_imports_v1"
}

fn directories_key() -> &'static str {
    "relations_directories_v1"
}

/// The symbols entry of one relations index, read only at its file table.
struct SymbolsIndex<'a> {
    relations: &'a Relations,
}

impl cache::Index for SymbolsIndex<'_> {
    type Stored = StoredSymbols;
    type Change = ();

    fn key(&self) -> String {
        symbols_key().to_string()
    }

    fn read(&self, bytes: &[u8]) -> Option<StoredSymbols> {
        serde_json::from_slice::<StoredSymbols>(bytes).ok().filter(|stored| stored.table == self.relations.file_table_hash)
    }

    fn change(&self, stored: Option<&StoredSymbols>) -> Option<()> {
        stored.is_none().then_some(())
    }

    fn apply(&self, _stored: Option<StoredSymbols>, _change: ()) -> Option<StoredSymbols> {
        Some(StoredSymbols {
            table: self.relations.file_table_hash.clone(),
            symbols: self.relations.rebuild_symbols(),
        })
    }
}

pub(super) struct DirectoriesIndex<'a> {
    relations: &'a Relations,
}

impl cache::Index for DirectoriesIndex<'_> {
    type Stored = DirectoryIndex;
    type Change = ();

    fn key(&self) -> String {
        directories_key().to_string()
    }

    fn read(&self, bytes: &[u8]) -> Option<DirectoryIndex> {
        serde_json::from_slice(bytes).ok()
    }

    fn change(&self, stored: Option<&DirectoryIndex>) -> Option<()> {
        stored.is_none().then_some(())
    }

    fn apply(&self, _stored: Option<DirectoryIndex>, _change: ()) -> Option<DirectoryIndex> {
        Some(self.relations.build_directory_metrics())
    }
}

fn table_hash<'a>(paths: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for path in paths {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}

/// The files the index no longer covers and the files whose content key
/// differs from the one their rows were absorbed from, or None when the
/// index is current.
fn stale(
    relations: &Relations,
    hashes: &BTreeMap<String, String>,
) -> Option<(Vec<String>, Vec<String>)> {
    let gone: Vec<String> = relations
        .built_from
        .keys()
        .filter(|path| !hashes.contains_key(&***path))
        .map(|path| path.to_string())
        .collect();
    let moved: Vec<String> = hashes
        .iter()
        .filter(|(path, key)| relations.built_from.get(path.as_str()).map(|b| &b.key) != Some(*key))
        .map(|(path, _)| path.clone())
        .collect();
    (!gone.is_empty() || !moved.is_empty()).then_some((gone, moved))
}

pub(super) struct RelationsIndex<'a> {
    pub(super) repo_root: &'a Path,
    pub(super) hashes: OnceLock<Option<BTreeMap<String, String>>>,
    /// When extra context stops waiting, `None` for an answer.
    pub(super) limit: Option<Instant>,
}

pub(super) struct RelationsChange {
    gone: Vec<String>,
    moved: Vec<String>,
    keys: HashMap<String, String>,
    roots: Roots,
    manifests: CargoManifests,
}

impl cache::Index for RelationsIndex<'_> {
    type Stored = Relations;
    type Change = RelationsChange;

    fn key(&self) -> String {
        edges_key().to_string()
    }

    fn read(&self, bytes: &[u8]) -> Option<Relations> {
        Relations::from_bytes(bytes, self.repo_root)
    }

    /// The per-file content hashes are swept once, against the extensions the
    /// first index read knows; the holder of the lock may have absorbed this
    /// very change while another call waited, so the change is computed again
    /// under the lock and only what it left remains. A file that moves
    /// between the hash sweep and the update is absorbed under its earlier
    /// key, which the next call sees as moved again and corrects.
    fn change(&self, stored: Option<&Relations>) -> Option<RelationsChange> {
        let empty;
        let relations = match stored {
            Some(relations) => relations,
            None => {
                empty = self.empty();
                &empty
            }
        };
        let hashes = self
            .hashes
            .get_or_init(|| {
                let known_extensions: HashSet<String> = relations
                    .built_from
                    .keys()
                    .filter_map(|path| Path::new(&**path).extension().and_then(|extension| extension.to_str()))
                    .map(str::to_lowercase)
                    .collect();
                let (files, stamps) = discover_files_stamped(self.repo_root, &known_extensions)?;
                Some(file_facts::file_hashes_for_stamped(&files, &stamps, self.repo_root))
            })
            .as_ref()?;
        let (roots, manifests) = Roots::read(self.repo_root, &relations.cargo_manifests);
        let stale = stale(relations, hashes);
        if stale.is_none() && relations.roots == roots && relations.cargo_manifests == manifests {
            return None;
        }
        let (gone, moved) = stale.unwrap_or_default();
        let keys = moved
            .iter()
            .filter_map(|path| Some((path.clone(), hashes.get(path)?.clone())))
            .collect();
        Some(RelationsChange {
            gone,
            moved,
            keys,
            roots,
            manifests,
        })
    }

    fn apply(&self, stored: Option<Relations>, change: RelationsChange) -> Option<Relations> {
        update(stored.unwrap_or_else(|| self.empty()), change, self.repo_root, self.limit)
    }

    fn limit(&self) -> Option<Instant> {
        self.limit
    }
}

impl RelationsIndex<'_> {
    pub(super) fn empty(&self) -> Relations {
        Relations {
            repo_root: self.repo_root.to_path_buf(),
            ..Default::default()
        }
    }
}

impl serde::Serialize for Relations {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.edges_entry().serialize(serializer)
    }
}

/// The most files an update extracts with no limit. Extracting them costs
/// about as much as the process starting up, so cutting them would only make
/// a busy machine's small repository lose its relations for nothing.
const UNCUT_FILES: usize = 100;

/// The index with `change` absorbed, `None` when `limit` passed before every
/// moved file was extracted. An update of at most `UNCUT_FILES` files always
/// finishes.
fn update(mut relations: Relations, change: RelationsChange, repo_root: &Path, limit: Option<Instant>) -> Option<Relations> {
    let RelationsChange {
        gone,
        moved,
        keys,
        roots,
        manifests,
    } = change;
    let limit = limit.filter(|_| moved.len() > UNCUT_FILES);
    relations.cargo_manifests = manifests;
    let roots_changed = ["php", "typescript", "rust"]
        .into_iter()
        .any(|language| !relations.roots.has_same_mapping(&roots, Some(language)));
    if gone.is_empty() && moved.is_empty() && !roots_changed {
        relations.roots = roots;
        return Some(relations);
    }

    // A content change re-absorbs the files that moved. When a file appears
    // or disappears, or a touched file's declarations changed, module-path
    // and symbol-fallback resolution can shift for importers that never
    // changed, so the importers whose rows name an affected module tail or
    // symbol are re-resolved from their stored import rows. A changed root
    // mapping re-resolves the rows of every file in its language. Nothing
    // but the touched files is read back.
    let added: Vec<String> = moved
        .iter()
        .filter(|path| !relations.built_from.contains_key(path.as_str()))
        .cloned()
        .collect();
    relations.symbols.get_or_init(|| relations.load_symbols());
    relations.imports.get_or_init(|| relations.load_imports());
    let moved_files: HashSet<&str> = moved.iter().map(String::as_str).collect();
    let touched_files: HashSet<&str> = moved_files
        .iter()
        .copied()
        .chain(gone.iter().map(String::as_str))
        .collect();
    let declarations_of =
        |relations: &Relations, files: &HashSet<&str>| -> BTreeSet<(String, String)> {
            relations
                .symbols
                .get()
                .expect("symbols loaded before the update")
                .iter()
                .flat_map(|(name, (defined_in, _))| {
                    defined_in
                        .iter()
                        .filter_map(|index| relations.files.get(*index as usize))
                        .filter(|file| files.contains::<str>(file.as_ref()))
                        .map(|file| (name.clone(), file.to_string()))
                })
                .collect()
        };
    let declarations_before = declarations_of(&relations, &touched_files);
    // A Rust path reads a module from the `#[path]` items and, one hop, from
    // the top-level `use` items of the files it passes, so a touched file
    // whose either changed moves resolution in files that never changed.
    let bindings_of = |relations: &Relations, files: &HashSet<&str>| -> BTreeSet<(String, String, String, Option<String>)> {
        files
            .iter()
            .flat_map(|file| {
                relations
                    .imports_of(file)
                    .iter()
                    .filter(|import| import.block.is_none())
                    .filter_map(|import| {
                        Some((file.to_string(), import.binding()?.to_string(), import.module.clone(), import.symbol.clone()))
                    })
            })
            .collect()
    };
    let module_files_of = |relations: &Relations, files: &HashSet<&str>| -> BTreeSet<(String, String, String)> {
        files
            .iter()
            .filter_map(|file| relations.built_from.get(*file).map(|built| (file, built)))
            .flat_map(|(file, built)| {
                built.module_files.iter().map(|(name, target)| (file.to_string(), name.clone(), target.clone()))
            })
            .collect()
    };
    let bindings_before = bindings_of(&relations, &touched_files);
    let module_files_before = module_files_of(&relations, &touched_files);

    for path in gone.iter().chain(moved.iter()) {
        relations.forget(path);
    }
    for chunk in moved.chunks(file_facts::RESOLVE_CHUNK) {
        let needed: Vec<PathBuf> = chunk.iter().map(|rel| repo_root.join(rel)).collect();
        let facts = file_facts::extracted_within(&needed, repo_root, limit)?;
        for path in chunk {
            let (Some(f), Some(key)) = (facts.get(path), keys.get(path)) else {
                continue;
            };
            relations.absorb_symbols(path, f, key);
        }
    }

    // Declarations first, then imports: an import that misses on module path
    // falls back to the symbol map, so no file's imports can resolve until
    // every touched file's declarations have landed.
    //
    // An importer is affected when one of its rows names a module whose
    // final segment is an added or removed file's stem, which is the only way
    // module-path resolution can gain or lose a candidate, or names a symbol
    // that a touched file declared before or declares now, which is the only
    // way symbol fallback can move. Re-resolving every row instead cost 140ms
    // per update on 37,000 rows.
    let declarations_after = declarations_of(&relations, &moved_files);
    let mut stems: HashSet<String> = HashSet::new();
    for path in gone.iter().chain(added.iter()) {
        if let Some(stem) = Path::new(path).file_stem().and_then(|stem| stem.to_str()) {
            stems.insert(stem.to_string());
            stems.insert(stem.to_lowercase());
            if stem == "index" || stem == "mod" {
                if let Some(parent) = Path::new(path)
                    .parent()
                    .and_then(|parent| parent.file_name())
                {
                    let parent = parent.to_string_lossy().to_string();
                    stems.insert(parent.to_lowercase());
                    stems.insert(parent);
                }
            }
        }
    }
    let names: HashSet<&str> = declarations_before
        .iter()
        .chain(declarations_after.iter())
        .map(|(name, _)| name.as_str())
        .collect();
    let rebound: HashSet<String> = bindings_before
        .symmetric_difference(&bindings_of(&relations, &moved_files))
        .map(|(_, binding, _, _)| binding.clone())
        .collect();
    let modules_moved = module_files_before != module_files_of(&relations, &moved_files);
    let affected: HashSet<Arc<str>> = relations
        .imports
        .get()
        .expect("imports loaded before the update")
        .iter()
        .filter(|(path, (rows, _, _))| {
            relations.importers.is_empty()
                || moved_files.contains::<str>(path.as_ref())
                || !relations.roots.has_same_mapping(&roots, relations.language(path))
                || (modules_moved && relations.language(path) == Some("rust"))
                || rows.iter().any(|row| {
                    let module = row.module.replace('\\', "/");
                    let end = tail(&module);
                    // A named import can resolve to the symbol's own file
                    // (Python's `from pkg import helper` reaching
                    // `pkg/helper.py`), so the symbol is checked as a stem
                    // as well as a name.
                    stems.contains(end)
                        || stems.contains(&end.to_lowercase())
                        || row.module.split("::").any(|segment| rebound.contains(segment))
                        || row.symbol.as_deref().is_some_and(|symbol| {
                            let lower = symbol.to_lowercase();
                            names.contains(lower.as_str())
                                || stems.contains(symbol)
                                || stems.contains(&lower)
                        })
                })
        })
        .map(|(path, _)| Arc::clone(path))
        .collect();
    relations.roots = roots;
    for list in relations.importers.values_mut() {
        list.retain(|importer| !affected.contains(&importer.file));
    }
    relations.importers.retain(|_, list| !list.is_empty());
    let resolved: Vec<_> = {
        let modules = ModulePaths::new(&relations);
        affected
            .iter()
            .map(|path| {
                let targets = relations.resolve_imports(path, relations.imports_of(path), relations.language(path), &modules);
                (Arc::clone(path), targets)
            })
            .collect()
    };
    for (path, targets) in resolved {
        relations.absorb_imports(&path, targets);
    }

    // The table is hashed in stored order, which `from_bytes` reproduces;
    // `files` itself appends each newly interned path at the end.
    relations.file_table_hash = table_hash(relations.built_from.keys().map(|path| &**path));
    let (_, symbols, imports) = relations.stored_forms();
    let _ = cache::save(cache::NAMESPACE_FILE, symbols_key(), &symbols, repo_root);
    let _ = cache::save(cache::NAMESPACE_FILE, imports_key(), &imports, repo_root);
    cache::update(&DirectoriesIndex { relations: &relations }, repo_root, ());
    Some(relations)
}

fn discover_files_stamped(
    repo_root: &Path,
    known_extensions: &HashSet<String>,
) -> Option<(Vec<PathBuf>, Vec<crate::repo_files::Stamp>)> {
    let listed = crate::repo_files::tracked_files(repo_root, None)?;
    let mut paths = Vec::new();
    let mut stamps = Vec::new();
    for ((path, stamp), symlink) in listed
        .paths
        .iter()
        .zip(&listed.stamps)
        .zip(&listed.symlinks)
    {
        let path = repo_root.join(path);
        let known_extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| known_extensions.contains(&extension.to_lowercase()));
        if !symlink && (known_extension || extraction::is_supported(&path)) {
            paths.push(path);
            stamps.push(stamp.clone());
        }
    }
    Some((paths, stamps))
}

/// The inline `mod`s `extraction` declares: each `mod` with a body, whose
/// header elides it.
fn inline_modules(extraction: &ExtractionResult) -> Vec<InlineModule> {
    extraction
        .declarations
        .iter()
        .filter(|declaration| declaration.kind == "module" && declaration.header.ends_with("{ … }"))
        .map(|declaration| (declaration.name.clone(), (declaration.header_line, declaration.end_line)))
        .collect()
}

/// What the index keeps of one file's modules, or `None` when the file
/// imports and declares none of it.
fn module_rows(extraction: &ExtractionResult, language: Option<&str>) -> Option<ModuleRows> {
    let inline_modules = inline_modules(extraction);
    let types: Vec<DeclaredType> = extraction
        .declarations
        .iter()
        .filter(|declaration| {
            language == Some("rust") && matches!(declaration.kind.as_str(), "class" | "enum" | "interface")
        })
        .map(|declaration| (declaration.name.clone(), inline_module(extraction, declaration.line, Some(declaration))))
        .collect();
    (!extraction.imports.is_empty() || !inline_modules.is_empty() || !types.is_empty())
        .then(|| (extraction.imports.clone(), inline_modules, types))
}

/// The top-level `mod` items of the file at `path` whose `#[path]` names
/// their file, each with that file, read from the directory that holds
/// `path` the way Cargo reads it.
fn module_files(path: &str, extraction: &ExtractionResult) -> Vec<ModuleFile> {
    let directory = Path::new(path).parent().unwrap_or(Path::new("")).to_path_buf();
    extraction
        .declarations
        .iter()
        .filter(|declaration| inline_module(extraction, declaration.line, Some(declaration)).is_none())
        .filter_map(|declaration| {
            let file = normalized_path(directory.clone(), declaration.module_file.as_deref()?)?;
            Some((declaration.name.clone(), file.to_string_lossy().to_string()))
        })
        .collect()
}

/// Each class, interface, trait, and enum a PHP file declares that inherits
/// from another type, with each type it names and whether a `use` of the
/// file binds that name.
fn inherits(extraction: &ExtractionResult) -> Vec<Inherits> {
    extraction
        .declarations
        .iter()
        .filter(|declaration| declaration.container.is_none() && !declaration.supertypes.is_empty())
        .map(|declaration| {
            let supertypes = declaration
                .supertypes
                .iter()
                .map(|supertype| {
                    let bound = extraction
                        .imports
                        .iter()
                        .any(|import| import.binding().is_some_and(|binding| binding.eq_ignore_ascii_case(supertype)));
                    (supertype.clone(), bound)
                })
                .collect();
            (declaration.name.clone(), supertypes)
        })
        .collect()
}
