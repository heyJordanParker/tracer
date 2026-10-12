//! The on-demand resolver: the declarations a name could be, and which of
//! them each use site reaches, with how surely.

use crate::extraction::{self, Declaration, ExtractionResult, RefShape};
use crate::file_facts::{self, FileFacts};
use crate::relations::lineage::{bound_this, overrides, Ancestry, ReturnTypes};
use crate::relations::modules::{
    admits, bound_paths, inline_module, named_module, path_after_binding, position_of, tail, walk_modules,
    ImportResolution, Module, ModulePaths,
};
use crate::relations::types::{type_scope, Scope, TypeOwners, TypePaths};
use crate::relations::{
    confidence_code, confidence_name, file_to_module, get, same_name, shape_matches, types_member_receiver, weaker,
    Candidate, Relations, CONFIDENCE_AMBIGUOUS, CONFIDENCE_EXTRACTED, CONFIDENCE_INFERRED,
};
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Every declaration of `name` in the file at `path`, as a candidate. A
/// candidate is file-local by its own visibility, never its parents': a
/// method of a file-local class is still called through an instance another
/// file holds. A Rust item without `pub` stays reachable, through `super::`
/// from a child module's file, and so does a member of a PHP trait, which
/// the classes using it take in.
fn candidates_in<'a>(path: &'a str, facts: &'a FileFacts, name: &'a str) -> impl Iterator<Item = Candidate> + 'a {
    facts.extraction.iter().flat_map(move |extraction| {
        extraction
            .declarations
            .iter()
            .filter(move |declaration| same_name(facts.language.as_deref(), &declaration.name, name))
            .map(move |declaration| Candidate {
                file: path.to_string(),
                language: facts.language.clone(),
                inline_module: inline_module(extraction, declaration.line, Some(declaration)),
                is_file_local: declaration.visibility == extraction::Visibility::FileLocal
                    && facts.language.as_deref() != Some("rust")
                    && !declaration.parent.and_then(|parent| extraction.declarations.get(parent as usize)).is_some_and(|parent| {
                        extraction::modifiers(&parent.header, parent.line - parent.header_line, &parent.name).contains(&"trait")
                    }),
                declaration: declaration.clone(),
            })
    })
}

/// One resolved use site: the file and line where the call is written, the
/// declaration it resolves to, and how sure the resolution is.
#[derive(Debug, Clone)]
pub struct UseSite {
    pub file: String,
    pub line: i64,
    /// The declaration the use site sits inside, when it sits inside one.
    /// This is the calling function a `callers` row names, and it is resolved
    /// from the referencing file's own declarations — the file is already
    /// loaded, so it costs nothing. `None` for a use site at module top
    /// level, whose row is the file itself.
    pub caller: Option<Declaration>,
    pub target_file: String,
    pub target: Declaration,
    pub confidence: &'static str,
}

/// Every declaration of `symbol`, loaded from the files the index names.
///
/// The index carries no payload, so kind, line, container and language come
/// from the declaring files' own entries — and those are exactly the files
/// the answer is about, so the load is the answer, not overhead.
///
/// `Class::method` and `Class.method` name one class's member: the member's
/// declarations, kept to the ones `qualifies` says the qualifier names.
pub fn declarations(relations: &Relations, symbol: &str) -> Vec<Candidate> {
    candidates(relations, &[symbol])
        .iter()
        .flat_map(Candidates::declarations)
        .cloned()
        .collect()
}

/// `Class::method` or `Class.method` as `(Some("Class"), "method")`; a bare
/// name as `(None, name)`. A namespaced qualifier keeps its last segment,
/// the way a declaration's `container` is written.
fn split_qualified(symbol: &str) -> (Option<&str>, &str) {
    let split = symbol
        .rfind("::")
        .map(|at| (at, 2))
        .or_else(|| symbol.rfind('.').map(|at| (at, 1)));
    match split {
        Some((at, width)) if at > 0 && at + width < symbol.len() => {
            let qualifier = &symbol[..at];
            let last = qualifier.rsplit(['\\', '.', ':']).next().unwrap_or(qualifier);
            (Some(last), &symbol[at + width..])
        }
        _ => (None, symbol),
    }
}

