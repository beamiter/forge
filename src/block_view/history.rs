//! history — extracted from block_view (mechanical split, no logic changes)
//!
//! Persist the in-memory `block_data` deque to/from disk as length-prefixed
//! rkyv records (optional zstd). Truncate-on-save (not append) keeps the file
//! bounded, since the deque was already seeded from this file on startup.

use super::{
    estimated_finished_block_height_for_text, install_finished_block_selection, next_block_id,
    BlockData, FinishedBlock, TermView,
};
use crate::persistence::{self, PersistenceKey};
use gtk4::glib;
use gtk4::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CString, OsString};
use std::fs::{self, File, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_ENCODED_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_DECODED_RECORD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HISTORY_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_HISTORY_DECODED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_HISTORY_FRAMES: usize = 100_000;
const MAX_HISTORY_DECODE_DURATION: Duration = Duration::from_secs(5);
const MAX_SESSION_COMPONENT_BYTES: usize = 96;
const HISTORY_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SCANNED_HISTORY_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_HISTORY_PROMPT_BYTES: usize = 64 * 1024;
const MAX_HISTORY_COMMAND_BYTES: usize = crate::review_input::MAX_REVIEW_INPUT_BYTES;
const MAX_HISTORY_COMMAND_MARKUP_BYTES: usize = crate::review_input::MAX_REVIEW_INPUT_BYTES;
const MAX_HISTORY_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HISTORY_CWD_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryRevision {
    Missing,
    Present {
        device: u64,
        inode: u64,
        len: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
    },
}

impl HistoryRevision {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self::Present {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

pub(super) struct LoadedHistory {
    blocks: Arc<Vec<BlockData>>,
    total_loaded: usize,
    target_revision: HistoryRevision,
}

#[derive(Clone)]
enum HistoryLoadOutcome {
    Idle,
    Pending,
    Loaded(Arc<LoadedHistory>),
    Failed {
        kind: io::ErrorKind,
        message: Arc<str>,
    },
}

pub(super) struct HistoryLoadShared {
    outcome: Mutex<HistoryLoadOutcome>,
    revision: Mutex<Option<HistoryRevision>>,
    applied: AtomicBool,
    discarded: AtomicBool,
}

impl Default for HistoryLoadShared {
    fn default() -> Self {
        Self {
            outcome: Mutex::new(HistoryLoadOutcome::Idle),
            revision: Mutex::new(None),
            applied: AtomicBool::new(true),
            discarded: AtomicBool::new(false),
        }
    }
}

impl HistoryLoadShared {
    pub(super) fn discard(&self) {
        self.discarded.store(true, Ordering::Release);
        self.applied.store(true, Ordering::Release);
    }

    fn begin(&self) {
        *self
            .outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = HistoryLoadOutcome::Pending;
        self.discarded.store(false, Ordering::Release);
        self.applied.store(false, Ordering::Release);
    }

    fn complete(&self, result: &io::Result<Arc<LoadedHistory>>) {
        *self
            .revision
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            result.as_ref().ok().map(|loaded| loaded.target_revision);
        let outcome = match result {
            Ok(loaded) => HistoryLoadOutcome::Loaded(Arc::clone(loaded)),
            Err(error) => HistoryLoadOutcome::Failed {
                kind: error.kind(),
                message: Arc::from(error.to_string()),
            },
        };
        *self
            .outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = outcome;
    }

    fn outcome(&self) -> HistoryLoadOutcome {
        self.outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn revision(&self) -> Option<HistoryRevision> {
        *self
            .revision
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_revision(&self, revision: Option<HistoryRevision>) {
        *self
            .revision
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = revision;
    }
}

fn decode_zstd_bounded(data: &[u8], max_decoded_bytes: u64) -> io::Result<Vec<u8>> {
    let decoder = zstd::Decoder::new(data).map_err(|error| io::Error::other(error.to_string()))?;
    let mut decoded = Vec::new();
    decoder
        .take(max_decoded_bytes + 1)
        .read_to_end(&mut decoded)?;
    if decoded.len() as u64 > max_decoded_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("block history record expands beyond {max_decoded_bytes} bytes"),
        ));
    }
    Ok(decoded)
}

/// The record shape every save before round 8 used: `exit_code` was a bare
/// `i32`, so a command whose status the shell never reported was stored as a
/// fabricated `0`. Kept only so those files still decode; the archived layout
/// of the current `BlockData` differs and would otherwise reject every old
/// frame, silently dropping the user's saved history on upgrade.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct LegacyBlockDataV1 {
    id: u64,
    prompt: String,
    cmd: String,
    cmd_markup: Option<String>,
    output: String,
    exit_code: i32,
    estimated_height: i32,
    line_count: usize,
    start_time_ms: Option<u64>,
    end_time_ms: Option<u64>,
    duration_ms: Option<u64>,
    cwd: Option<String>,
    cols: u16,
}

impl From<LegacyBlockDataV1> for BlockData {
    fn from(legacy: LegacyBlockDataV1) -> Self {
        Self {
            id: legacy.id,
            prompt: legacy.prompt,
            cmd: legacy.cmd,
            cmd_markup: legacy.cmd_markup,
            output: legacy.output,
            // The legacy field cannot distinguish "exited 0" from "no status
            // reported"; both were written as 0. Some(0) preserves what the old
            // file actually says rather than re-guessing it.
            exit_code: Some(legacy.exit_code),
            estimated_height: legacy.estimated_height,
            line_count: legacy.line_count,
            start_time_ms: legacy.start_time_ms,
            end_time_ms: legacy.end_time_ms,
            duration_ms: legacy.duration_ms,
            cwd: legacy.cwd,
            cols: legacy.cols,
        }
    }
}

/// Current schema first, then the pre-round-8 layout. Order matters: a current
/// frame must never be squeezed through the legacy shape.
fn decode_rkyv_block(data: &[u8]) -> Option<BlockData> {
    rkyv::from_bytes::<BlockData, rkyv::rancor::Error>(data)
        .ok()
        .or_else(|| {
            rkyv::from_bytes::<LegacyBlockDataV1, rkyv::rancor::Error>(data)
                .ok()
                .map(BlockData::from)
        })
}

fn validate_block_fields(block: &BlockData) -> io::Result<()> {
    let valid = block.prompt.len() <= MAX_HISTORY_PROMPT_BYTES
        && block.cmd.len() <= MAX_HISTORY_COMMAND_BYTES
        && block
            .cmd_markup
            .as_ref()
            .is_none_or(|markup| markup.len() <= MAX_HISTORY_COMMAND_MARKUP_BYTES)
        && block.output.len() <= MAX_HISTORY_OUTPUT_BYTES
        && block
            .cwd
            .as_ref()
            .is_none_or(|cwd| cwd.len() <= MAX_HISTORY_CWD_BYTES);
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Block-history record exceeds a field-specific display/runtime limit",
        ))
    }
}

/// History frames predate an on-disk codec marker. Try the configured codec
/// first, then the alternate representation so toggling compression never makes
/// the previous session look corrupt.
fn decode_block_record(data: &[u8], prefer_compressed: bool) -> io::Result<(BlockData, usize)> {
    let decode_as = |compressed: bool| -> io::Result<(BlockData, usize)> {
        let decoded = if compressed {
            decode_zstd_bounded(data, MAX_DECODED_RECORD_BYTES)?
        } else {
            if data.len() as u64 > MAX_DECODED_RECORD_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "raw Block-history record exceeds the decoded record limit",
                ));
            }
            data.to_vec()
        };
        let decoded_len = decoded.len();
        let block = decode_rkyv_block(&decoded).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Block-history rkyv record",
            )
        })?;
        validate_block_fields(&block)?;
        Ok((block, decoded_len))
    };

    decode_as(prefer_compressed)
        .or_else(|_| decode_as(!prefer_compressed))
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Block-history frame is neither a valid raw nor bounded zstd record",
            )
        })
}

fn history_load_limit(lazy_load_threshold: usize, max_visible_blocks: usize) -> usize {
    lazy_load_threshold.min(max_visible_blocks)
}

/// Persisted IDs are process-local implementation details. Reusing them after a
/// restart collides with the global allocator (which starts from zero again), so
/// restore every record with a fresh runtime ID before exposing it to selection,
/// deletion, bookmarks, search, and export.
fn refresh_loaded_block_ids(blocks: &mut VecDeque<BlockData>) {
    for block in blocks {
        block.id = next_block_id();
    }
}

