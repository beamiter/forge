//! Bounded, crash-safe persistence and restoration for Block-mode history.
//!
//! Persist the in-memory `block_data` deque to/from disk as length-prefixed
//! rkyv records (optional zstd). Truncate-on-save (not append) keeps the file
//! bounded, since the deque was already seeded from this file on startup.

#[cfg(test)]
use super::completed_block_retention_plan;
use super::zone_history;
use super::{
    estimated_finished_block_height_for_text, install_finished_block_selection, next_block_id,
    BlockData, FinishedBlock, TermView, MAX_COMPLETED_BLOCK_RETAINED_BYTES,
};
use crate::persistence::{self, PersistenceKey};
use gtk4::glib;
use gtk4::prelude::*;
use std::collections::VecDeque;
use std::ffi::{CString, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::SystemTime;
use std::time::{Duration, Instant};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_ENCODED_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_DECODED_RECORD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HISTORY_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_HISTORY_DECODED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_HISTORY_FRAMES: usize = 100_000;
const MAX_HISTORY_DECODE_DURATION: Duration = Duration::from_secs(5);
const MAX_SESSION_COMPONENT_BYTES: usize = 96;
const HISTORY_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const MAX_SCANNED_HISTORY_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_HISTORY_PROMPT_BYTES: usize = 64 * 1024;
const MAX_HISTORY_COMMAND_BYTES: usize = jterm_core::review_input::MAX_REVIEW_INPUT_BYTES;
const MAX_HISTORY_COMMAND_MARKUP_BYTES: usize = jterm_core::review_input::MAX_REVIEW_INPUT_BYTES;
const MAX_HISTORY_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HISTORY_CWD_BYTES: usize = 16 * 1024;
const HISTORY_ZSTD_WINDOW_LOG_MAX: u32 = 24;
/// One decode can briefly own the 16 MiB encoded frame, a 16 MiB decompressed
/// archive, and the newly deserialized owned strings before either input Vec is
/// released. Reserve another 16 MiB for zstd/allocator scratch; the retained
/// result permit may legitimately be zero and cannot cover this peak.
const HISTORY_LOAD_TRANSIENT_ESTIMATED_BYTES: usize = 64 * 1024 * 1024;
/// Runtime-only save workspace. Pending closures are already charged for their
/// owned snapshots; the single worker reserves this only while it streams one
/// save. Half covers the worst overlap of one encoded frame, decode scratch,
/// two bounded BlockData values and target-codec serialization. Half covers
/// metadata for at most one complete incoming snapshot. Exact source revisions
/// are validated one record at a time and then discarded; stale sources are
/// rejected instead of being union-merged, so they never add candidate rows.
const HISTORY_SAVE_WORKING_ESTIMATED_BYTES: usize = 128 * 1024 * 1024;
const HISTORY_SAVE_RECORD_TRANSIENT_ESTIMATED_BYTES: usize = 64 * 1024 * 1024;
const HISTORY_SAVE_METADATA_BYTES_PER_RECORD: usize = 192;
const MAX_HISTORY_SAVE_CANDIDATE_RECORDS: usize = MAX_HISTORY_FRAMES;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LegacyHistoryAuthority {
    /// This pane loaded its own session target (or no file), so the shared
    /// legacy sibling is unrelated and must neither be merged nor removed.
    Ignore,
    /// The legacy source was not fully observed. A normal save must fail while
    /// it remains present; union-merging a partial view could later resurrect
    /// records removed by another pane's explicit Clear.
    MergeOnly,
    /// A complete fallback load observed this exact legacy revision, allowing
    /// UI deletions only while the locked source still matches it.
    Revision(HistoryRevision),
}

pub(super) struct LoadedHistory {
    blocks: Arc<Vec<BlockData>>,
    total_loaded: usize,
    /// `None` means this load did not observe every eligible disk record and
    /// therefore must reload before a normal save can authorize replacement.
    target_revision: Option<HistoryRevision>,
    legacy_authority: LegacyHistoryAuthority,
    retained_estimated_bytes: usize,
    _reservation: Option<persistence::EstimatedBytesReservation>,
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

struct HistoryLoadState {
    outcome: HistoryLoadOutcome,
    pre_apply_save_leases: usize,
    consume_requested: bool,
}

pub(super) struct HistoryLoadShared {
    state: Mutex<HistoryLoadState>,
    revision: Mutex<Option<HistoryRevision>>,
    legacy_authority: Mutex<LegacyHistoryAuthority>,
    applied: AtomicBool,
    discarded: AtomicBool,
    explicit_replace_epoch: AtomicU64,
    persisted_replace_epoch: AtomicU64,
}

impl Default for HistoryLoadShared {
    fn default() -> Self {
        Self {
            state: Mutex::new(HistoryLoadState {
                outcome: HistoryLoadOutcome::Idle,
                pre_apply_save_leases: 0,
                consume_requested: false,
            }),
            revision: Mutex::new(None),
            legacy_authority: Mutex::new(LegacyHistoryAuthority::MergeOnly),
            applied: AtomicBool::new(true),
            discarded: AtomicBool::new(false),
            explicit_replace_epoch: AtomicU64::new(0),
            persisted_replace_epoch: AtomicU64::new(0),
        }
    }
}

struct PreApplyHistorySaveLease {
    shared: Arc<HistoryLoadShared>,
}

impl Drop for PreApplyHistorySaveLease {
    fn drop(&mut self) {
        self.shared.release_pre_apply_save_lease();
    }
}

impl HistoryLoadShared {
    /// Cancel any in-flight restore and persist the user's explicit request to
    /// replace history. This intent is independent of load deletion authority:
    /// resource pressure may make ordinary saves require a complete reload,
    /// but must not make a successful Clear reappear on the next launch.
    pub(super) fn discard_for_explicit_clear(&self) {
        self.explicit_replace_epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                Some(
                    epoch
                        .checked_add(1)
                        .expect("explicit Block-history replacement epoch exhausted"),
                )
            })
            .expect("explicit Block-history replacement epoch update is infallible");
        self.discarded.store(true, Ordering::Release);
        self.applied.store(true, Ordering::Release);
        let prior = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.consume_requested = true;
            (state.pre_apply_save_leases == 0)
                .then(|| std::mem::replace(&mut state.outcome, HistoryLoadOutcome::Idle))
        };
        drop(prior);
    }

    fn pending_explicit_replace_epoch(&self) -> Option<u64> {
        let requested = self.explicit_replace_epoch.load(Ordering::Acquire);
        let persisted = self.persisted_replace_epoch.load(Ordering::Acquire);
        (requested > persisted).then_some(requested)
    }

    fn mark_explicit_replace_persisted(&self, epoch: u64) {
        self.persisted_replace_epoch
            .fetch_max(epoch, Ordering::AcqRel);
    }

    fn begin(&self) {
        let prior = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            debug_assert_eq!(state.pre_apply_save_leases, 0);
            state.consume_requested = false;
            std::mem::replace(&mut state.outcome, HistoryLoadOutcome::Pending)
        };
        // LoadedHistory may own a persistence reservation. Its destructor must
        // never run while the history mutex is held: persistence replacement
        // drops save leases in the opposite lock order.
        drop(prior);
        self.discarded.store(false, Ordering::Release);
        self.applied.store(false, Ordering::Release);
    }

    fn complete(&self, result: &io::Result<Arc<LoadedHistory>>) {
        *self
            .revision
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = result
            .as_ref()
            .ok()
            .and_then(|loaded| loaded.target_revision);
        *self
            .legacy_authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = result
            .as_ref()
            .map_or(LegacyHistoryAuthority::MergeOnly, |loaded| {
                loaded.legacy_authority
            });
        let outcome = match result {
            Ok(loaded) => HistoryLoadOutcome::Loaded(Arc::clone(loaded)),
            Err(error) => HistoryLoadOutcome::Failed {
                kind: error.kind(),
                message: Arc::from(error.to_string()),
            },
        };
        let prior = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let next = if self.discarded.load(Ordering::Acquire) {
                HistoryLoadOutcome::Idle
            } else {
                outcome
            };
            std::mem::replace(&mut state.outcome, next)
        };
        // See `begin`: both the displaced outcome and a discarded new result
        // can release a persistence permit, so drop them outside this mutex.
        drop(prior);
    }

    fn outcome(&self) -> HistoryLoadOutcome {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .outcome
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

    fn legacy_authority(&self) -> LegacyHistoryAuthority {
        *self
            .legacy_authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_legacy_authority(&self, authority: LegacyHistoryAuthority) {
        *self
            .legacy_authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = authority;
    }

    fn acquire_pre_apply_save_lease(self: &Arc<Self>) -> Option<PreApplyHistorySaveLease> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.applied.load(Ordering::Acquire) || self.discarded.load(Ordering::Acquire) {
            return None;
        }
        state.pre_apply_save_leases = state
            .pre_apply_save_leases
            .checked_add(1)
            .expect("pre-apply history save lease count exhausted");
        Some(PreApplyHistorySaveLease {
            shared: Arc::clone(self),
        })
    }

    fn release_pre_apply_save_lease(&self) {
        let prior = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.pre_apply_save_leases = state
                .pre_apply_save_leases
                .checked_sub(1)
                .expect("pre-apply history save lease accounting underflow");
            (state.pre_apply_save_leases == 0 && state.consume_requested)
                .then(|| std::mem::replace(&mut state.outcome, HistoryLoadOutcome::Idle))
        };
        drop(prior);
    }

    /// Mark the outcome consumed. A save which snapshotted the UI before apply
    /// holds a lease, keeping LoadedHistory (and its reservation) alive until
    /// that closure has streamed its prefix or its rejected task is dropped.
    fn mark_applied_and_consume(&self) {
        let prior = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.applied.store(true, Ordering::Release);
            state.consume_requested = true;
            (state.pre_apply_save_leases == 0)
                .then(|| std::mem::replace(&mut state.outcome, HistoryLoadOutcome::Idle))
        };
        drop(prior);
    }
}

