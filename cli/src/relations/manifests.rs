//! The root mappings each language resolves imports through: PSR-4 from
//! `composer.json`, `paths` and `baseUrl` from `tsconfig.json` and
//! `jsconfig.json`, Pest's test case bindings, and every Cargo package's
//! crates.

use crate::relations::normalized_path;
use crate::repo_files::Stamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Roots {
    pub(super) php: BTreeMap<String, String>,
    pub(super) javascript_paths: BTreeMap<String, Vec<String>>,
    pub(super) javascript_base_url: Option<String>,
    /// Every Rust crate root file, with the name other crates `use` it by
    /// when it is a library.
    pub(super) rust_crates: BTreeMap<String, Option<String>>,
    /// Every Cargo package's directory, with each name its code reaches a
    /// crate by, its dependencies and its own library, and that crate's
    /// library root in the repository, or none outside it.
    pub(super) rust_dependencies: BTreeMap<String, BTreeMap<String, Option<String>>>,
    /// Every Cargo package's directory, with each name its dependencies
    /// leave out, and the library roots of the packages of the repository
    /// that name may reach.
    pub(super) rust_left_out: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    /// Every directory `tests/Pest.php` binds, with the test case class its
    /// closures run as, so `$this` in a Pest test names that class.
    php_test_cases: BTreeMap<String, String>,
}

/// The parts of a `Cargo.toml` that name its crate roots, its
/// dependencies, and the sources its `[patch]` redirects.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub(super) struct CargoManifest {
    package: Option<CargoPackage>,
    lib: Option<CargoTarget>,
    #[serde(default)]
    bin: Vec<CargoTarget>,
    #[serde(flatten)]
    dependencies: CargoDependencies,
    #[serde(default)]
    target: BTreeMap<String, CargoDependencies>,
    workspace: Option<CargoWorkspace>,
    #[serde(default)]
    patch: BTreeMap<String, toml::Table>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
