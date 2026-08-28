use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Label, Notebook, Paned};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use vte4::Terminal;
use vte4::TerminalExt;

use crate::persistence::{self, PersistenceKey};
use crate::process::deserialize_restorable_argv_bounded;
use jterm_core::snapshot_file;

use crate::terminal::{
    find_first_terminal, terminal_child_lifecycle, terminal_child_pid, terminal_working_directory,
    TERMINAL_ESCALATION,
};
use crate::ui::{PaneLeaf, PaneNode};

const MAX_READY_WINDOW_STATES: usize = 32;
/// Quarantined corrupt snapshots kept for manual recovery. Without a bound the
/// windows/ prune predicate would never match `*.corrupt-*` names and a
/// snapshot that corrupts on every launch would grow the directory forever —
/// trading the old data loss for unbounded disk use.
const MAX_QUARANTINED_SNAPSHOTS: usize = 8;
const MAX_WORKSPACE_STATE_BYTES: usize = 20 * 1024 * 1024;
const MAX_AI_METADATA_RESERVE_BYTES: usize = 64 * 1024;
const MAX_WINDOW_STATE_BYTES: usize = MAX_WORKSPACE_STATE_BYTES + MAX_AI_METADATA_RESERVE_BYTES;
const READY_STATE_EXTENSION: &str = "state";
const ACTIVE_STATE_EXTENSION: &str = "active";
const AI_CONVERSATION_PREFIX: &str = "ai_conversation=";
const MAX_AI_CONVERSATION_LINE_BYTES: usize = crate::ai::MAX_CONVERSATION_SNAPSHOT_JSON_BYTES * 2;
// The preservation worker can simultaneously own the bounded input state,
// cloned non-AI lines, and replacement payload. AI compaction additionally
// keeps the captured/working/original/emptied/candidate snapshots plus JSON,
// escaped, and final line encodings. Reserve those worst-case owners rather
// than admitting this multi-MiB job through the legacy zero-weight API.
const PRESERVED_WORKSPACE_BUFFER_OWNERS: usize = 3;
const PRESERVED_AI_JSON_EQUIVALENT_OWNERS: usize = 10;
const MAX_RESTORED_TABS: usize = 32;
const MAX_RESTORED_PANES_PER_TAB: usize = 16;
const MAX_RESTORED_PANES_TOTAL: usize = 64;
const MAX_RESTORED_TAB_NAME_BYTES: usize = 4 * 1024;
const MAX_RESTORED_CWD_BYTES: usize = 4 * 1024;
/// A private state directory normally contains only the retained snapshots.
/// Bound even corrupted/same-user namespaces so startup and shutdown cannot
/// allocate or stat an attacker-controlled number of entries.
const MAX_SCANNED_STATE_DIRECTORY_ENTRIES: usize = 4_096;

#[derive(Debug)]
struct WindowStatePaths {
    directory: PathBuf,
    active: PathBuf,
    ready: PathBuf,
}

static WINDOW_STATE_PATHS: OnceLock<WindowStatePaths> = OnceLock::new();
static WINDOW_STATE_FINALIZED: AtomicBool = AtomicBool::new(false);
static WINDOW_STATE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
struct AiConversationState {
    generation: u64,
    snapshot: Option<crate::ai::ConversationSnapshot>,
}

static AI_CONVERSATION_SNAPSHOT: OnceLock<Mutex<AiConversationState>> = OnceLock::new();

fn ai_conversation_slot() -> &'static Mutex<AiConversationState> {
    AI_CONVERSATION_SNAPSHOT.get_or_init(|| {
        Mutex::new(AiConversationState {
            generation: 0,
            snapshot: None,
        })
    })
}

/// Return the complete, bounded AI conversation associated with this process's
/// active window snapshot.
pub(crate) fn get_ai_conversation_snapshot() -> Option<crate::ai::ConversationSnapshot> {
    ai_conversation_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .snapshot
        .clone()
}

/// Replace the AI conversation that the next window-state save will embed.
/// Passing `None` durably removes the entire chat library from the snapshot.
pub(crate) fn set_ai_conversation_snapshot(snapshot: Option<crate::ai::ConversationSnapshot>) {
    let mut state = ai_conversation_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.generation = state.generation.wrapping_add(1);
    state.snapshot = snapshot;
}

fn versioned_ai_conversation_snapshot() -> (u64, Option<crate::ai::ConversationSnapshot>) {
    let state = ai_conversation_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (state.generation, state.snapshot.clone())
}

/// A slow older write must never replace newer in-memory chat state. Apply the
/// compacted form only while the UI generation captured by that write is still
/// current; a later autosave owns any subsequent durable update.
fn commit_ai_conversation_snapshot(
    generation: u64,
    snapshot: Option<crate::ai::ConversationSnapshot>,
) {
    let mut state = ai_conversation_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.generation == generation {
        state.snapshot = snapshot;
    }
}

fn ensure_private_directory(path: &Path) -> io::Result<File> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    builder.create(path)?;

    // Keep the descriptor across validation and chmod. A path-based chmod
    // would follow a final symlink if the directory entry were substituted
    // between the metadata check and the permission change.
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "window-state parent is not a directory",
        ));
    }
    // SAFETY: geteuid has no preconditions and only reads process state.
    if metadata.uid() != unsafe { nix::libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "window-state parent is not owned by the current user",
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

fn make_file_private(path: &Path) -> io::Result<()> {
    let file = open_private_regular_file(path)?;
    if file.metadata()?.permissions().mode() & 0o077 != 0 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Open a private persistence file without following a final symlink or ever
/// blocking on a FIFO, then validate the opened inode rather than the path.
fn open_private_regular_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "window snapshot path is not a regular file",
        ));
    }
    if metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "window snapshot must have exactly one hard link",
        ));
    }
    // SAFETY: geteuid has no preconditions and only reads process state.
    if metadata.uid() != unsafe { nix::libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "window snapshot is not owned by the current user",
        ));
    }
    Ok(file)
}

/// Durably replace a private state file without ever truncating the last good
/// snapshot. The shared implementation writes a synced sibling temporary (so the
/// rename cannot cross filesystems), keeps the file `0600`, and syncs the
/// directory entry. This app owns its `windows/` parent, so it explicitly
/// validates and tightens that directory before handing the payload off.
fn atomic_write_private_file(target: &Path, payload: &[u8]) -> io::Result<()> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = ensure_private_directory(parent)?;
    atomic_write_private_file_in_directory(&directory, target, payload)
}

fn atomic_write_private_file_in_directory(
    directory: &File,
    target: &Path,
    payload: &[u8],
) -> io::Result<()> {
    let target_name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "window snapshot path has no file name",
        )
    })?;
    let target_name_c = CString::new(target_name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "window snapshot name contains NUL",
        )
    })?;

    for _ in 0..128 {
        let sequence = WINDOW_STATE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(target_name);
        temp_name.push(format!(".tmp.{}.{sequence}", std::process::id()));
        let temp_name_c = CString::new(temp_name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "window snapshot temporary name contains NUL",
            )
        })?;

        // Operate relative to the retained validated directory descriptor.
        // Swapping the parent pathname after validation cannot redirect the
        // create, rename, cleanup, or durability sync to another namespace.
        // SAFETY: both C strings are live and the directory descriptor remains
        // open through every *at operation.
        let fd = unsafe {
            nix::libc::openat(
                directory.as_raw_fd(),
                temp_name_c.as_ptr(),
                nix::libc::O_WRONLY
                    | nix::libc::O_CREAT
                    | nix::libc::O_EXCL
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(error);
        }
        // SAFETY: openat returned a new owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let write_result =
            std::io::Write::write_all(&mut file, payload).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = write_result {
            // SAFETY: arguments remain valid; unlinkat retains no pointer.
            unsafe {
                nix::libc::unlinkat(directory.as_raw_fd(), temp_name_c.as_ptr(), 0);
            }
            return Err(error);
        }
        // SAFETY: names are relative to the same retained directory inode.
        let renamed = unsafe {
            nix::libc::renameat(
                directory.as_raw_fd(),
                temp_name_c.as_ptr(),
                directory.as_raw_fd(),
                target_name_c.as_ptr(),
            )
        };
        if renamed != 0 {
            let error = io::Error::last_os_error();
            // SAFETY: see unlinkat above.
            unsafe {
                nix::libc::unlinkat(directory.as_raw_fd(), temp_name_c.as_ptr(), 0);
            }
            return Err(error);
        }
        directory.sync_all()?;
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique window snapshot temporary file",
    ))
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(parent)?;
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and only reads process state.
    if metadata.uid() != unsafe { nix::libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "window-state parent is not owned by the current user",
        ));
    }
    directory.sync_all()
}

fn rename_noreplace_between_directories(
    source_directory: &File,
    source: &Path,
    target_directory: &File,
    target: &Path,
) -> io::Result<()> {
    let source_name = source.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot source has no file name",
        )
    })?;
    let target_name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot target has no file name",
        )
    })?;
    let source_name = CString::new(source_name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "snapshot name contains NUL"))?;
    let target_name = CString::new(target_name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "snapshot name contains NUL"))?;

    #[cfg(target_os = "linux")]
    {
        // SAFETY: both names are live and relative to the retained directory
        // descriptor; renameat2 retains no pointers.
        let result = unsafe {
            nix::libc::renameat2(
                source_directory.as_raw_fd(),
                source_name.as_ptr(),
                target_directory.as_raw_fd(),
                target_name.as_ptr(),
                nix::libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // The GTK build currently targets Unix/Linux. Keep a conservative
        // fallback for other Unix builds: refuse an existing destination, then
        // perform the descriptor-relative rename.
        if target.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "snapshot destination already exists",
            ));
        }
        // SAFETY: see the Linux branch.
        let result = unsafe {
            nix::libc::renameat(
                source_directory.as_raw_fd(),
                source_name.as_ptr(),
                target_directory.as_raw_fd(),
                target_name.as_ptr(),
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn rename_noreplace_in_directory(directory: &File, source: &Path, target: &Path) -> io::Result<()> {
    rename_noreplace_between_directories(directory, source, directory, target)
}

fn window_state_directory() -> PathBuf {
    glib::user_config_dir().join("forge").join("windows")
}

fn legacy_tabs_state_file_path() -> PathBuf {
    glib::user_config_dir().join("forge").join("tabs.state")
}

fn window_state_paths() -> &'static WindowStatePaths {
    WINDOW_STATE_PATHS.get_or_init(|| {
        let directory = window_state_directory();
        let id = generate_window_state_id();
        WindowStatePaths {
            active: directory.join(format!("window-{id}.{ACTIVE_STATE_EXTENSION}")),
            ready: directory.join(format!("window-{id}.{READY_STATE_EXTENSION}")),
            directory,
        }
    })
}

pub(crate) fn tabs_state_file_path() -> PathBuf {
    window_state_paths().active.clone()
}

/// Generate a unique session ID for jsh session persistence.
pub(crate) fn generate_session_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}", std::process::id(), ts)
}

