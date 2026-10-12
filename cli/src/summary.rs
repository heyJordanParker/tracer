//! A file's facts — the one structure every command shows about a file.
//!
//! `Facts` is built from the cached `FileFacts` and the graph around the file,
//! printed as YAML front matter through `yamlfmt`, and carried under the same
//! keys in `--json`. Keys use the words git, GitHub, and editors use; values
//! are counts, names, and ages, never ratios. `headline` is the one-line form
//! a list of many files shows.

use crate::session::{Delivery, Kind, Offer};
use crate::file_facts::FileFacts;
use crate::git_activity::GitActivity;
use crate::relations::ModuleCounts;
use serde::Serialize;
use serde_json::{Map, Value};
use std::path::Path;

#[derive(Serialize)]
pub struct Facts {
    pub file: String,
    pub lines: i64,
    pub cyclomatic_complexity: i64,
    pub complexity_rank: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_by: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imports: Option<u32>,
    pub git: Git,
}

#[derive(Serialize)]
pub struct Git {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits: Option<i64>,
    /// Set instead of `commits` when the history walk stopped at its cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_at_least: Option<i64>,
    pub commits_last_30_days: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub on_deploy_branches: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub usually_changed_with: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_author: Option<String>,
}

impl Facts {
    pub fn of(facts: &FileFacts, graph: Option<&ModuleCounts>) -> Facts {
        Facts {
            file: facts.path.clone(),
            lines: facts.loc,
            cyclomatic_complexity: facts.cyclomatic_complexity_total,
            complexity_rank: facts.rank.clone(),
            imported_by: graph.map(|graph| graph.imported_by),
            imports: graph.map(|graph| graph.imports),
            git: Git {
                status: status(facts.working_state.as_deref()),
                renamed_from: facts.rename_from.clone(),
                commits: (!facts.commit_count_is_floor).then_some(facts.commit_count),
                commits_at_least: facts.commit_count_is_floor.then_some(facts.commit_count),
                commits_last_30_days: facts.commits_30d,
                // A capped walk never reached the first commit, only a floor.
                first_commit: facts.first_seen.as_deref().and_then(age).map(|age| {
                    if facts.commit_count_is_floor {
                        format!("at least {age}")
                    } else {
                        age
                    }
                }),
                last_commit: last_commit(
                    facts.last_modified.as_deref(),
                    facts.last_author.as_deref(),
                    facts.last_subject.as_deref(),
                ),
                on_deploy_branches: facts.present_in.iter().map(|branch| branch.to_string()).collect(),
                // Whole paths: three `Claude.md` in one list name nothing.
                usually_changed_with: facts
                    .co_changed
                    .iter()
                    .take(3)
                    .map(|(path, _)| path.to_string())
                    .collect(),
                main_author: facts.top_author.clone(),
            },
        }
    }

    /// The facts as a mapping, so a command can add its own keys (the
    /// directory, the docs not yet loaded) before printing.
    pub fn to_map(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    /// The few facts a list of many files shows per file, so a command can add
    /// its own before printing.
    pub fn headline_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        if let Some(imported_by) = self.imported_by {
            map.insert("imported_by".into(), imported_by.into());
        }
        map.insert("cyclomatic_complexity".into(), self.cyclomatic_complexity.into());
        map.insert("lines".into(), self.lines.into());
        if self.git.status != "unmodified" {
            map.insert("git".into(), self.git.status.clone().into());
        }
        map
    }

    /// `headline_map` as the one-line flow mapping a list shows per file.
    pub fn headline(&self) -> String {
        crate::yamlfmt::flow(&Value::Object(self.headline_map()), false)
    }

    /// The facts `headline` shows, under their `to_map` keys, a nested one
    /// by its dotted path. An unmodified file's headline leaves `git` out,
    /// which says it is unmodified, so `git.status` is always one of them.
    fn headline_lines(&self) -> Map<String, Value> {
        let mut lines = self.headline_map();
        lines.remove("git");
        lines.insert("git.status".into(), self.git.status.clone().into());
        lines
    }
}

/// A listing row's facts, offered once per agent context for the file at
/// `path`, an absolute path under a canonical root: `None` when the agent
/// was shown them as they are, so the row prints without its headline; else
/// the key to withdraw when the budget cuts the headline away.
pub fn offer_headline(delivery: &mut Delivery, path: &Path, facts: &Facts) -> Option<String> {
    offer_lines(delivery, path, facts.headline_lines())
}

/// `offer_headline` for a row that shows its own `lines` of the file's
/// facts, keyed as `Facts::to_map` keys them, a nested one by its dotted
/// path. A listing builds every row's path from a canonical root, so it is
/// used as the key unresolved: one syscall a row would cost a large listing
/// more than the rest of its offer.
pub fn offer_lines(delivery: &mut Delivery, path: &Path, lines: Map<String, Value>) -> Option<String> {
    let key = path.to_string_lossy().to_string();
    match delivery.offer(Kind::Facts, &key, &Value::Object(lines)) {
        Offer::Same => None,
        Offer::New | Offer::Changed(_) => Some(key),
    }
}

/// A file's facts as extra context after a change the agent made: the whole
/// block when they are new to it, only the changed lines when it was shown
/// them before, and nothing while they stay the same.
pub fn offer_facts(path: &Path, facts: &Facts, delivery: &mut Delivery) -> String {
    let own = facts.to_map();
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    match delivery.offer(Kind::Facts, &key.to_string_lossy(), &Value::Object(own.clone())) {
        Offer::New => front_matter(&own),
        Offer::Same => String::new(),
        Offer::Changed(lines) => front_matter(&lines),
    }
}

