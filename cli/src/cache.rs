//! Worktree-anchored disk cache for the repo-state namespace.
//!
//! `.tracer-cache/file/schema{SCHEMA_VERSION}/{key}.json` at the worktree
//! root — the main-repo root for a normal checkout, or the linked-worktree's
//! own root for a `git worktree add` checkout. A tracer cache exists ONLY at
//! a worktree root. Outside any worktree, reads still return live results but
//! nothing persists — the `cache::save` chokepoint hard-gates on
//! `worktree_root_for` and no-ops when it returns `None`.
//!
//! Each schema's directory holds two kinds of entry, both JSON written as a
//! single line (no indent, the `jsonfmt` byte format) via `save`:
//! content-addressed per-file entries, which are immutable, and repo-wide
//! entries, each swept by `evict_prefixed` so a superseded key leaves nothing
//! behind. Another schema's directory stays until no build has written in it
//! for `stale_schema_age`. Session records are not cache: `crate::session`
//! keeps them outside every repository.
//!
//! File cache key:
//!   sha256("v{SCHEMA_VERSION}|ccn:{backend}\0" + contents + "\0" + relpath)

use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CACHE_DIR_NAME: &str = ".tracer-cache";
pub const NAMESPACE_FILE: &str = "file";
/// `.tracer-cache/tmp/`: the files a child process reads its input from.
pub const TEMPORARY_DIR_NAME: &str = "tmp";

/// One index family's cross-process lock, `file/schema<N>/.<family>.lock`,
/// held with an exclusive `flock` across that family's update. A process
/// that finds its index fresh never takes it; one that finds it stale takes
/// it, re-reads the index the holder just wrote, and does only what is left.
/// Without it eight concurrent calls after one saved file each rebuilt the
/// index and overwrote each other's write. A build at another schema writes
/// none of these entries, so it holds its own locks and never waits here.
///
/// Families nest one way. The relations update takes the extraction side's
/// locks while it holds its own: the listing, mtime, scc, and ctags maps,
/// then directories and symbols. Every other family takes no lock while it
/// holds its own. So a leaf updates during a long relations build, and the
/// order can't deadlock.
///
/// Reentrant per process: `flock` on a second descriptor in the same process
/// would block forever, so the depth counter hands a nested caller the lock
/// the process already holds.
struct Held {
    family: String,
}

/// Each family whose lock this process holds: how deep, and the descriptor.
#[allow(non_upper_case_globals)]
static held_locks: std::sync::Mutex<Vec<(String, usize, fs::File)>> = std::sync::Mutex::new(Vec::new());

/// The lock wait ran past the index's limit.
struct TimedOut;

/// `family`'s lock, waiting for it until `limit`. `Ok(None)` when there is no
/// cache to lock, so the update runs without one.
fn lock(repo_root: &Path, family: &str, limit: Option<std::time::Instant>) -> Result<Option<Held>, TimedOut> {
    let held = || held_locks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = held().iter_mut().find(|(name, ..)| name == family) {
        entry.1 += 1;
        return Ok(Some(Held { family: family.to_string() }));
    }
    // The wait runs without `held_locks`, so this process can still release
    // the families it holds while another process holds this one.
    let Some(file) = schema_directory(NAMESPACE_FILE, repo_root).ok().and_then(|dir| {
        fs::OpenOptions::new().write(true).create(true).truncate(false).open(dir.join(format!(".{family}.lock"))).ok()
    }) else {
        return Ok(None);
    };
    let taken = crate::timing::phase(&format!("lock {family}"), || match limit {
        None => rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).is_ok(),
        Some(limit) => loop {
            if rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok() {
                break true;
            }
            if std::time::Instant::now() >= limit {
                break false;
            }
            std::thread::sleep(Duration::from_millis(5));
        },
    });
    if !taken {
        return match limit {
            Some(_) => Err(TimedOut),
            None => Ok(None),
        };
    }
    held().push((family.to_string(), 1, file));
    Ok(Some(Held { family: family.to_string() }))
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut held = held_locks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(at) = held.iter().position(|(name, ..)| *name == self.family) {
            held[at].1 -= 1;
            if held[at].1 == 0 {
                // Closing the descriptor releases the flock.
                held.remove(at);
            }
        }
    }
}

/// Remove the stale schemas and the leftovers of killed calls. The listing
/// update runs it, since it runs whenever the tree changed, and so do
/// `trace cache build` and `cache clear`.
pub fn sweep(repo_root: &Path) {
    if let Ok(dir) = schema_directory(NAMESPACE_FILE, repo_root) {
        evict_stale_schemas(&dir);
        sweep_leftovers(&dir, &repo_root.join(CACHE_DIR_NAME).join(TEMPORARY_DIR_NAME));
    }
}