fn decode_zstd_bounded(data: &[u8], max_decoded_bytes: u64) -> io::Result<Vec<u8>> {
    let mut decoder =
        zstd::Decoder::new(data).map_err(|error| io::Error::other(error.to_string()))?;
    // `Read::take` bounds produced bytes but not zstd's internal history
    // window. Refuse frames which advertise more working memory than one
    // maximum decoded record before the first read can allocate that window.
    decoder.window_log_max(HISTORY_ZSTD_WINDOW_LOG_MAX)?;
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

/// Exact schema immediately before lifecycle provenance was persisted.
/// Keeping this separate from the older bare-i32 V1 prevents a normal recent
/// history file from becoming undecodable when BlockData's archive grows.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct LegacyBlockDataV2 {
    id: u64,
    prompt: String,
    cmd: String,
    cmd_markup: Option<String>,
    output: String,
    exit_code: Option<i32>,
    estimated_height: i32,
    line_count: usize,
    start_time_ms: Option<u64>,
    end_time_ms: Option<u64>,
    duration_ms: Option<u64>,
    cwd: Option<String>,
    cols: u16,
}

impl From<LegacyBlockDataV2> for BlockData {
    fn from(legacy: LegacyBlockDataV2) -> Self {
        let is_background = legacy.cmd.trim().is_empty();
        let trusted_completion = !is_background && legacy.exit_code.is_some();
        Self {
            id: legacy.id,
            prompt: legacy.prompt,
            cmd: legacy.cmd,
            cmd_markup: legacy.cmd_markup,
            output: legacy.output,
            exit_code: (!is_background).then_some(legacy.exit_code).flatten(),
            lifecycle_schema: super::blocks::BLOCK_LIFECYCLE_SCHEMA,
            completion_provenance: if trusted_completion {
                super::CompletionProvenance::JournalRecovered
            } else {
                super::CompletionProvenance::Unknown
            }
            .into(),
            start_mark_seen: trusted_completion,
            estimated_height: legacy.estimated_height,
            line_count: legacy.line_count,
            start_time_ms: trusted_completion.then_some(legacy.start_time_ms).flatten(),
            end_time_ms: trusted_completion.then_some(legacy.end_time_ms).flatten(),
            duration_ms: trusted_completion.then_some(legacy.duration_ms).flatten(),
            cwd: legacy.cwd,
            cols: legacy.cols,
        }
    }
}

/// The record shape every save before round 8 used: `exit_code` was a bare
/// `i32`, so zero conflated success with an unreported status. Kept only so
/// those files still decode without silently dropping the user's history.
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
        let is_background = legacy.cmd.trim().is_empty();
        let trusted_completion = !is_background && legacy.exit_code != 0;
        Self {
            id: legacy.id,
            prompt: legacy.prompt,
            cmd: legacy.cmd,
            cmd_markup: legacy.cmd_markup,
            output: legacy.output,
            // Zero cannot distinguish success from an unreported status and
            // is therefore normalized to None; a nonzero legacy code is
            // unambiguous evidence of a reported completion.
            exit_code: trusted_completion.then_some(legacy.exit_code),
            lifecycle_schema: super::blocks::BLOCK_LIFECYCLE_SCHEMA,
            completion_provenance: if trusted_completion {
                super::CompletionProvenance::JournalRecovered
            } else {
                super::CompletionProvenance::Unknown
            }
            .into(),
            start_mark_seen: trusted_completion,
            estimated_height: legacy.estimated_height,
            line_count: legacy.line_count,
            start_time_ms: trusted_completion.then_some(legacy.start_time_ms).flatten(),
            end_time_ms: trusted_completion.then_some(legacy.end_time_ms).flatten(),
            duration_ms: trusted_completion.then_some(legacy.duration_ms).flatten(),
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
        .filter(|block| block.lifecycle_schema == super::blocks::BLOCK_LIFECYCLE_SCHEMA)
        .or_else(|| {
            rkyv::from_bytes::<LegacyBlockDataV2, rkyv::rancor::Error>(data)
                .ok()
                .map(BlockData::from)
        })
        .or_else(|| {
            rkyv::from_bytes::<LegacyBlockDataV1, rkyv::rancor::Error>(data)
                .ok()
                .map(BlockData::from)
        })
        .map(normalize_block_lifecycle)
}

fn normalize_block_lifecycle(mut block: BlockData) -> BlockData {
    if block.is_background() {
        block.exit_code = None;
        block.completion_provenance = super::CompletionProvenance::Unknown.into();
        block.start_mark_seen = false;
    }
    if block.is_background() || !block.timing_is_authoritative() {
        block.start_time_ms = None;
        block.end_time_ms = None;
        block.duration_ms = None;
    }
    block
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

/// Config loading owns path validation and `~/` expansion. Keep this final
/// consumer boundary fail-closed so a future in-memory caller cannot silently
/// reintroduce cwd-relative Block history.
fn absolute_history_path(path: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Block history path must be normalized to an absolute path",
        ));
    }
    Ok(path)
}

/// Kept only to exercise the old filename filter in tests. Runtime saves must
/// not prune siblings by mtime: an open or restorable pane can legitimately go
/// longer than this without saving, and deleting its file makes its revisioned
/// next save fail closed after the data is already gone.
#[cfg(test)]
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
#[cfg(test)]
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

/// Create a private read/write spool in the retained history directory and
/// unlink its name immediately. The descriptor remains usable for seek/copy,
/// while every failure path (including panic/process exit) lets the filesystem
/// reclaim the staging bytes without a pathname cleanup race.
fn create_unlinked_spool(parent_directory: &File, target: &Path) -> io::Result<File> {
    for _ in 0..128 {
        let temp_name = temp_file_name(target)?;
        let temp_name_c = os_name_cstring(&temp_name, "Block-history spool")?;
        // SAFETY: the name is relative to the retained directory; a successful
        // descriptor is uniquely transferred into File below.
        let descriptor = unsafe {
            nix::libc::openat(
                parent_directory.as_raw_fd(),
                temp_name_c.as_ptr(),
                nix::libc::O_RDWR
                    | nix::libc::O_CREAT
                    | nix::libc::O_EXCL
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW,
                0o600,
            )
        };
        if descriptor < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(error);
        }
        // SAFETY: descriptor is newly returned and uniquely owned.
        let file = unsafe { File::from_raw_fd(descriptor) };
        // SAFETY: the same retained directory and live C string are used; the
        // open descriptor remains valid after unlink.
        if unsafe { nix::libc::unlinkat(parent_directory.as_raw_fd(), temp_name_c.as_ptr(), 0) }
            != 0
        {
            let error = io::Error::last_os_error();
            drop(file);
            // Best-effort cleanup if the first unlink failed transiently.
            unsafe {
                nix::libc::unlinkat(parent_directory.as_raw_fd(), temp_name_c.as_ptr(), 0);
            }
            return Err(error);
        }
        return Ok(file);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a temporary Block-history spool",
    ))
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
    retained_estimated_bytes: usize,
    retained_records_dropped: usize,
    skipped_undecodable: usize,
}

/// Keep a chronological suffix within both the configured count and the
/// exact retained-result permit. The just-decoded record is the only memory
/// outside `retained_estimated_bytes`; the separate decoder reservation covers
/// that record and its encoded frame until this function either admits or
/// drops it.
fn push_loaded_record_bounded(
    blocks: &mut VecDeque<BlockData>,
    retained_costs: &mut VecDeque<usize>,
    block: BlockData,
    count_limit: usize,
    byte_limit: usize,
    retained_estimated_bytes: &mut usize,
) -> usize {
    debug_assert_eq!(blocks.len(), retained_costs.len());
    if count_limit == 0 {
        return 1;
    }

    let mut dropped = 0usize;
    while blocks.len() >= count_limit {
        blocks.pop_front();
        let removed = retained_costs
            .pop_front()
            .expect("history retention costs stay aligned with records");
        *retained_estimated_bytes = retained_estimated_bytes
            .checked_sub(removed)
            .expect("history retained-byte accounting underflow");
        dropped = dropped.saturating_add(1);
    }

    let cost = estimated_loaded_block_owned_bytes(&block);
    if cost > byte_limit {
        // With the full product budget every valid record fits and the normal
        // newest-wins rule applies. Under global pressure a smaller permit may
        // not fit even one record; keep no non-contiguous older prefix and
        // revoke deletion authority at the caller.
        dropped = dropped.saturating_add(blocks.len()).saturating_add(1);
        blocks.clear();
        retained_costs.clear();
        *retained_estimated_bytes = 0;
        return dropped;
    }

    while !blocks.is_empty() && *retained_estimated_bytes > byte_limit - cost {
        blocks.pop_front();
        let removed = retained_costs
            .pop_front()
            .expect("history retention costs stay aligned with records");
        *retained_estimated_bytes = retained_estimated_bytes
            .checked_sub(removed)
            .expect("history retained-byte accounting underflow");
        dropped = dropped.saturating_add(1);
    }

    *retained_estimated_bytes = retained_estimated_bytes
        .checked_add(cost)
        .expect("admission check guarantees history retained-byte sum fits");
    blocks.push_back(block);
    retained_costs.push_back(cost);
    debug_assert!(*retained_estimated_bytes <= byte_limit);
    dropped
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
        None,
    )
}