/// Take back the headline offers of rows that printed below their first
/// `headline_levels` levels.
pub fn withdraw_cut(delivery: &mut Delivery, offered: &[Option<String>], levels: &[usize], headline_levels: usize) {
    for (key, level) in offered.iter().zip(levels) {
        if let Some(key) = key.as_ref().filter(|_| *level >= headline_levels) {
            delivery.withdraw(Kind::Facts, key);
        }
    }
}

/// The git facts the bulk activity map holds alone, for a file with no
/// per-file extraction: a listing entry, or a file that could not be read.
/// Keys match `Git`'s.
pub fn activity_git(activity: &GitActivity) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("status".into(), status(activity.working_state.as_deref()).into());
    map.insert("commits".into(), activity.commit_count.into());
    map.insert("commits_last_30_days".into(), activity.commits_30d.into());
    if let Some(first) = activity.first_seen.as_deref().and_then(age) {
        map.insert("first_commit".into(), first.into());
    }
    if let Some(last) = activity.last_modified.as_deref().and_then(age) {
        map.insert("last_commit".into(), last.into());
    }
    if let Some(author) = &activity.top_author {
        map.insert("main_author".into(), author.clone().into());
    }
    map
}

/// A mapping as a YAML front matter block.
pub fn front_matter(map: &Map<String, Value>) -> String {
    format!("---\n{}---\n", crate::yamlfmt::block(map))
}

/// What a command prints above a file's rows: `heading`, then `map` whole.
/// Text output with the file's `facts` prints the whole block the first time
/// an Agent context is shown `facts` alone, through `delivery`'s `Facts`
/// offer, so every command shares one record of them. Later the heading line
/// carries `facts.headline()` instead, and a block holds only the lines of
/// `facts` that changed since, beside every key `facts` do not own: the
/// command's own answer and the `directory` its own offer left. A command with no heading line passes `None` and takes `# <file>` as
/// one. The caller commits `delivery` once the text is flushed.
pub fn front_matter_once(
    heading: Option<&str>,
    path: &Path,
    facts: Option<&Facts>,
    map: &Map<String, Value>,
    delivery: &mut Delivery,
    once: bool,
) -> String {
    let whole = || heading.map(|heading| format!("{heading}\n")).unwrap_or_default() + &front_matter(map);
    let Some(facts) = facts.filter(|_| once) else {
        return whole();
    };
    let own = Value::Object(facts.to_map());
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut block = match delivery.offer(Kind::Facts, &key.to_string_lossy(), &own) {
        Offer::New => return whole(),
        Offer::Same => Map::new(),
        Offer::Changed(lines) => lines,
    };
    block.extend(
        map.iter()
            .filter(|(key, _)| own.get(*key).is_none())
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let heading = heading.map_or_else(|| format!("# {}", facts.file), str::to_string);
    let block = if block.is_empty() { String::new() } else { front_matter(&block) };
    format!("{heading}  {}\n{block}", facts.headline())
}

fn status(working_state: Option<&str>) -> String {
    working_state.unwrap_or("unmodified").to_string()
}

fn last_commit(date: Option<&str>, author: Option<&str>, subject: Option<&str>) -> Option<String> {
    let mut text = age(date?)?;
    if let Some(author) = author {
        text.push_str(&format!(" by {author}"));
    }
    if let Some(subject) = subject {
        text.push_str(&format!(": {}", clip(subject, 60)));
    }
    Some(text)
}

pub(crate) fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars - 1).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// A `YYYY-MM-DD` date as the age GitHub shows: "today", "3 weeks ago".
pub fn age(date: &str) -> Option<String> {
    // Git dates carry the committer's local day while this counts UTC days,
    // so a commit just after local midnight can sit a day in the future.
    let days = days_since(date)?.max(0);
    let plural = |count: i64, unit: &str| {
        format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
    };
    Some(match days {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        2..=13 => plural(days, "day"),
        14..=59 => plural(days / 7, "week"),
        60..=364 => plural(days / 30, "month"),
        _ => plural(days / 365, "year"),
    })
}

fn days_since(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.get(..2)?.parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(now.div_euclid(86400) - days_from_civil(y, m, d))
}

pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(count: i64, floor: bool) -> FileFacts {
        FileFacts {
            path: "bounded.py".into(),
            language: Some("python".into()),
            loc: 1,
            function_count: 0,
            cyclomatic_complexity_total: 0,
            cyclomatic_complexity_max: 0,
            rank: "low".into(),
            functions: Vec::new(),
            extraction: None,
            last_modified: Some("2023-11-14".into()),
            last_author: None,
            commits_30d: 0,
            first_seen: Some("2023-11-14".into()),
            commit_count: count,
            commit_count_is_floor: floor,
            rename_from: None,
            working_state: None,
            present_in: Vec::new(),
            last_subject: None,
            top_author: None,
            co_changed: Vec::new(),
            mtime_ns: 0,
            size_bytes: 0,
            extraction_failure: None,
            ctags: None,
            leading_comment: None,
        }
    }

    #[test]
    fn a_capped_history_says_at_least_and_never_an_exact_count() {
        let map = Facts::of(&facts(4000, true), None).to_map();
        assert_eq!(map["git"]["commits_at_least"], 4000);
        assert!(map["git"].get("commits").is_none());
        assert!(map["git"]["first_commit"].as_str().unwrap().starts_with("at least "));
    }

    #[test]
    fn keys_print_in_the_order_the_struct_declares() {
        let text = front_matter(&Facts::of(&facts(2, false), None).to_map());
        let keys: Vec<&str> = text
            .lines()
            .filter(|line| !line.starts_with(' ') && line.contains(':'))
            .map(|line| line.split(':').next().unwrap())
            .collect();
        assert_eq!(keys, ["file", "lines", "cyclomatic_complexity", "complexity_rank", "git"]);
    }
}