/// Bump whenever extraction, the `FileFacts` shape, or a repo-wide index
/// shape changes — a new schema writes into its own directory, so old
/// entries become unreachable automatically.
pub const SCHEMA_VERSION: u32 = 53;

/// A build in use writes its schema's git-activity key at least once a day,
/// because that key carries the date, so a week without a write means no build
/// at that schema ran through a weekend and the days around it.
#[allow(non_upper_case_globals)]
const stale_schema_age: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A temporary file lives for one write or one `ctags` run, well under a
/// second, so one an hour old belongs to a trace that was killed mid-call.
#[allow(non_upper_case_globals)]
const leftover_age: Duration = Duration::from_secs(60 * 60);

pub fn worktree_root_for(path: &Path) -> Option<PathBuf> {
    let mut current = cwd_of(path);
    loop {
        if current.file_name().is_some_and(|name| name == ".git") {
            return None;
        }
        let git = current.join(".git");
        if git.is_dir()
            || fs::read_to_string(&git)
                .ok()
                .is_some_and(|contents| contents.starts_with("gitdir:"))
        {
            return Some(current);
        }
        current = current.parent()?.to_path_buf();
    }
}

pub fn git_dir(root: &Path) -> Option<PathBuf> {
    let git = root.join(".git");
    if git.is_dir() {
        return Some(git);
    }
    let contents = fs::read_to_string(&git).ok()?;
    let target = contents.strip_prefix("gitdir:")?.trim();
    let target = PathBuf::from(target);
    Some(if target.is_absolute() {
        target
    } else {
        root.join(target)
    })
}

/// Non-persisting display root for read paths that need *some* base for
/// relative-path rendering when `path` lies outside any worktree. Never
/// pass this to `cache::save` — its hard-gate on `worktree_root_for`
/// already no-ops if you do, but the design contract is: read paths use
/// this; write paths gate on the worktree resolver themselves.
pub(crate) fn display_root(path: &Path) -> PathBuf {
    cwd_of(path)
}

fn cwd_of(path: &Path) -> PathBuf {
    let abs = path.canonicalize().unwrap_or_else(|_| absolutize(path));
    if abs.is_dir() {
        abs
    } else {
        abs.parent().map(|p| p.to_path_buf()).unwrap_or(abs)
    }
}

pub fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn cache_root(repo_root: &Path) -> Result<PathBuf> {
    // Worktree gate: a `.tracer-cache/` directory exists only at a worktree
    // root. The cheap-and-correct check is `<repo_root>/.git` — every
    // worktree (main or linked) has a `.git` entry at its own root (a
    // directory for the main repo, a file for a linked worktree), and
    // nothing else does. Using a filesystem stat instead of a fresh git
    // subprocess keeps the per-file warm-read path free of an extra
    // `git rev-parse` per call. Reads via `load` cleanly miss; writes via
    // `save` are already gated separately. The gate stops scattered
    // `.tracer-cache/` dirs from materializing under cwd when tracer is
    // invoked outside any git repo.
    if git_dir(repo_root).is_none() {
        anyhow::bail!("cache_root: not a worktree root: {}", repo_root.display());
    }
    let dir = repo_root.join(CACHE_DIR_NAME);
    fs::create_dir_all(&dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        fs::write(ignore, "*\n")?;
    }
    Ok(dir)
}

fn schema_directory(namespace: &str, repo_root: &Path) -> Result<PathBuf> {
    let dir = cache_root(repo_root)?
        .join(namespace)
        .join(format!("schema{SCHEMA_VERSION}"));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Relative path from `repo_root` (both canonicalized), falling back to the
/// absolute path string when `path` is not under `repo_root`.
pub fn relative_to_root(path: &Path, repo_root: &Path) -> String {
    let abs = path.canonicalize().unwrap_or_else(|_| absolutize(path));
    let root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| absolutize(repo_root));
    match abs.strip_prefix(&root) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        Err(_) => abs.to_string_lossy().to_string(),
    }
}

/// sha256("v{SCHEMA}\0" + data + "\0" + relpath).
pub fn file_hash_from_bytes(data: &[u8], path: &Path, repo_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("v{SCHEMA_VERSION}\0").as_bytes());
    hasher.update(data);
    hasher.update(b"\0");
    hasher.update(relative_to_root(path, repo_root).as_bytes());
    hex::encode(hasher.finalize())
}

