//! The agent's record, `<agent>/record.json`: the values it was shown, and
//! every file it holds lines of, with the doc attribution `trace docs status`
//! reports. A record of another `VERSION` reads as empty.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use super::delivery::Kind;
use super::{agent_dir, hold, key_of, write_atomically};
use crate::nested_memory::LoadedMemory;

const VERSION: u32 = 1;
const FILE: &str = "record.json";

static SNAPSHOT: Mutex<Option<Arc<Record>>> = Mutex::new(None);

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Record {
    version: u32,
    /// Each shown value's fingerprint, by kind and subject (`delivery.rs`).
    #[serde(default)]
    pub(super) shown: BTreeMap<Kind, BTreeMap<String, Value>>,
    /// Every file the agent was sent or read, keyed by absolute path.
    #[serde(default)]
    files: BTreeMap<String, Held>,
}

/// One file the agent holds lines of, at the content it got them from.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Held {
    hash: String,
    /// `doc_injection` for a doc sent whole, `read_file` for a file read.
    kind: String,
    /// The command or hook that delivered it.
    source: String,
    size: usize,
    visible_as: String,
    /// The lines read, when the file arrived through reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    coverage: Option<ReadCoverage>,
    /// Sent whole at `hash`, so reading part of it later never makes it partial.
    #[serde(default)]
    whole: bool,
    /// One short hash per line of a doc sent whole, so a changed doc sends
    /// only its changed lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    lines: Vec<String>,
}

impl Held {
    fn complete(&self) -> bool {
        self.whole || self.coverage.as_ref().is_none_or(ReadCoverage::complete)
    }

    /// Whether the agent still holds the file as it is on disk. Only a doc
    /// sent whole is checked, since it is the only file kept by its lines.
    fn current(&self, path: &str) -> bool {
        self.lines.is_empty()
            || std::fs::read_to_string(path).is_ok_and(|content| super::content_hash(&content) == self.hash)
    }
}

/// One short hash per line of `content`.
pub fn line_hashes(content: &str) -> Vec<String> {
    use sha2::{Digest, Sha256};
    content.lines().map(|line| hex::encode(&Sha256::digest(line.as_bytes())[..4])).collect()
}

impl Record {
    pub(super) fn load() -> Record {
        agent_dir()
            .and_then(|dir| std::fs::read(dir.join(FILE)).ok())
            .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
            .filter(|record| record.version == VERSION)
            .unwrap_or_default()
    }

    /// The record as this process last read it, which every reader outside a
    /// `Delivery` shares, so one read serves every site that asks until this
    /// process saves the record or takes the agent's lock.
    fn snapshot() -> Arc<Record> {
        SNAPSHOT.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(|| Arc::new(Record::load())).clone()
    }

    pub(super) fn save(mut self) {
        self.version = VERSION;
        if let Some(dir) = agent_dir() {
            let _ = crate::timing::phase("session record", || {
                write_atomically(&dir.join(FILE), crate::jsonfmt::to_compact(&self).as_bytes())
            });
        }
        forget_snapshot();
    }
}

/// Drop the shared read of the record, so the next reader reads it again.
pub(super) fn forget_snapshot() {
    *SNAPSHOT.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Changes the agent's record under its lock. Without a session, or when the
/// lock can't be taken, nothing is recorded.
fn change<T>(edit: impl FnOnce(&mut Record) -> T) -> Option<T> {
    let _held = hold()?;
    let mut record = Record::load();
    let out = edit(&mut record);
    record.save();
    Some(out)
}

/// Accumulated line-read coverage for one file: `total_lines` at the
/// recorded content, and `read`, the union of every 1-based inclusive line
/// range read, kept sorted and disjoint so the covered count is the plain
/// sum of the ranges' lengths. A content change starts it over.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadCoverage {
    pub total_lines: usize,
    pub read: Vec<[usize; 2]>,
}

impl ReadCoverage {
    /// Lines read so far: the sum of the disjoint ranges' lengths.
    pub fn lines_read(&self) -> usize {
        self.read.iter().map(|[s, e]| e - s + 1).sum()
    }

    /// Fraction of the file's lines read, in `[0.0, 1.0]`. A zero-line file is
    /// trivially fully read.
    pub fn fraction(&self) -> f64 {
        if self.total_lines == 0 {
            1.0
        } else {
            self.lines_read() as f64 / self.total_lines as f64
        }
    }