/// Whether a qualifier names this declaration: its container (a class, an
/// impl's type), or — for a declaration with no container — the file that
/// declares it, which is how `module.function` and `module::function` read.
fn qualifies(qualifier: Option<&str>, language: Option<&str>, file: &str, declaration: &Declaration) -> bool {
    let Some(qualifier) = qualifier else {
        return true;
    };
    match &declaration.container {
        Some(container) => container
            .rsplit(['\\', '.', ':'])
            .next()
            .is_some_and(|last| same_name(language, last, qualifier)),
        None => Path::new(file)
            .file_stem()
            .is_some_and(|stem| same_name(language, &stem.to_string_lossy(), qualifier)),
    }
}

/// Files whose module path's last segment is `name` — the module fallback
/// that makes `trace callers app` answer for `src/app.py`. Derived from the
/// file list, which is why it needs no stored module index.
pub fn modules_named(name: &str, repo_root: &Path) -> Vec<String> {
    let relations = get(repo_root);
    let wanted = name.to_lowercase();
    relations
        .files()
        .filter(|path| {
            Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase() == wanted)
                .unwrap_or(false)
        })
        .map(str::to_string)
        .collect()
}

/// One symbol's name, its qualifier, and every declaration a reference to
/// the name could resolve to, loaded once so `use_sites` can resolve the
/// symbol in one file and then everywhere without loading its declaring
/// files again.
pub struct Candidates {
    qualifier: Option<String>,
    name: String,
    candidates: Vec<Candidate>,
    declared_modules: HashSet<String>,
}

impl Candidates {
    /// The candidates the symbol's qualifier names, the declarations
    /// `Class::method` or `module.function` means; every candidate of a bare
    /// name.
    pub fn declarations(&self) -> impl Iterator<Item = &Candidate> {
        self.candidates.iter().filter(|candidate| {
            qualifies(
                self.qualifier.as_deref(),
                candidate.language.as_deref(),
                &candidate.file,
                &candidate.declaration,
            )
        })
    }
}

/// The `Candidates` of each of `symbols`, in their order, loaded from the
/// files the index names, each name's in the order the index names them.
pub fn candidates(relations: &Relations, symbols: &[&str]) -> Vec<Candidates> {
    let repo_root = relations.repo_root.as_path();
    let names: Vec<(Option<&str>, &str)> = symbols.iter().map(|symbol| split_qualified(symbol)).collect();
    let defined: Vec<Vec<&str>> = names.iter().map(|(_, name)| relations.defined_in(name).collect()).collect();
    let defining: Vec<&str> = defined.iter().flatten().copied().collect::<BTreeSet<&str>>().into_iter().collect();
    let mut loaded: Vec<Candidates> = names
        .iter()
        .map(|(qualifier, name)| Candidates {
            qualifier: qualifier.map(str::to_string),
            name: name.to_string(),
            candidates: Vec::new(),
            declared_modules: HashSet::new(),
        })
        .collect();
    for chunk in defining.chunks(file_facts::RESOLVE_CHUNK) {
        let declaring: Vec<PathBuf> = chunk.iter().map(|path| repo_root.join(*path)).collect();
        let facts = file_facts::extracted(&declaring, repo_root);
        for path in chunk {
            let Some(f) = facts.get(*path) else {
                continue;
            };
            for (named, _) in loaded.iter_mut().zip(&defined).filter(|(_, defined)| defined.contains(path)) {
                named.declared_modules.extend(module_names(f.extraction.as_ref()).map(str::to_string));
                named.candidates.extend(candidates_in(path, f, &named.name));
            }
        }
    }
    for (named, defined) in loaded.iter_mut().zip(&defined) {
        named.candidates.sort_by_cached_key(|candidate| defined.iter().position(|file| *file == candidate.file));
    }
    loaded
}