/// New window snapshots bind their owner PID to the kernel process start tick.
/// A PID alone can be recycled while an interrupted `.active` file remains on
/// disk; the extra token lets the next window distinguish that stale owner
/// without signalling any unrelated process. Falling back to the legacy shape
/// is deliberately conservative on systems where `/proc` is unreadable.
fn generate_window_state_id() -> String {
    let pid = std::process::id();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    match process_start_ticks_result(pid as i32) {
        Ok(start_ticks) => format!("{pid}-{start_ticks}-{timestamp}"),
        Err(_) => format!("{pid}-{timestamp}"),
    }
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some(extension)
}

fn modified_time(path: &Path) -> SystemTime {
    fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(UNIX_EPOCH)
}

fn snapshots_with_extension(directory: &Path, extension: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut snapshots = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_SCANNED_STATE_DIRECTORY_ENTRIES {
            log::warn!(
                "Stopped scanning window-state directory {} after {} entries",
                directory.display(),
                MAX_SCANNED_STATE_DIRECTORY_ENTRIES
            );
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        // `file_type` does not follow a final symlink. Unsafe entries never
        // enter sorting (which would otherwise stat their targets).
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if has_extension(&path, extension) {
            snapshots.push(path);
        }
    }
    snapshots.sort_by(|left, right| {
        modified_time(right)
            .cmp(&modified_time(left))
            .then_with(|| right.cmp(left))
    });
    snapshots
}

fn ready_snapshots_in(directory: &Path) -> Vec<PathBuf> {
    snapshots_with_extension(directory, READY_STATE_EXTENSION)
}

fn snapshot_owner_pid(path: &Path) -> Option<i32> {
    path.file_stem()?
        .to_str()?
        .strip_prefix("window-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

fn snapshot_owner_start_ticks(path: &Path) -> Option<u64> {
    let mut fields = path
        .file_stem()?
        .to_str()?
        .strip_prefix("window-")?
        .split('-');
    fields.next()?.parse::<i32>().ok()?;
    let start_ticks = fields.next()?.parse().ok()?;
    // Legacy names are `pid-wallclock`; new names are
    // `pid-start_ticks-wallclock`. Extra fields are not trusted as a token.
    fields.next()?;
    fields.next().is_none().then_some(start_ticks)
}

fn parse_process_start_ticks(contents: &str) -> Option<u64> {
    // `/proc/<pid>/stat` field 2 is parenthesized `comm` and may itself contain
    // spaces or `)`, so index from its final closing paren. The remaining
    // sequence begins at field 3; starttime is field 22, hence index 19.
    let after_comm = contents.get(contents.rfind(')')? + 1..)?;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

fn process_start_ticks_result(pid: i32) -> io::Result<u64> {
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process id must be positive",
        ));
    }
    let contents = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    parse_process_start_ticks(&contents)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed proc stat"))
}

fn snapshot_owner_is_current(path: &Path) -> bool {
    let Some(pid) = snapshot_owner_pid(path) else {
        return false;
    };
    let Some(expected_start_ticks) = snapshot_owner_start_ticks(path) else {
        // Preserve the conservative behavior for every pre-token snapshot.
        return snapshot_owner_is_running(pid);
    };
    match process_start_ticks_result(pid) {
        Ok(actual_start_ticks) => actual_start_ticks == expected_start_ticks,
        Err(error) => !matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
        ),
    }
}

fn recover_stale_active_snapshots(directory: &Path) {
    let Ok(directory_file) = ensure_private_directory(directory) else {
        return;
    };
    for active in snapshots_with_extension(directory, ACTIVE_STATE_EXTENSION) {
        if snapshot_owner_is_current(&active) {
            continue;
        }
        if let Err(error) = open_private_regular_file(&active) {
            log::warn!(
                "Ignoring unsafe interrupted window snapshot {}: {error}",
                active.display()
            );
            continue;
        }
        let ready = active.with_extension(READY_STATE_EXTENSION);
        match rename_noreplace_in_directory(&directory_file, &active, &ready) {
            Ok(()) => {
                if let Err(error) = make_file_private(&ready) {
                    log::warn!(
                        "Failed to tighten snapshot permissions {}: {error}",
                        ready.display()
                    );
                }
                log::info!("Recovered interrupted window snapshot {}", ready.display());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "Failed to recover interrupted window snapshot {}: {error}",
                active.display()
            ),
        }
    }
}

fn claim_ready_snapshot_in(directory: &Path, active: &Path) -> Option<PathBuf> {
    let directory_file = ensure_private_directory(directory).ok()?;
    for candidate in ready_snapshots_in(directory) {
        if let Err(error) = open_private_regular_file(&candidate) {
            log::warn!(
                "Ignoring unsafe ready window snapshot {}: {error}",
                candidate.display()
            );
            continue;
        }
        match rename_noreplace_in_directory(&directory_file, &candidate, active) {
            Ok(()) => return Some(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => log::debug!(
                "Failed to claim window snapshot {}: {error}",
                candidate.display()
            ),
        }
    }
    None
}

fn prune_ready_snapshots_in(directory: &Path, keep: usize) {
    for stale in ready_snapshots_in(directory).into_iter().skip(keep) {
        if let Err(error) = fs::remove_file(&stale) {
            log::debug!(
                "Failed to prune old window snapshot {}: {error}",
                stale.display()
            );
        }
    }
}

/// Whether a directory entry is a snapshot [`quarantine_corrupt_snapshot`] moved
/// aside. The suffix appends `.corrupt-<millis>-<pid>-<attempt>` after the
/// original name, so the *final* extension starts with `corrupt-` — which also
/// keeps such files invisible to [`snapshots_with_extension`]'s `state`/`active`
/// scans, so a quarantined snapshot can never be restored by accident.
fn is_quarantined_snapshot(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.starts_with("corrupt-"))
}

/// Retire all but the newest `keep` quarantined snapshots. The ready-state
/// prune above matches only the `state` extension, so without this a snapshot
/// that corrupts on every launch would fill windows/ with `*.corrupt-*` files
/// nothing ever deletes.
fn prune_quarantined_snapshots_in(directory: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut quarantined = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_SCANNED_STATE_DIRECTORY_ENTRIES {
            log::warn!(
                "Stopped scanning quarantined window states in {} after {} entries",
                directory.display(),
                MAX_SCANNED_STATE_DIRECTORY_ENTRIES
            );
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if is_quarantined_snapshot(&path) {
            quarantined.push(path);
        }
    }
    quarantined.sort_by(|left, right| {
        modified_time(right)
            .cmp(&modified_time(left))
            .then_with(|| right.cmp(left))
    });
    for stale in quarantined.into_iter().skip(keep) {
        if let Err(error) = fs::remove_file(&stale) {
            log::debug!(
                "Failed to prune quarantined snapshot {}: {error}",
                stale.display()
            );
        }
    }
}

/// Move an unreadable snapshot aside so the fresh state the session is about to
/// save cannot overwrite it, and report where it went.
///
/// Ordering is the point: this window has already *claimed* the snapshot by
/// renaming a `window-*.state` onto its own `.active` name, so by the time a
/// parse fails the original file name is gone and the very first autosave (or
/// the unconditional save right after restore in `main.rs`) would destroy the
/// only copy. Quarantining before returning from the failed load is what keeps
/// the bytes recoverable.
fn quarantine_corrupt_snapshot(path: &Path) {
    match snapshot_file::quarantine_corrupt(path) {
        Ok(backup) => log::warn!(
            "Quarantined corrupt window snapshot {} as {}",
            path.display(),
            backup.display()
        ),
        Err(error) => log::warn!(
            "Failed to quarantine corrupt window snapshot {}: {error}",
            path.display()
        ),
    }
}

fn prepare_active_tabs_state_path() -> PathBuf {
    let paths = window_state_paths();
    let directory_file = match ensure_private_directory(&paths.directory) {
        Ok(directory) => directory,
        Err(error) => {
            log::warn!(
                "Failed to create window-state directory {}: {error}",
                paths.directory.display()
            );
            return paths.active.clone();
        }
    };

    recover_stale_active_snapshots(&paths.directory);
    if paths.active.exists() {
        return paths.active.clone();
    }

    // Upgrade the old single-file format first. Atomic rename means concurrent
    // launches cannot restore the same legacy snapshot.
    let legacy = legacy_tabs_state_file_path();
    if let Ok(legacy_file) = open_private_regular_file(&legacy) {
        let legacy_parent = legacy.parent().unwrap_or_else(|| Path::new("."));
        match ensure_private_directory(legacy_parent).and_then(|legacy_directory| {
            let expected = legacy_file.metadata()?;
            rename_noreplace_between_directories(
                &legacy_directory,
                &legacy,
                &directory_file,
                &paths.active,
            )?;
            let claimed = open_private_regular_file(&paths.active)?;
            let actual = claimed.metadata()?;
            if expected.dev() != actual.dev() || expected.ino() != actual.ino() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "legacy snapshot entry changed while it was being claimed",
                ));
            }
            legacy_directory.sync_all()?;
            directory_file.sync_all()
        }) {
            Ok(()) => {
                if let Err(error) = make_file_private(&paths.active) {
                    log::warn!(
                        "Failed to tighten legacy snapshot permissions {}: {error}",
                        paths.active.display()
                    );
                }
                log::info!("Claimed legacy tabs snapshot {}", legacy.display());
                return paths.active.clone();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "Failed to claim legacy tabs snapshot {}: {error}",
                legacy.display()
            ),
        }
    } else if fs::symlink_metadata(&legacy).is_ok() {
        log::warn!("Ignoring unsafe legacy tabs snapshot {}", legacy.display());
    }

    if let Some(claimed) = claim_ready_snapshot_in(&paths.directory, &paths.active) {
        if let Err(error) = make_file_private(&paths.active) {
            log::warn!(
                "Failed to tighten claimed snapshot permissions {}: {error}",
                paths.active.display()
            );
        }
        log::info!("Claimed window snapshot {}", claimed.display());
    }
    prune_ready_snapshots_in(&paths.directory, MAX_READY_WINDOW_STATES);
    prune_quarantined_snapshots_in(&paths.directory, MAX_QUARANTINED_SNAPSHOTS);
    paths.active.clone()
}

/// Report saved and currently active window snapshots without exposing paths.
pub(crate) fn session_snapshot_counts() -> (usize, usize) {
    let directory = window_state_directory();
    (
        ready_snapshots_in(&directory).len(),
        snapshots_with_extension(&directory, ACTIVE_STATE_EXTENSION).len(),
    )
}

/// Whether any pane in this tab's split tree hosts a native agent task
/// terminal (the codex CLI or a validation rerun). The walk mirrors
/// [`serialize_pane_layout`]: `Paned` nodes recurse, anything else is a leaf
/// whose `PaneLeaf` marker decides.
fn widget_hosts_task_terminal(widget: &gtk4::Widget) -> bool {
    if let Some(paned) = widget.downcast_ref::<Paned>() {
        return paned
            .start_child()
            .is_some_and(|child| widget_hosts_task_terminal(&child))
            || paned
                .end_child()
                .is_some_and(|child| widget_hosts_task_terminal(&child));
    }
    PaneLeaf::from_widget(widget).is_some_and(|leaf| leaf.task_role().is_some())
}