/// Expand the shell-style `~/` prefix used in configuration, but leave every
/// other tilde form alone (`~`, `~user/...`, and embedded tildes are literal).
fn expand_home_prefix_with(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

fn history_path(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    expand_home_prefix_with(path, home.as_deref())
}

/// Session-history files older than this are removed opportunistically after a
/// successful save. Closed tabs never delete their own file (the session id in
/// the window snapshot may be restored later), so orphans from tabs that were
/// never restored again would otherwise accumulate forever.
const STALE_SESSION_HISTORY_MAX_AGE: std::time::Duration =
    std::time::Duration::from_secs(30 * 24 * 3600);

/// Encode a session id injectively into filename-safe ASCII. Generated ids are
/// already safe and retain their old names; user-edited bytes use `~HH`
/// escapes. Pathological ids switch to a fixed SHA-256 name before they can
/// approach a filesystem's `NAME_MAX` limit. `~H` cannot be produced by the
/// escape grammar, so hashed names cannot collide with directly encoded ones.
fn sanitize_session_component(session_id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(session_id.len().min(MAX_SESSION_COMPONENT_BYTES));
    for &byte in session_id.as_bytes() {
        let safe = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
        let added = if safe { 1 } else { 3 };
        if encoded.len() + added > MAX_SESSION_COMPONENT_BYTES {
            let digest =
                glib::compute_checksum_for_data(glib::ChecksumType::Sha256, session_id.as_bytes())
                    .expect("GLib must support SHA-256");
            return format!("~H{digest}");
        }
        if safe {
            encoded.push(byte as char);
        } else {
            encoded.push('~');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

/// Per-tab history file derived from the configured path: `<stem>-<sid>.<ext>`
/// in the same directory. The configured path itself was previously shared by
/// every tab, so concurrent tabs overwrote each other's history on close
/// (last close wins); keying the file by the tab's persistent session id gives
/// each restored tab its own history.
fn per_session_history_path(base: &Path, session_id: &str) -> PathBuf {
    let sid = sanitize_session_component(session_id);
    let stem = base
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "blocks".to_string());
    let name = match base.extension() {
        Some(ext) => format!("{stem}-{sid}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{sid}"),
    };
    base.with_file_name(name)
}

/// Where to read history from: the tab's own file when it exists, otherwise
/// the legacy shared file (pre-split sessions saved there). Returns `None`
/// when neither exists yet.
fn choose_load_path(base: &Path, per_session: Option<&Path>) -> Option<PathBuf> {
    if let Some(session_path) = per_session {
        if session_path.exists() {
            return Some(session_path.to_path_buf());
        }
    }
    base.exists().then(|| base.to_path_buf())
}

/// Remove sibling per-session history files that have not been touched within
/// `max_age`. Only names matching this base's `<stem>-*.<ext>` shape are
/// candidates; `keep` (the file just written) always survives.
fn prune_stale_session_histories(base: &Path, keep: &Path, max_age: std::time::Duration) {
    let Some(parent) = base.parent() else {
        return;
    };
    let Some(stem) = base.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
        return;
    };
    let prefix = format!("{stem}-");
    let extension = base
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned());
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let now = SystemTime::now();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_SCANNED_HISTORY_DIRECTORY_ENTRIES {
            log::warn!(
                "stopped pruning Block-history directory {} after {} entries",
                parent.display(),
                MAX_SCANNED_HISTORY_DIRECTORY_ENTRIES
            );
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path == keep || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        if path
            .extension()
            .map(|ext| ext.to_string_lossy().into_owned())
            != extension
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= max_age);
        if stale {
            if let Err(error) = fs::remove_file(&path) {
                log::warn!("prune stale block history {}: {error}", path.display());
            }
        }
    }
}

fn temp_file_name(target: &Path) -> io::Result<OsString> {
    let file_name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("history path has no file name: {}", target.display()),
        )
    })?;
    let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    Ok(name)
}

fn lock_file_name(target: &Path) -> io::Result<OsString> {
    let file_name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("history path has no file name: {}", target.display()),
        )
    })?;
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(".lock");
    Ok(name)
}

fn parent_path(target: &Path) -> &Path {
    target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn relative_name_cstring(target: &Path, label: &str) -> io::Result<CString> {
    let name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} path has no file name: {}", target.display()),
        )
    })?;
    CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} file name contains NUL"),
        )
    })
}

fn os_name_cstring(name: &std::ffi::OsStr, label: &str) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} file name contains NUL"),
        )
    })
}

/// Create a private missing parent, then validate and retain its final inode.
/// A group/world-writable namespace cannot protect a persistent lock filename
/// or an atomic replacement from another uid replacing directory entries.
fn open_parent_directory(target: &Path) -> io::Result<File> {
    let parent = parent_path(target);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(parent)?;

    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(parent)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Block history parent is not a directory: {}",
                parent.display()
            ),
        ));
    }
    // SAFETY: geteuid has no preconditions and only reads process state.
    if metadata.uid() != unsafe { nix::libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Block history parent must be owned by the current user and not group/world writable: {}",
                parent.display()
            ),
        ));
    }
    Ok(directory)
}

fn validate_regular_user_file(file: &File, path: &Path, label: &str) -> io::Result<fs::Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} is not a regular file: {}", path.display()),
        ));
    }
    // SAFETY: geteuid has no preconditions and only reads process state.
    if metadata.uid() != unsafe { nix::libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} is not owned by the current user: {}",
                path.display()
            ),
        ));
    }
    if metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{label} has multiple hard links: {}", path.display()),
        ));
    }
    Ok(metadata)
}

fn open_history_file(path: &Path) -> io::Result<Option<(File, fs::Metadata)>> {
    let directory = open_parent_directory(path)?;
    open_history_file_in_directory(&directory, path)
}

fn open_history_file_in_directory(
    directory: &File,
    path: &Path,
) -> io::Result<Option<(File, fs::Metadata)>> {
    let name = relative_name_cstring(path, "Block history")?;
    // SAFETY: `name` and the retained directory descriptor remain live for
    // the call; ownership of a successful descriptor is transferred once.
    let descriptor = unsafe {
        nix::libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_RDONLY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        };
    }
    // SAFETY: `descriptor` is newly returned and uniquely owned.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = validate_regular_user_file(&file, path, "Block history")?;
    if metadata.len() > MAX_HISTORY_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!(
                "Block history is {} bytes, exceeding the {} byte limit: {}",
                metadata.len(),
                MAX_HISTORY_FILE_BYTES,
                path.display()
            ),
        ));
    }
    Ok(Some((file, metadata)))
}

fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    // SAFETY: file owns a live descriptor and flock retains no pointer.
    let result =
        unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error
        .raw_os_error()
        .is_some_and(|code| code == nix::libc::EAGAIN || code == nix::libc::EWOULDBLOCK)
    {
        Ok(false)
    } else {
        Err(error)
    }
}

fn lock_with_timeout(file: &File, deadline: Instant) -> io::Result<()> {
    loop {
        match try_lock_exclusive(file)? {
            true => return Ok(()),
            false if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            false => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for the Block-history lock",
                ));
            }
        }
    }
}

fn unlock(file: &File) {
    // SAFETY: file owns a live descriptor for this call.
    if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_UN) } != 0 {
        log::warn!(
            "failed to unlock Block-history file: {}",
            io::Error::last_os_error()
        );
    }
}

struct HistoryFileLock {
    directory: File,
    file: File,
}

impl HistoryFileLock {
    fn acquire(target: &Path) -> io::Result<Self> {
        Self::acquire_with_timeout(target, HISTORY_LOCK_TIMEOUT)
    }