fn read_history_records_in_directory(
    directory: &File,
    path: &Path,
    prefer_compressed: bool,
    keep_limit: usize,
    undecodable_policy: UndecodablePolicy,
    retained_budget: Option<usize>,
) -> io::Result<LoadedRecords> {
    let mut blocks = VecDeque::new();
    let mut retained_costs = VecDeque::new();
    let mut retained_estimated_bytes = 0usize;
    let mut retained_records_dropped = 0usize;
    let scanned = scan_history_records_in_directory(
        directory,
        path,
        prefer_compressed,
        undecodable_policy,
        |block| {
            if let Some(retained_budget) = retained_budget {
                retained_records_dropped =
                    retained_records_dropped.saturating_add(push_loaded_record_bounded(
                        &mut blocks,
                        &mut retained_costs,
                        block,
                        keep_limit,
                        retained_budget,
                        &mut retained_estimated_bytes,
                    ));
            } else {
                push_bounded_back(&mut blocks, block, keep_limit);
            }
            Ok(())
        },
    )?;
    Ok(LoadedRecords {
        blocks,
        total_loaded: scanned.total_loaded,
        revision: scanned.revision,
        retained_estimated_bytes,
        retained_records_dropped,
        skipped_undecodable: scanned.skipped_undecodable,
    })
}

#[derive(Clone, Copy)]
struct ScannedHistory {
    total_loaded: usize,
    revision: HistoryRevision,
    skipped_undecodable: usize,
}

/// Strictly bound and decode each frame, handing off at most one owned record
/// at a time. Save paths use this callback form so validation never implies
/// retaining the complete 256 MiB decoded file.
fn scan_history_records_in_directory(
    directory: &File,
    path: &Path,
    prefer_compressed: bool,
    undecodable_policy: UndecodablePolicy,
    mut on_block: impl FnMut(BlockData) -> io::Result<()>,
) -> io::Result<ScannedHistory> {
    let Some((mut file, metadata)) = open_history_file_in_directory(directory, path)? else {
        return Ok(ScannedHistory {
            total_loaded: 0,
            revision: HistoryRevision::Missing,
            skipped_undecodable: 0,
        });
    };
    let revision = HistoryRevision::from_metadata(&metadata);
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
        let decoded = decode_block_record(&data, prefer_compressed);
        drop(data);
        match decoded {
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
                on_block(block)?;
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
    Ok(ScannedHistory {
        total_loaded,
        revision,
        skipped_undecodable: undecodable,
    })
}

fn read_history_snapshot(
    base: &Path,
    session_id: Option<&str>,
    prefer_compressed: bool,
    load_limit: usize,
) -> io::Result<Arc<LoadedHistory>> {
    read_history_snapshot_with_retained_budget(
        base,
        session_id,
        prefer_compressed,
        load_limit,
        MAX_COMPLETED_BLOCK_RETAINED_BYTES,
    )
    .map(Arc::new)
}

fn read_history_snapshot_with_retained_budget(
    base: &Path,
    session_id: Option<&str>,
    prefer_compressed: bool,
    load_limit: usize,
    retained_budget: usize,
) -> io::Result<LoadedHistory> {
    let session_path = session_id.map(|sid| per_session_history_path(base, sid));
    let target = session_path.as_deref().unwrap_or(base);
    let Some(path) = choose_load_path(base, session_path.as_deref()) else {
        return Ok(LoadedHistory {
            blocks: Arc::new(Vec::new()),
            total_loaded: 0,
            target_revision: Some(HistoryRevision::Missing),
            legacy_authority: LegacyHistoryAuthority::Ignore,
            retained_estimated_bytes: 0,
            _reservation: None,
        });
    };
    let directory = open_parent_directory(&path)?;
    let mut loaded = read_history_records_in_directory(
        &directory,
        &path,
        prefer_compressed,
        load_limit,
        UndecodablePolicy::Skip,
        Some(retained_budget),
    )?;
    let observed_target_revision = if path == target {
        loaded.revision
    } else {
        HistoryRevision::Missing
    };
    let deletion_authority =
        loaded.retained_records_dropped == 0 && loaded.skipped_undecodable == 0;
    let target_revision = deletion_authority.then_some(observed_target_revision);
    let legacy_authority = if path == target {
        LegacyHistoryAuthority::Ignore
    } else if deletion_authority {
        LegacyHistoryAuthority::Revision(loaded.revision)
    } else {
        LegacyHistoryAuthority::MergeOnly
    };

    if loaded.total_loaded > load_limit {
        log::info!(
            "Loading Block history: keeping {} recent blocks out of {} total",
            load_limit,
            loaded.total_loaded
        );
    }
    if loaded.retained_records_dropped > 0 {
        log::warn!(
            "Block history load omitted {} record(s) to fit its count/{}-byte retained-result limits; normal saves require a complete reload",
            loaded.retained_records_dropped,
            retained_budget,
        );
    }
    refresh_loaded_block_ids(&mut loaded.blocks);
    Ok(LoadedHistory {
        blocks: Arc::new(loaded.blocks.into_iter().collect()),
        total_loaded: loaded.total_loaded,
        target_revision,
        legacy_authority,
        retained_estimated_bytes: loaded.retained_estimated_bytes,
        _reservation: None,
    })
}

/// Reserve decoder working memory first, then take only the currently
/// available result budget. No permit waits: a queued weighted save may own
/// the missing capacity and is itself waiting for this single worker.
fn read_history_snapshot_reserved(
    base: &Path,
    session_id: Option<&str>,
    prefer_compressed: bool,
    load_limit: usize,
) -> io::Result<Arc<LoadedHistory>> {
    let _transient =
        persistence::try_reserve_estimated_bytes(HISTORY_LOAD_TRANSIENT_ESTIMATED_BYTES)?;
    let mut reservation =
        persistence::reserve_estimated_bytes_up_to(MAX_COMPLETED_BLOCK_RETAINED_BYTES)?;
    let retained_budget = reservation.estimated_bytes();
    let mut loaded = read_history_snapshot_with_retained_budget(
        base,
        session_id,
        prefer_compressed,
        load_limit,
        retained_budget,
    )?;
    debug_assert!(loaded.retained_estimated_bytes <= retained_budget);
    reservation.shrink_to(loaded.retained_estimated_bytes);
    if loaded.retained_estimated_bytes > 0 {
        loaded._reservation = Some(reservation);
    }
    Ok(Arc::new(loaded))
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

/// Heap actually retained by a decoded background-load result. Widget/VTE
/// reconstruction is a later UI concern with its own per-pane retention plan;
/// charging that future cost here would reject a valid 4–8 MiB newest record
/// before the UI can apply the product's explicit newest-block exception.
///
/// A growing `VecDeque` can retain nearly two inline slots per record. During
/// its conversion to `Vec`, that allocation can briefly coexist with one new
/// inline slot while String allocations move, so charge three BlockData slots
/// (plus the parallel cost ledger) and the exact owned String capacities.
fn estimated_loaded_block_owned_bytes(block: &BlockData) -> usize {
    std::mem::size_of::<BlockData>()
        .saturating_mul(3)
        .saturating_add(std::mem::size_of::<usize>().saturating_mul(2))
        .saturating_add(block.prompt.capacity())
        .saturating_add(block.cmd.capacity())
        .saturating_add(block.cmd_markup.as_ref().map_or(0, String::capacity))
        .saturating_add(block.output.capacity())
        .saturating_add(block.cwd.as_ref().map_or(0, String::capacity))
}

/// Estimate the allocations moved into the persistence closure. Use String
/// capacities rather than lengths and include spare Vec capacity so admission
/// never undercounts memory that the snapshot already owns. Saturating every
/// term turns an unrepresentable estimate into `usize::MAX`, which the weighted
/// queue rejects instead of wrapping it into a small value.
fn estimated_snapshot_retained_bytes(blocks: &[BlockData], capacity: usize) -> usize {
    let inline = capacity.saturating_mul(std::mem::size_of::<BlockData>());
    blocks.iter().fold(
        std::mem::size_of::<Vec<BlockData>>().saturating_add(inline),
        |total, block| {
            total
                .saturating_add(block.prompt.capacity())
                .saturating_add(block.cmd.capacity())
                .saturating_add(block.cmd_markup.as_ref().map_or(0, String::capacity))
                .saturating_add(block.output.capacity())
                .saturating_add(block.cwd.as_ref().map_or(0, String::capacity))
        },
    )
}

fn estimated_history_save_working_bytes(candidate_records: usize) -> usize {
    HISTORY_SAVE_RECORD_TRANSIENT_ESTIMATED_BYTES
        .saturating_add(1024 * 1024)
        .saturating_add(candidate_records.saturating_mul(HISTORY_SAVE_METADATA_BYTES_PER_RECORD))
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
    // `into_boxed_slice` discards growth slack before the closure is admitted,
    // making the queue's Vec-inline estimate exact rather than aspirational.
    newest_first.into_boxed_slice().into_vec()
}

#[derive(Clone, Copy)]
struct SpoolRecord {
    offset: u64,
    encoded_len: u32,
    decoded_len: u32,
}

#[derive(Clone, Copy)]
struct SpoolEntry {
    record: SpoolRecord,
}

#[derive(Default)]
struct SpoolCandidateSet {
    entries: Vec<SpoolEntry>,
}

fn append_spooled_block(
    spool: &mut File,
    block: &BlockData,
    compress: bool,
) -> io::Result<Option<SpoolRecord>> {
    let serialized = rkyv::to_bytes::<rkyv::rancor::Error>(block)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if serialized.len() > MAX_DECODED_RECORD_BYTES as usize {
        log::warn!(
            "save_history: skipping a {} byte block (decoded record limit {})",
            serialized.len(),
            MAX_DECODED_RECORD_BYTES,
        );
        return Ok(None);
    }
    let encoded = if compress {
        zstd::encode_all(serialized.as_slice(), 3)
            .map_err(|error| io::Error::other(error.to_string()))?
    } else {
        serialized.as_slice().to_vec()
    };
    if encoded.len() > MAX_ENCODED_RECORD_BYTES || encoded.len() > u32::MAX as usize {
        log::warn!(
            "save_history: skipping a {} byte block (encoded record limit {})",
            encoded.len(),
            MAX_ENCODED_RECORD_BYTES,
        );
        return Ok(None);
    }
    let offset = spool.seek(SeekFrom::End(0))?;
    spool.write_all(&encoded)?;
    Ok(Some(SpoolRecord {
        offset,
        encoded_len: encoded.len() as u32,
        decoded_len: serialized.len() as u32,
    }))
}

impl SpoolCandidateSet {
    /// Preserve event multiplicity and chronological order. Identical command
    /// records are distinct terminal events and must not be set-deduplicated.
    fn push_block(
        &mut self,
        spool: &mut File,
        block: &BlockData,
        compress: bool,
    ) -> io::Result<()> {
        let Some(record) = append_spooled_block(spool, block, compress)? else {
            return Ok(());
        };
        self.entries.push(SpoolEntry { record });
        Ok(())
    }
}

fn spool_snapshot_sources(
    prefix: &[BlockData],
    blocks: &[BlockData],
    compress: bool,
    spool: &mut File,
) -> io::Result<SpoolCandidateSet> {
    let mut source = SpoolCandidateSet::default();
    let skip = prefix
        .len()
        .saturating_add(blocks.len())
        .saturating_sub(MAX_HISTORY_FRAMES);
    for block in prefix.iter().chain(blocks).skip(skip) {
        source.push_block(spool, block, compress)?;
    }
    Ok(source)
}

fn selected_spool_entries(candidates: &SpoolCandidateSet) -> io::Result<Vec<usize>> {
    let mut newest_first = Vec::new();
    let mut encoded_bytes = 0usize;
    let mut decoded_bytes = 0u64;
    for (index, entry) in candidates.entries.iter().enumerate().rev() {
        if newest_first.len() == MAX_HISTORY_FRAMES {
            log::warn!("save_history: keeping newer blocks within the record-count limit");
            break;
        }
        let frame_bytes = 4usize
            .checked_add(entry.record.encoded_len as usize)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "Block history frame size overflow",
                )
            })?;
        let Some(next_encoded) = encoded_bytes.checked_add(frame_bytes) else {
            break;
        };
        let Some(next_decoded) = decoded_bytes.checked_add(entry.record.decoded_len as u64) else {
            break;
        };
        if next_encoded > MAX_HISTORY_FILE_BYTES as usize
            || next_decoded > MAX_HISTORY_DECODED_BYTES
        {
            log::warn!("save_history: keeping newer blocks within encoded and decoded byte limits");
            break;
        }
        encoded_bytes = next_encoded;
        decoded_bytes = next_decoded;
        newest_first.push(index);
    }
    Ok(newest_first)
}

