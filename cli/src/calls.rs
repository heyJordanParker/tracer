//! The `calls:` block a windowed `read` or `context` prints after its rows:
//! each call in the window, the declaration it resolves to with that
//! declaration's source, and the other places that call it.

use crate::session;
use crate::file_facts::FileFacts;
use crate::relations;
use crate::summary::clip;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

const CALL_BODY_LINES: i64 = 12;

// A next.js mentioning file costs 0.9 to 3.7 ms to resolve alone, so 300 keep `calls:` near a second there; a 700-file repository's windows read at most 241.
const CALLS_FILE_BUDGET: usize = 300;

#[derive(Serialize)]
pub struct Call {
    pub line: i64,
    pub method: String,
    #[serde(flatten)]
    pub target: Target,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Target {
    Resolved {
        file: String,
        declared_at: i64,
        callers: usize,
        source: String,
        also_called_from: Vec<CallSite>,
    },
    CallSitesPastBudget {
        file: String,
        declared_at: i64,
        source: String,
    },
    PastBudget {
        defined_in: usize,
    },
}

#[derive(Serialize)]
pub struct CallSite {
    pub file: String,
    pub line: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    pub text: String,
}

pub fn calls(facts: &FileFacts, relative: &str, window: (i64, i64), repo_root: &Path) -> Vec<Call> {
    crate::timing::phase("calls", || {
        let Some(extraction) = &facts.extraction else {
            return Vec::new();
        };
        let within = |line: i64| window.0 <= line && line <= window.1;
        let called: BTreeMap<&str, BTreeSet<i64>> = extraction
            .references
            .iter()
            .filter(|reference| within(reference.line) && reference.receiver.as_deref() != Some(&reference.name))
            .fold(BTreeMap::new(), |mut called, reference| {
                called.entry(reference.name.as_str()).or_default().insert(reference.line);
                called
            });
        let Some(index) = relations::ready(repo_root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut charged: HashSet<&str> = HashSet::from([relative]);

        let mut by_cost: Vec<(&str, Vec<&str>)> = called
            .keys()
            .map(|&name| (name, index.defined_in(name).collect::<Vec<&str>>()))
            .filter(|(_, defining)| !defining.is_empty())
            .collect();
        by_cost.sort_by_key(|(name, defining)| (defining.len(), called[name].first().copied()));
        let mut in_window = Vec::new();
        for (name, defining) in by_cost {
            if charge(&mut charged, &defining) {
                in_window.push(name);
                continue;
            }
            out.extend(called[name].iter().map(|&line| Call {
                line,
                method: name.to_string(),
                target: Target::PastBudget { defined_in: defining.len() },
            }));
        }
        let candidates = relations::candidates(&index, &in_window);
        let window_sites = relations::use_sites(&index, &candidates, Some(relative));

        let mut resolved_in_window = Vec::new();
        for ((name, candidates), sites) in in_window.into_iter().zip(candidates).zip(&window_sites) {
            let resolved: Vec<(i64, &relations::UseSite)> = called[name]
                .iter()
                .filter_map(|&line| resolved_at(sites, relative, line).map(|site| (line, site)))
                .collect();
            if !resolved.is_empty() {
                resolved_in_window.push((name, candidates, resolved, index.used_in(name).collect::<Vec<&str>>()));
            }
        }
        resolved_in_window.sort_by_cached_key(|(name, _, _, mentioning)| {
            (mentioning.iter().filter(|file| !charged.contains(*file)).count(), called[name].first().copied())
        });
        let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let (mut everywhere, mut everywhere_candidates) = (Vec::new(), Vec::new());
        for (name, candidates, resolved, mentioning) in resolved_in_window {
            if charge(&mut charged, &mentioning) {
                everywhere.push(name);
                everywhere_candidates.push(candidates);
                continue;
            }
            for (line, site) in resolved {
                out.push(Call {
                    line,
                    method: target_method(site),
                    target: Target::CallSitesPastBudget {
                        file: site.target_file.clone(),
                        declared_at: site.target.line,
                        source: target_source(site, &mut sources, repo_root),
                    },
                });
            }
        }

        for (name, sites) in everywhere.iter().zip(relations::use_sites(&index, &everywhere_candidates, None)) {
            for &line in &called[name] {
                let Some(site) = resolved_at(&sites, relative, line) else {
                    continue;
                };
                let target = &site.target;
                let mut callers: Vec<&relations::UseSite> = sites
                    .iter()
                    .filter(|other| other.target_file == site.target_file && other.target.line == target.line)
                    .collect();
                callers.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
                let source = target_source(site, &mut sources, repo_root);
                let also_called_from = callers
                    .iter()
                    .filter(|other| !(other.file == relative && within(other.line)))
                    .map(|other| CallSite {
                        file: other.file.clone(),
                        line: other.line,
                        caller: other.caller.as_ref().map(|caller| caller.name.clone()),
                        text: excerpt(
                            sources
                                .entry(other.file.clone())
                                .or_insert_with(|| source_lines(&repo_root.join(&other.file))),
                            other.line,
                            other.line,
                        )
                        .trim()
                        .to_string(),
                    })
                    .collect();
                out.push(Call {
                    line,
                    method: target_method(site),
                    target: Target::Resolved {
                        file: site.target_file.clone(),
                        declared_at: target.line,
                        callers: callers.len(),
                        source,
                        also_called_from,
                    },
                });
            }
        }
        out.sort_by_key(|call| call.line);
        out
    })
}

/// Each function or method of `relative` whose lines `changed` touch, with
/// every place that calls it, so an agent that changed it can update its
/// callers. A function mentioned in more files than `CALLS_FILE_BUDGET` is
/// left out, as is one nothing calls.
pub fn callers_of(facts: &FileFacts, relative: &str, changed: &[(i64, i64)], repo_root: &Path) -> Vec<Call> {
    crate::timing::phase("callers", || {
        let Some(extraction) = &facts.extraction else {
            return Vec::new();
        };
        let touched = |start: i64, end: i64| changed.iter().any(|&(from, to)| from <= end && start <= to);
        let functions: Vec<&crate::extraction::Declaration> = extraction
            .declarations
            .iter()
            .filter(|declaration| matches!(declaration.kind.as_str(), "function" | "method"))
            .filter(|declaration| touched(declaration.header_line, declaration.end_line))
            .collect();
        let Some(index) = relations::ready(repo_root) else {
            return Vec::new();
        };
        let mut charged: HashSet<&str> = HashSet::from([relative]);
        let mut out = Vec::new();
        let mut resolvable = Vec::new();
        for declaration in functions {
            let mentioning: Vec<&str> = index.used_in(&declaration.name).collect();
            if charge(&mut charged, &mentioning) {
                let method = match &declaration.container {
                    Some(container) => format!("{container}::{}", declaration.name),
                    None => declaration.name.clone(),
                };
                resolvable.push((declaration, method));
            }
        }
        let names: Vec<&str> = resolvable.iter().map(|(declaration, _)| declaration.name.as_str()).collect();
        let candidates = relations::candidates(&index, &names);
        let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for ((declaration, method), sites) in resolvable.into_iter().zip(relations::use_sites(&index, &candidates, None)) {
            let mut callers: Vec<&relations::UseSite> = sites
                .iter()
                .filter(|site| site.target_file == relative && site.target.line == declaration.line)
                .collect();
            callers.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
            let also_called_from: Vec<CallSite> = callers
                .iter()
                .map(|site| CallSite {
                    file: site.file.clone(),
                    line: site.line,
                    caller: site.caller.as_ref().map(|caller| caller.name.clone()),
                    text: excerpt(
                        sources.entry(site.file.clone()).or_insert_with(|| source_lines(&repo_root.join(&site.file))),
                        site.line,
                        site.line,
                    )
                    .trim()
                    .to_string(),
                })
                .collect();
            if also_called_from.is_empty() {
                continue;
            }
            out.push(Call {
                line: declaration.line,
                method,
                target: Target::Resolved {
                    file: relative.to_string(),
                    declared_at: declaration.line,
                    callers: also_called_from.len(),
                    source: String::new(),
                    also_called_from,
                },
            });
        }
        out.sort_by_key(|call| call.line);
        out
    })
}

fn resolved_at<'a>(sites: &'a [relations::UseSite], file: &str, line: i64) -> Option<&'a relations::UseSite> {
    match sites
        .iter()
        .filter(|site| site.file == file && site.line == line && site.confidence != relations::CONFIDENCE_AMBIGUOUS)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [site] if site.target.kind == "function" => Some(site),
        _ => None,
    }
}