/// Select which tabs survive snapshot capture when some hold task terminals.
///
/// Task terminals (native agent / validation reruns) exist only at runtime:
/// their task metadata is never persisted, and a restored pane would be an
/// ordinary shell that happens to land inside the task worktree — an easy
/// footgun. The save side therefore drops every tab containing a task pane as
/// a whole, exactly like ember's `sessions_snapshot_for_persistence`, instead
/// of resurrecting it. Returns the kept tab indices in original order plus
/// the active index remapped into the kept list (falling back to the first
/// survivor, and to 0 when nothing survives, which restores as the explicit
/// empty tombstone).
pub(crate) fn pruned_snapshot_tabs(tab_is_task: &[bool], active: usize) -> (Vec<usize>, usize) {
    let kept: Vec<usize> = tab_is_task
        .iter()
        .enumerate()
        .filter_map(|(index, is_task)| (!*is_task).then_some(index))
        .collect();
    let remapped = kept.iter().position(|&index| index == active).unwrap_or(0);
    (kept, remapped)
}

/// Pane layout structure for serialization
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PaneLayout {
    Leaf {
        dir: String,
        sid: String,
        /// True when `dir` belongs to an ssh/docker namespace and must never be
        /// supplied as a local process working directory during restore.
        #[serde(default)]
        cwd_external: bool,
        /// Stable name of a managed remote profile. The mutable connection argv
        /// is deliberately omitted and rebuilt from current validated config.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_name: Option<String>,
        /// Explicit tab-title ownership. Older snapshots omit this and retain
        /// the historical inference behavior during restore.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        custom_title: Option<bool>,
        /// Hide title details in tab chrome while retaining the real title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        private_title: Option<bool>,
        /// Restorable command argv to replay on restore (e.g. `["ssh", "host"]`).
        /// Keeping it structured prevents shell metacharacters inside one
        /// argument from becoming a different local command after a restart.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_restorable_argv_bounded"
        )]
        cmds: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pinned: Option<bool>,
    },
    Split {
        orientation: char, // 'h' or 'v'
        position: i32,
        start: Box<PaneLayout>,
        end: Box<PaneLayout>,
    },
}

/// Serialize a pane layout tree from a GTK widget
pub(crate) fn serialize_pane_layout(
    widget: &gtk4::Widget,
    session_ids: &HashMap<u32, String>,
) -> PaneLayout {
    let custom_title = crate::ui::tab_custom_title_cell(widget).map(|flag| flag.get());
    let private_title = crate::ui::tab_private_title_cell(widget).map(|flag| flag.get());
    serialize_pane_layout_with_tab_state(widget, session_ids, custom_title, private_title)
}

fn serialize_pane_layout_with_tab_state(
    widget: &gtk4::Widget,
    session_ids: &HashMap<u32, String>,
    custom_title: Option<bool>,
    private_title: Option<bool>,
) -> PaneLayout {
    if let Some(paned) = widget.downcast_ref::<Paned>() {
        let orientation = match paned.orientation() {
            gtk4::Orientation::Horizontal => 'h',
            gtk4::Orientation::Vertical => 'v',
            _ => 'h',
        };

        let start = paned.start_child().expect("Paned must have start child");
        let end = paned.end_child().expect("Paned must have end child");

        PaneLayout::Split {
            orientation,
            position: paned.position(),
            start: Box::new(serialize_pane_layout_with_tab_state(
                &start,
                session_ids,
                custom_title,
                private_title,
            )),
            end: Box::new(serialize_pane_layout_with_tab_state(
                &end,
                session_ids,
                custom_title,
                private_title,
            )),
        }
    } else {
        // Leaf terminal
        let terminal = find_first_terminal(widget).expect("Leaf must contain terminal");
        let pane_leaf = PaneLeaf::from_widget(widget);
        let dir = pane_leaf
            .as_ref()
            .and_then(PaneLeaf::block_view)
            .map(|view| view.cwd())
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| terminal_working_directory(&terminal))
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".to_string()));

        // Prefer the identity attached to this exact leaf. The tab-number map
        // remains a compatibility fallback for older/top-level pages.
        let widget_name = widget.widget_name();
        let sid = pane_leaf
            .as_ref()
            .and_then(PaneLeaf::managed_remote_session_id)
            .or_else(|| pane_leaf.as_ref().and_then(PaneLeaf::session_id))
            .unwrap_or_else(|| {
                if let Some(tab_str) = widget_name.to_string().strip_prefix("tab-") {
                    if let Ok(tab_num) = tab_str.parse::<u32>() {
                        session_ids
                            .get(&tab_num)
                            .cloned()
                            .unwrap_or_else(generate_session_id)
                    } else {
                        generate_session_id()
                    }
                } else {
                    generate_session_id()
                }
            });

        let remote_name = pane_leaf.as_ref().and_then(PaneLeaf::managed_remote_name);
        let cmds = remote_name
            .is_none()
            .then(|| {
                pane_leaf
                    .as_ref()
                    .and_then(PaneLeaf::restorable_command)
                    .or_else(|| get_restorable_commands(&terminal))
                    .and_then(|argv| crate::process::match_restorable_command_bounded(&argv))
            })
            .flatten();
        let cwd_external = pane_leaf.as_ref().is_some_and(PaneLeaf::is_remote)
            || cmds
                .as_deref()
                .is_some_and(jterm_core::process::command_uses_external_cwd);

        // Check if this tab is pinned
        let pinned = unsafe { widget.data::<bool>("pinned").map(|p| *p.as_ref()) };

        PaneLayout::Leaf {
            dir,
            sid,
            cwd_external,
            remote_name,
            custom_title,
            private_title,
            cmds,
            pinned,
        }
    }
}

pub fn escape_tab_state(value: &str) -> String {
    // Optimized: single pass instead of multiple replace() calls
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn unescape_tab_state(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some('t') => {
                    out.push('\t');
                    chars.next();
                }
                Some('n') => {
                    out.push('\n');
                    chars.next();
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                _ => out.push(ch),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn ai_conversation_state_line(
    snapshot: &crate::ai::ConversationSnapshot,
) -> Result<String, crate::ai::ConversationSnapshotError> {
    let encoded = snapshot.to_json()?;
    let escaped = escape_tab_state(&encoded);
    if escaped.len() > MAX_AI_CONVERSATION_LINE_BYTES {
        return Err(crate::ai::ConversationSnapshotError::EncodedTooLarge);
    }
    Ok(format!("{AI_CONVERSATION_PREFIX}{escaped}"))
}

fn compact_ai_conversation_for_window(
    snapshot: &crate::ai::ConversationSnapshot,
    base_lines: &[String],
    max_workspace_bytes: usize,
    max_total_bytes: usize,
) -> Option<(String, crate::ai::ConversationSnapshot, bool)> {
    let base_len = window_state_payload_len(base_lines)?;
    if base_len > max_workspace_bytes {
        return None;
    }

    let line_separator = usize::from(!base_lines.is_empty());
    let mut compacted = snapshot.clone();
    compacted.compact_to_measured_limit(max_total_bytes, |candidate| {
        let line = ai_conversation_state_line(candidate).ok()?;
        base_len
            .checked_add(line_separator)?
            .checked_add(line.len())
    })?;
    let line = ai_conversation_state_line(&compacted).ok()?;
    let changed = compacted != *snapshot;
    Some((line, compacted, changed))
}

fn window_state_payload_len(lines: &[String]) -> Option<usize> {
    if lines.is_empty() {
        return Some(1);
    }
    lines.iter().try_fold(0usize, |total, line| {
        total.checked_add(line.len())?.checked_add(1)
    })
}

fn bounded_window_state_payload(lines: &[String], max_bytes: usize) -> Option<String> {
    let payload = lines.join("\n") + "\n";
    (payload.len() <= max_bytes).then_some(payload)
}

fn estimated_workspace_ai_preservation_bytes(has_ai_snapshot: bool) -> usize {
    let workspace_bytes = MAX_WINDOW_STATE_BYTES.saturating_mul(PRESERVED_WORKSPACE_BUFFER_OWNERS);
    if has_ai_snapshot {
        workspace_bytes.saturating_add(
            crate::ai::MAX_CONVERSATION_SNAPSHOT_JSON_BYTES
                .saturating_mul(PRESERVED_AI_JSON_EQUIVALENT_OWNERS),
        )
    } else {
        workspace_bytes
    }
}

/// When the current workspace itself is too large to replace, preserve the
/// previous tab/pane payload but still refresh its optional AI line. This
/// keeps New chat and newly enabled redaction durable even at the workspace
/// size boundary.
fn rewrite_existing_ai_conversation(
    path: &Path,
    snapshot: Option<&crate::ai::ConversationSnapshot>,
) -> io::Result<(bool, Option<crate::ai::ConversationSnapshot>)> {
    let contents = read_window_state_bounded(path)?;
    let mut lines = Vec::new();
    let mut had_ai_line = false;
    for line in contents.lines() {
        if line.trim().starts_with(AI_CONVERSATION_PREFIX) {
            had_ai_line = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if snapshot.is_none() && !had_ai_line {
        return Ok((false, None));
    }

    let mut compacted_ai = false;
    let mut durable_ai_snapshot = None;
    if let Some(snapshot) = snapshot {
        let (line, compacted_snapshot, compacted) = compact_ai_conversation_for_window(
            snapshot,
            &lines,
            MAX_WORKSPACE_STATE_BYTES,
            MAX_WINDOW_STATE_BYTES,
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "AI chat metadata cannot fit the preserved workspace snapshot",
            )
        })?;
        compacted_ai = compacted;
        durable_ai_snapshot = Some(compacted_snapshot);
        if compacted {
            log::warn!("Compacted AI chats to fit the preserved workspace snapshot");
        }
        let insertion = usize::from(
            lines
                .first()
                .is_some_and(|line| line.starts_with("current_page=")),
        );
        lines.insert(insertion, line);
    }

    let payload_limit = if snapshot.is_some() {
        MAX_WINDOW_STATE_BYTES
    } else {
        MAX_WORKSPACE_STATE_BYTES
    };
    let payload = bounded_window_state_payload(&lines, payload_limit).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "previous workspace snapshot cannot be safely rewritten",
        )
    })?;
    atomic_write_private_file(path, payload.as_bytes())?;
    Ok((compacted_ai, durable_ai_snapshot))
}