    fn acquire_with_timeout(target: &Path, timeout: Duration) -> io::Result<Self> {
        let directory = open_parent_directory(target)?;
        let deadline = Instant::now() + timeout;
        // Lock the directory as well as the persistent lock inode. This keeps
        // cooperating writers serialized even if a stale process replaces the
        // lock filename between opens.
        lock_with_timeout(&directory, deadline)?;

        let lock_name = lock_file_name(target)?;
        let path = parent_path(target).join(&lock_name);
        let lock_name = os_name_cstring(&lock_name, "Block-history lock")?;
        // SAFETY: the name is relative to the retained validated directory;
        // ownership of a successful descriptor is transferred exactly once.
        let descriptor = unsafe {
            nix::libc::openat(
                directory.as_raw_fd(),
                lock_name.as_ptr(),
                nix::libc::O_CREAT
                    | nix::libc::O_RDWR
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK,
                0o600,
            )
        };
        if descriptor < 0 {
            let error = io::Error::last_os_error();
            unlock(&directory);
            return Err(error);
        }
        // SAFETY: `descriptor` is newly returned and uniquely owned.
        let file = unsafe { File::from_raw_fd(descriptor) };
        if let Err(error) = validate_regular_user_file(&file, &path, "Block-history lock") {
            unlock(&directory);
            return Err(error);
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        if let Err(error) = lock_with_timeout(&file, deadline) {
            unlock(&directory);
            return Err(error);
        }
        Ok(Self { directory, file })
    }
}

impl Drop for HistoryFileLock {
    fn drop(&mut self) {
        unlock(&self.file);
        unlock(&self.directory);
    }
}

/// Write a replacement beside `target`, sync it, then atomically rename it over
/// the old file. Keeping the temporary file in the same directory guarantees
/// that the rename cannot cross filesystems. A failed encoder leaves the old
/// history intact and removes its incomplete temporary file.
fn atomic_write(
    target: &Path,
    write_contents: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let parent_directory = open_parent_directory(target)?;
    atomic_write_in_directory(&parent_directory, target, write_contents)
}

fn atomic_write_in_directory(
    parent_directory: &File,
    target: &Path,
    write_contents: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let target_name = relative_name_cstring(target, "Block history")?;

    // Create, publish, and clean up relative to the same retained directory
    // inode. Swapping the parent pathname after validation cannot redirect a
    // write or make a temporary file escape into another namespace.
    let mut created = None;
    for _ in 0..128 {
        let temp_name = temp_file_name(target)?;
        let temp_name_c = os_name_cstring(&temp_name, "temporary Block history")?;
        // SAFETY: all names and the directory descriptor are live for the
        // call; a successful descriptor is returned to the caller.
        let descriptor = unsafe {
            nix::libc::openat(
                parent_directory.as_raw_fd(),
                temp_name_c.as_ptr(),
                nix::libc::O_WRONLY
                    | nix::libc::O_CREAT
                    | nix::libc::O_EXCL
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW,
                0o600,
            )
        };
        if descriptor >= 0 {
            created = Some((temp_name_c, descriptor));
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let (temp_name_c, descriptor) = created.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a temporary Block-history file",
        )
    })?;
    // SAFETY: `descriptor` is newly returned and uniquely owned.
    let mut temp = unsafe { File::from_raw_fd(descriptor) };
    let written = write_contents(&mut temp)
        .and_then(|()| temp.flush())
        .and_then(|()| temp.sync_all());
    drop(temp);
    if let Err(error) = written {
        // SAFETY: the name is relative to the retained directory and unlinkat
        // retains no pointers.
        unsafe {
            nix::libc::unlinkat(parent_directory.as_raw_fd(), temp_name_c.as_ptr(), 0);
        }
        return Err(error);
    }
    // SAFETY: source and destination are relative to the same retained
    // directory inode and renameat retains no pointers.
    if unsafe {
        nix::libc::renameat(
            parent_directory.as_raw_fd(),
            temp_name_c.as_ptr(),
            parent_directory.as_raw_fd(),
            target_name.as_ptr(),
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        // SAFETY: see the cleanup above.
        unsafe {
            nix::libc::unlinkat(parent_directory.as_raw_fd(), temp_name_c.as_ptr(), 0);
        }
        return Err(error);
    }

    // Persist the directory entry as well as the file contents. Directory
    // syncing is supported on the Unix platforms forge targets.
    parent_directory.sync_all()
}

fn push_bounded_back<T>(items: &mut VecDeque<T>, item: T, limit: usize) {
    if limit == 0 {
        return;
    }
    if items.len() == limit {
        items.pop_front();
    }
    items.push_back(item);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UndecodablePolicy {
    Skip,
    Reject,
}

struct LoadedRecords {
    blocks: VecDeque<BlockData>,
    total_loaded: usize,
    revision: HistoryRevision,
}

fn read_frame_header(file: &mut File, frame_index: usize) -> io::Result<Option<[u8; 4]>> {
    let mut header = [0u8; 4];
    let mut read = 0usize;
    while read < header.len() {
        match file.read(&mut header[read..]) {
            Ok(0) if read == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("partial Block-history frame header #{frame_index}: {read}/4 bytes"),
                ));
            }
            Ok(count) => read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(header))
}

fn read_frame_payload(file: &mut File, payload: &mut [u8], frame_index: usize) -> io::Result<()> {
    let mut read = 0usize;
    while read < payload.len() {
        match file.read(&mut payload[read..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "partial Block-history frame payload #{frame_index}: {read}/{} bytes",
                        payload.len()
                    ),
                ));
            }
            Ok(count) => read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_history_records(
    path: &Path,
    prefer_compressed: bool,
    keep_limit: usize,
    undecodable_policy: UndecodablePolicy,
) -> io::Result<LoadedRecords> {
    let directory = open_parent_directory(path)?;
    read_history_records_in_directory(
        &directory,
        path,
        prefer_compressed,
        keep_limit,
        undecodable_policy,
    )
}

fn read_history_records_in_directory(
    directory: &File,
    path: &Path,
    prefer_compressed: bool,
    keep_limit: usize,
    undecodable_policy: UndecodablePolicy,
) -> io::Result<LoadedRecords> {
    let Some((mut file, metadata)) = open_history_file_in_directory(directory, path)? else {
        return Ok(LoadedRecords {
            blocks: VecDeque::new(),
            total_loaded: 0,
            revision: HistoryRevision::Missing,
        });
    };
    let revision = HistoryRevision::from_metadata(&metadata);
    let mut blocks = VecDeque::new();
    let mut total_loaded = 0usize;
    let mut total_file_bytes = 0u64;
    let mut total_decoded_bytes = 0u64;
    let mut undecodable = 0usize;
    let mut frame_index = 0usize;
    let decode_started = Instant::now();

    while let Some(header) = read_frame_header(&mut file, frame_index)? {
        if frame_index >= MAX_HISTORY_FRAMES {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "Block history exceeds its record-count limit",
            ));
        }
        if decode_started.elapsed() >= MAX_HISTORY_DECODE_DURATION {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Block history exceeded its decode time budget",
            ));
        }
        total_file_bytes = total_file_bytes.checked_add(4).ok_or_else(|| {
            io::Error::new(io::ErrorKind::FileTooLarge, "Block-history size overflow")
        })?;
        let len = u32::from_le_bytes(header) as usize;
        if len > MAX_ENCODED_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("oversized Block-history frame #{frame_index}: {len} bytes"),
            ));
        }
        total_file_bytes = total_file_bytes.checked_add(len as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::FileTooLarge, "Block-history size overflow")
        })?;
        if total_file_bytes > MAX_HISTORY_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "Block history grew beyond its encoded byte limit while reading",
            ));
        }

        let mut data = vec![0u8; len];
        read_frame_payload(&mut file, &mut data, frame_index)?;
        match decode_block_record(&data, prefer_compressed) {
            Ok((block, decoded_len)) => {
                total_decoded_bytes = total_decoded_bytes
                    .checked_add(decoded_len as u64)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::FileTooLarge,
                            "decoded Block-history size overflow",
                        )
                    })?;
                if total_decoded_bytes > MAX_HISTORY_DECODED_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        "decoded Block history exceeds its aggregate byte limit",
                    ));
                }
                total_loaded = total_loaded.saturating_add(1);
                push_bounded_back(&mut blocks, block, keep_limit);
            }
            Err(error) if undecodable_policy == UndecodablePolicy::Skip => {
                undecodable = undecodable.saturating_add(1);
                log::warn!(
                    "load_history: skipping undecodable frame #{frame_index} ({len} bytes): {error}"
                );
            }
            Err(error) => return Err(error),
        }
        frame_index = frame_index.saturating_add(1);
    }
    if undecodable > 0 {
        log::warn!(
            "Skipped {undecodable} Block-history record(s); strict save validation will preserve the original file"
        );
    }
    Ok(LoadedRecords {
        blocks,
        total_loaded,
        revision,
    })
}

