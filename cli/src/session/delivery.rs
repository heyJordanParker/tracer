//! `Delivery`: one call's offers of extra context, compared with what the
//! agent was last shown, and recorded, with the lines and docs the call
//! printed, in one write once the text that shows them is flushed.
//!
//! The record keeps a fingerprint of each shown value, not the value: one
//! hash per line the value prints, a nested line under its dotted path
//! (`git.status`), so a changed value prints only its changed lines, every
//! command that shows a line shares it, and the record stays small.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

use super::record::{Edited, Hunk, Record, Seen};
use super::{active, hold, key_of, Held};
use crate::nested_memory::LoadedMemory;

/// What a call shows an agent once per context, until it changes: a
/// directory's entries, keyed by the directory; a file's facts and its
/// declaration rows, keyed by the file; a function's other call sites, keyed
/// by its file and name; the `repo_context:` footer, keyed by the
/// repository; a search's definitions-and-mentions line, keyed by the
/// repository and the word; the primer a session starts with, keyed by the
/// repository; a native search's matches under their declarations, keyed by
/// the file; the notice that an index part was left out, keyed by the
/// repository; and the docs queued behind a command's own docs, keyed by
/// `docs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Listing,
    Facts,
    Surface,
    CallSites,
    RepoContext,
    Signpost,
    Primer,
    Matches,
    IndexState,
    Queue,
}

/// What an offered value is to the agent beside the value it was last shown.
pub enum Offer {
    New,
    Same,
    /// Only the lines that changed, nested as in the value; a removed line
    /// is `null`.
    Changed(Map<String, Value>),
}

/// One call's offers and the values they found new or changed, not yet
/// recorded as shown. The first offer that isn't `Same` takes the agent's
/// lock, and every later offer in the call compares under it until the
/// delivery commits or drops, so a call beside it compares against what it
/// commits. Dropped uncommitted, it records nothing.
///
/// The record is read once before the lock and once under it, never per
/// offer, so a listing of a thousand rows reads it twice.
#[must_use = "commit the delivery once the text that shows it is flushed"]
#[derive(Default)]
pub struct Delivery {
    values: Vec<(Kind, String, Value)>,
    seen: Vec<Seen>,
    sent: Vec<(Vec<LoadedMemory>, String)>,
    edited: Vec<Edited>,
    held: Option<Held>,
    /// The record as last read: before the lock while `held` is `None`,
    /// under it once `held` is set.
    record: Option<Record>,
}

impl Delivery {
    /// What `value` is to this agent as `key`'s `kind`, against what it was
    /// last shown, this call's earlier offers included. Always `New` without
    /// a session, so standalone use shows everything every time. An offer
    /// found `Same` before the delivery holds the lock reads the record and
    /// takes no lock.
    ///
    /// `value` carries only the lines the call prints: a key it leaves out
    /// is unknown, never removed. A nested mapping is the whole mapping, so
    /// a line missing from it was removed; a dotted key (`git.status`) is
    /// one line of a mapping the call does not print whole.
    pub fn offer(&mut self, kind: Kind, key: &str, value: &Value) -> Offer {
        if !active() {
            return Offer::New;
        }
        let compare = |record: &Record, pending: &[(Kind, String, Value)]| {
            let stored = record.shown.get(&kind).and_then(|keys| keys.get(key));
            let mut earlier = pending.iter().filter(|(offered, name, _)| *offered == kind && name == key).peekable();
            if earlier.peek().is_none() {
                return compared(stored, value);
            }
            let last = earlier.fold(stored.cloned(), |last, (_, _, value)| Some(merged(last, value)));
            compared(last.as_ref(), value)
        };
        if self.held.is_none() {
            let snapshot = self.record.get_or_insert_with(Record::load);
            if let Offer::Same = compare(snapshot, &self.values) {
                return Offer::Same;
            }
            let Some(held) = hold() else {
                return Offer::New;
            };
            self.held = Some(held);
            // A call beside this one may have committed before the lock was taken.
            self.record = Some(Record::load());
        }
        let record = self.record.get_or_insert_with(Record::load);
        let comparison = compare(record, &self.values);
        if !matches!(comparison, Offer::Same) {
            self.values.push((kind, key.to_string(), value.clone()));
        }
        comparison
    }