/// Every resolved use site of each of `names`, in their order, computed now
/// from the files the index names rather than read out of a stored edge list.
///
/// Resolution stays structural: candidates are restricted to the same
/// language as the use site, a free call resolves only to non-method
/// declarations, a static use resolves to the named class or its method, and
/// a member call resolves to methods. Member calls remain ambiguous when the
/// receiver type is unknown; free and static calls also remain ambiguous when
/// an ambiguous import legitimately names multiple candidates. Resolution
/// runs only over the files the index names for this name.
///
/// A Rust member call's stated receiver type narrows its candidates and
/// never empties them: a call it leaves with no candidate resolves as one
/// with no stated type, `AMBIGUOUS` against every method of its name. A Rust
/// path left to the name rule whose last segment names no type of a
/// candidate resolves as the free call of a module path, when its first
/// segment names a module of the repository, against only the candidates in
/// a module the path names.
///
/// A qualified `Class::method` resolves against every `method` like a bare
/// name — so a call stays ambiguous when it is — and keeps the sites whose
/// target the qualifier names.
pub fn use_sites(relations: &Relations, names: &[Candidates], in_file: Option<&str>) -> Vec<Vec<UseSite>> {
    let repo_root = relations.repo_root.as_path();
    let in_scope = |file: &&str| in_file.is_none_or(|in_file| in_file == *file);
    let mut wanted: Vec<&str> = names
        .iter()
        .filter(|named| !named.candidates.is_empty())
        .flat_map(|named| relations.used_in(&named.name).filter(in_scope))
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();
    let mut sites: Vec<Vec<UseSite>> = vec![Vec::new(); names.len()];
    if wanted.is_empty() {
        return sites;
    }

    let modules = ModulePaths::new(relations);
    let candidates: Vec<Vec<&Candidate>> = names.iter().map(|named| named.candidates.iter().collect()).collect();
    let crate_scoped = relations.roots.has_mapping(Some("rust"));
    let candidate_types: Vec<OnceLock<TypeOwners>> = names.iter().map(|_| OnceLock::new()).collect();

    // Each mentioning file resolves against the same candidate set and
    // nothing else, so the files run in parallel. On laravel-framework
    // `Collection` is named in over a thousand files, and resolving them one
    // after another was the whole cost of the command.
    //
    // A chunk at a time, like every other whole-repository walk: a name used
    // in thousands of files held every one of their extractions at once, and
    // a use site is finished the moment its file is resolved.
    wanted.sort();
    let php = Some("php");
    let mut return_types = ReturnTypes::default();
    let ancestry = &Ancestry::new(relations);
    let lineages: Vec<OnceLock<Vec<Vec<(String, Vec<&str>)>>>> = names.iter().map(|_| OnceLock::new()).collect();
    for chunk in wanted.chunks(file_facts::RESOLVE_CHUNK) {
        let needed: Vec<PathBuf> = chunk.iter().map(|p| repo_root.join(*p)).collect();
        let facts = file_facts::extracted(&needed, repo_root);
        let receivers: Vec<String> = facts
            .iter()
            .filter(|(_, f)| f.language.as_deref() == php)
            .flat_map(|(path, f)| {
                let test_case = relations.roots.test_case_of(path);
                f.extraction
                    .iter()
                    .flat_map(|extraction| extraction.references.iter())
                    .filter(|reference| names.iter().any(|named| same_name(php, &reference.name, &named.name)))
                    .filter_map(move |reference| {
                        bound_this(reference, test_case).map_or_else(|| reference.receiver.clone(), |bound| bound.receiver)
                    })
            })
            .filter(|receiver| receiver.contains("->"))
            .collect();
        let chains: Vec<&str> = receivers.iter().map(String::as_str).collect();
        return_types.read(&chains, relations, &modules, repo_root);
        let return_types = &return_types;
        let resolved: Vec<Vec<Vec<UseSite>>> = chunk
            .par_iter()
            .map(|path| {
                let mut outs: Vec<Vec<UseSite>> = vec![Vec::new(); names.len()];
                let Some(f) = facts.get(*path) else {
                    return outs;
                };
                let Some(extraction) = &f.extraction else {
                    return outs;
                };
                let import_resolutions: Vec<ImportResolution<'_>> = extraction
                    .imports
                    .iter()
                    .map(|import| {
                        relations.resolve_import(path, import, f.language.as_deref(), &modules, &extraction.imports)
                    })
                    .collect();
                let imported: HashMap<String, &'static str> = import_resolutions
                    .iter()
                    .flat_map(|resolution| resolution.targets.iter())
                    .fold(HashMap::new(), |mut imported, (target, confidence, _)| {
                        let confidence = confidence_name(confidence_code(confidence));
                        imported
                            .entry(target.clone())
                            .and_modify(|current| {
                                if confidence_code(confidence) < confidence_code(current) {
                                    *current = confidence;
                                }
                            })
                            .or_insert(confidence);
                        imported
                    });
                let mut type_paths = TypePaths::new();
                let is_php = f.language.as_deref() == php;
                let test_case = relations.roots.test_case_of(path);
                for (index, reference) in names.iter().enumerate().flat_map(|(index, named)| {
                    extraction
                        .references
                        .iter()
                        .filter(move |r| same_name(f.language.as_deref(), &r.name, &named.name))
                        .map(move |reference| (index, reference))
                }) {
                    let out = &mut outs[index];
                    let candidates = &candidates[index];
                    let candidate_types = &candidate_types[index];
                    let declared_modules = &names[index].declared_modules;
                    let rust_scoped = crate_scoped && f.language.as_deref() == Some("rust");
                    let bound_outside = |reference: &extraction::Reference| {
                        import_resolutions.iter().zip(&extraction.imports).any(|(resolution, import)| {
                            resolution
                                .invalid_bindings
                                .iter()
                                .any(|binding| reference_uses_binding(f.language.as_deref(), reference, binding))
                                && import.in_force(reference.line, &extraction.imports)
                        })
                    };
                    let bound = is_php.then(|| bound_this(reference, test_case)).flatten();
                    let reference = bound.as_ref().unwrap_or(reference);
                    if (!rust_scoped || reference.shape == RefShape::Free) && bound_outside(reference) {
                        continue;
                    }
                    let returned = is_php
                        .then(|| return_types.stated(reference, bound_outside, candidates, ancestry))
                        .flatten();
                    let reference = returned.as_ref().unwrap_or(reference);
                    let typed = (is_php && reference.shape == RefShape::Member)
                        .then_some(reference.receiver.as_deref())
                        .flatten()
                        .map(|class| (class.to_string(), ancestry.named_in(path, class, &extraction.imports)));
                    let all_candidates = candidates;
                    let inherited =
                        is_php.then(|| ancestry.inherited(path, &extraction.imports, reference, candidates)).flatten();
                    let (reference, candidates) = match &inherited {
                        Some((inherited, reached)) => (inherited, reached),
                        None => (reference, candidates),
                    };
                    let imports: Vec<extraction::Import> = extraction
                        .imports
                        .iter()
                        .filter(|import| import.in_force(reference.line, &extraction.imports))
                        .cloned()
                        .collect();
                    if let Some(scope) = rust_scoped
                        .then(|| {
                            let inline_module = inline_module(extraction, reference.line, None);
                            crate_scope(
                                reference,
                                path,
                                inline_module.as_deref(),
                                &imports,
                                &modules,
                                relations,
                                candidates,
                                candidate_types,
                                &mut type_paths,
                            )
                        })
                        .flatten()
                    {
                        let before = out.len();
                        let matched = resolve_one(
                            &scope.candidates,
                            |_| true,
                            path,
                            f,
                            extraction,
                            &scope.reference,
                            reference,
                            &scope.reached,
                            out,
                        );
                        cap(&mut out[before..], scope.ceiling);
                        if matched || (scope.external && reference.shape != RefShape::Member) {
                            continue;
                        }
                    }
                    let before = out.len();
                    let language = f.language.as_deref();
                    let in_calling_file: Vec<&Candidate> = candidates
                        .iter()
                        .copied()
                        .filter(|candidate| {
                            !rust_scoped
                                && reference.shape == RefShape::Free
                                && candidate.file == *path
                                && shape_matches(candidate, language, RefShape::Free, None)
                        })
                        .collect();
                    let preferred = if in_calling_file.len() == 1 { &in_calling_file } else { candidates };
                    let matched =
                        resolve_one(preferred, |_| true, path, f, extraction, reference, reference, &imported, out);
                    let without_receiver = match reference.shape {
                        RefShape::Member if untyped_member_is_ambiguous(language) => Some(RefShape::Member),
                        RefShape::Member if inside_receiver_type(extraction, reference, language) => {
                            Some(RefShape::Member)
                        }
                        RefShape::Static
                            if paths_name_modules(language)
                                && reference.receiver.as_deref().is_some_and(|receiver| {
                                    starts_at_module(receiver, declared_modules, extraction, &modules)
                                }) =>
                        {
                            Some(RefShape::Free)
                        }
                        _ => None,
                    };
                    if let Some(shape) = without_receiver.filter(|_| !matched && reference.receiver.is_some()) {
                        let written = reference.receiver.as_deref().unwrap_or_default();
                        let named_modules: Vec<(Vec<String>, Vec<String>)> = if crate_scoped && shape == RefShape::Free {
                            let bound = path_after_binding(written, &imports);
                            let paths = if bound.is_empty() { vec![written.to_string()] } else { bound };
                            paths.iter().map(|bound| modules.rust_path(bound, path, &imports)).collect()
                        } else {
                            vec![(Vec::new(), Vec::new())]
                        };
                        let named = |candidate: &Candidate| {
                            shape != RefShape::Free
                                || named_modules.iter().any(|(named_files, rest)| {
                                    in_named_module(candidate, written, named_files, rest, relations)
                                })
                        };
                        let from_root = shape == RefShape::Free
                            && matches!(
                                written.split("::").find(|segment| !segment.is_empty()),
                                Some("crate" | "self" | "super")
                            );
                        let admitted: Vec<&Candidate> =
                            candidates.iter().copied().filter(|candidate| from_root || named(candidate)).collect();
                        let retry = extraction::Reference {
                            shape,
                            receiver: None,
                            ..reference.clone()
                        };
                        resolve_one(&admitted, named, path, f, extraction, &retry, reference, &imported, out);
                    }
                    if rust_scoped {
                        cap(&mut out[before..], CONFIDENCE_INFERRED);
                    }
                    if let Some((class, declaring)) = typed {
                        let caller = caller_at(extraction, reference.line);
                        let lineages = lineages[index].get_or_init(|| ancestry.lineages(all_candidates));
                        for candidate in overrides(&class, &declaring, all_candidates, lineages) {
                            if !is_recursion(path, caller.as_ref(), candidate) {
                                out.push(UseSite {
                                    file: path.to_string(),
                                    line: reference.line,
                                    caller: caller.clone(),
                                    target_file: candidate.file.clone(),
                                    target: candidate.declaration.clone(),
                                    confidence: CONFIDENCE_AMBIGUOUS,
                                });
                            }
                        }
                    }
                }
                for out in &mut outs {
                    out.sort_by(|a, b| {
                        (a.line, &a.target_file, a.target.line, &a.target.name, confidence_code(a.confidence)).cmp(&(
                            b.line,
                            &b.target_file,
                            b.target.line,
                            &b.target.name,
                            confidence_code(b.confidence),
                        ))
                    });
                    out.dedup_by(|later, kept| {
                        (later.line, &later.target_file, later.target.line, &later.target.name)
                            == (kept.line, &kept.target_file, kept.target.line, &kept.target.name)
                    });
                }
                outs
            })
            .collect();
        for outs in resolved {
            for (sites, out) in sites.iter_mut().zip(outs) {
                sites.extend(out);
            }
        }
    }
    for (sites, named) in sites.iter_mut().zip(names) {
        sites.retain(|site| {
            qualifies(named.qualifier.as_deref(), relations.language(&site.target_file), &site.target_file, &site.target)
        });
    }
    sites
}