    pub fn complete(&self) -> bool {
        self.lines_read() >= self.total_lines
    }

    pub fn first_unread(&self) -> usize {
        match self.read.first() {
            Some([1, end]) => end + 1,
            _ => 1,
        }
    }
}

/// Merge a 1-based inclusive `[start, end]` line range into a sorted, disjoint
/// set of ranges, coalescing any overlap OR adjacency so the set stays minimal
/// and the covered-line count is the plain sum of each range's length. Reading
/// 1–60 then 40–80 yields `[[1, 80]]` (80 lines), never a double-counted 1–60
/// plus 40–80.
fn merge_range(ranges: &mut Vec<[usize; 2]>, start: usize, end: usize) {
    let mut merged_start = start;
    let mut merged_end = end;
    let mut out: Vec<[usize; 2]> = Vec::with_capacity(ranges.len() + 1);
    let mut inserted = false;
    for &[s, e] in ranges.iter() {
        if e + 1 < merged_start {
            out.push([s, e]);
        } else if merged_end + 1 < s {
            if !inserted {
                out.push([merged_start, merged_end]);
                inserted = true;
            }
            out.push([s, e]);
        } else {
            merged_start = merged_start.min(s);
            merged_end = merged_end.max(e);
        }
    }
    if !inserted {
        out.push([merged_start, merged_end]);
    }
    *ranges = out;
}

/// Paths the agent holds whole: sent whole, or read to their last line. A
/// doc that changed since it was sent whole is not held.
pub fn loaded_paths() -> BTreeSet<String> {
    Record::snapshot()
        .files
        .iter()
        .filter(|(path, held)| held.complete() && held.current(path))
        .map(|(path, _)| path.clone())
        .collect()
}

/// Whether the agent holds `path` whole, at its current content.
pub fn holds(path: &Path) -> bool {
    let key = key_of(path);
    Record::snapshot().files.get(&key).is_some_and(|held| held.complete() && held.current(&key))
}

/// What the agent holds of a doc, at the content `hash` names.
pub enum DocState {
    /// Some of its lines.
    Partial { hash: String, coverage: ReadCoverage },
    /// All of it, by the hash of each line.
    Whole { hash: String, lines: Vec<String> },
}

/// Each doc the agent holds part of, or was sent whole.
pub fn doc_states() -> BTreeMap<String, DocState> {
    Record::snapshot()
        .files
        .iter()
        .filter_map(|(path, held)| {
            let state = if !held.complete() {
                DocState::Partial { hash: held.hash.clone(), coverage: held.coverage.clone()? }
            } else if !held.lines.is_empty() {
                DocState::Whole { hash: held.hash.clone(), lines: held.lines.clone() }
            } else {
                return None;
            };
            Some((path.clone(), state))
        })
        .collect()
}

/// Whether the agent read lines `start..=end` of `file_path` at `content_hash`.
pub fn has_read(file_path: &Path, content_hash: &str, start: usize, end: usize) -> bool {
    Record::snapshot().files.get(&key_of(file_path)).is_some_and(|held| {
        held.hash == content_hash
            && held.coverage.as_ref().is_some_and(|coverage| coverage.read.iter().any(|[s, e]| *s <= start && end <= *e))
    })
}

/// Record docs sent whole now, for a call that shows nothing else: the
/// harness loaded them, or a JSON document carried them.
pub fn record_emission(memories: &[LoadedMemory], source: &str) {
    let mut delivery = super::Delivery::default();
    delivery.sent(memories, source);
    delivery.commit();
}

/// A file the agent was shown lines of, as a `Delivery` queues it for its
/// commit.
pub(super) struct Seen {
    pub(super) key: String,
    pub(super) visible_as: String,
    pub(super) source: String,
    pub(super) hash: String,
    pub(super) size: usize,
    pub(super) total_lines: usize,
    /// 1-based inclusive line ranges, `None` for the whole file.
    pub(super) spans: Vec<Option<(usize, usize)>>,
}

impl Record {
    /// Docs sent whole. A doc already held at the same content keeps its
    /// first delivery's attribution.
    pub(super) fn sent(&mut self, memories: &[LoadedMemory], source: &str) {
        for memory in memories {
            let hash = super::content_hash(&memory.content);
            let held = self.files.entry(memory.path.clone()).or_default();
            held.whole = true;
            if held.hash != hash || held.lines.is_empty() {
                held.hash = hash;
                held.kind = "doc_injection".into();
                held.source = source.to_string();
                held.size = memory.size;
                held.visible_as = memory.relative_path.clone();
                held.lines = line_hashes(&memory.content);
            }
        }
    }

