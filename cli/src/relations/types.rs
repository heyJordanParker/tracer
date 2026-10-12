//! The identity of a Rust type, so a type path's candidates are the methods
//! whose `Self` is the same type wherever it is written.

use crate::extraction::{self, RefShape};
use crate::relations::modules::{bound_paths, in_module, position_of, tail, walk_modules, Module, ModulePaths};
use crate::relations::{
    confidence_code, weaker, Candidate, Relations, CONFIDENCE_AMBIGUOUS, CONFIDENCE_EXTRACTED, CONFIDENCE_INFERRED,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

/// A Rust reference's scope: the reference as its crate reads it, the
/// `candidates` in its scope, the files they sit in, each with the
/// confidence it is reached by, the strongest confidence the scope proves,
/// and whether the path is proven to live outside the repository, which
/// leaves a reference the scope cannot place unresolved.
pub(super) struct Scope<'a> {
    pub(super) reference: extraction::Reference,
    pub(super) candidates: Vec<&'a Candidate>,
    pub(super) reached: HashMap<String, &'static str>,
    pub(super) ceiling: &'static str,
    pub(super) external: bool,
}

/// One identity a Rust type can have: a declaration of the repository, a
/// module position the walk reached and found no declaration in, with the
/// name it looked for there, or a type outside the repository, by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum TypeKey {
    Declared(String, String),
    Unreached(String, Module, String),
    Outside(String),
}

/// The identities a type path names, each with the confidence it is
/// reached by. An `Outside` identity is never more than `AMBIGUOUS`.
type ResolvedType = HashMap<TypeKey, &'static str>;

/// Each identity the candidates' own types name, with every candidate that
/// has it, by its position, and the confidence it has it at.
pub(super) type TypeOwners = HashMap<TypeKey, Vec<(usize, &'static str)>>;

/// The type paths one file resolved, each by the inline `mod` it is written
/// in, its text, and the imports in force there, with what [`resolve_type`]
/// returned for it.
pub(super) type TypePaths = HashMap<(Option<String>, String, Vec<extraction::Import>), (ResolvedType, bool)>;

/// The scope of a Rust call whose path names a type: the `candidates` whose
/// own type, by [`own_types`], shares an identity with the type the path
/// `written` resolves to where the reference is, each sitting in a file
/// reached by the weaker of the two sides' confidence. A type outside the
/// repository proves no target, so its candidates are at most `AMBIGUOUS`.
/// The path resolves once per file, read back from `type_paths` after that.
#[allow(clippy::too_many_arguments)]
pub(super) fn type_scope<'a>(
    written: &str,
    reference: &extraction::Reference,
    file: &str,
    inline_module: Option<&str>,
    imports: &[extraction::Import],
    modules: &ModulePaths,
    relations: &Relations,
    candidates: &[&'a Candidate],
    candidate_types: &OnceLock<TypeOwners>,
    type_paths: &mut TypePaths,
) -> Scope<'a> {
    let (resolved, external) = type_paths
        .entry((inline_module.map(str::to_string), written.to_string(), imports.to_vec()))
        .or_insert_with(|| {
            crate::timing::phase("resolve_type", || resolve_type(written, file, inline_module, imports, modules, relations))
        });
    let owners = candidate_types.get_or_init(|| crate::timing::phase("own_types", || own_types(candidates, modules, relations)));
    let mut matched: BTreeMap<usize, &'static str> = BTreeMap::new();
    for (key, ours) in resolved.iter() {
        for (index, theirs) in owners.get(key).into_iter().flatten() {
            let confidence = weaker(ours, theirs);
            let held = matched.entry(*index).or_insert(confidence);
            if confidence_code(confidence) < confidence_code(held) {
                *held = confidence;
            }
        }
    }
    let mut reached: HashMap<String, &'static str> = HashMap::new();
    let mut in_scope = Vec::new();
    for (index, confidence) in matched {
        let candidate = candidates[index];
        let held = reached.entry(candidate.file.clone()).or_insert(confidence);
        if confidence_code(confidence) < confidence_code(held) {
            *held = confidence;
        }
        in_scope.push(candidate);
    }
    let ceiling = reached
        .values()
        .copied()
        .min_by_key(|confidence| confidence_code(confidence))
        .unwrap_or(CONFIDENCE_EXTRACTED);
    Scope {
        reference: extraction::Reference {
            shape: RefShape::Static,
            receiver: None,
            ..reference.clone()
        },
        candidates: in_scope,
        reached,
        ceiling,
        external: *external,
    }
}