fn reference_uses_binding(language: Option<&str>, reference: &extraction::Reference, binding: &str) -> bool {
    match reference.shape {
        RefShape::Free => same_name(language, &reference.name, binding),
        RefShape::Member | RefShape::Static => reference
            .receiver
            .as_deref()
            .is_some_and(|receiver| same_name(language, receiver, binding)),
    }
}

/// The innermost declaration of `extraction` whose span holds `line`.
fn caller_at(extraction: &ExtractionResult, line: i64) -> Option<Declaration> {
    extraction::innermost(
        extraction
            .declarations
            .iter()
            .map(|declaration| (declaration.header_line, declaration.end_line)),
        line,
    )
    .map(|index| extraction.declarations[index].clone())
}

/// A call whose calling declaration is the candidate itself is recursion and
/// adds no caller.
fn is_recursion(path: &str, caller: Option<&Declaration>, candidate: &Candidate) -> bool {
    candidate.file == path
        && caller.is_some_and(|caller| {
            caller.line == candidate.declaration.line && caller.name == candidate.declaration.name
        })
}

/// A Rust reference as its crate reads it, or `None`, which leaves it to
/// the name rule: a member call with no stated receiver type. A path names
/// a module, where the call is a free call, or a type, where the call's
/// candidates are the methods whose `Self` resolves to the type the path
/// resolves to, in any file of the repository, by [`resolve_type`]. The
/// path's first segment may be a name each `use` of `imports`, the imports
/// in force at the reference, binds, or a module a `*` imports. A path whose
/// first segment is a name the file's package leaves out and whose rest
/// names a module of that library reaches every file of the libraries of the
/// packages of that name, each `INFERRED`.
#[allow(clippy::too_many_arguments)]
fn crate_scope<'a>(
    reference: &extraction::Reference,
    file: &str,
    inline_module: Option<&str>,
    imports: &[extraction::Import],
    modules: &ModulePaths,
    relations: &Relations,
    candidates: &[&'a Candidate],
    candidate_types: &OnceLock<TypeOwners>,
    type_paths: &mut TypePaths,
) -> Option<Scope<'a>> {
    let call_site = (
        file.to_string(),
        inline_module.map_or(Module::TopLevel, |name| Module::Inline(name.to_string())),
        true,
        reference.name.clone(),
    );
    let free = extraction::Reference {
        shape: RefShape::Free,
        receiver: None,
        ..reference.clone()
    };
    let starts = match reference.receiver.as_deref() {
        None if reference.shape == RefShape::Member => return None,
        None => vec![call_site],
        Some(written) => {
            let paths = bound_paths(written, imports);
            let modules_named: Vec<(String, Module, bool, String)> = paths
                .iter()
                .flat_map(|path| {
                    let (files, rest) = modules.rust_path(path, file, imports);
                    files.into_iter().map(move |target| position_of(path, file, target, &rest, relations))
                })
                .filter(|(_, _, type_path)| !type_path)
                .map(|(target, module, _)| (target, module, false, reference.name.clone()))
                .collect();
            if modules_named.is_empty() {
                let left_out: Vec<&String> = paths
                    .iter()
                    .filter_map(|path| modules.left_out_crates(path, file).map(|libraries| (path, libraries)))
                    .filter(|(path, libraries)| {
                        libraries.iter().any(|library| modules.rust_path_within(path, library).1.is_empty())
                    })
                    .flat_map(|(_, libraries)| libraries)
                    .collect();
                if left_out.is_empty() {
                    return Some(type_scope(
                        written,
                        reference,
                        file,
                        inline_module,
                        imports,
                        modules,
                        relations,
                        candidates,
                        candidate_types,
                        type_paths,
                    ));
                }
                let reached: HashMap<String, &'static str> = left_out
                    .into_iter()
                    .flat_map(|library| modules.crate_files(library))
                    .map(|member| (member, CONFIDENCE_INFERRED))
                    .collect();
                return Some(Scope {
                    candidates: candidates.iter().copied().filter(|candidate| reached.contains_key(&candidate.file)).collect(),
                    reference: free,
                    reached,
                    ceiling: CONFIDENCE_EXTRACTED,
                    external: false,
                });
            }
            modules_named
        }
    };
    let declares = |path: &str, module: &Module, name: &str| {
        candidates.iter().any(|candidate| {
            candidate.file == path
                && same_name(Some("rust"), &candidate.declaration.name, name)
                && admits(module, candidate)
                && shape_matches(candidate, Some("rust"), RefShape::Free, None)
        })
    };
    let walk = walk_modules(starts, imports, declares, modules, relations)?;
    Some(Scope {
        candidates: candidates
            .iter()
            .copied()
            .filter(|candidate| {
                walk.positions.iter().any(|(path, module, name)| {
                    *path == candidate.file
                        && admits(module, candidate)
                        && same_name(Some("rust"), &candidate.declaration.name, name)
                })
            })
            .collect(),
        reference: free,
        external: walk.external(),
        reached: walk.reached,
        ceiling: CONFIDENCE_EXTRACTED,
    })
}

