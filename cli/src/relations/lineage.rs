//! What a PHP call on a class can reach: the class's lineage, the return
//! types its methods state, and the test case `$this` names in a Pest test.

use crate::extraction::{self, RefShape};
use crate::file_facts::{self, FileFacts};
use crate::relations::modules::ModulePaths;
use crate::relations::{same_name, shape_matches, Candidate, Relations};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(super) struct ReturnTypes(HashMap<(String, String), Option<String>>);

impl ReturnTypes {
    pub(super) fn stated(
        &self,
        reference: &extraction::Reference,
        bound_outside: impl Fn(&extraction::Reference) -> bool,
        candidates: &[&Candidate],
        ancestry: &Ancestry,
    ) -> Option<extraction::Reference> {
        let receiver = reference.receiver.as_deref().filter(|receiver| receiver.contains("->"))?;
        let first = extraction::Reference {
            receiver: receiver.split("->").next().map(str::to_string),
            ..reference.clone()
        };
        let class = if bound_outside(&first) {
            None
        } else {
            self.class_of(receiver).ok().flatten()
        }
        .filter(|class| {
            class_declares(candidates, class)
                || ancestry
                    .ancestors(class, &ancestry.relations.defined_in(class).collect::<Vec<&str>>())
                    .iter()
                    .any(|(ancestor, _)| class_declares(candidates, ancestor))
        });
        Some(extraction::Reference {
            receiver: class,
            ..reference.clone()
        })
    }

    fn class_of(&self, receiver: &str) -> Result<Option<String>, (String, String)> {
        let mut steps = receiver.split("->");
        let mut class = steps.next().unwrap_or_default().to_string();
        for step in steps {
            let method = step.trim_end_matches("()");
            match self.0.get(&(class.to_ascii_lowercase(), method.to_ascii_lowercase())) {
                Some(Some(returned)) => class = returned.clone(),
                Some(None) => return Ok(None),
                None => return Err((class, method.to_string())),
            }
        }
        Ok(Some(class))
    }

    pub(super) fn read(&mut self, receivers: &[&str], relations: &Relations, modules: &ModulePaths, repo_root: &Path) {
        loop {
            let unread: BTreeSet<(String, String)> =
                receivers.iter().filter_map(|receiver| self.class_of(receiver).err()).collect();
            if unread.is_empty() {
                return;
            }
            let declaring: Vec<PathBuf> = unread
                .iter()
                .flat_map(|(_, method)| relations.defined_in(method))
                .collect::<BTreeSet<&str>>()
                .into_iter()
                .map(|path| repo_root.join(path))
                .collect();
            let facts = file_facts::extracted(&declaring, repo_root);
            for (class, method) in unread {
                let returned = declared_return(&class, &method, &facts, relations, modules);
                self.0.insert((class.to_ascii_lowercase(), method.to_ascii_lowercase()), returned);
            }
        }
    }
}

fn declared_return(
    class: &str,
    method: &str,
    facts: &HashMap<String, FileFacts>,
    relations: &Relations,
    modules: &ModulePaths,
) -> Option<String> {
    let php = Some("php");
    let mut returned = facts
        .iter()
        .filter(|(_, f)| f.language.as_deref() == php)
        .filter_map(|(path, f)| f.extraction.as_ref().map(|extracted| (path, extracted)))
        .flat_map(|(path, extracted)| {
            extracted
                .declarations
                .iter()
                .filter(|declaration| {
                    declaration.kind == "function"
                        && same_name(php, &declaration.name, method)
                        && declaration.container.as_deref().is_some_and(|container| same_name(php, container, class))
                })
                .map(move |declaration| {
                    extraction::php::returned_class(declaration).filter(|named| {
                        !extracted.imports.iter().any(|import| {
                            relations
                                .resolve_import(path, import, php, modules, &extracted.imports)
                                .invalid_bindings
                                .iter()
                                .any(|binding| same_name(php, binding, named))
                        })
                    })
                })
        });
    let first = returned.next()??;
    returned
        .all(|other| other.is_some_and(|other| same_name(php, &other, &first)))
        .then_some(first)
}

/// The PHP types the index's files declare, each with what it inherits from,
/// read from each file's [`Built`], so walking a lineage reads no file.
pub(super) struct Ancestry<'r> {
    relations: &'r Relations,
    declared: HashMap<String, Vec<(&'r str, &'r [(String, bool)])>>,
}

impl<'r> Ancestry<'r> {
    pub(super) fn new(relations: &'r Relations) -> Self {
        let mut declared: HashMap<String, Vec<(&'r str, &'r [(String, bool)])>> = HashMap::new();
        for (file, built) in &relations.built_from {
            for (name, supertypes) in &built.inherits {
                declared
                    .entry(name.to_ascii_lowercase())
                    .or_default()
                    .push((&**file, supertypes.as_slice()));
            }
        }
        Ancestry { relations, declared }
    }