/// Adds `files` to `charged` and returns true when the ones it does not
/// hold yet fit `CALLS_FILE_BUDGET`; else leaves `charged` as it was.
fn charge<'a>(charged: &mut HashSet<&'a str>, files: &[&'a str]) -> bool {
    let new: HashSet<&str> = files.iter().copied().filter(|file| !charged.contains(file)).collect();
    if charged.len() + new.len() > CALLS_FILE_BUDGET {
        return false;
    }
    charged.extend(new);
    true
}

fn target_method(site: &relations::UseSite) -> String {
    match &site.target.container {
        Some(container) => format!("{container}::{}", site.target.name),
        None => site.target.name.clone(),
    }
}

/// Keep, of each called function's other call sites, only those not yet
/// shown to this agent as they are: each function's sites are offered as its
/// `CallSites`, keyed by its file and name, so lines that only moved keep a
/// site's key. A function called on several lines is offered once. Returns
/// each call's offer key, to withdraw when its sites do not print.
fn deliver(calls: &mut [Call], delivery: &mut session::Delivery) -> Vec<Option<String>> {
    let mut offered = Vec::with_capacity(calls.len());
    for call in calls {
        let Target::Resolved { file, also_called_from, .. } = &mut call.target else {
            offered.push(None);
            continue;
        };
        let sites: serde_json::Map<String, serde_json::Value> = also_called_from
            .iter()
            .map(|site| {
                let at = site.caller.clone().unwrap_or_else(|| site.line.to_string());
                (format!("{}:{at}", site.file), site.text.clone().into())
            })
            .collect();
        let key = format!("{file}#{}", call.method);
        match delivery.offer(session::Kind::CallSites, &key, &serde_json::json!({ "sites": sites })) {
            session::Offer::New => {}
            session::Offer::Same => also_called_from.clear(),
            session::Offer::Changed(lines) => {
                let changed = lines.get("sites").and_then(serde_json::Value::as_object);
                also_called_from.retain(|site| {
                    let at = site.caller.clone().unwrap_or_else(|| site.line.to_string());
                    changed.is_some_and(|changed| changed.get(&format!("{}:{at}", site.file)).is_some_and(|line| !line.is_null()))
                });
            }
        }
        offered.push(Some(key));
    }
    offered
}