pub fn file_hash(path: &Path, repo_root: &Path) -> Result<String> {
    let data = fs::read(path)?;
    Ok(file_hash_from_bytes(&data, path, repo_root))
}

/// The entry's raw bytes, for an entry whose reader deserializes straight
/// into its own type. A `serde_json::Value` is a tree of boxed nodes, so
/// parsing a repo-wide index into one first costs more memory than the index
/// itself: the relations index reads this instead.
pub fn load_bytes(namespace: &str, key: &str, repo_root: &Path) -> Option<Vec<u8>> {
    let dir = schema_directory(namespace, repo_root).ok()?;
    let entry = dir.join(format!("{key}.json"));
    if !entry.exists() {
        return None;
    }
    if crate::timing::enabled() {
        let name = format!("decode {key}");
        return crate::timing::phase(&name, || fs::read(&entry).ok());
    }
    fs::read(&entry).ok()
}

/// Atomic save: write a temp file in the same dir, fsync, rename into place.
/// Serialized as a single line (no indent) via the `jsonfmt` byte format.
/// Returns whether the key was new, which is the one moment a caller sweeps
/// its superseded keys: a stable key exists after its first write, so the
/// whole-directory sweep stops running on every write.
///
/// Hard-gated on `worktree_root_for(repo_root)` returning `Some` — the
/// single chokepoint that enforces "tracer caches live only at a worktree
/// root, never anywhere else." When `repo_root` is not itself a worktree
/// root (callers that slipped a `display_root` through, or paths outside
/// any worktree), the save is a silent no-op so standalone tracer use
/// outside a git repo keeps working without persisting state.
///
/// Debug builds assert that `repo_root` has a `.git` ancestor — that's
/// the same invariant `worktree_root_for` enforces, but the assert fires
/// the moment a caller passes a non-worktree path so the wrong call site
/// is named in tests rather than silently no-op'd.
pub fn save<T: serde::Serialize + ?Sized>(
    namespace: &str,
    key: &str,
    value: &T,
    repo_root: &Path,
) -> Result<bool> {
    debug_assert!(
        git_dir(repo_root).is_some(),
        "cache::save called with non-worktree repo_root: {}",
        repo_root.display()
    );
    // Hard-gate: a `.git` entry at `repo_root` is the cheap, exact
    // worktree-root predicate (see `cache_root`). When the gate fails the
    // save is a silent no-op so standalone use outside a git repo keeps
    // working without persisting state.
    if git_dir(repo_root).is_none() {
        return Ok(false);
    }
    let dir = schema_directory(namespace, repo_root)?;
    let entry = dir.join(format!("{key}.json"));
    let new = !entry.exists();
    // Unique temp per call: process id plus a monotonic sequence number,
    // so concurrent rayon writers in get_batch never collide on one temp
    // path (a collision is both a concurrency hazard and a write
    // serialization point).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{key}.{}.{n}.tmp", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(crate::jsonfmt::to_compact(value).as_bytes())?;
        // No fsync: atomic rename alone gives the crash-consistency we
        // need — a lost cache entry just re-extracts. Skipping the
        // per-entry fsync eliminates the 1000+-fsync cold-`list` stall.
    }
    fs::rename(&tmp, &entry)?;
    Ok(new)
}

/// Remove one entry. The mtime index calls this for the per-file key it just
/// superseded, so the namespace holds one entry per file instead of one per
/// version ever written: one repository had reached 17,736 entries for 795 files.
pub fn remove(namespace: &str, key: &str, repo_root: &Path) {
    if let Ok(dir) = schema_directory(namespace, repo_root) {
        let _ = fs::remove_file(dir.join(format!("{key}.json")));
    }
}

/// Delete every `{prefix}*` entry in this schema's directory except `keep`.
///
/// The git-activity and deploy-presence maps are keyed by a repo state that
/// moves — HEAD, the 30-day cutoff date, the deploy-branch tips — so without
/// this sweep each move leaves its predecessor behind forever: 64 superseded
/// `git_activity__*` entries totalling 52 MB had accumulated in this repo.
/// Runs after the rename, so a crash mid-write never removes the only good
/// entry. A content entry's name is its hex hash, which no prefix matches.
fn evict_prefixed(namespace: &str, prefix: &str, keep: &str, repo_root: &Path) {
    let Ok(dir) = schema_directory(namespace, repo_root) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
            .is_some_and(|key| key != keep && key.starts_with(prefix))
        {
            let _ = fs::remove_file(&path);
        }
    }
}