    /// Lines the agent was shown. Lines of the same content accumulate; a
    /// content change starts the coverage over at these lines.
    pub(super) fn seen(&mut self, seen: Seen) {
        let held = self.files.entry(seen.key).or_default();
        let first_touch = held.hash != seen.hash;
        let total_lines = seen.total_lines;
        let coverage = held.coverage.get_or_insert_with(ReadCoverage::default);
        if first_touch || coverage.total_lines != total_lines {
            coverage.total_lines = total_lines;
            coverage.read.clear();
        }
        for span in seen.spans {
            let (start, end) = match span {
                Some((s, e)) => (s.max(1), e.min(total_lines)),
                None => (1, total_lines),
            };
            if total_lines > 0 && start <= end {
                merge_range(&mut coverage.read, start, end);
            }
        }
        if first_touch {
            held.whole = false;
            held.lines.clear();
            held.hash = seen.hash;
            held.kind = "read_file".into();
            held.source = seen.source;
            held.size = seen.size;
            held.visible_as = seen.visible_as;
        }
    }
}

/// One change an edit made: `old_count` lines from `old_start` became
/// `new_count` lines from `new_start`, both 1-based. An insertion has no old
/// lines and starts at the old line it goes before; a deletion has no new
/// lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
}

/// An edit the agent made to a file, as a `Delivery` queues it for its
/// commit: the file's content after it, and its changes, `None` when the
/// agent wrote the whole file.
pub(super) struct Edited {
    pub(super) key: String,
    pub(super) visible_as: String,
    pub(super) source: String,
    pub(super) hash: String,
    pub(super) size: usize,
    pub(super) total_lines: usize,
    pub(super) hunks: Option<Vec<Hunk>>,
}

impl Record {
    /// The agent's own edit: the lines it held move with the edit, the lines
    /// it wrote are held too, and both are kept at the file's new content, so
    /// its own change never reads to it as another agent's.
    pub(super) fn edited(&mut self, edited: Edited) {
        let held = self.files.entry(edited.key).or_default();
        let before = held.coverage.take().map(|coverage| coverage.read).unwrap_or_default();
        let mut read = Vec::new();
        match &edited.hunks {
            None => read.push([1, edited.total_lines]),
            Some(hunks) => {
                for line in before.iter().flat_map(|[start, end]| *start..=*end) {
                    if let Some(moved) = moved_line(line, hunks) {
                        merge_range(&mut read, moved, moved);
                    }
                }
                for hunk in hunks.iter().filter(|hunk| hunk.new_count > 0) {
                    merge_range(&mut read, hunk.new_start, hunk.new_start + hunk.new_count - 1);
                }
            }
        }
        read.retain(|[start, _]| *start <= edited.total_lines);
        if let Some(last) = read.last_mut() {
            last[1] = last[1].min(edited.total_lines);
        }
        held.coverage = Some(ReadCoverage { total_lines: edited.total_lines, read });
        held.whole = false;
        held.lines.clear();
        held.hash = edited.hash;
        held.kind = "read_file".into();
        held.source = edited.source;
        held.size = edited.size;
        held.visible_as = edited.visible_as;
    }
}

/// Where old line `line` sits after `hunks`, sorted by `old_start`; `None`
/// when the edit replaced it.
fn moved_line(line: usize, hunks: &[Hunk]) -> Option<usize> {
    let mut delta: isize = 0;
    for hunk in hunks {
        if line < hunk.old_start {
            break;
        }
        if line < hunk.old_start + hunk.old_count {
            return None;
        }
        delta += hunk.new_count as isize - hunk.old_count as isize;
    }
    line.checked_add_signed(delta)
}

/// Clear everything the record says the agent holds, because its context
/// dropped it, so the next call shows each value again. Returns how many
/// files it held; 0, writing nothing, with no record.
pub fn reset() -> usize {
    if agent_dir().is_none_or(|dir| !dir.join(FILE).is_file()) {
        return 0;
    }
    change(|record| {
        let cleared = record.files.len();
        *record = Record::default();
        cleared
    })
    .unwrap_or(0)
}