/// The function `site` resolves to as a call shows it: its whole source
/// while the session has not read it and it spans at most
/// `CALL_BODY_LINES`, else its header.
fn target_source(site: &relations::UseSite, sources: &mut BTreeMap<String, Vec<String>>, repo_root: &Path) -> String {
    let target = &site.target;
    let lines = sources
        .entry(site.target_file.clone())
        .or_insert_with(|| source_lines(&repo_root.join(&site.target_file)));
    let read = session::has_read(
        &repo_root.join(&site.target_file),
        &session::content_hash(&lines.concat()),
        target.header_line as usize,
        target.end_line as usize,
    );
    if read || target.end_line - target.header_line + 1 > CALL_BODY_LINES {
        target.header.clone()
    } else {
        excerpt(lines, target.header_line, target.end_line)
    }
}

/// The calls block of a window of `file` in at most `budget` characters,
/// at the first of three levels whose heads fit: one head per call, one head
/// per function naming every line that calls it, then one line per file the
/// calls reach with their count, and one line naming the functions past
/// `CALLS_FILE_BUDGET` that name no file. At the first two, heads get their
/// source back in line order, then their other call sites, calls into other
/// files first, while the budget holds — the way `surface::render_within`
/// grows rows. The last
/// keeps its lines while the budget holds, other files first, then `file`,
/// then the functions past `CALLS_FILE_BUDGET`, so the block never overruns
/// it. `None` renders every call whole. A function the window calls on
/// several lines shows its source and other call sites once, under its first
/// line.
///
/// With a `delivery`, each function's other call sites are extra context:
/// only the sites this agent was not shown print, and a function whose sites
/// the budget cuts stays unshown. `head` names the block: `calls` for what a
/// window calls, `callers` for who calls what an edit changed.
pub fn render_calls(
    head: &str,
    calls: &mut [Call],
    file: &str,
    budget: Option<usize>,
    delivery: Option<&mut session::Delivery>,
) -> String {
    if calls.is_empty() {
        return String::new();
    }
    let Some(delivery) = delivery else {
        return fitted_calls(head, calls, file, budget).0;
    };
    let offered = deliver(calls, delivery);
    // A `callers` entry carries no source, so with its sites all shown it
    // has nothing left to say.
    if calls.iter().all(|call| matches!(&call.target, Target::Resolved { source, also_called_from, .. } if source.is_empty() && also_called_from.is_empty())) {
        return String::new();
    }
    let (text, printed) = fitted_calls(head, calls, file, budget);
    for (call, key) in calls.iter().zip(offered) {
        if let Some(key) = key.filter(|_| !printed.contains(&target_key(call))) {
            delivery.withdraw(session::Kind::CallSites, &key);
        }
    }
    text
}