fn read_history_snapshot(
    base: &Path,
    session_id: Option<&str>,
    prefer_compressed: bool,
    load_limit: usize,
) -> io::Result<Arc<LoadedHistory>> {
    let session_path = session_id.map(|sid| per_session_history_path(base, sid));
    let target = session_path.as_deref().unwrap_or(base);
    let Some(path) = choose_load_path(base, session_path.as_deref()) else {
        return Ok(Arc::new(LoadedHistory {
            blocks: Arc::new(Vec::new()),
            total_loaded: 0,
            target_revision: HistoryRevision::Missing,
        }));
    };
    let mut loaded = read_history_records(
        &path,
        prefer_compressed,
        load_limit,
        UndecodablePolicy::Skip,
    )?;
    let target_revision = if path == target {
        loaded.revision
    } else {
        HistoryRevision::Missing
    };

    if loaded.total_loaded > load_limit {
        log::info!(
            "Loading Block history: keeping {} recent blocks out of {} total",
            load_limit,
            loaded.total_loaded
        );
    }
    refresh_loaded_block_ids(&mut loaded.blocks);
    Ok(Arc::new(LoadedHistory {
        blocks: Arc::new(loaded.blocks.into_iter().collect()),
        total_loaded: loaded.total_loaded,
        target_revision,
    }))
}

/// Return only the newest loaded blocks that still fit before commands which
/// completed while the background read was in flight. Live commands always
/// win; a delayed restore must never erase or reorder them.
fn loaded_prefix_for_live(
    loaded: &[BlockData],
    live_len: usize,
    max_blocks: usize,
) -> Vec<BlockData> {
    let available = max_blocks.saturating_sub(live_len);
    let start = loaded.len().saturating_sub(available);
    loaded[start..].to_vec()
}

fn estimated_block_payload_bytes(block: &BlockData) -> u64 {
    let strings = block
        .prompt
        .len()
        .saturating_add(block.cmd.len())
        .saturating_add(block.cmd_markup.as_ref().map_or(0, String::len))
        .saturating_add(block.output.len())
        .saturating_add(block.cwd.as_ref().map_or(0, String::len));
    (std::mem::size_of::<BlockData>() as u64).saturating_add(strings as u64)
}

/// Clone only the newest live records that can possibly fit the persistent
/// record-count and decoded-byte budgets. This bounds memory before the GTK
/// thread hands ownership to the background worker; encode-time limits alone
/// would still allow an enormous configured deque to be cloned first.
fn snapshot_live_blocks_bounded(
    blocks: &VecDeque<BlockData>,
    max_blocks: usize,
    max_record_bytes: u64,
    max_total_bytes: u64,
) -> Vec<BlockData> {
    let mut newest_first = Vec::new();
    let mut total_bytes = 0u64;
    for block in blocks.iter().rev().take(max_blocks.min(MAX_HISTORY_FRAMES)) {
        let bytes = estimated_block_payload_bytes(block);
        if bytes > max_record_bytes {
            log::warn!(
                "save_history: skipping a live block whose estimated payload is {bytes} bytes"
            );
            continue;
        }
        let Some(next_total) = total_bytes.checked_add(bytes) else {
            break;
        };
        if next_total > max_total_bytes {
            break;
        }
        total_bytes = next_total;
        newest_first.push(block.clone());
    }
    newest_first.reverse();
    newest_first
}

fn block_identity_hash(block: &BlockData) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Persisted IDs are deliberately refreshed on restore and estimated height
    // is viewport-derived. Neither is a stable cross-process identity.
    block.prompt.hash(&mut hasher);
    block.cmd.hash(&mut hasher);
    block.cmd_markup.hash(&mut hasher);
    block.output.hash(&mut hasher);
    block.exit_code.hash(&mut hasher);
    block.line_count.hash(&mut hasher);
    block.start_time_ms.hash(&mut hasher);
    block.end_time_ms.hash(&mut hasher);
    block.duration_ms.hash(&mut hasher);
    block.cwd.hash(&mut hasher);
    block.cols.hash(&mut hasher);
    hasher.finish()
}

fn same_block_identity(left: &BlockData, right: &BlockData) -> bool {
    left.prompt == right.prompt
        && left.cmd == right.cmd
        && left.cmd_markup == right.cmd_markup
        && left.output == right.output
        && left.exit_code == right.exit_code
        && left.line_count == right.line_count
        && left.start_time_ms == right.start_time_ms
        && left.end_time_ms == right.end_time_ms
        && left.duration_ms == right.duration_ms
        && left.cwd == right.cwd
        && left.cols == right.cols
}

fn deduplicate_newest(blocks: impl IntoIterator<Item = BlockData>) -> Vec<BlockData> {
    let mut newest_first = Vec::new();
    let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
    let collected = blocks.into_iter().collect::<Vec<_>>();
    for block in collected.into_iter().rev() {
        let hash = block_identity_hash(&block);
        let duplicate = buckets.get(&hash).is_some_and(|indices| {
            indices
                .iter()
                .any(|&index| same_block_identity(&newest_first[index], &block))
        });
        if duplicate {
            continue;
        }
        let index = newest_first.len();
        newest_first.push(block);
        buckets.entry(hash).or_default().push(index);
    }
    newest_first.reverse();
    newest_first
}

/// Existing locked disk order is authoritative. A stale pane may append its
/// genuinely new blocks, but absence from its old UI snapshot never deletes a
/// command another process saved in the meantime.
fn merge_stale_snapshot(
    existing: impl IntoIterator<Item = BlockData>,
    incoming: impl IntoIterator<Item = BlockData>,
) -> Vec<BlockData> {
    let mut merged = deduplicate_newest(existing);
    let incoming = deduplicate_newest(incoming);
    let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
    for (index, block) in merged.iter().enumerate() {
        buckets
            .entry(block_identity_hash(block))
            .or_default()
            .push(index);
    }
    for block in incoming {
        let hash = block_identity_hash(&block);
        let existing_index = buckets.get(&hash).and_then(|indices| {
            indices
                .iter()
                .copied()
                .find(|&index| same_block_identity(&merged[index], &block))
        });
        if let Some(index) = existing_index {
            merged[index] = block;
        } else {
            let index = merged.len();
            merged.push(block);
            buckets.entry(hash).or_default().push(index);
        }
    }
    merged
}

/// Encode a newest-first budget, then restore chronological order for disk.
/// A single pathological block is skipped, while reaching the aggregate
/// budget stops before any older record can displace a newer complete one.
fn encode_history_frames_bounded(
    blocks: &[BlockData],
    compress: bool,
    max_encoded_record_bytes: usize,
    max_decoded_record_bytes: usize,
    max_history_bytes: usize,
) -> io::Result<Vec<Vec<u8>>> {
    let mut newest_first = Vec::new();
    let mut encoded_bytes = 0usize;
    let mut decoded_bytes = 0u64;

    for block in blocks.iter().rev() {
        let serialized = rkyv::to_bytes::<rkyv::rancor::Error>(block)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if serialized.len() > max_decoded_record_bytes {
            log::warn!(
                "save_history: skipping a {} byte block (decoded record limit {})",
                serialized.len(),
                max_decoded_record_bytes
            );
            continue;
        }

        let record = if compress {
            zstd::encode_all(serialized.as_slice(), 3)
                .map_err(|error| io::Error::other(error.to_string()))?
        } else {
            serialized.as_slice().to_vec()
        };
        if record.len() > max_encoded_record_bytes || record.len() > u32::MAX as usize {
            log::warn!(
                "save_history: skipping a {} byte block (encoded record limit {})",
                record.len(),
                max_encoded_record_bytes
            );
            continue;
        }

        let frame_bytes = 4usize.checked_add(record.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::FileTooLarge,
                "Block history frame size overflow",
            )
        })?;
        let Some(next_total) = encoded_bytes.checked_add(frame_bytes) else {
            break;
        };
        let Some(next_decoded) = decoded_bytes.checked_add(serialized.len() as u64) else {
            break;
        };
        if next_total > max_history_bytes || next_decoded > MAX_HISTORY_DECODED_BYTES {
            log::warn!("save_history: keeping newer blocks within encoded and decoded byte limits");
            break;
        }
        encoded_bytes = next_total;
        decoded_bytes = next_decoded;
        newest_first.push(record);
    }

    newest_first.reverse();
    Ok(newest_first)
}