/// Parse the AI payload independently from tabs. Any malformed, duplicated,
/// unsupported, or oversized value is ignored without affecting tab recovery.
fn parse_ai_conversation(contents: &str) -> Option<crate::ai::ConversationSnapshot> {
    let mut parsed = None;
    let mut found = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        let Some(value) = line.strip_prefix(AI_CONVERSATION_PREFIX) else {
            continue;
        };
        if found {
            log::warn!("Ignoring duplicated AI conversation in window snapshot");
            return None;
        }
        found = true;
        if value.len() > MAX_AI_CONVERSATION_LINE_BYTES {
            log::warn!("Ignoring oversized AI conversation in window snapshot");
            return None;
        }
        let encoded = unescape_tab_state(value);
        match crate::ai::ConversationSnapshot::from_json(&encoded) {
            Ok(snapshot) => parsed = Some(snapshot),
            Err(error) => {
                log::warn!("Ignoring invalid AI conversation in window snapshot: {error}");
                return None;
            }
        }
    }
    parsed
}

fn normalize_pane_layout_bounded(layout: &mut PaneLayout, limit: usize) -> Option<usize> {
    let mut pending = vec![layout];
    let mut leaves = 0usize;
    let mut managed_remote_seen = false;
    while let Some(node) = pending.pop() {
        match node {
            PaneLayout::Leaf {
                dir,
                sid,
                cwd_external,
                remote_name,
                cmds,
                ..
            } => {
                leaves = leaves.checked_add(1)?;
                if leaves > limit {
                    return None;
                }
                if dir.is_empty()
                    || dir.len() > MAX_RESTORED_CWD_BYTES
                    || dir.chars().any(char::is_control)
                {
                    return None;
                }
                if !jterm_core::execution_journal::is_valid_jsh_session_id(sid) {
                    // Session ids are fed to the shell bootstrap and become
                    // history routing keys. Preserve the tab, but never retain
                    // an identifier outside the exact grammar those consumers
                    // share with jsh.
                    *sid = generate_session_id();
                }
                if cmds.as_ref().is_some_and(|argv| {
                    crate::process::match_restorable_command_bounded(argv).is_none()
                }) {
                    *cmds = None;
                }
                if cmds
                    .as_deref()
                    .is_some_and(jterm_core::process::command_uses_external_cwd)
                {
                    *cwd_external = true;
                }
                if let Some(name) = remote_name.as_deref() {
                    let valid = !name.trim().is_empty()
                        && name.len() <= MAX_RESTORED_TAB_NAME_BYTES
                        && !name.chars().any(char::is_control)
                        && !jterm_core::review_input::contains_visual_spoofing(name);
                    // One tab owns at most one reconnect controller. A modified
                    // snapshot must not smuggle a second managed argv into a
                    // local shell when that invariant cannot be represented.
                    *cmds = None;
                    *cwd_external = true;
                    if !valid || managed_remote_seen {
                        *remote_name = None;
                    } else {
                        managed_remote_seen = true;
                    }
                }
            }
            PaneLayout::Split {
                orientation,
                position,
                start,
                end,
            } => {
                if !matches!(*orientation, 'h' | 'v') {
                    return None;
                }
                *position = (*position).clamp(0, 1_000_000);
                pending.push(end);
                pending.push(start);
            }
        }
    }
    Some(leaves)
}

fn pane_layout_leaf_count_bounded(layout: &PaneLayout, limit: usize) -> Option<usize> {
    let mut pending = vec![layout];
    let mut leaves = 0usize;
    while let Some(node) = pending.pop() {
        match node {
            PaneLayout::Leaf { .. } => {
                leaves = leaves.checked_add(1)?;
                if leaves > limit {
                    return None;
                }
            }
            PaneLayout::Split { start, end, .. } => {
                pending.push(end);
                pending.push(start);
            }
        }
    }
    Some(leaves)
}

fn push_restored_tab_bounded(
    tabs: &mut Vec<(Option<String>, PaneLayout)>,
    total_panes: &mut usize,
    name: Option<String>,
    mut layout: PaneLayout,
) {
    if tabs.len() >= MAX_RESTORED_TABS {
        return;
    }
    let Some(panes) = normalize_pane_layout_bounded(&mut layout, MAX_RESTORED_PANES_PER_TAB) else {
        return;
    };
    if total_panes.saturating_add(panes) > MAX_RESTORED_PANES_TOTAL {
        return;
    }
    let name = name.filter(|name| {
        name.len() <= MAX_RESTORED_TAB_NAME_BYTES && !name.chars().any(char::is_control)
    });
    *total_panes += panes;
    tabs.push((name, layout));
}

pub fn parse_tabs_state(contents: &str) -> (Option<u32>, Vec<(Option<String>, PaneLayout)>) {
    let mut current_page: Option<u32> = None;
    let mut tabs: Vec<(Option<String>, PaneLayout)> = Vec::new();
    let mut total_panes = 0usize;

    for raw_line in contents.lines() {
        if tabs.len() == MAX_RESTORED_TABS {
            break;
        }
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("current_page=") {
            current_page = rest.trim().parse::<u32>().ok();
            continue;
        }
        if let Some(rest) = line.strip_prefix("tab=") {
            // Split into fields
            let fields: Vec<&str> = rest.splitn(4, '\t').collect();
            match fields.len() {
                1 => {
                    // Just dir (legacy)
                    let dir = unescape_tab_state(fields[0]);
                    let layout = PaneLayout::Leaf {
                        dir,
                        sid: generate_session_id(),
                        cwd_external: false,
                        remote_name: None,
                        custom_title: None,
                        private_title: None,
                        cmds: None,
                        pinned: None,
                    };
                    push_restored_tab_bounded(&mut tabs, &mut total_panes, None, layout);
                }
                2 => {
                    // New format: name + layout_json OR legacy: name + dir
                    let name = unescape_tab_state(fields[0]);
                    let data = unescape_tab_state(fields[1]);

                    // Try parsing as JSON first (new format)
                    if let Ok(layout) = serde_json::from_str::<PaneLayout>(&data) {
                        push_restored_tab_bounded(&mut tabs, &mut total_panes, Some(name), layout);
                    } else {
                        // Legacy: treat as directory
                        let layout = PaneLayout::Leaf {
                            dir: data,
                            sid: generate_session_id(),
                            cwd_external: false,
                            remote_name: None,
                            custom_title: None,
                            private_title: None,
                            cmds: None,
                            pinned: None,
                        };
                        push_restored_tab_bounded(&mut tabs, &mut total_panes, Some(name), layout);
                    }
                }
                3 => {
                    // Legacy: name + dir + session_id
                    let name = unescape_tab_state(fields[0]);
                    let dir = unescape_tab_state(fields[1]);
                    let sid = unescape_tab_state(fields[2]);
                    let effective_sid = if sid.is_empty() {
                        generate_session_id()
                    } else {
                        sid
                    };
                    let layout = PaneLayout::Leaf {
                        dir,
                        sid: effective_sid,
                        cwd_external: false,
                        remote_name: None,
                        custom_title: None,
                        private_title: None,
                        cmds: None,
                        pinned: None,
                    };
                    push_restored_tab_bounded(&mut tabs, &mut total_panes, Some(name), layout);
                }
                4 => {
                    // Legacy: name + dir + session_id + commands. The old
                    // command field was a joined string whose argv boundaries
                    // cannot be recovered safely, so the tab loads but the
                    // command is never replayed.
                    let name = unescape_tab_state(fields[0]);
                    let dir = unescape_tab_state(fields[1]);
                    let sid = unescape_tab_state(fields[2]);
                    if !fields[3].is_empty() {
                        log::debug!(
                            "Ignoring legacy session restore command without argv boundaries"
                        );
                    }
                    let effective_sid = if sid.is_empty() {
                        generate_session_id()
                    } else {
                        sid
                    };
                    let layout = PaneLayout::Leaf {
                        dir,
                        sid: effective_sid,
                        cwd_external: false,
                        remote_name: None,
                        custom_title: None,
                        private_title: None,
                        cmds: None,
                        pinned: None,
                    };
                    push_restored_tab_bounded(&mut tabs, &mut total_panes, Some(name), layout);
                }
                _ => {}
            }
            continue;
        }
        // Parsed separately so a damaged or future AI payload cannot create a
        // bogus legacy path tab or interfere with workspace recovery.
        if line.starts_with(AI_CONVERSATION_PREFIX) {
            continue;
        }
        // Legacy: bare path line
        let layout = PaneLayout::Leaf {
            dir: line.to_string(),
            sid: generate_session_id(),
            cwd_external: false,
            remote_name: None,
            custom_title: None,
            private_title: None,
            cmds: None,
            pinned: None,
        };
        push_restored_tab_bounded(&mut tabs, &mut total_panes, None, layout);
    }

    (current_page, tabs)
}

/// Read a window snapshot through the family's bounded loader: an oversized
/// file, a fifo or device at the path, and non-UTF-8 bytes are all rejected
/// before anything is parsed, and the fifo case cannot block the GTK thread.
fn read_window_state_bounded(path: &Path) -> io::Result<String> {
    let file = open_private_regular_file(path)?;
    let declared_len = file.metadata()?.len();
    let max_bytes = MAX_WINDOW_STATE_BYTES as u64;
    if declared_len > max_bytes {
        return Err(window_state_oversize_error(path, declared_len));
    }

    // The descriptor can grow after fstat. Read one byte past the budget so a
    // concurrent append is rejected instead of silently truncated and parsed.
    let mut bytes = Vec::with_capacity(declared_len as usize);
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(window_state_oversize_error(path, bytes.len() as u64));
    }
    String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("window snapshot {} is not valid UTF-8", path.display()),
        )
    })
}

fn window_state_oversize_error(path: &Path, actual: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!(
            "window snapshot {} is {actual} bytes, over the {MAX_WINDOW_STATE_BYTES}-byte limit",
            path.display()
        ),
    )
}

pub(crate) fn load_tabs_state() -> (Option<u32>, Vec<(Option<String>, PaneLayout)>) {
    let path = prepare_active_tabs_state_path();
    log::info!("Loading tabs state from: {}", path.display());

    let contents = match read_window_state_bounded(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            set_ai_conversation_snapshot(None);
            log::info!("No window snapshot found (first run or a new window)");
            return (None, Vec::new());
        }
        Err(error) => {
            set_ai_conversation_snapshot(None);
            log::warn!(
                "Ignoring unreadable window snapshot {}: {error}",
                path.display()
            );
            // The claim above already renamed the snapshot onto this window's
            // .active name, and main.rs saves over that path unconditionally
            // right after restore — move the bytes aside first or they are
            // unrecoverable.
            quarantine_corrupt_snapshot(&path);
            return (None, Vec::new());
        }
    };

    set_ai_conversation_snapshot(parse_ai_conversation(&contents));
    // Quarantine covers the read, not the parse: `parse_tabs_state` cannot fail.
    // Its last arm takes any non-blank line it does not recognise as a legacy
    // bare-path tab, so damaged-but-readable contents restore as a tab rather
    // than as an empty window. Every way of *losing* the snapshot goes through
    // the error arm above.
    let (current_page, tabs) = parse_tabs_state(&contents);
    log::info!("Loaded {} tabs from window snapshot", tabs.len());
    (current_page, tabs)
}

