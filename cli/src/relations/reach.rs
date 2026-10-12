//! Walks over the import graph: the files a file reaches, and the files
//! ranked by how much of the repository reaches them.

use crate::file_facts;
use crate::relations::{get, Relations, CONFIDENCE_AMBIGUOUS};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Which way an import walk runs.
#[derive(Clone, Copy)]
pub enum Reach {
    /// Who depends on this file.
    Importers,
    /// What this file depends on.
    Imports,
}

/// One file reached by an import walk, how many hops away it is, and the
/// symbol the reaching import named when it named one.
pub struct Reached {
    pub file: String,
    pub depth: i64,
    pub symbol: Option<String>,
}

/// Files reachable from `file` along import edges, up to `max_depth`.
///
/// Only the importer direction is stored; the forward direction is that map
/// read the other way, inverted once in memory. 14,094 edges on
/// laravel-framework and 678 on WordPress, so inverting costs nothing and
/// storing both would be a second copy of one fact.
pub fn reachable(file: &str, max_depth: i64, reach: Reach, repo_root: &Path) -> Vec<Reached> {
    let relations = get(repo_root);
    let forward: HashMap<&str, Vec<(&str, Option<&String>)>> = match reach {
        Reach::Importers => HashMap::new(),
        Reach::Imports => {
            let mut map: HashMap<&str, Vec<(&str, Option<&String>)>> = HashMap::new();
            for (target, importer) in relations.import_edges() {
                map.entry(&importer.file)
                    .or_default()
                    .push((target, importer.symbol.as_ref()));
            }
            map
        }
    };
    reachable_from(&relations, &forward, file, max_depth, reach)
}

fn reachable_from(
    relations: &Relations,
    forward: &HashMap<&str, Vec<(&str, Option<&String>)>>,
    file: &str,
    max_depth: i64,
    reach: Reach,
) -> Vec<Reached> {
    let mut seen: HashSet<String> = HashSet::from([file.to_string()]);
    let mut frontier = std::collections::VecDeque::from([(file.to_string(), 0i64)]);
    let mut out = Vec::new();
    while let Some((current, depth)) = frontier.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let next: Vec<(String, Option<String>)> = match reach {
            Reach::Importers => relations
                .importers_of(&current)
                .iter()
                .map(|i| (i.file.to_string(), i.symbol.clone()))
                .collect(),
            Reach::Imports => forward
                .get(current.as_str())
                .map(|v| v.iter().map(|(f, s)| (f.to_string(), s.cloned())).collect())
                .unwrap_or_default(),
        };
        for (step, symbol) in next {
            if !seen.insert(step.clone()) {
                continue;
            }
            out.push(Reached {
                file: step.clone(),
                depth: depth + 1,
                symbol,
            });
            frontier.push_back((step, depth + 1));
        }
    }
    out
}

/// The files with the widest reach: `(file, direct, transitive)`, ranked by
/// transitive reach then direct edges, ties broken by path so repeated runs
/// rank identically.
///
/// Ranking by raw edge count buries a file that one hub imports and
/// everything else reaches through that hub, so every scoped subject's
/// transitive set is measured before the limit. `usages --path`,
/// `dependencies --path` and the primer's Spine are the same question
/// asked three ways and read this one function.
pub fn ranked_by_reach(
    relations: &Relations,
    max_depth: i64,
    limit: usize,
    reach: Reach,
    scope: Option<&str>,
) -> Vec<Ranked> {
    let repo_root = relations.repo_root.as_path();
    // An AMBIGUOUS row names several candidate files and depends on none, so
    // it ranks nothing — the same rows every importer count skips.
    let resolved_edges = || {
        relations
            .import_edges()
            .filter(|(_, importer)| importer.confidence != CONFIDENCE_AMBIGUOUS)
    };
    let mut identity: HashMap<&str, usize> = HashMap::new();
    let mut files: Vec<&str> = Vec::new();
    for (target, importer) in resolved_edges() {
        for file in [target, &*importer.file] {
            if !identity.contains_key(file) {
                identity.insert(file, files.len());
                files.push(file);
            }
        }
    }

    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
    let mut direct = vec![0i64; files.len()];
    // The ranked file is the TARGET there, and the edge's symbol lives in the
    // target, so the row can name what is actually depended on rather than
    // the file that holds it. The importer with the lowest path names it, so
    // the label does not drift with the order rows were absorbed in.
    let mut named: Vec<Option<(&str, &String)>> = vec![None; files.len()];
    for (target, importer) in resolved_edges() {
        let target = identity[target];
        let importer_file = identity[&*importer.file];
        let (node, next) = match reach {
            Reach::Importers => (target, importer_file),
            Reach::Imports => (importer_file, target),
        };
        adjacency[node].push(next);
        direct[node] += 1;
        if matches!(reach, Reach::Importers) {
            if let Some(symbol) = importer.symbol.as_ref() {
                let file: &str = &importer.file;
                if named[target].map_or(true, |(known, _)| file < known) {
                    named[target] = Some((file, symbol));
                }
            }
        }
    }
    let scope = scope.unwrap_or("").trim_end_matches('/');
    let prefix = if scope.is_empty() {
        None
    } else {
        Some(format!("{scope}/"))
    };
    let mut subjects: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(id, file)| {
            direct[*id] > 0
                && (scope.is_empty()
                    || **file == scope
                    || prefix
                        .as_deref()
                        .map(|prefix| file.starts_with(prefix))
                        .unwrap_or(false))
        })
        .map(|(id, _)| id)
        .collect();
    subjects.sort_by_key(|id| files[*id]);

    let mut ranked: Vec<Ranked> = subjects
        .into_par_iter()
        .map_init(
            || {
                (
                    vec![0usize; files.len()],
                    0usize,
                    std::collections::VecDeque::new(),
                )
            },
            |(visited, visit, frontier), file| {
                *visit += 1;
                visited[file] = *visit;
                frontier.clear();
                frontier.push_back((file, 0i64));
                let mut transitive = 0i64;
                while let Some((current, depth)) = frontier.pop_front() {
                    if depth >= max_depth {
                        continue;
                    }
                    for &next in &adjacency[current] {
                        if visited[next] == *visit {
                            continue;
                        }
                        visited[next] = *visit;
                        transitive += 1;
                        frontier.push_back((next, depth + 1));
                    }
                }
                Ranked {
                    file: files[file].to_string(),
                    symbol: named[file].map(|(_, symbol)| symbol.clone()),
                    direct: direct[file],
                    transitive,
                }
            },
        )
        .collect();
    ranked
        .sort_by(|a, b| (b.transitive, b.direct, &b.file).cmp(&(a.transitive, a.direct, &a.file)));
    ranked.truncate(limit);
    // An import can name something the file does not declare — `Path` from a
    // `use std::path::Path` the resolver pinned on it. That name labels
    // nothing in the repository, so the row names the file's module instead.
    for row in &mut ranked {
        let Some(symbol) = row.symbol.as_deref() else {
            continue;
        };
        let declared = file_facts::get(&repo_root.join(&row.file), repo_root)
            .and_then(|facts| facts.extraction)
            .is_some_and(|extraction| {
                extraction.declarations.iter().any(|declaration| declaration.name == symbol)
            });
        if !declared {
            row.symbol = None;
        }
    }
    ranked
}

/// One row of a reach ranking: the file, the symbol it is depended on
/// through when there is one, and its direct and transitive edge counts.
pub struct Ranked {
    pub file: String,
    pub symbol: Option<String>,
    pub direct: i64,
    pub transitive: i64,
}