/// `render_calls`'s text, and the functions whose other call sites it
/// printed.
fn fitted_calls<'a>(head: &str, calls: &'a [Call], file: &str, budget: Option<usize>) -> (String, HashSet<(&'a str, i64)>) {
    let mut seen = HashSet::new();
    let per_call: Vec<CallEntry> = calls
        .iter()
        .map(|call| CallEntry::of(vec![call], seen.insert(target_key(call)), file))
        .collect();
    let whole: String = per_call.iter().map(|entry| entry.levels[0].as_str()).collect();
    let every = || calls.iter().map(target_key).collect();
    let Some(budget) = budget.map(|budget| budget.saturating_sub(head.len() + 2)) else {
        return (format!("{head}:\n{whole}"), every());
    };
    if whole.len() <= budget {
        return (format!("{head}:\n{whole}"), every());
    }
    let shortened = crate::output::shortened_line(calls.len(), calls.len(), "calls");
    let Some(budget) = budget.checked_sub(shortened.len() + 3) else {
        return (String::new(), HashSet::new());
    };
    let per_function: Vec<CallEntry> = grouped(calls, target_key)
        .into_iter()
        .map(|calls| CallEntry::of(calls, true, file))
        .collect();
    let (text, cut, printed) = fit_entries(&per_call, budget)
        .or_else(|| fit_entries(&per_function, budget))
        .unwrap_or_else(|| (by_file(calls, file, budget), calls.len(), HashSet::new()));
    (format!("{head}:\n{text}  {}\n", crate::output::shortened_line(cut, calls.len(), head)), printed)
}

struct CallEntry<'a> {
    calls: Vec<&'a Call>,
    levels: [String; 3],
    into_reading_file: bool,
}