/// A repo-wide index, read from its stored entry and brought up to date
/// under its family's lock.
pub trait Index {
    type Stored: serde::Serialize;
    type Change;
    fn key(&self) -> String;
    fn read(&self, bytes: &[u8]) -> Option<Self::Stored>;
    fn change(&self, stored: Option<&Self::Stored>) -> Option<Self::Change>;
    /// The index brought up to date, `None` when `limit` passed first.
    fn apply(&self, stored: Option<Self::Stored>, change: Self::Change) -> Option<Self::Stored>;
    /// Whether the result may be stored: a result built from a failed git
    /// call is used by this call and never stored.
    fn complete(&self, _stored: &Self::Stored) -> bool {
        true
    }
    /// When a call that only adds context to its answer stops waiting on
    /// this index, `None` for an answer, which waits until it is complete.
    fn limit(&self) -> Option<std::time::Instant> {
        None
    }
}

/// What `index` found.
pub enum Outcome<T> {
    /// The index, up to date.
    Ready(T),
    /// The stored index, behind the tree, because the limit passed first.
    Stale(T),
    /// There is nothing to index.
    Empty,
    /// The limit passed before any index was stored: leave out the parts
    /// that need it.
    NotReady,
}

impl<T> Outcome<T> {
    /// The index, current or stale.
    pub fn ready(self) -> Option<T> {
        match self {
            Outcome::Ready(stored) | Outcome::Stale(stored) => Some(stored),
            Outcome::Empty | Outcome::NotReady => None,
        }
    }
}

/// The index, up to date. When its limit passes first, whether waiting on
/// another process's lock or during its own update, the stored index
/// answers as `Stale`, and `NotReady` means nothing is stored yet. An update
/// its limit cancels stores nothing but the per-file extraction it finished,
/// which the next call picks up.
pub fn index<I: Index>(index: &I, repo_root: &Path) -> Outcome<I::Stored> {
    let key = index.key();
    let found = |stored: Option<I::Stored>| stored.map_or(Outcome::Empty, Outcome::Ready);
    let behind = |stored: Option<I::Stored>| stored.map_or(Outcome::NotReady, Outcome::Stale);
    let stored = read_entry(index, &key, repo_root);
    if index.change(stored.as_ref()).is_none() {
        return found(stored);
    }
    let Ok(_lock) = lock(repo_root, family(&key), index.limit()) else {
        return behind(stored);
    };
    let stored = read_entry(index, &key, repo_root);
    let Some(change) = index.change(stored.as_ref()) else {
        return found(stored);
    };
    match index.apply(stored, change) {
        Some(applied) => Outcome::Ready(store(index, &key, applied, repo_root)),
        None => behind(read_entry(index, &key, repo_root)),
    }
}

/// `change` applied to the stored index under its family's lock.
pub fn update<I: Index>(index: &I, repo_root: &Path, change: I::Change) -> Option<I::Stored> {
    let key = index.key();
    let _lock = lock(repo_root, family(&key), index.limit()).ok()?;
    let stored = read_entry(index, &key, repo_root);
    Some(store(index, &key, index.apply(stored, change)?, repo_root))
}

fn read_entry<I: Index>(index: &I, key: &str, repo_root: &Path) -> Option<I::Stored> {
    load_bytes(NAMESPACE_FILE, key, repo_root).and_then(|bytes| index.read(&bytes))
}

fn store<I: Index>(index: &I, key: &str, stored: I::Stored, repo_root: &Path) -> I::Stored {
    if index.complete(&stored) {
        if let Ok(true) = save(NAMESPACE_FILE, key, &stored, repo_root) {
            evict_prefixed(NAMESPACE_FILE, family(key), key, repo_root);
        }
    }
    stored
}

fn family(key: &str) -> &str {
    let name = key.split("__").next().unwrap_or(key);
    let unversioned = name.trim_end_matches(|character: char| character.is_ascii_digit());
    unversioned.strip_suffix("_v").unwrap_or(name)
}