/// Lowers every site of `sites` to at most `ceiling`.
fn cap(sites: &mut [UseSite], ceiling: &'static str) {
    for site in sites {
        site.confidence = weaker(site.confidence, ceiling);
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_one(
    candidates: &[&Candidate],
    proven: impl Fn(&Candidate) -> bool,
    path: &str,
    facts: &FileFacts,
    extraction: &ExtractionResult,
    reference: &extraction::Reference,
    written: &extraction::Reference,
    imported: &HashMap<String, &'static str>,
    out: &mut Vec<UseSite>,
) -> bool {
    let matching: Vec<&Candidate> = candidates
        .iter()
        .copied()
        .filter(|c| {
            shape_matches(
                c,
                facts.language.as_deref(),
                reference.shape,
                reference.receiver.as_deref(),
            ) && (!c.is_file_local || c.file == path)
        })
        .collect();
    if matching.is_empty() {
        return false;
    }
    let caller = caller_at(extraction, reference.line);
    let site = |candidate: &Candidate, confidence| UseSite {
        file: path.to_string(),
        line: reference.line,
        caller: caller.clone(),
        target_file: candidate.file.clone(),
        target: candidate.declaration.clone(),
        confidence,
    };
    if reference.shape == RefShape::Member
        && reference.receiver.is_none()
        && untyped_member_is_ambiguous(facts.language.as_deref())
    {
        for candidate in matching {
            out.push(site(candidate, CONFIDENCE_AMBIGUOUS));
        }
        return true;
    }
    if matching.len() == 1 {
        let candidate = matching[0];
        if is_recursion(path, caller.as_ref(), candidate) {
            return true;
        }
        let has_value_receiver = written.shape == RefShape::Member
            && match written.receiver.as_deref() {
                None => types_member_receiver(facts.language.as_deref()),
                Some(receiver) => {
                    let named = receiver.rsplit(['\\', '.', ':']).next().unwrap_or(receiver);
                    !types_member_receiver(facts.language.as_deref())
                        && !is_self_receiver(receiver)
                        && !candidate
                            .declaration
                            .container
                            .as_deref()
                            .is_some_and(|container| same_name(facts.language.as_deref(), container, named))
                }
            };
        let confidence = if !proven(candidate) {
            CONFIDENCE_INFERRED
        } else if candidate.file == path && !has_value_receiver {
            CONFIDENCE_EXTRACTED
        } else {
            imported.get(&candidate.file).copied().unwrap_or(if has_value_receiver {
                CONFIDENCE_AMBIGUOUS
            } else {
                CONFIDENCE_INFERRED
            })
        };
        out.push(site(candidate, confidence));
        return true;
    }
    // Import context narrows a multi-candidate match. One imported candidate
    // keeps the import edge's confidence; several are all legitimate targets
    // and therefore remain ambiguous.
    let in_imports: Vec<(&Candidate, &str)> = matching
        .iter()
        .filter(|candidate| proven(candidate))
        .filter_map(|candidate| {
            imported
                .get(&candidate.file)
                .map(|confidence| (*candidate, *confidence))
        })
        .collect();
    if in_imports.len() == 1 {
        out.push(site(in_imports[0].0, in_imports[0].1));
        return true;
    }
    if in_imports.len() > 1 {
        for (candidate, _) in in_imports {
            out.push(site(candidate, CONFIDENCE_AMBIGUOUS));
        }
        return true;
    }
    // Only a member call may stay ambiguous: its receiver type is not named,
    // so several same-language methods genuinely could be the target. A free
    // or static call names its target exactly, so an unresolved one is name
    // coincidence, and fanning out would reintroduce the noise this model
    // removes.
    if reference.shape == RefShape::Member {
        for candidate in matching {
            out.push(site(candidate, CONFIDENCE_AMBIGUOUS));
        }
    }
    true
}

/// Receivers that name the object the call is written in, in every language
/// whose member call carries the receiver's text, so a call on one is not a
/// call on a value.
fn is_self_receiver(receiver: &str) -> bool {
    matches!(receiver, "this" | "self" | "super" | "super()" | "cls")
}

/// Languages whose method call reaches the trait methods of every type in
/// scope, the standard library's and other crates' included, so a call with
/// no stated receiver type has no in-repository method known to be its
/// target.
fn untyped_member_is_ambiguous(language: Option<&str>) -> bool {
    matches!(language, Some("rust"))
}

/// Languages whose path call `a::name()` names a module or a type with one
/// syntax, so a path that names no type is the free call of a module path.
fn paths_name_modules(language: Option<&str>) -> bool {
    matches!(language, Some("rust"))
}

/// Whether a Rust path's first segment names a module of the repository:
/// `crate`, `self`, or `super`, a `mod` the calling file or a candidate's
/// file declares, or a module file of that name.
fn starts_at_module(path: &str, declared: &HashSet<String>, extraction: &ExtractionResult, modules: &ModulePaths) -> bool {
    let Some(first) = path.split("::").find(|segment| !segment.is_empty()) else {
        return false;
    };
    matches!(first, "crate" | "self" | "super")
        || declared.contains(first)
        || module_names(Some(extraction)).any(|name| name == first)
        || modules.has_module_file(first)
}

fn inside_receiver_type(extraction: &ExtractionResult, reference: &extraction::Reference, language: Option<&str>) -> bool {
    reference.receiver.as_deref().is_some_and(|receiver| {
        extraction.declarations.iter().any(|declaration| {
            matches!(declaration.kind.as_str(), "class" | "interface" | "trait" | "enum" | "struct")
                && same_name(language, &declaration.name, receiver)
                && (declaration.header_line..=declaration.end_line).contains(&reference.line)
        })
    })
}

/// Whether `candidate` sits in a module the Rust module path `path` names.
/// `named_files` are the module files `rust_path` resolves the path to, and
/// `rest` the segments past them. A candidate in those files is in the
/// module when [`admits`] accepts it at the module [`named_module`] reads
/// from `rest` in its file, the rule `crate_scope`'s positions read, so a
/// path that names a type admits no free function and a path that names
/// only the file admits its top level. For a path that names no file, the
/// path's last segment is the candidate's module: its enclosing inline
/// `mod`, else its file, named by its stem or, for a `mod.rs`, its
/// directory.
fn in_named_module(
    candidate: &Candidate,
    path: &str,
    named_files: &[String],
    rest: &[String],
    relations: &Relations,
) -> bool {
    if !named_files.is_empty() {
        return named_files.contains(&candidate.file)
            && named_module(&candidate.file, rest, relations).is_some_and(|module| admits(&module, candidate));
    }
    let declaring = file_to_module(&candidate.file, Some("rust"));
    let module = candidate
        .inline_module
        .as_deref()
        .unwrap_or_else(|| tail(declaring.strip_suffix("/mod").unwrap_or(&declaring)));
    module == tail(path)
}

/// The names of the `mod` items a file declares, inline or not.
fn module_names(extraction: Option<&ExtractionResult>) -> impl Iterator<Item = &str> {
    extraction
        .into_iter()
        .flat_map(|extraction| extraction.declarations.iter())
        .filter(|declaration| declaration.kind == "module")
        .map(|declaration| declaration.name.as_str())
}