/// The type a Rust type path `written` names where it is written, in
/// `file` at `inline_module` with `imports` in force, and whether the path
/// is proven to live outside the repository. A one-segment path walks from
/// where it is written, through the `use` items in force there, which a
/// `type` alias is one of; a longer one starts in the files its path names,
/// through each `use` that binds its first segment. The walk stops at a
/// position that declares the type by [`Relations::types_of`], and the
/// type's identities are those declarations. A walk that reaches none names
/// the module positions past a hop that it reached and found nothing in;
/// a walk that a hop proves external names a type outside the repository by
/// the name that hop leads to, and a walk that reaches neither by the path's
/// last segment.
fn resolve_type(
    written: &str,
    file: &str,
    inline_module: Option<&str>,
    imports: &[extraction::Import],
    modules: &ModulePaths,
    relations: &Relations,
) -> (ResolvedType, bool) {
    let mut outside_names = Vec::new();
    let mut inferred: HashSet<String> = HashSet::new();
    let mut starts = Vec::new();
    if written.contains("::") {
        for path in bound_paths(written, imports) {
            let (files, rest) = modules.rust_item_path(&path, file, imports);
            let reached: Vec<(Vec<String>, Vec<String>)> = if !files.is_empty() {
                vec![(files, rest)]
            } else if let Some(libraries) = modules.left_out_crates(&path, file) {
                inferred.extend(libraries.iter().flat_map(|library| modules.crate_files(library)));
                libraries.iter().map(|library| modules.rust_path_within(&path, library)).collect()
            } else {
                if modules.is_external(&path, file) {
                    outside_names.push(tail(&path).to_string());
                }
                Vec::new()
            };
            for (files, rest) in reached {
                for target in files {
                    let (target, module, _) = position_of(&path, file, target, &rest, relations);
                    starts.push((target, module, false, tail(&path).to_string()));
                }
            }
        }
    } else {
        starts.push((
            file.to_string(),
            inline_module.map_or(Module::TopLevel, |name| Module::Inline(name.to_string())),
            true,
            written.to_string(),
        ));
    }
    let declares = |path: &str, module: &Module, name: &str| {
        relations
            .types_of(path)
            .iter()
            .any(|(declared, inline_module)| declared == name && in_module(module, inline_module.as_deref()))
    };
    let outside = |names: Vec<String>| -> ResolvedType {
        let names = if names.is_empty() { vec![tail(written).to_string()] } else { names };
        names.into_iter().map(|name| (TypeKey::Outside(name), CONFIDENCE_AMBIGUOUS)).collect()
    };
    let Some(walk) = walk_modules(starts, imports, declares, modules, relations) else {
        let external = !outside_names.is_empty();
        return (outside(outside_names), external);
    };
    let declared: ResolvedType = walk
        .positions
        .iter()
        .filter(|(path, module, name)| declares(path, module, name))
        .map(|(path, _, name)| {
            let confidence = match inferred.contains(path) {
                true => CONFIDENCE_INFERRED,
                false => walk.reached.get(path).copied().unwrap_or(CONFIDENCE_EXTRACTED),
            };
            (TypeKey::Declared(path.clone(), name.clone()), confidence)
        })
        .collect();
    let external = !outside_names.is_empty() || walk.external();
    outside_names.extend(walk.outside);
    if !declared.is_empty() {
        return (declared, external);
    }
    if external || walk.dead_ends.is_empty() {
        return (outside(outside_names), external);
    }
    let unreached = walk
        .dead_ends
        .into_iter()
        .map(|(path, module, name)| (TypeKey::Unreached(path, module, name), CONFIDENCE_EXTRACTED))
        .collect();
    (unreached, external)
}

/// Each candidate's own type, the way [`resolve_type`] reads a type path
/// where the candidate is declared: a Rust method's `Self` and a Rust type
/// itself, and none for anything else, keyed by identity, so a type path
/// finds its candidates by its own identities. It runs inside the parallel
/// resolve of a `OnceLock`, so it runs on one thread: a rayon join here could
/// steal a task that waits on the same `OnceLock`.
pub(super) fn own_types(candidates: &[&Candidate], modules: &ModulePaths, relations: &Relations) -> TypeOwners {
    let mut owners = TypeOwners::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let declaration = &candidate.declaration;
        if candidate.language.as_deref() != Some("rust") {
            continue;
        }
        let own = if matches!(declaration.kind.as_str(), "class" | "enum" | "interface") {
            HashMap::from([(TypeKey::Declared(candidate.file.clone(), declaration.name.clone()), CONFIDENCE_EXTRACTED)])
        } else if let Some(self_type) = &declaration.self_type {
            let rows = relations.imports_of(&candidate.file);
            let imports: Vec<extraction::Import> =
                rows.iter().filter(|import| import.in_force(declaration.line, rows)).cloned().collect();
            resolve_type(self_type, &candidate.file, candidate.inline_module.as_deref(), &imports, modules, relations).0
        } else {
            continue;
        };
        for (key, confidence) in own {
            owners.entry(key).or_default().push((index, confidence));
        }
    }
    owners
}