/// Delete every other schema's directory beside `current` once nothing was
/// written in it for `stale_schema_age`, and the loose entries a build from
/// before schema directories left beside them once none of them was. A
/// directory's own modification time is its newest write, because `save`
/// renames every entry into it. Another schema's directory belongs to another
/// installed build that may still run, so removing it on sight made the two
/// builds rebuild each other's indexes on alternate calls.
fn evict_stale_schemas(current: &Path) {
    let Some(entries) = current.parent().and_then(|namespace| fs::read_dir(namespace).ok()) else {
        return;
    };
    let stale = |written: SystemTime| written.elapsed().is_ok_and(|age| age > stale_schema_age);
    let mut loose = (UNIX_EPOCH, Vec::new());
    for entry in entries.flatten() {
        let path = entry.path();
        let written = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            loose.0 = loose.0.max(written);
            loose.1.push(path);
        } else if path != current && stale(written) {
            let _ = fs::remove_dir_all(path);
        }
    }
    if stale(loose.0) {
        for path in loose.1 {
            let _ = fs::remove_file(path);
        }
    }
}

/// Delete what a killed trace left behind: a `save` temp (`*.tmp`) in this
/// schema's directory, and any file in `.tracer-cache/tmp/`. Signals and the
/// watchdog end trace without running destructors, so these outlive it.
fn sweep_leftovers(schema: &Path, temporary: &Path) {
    let old = |entry: &fs::DirEntry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|written| written.elapsed().is_ok_and(|age| age > leftover_age))
    };
    for entry in fs::read_dir(schema).into_iter().flatten().flatten() {
        if entry.path().extension().is_some_and(|extension| extension == "tmp") && old(&entry) {
            let _ = fs::remove_file(entry.path());
        }
    }
    for entry in fs::read_dir(temporary).into_iter().flatten().flatten() {
        if old(&entry) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub entry_count: usize,
    pub total_bytes: u64,
}

/// Entry count and total size of the `file/` namespace, every schema's
/// directory included. Absent means zero, not an error: a repo that has never
/// been read has no namespace directory.
pub fn stats(repo_root: &Path) -> Result<CacheStats> {
    let dir = cache_root(repo_root)?.join(NAMESPACE_FILE);
    let mut out = CacheStats {
        entry_count: 0,
        total_bytes: 0,
    };
    for e in walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| is_cache_entry(e.path()))
    {
        out.entry_count += 1;
        out.total_bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
    }
    Ok(out)
}

/// Delete every entry in the `file/` namespace, every schema's directory
/// included, returning the removed count. The directories and their locks
/// stay, so `clear_all` remains the only way to remove the cache tree.
pub fn clear(repo_root: &Path) -> Result<usize> {
    let dir = cache_root(repo_root)?.join(NAMESPACE_FILE);
    let mut removed = 0;
    for e in walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| is_cache_entry(e.path()))
    {
        fs::remove_file(e.path())?;
        removed += 1;
    }
    Ok(removed)
}

/// Remove the entire cache tree, returning the count of cache entries.
pub fn clear_all(repo_root: &Path) -> Result<usize> {
    let root = repo_root.join(CACHE_DIR_NAME);
    if !root.is_dir() {
        return Ok(0);
    }
    let count = walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| is_cache_entry(e.path()))
        .count();
    fs::remove_dir_all(&root)?;
    Ok(count)
}

/// Every persisted cache entry is a `.json`. Temp files and lock files are
/// excluded so `stats` / `clear` count only durable entries.
fn is_cache_entry(p: &Path) -> bool {
    p.extension().and_then(|x| x.to_str()) == Some("json")
}

#[cfg(test)]
mod tests {
    use super::{leftover_age, sweep_leftovers};
    use std::fs;
    use std::path::Path;
    use std::time::SystemTime;

    fn write(path: &Path, aged: bool) {
        let file = fs::File::create(path).unwrap();
        if aged {
            file.set_modified(SystemTime::now() - leftover_age * 2).unwrap();
        }
    }

    #[test]
    fn a_killed_calls_leftovers_are_swept_and_live_files_kept() {
        let root = tempfile::tempdir().unwrap();
        let (schema, temporary) = (root.path().join("schema"), root.path().join("tmp"));
        fs::create_dir_all(&schema).unwrap();
        fs::create_dir_all(&temporary).unwrap();
        write(&schema.join("key.123.0.tmp"), true);
        write(&schema.join("key.456.0.tmp"), false);
        write(&schema.join("key.json"), true);
        write(&temporary.join("tmpAbc.rb"), true);
        write(&temporary.join("tmpDef.rb"), false);

        sweep_leftovers(&schema, &temporary);

        assert!(!schema.join("key.123.0.tmp").exists());
        assert!(schema.join("key.456.0.tmp").exists());
        assert!(schema.join("key.json").exists());
        assert!(!temporary.join("tmpAbc.rb").exists());
        assert!(temporary.join("tmpDef.rb").exists());
    }
}