#[derive(Debug)]
struct SaveHistoryOutcome {
    revision: HistoryRevision,
    authoritative: bool,
}

fn write_history_snapshot(
    base: &Path,
    path: &Path,
    session_id: Option<&str>,
    blocks: &[BlockData],
    compress: bool,
    expected_revision: Option<HistoryRevision>,
) -> io::Result<SaveHistoryOutcome> {
    let lock = HistoryFileLock::acquire(path)?;
    // Re-read and strictly decode while holding the cross-process lock. A
    // corrupt/unknown old frame is evidence, not permission to overwrite it.
    let existing = read_history_records_in_directory(
        &lock.directory,
        path,
        compress,
        usize::MAX,
        UndecodablePolicy::Reject,
    )?;
    let authoritative = expected_revision.is_some_and(|expected| expected == existing.revision);
    let merged = if authoritative {
        deduplicate_newest(blocks.iter().cloned())
    } else {
        merge_stale_snapshot(existing.blocks, blocks.iter().cloned())
    };
    // Overwrite (do NOT append). The in-memory deque was itself seeded from
    // this file at startup, so appending it re-wrote every loaded block on each
    // session. Encode into a sibling temp file first so a crash or serialization
    // error never truncates the last good history.
    let frames = encode_history_frames_bounded(
        &merged,
        compress,
        MAX_ENCODED_RECORD_BYTES,
        MAX_DECODED_RECORD_BYTES as usize,
        MAX_HISTORY_FILE_BYTES as usize,
    )?;
    let result = atomic_write_in_directory(&lock.directory, path, |file| {
        for record in &frames {
            file.write_all(&(record.len() as u32).to_le_bytes())?;
            file.write_all(record)?;
        }
        Ok(())
    });

    if result.is_ok() && session_id.is_some() {
        // This tab's history now lives in its own file; the legacy shared file
        // (which every tab used to overwrite on close) is superseded. Removing
        // it also stops future new tabs from inheriting it.
        if base != path && base.is_file() {
            if let Err(error) = fs::remove_file(base) {
                log::warn!(
                    "remove superseded shared block history {}: {error}",
                    base.display()
                );
            }
        }
        prune_stale_session_histories(base, path, STALE_SESSION_HISTORY_MAX_AGE);
    }
    result?;
    let (_, metadata) =
        open_history_file_in_directory(&lock.directory, path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("Block history disappeared after save: {}", path.display()),
            )
        })?;
    Ok(SaveHistoryOutcome {
        revision: HistoryRevision::from_metadata(&metadata),
        authoritative,
    })
}

#[allow(dead_code)]
impl TermView {
    /// Mark a fallibly prepared pane as rolled back. Exit callbacks and Drop
    /// may still run while GTK releases the temporary tree, but neither may
    /// turn that rollback into a durable history write.
    pub(crate) fn suppress_history_persistence(&self) {
        self.persist_history_on_drop.set(false);
    }

