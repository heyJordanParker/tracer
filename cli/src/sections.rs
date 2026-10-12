//! The per-file sections the relation commands print: `callers`, `usages`,
//! `dependencies`, and `defines` each group their rows under a heading and a
//! file, with that file's facts. `facts_of` joins those facts onto the rows
//! at render time, batched and deduped by path, so a file appearing in many
//! rows is resolved once.

use crate::output::counted;
use crate::summary::Facts;
use crate::file_facts;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

pub struct Section {
    pub heading: String,
    pub files: Vec<(String, Vec<String>, usize)>,
}

/// Each section under its heading, every file with its rows. A file's facts
/// print once per agent context; `facts` is keyed by path from `repo_root`.
pub fn render_sections(sections: &[Section], facts: &HashMap<String, Facts>, repo_root: &Path, one: &str, many: &str) {
    let mut delivery = crate::session::Delivery::default();
    let mut offered: Vec<Vec<Option<String>>> = Vec::with_capacity(sections.len());
    let entries: Vec<Vec<crate::output::Entry>> = sections
        .iter()
        .map(|section| {
            let mut keys = Vec::with_capacity(section.files.len());
            let entries = section
                .files
                .iter()
                .map(|(file, rows, count)| {
                    let head = format!("    {file}");
                    let offer = facts
                        .get(file)
                        .and_then(|shown| crate::summary::offer_headline(&mut delivery, &repo_root.join(file), shown).map(|key| (key, shown)));
                    let headline = offer.as_ref().map_or(String::new(), |(_, shown)| format!("  {}", shown.headline()));
                    keys.push(offer.map(|(key, _)| key));
                    crate::output::Entry {
                        rank: facts.get(file).and_then(|facts| facts.imported_by).unwrap_or(0) as i64,
                        levels: vec![
                            format!("{head}{headline}\n{}", rows.join("\n")),
                            format!("{head}  {}", counted(*count, one, many)),
                        ],
                    }
                })
                .collect();
            offered.push(keys);
            entries
        })
        .collect();
    let whole: usize = sections
        .iter()
        .zip(&entries)
        .map(|(section, entries)| section.heading.len() + 1 + entries.iter().map(|entry| entry.levels[0].len() + 1).sum::<usize>())
        .sum();
    let closing = crate::output::closing_room(entries.iter().map(Vec::len).sum(), "files");
    let share = crate::output::budget()
        .filter(|&budget| whole + closing > budget)
        .map(|budget| budget.saturating_sub(closing) / sections.len().max(1));
    let mut shortened = 0;
    let mut files = 0;
    for ((section, entries), keys) in sections.iter().zip(&entries).zip(&offered) {
        println!("{}", section.heading);
        let paths: Vec<&str> = section.files.iter().map(|(file, _, _)| file.as_str()).collect();
        let fixed = match (crate::output::budget(), share) {
            (Some(budget), Some(share)) => budget - share + section.heading.len() + 1,
            _ => section.heading.len() + 1,
        };
        let crate::output::Fitted { texts, shortened: cut, levels } = crate::output::fit_listing(entries, &paths, fixed);
        crate::summary::withdraw_cut(&mut delivery, keys, &levels, 1);
        for text in texts {
            println!("{text}");
        }
        shortened += cut;
        files += entries.len();
    }
    if shortened > 0 {
        println!("{}", crate::output::shortened_line(shortened, files, "files"));
    }
    if std::io::Write::flush(&mut std::io::stdout()).is_ok() {
        delivery.commit();
    }
}

pub fn facts_of(loaded: &HashMap<String, file_facts::FileFacts>, repo_root: &Path) -> HashMap<String, Facts> {
    let relations = crate::relations::get(repo_root);
    loaded
        .iter()
        .map(|(rel, facts)| (rel.clone(), Facts::of(facts, relations.module_counts(rel).as_ref())))
        .collect()
}

/// The same join as the document's `context.files` slot: each file's facts
/// under the keys its front matter uses.
pub fn facts_context(facts: &HashMap<String, Facts>) -> Value {
    let mut out = serde_json::Map::with_capacity(facts.len());
    for (path, facts) in facts {
        out.insert(path.clone(), Value::Object(facts.to_map()));
    }
    Value::Object(out)
}