    /// Take the agent's lock now, before reading what the agent holds, so a
    /// call beside this one reads it only once this delivery commits.
    pub fn hold(&mut self) {
        if self.held.is_some() || !active() {
            return;
        }
        if let Some(held) = hold() {
            self.held = Some(held);
            self.record = Some(Record::load());
        }
    }

    /// Take back this call's offers of `key`'s `kind`: the value did not
    /// print after all, so it stays unshown.
    pub fn withdraw(&mut self, kind: Kind, key: &str) {
        self.values.retain(|(offered, name, _)| !(*offered == kind && name == key));
    }

    /// Lines of `path` the call prints, at the content `hash` names:
    /// `spans` are 1-based inclusive ranges, `None` the whole file.
    pub fn seen(
        &mut self,
        path: &Path,
        source: &str,
        hash: &str,
        size: usize,
        total_lines: usize,
        spans: &[Option<(usize, usize)>],
    ) {
        if !active() || spans.is_empty() {
            return;
        }
        self.seen.push(Seen {
            key: key_of(path),
            visible_as: path.to_string_lossy().to_string(),
            source: source.to_string(),
            hash: hash.to_string(),
            size,
            total_lines,
            spans: spans.to_vec(),
        });
    }

    /// Docs the call prints whole.
    pub fn sent(&mut self, memories: &[LoadedMemory], source: &str) {
        if active() && !memories.is_empty() {
            self.sent.push((memories.to_vec(), source.to_string()));
        }
    }

    /// The agent's own edit of `path`, now `content`: `hunks` are its
    /// changes, `None` when it wrote the whole file.
    pub fn edited(&mut self, path: &Path, source: &str, content: &str, hunks: Option<Vec<Hunk>>) {
        if !active() {
            return;
        }
        self.edited.push(Edited {
            key: key_of(path),
            visible_as: path.to_string_lossy().to_string(),
            source: source.to_string(),
            hash: super::content_hash(content),
            size: content.len(),
            total_lines: content.lines().count(),
            hunks,
        });
    }

    /// Record every offered value as shown, with the lines and docs the call
    /// printed, in one write. Call it only after the text that shows them is
    /// flushed, so a call that dies first records nothing; past the call's
    /// deadline its host threw the text away, so nothing is recorded either.
    pub fn commit(self) {
        if self.values.is_empty() && self.seen.is_empty() && self.sent.is_empty() && self.edited.is_empty() {
            return;
        }
        if !crate::process::before_deadline() {
            return;
        }
        let read_under_lock = self.held.is_some();
        let Some(_held) = self.held.or_else(hold) else {
            return;
        };
        let mut record = match self.record.filter(|_| read_under_lock) {
            Some(record) => record,
            None => Record::load(),
        };
        for (kind, key, value) in &self.values {
            let shown = record.shown.entry(*kind).or_default();
            let print = merged(shown.remove(key), value);
            shown.insert(key.clone(), print);
        }
        for (memories, source) in &self.sent {
            record.sent(memories, source);
        }
        for edited in self.edited {
            record.edited(edited);
        }
        for seen in self.seen {
            record.seen(seen);
        }
        record.save();
    }
}

/// A value's fingerprint: a mapping becomes a flat mapping of one hash per
/// line, a nested line under its dotted path; anything else is one hash.
fn fingerprint(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return hash(value);
    };
    let mut print = Map::new();
    for (key, line) in map {
        match line {
            Value::Object(inner) => {
                for (name, leaf) in inner {
                    print.insert(format!("{key}.{name}"), hash(leaf));
                }
            }
            leaf => {
                print.insert(key.clone(), hash(leaf));
            }
        }
    }
    Value::Object(print)
}

fn hash(value: &Value) -> Value {
    let digest = Sha256::digest(value.to_string().as_bytes());
    Value::String(hex::encode(&digest[..8]))
}

/// What `value` is beside the fingerprint `last`.
fn compared(last: Option<&Value>, value: &Value) -> Offer {
    match (last, value) {
        (None, _) => Offer::New,
        (Some(Value::Object(last)), Value::Object(new)) => {
            let lines = changed(last, new);
            if lines.is_empty() {
                Offer::Same
            } else {
                Offer::Changed(lines)
            }
        }
        (Some(last), _) if *last == fingerprint(value) => Offer::Same,
        (Some(_), _) => Offer::New,
    }
}