impl<'a> CallEntry<'a> {
    fn of(calls: Vec<&'a Call>, detailed: bool, file: &str) -> CallEntry<'a> {
        let first = calls[0];
        let lines: Vec<String> = calls.iter().map(|call| format!("L{}", call.line)).collect();
        let (lines, method) = (lines.join(", "), &first.method);
        let trace = crate::output::trace_command();
        let (head, source, sites) = match &first.target {
            Target::Resolved { file, declared_at, callers, source, also_called_from } => (
                format!("  {lines} {method}  {file}:{declared_at}  {{callers: {callers}}}\n"),
                source.as_str(),
                also_called_from.as_slice(),
            ),
            Target::CallSitesPastBudget { file, declared_at, source } => (
                format!("  {lines} {method}  {file}:{declared_at} \u{2192} {trace} callers {method}\n"),
                source.as_str(),
                &[][..],
            ),
            Target::PastBudget { defined_in } => (
                format!("  {lines} {method}  {{defined_in: {defined_in}}} \u{2192} {trace} callers {method}\n"),
                "",
                &[][..],
            ),
        };
        let (source, sites) = if detailed { (source, sites) } else { ("", &[][..]) };
        let sourced = source.lines().fold(head.clone(), |text, line| text + &format!("    {line}\n"));
        let whole = match sites {
            [] => sourced.clone(),
            sites => sites.iter().fold(format!("{sourced}    also called from:\n"), |text, site| {
                let caller = site.caller.as_ref().map(|caller| format!("  {caller}")).unwrap_or_default();
                text + &format!("      {}:{}{caller}  {}\n", site.file, site.line, clip(&site.text, 100))
            }),
        };
        CallEntry {
            into_reading_file: target_file(first) == Some(file),
            calls,
            levels: [whole, sourced, head],
        }
    }
}

/// The entries at the most detail `budget` allows, how many calls that cut,
/// and the functions whose entry printed whole, with its call sites.
fn fit_entries<'a>(entries: &[CallEntry<'a>], budget: usize) -> Option<(String, usize, HashSet<(&'a str, i64)>)> {
    let mut chosen = vec![2; entries.len()];
    let mut size: usize = entries.iter().map(|entry| entry.levels[2].len()).sum();
    if size > budget {
        return None;
    }
    let mut other_files_first: Vec<usize> = (0..entries.len()).collect();
    other_files_first.sort_by_key(|&at| entries[at].into_reading_file);
    for (level, order) in [(1, (0..entries.len()).collect()), (0, other_files_first)] {
        for at in order {
            let levels = &entries[at].levels;
            let resized = size - levels[chosen[at]].len() + levels[level].len();
            if resized <= budget {
                size = resized;
                chosen[at] = level;
            }
        }
    }
    let cut = entries
        .iter()
        .zip(&chosen)
        .filter(|(entry, &level)| entry.calls.len() > 1 || entry.levels[level] != entry.levels[0])
        .map(|(entry, _)| entry.calls.len())
        .sum();
    let text = entries.iter().zip(&chosen).map(|(entry, &level)| entry.levels[level].as_str()).collect();
    // Only a whole level that holds call sites printed them.
    let printed = entries
        .iter()
        .zip(&chosen)
        .filter(|(entry, &level)| level == 0 && entry.levels[0] != entry.levels[1])
        .map(|(entry, _)| target_key(entry.calls[0]))
        .collect();
    Some((text, cut, printed))
}

fn by_file(calls: &[Call], file: &str, budget: usize) -> String {
    let mut files = grouped(calls, target_file);
    files.sort_by_key(|calls| target_file(calls[0]).map_or(2, |into| usize::from(into == file)));
    let place = |calls: &[&Call]| match target_file(calls[0]) {
        Some(into) => into.to_string(),
        None => {
            let mut seen = HashSet::new();
            let names: Vec<&str> =
                calls.iter().map(|call| call.method.as_str()).filter(|name| seen.insert(*name)).collect();
            names.join(", ")
        }
    };
    let mut size = 0;
    files
        .iter()
        .map(|calls| format!("  {}  {{calls: {}}}\n", place(calls), calls.len()))
        .take_while(|line| {
            size += line.len();
            size <= budget
        })
        .collect()
}

fn grouped<'a, K: Eq + std::hash::Hash>(calls: &'a [Call], key: impl Fn(&'a Call) -> K) -> Vec<Vec<&'a Call>> {
    let mut groups: Vec<Vec<&Call>> = Vec::new();
    let mut at: HashMap<K, usize> = HashMap::new();
    for call in calls {
        let index = *at.entry(key(call)).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[index].push(call);
    }
    groups
}

fn target_key(call: &Call) -> (&str, i64) {
    match &call.target {
        Target::Resolved { file, declared_at, .. } | Target::CallSitesPastBudget { file, declared_at, .. } => {
            (file, *declared_at)
        }
        Target::PastBudget { .. } => (&call.method, 0),
    }
}

fn target_file(call: &Call) -> Option<&str> {
    match &call.target {
        Target::Resolved { file, .. } | Target::CallSitesPastBudget { file, .. } => Some(file),
        Target::PastBudget { .. } => None,
    }
}

fn source_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|source| source.split_inclusive('\n').map(str::to_string).collect())
        .unwrap_or_default()
}

fn excerpt(lines: &[String], start: i64, end: i64) -> String {
    lines
        .iter()
        .skip((start - 1).max(0) as usize)
        .take((end - start + 1).max(0) as usize)
        .map(|line| line.trim_end_matches(['\n', '\r']))
        .collect::<Vec<_>>()
        .join("\n")
}