fn copy_spooled_record(
    spool: &mut File,
    target: &mut File,
    record: SpoolRecord,
    buffer: &mut [u8],
) -> io::Result<()> {
    target.write_all(&record.encoded_len.to_le_bytes())?;
    spool.seek(SeekFrom::Start(record.offset))?;
    let mut remaining = record.encoded_len as usize;
    while remaining > 0 {
        let chunk = remaining.min(buffer.len());
        spool.read_exact(&mut buffer[..chunk])?;
        target.write_all(&buffer[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

/// Encode a newest-first budget, then restore chronological order for disk.
/// A single pathological block is skipped, while reaching the aggregate
/// budget stops before any older record can displace a newer complete one.
#[cfg(test)]
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
    legacy_handled: bool,
    legacy_retry_revision: Option<HistoryRevision>,
}

#[derive(Clone, Copy, Debug)]
enum HistoryWriteIntent {
    Revision {
        target: Option<HistoryRevision>,
        legacy: LegacyHistoryAuthority,
    },
    ExplicitReplace,
}

#[cfg(test)]
fn write_history_snapshot(
    base: &Path,
    path: &Path,
    session_id: Option<&str>,
    blocks: &[BlockData],
    compress: bool,
    expected_revision: Option<HistoryRevision>,
) -> io::Result<SaveHistoryOutcome> {
    write_history_snapshot_with_intent(
        base,
        path,
        session_id,
        blocks,
        compress,
        HistoryWriteIntent::Revision {
            target: expected_revision,
            legacy: if base == path {
                LegacyHistoryAuthority::Ignore
            } else {
                LegacyHistoryAuthority::MergeOnly
            },
        },
    )
}

fn current_history_revision_in_directory(
    directory: &File,
    path: &Path,
) -> io::Result<HistoryRevision> {
    Ok(open_history_file_in_directory(directory, path)?
        .map(|(_, metadata)| HistoryRevision::from_metadata(&metadata))
        .unwrap_or(HistoryRevision::Missing))
}

fn ensure_scanned_revision(
    path: &Path,
    scanned: ScannedHistory,
    expected: HistoryRevision,
) -> io::Result<()> {
    if scanned.revision == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "Block history changed during its locked save scan: {}",
                path.display()
            ),
        ))
    }
}

fn validate_history_source(
    directory: &File,
    path: &Path,
    prefer_compressed: bool,
    expected_revision: HistoryRevision,
) -> io::Result<()> {
    let scanned = scan_history_records_in_directory(
        directory,
        path,
        prefer_compressed,
        UndecodablePolicy::Reject,
        |_| Ok(()),
    )?;
    ensure_scanned_revision(path, scanned, expected_revision)
}

fn remove_history_source_in_directory(
    directory: &File,
    path: &Path,
    expected_revision: HistoryRevision,
) -> io::Result<()> {
    let Some((file, metadata)) = open_history_file_in_directory(directory, path)? else {
        return Ok(());
    };
    if HistoryRevision::from_metadata(&metadata) != expected_revision {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "legacy Block history changed before it could be removed",
        ));
    }
    drop(file);
    let name = relative_name_cstring(path, "legacy Block history")?;
    // SAFETY: `name` is a relative basename and `directory` retains the
    // validated, exclusively locked parent directory for this call.
    if unsafe { nix::libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    directory.sync_all()
}

fn legacy_removal_state(
    should_consume: bool,
    revision: HistoryRevision,
    removal_succeeded: bool,
) -> (bool, Option<HistoryRevision>) {
    if !should_consume {
        (false, None)
    } else if revision == HistoryRevision::Missing || removal_succeeded {
        (true, None)
    } else {
        (false, Some(revision))
    }
}

fn write_history_snapshot_with_intent(
    base: &Path,
    path: &Path,
    session_id: Option<&str>,
    blocks: &[BlockData],
    compress: bool,
    intent: HistoryWriteIntent,
) -> io::Result<SaveHistoryOutcome> {
    write_history_snapshot_with_intent_parts(base, path, session_id, &[], blocks, compress, intent)
}