/// One entry in "what the agent holds right now".
#[derive(Debug, Clone, Serialize)]
pub struct LoadedEntry {
    pub path: String,
    pub visible_as: String,
    pub kind: String,
    pub size: usize,
    pub content_hash: String,
    pub source: String,
    /// The file's line count at the recorded content; 0 for a doc only ever
    /// sent whole.
    pub total_lines: usize,
    /// Lines the agent has read: the union of every read range.
    pub lines_read: usize,
    /// Fraction of the file's lines read, in `[0.0, 1.0]`.
    pub read_fraction: f64,
}

/// Every file the record holds, with its attribution and coverage.
pub fn loaded_entries() -> Vec<LoadedEntry> {
    Record::snapshot()
        .files
        .iter()
        .map(|(path, held)| LoadedEntry {
            total_lines: held.coverage.as_ref().map_or(0, |coverage| coverage.total_lines),
            lines_read: held.coverage.as_ref().map_or(0, ReadCoverage::lines_read),
            read_fraction: held.coverage.as_ref().map_or(0.0, ReadCoverage::fraction),
            path: path.clone(),
            visible_as: held.visible_as.clone(),
            kind: held.kind.clone(),
            size: held.size,
            content_hash: held.hash.clone(),
            source: held.source.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage from a single sequence of merged reads.
    fn cov(total: usize, reads: &[(usize, usize)]) -> ReadCoverage {
        let mut c = ReadCoverage {
            total_lines: total,
            read: Vec::new(),
        };
        for &(s, e) in reads {
            merge_range(&mut c.read, s, e);
        }
        c
    }

    #[test]
    fn single_partial_read_is_half() {
        let c = cov(100, &[(1, 50)]);
        assert_eq!(c.lines_read(), 50);
        assert_eq!(c.fraction(), 0.5);
    }

    #[test]
    fn two_non_overlapping_reads_reach_full() {
        let c = cov(100, &[(1, 50), (51, 100)]);
        assert_eq!(c.read, vec![[1, 100]]);
        assert_eq!(c.lines_read(), 100);
        assert_eq!(c.fraction(), 1.0);
    }

    #[test]
    fn overlapping_reads_count_the_union_not_double() {
        let c = cov(100, &[(1, 60), (40, 80)]);
        assert_eq!(c.read, vec![[1, 80]]);
        assert_eq!(c.lines_read(), 80);
    }

    #[test]
    fn whole_file_read_is_full() {
        let c = cov(42, &[(1, 42)]);
        assert_eq!(c.fraction(), 1.0);
    }

    #[test]
    fn never_read_file_is_zero() {
        let c = cov(10, &[]);
        assert_eq!(c.lines_read(), 0);
        assert_eq!(c.fraction(), 0.0);
    }

    #[test]
    fn an_edit_moves_the_lines_it_kept_and_holds_the_lines_it_wrote() {
        let mut record = Record::default();
        record.files.insert("f".into(), Held { coverage: Some(cov(10, &[(1, 10)])), ..Held::default() });
        let hunks = vec![
            Hunk { old_start: 3, old_count: 2, new_start: 3, new_count: 1 },
            Hunk { old_start: 8, old_count: 0, new_start: 7, new_count: 3 },
        ];
        record.edited(Edited {
            key: "f".into(),
            visible_as: "f".into(),
            source: "agent_edit".into(),
            hash: "h".into(),
            size: 0,
            total_lines: 12,
            hunks: Some(hunks),
        });
        assert_eq!(record.files["f"].coverage.as_ref().unwrap().read, vec![[1, 12]]);
    }

    #[test]
    fn an_edit_holds_only_its_own_lines_of_a_file_never_read() {
        let mut record = Record::default();
        record.edited(Edited {
            key: "f".into(),
            visible_as: "f".into(),
            source: "agent_edit".into(),
            hash: "h".into(),
            size: 0,
            total_lines: 20,
            hunks: Some(vec![Hunk { old_start: 5, old_count: 1, new_start: 5, new_count: 2 }]),
        });
        assert_eq!(record.files["f"].coverage.as_ref().unwrap().read, vec![[5, 6]]);
    }

    #[test]
    fn out_of_order_reads_with_a_gap_stay_disjoint() {
        let c = cov(100, &[(60, 70), (1, 10)]);
        assert_eq!(c.read, vec![[1, 10], [60, 70]]);
        assert_eq!(c.lines_read(), 21);
    }
}