/// Publish this process's active snapshot for a future forge window. Active
/// snapshots are deliberately invisible to other running instances.
pub(crate) fn finalize_tabs_state() {
    if WINDOW_STATE_FINALIZED.swap(true, Ordering::AcqRel) {
        return;
    }

    let paths = window_state_paths();
    let active = paths.active.clone();
    let ready = paths.ready.clone();
    let directory = paths.directory.clone();
    let key = PersistenceKey::for_path("window-finalize", &active);
    if let Err(error) = persistence::enqueue(key, "Publish window session", move || {
        match open_private_regular_file(&active) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        let directory_file = ensure_private_directory(&directory)?;
        rename_noreplace_in_directory(&directory_file, &active, &ready)?;
        if let Err(error) = make_file_private(&ready) {
            log::warn!(
                "Failed to tighten published snapshot permissions {}: {error}",
                ready.display()
            );
        }
        if let Err(error) = sync_parent_directory(&ready) {
            log::debug!(
                "Failed to sync window-state directory {}: {error}",
                directory.display()
            );
        }
        prune_ready_snapshots_in(&directory, MAX_READY_WINDOW_STATES);
        prune_quarantined_snapshots_in(&directory, MAX_QUARANTINED_SNAPSHOTS);
        log::info!("Published window snapshot {}", ready.display());
        Ok(())
    }) {
        log::error!("Could not queue window snapshot publication: {error}");
    }
}

pub(crate) fn tab_label_text(notebook: &Notebook, widget: &gtk4::Widget) -> Option<String> {
    let tab_label = notebook.tab_label(widget)?;
    let tab_box = tab_label.downcast::<gtk4::Box>().ok()?;
    let first_child = tab_box.first_child()?;
    let label = first_child.downcast::<Label>().ok()?;
    Some(label.text().to_string())
}

/// Whether the window process that owns an interrupted snapshot is still
/// running.
///
/// Deliberately not a [`crate::process::ChildLifecycle`] question: this pid
/// belongs to another forge window, never to a child of this process, so
/// nothing on this path may ever signal it — not even signal 0. A `/proc` probe
/// answers the only thing snapshot recovery asks, and anything short of a
/// definitely-vanished process counts as alive so a live window's snapshot is
/// never stolen.
fn snapshot_owner_is_running(pid: i32) -> bool {
    match crate::process::process_stat_result(pid) {
        Ok(_) => true,
        Err(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
        ),
    }
}

pub(crate) fn kill_widget_child_processes(widget: &gtk4::Widget) -> bool {
    if let Some(node) = PaneNode::from_widget(widget) {
        for leaf in node.leaves() {
            leaf.kill();
        }
        return true;
    }
    false
}

/// Terminate a terminal child process and everything it dragged into its PTY
/// session before the UI tears down.
///
/// The lifecycle attached to the widget carries who reaps that child — VTE's
/// glib child watch for a conventional pane, this process for a Block pane —
/// so this path never has to guess from the widget type. Calling it a second
/// time (an explicit close followed by the drop of the same pane) is a no-op.
pub(crate) fn kill_terminal_child(terminal: &Terminal) {
    let Some(lifecycle) = terminal_child_lifecycle(terminal) else {
        return;
    };
    lifecycle.terminate(TERMINAL_ESCALATION);
}

/// Send SIGHUP to all child process groups across every terminal in the notebook.
pub(crate) fn kill_all_terminal_children(notebook: &Notebook) {
    for i in 0..notebook.n_pages() {
        if let Some(page_widget) = notebook.nth_page(Some(i)) {
            let _ = kill_widget_child_processes(&page_widget);
        }
    }
}

/// Capture every Block pane before teardown. Exit notification is dispatched
/// through the GLib loop and may not run before `Application::quit`, while the
/// pane root intentionally owns its controller through qdata. Relying on either
/// callback or `Drop` therefore loses the last commands on an ordinary close.
pub(crate) fn save_all_block_histories(notebook: &Notebook) {
    for i in 0..notebook.n_pages() {
        let Some(page_widget) = notebook.nth_page(Some(i)) else {
            continue;
        };
        let Some(node) = PaneNode::from_widget(&page_widget) else {
            continue;
        };
        for leaf in node.leaves() {
            if let PaneLeaf::Block(view) = leaf {
                if let Err(error) = view.save_history() {
                    log::warn!("could not queue Block history before shutdown: {error}");
                }
            }
        }
    }
}

/// Break each `root -> PaneLeaf -> controller -> root` qdata cycle before GTK
/// unparents the notebook pages. Temporary split/zoom reparenting must retain
/// qdata, but permanent window teardown must release it explicitly.
pub(crate) fn detach_all_pane_leaves(notebook: &Notebook) {
    for i in 0..notebook.n_pages() {
        let Some(page_widget) = notebook.nth_page(Some(i)) else {
            continue;
        };
        let Some(node) = PaneNode::from_widget(&page_widget) else {
            continue;
        };
        for leaf in node.leaves() {
            let _ = PaneLeaf::detach_from(&leaf.root_widget());
        }
    }
}

/// Conventional-VTE compatibility wrapper. Block panes must use their
/// `PaneLeaf` probe because their custom PTY is intentionally not VTE-owned.
pub(crate) fn get_restorable_commands(terminal: &Terminal) -> Option<Vec<String>> {
    let shell_pid = terminal_child_pid(terminal)?;
    let pty_fd = terminal.pty()?.fd().as_raw_fd();
    crate::process::restorable_command(pty_fd, shell_pid)
        .and_then(|argv| crate::process::match_restorable_command_bounded(&argv))
}

/// Conventional-VTE compatibility wrapper for tooltip callers.
pub(crate) fn get_foreground_process_name(terminal: &Terminal) -> Option<String> {
    let shell_pid = terminal_child_pid(terminal)?;
    let pty_fd = terminal.pty()?.fd().as_raw_fd();
    crate::process::foreground_process_name(pty_fd, shell_pid)
        .map(|name| jterm_core::review_input::safe_inline_display(&name, 256))
}