struct CargoDependencies {
    #[serde(default)]
    dependencies: toml::Table,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: toml::Table,
    #[serde(default, rename = "build-dependencies")]
    build_dependencies: toml::Table,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
struct CargoWorkspace {
    #[serde(default)]
    dependencies: toml::Table,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct CargoPackage {
    name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct CargoTarget {
    name: Option<String>,
    path: Option<String>,
}

/// Every listed `Cargo.toml` as parsed, beside the stamp of the file it was
/// parsed from.
pub(super) type CargoManifests = BTreeMap<String, (Stamp, CargoManifest)>;

impl Roots {
    /// The root mappings, and the Cargo manifests they were read from. A
    /// manifest whose stamp matches `parsed` is not read again.
    pub(super) fn read(repo_root: &Path, parsed: &CargoManifests) -> (Self, CargoManifests) {
        let mut roots = Self::default();
        let manifests = roots.read_cargo_manifests(repo_root, parsed);
        for name in ["composer.json", "tsconfig.json", "jsconfig.json"] {
            let path = repo_root.join(name);
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if name == "composer.json" {
                for section in ["autoload", "autoload-dev"] {
                    let Some(psr4) = value[section]["psr-4"].as_object() else {
                        continue;
                    };
                    for (prefix, target) in psr4 {
                        let Some(target) = target.as_str() else {
                            continue;
                        };
                        roots
                            .php
                            .insert(prefix.clone(), target.trim_end_matches('/').to_string());
                    }
                }
            } else {
                let options = &value["compilerOptions"];
                if let Some(base_url) = options["baseUrl"].as_str() {
                    roots.javascript_base_url = normalized_path(PathBuf::new(), base_url)
                        .map(|path| path.to_string_lossy().to_string());
                }
                if let Some(paths) = options["paths"].as_object() {
                    for (alias, targets) in paths {
                        let targets: Vec<String> = targets
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .filter_map(|target| normalized_path(PathBuf::new(), target))
                            .map(|path| path.to_string_lossy().to_string())
                            .collect();
                        if !targets.is_empty() {
                            roots.javascript_paths.insert(alias.clone(), targets);
                        }
                    }
                }
            }
        }
        if let Ok(source) = std::fs::read(repo_root.join("tests/Pest.php")) {
            for (class, directories) in crate::extraction::php::pest_bindings(&source) {
                for directory in directories {
                    if let Some(bound) = normalized_path(PathBuf::from("tests"), &directory) {
                        roots.php_test_cases.insert(bound.to_string_lossy().to_string(), class.clone());
                    }
                }
            }
        }
        (roots, manifests)
    }

    /// The test case class `$this` names in a Pest test written in `file`:
    /// the class bound to the nearest directory holding it.
    pub(super) fn test_case_of(&self, file: &str) -> Option<&str> {
        Path::new(file)
            .ancestors()
            .skip(1)
            .find_map(|directory| self.php_test_cases.get(&*directory.to_string_lossy()))
            .map(String::as_str)
    }

    /// Every listed `Cargo.toml` with a `[package]` is a crate. Its library
    /// root is the `[lib]` path or `src/lib.rs`, named by `[lib] name`, else
    /// by the package name with `-` read as `_`. Its binary roots are
    /// `src/main.rs`, `src/bin/*.rs`, `src/bin/*/main.rs`, each `[[bin]]`
    /// path, and `src/bin/<name>.rs` for a `[[bin]]` with no path. A root
    /// counts wherever its file exists. Its code reaches its own library by
    /// its name, and each key of `[dependencies]`, `[dev-dependencies]`,
    /// `[build-dependencies]`, and the same three under every `[target.*]`.
    /// Its workspace root is the nearest manifest holding `[workspace]`, else
    /// its own, and a `workspace = true` key reads its entry there. An entry
    /// with a `path` reaches the library of the package it names; any other
    /// reaches the library of the workspace root's `[patch]` entry whose
    /// source and package match its own, and none otherwise. An entry with no
    /// `path` that reaches no library is left out when its package is the
    /// name of a package of the repository, because a redirect tracer does
    /// not read may lead there, so it is never proven external. A left-out
    /// key keeps the library roots of the packages of that name. A key is named
    /// by the library it reaches, as Cargo names it, a left-out key by the
    /// library of each package of its name, unless a `package = "…"` rename or
    /// no library leaves it named by itself with `-` read as `_`.
    fn read_cargo_manifests(&mut self, repo_root: &Path, parsed: &CargoManifests) -> CargoManifests {
        let Some(listed) = crate::repo_files::tracked_files(repo_root, None) else {
            return CargoManifests::new();
        };
        let manifests: CargoManifests = listed
            .paths
            .iter()
            .zip(&listed.stamps)
            .filter(|(path, _)| path.rsplit('/').next() == Some("Cargo.toml"))
            .map(|(path, stamp)| {
                let manifest = match parsed.get(path) {
                    Some((known, manifest)) if known == stamp => manifest.clone(),
                    _ => std::fs::read_to_string(repo_root.join(path))
                        .ok()
                        .and_then(|text| toml::from_str(&text).ok())
                        .unwrap_or_default(),
                };
                (path.clone(), (stamp.clone(), manifest))
            })
            .collect();
        let exists: HashSet<&str> = listed.iter().map(String::as_str).collect();
        let mut automatic: HashMap<&str, Vec<&str>> = HashMap::new();
        for path in listed.iter() {
            let Some((directory, target)) = path.rsplit_once("src/bin/") else {
                continue;
            };
            let binary = match target.split_once('/') {
                None => target.ends_with(".rs"),
                Some((_, file)) => file == "main.rs",
            };
            if binary && (directory.is_empty() || directory.ends_with('/')) {
                automatic.entry(directory).or_default().push(path);
            }
        }
        let directories: Vec<(&str, &CargoManifest)> = manifests
            .iter()
            .map(|(path, (_, manifest))| (path.strip_suffix("Cargo.toml").unwrap_or_default(), manifest))
            .collect();
        let mut libraries: HashMap<&str, (String, String)> = HashMap::new();
        for &(directory, manifest) in &directories {
            let Some(package) = &manifest.package else {
                continue;
            };
            let declared = manifest.bin.iter().filter_map(|target| match (&target.path, &target.name) {
                (Some(path), _) => Some(format!("{directory}{path}")),
                (None, Some(name)) => Some(format!("{directory}src/bin/{name}.rs")),
                (None, None) => None,
            });
            let binaries: Vec<String> = std::iter::once(format!("{directory}src/main.rs"))
                .chain(declared)
                .chain(automatic.get(directory).into_iter().flatten().map(|path| path.to_string()))
                .collect();
            for binary in binaries {
                if exists.contains(binary.as_str()) {
                    self.rust_crates.insert(binary, None);
                }
            }
            let library = manifest.lib.as_ref();
            let library_path = format!(
                "{directory}{}",
                library.and_then(|lib| lib.path.as_deref()).unwrap_or("src/lib.rs")
            );
            if exists.contains(library_path.as_str()) {
                let name = library
                    .and_then(|lib| lib.name.clone())
                    .unwrap_or_else(|| package.name.replace('-', "_"));
                self.rust_crates.insert(library_path.clone(), Some(name.clone()));
                libraries.insert(directory, (library_path, name));
            }
        }
        let library_at = |base: &str, entry: &toml::Value| -> Option<&(String, String)> {
            let target = normalized_path(PathBuf::from(base), entry.get("path")?.as_str()?)?;
            let directory = match target.to_str()? {
                "" => String::new(),
                target => format!("{target}/"),
            };
            libraries.get(directory.as_str())
        };
        let packages: Vec<(&str, &CargoManifest, (&str, &CargoManifest))> = directories
            .iter()
            .filter(|(_, manifest)| manifest.package.is_some())
            .map(|&(directory, manifest)| {
                let root = directories
                    .iter()
                    .filter(|(root, holder)| directory.starts_with(root) && holder.workspace.is_some())
                    .max_by_key(|(root, _)| root.len())
                    .copied()
                    .unwrap_or((directory, manifest));
                (directory, manifest, root)
            })
            .collect();
        let mut repository_packages: HashMap<&str, Vec<&(String, String)>> = HashMap::new();
        for &(directory, manifest, _) in &packages {
            let Some(package) = &manifest.package else {
                continue;
            };
            repository_packages
                .entry(package.name.as_str())
                .or_default()
                .extend(libraries.get(directory));
        }
        for &(directory, manifest, (root_directory, root_manifest)) in &packages {
            let mut names: BTreeMap<String, Option<String>> = BTreeMap::new();
            let mut left_out: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let tables = std::iter::once(&manifest.dependencies)
                .chain(manifest.target.values())
                .flat_map(|table| [&table.dependencies, &table.dev_dependencies, &table.build_dependencies]);
            for (key, entry) in tables.flatten() {
                let (base, entry) = match entry.get("workspace").and_then(toml::Value::as_bool) {
                    Some(true) => (
                        root_directory,
                        root_manifest.workspace.as_ref().and_then(|shared| shared.dependencies.get(key)),
                    ),
                    _ => (directory, Some(entry)),
                };
                let package = entry
                    .and_then(|entry| entry.get("package"))
                    .and_then(toml::Value::as_str)
                    .unwrap_or(key);
                let library = entry.and_then(|entry| match entry.get("path") {
                    Some(_) => library_at(base, entry),
                    None => {
                        let source = canonical_source(
                            ["git", "registry"].into_iter().find_map(|field| entry.get(field)?.as_str()),
                        );
                        let patch = root_manifest
                            .patch
                            .iter()
                            .filter(|(patched, _)| canonical_source(Some(patched.as_str())) == source)
                            .flat_map(|(_, patches)| patches)
                            .find_map(|(name, patch)| {
                                let patched = patch.get("package").and_then(toml::Value::as_str).unwrap_or(name);
                                (patched == package).then_some(patch)
                            })?;
                        library_at(root_directory, patch)
                    }
                });
                let renamed = entry.is_some_and(|entry| entry.get("package").is_some());
                if let Some(reached) = repository_packages
                    .get(package)
                    .filter(|_| library.is_none() && entry.is_some_and(|entry| entry.get("path").is_none()))
                {
                    for (root, library) in reached {
                        let name = if renamed { key.replace('-', "_") } else { library.clone() };
                        let roots = left_out.entry(name).or_default();
                        if !roots.contains(root) {
                            roots.push(root.clone());
                        }
                    }
                    continue;
                }
                let name = match library {
                    Some((_, name)) if !renamed => name.clone(),
                    _ => key.replace('-', "_"),
                };
                let reached = names.entry(name).or_default();
                if reached.is_none() {
                    *reached = library.map(|(root, _)| root.clone());
                }
            }
            if let Some((root, name)) = libraries.get(directory) {
                names.entry(name.clone()).or_insert_with(|| Some(root.clone()));
            }
            self.rust_dependencies.insert(directory.to_string(), names);
            self.rust_left_out.insert(directory.to_string(), left_out);
        }
        manifests
    }

    pub(super) fn has_mapping(&self, language: Option<&str>) -> bool {
        match language {
            Some("php") => !self.php.is_empty(),
            Some("typescript") => {
                !self.javascript_paths.is_empty() || self.javascript_base_url.is_some()
            }
            Some("rust") => !self.rust_crates.is_empty(),
            _ => false,
        }
    }

    pub(super) fn has_same_mapping(&self, other: &Roots, language: Option<&str>) -> bool {
        match language {
            Some("php") => self.php == other.php,
            Some("typescript") => {
                self.javascript_paths == other.javascript_paths
                    && self.javascript_base_url == other.javascript_base_url
            }
            Some("rust") => {
                self.rust_crates == other.rust_crates
                    && self.resolving_names() == other.resolving_names()
                    && self.rust_left_out == other.rust_left_out
            }
            _ => true,
        }
    }

    /// The package names that move where a Rust path resolves: the names
    /// that reach a library of the repository. A new dependency from outside
    /// the repository moves none.
    fn resolving_names(&self) -> Vec<(&str, &str, &str)> {
        self.rust_dependencies
            .iter()
            .flat_map(|(directory, names)| {
                names
                    .iter()
                    .filter_map(move |(name, root)| Some((directory.as_str(), name.as_str(), root.as_deref()?)))
            })
            .collect()
    }
}

fn canonical_source(source: Option<&str>) -> String {
    let source = source.unwrap_or("crates-io");
    let source = source.strip_suffix('/').unwrap_or(source);
    let canonical = match source.split_once("://") {
        Some((scheme, address)) => {
            let (authority, path) = address.split_once('/').unwrap_or((address, ""));
            let (user, host) = authority.split_at(authority.rfind('@').map_or(0, |at| at + 1));
            let host = host.to_lowercase();
            let (scheme, path) = match host.split_once(':').map_or(host.as_str(), |(host, _)| host) {
                "github.com" => ("https".to_string(), path.to_lowercase()),
                _ => (scheme.to_lowercase(), path.to_string()),
            };
            format!("{scheme}://{user}{host}/{}", path.strip_suffix(".git").unwrap_or(&path))
        }
        None => source.to_string(),
    };
    match canonical.as_str() {
        "https://github.com/rust-lang/crates.io-index" => "crates-io".to_string(),
        _ => canonical,
    }
}