    /// Snapshot block history on the GTK thread and queue all encoding and
    /// durable file I/O on the shared persistence worker.
    pub fn save_history(&self) -> std::io::Result<()> {
        if !self.persist_history_on_drop.get() {
            return Ok(());
        }
        let (path_opt, compress, max_blocks) = {
            let config = self.config.borrow();
            (
                config.block_history_path.as_ref().cloned(),
                config.block_history_compress,
                config.max_visible_blocks as usize,
            )
        };
        if path_opt.is_none() {
            return Ok(());
        }

        let base = history_path(&path_opt.unwrap());
        let session_id = self.session_id.clone();
        let path = match session_id.as_deref() {
            Some(sid) => per_session_history_path(&base, sid),
            None => base.clone(),
        };
        let mut blocks = snapshot_live_blocks_bounded(
            &self.block_data.borrow(),
            max_blocks,
            MAX_DECODED_RECORD_BYTES,
            MAX_HISTORY_DECODED_BYTES,
        );
        let load_applied = self.history_load.applied.load(Ordering::Acquire);
        let load_discarded = self.history_load.discarded.load(Ordering::Acquire);
        let history_load = Arc::clone(&self.history_load);
        let key = PersistenceKey::for_path("block-history", &path);
        persistence::enqueue(key, "Save Block history", move || {
            if !load_applied && !load_discarded {
                match history_load.outcome() {
                    HistoryLoadOutcome::Loaded(loaded) => {
                        let mut merged = loaded_prefix_for_live(
                            loaded.blocks.as_ref(),
                            blocks.len(),
                            max_blocks,
                        );
                        merged.append(&mut blocks);
                        blocks = merged;
                    }
                    HistoryLoadOutcome::Failed { kind, message } => {
                        return Err(io::Error::new(
                            kind,
                            format!(
                                "refusing to overwrite Block history that failed to load: {message}"
                            ),
                        ));
                    }
                    HistoryLoadOutcome::Pending => {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "Block history save reached the worker before its earlier load",
                        ));
                    }
                    HistoryLoadOutcome::Idle => {}
                }
            }
            let expected_revision = history_load.revision();
            let outcome = write_history_snapshot(
                &base,
                &path,
                session_id.as_deref(),
                &blocks,
                compress,
                expected_revision,
            )?;
            // A stale merge cannot grant this pane deletion authority over
            // records it never loaded. It remains merge-only until reload.
            history_load.set_revision(outcome.authoritative.then_some(outcome.revision));
            Ok(())
        })
    }

    /// Load and decode Block history on the shared disk worker, then construct
    /// GTK widgets in a short main-thread callback. Commands that finish while
    /// the read is pending remain newer than every restored block.
    pub(crate) fn start_history_load(self: &Rc<Self>) {
        let (path_opt, compress, load_limit) = {
            let config = self.config.borrow();
            (
                config.block_history_path.as_ref().cloned(),
                config.block_history_compress,
                history_load_limit(
                    config.lazy_load_threshold as usize,
                    config.max_visible_blocks as usize,
                ),
            )
        };
        let Some(path) = path_opt else {
            return;
        };

        if let Some(source) = self.history_load_poll_id.borrow_mut().take() {
            source.remove();
        }
        self.history_load.begin();
        let base = history_path(&path);
        let session_id = self.session_id.clone();
        let target = session_id
            .as_deref()
            .map(|sid| per_session_history_path(&base, sid))
            .unwrap_or_else(|| base.clone());
        let load_for_job = Arc::clone(&self.history_load);
        let key = PersistenceKey::unique_for_path("block-history-load", &target);
        if let Err(error) = persistence::enqueue(key, "Load Block history", move || {
            let result = read_history_snapshot(&base, session_id.as_deref(), compress, load_limit);
            load_for_job.complete(&result);
            result.map(|_| ())
        }) {
            let result = Err(io::Error::new(error.kind(), error.to_string()));
            self.history_load.complete(&result);
            log::warn!("could not queue Block history load: {error}");
        }

        let weak_view = Rc::downgrade(self);
        let load_for_poll = Arc::clone(&self.history_load);
        let source = glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(view) = weak_view.upgrade() else {
                return glib::ControlFlow::Break;
            };
            match load_for_poll.outcome() {
                HistoryLoadOutcome::Idle | HistoryLoadOutcome::Pending => {
                    glib::ControlFlow::Continue
                }
                HistoryLoadOutcome::Loaded(loaded) => {
                    if !load_for_poll.discarded.load(Ordering::Acquire) {
                        view.apply_loaded_history(&loaded);
                    }
                    load_for_poll.applied.store(true, Ordering::Release);
                    view.history_load_poll_id.borrow_mut().take();
                    glib::ControlFlow::Break
                }
                HistoryLoadOutcome::Failed { message, .. } => {
                    log::warn!("load Block history: {message}");
                    // A later save may still succeed (for example, a removable
                    // drive was remounted). Pre-load shutdown saves preserve the
                    // unreadable file; subsequent user mutations may retry it.
                    load_for_poll.applied.store(true, Ordering::Release);
                    view.history_load_poll_id.borrow_mut().take();
                    glib::ControlFlow::Break
                }
            }
        });
        *self.history_load_poll_id.borrow_mut() = Some(source);
    }

    fn apply_loaded_history(&self, loaded: &LoadedHistory) {
        let max_blocks = self.config.borrow().max_visible_blocks as usize;
        let live_len = self.block_data.borrow().len();
        let mut restored = loaded_prefix_for_live(loaded.blocks.as_ref(), live_len, max_blocks);
        if restored.is_empty() {
            return;
        }

        let skipped = loaded.total_loaded.saturating_sub(restored.len());
        if skipped > 0 {
            log::info!(
                "Block history restore kept {} records and skipped {} older records",
                restored.len(),
                skipped
            );
        }

        let fallback_cols = self.active.borrow().grid_cols() as i64;
        {
            let config = self.config.borrow();
            for block in &mut restored {
                let cols = if block.cols > 0 {
                    block.cols as i64
                } else {
                    fallback_cols
                };
                block.estimated_height = estimated_finished_block_height_for_text(
                    &config,
                    &block.cmd,
                    &block.output,
                    cols,
                );
            }
        }

        let sibling = self
            .finished_blocks
            .borrow()
            .first()
            .map(|block| block.widget().clone().upcast::<gtk4::Widget>())
            // Inline notices (including the native organism) may already be
            // pinned above the live prompt while asynchronous history loads.
            // Insert restored blocks before the first child so those notices
            // remain adjacent to the prompt instead of drifting above history.
            .or_else(|| self.block_list.first_child())
            .unwrap_or_else(|| {
                self.active
                    .borrow()
                    .widget()
                    .clone()
                    .upcast::<gtk4::Widget>()
            });
        let mut restored_widgets = Vec::with_capacity(restored.len());
        {
            let config = self.config.borrow();
            for block in &restored {
                let cols = if block.cols > 0 {
                    block.cols as i64
                } else {
                    fallback_cols
                };
                log::debug!(
                    "Loaded historical block id={} prompt_len={} command_len={} output_len={} exit_code={:?}",
                    block.id,
                    block.prompt.len(),
                    block.cmd.len(),
                    block.output.len(),
                    block.exit_code
                );
                let finished = FinishedBlock::new(
                    block.id,
                    &block.prompt,
                    &block.cmd,
                    block.cmd_markup.as_deref(),
                    &block.output,
                    block.exit_code,
                    &config,
                    block.duration_ms,
                    block.end_time_ms,
                    block.cwd.as_deref(),
                    cols,
                );
                finished
                    .widget()
                    .insert_before(&self.block_list, Some(&sibling));
                finished.connect_actions(
                    &self.active_vte,
                    &self.pty,
                    &self.pty_synced,
                    &self.active,
                    &self.typed_cmd,
                    &self.typed_cmd_fidelity,
                    &self.submission_pending,
                    &self.pending_typeahead,
                    &self.bstate,
                    &self.bracketed_paste,
                );
                finished.connect_scroll_forwarding(&self.block_scroll);
                install_finished_block_selection(
                    &finished,
                    &self.active,
                    &self.finished_blocks,
                    &self.selected_block_ids,
                    &self.selected_block_id,
                    &self.selection_anchor_id,
                );
                restored_widgets.push(finished);
            }
        }

        let restored_len = restored.len();
        {
            let mut blocks = self.block_data.borrow_mut();
            for block in restored.into_iter().rev() {
                blocks.push_front(block);
            }
        }
        self.finished_blocks
            .borrow_mut()
            .splice(0..0, restored_widgets);
        if restored_len > 0 {
            let shifted = self
                .visible_indices
                .borrow()
                .iter()
                .map(|index| index.saturating_add(restored_len))
                .collect();
            *self.visible_indices.borrow_mut() = shifted;
        }
        self.update_viewport();
        self.update_block_visibility();
        if !self.user_scrolled_up.get() {
            self.reveal_live_input();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        atomic_write, choose_load_path, decode_block_record, decode_zstd_bounded,
        encode_history_frames_bounded, expand_home_prefix_with, history_load_limit,
        loaded_prefix_for_live, lock_file_name, per_session_history_path,
        prune_stale_session_histories, push_bounded_back, read_history_records,
        read_history_snapshot, refresh_loaded_block_ids, snapshot_live_blocks_bounded,
        write_history_snapshot, BlockData, HistoryFileLock, HistoryRevision, UndecodablePolicy,
        MAX_ENCODED_RECORD_BYTES, MAX_HISTORY_COMMAND_BYTES, MAX_HISTORY_FILE_BYTES,
    };
    use std::collections::VecDeque;
    use std::ffi::CString;
    use std::fs;
    use std::io::{self, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "forge-history-{name}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample_block(id: u64, command: &str) -> BlockData {
        BlockData {
            id,
            prompt: "prompt".to_string(),
            cmd: command.to_string(),
            cmd_markup: None,
            output: "output".to_string(),
            exit_code: Some(0),
            estimated_height: 32,
            line_count: 1,
            start_time_ms: None,
            end_time_ms: None,
            duration_ms: None,
            cwd: None,
            cols: 80,
        }
    }

    #[test]
    fn history_load_limit_never_exceeds_runtime_block_cap() {
        assert_eq!(history_load_limit(1_000, 200), 200);
        assert_eq!(history_load_limit(100, 200), 100);
        assert_eq!(history_load_limit(0, 200), 0);
    }

    #[test]
    fn live_history_snapshot_is_bounded_before_cloning() {
        let blocks = VecDeque::from([
            sample_block(1, "oldest"),
            sample_block(2, "middle"),
            sample_block(3, "newest"),
        ]);
        let one_record_budget = super::estimated_block_payload_bytes(&blocks[2]);
        let captured =
            snapshot_live_blocks_bounded(&blocks, 100, one_record_budget, one_record_budget);
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].cmd, "newest");
        assert_eq!(
            snapshot_live_blocks_bounded(&blocks, 0, 1_000, 1_000).len(),
            0
        );
    }

    #[test]
    fn loaded_blocks_receive_unique_runtime_ids() {
        let mut blocks = VecDeque::from([sample_block(0, "first"), sample_block(0, "second")]);
        refresh_loaded_block_ids(&mut blocks);
        assert_ne!(blocks[0].id, blocks[1].id);
    }

    #[test]
    fn history_decoder_accepts_raw_and_compressed_records_after_config_toggle() {
        let block = sample_block(7, "printf hello");
        let raw = rkyv::to_bytes::<rkyv::rancor::Error>(&block).unwrap();
        let compressed = zstd::encode_all(raw.as_slice(), 1).unwrap();

        for prefer_compressed in [false, true] {
            assert_eq!(
                decode_block_record(raw.as_slice(), prefer_compressed)
                    .unwrap()
                    .0
                    .cmd,
                "printf hello"
            );
            assert_eq!(
                decode_block_record(&compressed, prefer_compressed)
                    .unwrap()
                    .0
                    .cmd,
                "printf hello"
            );
        }
    }

    #[test]
    fn history_decoder_rejects_oversized_individual_fields() {
        let mut block = sample_block(8, "safe");
        block.cmd = "x".repeat(MAX_HISTORY_COMMAND_BYTES + 1);
        let raw = rkyv::to_bytes::<rkyv::rancor::Error>(&block).unwrap();
        let error = match decode_block_record(raw.as_slice(), false) {
            Ok(_) => panic!("oversized history command unexpectedly decoded"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    /// Round 8 changed `BlockData::exit_code` from `i32` to `Option<i32>`. A
    /// history file written before that must still decode — dropping every old
    /// frame on upgrade would silently erase the user's saved blocks.
    #[test]
    fn history_decoder_accepts_pre_round8_frames_with_bare_exit_codes() {
        let legacy = super::LegacyBlockDataV1 {
            id: 3,
            prompt: "prompt".to_string(),
            cmd: "false".to_string(),
            cmd_markup: None,
            output: "output".to_string(),
            exit_code: 1,
            estimated_height: 32,
            line_count: 1,
            start_time_ms: Some(5),
            end_time_ms: Some(9),
            duration_ms: Some(4),
            cwd: Some("/tmp".to_string()),
            cols: 80,
        };
        let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&legacy).unwrap();

        for prefer_compressed in [false, true] {
            let (decoded, _) = decode_block_record(encoded.as_slice(), prefer_compressed).unwrap();
            assert_eq!(decoded.cmd, "false");
            assert_eq!(decoded.output, "output");
            assert_eq!(decoded.exit_code, Some(1));
            assert_eq!(decoded.cwd.as_deref(), Some("/tmp"));
        }

        // Round-trip of the current schema still prefers the current shape,
        // including the honest unknown.
        let unknown = BlockData {
            exit_code: None,
            ..sample_block(4, "unreported")
        };
        let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&unknown).unwrap();
        let (decoded, _) = decode_block_record(encoded.as_slice(), false).unwrap();
        assert_eq!(decoded.exit_code, None);
    }

    #[test]
    fn push_bounded_back_keeps_only_recent_items() {
        let mut items = VecDeque::new();

        for item in 0..5 {
            push_bounded_back(&mut items, item, 3);
        }

        assert_eq!(items.into_iter().collect::<Vec<_>>(), vec![2, 3, 4]);
    }

    #[test]
    fn push_bounded_back_honors_zero_limit() {
        let mut items = VecDeque::new();

        push_bounded_back(&mut items, 1, 0);

        assert!(items.is_empty());
    }

    #[test]
    fn late_history_load_keeps_live_commands_newest_and_respects_cap() {
        let loaded = vec![
            sample_block(1, "old-1"),
            sample_block(2, "old-2"),
            sample_block(3, "old-3"),
        ];

        let kept = loaded_prefix_for_live(&loaded, 2, 4);
        assert_eq!(
            kept.iter()
                .map(|block| block.cmd.as_str())
                .collect::<Vec<_>>(),
            ["old-2", "old-3"]
        );
        assert!(loaded_prefix_for_live(&loaded, 4, 4).is_empty());
    }

    #[test]
    fn history_load_refuses_a_symlink_instead_of_following_it() {
        let dir = TestDir::new("no-follow-load");
        let target = dir.path().join("target.bin");
        let link = dir.path().join("history.bin");
        fs::write(&target, b"not history").unwrap();
        symlink(&target, &link).unwrap();

        let Err(error) = read_history_snapshot(&link, None, false, 10) else {
            panic!("a symlink must not be accepted as Block history");
        };
        assert!(matches!(
            error.raw_os_error(),
            Some(code) if code == nix::libc::ELOOP
        ));
        assert_eq!(fs::read(&target).unwrap(), b"not history");
    }

    #[test]
    fn history_load_rejects_fifo_without_blocking() {
        let dir = TestDir::new("nonblocking-fifo-load");
        let fifo = dir.path().join("history.fifo");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        let result = unsafe { nix::libc::mkfifo(fifo_c.as_ptr(), 0o600) };
        assert_eq!(result, 0, "mkfifo failed: {}", io::Error::last_os_error());

        let Err(error) = read_history_snapshot(&fifo, None, false, 10) else {
            panic!("a FIFO must not be accepted as Block history");
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn history_load_rejects_multiply_linked_regular_file() {
        let dir = TestDir::new("hardlink-load");
        let history = dir.path().join("history.bin");
        let alias = dir.path().join("alias.bin");
        fs::write(&history, []).unwrap();
        fs::hard_link(&history, &alias).unwrap();

        let Err(error) = read_history_snapshot(&history, None, false, 10) else {
            panic!("a multiply-linked file must not be accepted as Block history");
        };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn history_load_rejects_oversized_file_before_scanning() {
        let dir = TestDir::new("oversized-load");
        let history = dir.path().join("history.bin");
        let file = fs::File::create(&history).unwrap();
        file.set_len(MAX_HISTORY_FILE_BYTES + 1).unwrap();

        let Err(error) = read_history_snapshot(&history, None, false, 10) else {
            panic!("an oversized file must not be accepted as Block history");
        };
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
    }

    #[test]
    fn history_load_rejects_an_oversized_frame() {
        let dir = TestDir::new("oversized-frame");
        let history = dir.path().join("history.bin");
        fs::write(
            &history,
            ((MAX_ENCODED_RECORD_BYTES + 1) as u32).to_le_bytes(),
        )
        .unwrap();

        let error = match read_history_snapshot(&history, None, false, 10) {
            Ok(_) => panic!("oversized frame was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn save_budget_keeps_newest_complete_frames_in_original_order() {
        let blocks = vec![
            sample_block(1, "oldest"),
            sample_block(2, "middle"),
            sample_block(3, "newest"),
        ];
        let all = encode_history_frames_bounded(&blocks, false, usize::MAX, usize::MAX, usize::MAX)
            .unwrap();
        let newest_two_budget = 8 + all[1].len() + all[2].len();
        let kept = encode_history_frames_bounded(
            &blocks,
            false,
            usize::MAX,
            usize::MAX,
            newest_two_budget,
        )
        .unwrap();
        let commands = kept
            .iter()
            .map(|record| decode_block_record(record, false).unwrap().0.cmd)
            .collect::<Vec<_>>();
        assert_eq!(commands, ["middle", "newest"]);
    }

    #[test]
    fn expands_only_home_slash_prefix() {
        let home = Path::new("/home/tester");
        assert_eq!(
            expand_home_prefix_with("~/.local/share/forge/history", Some(home)),
            home.join(".local/share/forge/history")
        );
        assert_eq!(expand_home_prefix_with("~", Some(home)), PathBuf::from("~"));
        assert_eq!(
            expand_home_prefix_with("~other/history", Some(home)),
            PathBuf::from("~other/history")
        );
        assert_eq!(
            expand_home_prefix_with("cache/~/history", Some(home)),
            PathBuf::from("cache/~/history")
        );
        assert_eq!(
            expand_home_prefix_with("~/history", None),
            PathBuf::from("~/history")
        );
    }

    #[test]
    fn atomic_write_creates_parent_directories_and_replaces_file() {
        let dir = TestDir::new("replace");
        let target = dir.path().join("nested/deeper/history.bin");

        atomic_write(&target, |file| {
            use std::io::Write as _;
            file.write_all(b"first")
        })
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(target.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        atomic_write(&target, |file| {
            use std::io::Write as _;
            file.write_all(b"second")
        })
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn atomic_write_rejects_a_symlink_parent() {
        let dir = TestDir::new("no-follow-parent");
        let real_parent = dir.path().join("real");
        let linked_parent = dir.path().join("linked");
        fs::create_dir(&real_parent).unwrap();
        symlink(&real_parent, &linked_parent).unwrap();

        let target = linked_parent.join("history.bin");
        let error = atomic_write(&target, |file| file.write_all(b"payload")).unwrap_err();
        assert!(matches!(
            error.raw_os_error(),
            Some(code) if code == nix::libc::ELOOP || code == nix::libc::ENOTDIR
        ));
        assert!(!real_parent.join("history.bin").exists());
    }

    #[test]
    fn failed_atomic_write_preserves_previous_file_and_cleans_temp() {
        let dir = TestDir::new("failure");
        let target = dir.path().join("history.bin");
        fs::write(&target, b"last-good").unwrap();

        let err = atomic_write(&target, |file| {
            use std::io::Write as _;
            file.write_all(b"partial")?;
            Err(io::Error::other("simulated encoder failure"))
        })
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&target).unwrap(), b"last-good");
        let entries = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![target.file_name().unwrap()]);
    }

    #[test]
    fn per_session_paths_are_distinct_per_tab_and_filename_safe() {
        // Regression: every tab used to save to the configured path verbatim,
        // so concurrent tabs overwrote each other's history on close.
        let base = Path::new("/state/forge/blocks.bin");
        let first = per_session_history_path(base, "747026-1784511309421544366");
        let second = per_session_history_path(base, "747026-1784511391784501255");
        assert_ne!(first, second);
        assert_eq!(
            first,
            PathBuf::from("/state/forge/blocks-747026-1784511309421544366.bin")
        );

        assert_eq!(
            per_session_history_path(Path::new("/state/history"), "sid-1"),
            PathBuf::from("/state/history-sid-1")
        );
        // Ids round-trip through the user-editable window snapshot.
        assert_eq!(
            per_session_history_path(base, "../../etc/passwd"),
            PathBuf::from("/state/forge/blocks-..~2F..~2Fetc~2Fpasswd.bin")
        );
    }

    #[test]
    fn session_filename_encoding_avoids_replacement_collisions_and_name_amplification() {
        let base = Path::new("/state/forge/blocks.bin");
        assert_ne!(
            per_session_history_path(base, "a/b"),
            per_session_history_path(base, "a?b")
        );

        let path = per_session_history_path(base, &"恶意/".repeat(100_000));
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            name.len() <= 255,
            "bounded component exceeded NAME_MAX: {name}"
        );
        assert!(name.starts_with("blocks-~H"));
        assert!(name.ends_with(".bin"));
    }

    #[test]
    fn load_prefers_own_session_file_and_falls_back_to_legacy_shared_file() {
        let dir = TestDir::new("load-choice");
        let base = dir.path().join("blocks.bin");
        let session = per_session_history_path(&base, "sid-9");

        // Nothing on disk yet: a fresh tab has no history to load.
        assert_eq!(choose_load_path(&base, Some(&session)), None);

        // Pre-split installs only have the shared file: read it once so the
        // upgrade does not silently drop the visible history.
        fs::write(&base, b"legacy").unwrap();
        assert_eq!(choose_load_path(&base, Some(&session)), Some(base.clone()));

        // Once the tab owns a file, the shared one is ignored.
        fs::write(&session, b"own").unwrap();
        assert_eq!(
            choose_load_path(&base, Some(&session)),
            Some(session.clone())
        );

        // No session id (legacy caller): the shared file remains the source.
        assert_eq!(choose_load_path(&base, None), Some(base.clone()));
    }

    #[test]
    fn prune_removes_only_stale_matching_session_siblings() {
        let dir = TestDir::new("prune");
        let base = dir.path().join("blocks.bin");
        let keep = per_session_history_path(&base, "sid-live");
        let stale = per_session_history_path(&base, "sid-stale");
        let other_ext = dir.path().join("blocks-sid.log");
        let unrelated = dir.path().join("notes.bin");
        for path in [&keep, &stale, &other_ext, &unrelated] {
            fs::write(path, b"x").unwrap();
        }
        fs::write(&base, b"shared").unwrap();

        // Zero max-age marks every candidate stale, which exercises selection
        // without manipulating file mtimes.
        prune_stale_session_histories(&base, &keep, std::time::Duration::ZERO);

        assert!(keep.exists(), "the just-written file must survive");
        assert!(!stale.exists(), "stale session sibling should be removed");
        assert!(other_ext.exists(), "different extension is not a candidate");
        assert!(unrelated.exists(), "non-matching stem is not a candidate");
        assert!(base.exists(), "the shared base file is not prune's concern");

        // A realistic age leaves freshly-written files alone.
        fs::write(&stale, b"x").unwrap();
        prune_stale_session_histories(&base, &keep, super::STALE_SESSION_HISTORY_MAX_AGE);
        assert!(stale.exists());
    }

    #[test]
    fn compressed_record_decode_enforces_output_limit() {
        let compressed = zstd::encode_all(&b"0123456789abcdef"[..], 1).unwrap();

        let error = decode_zstd_bounded(&compressed, 8).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_frame_header_and_payload_are_corruption_not_clean_eof() {
        let dir = TestDir::new("truncated-frames");
        let history = dir.path().join("history.bin");

        fs::write(&history, [1_u8, 2]).unwrap();
        let error = match read_history_snapshot(&history, None, false, 10) {
            Ok(_) => panic!("partial header was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let mut partial_payload = 8_u32.to_le_bytes().to_vec();
        partial_payload.extend_from_slice(b"tiny");
        fs::write(&history, partial_payload).unwrap();
        let error = match read_history_snapshot(&history, None, false, 10) {
            Ok(_) => panic!("partial payload was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn strict_save_preserves_an_undecodable_last_good_file() {
        let dir = TestDir::new("preserve-corrupt");
        let history = dir.path().join("history.bin");
        let original = b"not-a-complete-frame";
        fs::write(&history, original).unwrap();

        let error = write_history_snapshot(
            &history,
            &history,
            None,
            &[sample_block(1, "new")],
            false,
            None,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&history).unwrap(), original);
    }

    #[test]
    fn stale_saves_merge_concurrent_additions_but_cannot_delete_them() {
        let dir = TestDir::new("stale-merge");
        let history = dir.path().join("history.bin");
        let baseline = sample_block(1, "baseline");
        let initial = write_history_snapshot(
            &history,
            &history,
            None,
            std::slice::from_ref(&baseline),
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();

        let first = vec![baseline.clone(), sample_block(2, "first-writer")];
        write_history_snapshot(
            &history,
            &history,
            None,
            &first,
            false,
            Some(initial.revision),
        )
        .unwrap();
        // This pane still believes the initial revision is current. Its absent
        // first-writer block must not become a deletion.
        let second = vec![
            BlockData { id: 99, ..baseline },
            sample_block(3, "stale-writer"),
        ];
        let stale = write_history_snapshot(
            &history,
            &history,
            None,
            &second,
            false,
            Some(initial.revision),
        )
        .unwrap();
        assert!(!stale.authoritative);

        let loaded =
            read_history_records(&history, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(
            loaded
                .blocks
                .iter()
                .map(|block| block.cmd.as_str())
                .collect::<Vec<_>>(),
            ["baseline", "first-writer", "stale-writer"]
        );

        write_history_snapshot(&history, &history, None, &[], false, Some(stale.revision)).unwrap();
        assert_eq!(fs::metadata(&history).unwrap().len(), 0);
    }

    #[test]
    fn lock_entry_symlink_hardlink_and_fifo_never_touch_their_victim() {
        for kind in ["symlink", "hardlink", "fifo"] {
            let dir = TestDir::new(kind);
            let history = dir.path().join("history.bin");
            let lock = dir.path().join(lock_file_name(&history).unwrap());
            let victim = dir.path().join("victim");
            fs::write(&victim, b"unchanged").unwrap();
            fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
            match kind {
                "symlink" => symlink(&victim, &lock).unwrap(),
                "hardlink" => fs::hard_link(&victim, &lock).unwrap(),
                "fifo" => {
                    let fifo = CString::new(lock.as_os_str().as_bytes()).unwrap();
                    // SAFETY: fifo is a live NUL-terminated path.
                    assert_eq!(unsafe { nix::libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
                }
                _ => unreachable!(),
            }

            assert!(write_history_snapshot(
                &history,
                &history,
                None,
                &[sample_block(1, "new")],
                false,
                Some(HistoryRevision::Missing),
            )
            .is_err());
            assert_eq!(fs::read(&victim).unwrap(), b"unchanged");
            assert_eq!(
                fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
    }

    #[test]
    fn directory_lock_survives_lock_filename_replacement() {
        let dir = TestDir::new("lock-replacement");
        let history = dir.path().join("history.bin");
        let first = HistoryFileLock::acquire(&history).unwrap();
        let lock = dir.path().join(lock_file_name(&history).unwrap());
        let moved = dir.path().join("old-lock");
        fs::rename(&lock, &moved).unwrap();
        fs::write(&lock, []).unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();

        let error = match HistoryFileLock::acquire_with_timeout(&history, Duration::from_millis(30))
        {
            Ok(_) => panic!("replacement lock bypassed the held directory lock"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(first);
        HistoryFileLock::acquire_with_timeout(&history, Duration::from_millis(30)).unwrap();
    }

    #[test]
    fn held_history_directory_prevents_parent_namespace_redirection() {
        let root = TestDir::new("parent-swap");
        let live = root.path().join("live");
        let displaced = root.path().join("displaced");
        fs::create_dir(&live).unwrap();
        fs::set_permissions(&live, fs::Permissions::from_mode(0o700)).unwrap();
        let history = live.join("history.bin");
        let lock = HistoryFileLock::acquire(&history).unwrap();

        fs::rename(&live, &displaced).unwrap();
        fs::create_dir(&live).unwrap();
        fs::set_permissions(&live, fs::Permissions::from_mode(0o700)).unwrap();
        super::atomic_write_in_directory(&lock.directory, &history, |file| {
            file.write_all(b"payload")
        })
        .unwrap();

        assert_eq!(fs::read(displaced.join("history.bin")).unwrap(), b"payload");
        assert!(!live.join("history.bin").exists());
    }

    #[test]
    fn history_write_rejects_a_world_writable_parent_namespace() {
        let dir = TestDir::new("unsafe-parent");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let history = dir.path().join("history.bin");
        let error = atomic_write(&history, |file| file.write_all(b"payload")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!history.exists());
    }
}