fn preserve_existing_workspace_with_ai(
    path: PathBuf,
    ai_generation: u64,
    ai_snapshot: Option<crate::ai::ConversationSnapshot>,
    reason: &str,
) {
    log::error!("Refusing to replace the window snapshot: {reason}");
    let key = PersistenceKey::for_path("window-state", &path);
    let path_for_job = path.clone();
    let estimated_bytes = estimated_workspace_ai_preservation_bytes(ai_snapshot.is_some());
    if let Err(error) = persistence::enqueue_weighted(
        key,
        "Save window session",
        estimated_bytes,
        move || {
            match rewrite_existing_ai_conversation(&path_for_job, ai_snapshot.as_ref()) {
                Ok((compacted, durable_snapshot)) => {
                    commit_ai_conversation_snapshot(ai_generation, durable_snapshot);
                    if compacted {
                        log::warn!(
                            "Preserved the previous workspace snapshot and compacted its AI conversation"
                        );
                    } else {
                        log::warn!(
                            "Preserved the previous workspace snapshot and refreshed its AI conversation"
                        );
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    log::debug!("No previous workspace snapshot exists to refresh")
                }
                Err(error) => return Err(error),
            }
            Ok(())
        },
    ) {
        log::error!("Could not queue window session preservation: {error}");
    }
}

pub(crate) fn save_tabs_state(notebook: &Notebook, session_ids: &HashMap<u32, String>) {
    if WINDOW_STATE_FINALIZED.load(Ordering::Acquire) {
        return;
    }
    let path = tabs_state_file_path();
    log::info!("Saving tabs state to: {}", path.display());

    let _home = std::env::var("HOME").ok();
    let n_pages = notebook.n_pages();
    log::info!("Saving {} tabs", n_pages);
    let (ai_generation, ai_snapshot) = versioned_ai_conversation_snapshot();
    if n_pages as usize > MAX_RESTORED_TABS {
        preserve_existing_workspace_with_ai(
            path,
            ai_generation,
            ai_snapshot,
            "the live tab count exceeds the restore budget",
        );
        return;
    }
    // Tabs containing task terminals (native agent / validation reruns) are
    // excluded as a whole: their task metadata is runtime-only, and a
    // restored pane would become an ordinary shell that happens to land
    // inside the task worktree. Interactive tabs keep their original
    // relative order and `current_page` is remapped onto the survivors.
    let tab_is_task: Vec<bool> = (0..n_pages)
        .map(|index| {
            notebook
                .nth_page(Some(index))
                .is_some_and(|widget| widget_hosts_task_terminal(&widget))
        })
        .collect();
    let active = notebook
        .current_page()
        .and_then(|page| usize::try_from(page).ok())
        .unwrap_or(0);
    let (kept_tabs, remapped_current) = pruned_snapshot_tabs(&tab_is_task, active);
    let mut lines: Vec<String> = Vec::with_capacity(kept_tabs.len() + 2);
    if !kept_tabs.is_empty() && notebook.current_page().is_some() {
        lines.push(format!("current_page={remapped_current}"));
    }
    let mut total_panes = 0usize;

    for i in kept_tabs
        .iter()
        .filter_map(|&index| u32::try_from(index).ok())
    {
        let Some(widget) = notebook.nth_page(Some(i)) else {
            continue;
        };

        let label_text = tab_label_text(notebook, &widget)
            .filter(|label| {
                label.len() <= MAX_RESTORED_TAB_NAME_BYTES && !label.chars().any(char::is_control)
            })
            .unwrap_or_else(|| format!("Terminal {}", i + 1));

        // Serialize the pane layout (supports splits)
        let mut layout = serialize_pane_layout(&widget, session_ids);
        let Some(panes) = normalize_pane_layout_bounded(&mut layout, MAX_RESTORED_PANES_PER_TAB)
        else {
            preserve_existing_workspace_with_ai(
                path,
                ai_generation,
                ai_snapshot,
                "a live pane layout contains invalid fields or exceeds the per-tab pane budget",
            );
            return;
        };
        if total_panes.saturating_add(panes) > MAX_RESTORED_PANES_TOTAL {
            preserve_existing_workspace_with_ai(
                path,
                ai_generation,
                ai_snapshot,
                "the live pane count exceeds the total restore budget",
            );
            return;
        }
        total_panes += panes;
        let layout_json = match serde_json::to_string(&layout) {
            Ok(layout) => layout,
            Err(error) => {
                preserve_existing_workspace_with_ai(
                    path,
                    ai_generation,
                    ai_snapshot,
                    &format!("a pane layout could not be serialized: {error}"),
                );
                return;
            }
        };

        let line = format!(
            "tab={}\t{}",
            escape_tab_state(&label_text),
            escape_tab_state(&layout_json)
        );
        lines.push(line);
    }

    if window_state_payload_len(&lines).is_none_or(|length| length > MAX_WORKSPACE_STATE_BYTES) {
        preserve_existing_workspace_with_ai(
            path,
            ai_generation,
            ai_snapshot,
            &format!("tabs and panes exceed the {MAX_WORKSPACE_STATE_BYTES}-byte workspace limit"),
        );
        return;
    }

    let mut has_ai_line = false;
    let mut durable_ai_snapshot = None;
    let mut durable_ai_estimated_bytes = 0usize;
    if let Some(snapshot) = ai_snapshot.as_ref() {
        match compact_ai_conversation_for_window(
            snapshot,
            &lines,
            MAX_WORKSPACE_STATE_BYTES,
            MAX_WINDOW_STATE_BYTES,
        ) {
            Some((line, compacted_snapshot, compacted)) => {
                if compacted {
                    log::warn!(
                        "Compacted older AI chat content to fit the complete window snapshot"
                    );
                }
                let insertion = usize::from(
                    lines
                        .first()
                        .is_some_and(|line| line.starts_with("current_page=")),
                );
                // The durable snapshot owns the same bounded text represented
                // by this serialized line. Charge twice its encoded size to
                // conservatively cover Vec/String container storage as well as
                // the final payload handed to the worker below.
                durable_ai_estimated_bytes = line.len().saturating_mul(2);
                lines.insert(insertion, line);
                has_ai_line = true;
                durable_ai_snapshot = Some(compacted_snapshot);
            }
            None => {
                log::error!(
                    "Refusing to replace the window snapshot because valid AI chat metadata cannot fit its reserved budget"
                );
                return;
            }
        }
    }

    let payload_limit = if has_ai_line {
        MAX_WINDOW_STATE_BYTES
    } else {
        MAX_WORKSPACE_STATE_BYTES
    };
    let Some(payload) = bounded_window_state_payload(&lines, payload_limit) else {
        log::error!(
            "Refusing to replace the window snapshot because its measured payload exceeds the {} byte limit",
            payload_limit
        );
        return;
    };
    let estimated_bytes = payload
        .capacity()
        .saturating_add(durable_ai_estimated_bytes);

    // GTK traversal ends here. Directory creation, write, fsync and atomic
    // replacement run on the single bounded persistence worker. Repeated
    // autosaves for this window replace an older pending snapshot.
    let key = PersistenceKey::for_path("window-state", &path);
    let path_for_job = path.clone();
    if let Err(error) =
        persistence::enqueue_weighted(key, "Save window session", estimated_bytes, move || {
            if let Some(parent) = path_for_job.parent() {
                ensure_private_directory(parent)?;
            }
            atomic_write_private_file(&path_for_job, payload.as_bytes())?;
            commit_ai_conversation_snapshot(ai_generation, durable_ai_snapshot);
            log::info!(
                "Successfully saved tabs state to {}",
                path_for_job.display()
            );
            Ok(())
        })
    {
        log::error!("Could not queue window session save: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pruned_snapshot_tabs_keep_interactive_order_and_remap_active() {
        // No task tabs: the selection is the identity.
        assert_eq!(
            pruned_snapshot_tabs(&[false, false, false], 2),
            (vec![0, 1, 2], 2)
        );
        // A task tab in the middle is dropped; later tabs shift down.
        assert_eq!(
            pruned_snapshot_tabs(&[false, true, false, false], 3),
            (vec![0, 2, 3], 2)
        );
        // Leading task tab: everything shifts and the active tab follows.
        assert_eq!(
            pruned_snapshot_tabs(&[true, false, false], 2),
            (vec![1, 2], 1)
        );
    }

    #[test]
    fn pruned_snapshot_tabs_fall_back_when_active_was_pruned() {
        // The active tab itself holds a task terminal: fall back to the first
        // survivor rather than pointing past the kept list.
        assert_eq!(
            pruned_snapshot_tabs(&[false, true, false], 1),
            (vec![0, 2], 0)
        );
        // Everything is a task tab: nothing survives and the snapshot becomes
        // the explicit empty tombstone the restore path already understands.
        assert_eq!(pruned_snapshot_tabs(&[true, true], 1), (Vec::new(), 0));
        // Degenerate empty input stays empty.
        assert_eq!(pruned_snapshot_tabs(&[], 0), (Vec::new(), 0));
    }

    fn conversation_snapshot(question: &str, answer: &str) -> crate::ai::ConversationSnapshot {
        let history = vec![
            crate::ai::Turn {
                role: crate::ai::Role::User,
                text: question.into(),
            },
            crate::ai::Turn {
                role: crate::ai::Role::Assistant,
                text: answer.into(),
            },
        ];
        crate::ai::ConversationSnapshot::from_completed_history(&history, None).unwrap()
    }

    #[test]
    fn workspace_ai_preservation_estimate_covers_all_bounded_owners() {
        let workspace_only =
            MAX_WINDOW_STATE_BYTES.saturating_mul(PRESERVED_WORKSPACE_BUFFER_OWNERS);
        assert_eq!(
            estimated_workspace_ai_preservation_bytes(false),
            workspace_only
        );
        assert_eq!(
            estimated_workspace_ai_preservation_bytes(true),
            workspace_only.saturating_add(
                crate::ai::MAX_CONVERSATION_SNAPSHOT_JSON_BYTES
                    .saturating_mul(PRESERVED_AI_JSON_EQUIVALENT_OWNERS)
            )
        );
        assert!(
            estimated_workspace_ai_preservation_bytes(true)
                <= crate::persistence::MAX_PENDING_ESTIMATED_BYTES
        );
    }

    fn temporary_state_dir(test_name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("forge-{test_name}-{}", generate_session_id()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn test_leaf(index: usize) -> PaneLayout {
        PaneLayout::Leaf {
            dir: format!("/tmp/{index}"),
            sid: format!("sid-{index}"),
            cwd_external: false,
            remote_name: None,
            custom_title: None,
            private_title: None,
            cmds: None,
            pinned: None,
        }
    }

    fn deep_test_layout(leaves: usize) -> PaneLayout {
        let mut layout = test_leaf(0);
        for index in 1..leaves {
            layout = PaneLayout::Split {
                orientation: 'h',
                position: 100,
                start: Box::new(layout),
                end: Box::new(test_leaf(index)),
            };
        }
        layout
    }

    fn wide_test_layout(start: usize, leaves: usize) -> PaneLayout {
        if leaves == 1 {
            return test_leaf(start);
        }
        let left = leaves / 2;
        PaneLayout::Split {
            orientation: 'v',
            position: 100,
            start: Box::new(wide_test_layout(start, left)),
            end: Box::new(wide_test_layout(start + left, leaves - left)),
        }
    }

    fn layout_tab_line(name: &str, layout: &PaneLayout) -> String {
        format!(
            "tab={name}\t{}",
            escape_tab_state(&serde_json::to_string(layout).unwrap())
        )
    }

    fn restored_test_sid(input: &str) -> String {
        let mut layout = test_leaf(0);
        let PaneLayout::Leaf { sid, .. } = &mut layout else {
            unreachable!("test_leaf always returns a leaf")
        };
        *sid = input.to_string();
        let (_, tabs) = parse_tabs_state(&layout_tab_line("sid", &layout));
        let PaneLayout::Leaf { sid, .. } = &tabs[0].1 else {
            panic!("expected a leaf")
        };
        sid.clone()
    }

    #[test]
    fn parses_snapshot_owner_pid() {
        assert_eq!(
            snapshot_owner_pid(Path::new("window-123-456.active")),
            Some(123)
        );
        assert_eq!(
            snapshot_owner_start_ticks(Path::new("window-123-456-789.active")),
            Some(456)
        );
        assert_eq!(
            snapshot_owner_start_ticks(Path::new("window-123-789.active")),
            None,
            "legacy pid-wallclock names have no reuse-proof owner token"
        );
        assert_eq!(snapshot_owner_pid(Path::new("other.active")), None);
    }

    #[test]
    fn parses_process_start_ticks_after_a_tricky_comm_field() {
        let mut stat = "77 (worker ) name) S".to_string();
        for _ in 0..18 {
            stat.push_str(" 0");
        }
        stat.push_str(" 4242 0 0");
        assert_eq!(parse_process_start_ticks(&stat), Some(4242));
        assert!(process_start_ticks_result(std::process::id() as i32).is_ok());
    }

    /// Snapshot recovery reclaims a file only from a window process that is
    /// definitely gone. The probe reads `/proc` and never signals: this pid
    /// belongs to another window, not to a child of this process.
    #[test]
    fn snapshot_recovery_only_reclaims_a_vanished_owner() {
        assert!(snapshot_owner_is_running(std::process::id() as i32));
        // Ill-formed owners parsed out of a file name must not read as alive.
        assert!(!snapshot_owner_is_running(0));
        assert!(!snapshot_owner_is_running(-1));

        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn test child");
        let pid = child.id() as i32;
        child.wait().expect("reap test child");
        assert!(
            !snapshot_owner_is_running(pid),
            "a reaped process must read as gone so its snapshot can be recovered"
        );
    }

    #[test]
    fn snapshot_recovery_distinguishes_a_reused_live_pid() {
        let directory = temporary_state_dir("pid-reuse-owner-token");
        let pid = std::process::id() as i32;
        let actual_start = process_start_ticks_result(pid).expect("current process start token");
        let different_start = actual_start.checked_add(1).unwrap_or(actual_start - 1);

        let current = directory.join(format!("window-{pid}-{actual_start}-1.active"));
        let reused = directory.join(format!("window-{pid}-{different_start}-2.active"));
        let legacy = directory.join(format!("window-{pid}-3.active"));
        fs::write(&current, "current").unwrap();
        fs::write(&reused, "reused").unwrap();
        fs::write(&legacy, "legacy").unwrap();

        recover_stale_active_snapshots(&directory);

        assert!(
            current.exists(),
            "matching live owner token must be retained"
        );
        assert!(
            legacy.exists(),
            "legacy live-PID snapshots stay conservative"
        );
        assert!(!reused.exists(), "mismatched start token is stale");
        assert_eq!(
            fs::read_to_string(reused.with_extension(READY_STATE_EXTENSION)).unwrap(),
            "reused"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn claims_each_ready_snapshot_at_most_once() {
        let directory = temporary_state_dir("claim-ready");
        fs::write(directory.join("window-1-1.state"), "one").unwrap();
        fs::write(directory.join("window-2-2.state"), "two").unwrap();

        let active_one = directory.join("window-10-10.active");
        let active_two = directory.join("window-11-11.active");
        assert!(claim_ready_snapshot_in(&directory, &active_one).is_some());
        assert!(claim_ready_snapshot_in(&directory, &active_two).is_some());
        assert!(
            claim_ready_snapshot_in(&directory, &directory.join("window-12-12.active")).is_none()
        );

        let mut payloads = vec![
            fs::read_to_string(active_one).unwrap(),
            fs::read_to_string(active_two).unwrap(),
        ];
        payloads.sort();
        assert_eq!(payloads, ["one", "two"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn never_claims_an_active_window_snapshot() {
        let directory = temporary_state_dir("ignore-active");
        fs::write(directory.join("window-1-1.active"), "live").unwrap();
        let destination = directory.join("window-2-2.active");
        assert!(claim_ready_snapshot_in(&directory, &destination).is_none());
        assert!(!destination.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn claiming_and_recovery_never_overwrite_an_existing_destination() {
        let directory = temporary_state_dir("noreplace-state");
        let ready = directory.join("window-1-1.state");
        let active = directory.join("window-2-2.active");
        fs::write(&ready, "ready-last-good").unwrap();
        fs::write(&active, "active-current").unwrap();

        assert!(claim_ready_snapshot_in(&directory, &active).is_none());
        assert_eq!(fs::read_to_string(&ready).unwrap(), "ready-last-good");
        assert_eq!(fs::read_to_string(&active).unwrap(), "active-current");

        let stale = directory.join(format!("window-{}-9.active", i32::MAX));
        let collision = stale.with_extension(READY_STATE_EXTENSION);
        fs::write(&stale, "interrupted").unwrap();
        fs::write(&collision, "published").unwrap();
        recover_stale_active_snapshots(&directory);
        assert_eq!(fs::read_to_string(&stale).unwrap(), "interrupted");
        assert_eq!(fs::read_to_string(&collision).unwrap(), "published");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn prunes_ready_snapshots_to_retention_limit() {
        let directory = temporary_state_dir("prune-ready");
        for index in 0..5 {
            fs::write(
                directory.join(format!("window-{index}-{index}.state")),
                index.to_string(),
            )
            .unwrap();
        }
        prune_ready_snapshots_in(&directory, 2);
        assert_eq!(ready_snapshots_in(&directory).len(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn private_state_storage_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_state_dir("private-permissions");
        let directory = root.join("windows");
        let snapshot = directory.join("window-1-1.active");
        atomic_write_private_file(&snapshot, b"state").unwrap();

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ai_conversation_line_round_trips_without_becoming_a_tab() {
        let snapshot = conversation_snapshot("为什么？\n第二行", "因为 C:\\tmp");
        let line = ai_conversation_state_line(&snapshot).unwrap();
        let contents = format!("current_page=0\n{line}\ntab=/tmp\n");

        assert_eq!(parse_ai_conversation(&contents), Some(snapshot));
        let (current_page, tabs) = parse_tabs_state(&contents);
        assert_eq!(current_page, Some(0));
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn restore_limits_many_tabs_before_any_spawn() {
        let contents = (0..(MAX_RESTORED_TABS + 20))
            .map(|index| format!("/tmp/tab-{index}"))
            .collect::<Vec<_>>()
            .join("\n");

        let (_, tabs) = parse_tabs_state(&contents);
        assert_eq!(tabs.len(), MAX_RESTORED_TABS);
        assert_eq!(
            tabs.iter()
                .filter_map(|(_, layout)| pane_layout_leaf_count_bounded(layout, usize::MAX))
                .sum::<usize>(),
            MAX_RESTORED_TABS
        );
    }

    #[test]
    fn restore_limits_discard_deep_and_wide_splits_and_cap_total_panes() {
        let deep = deep_test_layout(MAX_RESTORED_PANES_PER_TAB + 1);
        let wide = wide_test_layout(100, MAX_RESTORED_PANES_PER_TAB + 1);
        let full = wide_test_layout(200, MAX_RESTORED_PANES_PER_TAB);
        let mut lines = vec![
            layout_tab_line("deep", &deep),
            layout_tab_line("wide", &wide),
        ];
        for index in 0..5 {
            lines.push(layout_tab_line(&format!("full-{index}"), &full));
        }

        let (_, tabs) = parse_tabs_state(&lines.join("\n"));
        assert_eq!(tabs.len(), 4);
        assert_eq!(
            tabs.iter()
                .map(|(_, layout)| pane_layout_leaf_count_bounded(layout, usize::MAX).unwrap())
                .sum::<usize>(),
            MAX_RESTORED_PANES_TOTAL
        );
        assert!(tabs.iter().all(|(name, _)| name
            .as_deref()
            .is_some_and(|name| name.starts_with("full-"))));
    }

    #[test]
    fn window_headroom_compacts_chat_payload_without_dropping_metadata() {
        let snapshot = conversation_snapshot(&"q".repeat(4096), &"a".repeat(4096));
        let mut base_lines = vec!["current_page=0".to_string(), "tab=/tmp".to_string()];
        let base_len = base_lines.iter().map(String::len).sum::<usize>() + base_lines.len();
        let max_bytes = base_len + 512;

        let (line, compacted, changed) =
            compact_ai_conversation_for_window(&snapshot, &base_lines, base_len, max_bytes)
                .unwrap();
        assert!(changed);
        assert_eq!(compacted.chats().len(), 1);
        assert!(compacted.active_chat().unwrap().turns().is_empty());
        assert!(compacted.active_chat().unwrap().history_truncated());

        base_lines.insert(1, line.clone());
        let payload = base_lines.join("\n") + "\n";
        assert!(payload.len() <= max_bytes);
        assert_eq!(parse_ai_conversation(&line), Some(compacted));
    }

    #[test]
    fn metadata_reserve_keeps_fifty_worst_case_chat_rows() {
        let first_id = u64::MAX - (crate::ai::MAX_PERSISTED_CHATS as u64 - 1);
        let title = "\\".repeat(80);
        let draft = "d".repeat(64 * 1024);
        let chats: Vec<_> = (0..crate::ai::MAX_PERSISTED_CHATS)
            .map(|offset| {
                crate::ai::ChatSnapshot::from_completed_history(
                    first_id + offset as u64,
                    &title,
                    offset % 2 == 0,
                    &[],
                    None,
                    &draft,
                )
            })
            .collect();
        let snapshot = crate::ai::ConversationSnapshot::from_chats(u64::MAX, chats).unwrap();
        let workspace_limit = 1024;
        let mut base_lines = vec!["x".repeat(workspace_limit - 1)];

        let (line, compacted, changed) = compact_ai_conversation_for_window(
            &snapshot,
            &base_lines,
            workspace_limit,
            workspace_limit + MAX_AI_METADATA_RESERVE_BYTES,
        )
        .unwrap();
        assert!(changed);
        assert_eq!(compacted.active_chat_id(), u64::MAX);
        assert_eq!(compacted.chats().len(), crate::ai::MAX_PERSISTED_CHATS);
        assert!(compacted.chats().iter().all(|chat| chat.title() == title));
        assert!(compacted.chats().iter().all(|chat| chat.draft().is_empty()));
        assert!(compacted
            .chats()
            .iter()
            .all(crate::ai::ChatSnapshot::history_truncated));

        base_lines.push(line);
        assert!(
            (base_lines.join("\n") + "\n").len() <= workspace_limit + MAX_AI_METADATA_RESERVE_BYTES
        );

        let oversized_base = vec!["x".repeat(workspace_limit)];
        assert!(compact_ai_conversation_for_window(
            &snapshot,
            &oversized_base,
            workspace_limit,
            workspace_limit + MAX_AI_METADATA_RESERVE_BYTES,
        )
        .is_none());
    }

    #[test]
    fn invalid_ai_payload_does_not_prevent_tab_recovery() {
        let contents = concat!(
            "ai_conversation={not-json}\n",
            "current_page=0\n",
            "tab=Terminal 1\t/tmp\t123-456\n"
        );

        assert!(parse_ai_conversation(contents).is_none());
        let (current_page, tabs) = parse_tabs_state(contents);
        assert_eq!(current_page, Some(0));
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn duplicate_or_future_ai_payload_is_ignored() {
        let snapshot = conversation_snapshot("q", "a");
        let line = ai_conversation_state_line(&snapshot).unwrap();
        assert!(parse_ai_conversation(&format!("{line}\n{line}\n")).is_none());

        let future = r#"ai_conversation={"version":3,"active_chat_id":1,"chats":[]}"#;
        assert!(parse_ai_conversation(future).is_none());
    }

    #[test]
    fn stale_active_recovery_keeps_conversation_with_its_window() {
        let directory = temporary_state_dir("recover-ai");
        let snapshot = conversation_snapshot("crash question", "last complete answer");
        let line = ai_conversation_state_line(&snapshot).unwrap();
        let stale = directory.join(format!("window-{}-1.active", i32::MAX));
        fs::write(&stale, format!("{line}\ntab=/tmp\n")).unwrap();

        recover_stale_active_snapshots(&directory);
        let ready = stale.with_extension(READY_STATE_EXTENSION);
        assert!(ready.exists());
        let claimed = directory.join("window-10-10.active");
        assert!(claim_ready_snapshot_in(&directory, &claimed).is_some());
        let contents = fs::read_to_string(claimed).unwrap();
        assert_eq!(parse_ai_conversation(&contents), Some(snapshot));
        assert_eq!(parse_tabs_state(&contents).1.len(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomic_private_replace_is_durable_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_state_dir("atomic-replace");
        let directory = root.join("windows");
        ensure_private_directory(&directory).unwrap();
        let target = directory.join("window-1-1.active");
        atomic_write_private_file(&target, b"first").unwrap();
        atomic_write_private_file(&target, b"second").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"second");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(fs::read_dir(&directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn held_state_directory_prevents_parent_namespace_redirection() {
        let root = temporary_state_dir("parent-swap");
        let live = root.join("live");
        let displaced = root.join("displaced");
        let directory = ensure_private_directory(&live).unwrap();
        let target = live.join("window-1-1.active");

        fs::rename(&live, &displaced).unwrap();
        ensure_private_directory(&live).unwrap();
        atomic_write_private_file_in_directory(&directory, &target, b"payload").unwrap();

        assert_eq!(
            fs::read(displaced.join("window-1-1.active")).unwrap(),
            b"payload"
        );
        assert!(!live.join("window-1-1.active").exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// Oversized data is kept distinct from malformed UTF-8 in diagnostics.
    #[test]
    fn bounded_reader_rejects_pathological_snapshot_before_parsing() {
        let root = temporary_state_dir("bounded-read");
        let path = root.join("oversized.state");
        let file = fs::File::create(&path).unwrap();
        file.set_len((MAX_WINDOW_STATE_BYTES + 1) as u64).unwrap();
        drop(file);

        let error = read_window_state_bounded(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_reader_rejects_symlink_hardlink_and_fifo() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let root = temporary_state_dir("bounded-special-files");
        let original = root.join("original.state");
        let linked = root.join("linked.state");
        let symbolic = root.join("symbolic.state");
        let fifo = root.join("fifo.state");
        fs::write(&original, b"tab=/tmp\n").unwrap();
        fs::hard_link(&original, &linked).unwrap();
        symlink(&original, &symbolic).unwrap();
        let fifo_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_path is a live, NUL-terminated path and mkfifo retains
        // no pointer after returning.
        assert_eq!(unsafe { nix::libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);

        assert!(read_window_state_bounded(&original).is_err());
        assert!(read_window_state_bounded(&linked).is_err());
        assert!(read_window_state_bounded(&symbolic).is_err());
        assert!(read_window_state_bounded(&fifo).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_helpers_do_not_follow_file_or_parent_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temporary_state_dir("private-symlinks");
        let real_parent = root.join("real-parent");
        fs::create_dir(&real_parent).unwrap();
        fs::set_permissions(&real_parent, fs::Permissions::from_mode(0o755)).unwrap();
        let parent_link = root.join("parent-link");
        symlink(&real_parent, &parent_link).unwrap();

        let result = atomic_write_private_file(&parent_link.join("window.state"), b"state");
        assert!(result.is_err());
        assert!(!real_parent.join("window.state").exists());
        assert_eq!(
            fs::metadata(&real_parent).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let target = root.join("target");
        let file_link = root.join("file-link");
        fs::write(&target, b"do not touch").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&target, &file_link).unwrap();
        assert!(make_file_private(&file_link).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"do not touch");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// The data-loss fix this round exists for: a snapshot this window has
    /// already claimed (renamed onto its `.active` name) fails to read, and the
    /// very next save would overwrite it. Quarantine must move the bytes aside
    /// under a name the `state`/`active` scans can never restore, and the
    /// quarantine prune must retire old copies the ready-state prune cannot see.
    #[test]
    fn unreadable_claimed_snapshot_is_quarantined_before_it_can_be_overwritten() {
        let directory = temporary_state_dir("quarantine-corrupt");
        let active = directory.join("window-1-1.active");
        fs::write(&active, [0xffu8, 0xfe, b'{']).unwrap();

        let error = read_window_state_bounded(&active).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        quarantine_corrupt_snapshot(&active);

        assert!(!active.exists(), "the corrupt bytes must be moved aside");
        let quarantined: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_quarantined_snapshot(path))
            .collect();
        assert_eq!(quarantined.len(), 1);
        assert_eq!(fs::read(&quarantined[0]).unwrap(), [0xffu8, 0xfe, b'{']);
        // A fresh save over the claimed path no longer touches the evidence.
        atomic_write_private_file(&active, b"fresh state").unwrap();
        assert_eq!(fs::read(&quarantined[0]).unwrap(), [0xffu8, 0xfe, b'{']);

        // Quarantined names must be invisible to every snapshot scan: restoring
        // one would resurrect state the user was already told was corrupt.
        assert!(ready_snapshots_in(&directory).is_empty());
        assert!(snapshots_with_extension(&directory, ACTIVE_STATE_EXTENSION)
            .iter()
            .all(|path| path == &active));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quarantine_prune_bounds_corrupt_backups_without_touching_snapshots() {
        let directory = temporary_state_dir("quarantine-prune");
        for index in 0..4 {
            let active = directory.join(format!("window-{index}-{index}.active"));
            fs::write(&active, [0xffu8]).unwrap();
            quarantine_corrupt_snapshot(&active);
        }
        fs::write(directory.join("window-9-9.state"), "ready").unwrap();

        prune_quarantined_snapshots_in(&directory, 2);

        let quarantined = fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_quarantined_snapshot(path))
            .count();
        assert_eq!(quarantined, 2);
        assert_eq!(
            ready_snapshots_in(&directory).len(),
            1,
            "ready snapshots are not the quarantine prune's to delete"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    /// Pins the serde wiring to the shared decoder: structured argv restores,
    /// while the legacy joined-string form loads the tab but never replays.
    #[test]
    fn restorable_argv_accepts_structured_form_and_drops_legacy_strings() {
        let structured: PaneLayout = serde_json::from_str(
            r#"{"type":"leaf","dir":"/tmp","sid":"1-2","cmds":["ssh","host"]}"#,
        )
        .unwrap();
        let PaneLayout::Leaf { cmds, .. } = structured else {
            panic!("expected a leaf layout");
        };
        assert_eq!(cmds, Some(vec!["ssh".to_string(), "host".to_string()]));

        let legacy: PaneLayout = serde_json::from_str(
            r#"{"type":"leaf","dir":"/tmp","sid":"1-2","cmds":"ssh host 'echo a; rm b'"}"#,
        )
        .unwrap();
        let PaneLayout::Leaf { cmds, .. } = legacy else {
            panic!("expected a leaf layout");
        };
        assert_eq!(cmds, None, "a joined string must never be replayed");

        let arbitrary: PaneLayout = serde_json::from_str(
            r#"{"type":"leaf","dir":"/tmp","sid":"1-2","cmds":["sh","-c","touch /tmp/pwned"]}"#,
        )
        .unwrap();
        let PaneLayout::Leaf { cmds, .. } = arbitrary else {
            panic!("expected a leaf layout");
        };
        assert_eq!(cmds, None, "arbitrary structured argv must be dropped");

        let visually_spoofed: PaneLayout = serde_json::from_str(
            r#"{"type":"leaf","dir":"/tmp","sid":"1-2","cmds":["ssh","safe\u202etxt"]}"#,
        )
        .unwrap();
        let PaneLayout::Leaf { cmds, .. } = visually_spoofed else {
            panic!("expected a leaf layout");
        };
        assert_eq!(cmds, None, "visually spoofed argv must never be replayed");

        let too_many = std::iter::once("ssh".to_string())
            .chain(
                (0..crate::process::MAX_RESTORABLE_ARG_COUNT_LOCAL)
                    .map(|index| format!("arg-{index}")),
            )
            .collect::<Vec<_>>();
        let encoded = serde_json::json!({
            "type": "leaf",
            "dir": "/tmp",
            "sid": "1-2",
            "cmds": too_many,
        })
        .to_string();
        let PaneLayout::Leaf { cmds, .. } = serde_json::from_str(&encoded).unwrap() else {
            panic!("expected a leaf layout");
        };
        assert_eq!(
            cmds, None,
            "oversized ssh argv must be dropped during parse"
        );
    }

    #[test]
    fn pane_snapshot_schema_keeps_legacy_defaults_and_managed_remote_metadata() {
        let legacy: PaneLayout =
            serde_json::from_str(r#"{"type":"leaf","dir":"/tmp","sid":"legacy-1","pinned":true}"#)
                .unwrap();
        let PaneLayout::Leaf {
            cwd_external,
            remote_name,
            custom_title,
            ..
        } = legacy
        else {
            panic!("expected a leaf layout");
        };
        assert!(!cwd_external);
        assert_eq!(remote_name, None);
        assert_eq!(custom_title, None);

        let managed = PaneLayout::Leaf {
            dir: "/srv/remote".into(),
            sid: "resume-42".into(),
            cwd_external: false,
            remote_name: Some("production".into()),
            custom_title: Some(false),
            private_title: Some(true),
            // A modified/older producer may include an argv. Normalization
            // must still prefer the stable profile identity and discard it.
            cmds: Some(vec!["ssh".into(), "stale.example".into()]),
            pinned: Some(true),
        };
        let (_, tabs) = parse_tabs_state(&layout_tab_line("prod", &managed));
        let PaneLayout::Leaf {
            cwd_external,
            remote_name,
            custom_title,
            cmds,
            pinned,
            ..
        } = &tabs[0].1
        else {
            panic!("expected a leaf layout");
        };
        assert!(*cwd_external, "a remote cwd must never become a local cwd");
        assert_eq!(remote_name.as_deref(), Some("production"));
        assert_eq!(*custom_title, Some(false));
        assert_eq!(*pinned, Some(true));
        assert!(
            cmds.is_none(),
            "managed argv is rebuilt from current config"
        );
    }

    #[test]
    fn restore_sanitizes_session_ids_and_rejects_oversized_fields() {
        let sid = restored_test_sid("safe'\nrun-local");
        assert_ne!(sid, "safe'\nrun-local");
        assert!(sid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-'));

        let max_session_id = "s".repeat(jterm_core::execution_journal::MAX_JSH_SESSION_ID_BYTES);
        assert_eq!(restored_test_sid(&max_session_id), max_session_id);

        for invalid in [
            "session.with.dots".to_string(),
            "雪".to_string(),
            "s".repeat(jterm_core::execution_journal::MAX_JSH_SESSION_ID_BYTES + 1),
        ] {
            let sid = restored_test_sid(&invalid);
            assert_ne!(sid, invalid);
            assert!(jterm_core::execution_journal::is_valid_jsh_session_id(&sid));
        }

        let oversized_dir = PaneLayout::Leaf {
            dir: "x".repeat(MAX_RESTORED_CWD_BYTES + 1),
            sid: "1-2".into(),
            cwd_external: false,
            remote_name: None,
            custom_title: None,
            private_title: None,
            cmds: None,
            pinned: None,
        };
        let invalid_orientation = PaneLayout::Split {
            orientation: 'x',
            position: i32::MAX,
            start: Box::new(test_leaf(1)),
            end: Box::new(test_leaf(2)),
        };
        let contents = [
            layout_tab_line("oversized", &oversized_dir),
            layout_tab_line("invalid split", &invalid_orientation),
            layout_tab_line(&"n".repeat(MAX_RESTORED_TAB_NAME_BYTES + 1), &test_leaf(3)),
        ]
        .join("\n");
        let (_, tabs) = parse_tabs_state(&contents);
        assert_eq!(tabs.len(), 1);
        assert_eq!(
            tabs[0].0, None,
            "oversized label is replaced by the UI default"
        );
    }

    #[test]
    fn bounded_payload_refuses_oversize_instead_of_dropping_ai() {
        let lines = vec![
            "current_page=0".to_string(),
            "ai_conversation=xxxxxxxxxxxxxxxx".to_string(),
            "tab=workspace".to_string(),
        ];
        assert!(bounded_window_state_payload(&lines, 40).is_none());
        assert_eq!(lines[1], "ai_conversation=xxxxxxxxxxxxxxxx");
    }

    #[test]
    fn fallback_rewrites_ai_without_touching_previous_workspace() {
        let root = temporary_state_dir("rewrite-ai-only");
        let path = root.join("window-1-1.active");
        let original = conversation_snapshot("old question", "old answer");
        let replacement = conversation_snapshot("new question", "new answer");
        let original_line = ai_conversation_state_line(&original).unwrap();
        fs::write(
            &path,
            format!("current_page=0\n{original_line}\ntab=/tmp\n"),
        )
        .unwrap();

        let (compacted, durable) =
            rewrite_existing_ai_conversation(&path, Some(&replacement)).unwrap();
        assert!(!compacted);
        assert_eq!(durable, Some(replacement.clone()));
        let replaced = fs::read_to_string(&path).unwrap();
        assert_eq!(parse_ai_conversation(&replaced), Some(replacement));
        assert_eq!(parse_tabs_state(&replaced).1.len(), 1);

        let (compacted, durable) = rewrite_existing_ai_conversation(&path, None).unwrap();
        assert!(!compacted);
        assert_eq!(durable, None);
        let cleared = fs::read_to_string(&path).unwrap();
        assert!(parse_ai_conversation(&cleared).is_none());
        assert_eq!(parse_tabs_state(&cleared).1.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