    /// The files that declare the type `file` names `supertype`: the ones it
    /// imports when a `use` of it binds the name, else the ones in its own
    /// directory, its namespace under PSR-4, else every one. A `use` that
    /// reaches no file of the repository names a type outside it.
    fn declaring(&self, file: &str, supertype: &str, bound: bool) -> Vec<&'r str> {
        let declaring: Vec<&'r str> = self.relations.defined_in(supertype).collect();
        if bound {
            return declaring
                .into_iter()
                .filter(|declared| self.relations.importers_of(declared).iter().any(|importer| &*importer.file == file))
                .collect();
        }
        let directory = Path::new(file).parent();
        let beside: Vec<&'r str> = declaring
            .iter()
            .copied()
            .filter(|declared| Path::new(declared).parent() == directory)
            .collect();
        if beside.is_empty() { declaring } else { beside }
    }

    /// Each supertype the declarations of `class` in the files `keep` admits
    /// write, with the file that writes it, last first.
    fn supertypes_of(&self, class: &str, keep: impl Fn(&str) -> bool) -> Vec<(&'r str, &'r str, bool)> {
        let mut written: Vec<(&'r str, &'r str, bool)> = self
            .declared
            .get(&class.to_ascii_lowercase())
            .into_iter()
            .flatten()
            .filter(|(declared, _)| keep(declared))
            .flat_map(|&(declared, supertypes)| {
                supertypes.iter().map(move |(supertype, bound)| (declared, supertype.as_str(), *bound))
            })
            .collect();
        written.reverse();
        written
    }

    /// The files that declare the class a reference in `file` names `class`,
    /// read through the `use` items of the file.
    pub(super) fn named_in(&self, file: &str, class: &str, imports: &[extraction::Import]) -> Vec<&'r str> {
        let bound = imports
            .iter()
            .any(|import| import.binding().is_some_and(|binding| binding.eq_ignore_ascii_case(class)));
        self.declaring(file, class, bound)
    }

    /// Every type the declarations of `class` in `files` inherit from,
    /// nearest first, each with the files that declare it, the way PHP looks
    /// a method up: its traits, then its parent and all the parent inherits,
    /// then its interfaces.
    fn ancestors(&self, class: &str, files: &[&str]) -> Vec<(String, Vec<&'r str>)> {
        let mut seen = HashSet::from([class.to_ascii_lowercase()]);
        let mut lineage = Vec::new();
        let mut stack = self.supertypes_of(class, |declared| files.contains(&declared));
        while let Some((written_in, supertype, bound)) = stack.pop() {
            let declaring = self.declaring(written_in, supertype, bound);
            if declaring.is_empty() || !seen.insert(supertype.to_ascii_lowercase()) {
                continue;
            }
            stack.extend(self.supertypes_of(supertype, |declared| declaring.contains(&declared)));
            lineage.push((supertype.to_string(), declaring));
        }
        lineage
    }

    /// A PHP member or static call in `file` on a class that declares no
    /// method of its name, read as a call on the nearest ancestor that does,
    /// with the methods of that ancestor it reaches.
    pub(super) fn inherited<'c>(
        &self,
        file: &str,
        imports: &[extraction::Import],
        reference: &extraction::Reference,
        candidates: &[&'c Candidate],
    ) -> Option<(extraction::Reference, Vec<&'c Candidate>)> {
        let php = Some("php");
        let class = reference.receiver.as_deref()?;
        if reference.shape == RefShape::Free
            || candidates
                .iter()
                .any(|candidate| shape_matches(candidate, php, reference.shape, Some(class)))
        {
            return None;
        }
        self.ancestors(class, &self.named_in(file, class, imports))
            .into_iter()
            .find_map(|(ancestor, declaring)| {
                let reached: Vec<&'c Candidate> = candidates
                    .iter()
                    .copied()
                    .filter(|candidate| {
                        declaring.contains(&candidate.file.as_str())
                            && shape_matches(candidate, php, reference.shape, Some(&ancestor))
                    })
                    .collect();
                (!reached.is_empty()).then(|| {
                    let inherited = extraction::Reference {
                        receiver: Some(ancestor),
                        ..reference.clone()
                    };
                    (inherited, reached)
                })
            })
    }

    /// Each candidate's lineage: the types its PHP class inherits from.
    pub(super) fn lineages(&self, candidates: &[&Candidate]) -> Vec<Vec<(String, Vec<&'r str>)>> {
        candidates
            .iter()
            .map(|candidate| match (candidate.language.as_deref(), &candidate.declaration.container) {
                (Some("php"), Some(container)) => self.ancestors(container, &[candidate.file.as_str()]),
                _ => Vec::new(),
            })
            .collect()
    }
}

/// The candidates a call on `class`, declared in `declaring`, may also reach
/// at run time: each method of a type whose lineage holds that class.
pub(super) fn overrides<'c>(
    class: &str,
    declaring: &[&str],
    candidates: &[&'c Candidate],
    lineages: &[Vec<(String, Vec<&str>)>],
) -> Vec<&'c Candidate> {
    candidates
        .iter()
        .zip(lineages)
        .filter(|(_, lineage)| {
            lineage.iter().any(|(ancestor, files)| {
                same_name(Some("php"), ancestor, class) && files.iter().any(|file| declaring.contains(file))
            })
        })
        .map(|(candidate, _)| *candidate)
        .collect()
}

/// Whether `class` declares a method among `candidates`.
fn class_declares(candidates: &[&Candidate], class: &str) -> bool {
    let php = Some("php");
    candidates.iter().any(|candidate| {
        candidate.language.as_deref() == php
            && candidate
                .declaration
                .container
                .as_deref()
                .is_some_and(|container| same_name(php, container, class))
    })
}

/// A PHP reference whose receiver is `$this` outside every class, as in a
/// Pest test's closure, with `$this` read as the test case class its
/// directory binds, or as no class.
pub(super) fn bound_this(reference: &extraction::Reference, test_case: Option<&str>) -> Option<extraction::Reference> {
    let rest = reference.receiver.as_deref()?.strip_prefix("$this")?;
    Some(extraction::Reference {
        receiver: test_case.map(|class| format!("{class}{rest}")),
        ..reference.clone()
    })
}