/// The lines of `new` whose fingerprint differs from the flat `last`: a key
/// of `new`, or one key inside one of its mappings, with a line gone from a
/// mapping as `null`.
fn changed(last: &Map<String, Value>, new: &Map<String, Value>) -> Map<String, Value> {
    let mut changed = Map::new();
    for (key, line) in new {
        match line {
            Value::Object(inner) => {
                let prefix = format!("{key}.");
                let mut lines: Map<String, Value> = inner
                    .iter()
                    .filter(|(name, leaf)| last.get(&format!("{prefix}{name}")) != Some(&hash(leaf)))
                    .map(|(name, leaf)| (name.clone(), leaf.clone()))
                    .collect();
                for stored in last.keys().filter_map(|stored| stored.strip_prefix(&prefix)) {
                    if !inner.contains_key(stored) {
                        lines.insert(stored.to_string(), Value::Null);
                    }
                }
                if !lines.is_empty() {
                    changed.insert(key.clone(), Value::Object(lines));
                }
            }
            leaf if last.get(key) != Some(&hash(leaf)) => {
                changed.insert(key.clone(), leaf.clone());
            }
            _ => {}
        }
    }
    changed
}

/// The fingerprint `last` once `value` is shown: each key `value` carries
/// replaces the stored line, or with a mapping every stored line under it,
/// and every key it leaves out keeps its stored line.
fn merged(last: Option<Value>, value: &Value) -> Value {
    match (last, value, fingerprint(value)) {
        (Some(Value::Object(mut last)), Value::Object(new), Value::Object(print)) => {
            for key in new.keys() {
                let prefix = format!("{key}.");
                last.retain(|stored, _| stored != key && !stored.starts_with(&prefix));
            }
            last.extend(print);
            Value::Object(last)
        }
        (_, _, print) => print,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shown(value: &Value) -> Value {
        merged(None, value)
    }

    #[test]
    fn a_key_left_out_of_the_offer_is_unknown_not_removed() {
        let last = shown(&json!({"lines": 7, "imported_by": 2, "git": {"status": "unmodified"}}));
        let partial = json!({"lines": 7, "git": {"status": "unmodified"}});
        assert!(matches!(compared(Some(&last), &partial), Offer::Same));
    }

    #[test]
    fn a_line_removed_from_a_mapping_shows_as_null() {
        let last = shown(&json!({"git": {"status": "added", "renamed_from": "a.py"}}));
        let Offer::Changed(lines) = compared(Some(&last), &json!({"git": {"status": "added"}})) else {
            panic!("not changed")
        };
        assert_eq!(Value::Object(lines), json!({"git": {"renamed_from": null}}));
    }

    #[test]
    fn a_partial_offer_keeps_the_lines_it_left_out() {
        let stored = merged(Some(shown(&json!({"lines": 7, "imported_by": 2}))), &json!({"lines": 8}));
        assert!(matches!(compared(Some(&stored), &json!({"lines": 8, "imported_by": 2})), Offer::Same));
    }

    #[test]
    fn a_headline_after_the_whole_facts_is_the_same() {
        let stored = shown(&json!({"lines": 7, "git": {"status": "modified", "commits": 3}}));
        let headline = json!({"lines": 7, "git.status": "modified"});
        assert!(matches!(compared(Some(&stored), &headline), Offer::Same));
    }

    #[test]
    fn the_whole_facts_after_a_headline_print_only_the_lines_it_did_not_show() {
        let stored = shown(&json!({"lines": 7, "git.status": "modified"}));
        let whole = json!({"lines": 7, "git": {"status": "modified", "commits": 3}});
        let Offer::Changed(lines) = compared(Some(&stored), &whole) else { panic!("not changed") };
        assert_eq!(Value::Object(lines), json!({"git": {"commits": 3}}));
    }

    #[test]
    fn a_changed_leaf_value_is_new() {
        let line = json!("dispatch: 1 definition");
        assert!(matches!(compared(Some(&shown(&line)), &json!("dispatch: 2 definitions")), Offer::New));
        assert!(matches!(compared(Some(&shown(&line)), &line), Offer::Same));
    }
}
