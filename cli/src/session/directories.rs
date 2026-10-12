//! The session-wide directory baseline, `<session>/directories.json`: each
//! directory's import metrics when the session first surfaced it, so every
//! agent reviewing the same change sees the same before value. It survives a
//! context reset, because it records the repository, not the agent's context.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use super::{writable_session_dir, write_atomically};
use crate::relations::DirectoryMetrics;

const FILE: &str = "directories.json";

static BASELINES: OnceLock<Mutex<BTreeMap<String, DirectoryMetrics>>> = OnceLock::new();

/// What a directory's imports were when this session first surfaced it, only
/// where they differ from `current`: the counts then, and the directories it
/// now imports from or no longer does. `None` when nothing changed. The first
/// call for a directory records its baseline, keyed by its absolute path so
/// two repositories' `src/` never share one.
pub fn at_session_start(repo_root: &Path, dir: &str, current: &DirectoryMetrics) -> Option<Map<String, Value>> {
    let baseline = baseline(&repo_root.join(dir).to_string_lossy(), current);
    let mut since = Map::new();
    if baseline.imported_by != current.imported_by {
        since.insert("imported_by".into(), baseline.imported_by.into());
    }
    if baseline.imports != current.imports {
        since.insert("imports".into(), baseline.imports.into());
    }
    let added: Vec<&String> = current.imported_directories.difference(&baseline.imported_directories).collect();
    let removed: Vec<&String> = baseline.imported_directories.difference(&current.imported_directories).collect();
    if !added.is_empty() {
        since.insert("now_imports_from".into(), json!(added));
    }
    if !removed.is_empty() {
        since.insert("no_longer_imports_from".into(), json!(removed));
    }
    (!since.is_empty()).then_some(since)
}

/// The metrics this session recorded for `dir`, recording `current` on the
/// first call. Without a session, `current` itself.
fn baseline(dir: &str, current: &DirectoryMetrics) -> DirectoryMetrics {
    if !super::active() {
        return current.clone();
    }
    let memo = BASELINES.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(baseline) = memo.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(dir).cloned() {
        return baseline;
    }
    let Some(session) = writable_session_dir() else {
        return current.clone();
    };
    let path = session.join(FILE);
    let read = || -> BTreeMap<String, DirectoryMetrics> {
        fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    };
    let mut stored = read();
    if let Some(baseline) = stored.get(dir).cloned() {
        *memo.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = stored;
        return baseline;
    }
    let Some(_lock) = lock(&session.join("directories.lock")) else {
        return current.clone();
    };
    stored = read();
    let baseline = stored.entry(dir.to_string()).or_insert_with(|| current.clone()).clone();
    let _ = crate::timing::phase("session directories", || {
        write_atomically(&path, crate::jsonfmt::to_compact(&stored).as_bytes())
    });
    *memo.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = stored;
    baseline
}

fn lock(path: &Path) -> Option<fs::File> {
    let file = fs::OpenOptions::new().write(true).create(true).truncate(false).open(path).ok()?;
    crate::timing::phase("lock session directories", || {
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
    })
    .ok()?;
    Some(file)
}
