//! The agent's session: who the agent is, and what its context holds.
//!
//! An agent's record lives at `<user temp>/tracer-<uid>/sessions/<session>/`,
//! keyed by absolute path, so a session that works across several
//! repositories keeps one record and one reset clears it. The user temp
//! folder is the operating system's, never `TMPDIR`: a harness that gives
//! its shell another `TMPDIR` than its hooks, as Claude Code's sandbox does,
//! would otherwise split one agent's record in two. A writer that can't
//! write there, such as that sandboxed shell, reads the record and records
//! nothing.
//!
//! - `<agent>/record.json` is the agent's record (`record.rs`).
//! - `<agent>.lock` beside it serializes every change to that record.
//! - `directories.json` is the session-wide directory baseline
//!   (`directories.rs`).
//!
//! A session folder untouched for 7 days is removed when a new session
//! starts writing. With no session id there is no record: everything is new
//! to a person at a terminal, and nothing is written.

mod delivery;
mod directories;
mod record;

pub use delivery::{Delivery, Kind, Offer};
pub use directories::at_session_start;
pub use record::{
    doc_states, has_read, holds, line_hashes, loaded_entries, loaded_paths, record_emission, reset,
    DocState, Hunk, LoadedEntry,
};

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const AGENT_ID_DEFAULT: &str = "root";

/// A session folder nobody wrote to for this long belongs to a session that
/// ended.
#[allow(non_upper_case_globals)]
const stale_session_age: Duration = Duration::from_secs(7 * 24 * 60 * 60);

static SESSION: OnceLock<String> = OnceLock::new();

/// Names the session from a hook's input, which carries the session id the
/// harness keys its own events by.
pub fn set_session(id: String) {
    let _ = SESSION.set(id);
}

/// The session id: a hook input's, else `AGENT_SESSION_ID`, the
/// harness-neutral carrier, then Codex's `CODEX_THREAD_ID`, then Claude
/// Code's `CLAUDE_CODE_SESSION_ID`. The innermost harness comes before the
/// outer, so a Codex run under a Claude Code launcher keys on its own thread.
pub fn session_id() -> Option<String> {
    SESSION.get().cloned().filter(|id| !id.is_empty()).or_else(|| {
        ["AGENT_SESSION_ID", "CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|id| !id.is_empty()))
    })
}

/// Whether a session id is set, so the agent has a record.
pub fn active() -> bool {
    session_id().is_some()
}

static AGENT: OnceLock<String> = OnceLock::new();

/// Name the agent from `--agent`, which a hook writes into a Subagent's own
/// `trace` commands: its shell carries the session id but no agent id.
pub fn set_agent(id: String) {
    let _ = AGENT.set(id);
}

/// The agent id: `--agent`, else `TRACER_AGENT_ID`, else `root`.
fn agent_id() -> String {
    AGENT
        .get()
        .cloned()
        .or_else(|| std::env::var("TRACER_AGENT_ID").ok())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| AGENT_ID_DEFAULT.to_string())
}

/// `<user temp>/tracer-<uid>/`, the folder that holds this user's tracer
/// state: every session's record and the background build's lock. `None`
/// when it can't be made, or when it isn't a folder this user owns: in a
/// temp folder every user shares, another user could make it first.
pub(crate) fn user_folder() -> Option<PathBuf> {
    static FOLDER: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOLDER
        .get_or_init(|| {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
            let uid = unsafe { libc::getuid() };
            let folder = user_temp_dir().join(format!("tracer-{uid}"));
            let _ = fs::DirBuilder::new().mode(0o700).create(&folder);
            let metadata = fs::symlink_metadata(&folder).ok()?;
            if !metadata.is_dir() || metadata.uid() != uid {
                return None;
            }
            // A folder an earlier tracer made with the default mode is this
            // user's own, so it is made private rather than refused.
            if metadata.mode() & 0o077 != 0 {
                fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).ok()?;
            }
            Some(folder)
        })
        .clone()
}

/// The operating system's temporary folder for this user, whatever `TMPDIR`
/// says: macOS's `DARWIN_USER_TEMP_DIR`, else `/tmp`.
fn user_temp_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut buffer = [0u8; libc::PATH_MAX as usize];
        let written = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buffer.as_mut_ptr().cast(), buffer.len()) };
        if written > 0 && written <= buffer.len() {
            return PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..written - 1]));
        }
    }
    PathBuf::from("/tmp")
}

/// The folder every session's record sits in.
fn sessions_root() -> Option<PathBuf> {
    Some(user_folder()?.join("sessions"))
}

/// This session's folder, `None` without a session id.
fn session_dir() -> Option<PathBuf> {
    Some(sessions_root()?.join(session_id()?))
}

/// This agent's folder in the session, `None` without a session id.
fn agent_dir() -> Option<PathBuf> {
    Some(session_dir()?.join(agent_id()))
}

/// This session's folder, created for a write. Creating it is a session's
/// first write, which removes the folders of sessions untouched for
/// `stale_session_age`.
fn writable_session_dir() -> Option<PathBuf> {
    let dir = session_dir()?;
    if !dir.is_dir() {
        fs::create_dir_all(&dir).ok()?;
        sweep(&dir);
    }
    Some(dir)
}

fn sweep(current: &Path) {
    let Some(root) = sessions_root() else {
        return;
    };
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|written| written.elapsed().is_ok_and(|age| age > stale_session_age));
        if stale && entry.path() != current {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// The agent's lock, `<agent>.lock` beside its folder, held until the
/// returned guard drops. Re-entrant within the process: a record change made
/// while a `Delivery` holds the lock takes it again rather than waiting on
/// itself. `None` without a session, or when the lock can't be taken.
pub(crate) fn hold() -> Option<Held> {
    let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if held.0 == 0 {
        let dir = writable_session_dir()?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(format!("{}.lock", agent_id())))
            .ok()?;
        crate::timing::phase("lock session", || {
            rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        })
        .ok()?;
        held.1 = Some(file);
        held.2 = crate::timing::start();
        // Another process may have saved the record while this one waited.
        record::forget_snapshot();
    }
    held.0 += 1;
    Some(Held(()))
}

/// How many guards hold the agent's lock, its descriptor, and when it was
/// taken, for `TRACE_TIMING`'s `held session`.
static HELD: Mutex<(usize, Option<fs::File>, Option<std::time::Instant>)> = Mutex::new((0, None, None));

/// The agent's lock, held until dropped.
pub(crate) struct Held(());

impl Drop for Held {
    fn drop(&mut self) {
        let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        held.0 -= 1;
        if held.0 == 0 {
            // Closing the descriptor releases the flock.
            held.1 = None;
            if let Some(taken) = held.2.take() {
                eprintln!("timing held session {}", taken.elapsed().as_micros());
            }
        }
    }
}

/// Writes `bytes` to `path` through a temporary file in the same folder, so
/// a reader never sees half a file.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().expect("a record path has a parent");
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new().prefix(".record.").tempfile_in(parent)?;
    temp.write_all(bytes)?;
    temp.persist(path).map(|_| ()).map_err(|error| error.error)
}

pub fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

/// A file's absolute path as records key it.
fn key_of(path: &Path) -> String {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().to_string()
}