#[allow(clippy::too_many_arguments)]
fn write_history_snapshot_with_intent_parts(
    base: &Path,
    path: &Path,
    session_id: Option<&str>,
    prefix: &[BlockData],
    blocks: &[BlockData],
    compress: bool,
    intent: HistoryWriteIntent,
) -> io::Result<SaveHistoryOutcome> {
    let lock = HistoryFileLock::acquire(path)?;
    let target_revision = current_history_revision_in_directory(&lock.directory, path)?;
    let legacy_revision = if base != path {
        debug_assert_eq!(base.parent(), path.parent());
        current_history_revision_in_directory(&lock.directory, base)?
    } else {
        HistoryRevision::Missing
    };
    let (legacy_should_consume, authoritative) = match intent {
        HistoryWriteIntent::ExplicitReplace => (base != path, true),
        HistoryWriteIntent::Revision {
            target: expected_target,
            legacy,
        } => {
            match expected_target {
                Some(expected) if expected == target_revision => {}
                None if target_revision == HistoryRevision::Missing => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Block history changed since this pane loaded it; reload before saving",
                    ));
                }
            };
            let (legacy_should_consume, legacy_safe) = match legacy {
                LegacyHistoryAuthority::Ignore => (false, true),
                LegacyHistoryAuthority::MergeOnly
                    if legacy_revision == HistoryRevision::Missing =>
                {
                    (true, true)
                }
                LegacyHistoryAuthority::MergeOnly => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "legacy Block history was not fully loaded; reload before migrating it",
                    ));
                }
                LegacyHistoryAuthority::Revision(expected) => {
                    if expected != legacy_revision && legacy_revision != HistoryRevision::Missing {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "legacy Block history changed since this pane loaded it; reload before migrating it",
                        ));
                    }
                    (true, true)
                }
            };
            (legacy_should_consume, legacy_safe)
        }
    };

    let mut spool = create_unlinked_spool(&lock.directory, path)?;

    if base != path && legacy_should_consume && legacy_revision != HistoryRevision::Missing {
        validate_history_source(&lock.directory, base, compress, legacy_revision)?;
    }

    validate_history_source(&lock.directory, path, compress, target_revision)?;

    let candidates = spool_snapshot_sources(prefix, blocks, compress, &mut spool)?;
    let selected = selected_spool_entries(&candidates)?;
    let result = atomic_write_in_directory(&lock.directory, path, |file| {
        let mut buffer = vec![0u8; 64 * 1024];
        for &index in selected.iter().rev() {
            copy_spooled_record(
                &mut spool,
                file,
                candidates.entries[index].record,
                &mut buffer,
            )?;
        }
        Ok(())
    });

    let mut legacy_removal_succeeded = legacy_revision == HistoryRevision::Missing;
    if result.is_ok() && session_id.is_some() {
        // This tab's history now lives in its own file; the legacy shared file
        // is removed only when this save explicitly handled it by exact
        // revision validation or replacement. A pane which loaded its own target must not
        // consume an unrelated fallback source.
        if base != path && legacy_should_consume && legacy_revision != HistoryRevision::Missing {
            match remove_history_source_in_directory(&lock.directory, base, legacy_revision) {
                Ok(()) => legacy_removal_succeeded = true,
                Err(error) => {
                    log::warn!(
                        "could not remove superseded shared block history {}; its exact revision will be retried: {error}",
                        base.display()
                    );
                }
            }
        }
    }
    result?;
    let (legacy_handled, legacy_retry_revision) = legacy_removal_state(
        base != path && legacy_should_consume,
        legacy_revision,
        legacy_removal_succeeded,
    );
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
        legacy_handled,
        legacy_retry_revision,
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

    /// Where this pane's bounded zone document lives: a sibling of the Block
    /// history file, distinct by stem so a pane that changes mode between
    /// runs can never decode one as the other.
    fn zone_history_path(&self) -> Option<PathBuf> {
        let configured = self.config.borrow().block_history_path.as_ref().cloned()?;
        let base = absolute_history_path(&configured).ok()?;
        let stem = base
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "blocks".to_string());
        let name = match base.extension() {
            Some(ext) => format!("{stem}-zones.{}", ext.to_string_lossy()),
            None => format!("{stem}-zones"),
        };
        let zone_base = base.with_file_name(name);
        Some(match self.session_id.as_deref() {
            Some(sid) => per_session_history_path(&zone_base, sid),
            None => zone_base,
        })
    }

    /// Persist the backend's bounded zone document. Encoding is small and
    /// bounded by construction, so it stays on this thread rather than
    /// contending for the Block persistence worker.
    fn save_zone_history(&self) -> std::io::Result<()> {
        let Some(zones) = self.render_backend.zone_replay_snapshot(
            zone_history::MAX_RESTORED_ZONES,
            zone_history::MAX_RESTORED_SNAPSHOT_BYTES,
        ) else {
            return Ok(());
        };
        let Some(path) = self.zone_history_path() else {
            return Ok(());
        };
        if zones.is_empty() {
            // An empty session must not leave a stale document behind for the
            // next run to replay.
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            return Ok(());
        }
        let encoded = zone_history::encode_session(zones)?;
        atomic_write(&path, |file| file.write_all(&encoded))
    }

    /// Replay this pane's persisted zones onto the surface before any PTY
    /// byte reaches it. A failed or unreadable document is logged and skipped:
    /// a restart with no history is a working pane, and refusing to start is
    /// not.
    fn restore_zone_history(&self) {
        let Some(path) = self.zone_history_path() else {
            return;
        };
        let zones = match zone_history::read_session(&path) {
            Ok(zones) => zones,
            Err(error) => {
                log::warn!("zone history not restored from {}: {error}", path.display());
                return;
            }
        };
        if zones.is_empty() {
            return;
        }
        let restored = self.render_backend.replay_zone_snapshot(zones);
        log::debug!("restored {restored} zones from {}", path.display());
    }

    /// Snapshot block history on the GTK thread and queue all encoding and
    /// durable file I/O on the shared persistence worker.
    pub fn save_history(&self) -> std::io::Result<()> {
        if !self.persist_history_on_drop.get() {
            return Ok(());
        }
        // A backend that does not own the Block card document persists its
        // own bounded zone document instead, on a sibling path, so neither
        // representation can overwrite the other.
        if !self.render_backend.persists_block_history() {
            return self.save_zone_history();
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

        let base = absolute_history_path(&path_opt.unwrap())?;
        let session_id = self.session_id.clone();
        let path = match session_id.as_deref() {
            Some(sid) => per_session_history_path(&base, sid),
            None => base.clone(),
        };
        let records = self.render_backend.records();
        let Some(block_data) = records.block_data() else {
            return Ok(());
        };
        let blocks = snapshot_live_blocks_bounded(
            block_data,
            max_blocks,
            MAX_DECODED_RECORD_BYTES,
            MAX_HISTORY_DECODED_BYTES.saturating_sub(std::mem::size_of::<Vec<BlockData>>() as u64),
        );
        let explicit_replace_epoch = self.history_load.pending_explicit_replace_epoch();
        let pre_apply_save_lease = explicit_replace_epoch
            .is_none()
            .then(|| self.history_load.acquire_pre_apply_save_lease())
            .flatten();
        let estimated_bytes = estimated_snapshot_retained_bytes(&blocks, blocks.capacity());
        let history_load = Arc::clone(&self.history_load);
        let key = PersistenceKey::for_path("block-history", &path);
        persistence::enqueue_weighted(key, "Save Block history", estimated_bytes, move || {
            let _pre_apply_save_lease = pre_apply_save_lease;
            let loaded_for_save = if _pre_apply_save_lease.is_some() {
                if history_load.discarded.load(Ordering::Acquire) {
                    // Clear/teardown queued a replacement snapshot after
                    // discarding this load. Never let the earlier UI snapshot
                    // resurrect history that the user just removed.
                    return Ok(());
                }
                match history_load.outcome() {
                    HistoryLoadOutcome::Loaded(loaded) => Some(loaded),
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
                    HistoryLoadOutcome::Idle => {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "pre-apply Block history outcome was consumed before its save lease",
                        ));
                    }
                }
            } else {
                None
            };
            let loaded_prefix = loaded_for_save.as_ref().map_or(&[][..], |loaded| {
                let available = max_blocks.saturating_sub(blocks.len());
                let start = loaded.blocks.len().saturating_sub(available);
                &loaded.blocks[start..]
            });
            debug_assert!(
                estimated_history_save_working_bytes(MAX_HISTORY_SAVE_CANDIDATE_RECORDS)
                    <= HISTORY_SAVE_WORKING_ESTIMATED_BYTES
            );
            let _working_reservation =
                persistence::try_reserve_estimated_bytes(HISTORY_SAVE_WORKING_ESTIMATED_BYTES)?;
            let intent = explicit_replace_epoch.map_or_else(
                || HistoryWriteIntent::Revision {
                    target: history_load.revision(),
                    legacy: history_load.legacy_authority(),
                },
                |_| HistoryWriteIntent::ExplicitReplace,
            );
            let outcome = write_history_snapshot_with_intent_parts(
                &base,
                &path,
                session_id.as_deref(),
                loaded_prefix,
                &blocks,
                compress,
                intent,
            )?;
            if let Some(epoch) = explicit_replace_epoch {
                history_load.mark_explicit_replace_persisted(epoch);
            }
            // Only a complete, revision-matched snapshot grants deletion
            // authority. Any later mismatch fails closed until reload.
            history_load.set_revision(outcome.authoritative.then_some(outcome.revision));
            if outcome.legacy_handled {
                history_load.set_legacy_authority(LegacyHistoryAuthority::Ignore);
            } else if let Some(revision) = outcome.legacy_retry_revision {
                history_load.set_legacy_authority(LegacyHistoryAuthority::Revision(revision));
            }
            Ok(())
        })
    }

    /// Load and decode Block history on the shared disk worker, then construct
    /// GTK widgets in a short main-thread callback. Commands that finish while
    /// the read is pending remain newer than every restored block.
    pub(crate) fn start_history_load(self: &Rc<Self>) {
        // A backend without the Block card document restores its own bounded
        // zone document instead. That replay is synchronous on purpose: it
        // must reach the surface before the shell's first prompt, or restored
        // rows would land under output they precede.
        if !self.render_backend.persists_block_history() {
            self.restore_zone_history();
            return;
        }
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
        let base = match absolute_history_path(&path) {
            Ok(path) => path,
            Err(error) => {
                log::warn!("refusing invalid Block history path: {error}");
                return;
            }
        };

        if let Some(source) = self.history_load_poll_id.borrow_mut().take() {
            source.remove();
        }
        self.history_load.begin();
        let session_id = self.session_id.clone();
        let target = session_id
            .as_deref()
            .map(|sid| per_session_history_path(&base, sid))
            .unwrap_or_else(|| base.clone());
        let load_for_job = Arc::clone(&self.history_load);
        let key = PersistenceKey::unique_for_path("block-history-load", &target);
        if let Err(error) = persistence::enqueue(key, "Load Block history", move || {
            let result =
                read_history_snapshot_reserved(&base, session_id.as_deref(), compress, load_limit);
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
            if load_for_poll.discarded.load(Ordering::Acquire) {
                view.history_load_poll_id.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            match load_for_poll.outcome() {
                HistoryLoadOutcome::Idle
                    if load_for_poll.applied.load(Ordering::Acquire)
                        || load_for_poll.discarded.load(Ordering::Acquire) =>
                {
                    view.history_load_poll_id.borrow_mut().take();
                    glib::ControlFlow::Break
                }
                HistoryLoadOutcome::Idle | HistoryLoadOutcome::Pending => {
                    glib::ControlFlow::Continue
                }
                HistoryLoadOutcome::Loaded(loaded) => {
                    if !load_for_poll.discarded.load(Ordering::Acquire) {
                        view.apply_loaded_history(&loaded);
                    }
                    load_for_poll.mark_applied_and_consume();
                    view.history_load_poll_id.borrow_mut().take();
                    glib::ControlFlow::Break
                }
                HistoryLoadOutcome::Failed { message, .. } => {
                    log::warn!("load Block history: {message}");
                    // A later save may still succeed (for example, a removable
                    // drive was remounted). Pre-load shutdown saves preserve the
                    // unreadable file; subsequent user mutations may retry it.
                    load_for_poll.mark_applied_and_consume();
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
        self.clear_find();

        let retention_plan = {
            let finished = self.finished_blocks.borrow();
            super::plan_completed_block_retention_with_restored(&restored, &finished, max_blocks)
        };
        super::log_completed_block_retention("restoring persisted block history", retention_plan);
        let restored_evictions = retention_plan.evict_prefix.min(restored.len());
        restored.drain(..restored_evictions);
        let live_evictions = retention_plan
            .evict_prefix
            .saturating_sub(restored_evictions);
        super::evict_finished_block_prefix(
            live_evictions,
            &self.finished_blocks,
            &self.block_data,
            &self.block_list,
            &self.widget_pool,
            super::BlockRemovalRefs {
                selection: super::BlockSelectionRefs {
                    ids: &self.selected_block_ids,
                    active: &self.selected_block_id,
                    anchor: &self.selection_anchor_id,
                },
                bookmarks: &self.bookmarks,
                visible_indices: &self.visible_indices,
                failure_marker_redraw: self.failure_marker_redraw.as_ref(),
            },
        );

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
                if let Some(notice) = block.lifecycle_notice() {
                    finished.widget().set_tooltip_text(Some(&notice));
                }
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
                finished.connect_scroll_forwarding(&self.block_scroll, &self.scroll_debouncer);
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
        absolute_history_path, atomic_write, choose_load_path, decode_block_record,
        decode_zstd_bounded, encode_history_frames_bounded, estimated_history_save_working_bytes,
        estimated_loaded_block_owned_bytes, history_load_limit, loaded_prefix_for_live,
        lock_file_name, per_session_history_path, prune_stale_session_histories, push_bounded_back,
        push_loaded_record_bounded, read_history_records, read_history_snapshot,
        read_history_snapshot_reserved, read_history_snapshot_with_retained_budget,
        refresh_loaded_block_ids, snapshot_live_blocks_bounded, write_history_snapshot,
        write_history_snapshot_with_intent, write_history_snapshot_with_intent_parts, BlockData,
        HistoryFileLock, HistoryLoadOutcome, HistoryLoadShared, HistoryRevision,
        HistoryWriteIntent, LegacyHistoryAuthority, LoadedHistory, UndecodablePolicy,
        HISTORY_LOAD_TRANSIENT_ESTIMATED_BYTES, HISTORY_SAVE_WORKING_ESTIMATED_BYTES,
        MAX_ENCODED_RECORD_BYTES, MAX_HISTORY_COMMAND_BYTES, MAX_HISTORY_FILE_BYTES,
        MAX_HISTORY_SAVE_CANDIDATE_RECORDS,
    };
    use std::collections::VecDeque;
    use std::ffi::CString;
    use std::fs;
    use std::io::{self, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
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
            lifecycle_schema: super::super::blocks::BLOCK_LIFECYCLE_SCHEMA,
            completion_provenance: super::super::CompletionProvenance::ShellReported.into(),
            start_mark_seen: true,
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
    fn four_default_history_results_leave_room_for_one_decoder() {
        let blocks: Vec<_> = (0..200)
            .map(|id| sample_block(id, "printf small"))
            .collect();
        let candidates: Vec<_> = blocks
            .iter()
            .map(|block| (block.id, estimated_loaded_block_owned_bytes(block)))
            .collect();
        let plan = super::completed_block_retention_plan(
            &candidates,
            200,
            super::MAX_COMPLETED_BLOCK_RETAINED_BYTES,
        );

        assert_eq!(plan.retained_count, 200);
        let four_results_and_one_decoder = plan
            .retained_estimated_bytes
            .saturating_mul(4)
            .saturating_add(HISTORY_LOAD_TRANSIENT_ESTIMATED_BYTES);
        assert!(
            four_results_and_one_decoder <= crate::persistence::MAX_PENDING_ESTIMATED_BYTES,
            "four default panes plus the single worker's decoder need {four_results_and_one_decoder} bytes"
        );
    }

    #[test]
    fn streaming_pre_apply_save_fits_the_global_ledger_at_product_maxima() {
        let total = (super::MAX_HISTORY_DECODED_BYTES as usize)
            .saturating_add(super::MAX_COMPLETED_BLOCK_RETAINED_BYTES)
            .saturating_add(HISTORY_SAVE_WORKING_ESTIMATED_BYTES);
        assert_eq!(total, crate::persistence::MAX_PENDING_ESTIMATED_BYTES);
    }

    #[test]
    fn save_working_estimate_covers_the_max_snapshot_and_saturates() {
        let maximum = estimated_history_save_working_bytes(MAX_HISTORY_SAVE_CANDIDATE_RECORDS);
        assert!(maximum <= HISTORY_SAVE_WORKING_ESTIMATED_BYTES);
        assert_eq!(estimated_history_save_working_bytes(usize::MAX), usize::MAX);
    }

    #[test]
    fn loaded_owner_estimate_covers_vecdeque_to_vec_inline_peak() {
        let mut block = sample_block(1, "owner-cost");
        block.output = String::with_capacity(4096);
        block.output.push_str("payload");
        let string_capacities = block
            .prompt
            .capacity()
            .saturating_add(block.cmd.capacity())
            .saturating_add(block.cmd_markup.as_ref().map_or(0, String::capacity))
            .saturating_add(block.output.capacity())
            .saturating_add(block.cwd.as_ref().map_or(0, String::capacity));
        let expected = std::mem::size_of::<BlockData>()
            .saturating_mul(3)
            .saturating_add(std::mem::size_of::<usize>().saturating_mul(2))
            .saturating_add(string_capacities);

        assert_eq!(estimated_loaded_block_owned_bytes(&block), expected);
    }

    #[test]
    fn failed_legacy_unlink_retains_exact_retry_authority() {
        let revision = HistoryRevision::Present {
            device: 1,
            inode: 2,
            len: 3,
            modified_seconds: 4,
            modified_nanoseconds: 5,
        };
        assert_eq!(
            super::legacy_removal_state(true, revision, false),
            (false, Some(revision))
        );
        assert_eq!(
            super::legacy_removal_state(true, revision, true),
            (true, None)
        );
        assert_eq!(
            super::legacy_removal_state(true, HistoryRevision::Missing, false),
            (true, None)
        );
    }

    #[test]
    fn applied_outcome_waits_for_pre_apply_save_lease_before_releasing_loaded_arc() {
        let shared = Arc::new(HistoryLoadShared::default());
        shared.begin();
        let lease = shared.acquire_pre_apply_save_lease().unwrap();
        let loaded = Arc::new(LoadedHistory {
            blocks: Arc::new(vec![sample_block(1, "loaded")]),
            total_loaded: 1,
            target_revision: Some(HistoryRevision::Missing),
            legacy_authority: LegacyHistoryAuthority::Ignore,
            retained_estimated_bytes: 123,
            _reservation: None,
        });
        let weak = Arc::downgrade(&loaded);
        shared.complete(&Ok(Arc::clone(&loaded)));
        drop(loaded);

        shared.mark_applied_and_consume();
        assert!(matches!(shared.outcome(), HistoryLoadOutcome::Loaded(_)));
        assert!(weak.upgrade().is_some());

        drop(lease);
        assert!(matches!(shared.outcome(), HistoryLoadOutcome::Idle));
        assert!(weak.upgrade().is_none());
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
        assert_eq!(captured.capacity(), captured.len());
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

    #[test]
    fn history_decoder_accepts_pre_lifecycle_option_status_frames() {
        let legacy = super::LegacyBlockDataV2 {
            id: 2,
            prompt: "prompt".to_string(),
            cmd: "maybe".to_string(),
            cmd_markup: None,
            output: "output".to_string(),
            exit_code: None,
            estimated_height: 32,
            line_count: 1,
            start_time_ms: Some(5),
            end_time_ms: Some(9),
            duration_ms: Some(4),
            cwd: Some("/tmp".to_string()),
            cols: 80,
        };
        let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&legacy).unwrap();
        let (decoded, _) = decode_block_record(encoded.as_slice(), false).unwrap();
        assert_eq!(decoded.cmd, "maybe");
        assert_eq!(decoded.exit_code, None);
        assert_eq!(
            decoded.completion_provenance,
            super::super::CompletionProvenanceWire::Unknown
        );
        assert!(!decoded.start_mark_seen);
    }

    #[test]
    fn pre_lifecycle_background_fields_are_normalized() {
        let legacy = super::LegacyBlockDataV2 {
            id: 6,
            prompt: "forged prompt".into(),
            cmd: String::new(),
            cmd_markup: None,
            output: "background".into(),
            exit_code: Some(9),
            estimated_height: 1,
            line_count: 1,
            start_time_ms: Some(1),
            end_time_ms: Some(2),
            duration_ms: Some(1),
            cwd: None,
            cols: 80,
        };
        let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&legacy).unwrap();
        let (decoded, _) = decode_block_record(encoded.as_slice(), false).unwrap();
        assert!(decoded.is_background());
        assert_eq!(decoded.exit_code, None);
        assert_eq!(decoded.start_time_ms, None);
        assert_eq!(decoded.end_time_ms, None);
        assert_eq!(decoded.duration_ms, None);
        assert_eq!(
            decoded.completion_provenance,
            super::super::CompletionProvenanceWire::Unknown
        );
        assert!(!decoded.start_mark_seen);
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
            assert_eq!(
                decoded.completion_provenance,
                super::super::CompletionProvenanceWire::JournalRecovered
            );
            assert!(decoded.start_mark_seen);
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
    fn pre_round8_zero_is_unknown_because_old_schema_conflated_success_and_absence() {
        let legacy = super::LegacyBlockDataV1 {
            id: 5,
            prompt: "prompt".into(),
            cmd: "maybe-success".into(),
            cmd_markup: None,
            output: "output".into(),
            exit_code: 0,
            estimated_height: 1,
            line_count: 1,
            start_time_ms: Some(1),
            end_time_ms: Some(2),
            duration_ms: Some(1),
            cwd: None,
            cols: 80,
        };
        let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&legacy).unwrap();
        let (decoded, _) = decode_block_record(encoded.as_slice(), false).unwrap();
        assert_eq!(decoded.exit_code, None);
        assert_eq!(
            decoded.completion_provenance,
            super::super::CompletionProvenanceWire::Unknown
        );
        assert!(!decoded.start_mark_seen);
        assert_eq!(decoded.start_time_ms, None);
        assert_eq!(decoded.end_time_ms, None);
        assert_eq!(decoded.duration_ms, None);
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
    fn incremental_loaded_record_retention_honors_exact_over_and_tiny_budgets() {
        let first = sample_block(1, "first");
        let second = sample_block(2, "second");
        let first_cost = estimated_loaded_block_owned_bytes(&first);
        let second_cost = estimated_loaded_block_owned_bytes(&second);
        let exact = first_cost.checked_add(second_cost).unwrap();

        let mut blocks = VecDeque::new();
        let mut costs = VecDeque::new();
        let mut retained = 0;
        assert_eq!(
            push_loaded_record_bounded(
                &mut blocks,
                &mut costs,
                first.clone(),
                10,
                exact,
                &mut retained,
            ),
            0
        );
        assert_eq!(
            push_loaded_record_bounded(
                &mut blocks,
                &mut costs,
                second.clone(),
                10,
                exact,
                &mut retained,
            ),
            0
        );
        assert_eq!(retained, exact);

        let mut blocks = VecDeque::new();
        let mut costs = VecDeque::new();
        let mut retained = 0;
        push_loaded_record_bounded(&mut blocks, &mut costs, first, 10, exact - 1, &mut retained);
        assert_eq!(
            push_loaded_record_bounded(
                &mut blocks,
                &mut costs,
                second.clone(),
                10,
                exact - 1,
                &mut retained,
            ),
            1
        );
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].cmd, "second");
        assert_eq!(retained, second_cost);

        let mut blocks = VecDeque::new();
        let mut costs = VecDeque::new();
        let mut retained = 0;
        assert_eq!(
            push_loaded_record_bounded(
                &mut blocks,
                &mut costs,
                second,
                10,
                second_cost - 1,
                &mut retained,
            ),
            1
        );
        assert!(blocks.is_empty());
        assert_eq!(retained, 0);

        let mut blocks = VecDeque::new();
        let mut costs = VecDeque::new();
        let mut retained = 0;
        assert_eq!(
            push_loaded_record_bounded(
                &mut blocks,
                &mut costs,
                sample_block(3, "count-old"),
                1,
                usize::MAX,
                &mut retained,
            ),
            0
        );
        assert_eq!(
            push_loaded_record_bounded(
                &mut blocks,
                &mut costs,
                sample_block(4, "count-new"),
                1,
                usize::MAX,
                &mut retained,
            ),
            1,
            "count truncation must revoke deletion authority just like byte truncation"
        );
        assert_eq!(blocks[0].cmd, "count-new");

        let mut zero_retained = 0;
        assert_eq!(
            push_loaded_record_bounded(
                &mut VecDeque::new(),
                &mut VecDeque::new(),
                sample_block(5, "zero-count"),
                0,
                usize::MAX,
                &mut zero_retained,
            ),
            1
        );

        let mut blocks = VecDeque::new();
        let mut costs = VecDeque::new();
        let mut retained = 0;
        for id in 10..13 {
            assert_eq!(
                push_loaded_record_bounded(
                    &mut blocks,
                    &mut costs,
                    sample_block(id, "existing"),
                    3,
                    usize::MAX,
                    &mut retained,
                ),
                0
            );
        }
        assert_eq!(
            push_loaded_record_bounded(
                &mut blocks,
                &mut costs,
                sample_block(13, "cannot-fit"),
                3,
                0,
                &mut retained,
            ),
            4,
            "one count eviction plus two remaining records and the rejected incoming record are four distinct omissions"
        );
        assert!(blocks.is_empty());
        assert_eq!(retained, 0);
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
    fn runtime_block_history_requires_a_normalized_absolute_path() {
        assert_eq!(
            absolute_history_path("/tmp/forge-block-history.bin").unwrap(),
            PathBuf::from("/tmp/forge-block-history.bin")
        );
        assert!(absolute_history_path("forge-block-history.bin").is_err());
        assert!(absolute_history_path("~/forge-block-history.bin").is_err());
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
    fn saving_one_session_never_prunes_an_old_sibling() {
        let dir = TestDir::new("no-runtime-sibling-prune");
        let base = dir.path().join("blocks.bin");
        let session_a = per_session_history_path(&base, "sid-a");
        let session_b = per_session_history_path(&base, "sid-b");
        write_history_snapshot(
            &base,
            &session_b,
            Some("sid-b"),
            &[sample_block(1, "session-b")],
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();
        let old = SystemTime::now()
            .checked_sub(Duration::from_secs(31 * 24 * 3600))
            .unwrap();
        fs::File::options()
            .write(true)
            .open(&session_b)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();

        write_history_snapshot(
            &base,
            &session_a,
            Some("sid-a"),
            &[sample_block(2, "session-a")],
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();

        let restored =
            read_history_records(&session_b, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(restored.blocks.len(), 1);
        assert_eq!(restored.blocks[0].cmd, "session-b");
    }

    #[test]
    fn compressed_record_decode_enforces_output_limit() {
        let compressed = zstd::encode_all(&b"0123456789abcdef"[..], 1).unwrap();

        let error = decode_zstd_bounded(&compressed, 8).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn compressed_record_decode_rejects_an_oversized_window() {
        let mut encoder = zstd::Encoder::new(Vec::new(), 1).unwrap();
        encoder.window_log(25).unwrap();
        encoder.include_contentsize(false).unwrap();
        encoder.write_all(b"small payload").unwrap();
        let compressed = encoder.finish().unwrap();

        assert!(
            decode_zstd_bounded(&compressed, 1024).is_err(),
            "a frame advertising a 32 MiB window must exceed the 16 MiB decoder cap"
        );
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
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(fs::read(&history).unwrap(), original);

        let clear_error = write_history_snapshot_with_intent(
            &history,
            &history,
            None,
            &[],
            false,
            HistoryWriteIntent::ExplicitReplace,
        )
        .unwrap_err();
        assert_eq!(clear_error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&history).unwrap(), original);
    }

    #[test]
    fn stale_saves_fail_closed_without_resurrecting_cleared_history() {
        let dir = TestDir::new("stale-fail-closed");
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
        let first_outcome = write_history_snapshot(
            &history,
            &history,
            None,
            &first,
            false,
            Some(initial.revision),
        )
        .unwrap();
        // This pane still believes the initial revision is current. Refusing
        // the whole write is the only safe choice without a persisted common
        // Clear generation: a union could revive explicitly removed records.
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
        .unwrap_err();
        assert_eq!(stale.kind(), io::ErrorKind::WouldBlock);

        let loaded =
            read_history_records(&history, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(
            loaded
                .blocks
                .iter()
                .map(|block| block.cmd.as_str())
                .collect::<Vec<_>>(),
            ["baseline", "first-writer"]
        );

        let cleared = write_history_snapshot_with_intent(
            &history,
            &history,
            None,
            &[],
            false,
            HistoryWriteIntent::ExplicitReplace,
        )
        .unwrap();
        assert_eq!(fs::metadata(&history).unwrap().len(), 0);

        let stale_after_clear = write_history_snapshot(
            &history,
            &history,
            None,
            &second,
            false,
            Some(first_outcome.revision),
        )
        .unwrap_err();
        assert_eq!(stale_after_clear.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(fs::metadata(&history).unwrap().len(), 0);

        let new_after_clear = vec![sample_block(4, "new-after-clear")];
        write_history_snapshot(
            &history,
            &history,
            None,
            &new_after_clear,
            false,
            Some(cleared.revision),
        )
        .unwrap();
        let stale_after_new = write_history_snapshot(
            &history,
            &history,
            None,
            &second,
            false,
            Some(first_outcome.revision),
        )
        .unwrap_err();
        assert_eq!(stale_after_new.kind(), io::ErrorKind::WouldBlock);
        let reloaded =
            read_history_records(&history, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(reloaded.blocks.len(), 1);
        assert_eq!(reloaded.blocks[0].cmd, "new-after-clear");
    }

    #[test]
    fn pressure_truncated_legacy_load_cannot_delete_unloaded_disk_records() {
        let dir = TestDir::new("pressure-merge-only");
        let base = dir.path().join("history.bin");
        let session_id = "sid-pressure";
        let session = per_session_history_path(&base, session_id);
        let old = vec![
            sample_block(1, "oldest"),
            sample_block(2, "middle"),
            sample_block(3, "newest-on-disk"),
        ];
        write_history_snapshot(
            &base,
            &base,
            None,
            &old,
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();

        // Models a worker whose transient decoder permit succeeded while
        // queued/running work left no retained-result bytes available.
        let loaded =
            read_history_snapshot_with_retained_budget(&base, Some(session_id), false, 200, 0)
                .unwrap();
        assert!(loaded.blocks.is_empty());
        assert_eq!(loaded.total_loaded, old.len());
        assert_eq!(loaded.target_revision, None);

        assert_eq!(loaded.legacy_authority, LegacyHistoryAuthority::MergeOnly);
        let new = sample_block(4, "new-live-command");
        let error = write_history_snapshot_with_intent(
            &base,
            &session,
            Some(session_id),
            std::slice::from_ref(&new),
            false,
            HistoryWriteIntent::Revision {
                target: loaded.target_revision,
                legacy: loaded.legacy_authority,
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);

        assert!(!session.exists());
        let reloaded =
            read_history_records(&base, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(
            reloaded
                .blocks
                .iter()
                .map(|block| block.cmd.as_str())
                .collect::<Vec<_>>(),
            ["oldest", "middle", "newest-on-disk"]
        );
        assert!(
            base.exists(),
            "an incompletely loaded legacy source must remain untouched"
        );
    }

    #[test]
    fn complete_legacy_migration_preserves_ui_deletions_and_removes_the_source() {
        let dir = TestDir::new("complete-legacy-migration");
        let base = dir.path().join("history.bin");
        let session_id = "sid-migrate";
        let session = per_session_history_path(&base, session_id);
        let original = vec![sample_block(1, "keep"), sample_block(2, "delete")];
        let initial = write_history_snapshot(
            &base,
            &base,
            None,
            &original,
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();

        let loaded = read_history_snapshot_with_retained_budget(
            &base,
            Some(session_id),
            false,
            200,
            super::MAX_COMPLETED_BLOCK_RETAINED_BYTES,
        )
        .unwrap();
        assert_eq!(loaded.target_revision, Some(HistoryRevision::Missing));
        assert_eq!(
            loaded.legacy_authority,
            LegacyHistoryAuthority::Revision(initial.revision)
        );
        let retained = loaded
            .blocks
            .iter()
            .filter(|block| block.cmd != "delete")
            .cloned()
            .collect::<Vec<_>>();

        let migrated = write_history_snapshot_with_intent(
            &base,
            &session,
            Some(session_id),
            &retained,
            false,
            HistoryWriteIntent::Revision {
                target: loaded.target_revision,
                legacy: loaded.legacy_authority,
            },
        )
        .unwrap();
        assert!(migrated.authoritative);
        assert!(migrated.legacy_handled);
        assert!(!base.exists());
        let restored =
            read_history_records(&session, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(restored.blocks.len(), 1);
        assert_eq!(restored.blocks[0].cmd, "keep");
    }

    #[test]
    fn changed_legacy_source_rejects_migration_without_swallowing_new_data() {
        let dir = TestDir::new("changed-legacy-migration");
        let base = dir.path().join("history.bin");
        let session_id = "sid-changed";
        let session = per_session_history_path(&base, session_id);
        let original = vec![sample_block(1, "keep"), sample_block(2, "delete")];
        let initial = write_history_snapshot(
            &base,
            &base,
            None,
            &original,
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();
        let loaded = read_history_snapshot_with_retained_budget(
            &base,
            Some(session_id),
            false,
            200,
            super::MAX_COMPLETED_BLOCK_RETAINED_BYTES,
        )
        .unwrap();

        let mut concurrent = original.clone();
        concurrent.push(sample_block(3, "concurrent"));
        write_history_snapshot(
            &base,
            &base,
            None,
            &concurrent,
            false,
            Some(initial.revision),
        )
        .unwrap();

        let retained = loaded
            .blocks
            .iter()
            .filter(|block| block.cmd != "delete")
            .cloned()
            .collect::<Vec<_>>();
        let error = write_history_snapshot_with_intent(
            &base,
            &session,
            Some(session_id),
            &retained,
            false,
            HistoryWriteIntent::Revision {
                target: loaded.target_revision,
                legacy: loaded.legacy_authority,
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(!session.exists());
        let restored =
            read_history_records(&base, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(
            restored
                .blocks
                .iter()
                .map(|block| block.cmd.as_str())
                .collect::<Vec<_>>(),
            ["keep", "delete", "concurrent"]
        );
    }

    #[test]
    fn two_sessions_can_migrate_the_same_consumed_legacy_revision() {
        let dir = TestDir::new("parallel-legacy-migration");
        let base = dir.path().join("history.bin");
        let session_a = per_session_history_path(&base, "sid-a");
        let session_b = per_session_history_path(&base, "sid-b");
        let original = vec![sample_block(1, "legacy")];
        write_history_snapshot(
            &base,
            &base,
            None,
            &original,
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();
        let loaded_a = read_history_snapshot_with_retained_budget(
            &base,
            Some("sid-a"),
            false,
            200,
            super::MAX_COMPLETED_BLOCK_RETAINED_BYTES,
        )
        .unwrap();
        let loaded_b = read_history_snapshot_with_retained_budget(
            &base,
            Some("sid-b"),
            false,
            200,
            super::MAX_COMPLETED_BLOCK_RETAINED_BYTES,
        )
        .unwrap();

        for (session_id, session, loaded) in [
            ("sid-a", &session_a, &loaded_a),
            ("sid-b", &session_b, &loaded_b),
        ] {
            write_history_snapshot_with_intent(
                &base,
                session,
                Some(session_id),
                loaded.blocks.as_ref(),
                false,
                HistoryWriteIntent::Revision {
                    target: loaded.target_revision,
                    legacy: loaded.legacy_authority,
                },
            )
            .unwrap();
        }

        assert!(!base.exists());
        for session in [&session_a, &session_b] {
            let restored =
                read_history_records(session, false, usize::MAX, UndecodablePolicy::Reject)
                    .unwrap();
            assert_eq!(restored.blocks.len(), 1);
            assert_eq!(restored.blocks[0].cmd, "legacy");
        }
    }

    #[test]
    fn authoritative_save_preserves_identical_event_multiplicity() {
        let dir = TestDir::new("duplicate-events");
        let history = dir.path().join("history.bin");
        let event = sample_block(7, "same");
        write_history_snapshot(
            &history,
            &history,
            None,
            &[event.clone(), event],
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();

        let restored =
            read_history_records(&history, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(restored.blocks.len(), 2);
        assert!(restored.blocks.iter().all(|block| block.cmd == "same"));
        assert!(restored.blocks.iter().all(|block| block.id == 7));
    }

    #[test]
    fn pre_apply_save_streams_loaded_prefix_before_live_snapshot() {
        let dir = TestDir::new("stream-prefix-live");
        let history = dir.path().join("history.bin");
        let prefix = vec![sample_block(1, "loaded-old"), sample_block(2, "loaded-new")];
        let live = vec![sample_block(3, "live")];
        write_history_snapshot_with_intent_parts(
            &history,
            &history,
            None,
            &prefix,
            &live,
            false,
            HistoryWriteIntent::Revision {
                target: Some(HistoryRevision::Missing),
                legacy: LegacyHistoryAuthority::Ignore,
            },
        )
        .unwrap();

        let restored =
            read_history_records(&history, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert_eq!(
            restored
                .blocks
                .iter()
                .map(|block| block.cmd.as_str())
                .collect::<Vec<_>>(),
            ["loaded-old", "loaded-new", "live"]
        );
    }

    #[test]
    fn reserved_load_keeps_a_valid_large_newest_record_for_ui_retention() {
        let dir = TestDir::new("large-result-owner-cost");
        let history = dir.path().join("history.bin");
        let mut block = sample_block(8, "large-output");
        block.output = "x".repeat(super::MAX_HISTORY_OUTPUT_BYTES);
        let ui_cost = block.estimated_restored_retained_bytes();
        let owner_cost = estimated_loaded_block_owned_bytes(&block);
        assert!(
            ui_cost > owner_cost,
            "future bounded VTE/widget cost must stay separate from BlockData ownership"
        );
        assert!(ui_cost <= super::MAX_COMPLETED_BLOCK_RETAINED_BYTES);
        assert!(owner_cost < super::MAX_COMPLETED_BLOCK_RETAINED_BYTES);
        write_history_snapshot(
            &history,
            &history,
            None,
            std::slice::from_ref(&block),
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();

        let loaded = read_history_snapshot_reserved(&history, None, false, 200).unwrap();
        assert_eq!(loaded.blocks.len(), 1);
        assert_eq!(
            loaded.blocks[0].output.len(),
            super::MAX_HISTORY_OUTPUT_BYTES
        );
        assert!(loaded.retained_estimated_bytes <= super::MAX_COMPLETED_BLOCK_RETAINED_BYTES);
    }

    #[test]
    fn explicit_clear_replaces_history_after_a_low_budget_load() {
        let dir = TestDir::new("pressure-explicit-clear");
        let history = dir.path().join("history.bin");
        let old = vec![sample_block(1, "oldest"), sample_block(2, "newest")];
        write_history_snapshot(
            &history,
            &history,
            None,
            &old,
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();

        let loaded =
            read_history_snapshot_with_retained_budget(&history, None, false, 200, 0).unwrap();
        assert!(loaded.blocks.is_empty());
        assert_eq!(loaded.target_revision, None);

        let cleared = write_history_snapshot_with_intent(
            &history,
            &history,
            None,
            &[],
            false,
            HistoryWriteIntent::ExplicitReplace,
        )
        .unwrap();
        assert!(cleared.authoritative);
        let reloaded =
            read_history_records(&history, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert!(reloaded.blocks.is_empty());
    }

    #[test]
    fn failed_pending_load_cannot_erase_explicit_clear_intent() {
        let dir = TestDir::new("failed-load-explicit-clear");
        let history = dir.path().join("history.bin");
        write_history_snapshot(
            &history,
            &history,
            None,
            &[sample_block(1, "old")],
            false,
            Some(HistoryRevision::Missing),
        )
        .unwrap();

        let shared = HistoryLoadShared::default();
        shared.begin();
        shared.discard_for_explicit_clear();
        let epoch = shared.pending_explicit_replace_epoch().unwrap();
        let failed: io::Result<Arc<LoadedHistory>> = Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "decoder permit unavailable",
        ));
        shared.complete(&failed);
        assert_eq!(shared.pending_explicit_replace_epoch(), Some(epoch));

        write_history_snapshot_with_intent(
            &history,
            &history,
            None,
            &[],
            false,
            HistoryWriteIntent::ExplicitReplace,
        )
        .unwrap();
        shared.mark_explicit_replace_persisted(epoch);
        assert_eq!(shared.pending_explicit_replace_epoch(), None);
        let reloaded =
            read_history_records(&history, false, usize::MAX, UndecodablePolicy::Reject).unwrap();
        assert!(reloaded.blocks.is_empty());
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
