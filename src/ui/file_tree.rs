//! file_tree — asynchronous GTK4 sidebar file browser for UiState.
//!
//! The browser uses `TreeListModel` + `ListView`, the supported GTK4 model-view
//! stack. Directory enumeration remains off the UI thread and is created lazily
//! when a directory row is expanded. A `MultiSelection` gives ctrl+click and
//! shift+click selection for batch operations; a type-to-filter row wraps the
//! tree in a `FilterListModel` whose predicate consults a visible-path set
//! (matches + ancestors) computed from the loaded child stores, so filtering
//! never scans and row identity/expansion is untouched.
//!
//! Listing and file operations dispatch
//! through `super::remote_fs`: the tree browses the local disk or any
//! configured ssh/docker remote host, and a right-click context menu offers
//! New File/Folder, Rename, Delete, Copy, Cut, Copy Path, Paste and Refresh
//! on both. Paste across locations streams between the two filesystems
//! (download, upload, or a temp-relayed remote-to-remote hop) with throttled
//! progress in a persistent toast and a Cancel action that kills the
//! in-flight child and cleans up partial results. Dragging local files or
//! folders from the OS file manager onto the tree imports them the same way
//! (recursive copy locally, streaming upload remotely), planned up-front and
//! refused wholesale when the drop exceeds the item/byte caps. After a
//! mutation only the affected parent directories are re-listed, in place
//! with a minimal diff, so expansion state elsewhere in the tree survives.

use adw::prelude::*;
use gtk4::prelude::*;
use gtk4::{gio, glib, ListView, SignalListItemFactory, TreeListModel, TreeListRow};
use libadwaita as adw;
use std::cell::Cell;
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use vte4::TerminalExt;

use super::remote_fs::{self, FsClipboard, FsEntry, FsExecutionOverlay, FsLocation};
use super::*;
use crate::config::RemoteHost;
use crate::terminal::terminal_working_directory;
use jterm_core::jsh_remote::RemoteHostConfig;

const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(16);
const MAX_FILE_LABEL_BYTES: usize = 512;
const MAX_CONCURRENT_SCANS: usize = 8;
const MAX_PENDING_SCANS: usize = 64;
const MAX_PENDING_SCANS_PER_REMOTE_AUTHORITY: usize = 16;
const MAX_PENDING_SCANS_FOR_LOCAL_AUTHORITY: usize = 48;
const MAX_PENDING_FS_OPS: usize = 32;
const MAX_FILE_TREE_HISTORY: usize = 64;
const MAX_NAVIGATION_PATH_BYTES: usize = 4096;
const REMOTE_SNAPSHOT_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_TTL_REFRESHES_PER_TICK: usize = 4;
/// Mutating file operations get their own, smaller bound so a burst of
/// context-menu actions cannot crowd out directory scans.
const MAX_CONCURRENT_FS_OPS: usize = 4;
/// Header space is scarce. Forty-eight characters retain the recognizable
/// `root@dsw…aliyuncs.com (temporary)` ends while a tooltip carries the whole
/// sanitized endpoint.
const MAX_LOCATION_LABEL_CHARS: usize = 48;

// Re-exported so the existing tests keep one obvious name for the listing cap.
#[cfg(test)]
use super::remote_fs::MAX_DIRECTORY_ENTRIES;

#[derive(Clone, Debug)]
struct FileEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    status: Option<DirectoryRowStatus>,
}

/// Transient directory state rendered as a real row inside the tree. A
/// refresh appends one of these after the last-good children instead of
/// clearing them, so failure remains visible without sacrificing expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DirectoryRowStatus {
    Loading,
    Refreshing {
        last_good: Option<SystemTime>,
    },
    Error {
        message: String,
        last_good: Option<SystemTime>,
    },
}

impl DirectoryRowStatus {
    fn label(&self) -> String {
        match self {
            Self::Loading => "Loading…".to_string(),
            Self::Refreshing { last_good } => match last_good {
                Some(completed) => format!(
                    "Refreshing… · last updated {}",
                    snapshot_age(*completed, SystemTime::now())
                ),
                None => "Refreshing…".to_string(),
            },
            Self::Error { message, last_good } => match last_good {
                Some(completed) => format!(
                    "Error: {message} · showing snapshot from {}",
                    snapshot_age(*completed, SystemTime::now())
                ),
                None => format!("Error: {message}"),
            },
        }
    }

    fn is_retryable(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

fn snapshot_age(completed: SystemTime, now: SystemTime) -> String {
    let elapsed = now.duration_since(completed).unwrap_or_default();
    if elapsed < Duration::from_secs(60) {
        format!("{}s ago", elapsed.as_secs())
    } else if elapsed < Duration::from_secs(60 * 60) {
        format!("{}m ago", elapsed.as_secs() / 60)
    } else if elapsed < Duration::from_secs(24 * 60 * 60) {
        format!("{}h ago", elapsed.as_secs() / (60 * 60))
    } else {
        format!("{}d ago", elapsed.as_secs() / (24 * 60 * 60))
    }
}

#[derive(Clone, Copy, Debug)]
struct SnapshotMeta {
    completed_wall: SystemTime,
    completed_monotonic: Instant,
}

impl SnapshotMeta {
    fn now() -> Self {
        Self {
            completed_wall: SystemTime::now(),
            completed_monotonic: Instant::now(),
        }
    }
}

fn snapshot_meta_is_stale(snapshot: SnapshotMeta, now: Instant) -> bool {
    now.saturating_duration_since(snapshot.completed_monotonic) >= REMOTE_SNAPSHOT_TTL
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryFailureClass {
    Transient,
    Persistent,
}

#[derive(Clone, Copy, Debug)]
struct DirectoryFailureState {
    class: DirectoryFailureClass,
    consecutive: u32,
    retry_not_before: Instant,
}

fn directory_failure_class(error: &io::Error) -> DirectoryFailureClass {
    match error.kind() {
        io::ErrorKind::PermissionDenied
        | io::ErrorKind::InvalidInput
        | io::ErrorKind::InvalidData
        | io::ErrorKind::NotFound => DirectoryFailureClass::Persistent,
        _ => DirectoryFailureClass::Transient,
    }
}

fn directory_failure_delay(class: DirectoryFailureClass, consecutive: u32) -> Duration {
    match class {
        DirectoryFailureClass::Persistent => Duration::from_secs(30),
        DirectoryFailureClass::Transient => {
            let exponent = consecutive.saturating_sub(1).min(5);
            Duration::from_secs((1_u64 << exponent).min(30))
        }
    }
}

fn next_directory_failure_state(
    previous: Option<DirectoryFailureState>,
    error: &io::Error,
    now: Instant,
) -> DirectoryFailureState {
    let class = directory_failure_class(error);
    let consecutive = previous
        .filter(|previous| previous.class == class)
        .map(|previous| previous.consecutive.saturating_add(1))
        .unwrap_or(1);
    DirectoryFailureState {
        class,
        consecutive,
        retry_not_before: now + directory_failure_delay(class, consecutive),
    }
}

impl FileEntry {
    fn directory_status(dir: &Path, status: DirectoryRowStatus) -> Self {
        Self {
            name: status.label(),
            path: dir.to_path_buf(),
            is_dir: false,
            status: Some(status),
        }
    }

    fn is_item(&self) -> bool {
        self.status.is_none()
    }
}

#[derive(Clone, Debug)]
struct DirectoryScan {
    entries: Vec<FileEntry>,
    truncated: bool,
    timing: ScanTiming,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ScanTiming {
    queued_for: Duration,
    listed_for: Duration,
    queued_depth: usize,
}

fn entries_remain_current(expected: &[FileEntry], current: &[FileEntry]) -> bool {
    expected.iter().all(|expected| {
        expected.is_item()
            && current.iter().any(|entry| {
                entry.is_item() && entry.path == expected.path && entry.is_dir == expected.is_dir
            })
    })
}

fn surviving_selected_paths(selected: &[FileEntry], current: &[FileEntry]) -> Vec<PathBuf> {
    selected
        .iter()
        .filter(|selected| {
            selected.is_item()
                && current.iter().any(|entry| {
                    entry.is_item()
                        && entry.path == selected.path
                        && entry.is_dir == selected.is_dir
                })
        })
        .map(|entry| entry.path.clone())
        .collect()
}

fn selection_paths_after_reconcile(
    selected: &[FileEntry],
    current: &[FileEntry],
    preferred: Option<&[PathBuf]>,
) -> Vec<PathBuf> {
    match preferred {
        Some(paths) => paths
            .iter()
            .filter(|path| {
                current
                    .iter()
                    .any(|entry| entry.is_item() && entry.path == path.as_path())
            })
            .cloned()
            .collect(),
        None => surviving_selected_paths(selected, current),
    }
}

impl From<FsEntry> for FileEntry {
    /// Display names are sanitized on the way in; `path` keeps the exact
    /// bytes so file operations round-trip even for hostile names.
    fn from(entry: FsEntry) -> Self {
        FileEntry {
            name: safe_file_label(&entry.name),
            path: entry.path,
            is_dir: entry.is_dir,
            status: None,
        }
    }
}

/// Files owns only bare F5 while focus is inside its ListView. Modified F5
/// remains available to surrounding application/terminal shortcut handling;
/// lock/button state is deliberately irrelevant.
fn file_tree_is_plain_refresh_key(key: gtk4::gdk::Key, state: gtk4::gdk::ModifierType) -> bool {
    use gtk4::gdk::ModifierType;
    key == gtk4::gdk::Key::F5
        && !state.intersects(
            ModifierType::CONTROL_MASK
                | ModifierType::SHIFT_MASK
                | ModifierType::ALT_MASK
                | ModifierType::SUPER_MASK
                | ModifierType::HYPER_MASK
                | ModifierType::META_MASK,
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileTreeNavigationKey {
    Refresh,
    Up,
    Home,
    EnterDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileTreeNavigationPoint {
    location: FsLocation,
    overlay: FsExecutionOverlay,
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileTreeNavigationAction {
    Push,
    Back,
    Forward,
    Replace,
}

#[derive(Clone)]
struct FileTreeNavigationRequest {
    revision: u64,
    target: FileTreeNavigationPoint,
    action: FileTreeNavigationAction,
    cancel: remote_fs::CancelToken,
}

#[derive(Default)]
pub(crate) struct FileTreeNavigationState {
    revision: u64,
    pending: Option<FileTreeNavigationRequest>,
    current: Option<FileTreeNavigationPoint>,
    back: VecDeque<FileTreeNavigationPoint>,
    forward: VecDeque<FileTreeNavigationPoint>,
}

impl FileTreeNavigationState {
    fn push_bounded(
        history: &mut VecDeque<FileTreeNavigationPoint>,
        point: FileTreeNavigationPoint,
    ) {
        if history.len() == MAX_FILE_TREE_HISTORY {
            history.pop_front();
        }
        history.push_back(point);
    }

    fn begin(
        &mut self,
        target: FileTreeNavigationPoint,
        action: FileTreeNavigationAction,
    ) -> FileTreeNavigationRequest {
        if let Some(previous) = self.pending.take() {
            previous.cancel.cancel();
        }
        self.revision = self.revision.wrapping_add(1);
        let request = FileTreeNavigationRequest {
            revision: self.revision,
            target,
            action,
            cancel: remote_fs::CancelToken::default(),
        };
        self.pending = Some(request.clone());
        request
    }

    fn is_current(&self, request: &FileTreeNavigationRequest) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            pending.revision == request.revision && pending.target == request.target
        })
    }

    fn fail(&mut self, request: &FileTreeNavigationRequest) -> bool {
        if !self.is_current(request) {
            return false;
        }
        self.pending = None;
        true
    }

    fn cancel_pending(&mut self) -> bool {
        let Some(request) = self.pending.take() else {
            return false;
        };
        request.cancel.cancel();
        true
    }

    fn commit(&mut self, request: &FileTreeNavigationRequest) -> bool {
        if !self.is_current(request) {
            return false;
        }
        match request.action {
            FileTreeNavigationAction::Push => {
                if let Some(current) = self.current.take() {
                    if current != request.target {
                        Self::push_bounded(&mut self.back, current);
                        self.forward.clear();
                    }
                }
            }
            FileTreeNavigationAction::Back => {
                if self.back.back() != Some(&request.target) {
                    return false;
                }
                self.back.pop_back();
                if let Some(current) = self.current.take() {
                    Self::push_bounded(&mut self.forward, current);
                }
            }
            FileTreeNavigationAction::Forward => {
                if self.forward.back() != Some(&request.target) {
                    return false;
                }
                self.forward.pop_back();
                if let Some(current) = self.current.take() {
                    Self::push_bounded(&mut self.back, current);
                }
            }
            FileTreeNavigationAction::Replace => {}
        }
        self.current = Some(request.target.clone());
        self.pending = None;
        true
    }

    fn install_initial(&mut self, point: FileTreeNavigationPoint) {
        if let Some(previous) = self.pending.take() {
            previous.cancel.cancel();
        }
        self.revision = self.revision.wrapping_add(1);
        self.current = Some(point);
        self.back.clear();
        self.forward.clear();
    }

    fn back_target(&self) -> Option<FileTreeNavigationPoint> {
        self.back.back().cloned()
    }

    fn forward_target(&self) -> Option<FileTreeNavigationPoint> {
        self.forward.back().cloned()
    }

    fn remap_history_locations(
        &mut self,
        mut remap: impl FnMut(&FsLocation) -> Option<FsLocation>,
    ) {
        let remap_stack =
            |stack: &mut VecDeque<FileTreeNavigationPoint>,
             remap: &mut dyn FnMut(&FsLocation) -> Option<FsLocation>| {
                *stack = std::mem::take(stack)
                    .into_iter()
                    .filter_map(|mut point| {
                        point.location = remap(&point.location)?;
                        Some(point)
                    })
                    .collect();
            };
        remap_stack(&mut self.back, &mut remap);
        remap_stack(&mut self.forward, &mut remap);
    }

    fn revision(&self) -> u64 {
        self.revision
    }
}

fn validate_absolute_navigation_path(value: &str) -> Result<PathBuf, &'static str> {
    if value.is_empty() {
        return Err("Path is required");
    }
    if value.len() > MAX_NAVIGATION_PATH_BYTES {
        return Err("Path is too long");
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || jterm_core::review_input::is_visual_spoofing_character(ch))
    {
        return Err("Path contains hidden or control text");
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err("Path must be absolute");
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(component) => normalized.push(component),
            std::path::Component::ParentDir => {
                if normalized == Path::new("/") {
                    return Err("Path cannot traverse above the filesystem root");
                }
                normalized.pop();
            }
            std::path::Component::Prefix(_) => return Err("Path must use POSIX syntax"),
        }
    }
    Ok(normalized)
}

fn navigation_breadcrumbs(path: &Path) -> Vec<PathBuf> {
    let mut ancestors: Vec<_> = path.ancestors().map(Path::to_path_buf).collect();
    ancestors.reverse();
    ancestors
}

fn file_tree_location_from_selection(
    index: u32,
    active_remote_count: usize,
    current: &FsLocation,
) -> Option<FsLocation> {
    if index == gtk4::INVALID_LIST_POSITION {
        return None;
    }
    match index as usize {
        0 => Some(FsLocation::Local),
        selected if selected <= active_remote_count => Some(FsLocation::Remote(selected - 1)),
        _ if matches!(current, FsLocation::Transient(_)) => Some(current.clone()),
        _ => None,
    }
}

/// Shortcuts are captured only by the Files ListView. Ordinary Home/arrow
/// keys therefore remain GTK selection navigation and terminal shortcuts are
/// never claimed while the terminal has focus.
fn file_tree_navigation_key(
    key: gtk4::gdk::Key,
    state: gtk4::gdk::ModifierType,
) -> Option<FileTreeNavigationKey> {
    use gtk4::gdk::{Key, ModifierType};
    let modifiers = state
        & (ModifierType::CONTROL_MASK
            | ModifierType::SHIFT_MASK
            | ModifierType::ALT_MASK
            | ModifierType::SUPER_MASK
            | ModifierType::HYPER_MASK
            | ModifierType::META_MASK);
    if file_tree_is_plain_refresh_key(key, state) {
        return Some(FileTreeNavigationKey::Refresh);
    }
    if modifiers != ModifierType::ALT_MASK {
        return None;
    }
    match key {
        Key::Up => Some(FileTreeNavigationKey::Up),
        Key::Home => Some(FileTreeNavigationKey::Home),
        Key::Right => Some(FileTreeNavigationKey::EnterDirectory),
        _ => None,
    }
}

fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Local listing used by the entry-cap test; production scans dispatch
/// through `scan_entries` for any location.
#[cfg(test)]
fn scan_dir(dir: &Path) -> io::Result<Vec<FileEntry>> {
    scan_entries(
        &FsLocation::Local,
        &[],
        &FsExecutionOverlay::default(),
        dir,
        &remote_fs::CancelToken::default(),
    )
    .map(|listing| listing.entries)
}

fn scan_entries(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    dir: &Path,
    cancel: &remote_fs::CancelToken,
) -> io::Result<DirectoryScan> {
    remote_fs::list_dir_with_overlay_cancel(loc, hosts, overlay, dir, cancel).map(|listing| {
        DirectoryScan {
            entries: listing.entries.into_iter().map(FileEntry::from).collect(),
            truncated: listing.truncated,
            timing: ScanTiming::default(),
        }
    })
}

fn safe_file_label(value: &str) -> String {
    let mut label = String::with_capacity(value.len().min(MAX_FILE_LABEL_BYTES));
    for ch in value.chars() {
        let rendered =
            if ch.is_control() || jterm_core::review_input::is_visual_spoofing_character(ch) {
                '\u{fffd}'
            } else {
                ch
            };
        if label.len().saturating_add(rendered.len_utf8()) > MAX_FILE_LABEL_BYTES {
            if label.len().saturating_add('…'.len_utf8()) <= MAX_FILE_LABEL_BYTES {
                label.push('…');
            }
            break;
        }
        label.push(rendered);
    }
    label
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanPriority {
    Root,
    Manual,
    Lazy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryRefreshCause {
    Manual,
    Retry,
    AutoTtl,
}

impl DirectoryRefreshCause {
    fn priority(self) -> ScanPriority {
        match self {
            Self::AutoTtl => ScanPriority::Lazy,
            Self::Manual | Self::Retry => ScanPriority::Manual,
        }
    }
}

impl ScanPriority {
    fn lane(self) -> usize {
        match self {
            Self::Root => 0,
            Self::Manual => 1,
            Self::Lazy => 2,
        }
    }
}

/// Weighted service order: navigation/root work leads, explicit user refresh
/// follows, and lazy expansion still receives one guaranteed turn per cycle.
const SCAN_SERVICE_ORDER: [ScanPriority; 8] = [
    ScanPriority::Root,
    ScanPriority::Root,
    ScanPriority::Manual,
    ScanPriority::Root,
    ScanPriority::Manual,
    ScanPriority::Lazy,
    ScanPriority::Root,
    ScanPriority::Manual,
];

/// Pure bounded priority queue used by the fixed scan workers. A root/manual
/// request may evict the newest lazy request when full; no priority can starve
/// because `pop` advances through the weighted service cycle.
struct ScanQueue<T> {
    lanes: [VecDeque<T>; 3],
    capacity: usize,
    service_turn: usize,
}

impl<T> ScanQueue<T> {
    fn new(capacity: usize) -> Self {
        Self {
            lanes: std::array::from_fn(|_| VecDeque::new()),
            capacity,
            service_turn: 0,
        }
    }

    fn len(&self) -> usize {
        self.lanes.iter().map(VecDeque::len).sum()
    }

    fn count_where(&self, mut predicate: impl FnMut(&T) -> bool) -> usize {
        self.lanes
            .iter()
            .flatten()
            .filter(|item| predicate(item))
            .count()
    }

    fn push(&mut self, priority: ScanPriority, item: T) -> Result<Option<T>, T> {
        if self.len() < self.capacity {
            self.lanes[priority.lane()].push_back(item);
            return Ok(None);
        }
        if priority != ScanPriority::Lazy {
            if let Some(evicted) = self.lanes[ScanPriority::Lazy.lane()].pop_back() {
                self.lanes[priority.lane()].push_back(item);
                return Ok(Some(evicted));
            }
        }
        Err(item)
    }

    fn pop(&mut self) -> Option<T> {
        self.pop_where(|_| true)
    }

    fn pop_where(&mut self, mut predicate: impl FnMut(&T) -> bool) -> Option<T> {
        for offset in 0..SCAN_SERVICE_ORDER.len() {
            let turn = (self.service_turn + offset) % SCAN_SERVICE_ORDER.len();
            let lane = SCAN_SERVICE_ORDER[turn].lane();
            let position = self.lanes[lane].iter().position(&mut predicate);
            if let Some(item) = position.and_then(|position| self.lanes[lane].remove(position)) {
                self.service_turn = (turn + 1) % SCAN_SERVICE_ORDER.len();
                return Some(item);
            }
        }
        None
    }

    fn has_where(&self, mut predicate: impl FnMut(&T) -> bool) -> bool {
        self.lanes.iter().flatten().any(&mut predicate)
    }

    fn remove_newest_where(
        &mut self,
        priorities: &[ScanPriority],
        mut predicate: impl FnMut(&T) -> bool,
    ) -> Option<T> {
        for priority in priorities {
            let lane = &mut self.lanes[priority.lane()];
            if let Some(position) = lane.iter().rposition(&mut predicate) {
                return lane.remove(position);
            }
        }
        None
    }

    fn remove_where(&mut self, mut predicate: impl FnMut(&T) -> bool) -> Vec<T> {
        let mut removed = Vec::new();
        for lane in &mut self.lanes {
            let mut kept = VecDeque::with_capacity(lane.len());
            while let Some(item) = lane.pop_front() {
                if predicate(&item) {
                    removed.push(item);
                } else {
                    kept.push_back(item);
                }
            }
            *lane = kept;
        }
        removed
    }
}

struct ScanJob {
    authority: remote_fs::FilesystemIdentity,
    loc: FsLocation,
    hosts: Vec<RemoteHost>,
    overlay: FsExecutionOverlay,
    dir: PathBuf,
    cancel: remote_fs::CancelToken,
    tx: mpsc::SyncSender<io::Result<DirectoryScan>>,
    enqueued_at: Instant,
    queued_depth: usize,
}

impl ScanJob {
    fn run(self) {
        let started_at = Instant::now();
        let result = if self.cancel.is_cancelled() {
            Err(remote_fs::cancelled_error())
        } else {
            scan_entries(
                &self.loc,
                &self.hosts,
                &self.overlay,
                &self.dir,
                &self.cancel,
            )
        }
        .map(|mut scan| {
            scan.timing = ScanTiming {
                queued_for: started_at.saturating_duration_since(self.enqueued_at),
                listed_for: started_at.elapsed(),
                queued_depth: self.queued_depth,
            };
            scan
        });
        let _ = self.tx.send(result);
    }

    fn retire(self, error: io::Error) {
        self.cancel.cancel();
        let _ = self.tx.send(Err(error));
    }
}

struct ScanSchedulerState {
    queue: ScanQueue<ScanJob>,
    authority_order: VecDeque<remote_fs::FilesystemIdentity>,
    running_by_authority: std::collections::HashMap<remote_fs::FilesystemIdentity, usize>,
}

fn scan_authority_limit(authority: &remote_fs::FilesystemIdentity) -> usize {
    match authority {
        remote_fs::FilesystemIdentity::Local => MAX_CONCURRENT_SCANS,
        remote_fs::FilesystemIdentity::Remote { .. } => 2,
    }
}

fn scan_authority_pending_limit(authority: &remote_fs::FilesystemIdentity) -> usize {
    match authority {
        remote_fs::FilesystemIdentity::Local => MAX_PENDING_SCANS_FOR_LOCAL_AUTHORITY,
        remote_fs::FilesystemIdentity::Remote { .. } => MAX_PENDING_SCANS_PER_REMOTE_AUTHORITY,
    }
}

impl ScanSchedulerState {
    fn register_authority(&mut self, authority: &remote_fs::FilesystemIdentity) {
        if !self.authority_order.contains(authority) {
            self.authority_order.push_back(authority.clone());
        }
    }

    fn prune_authorities(&mut self) {
        self.authority_order.retain(|authority| {
            self.running_by_authority
                .get(authority)
                .copied()
                .unwrap_or(0)
                != 0
                || self.queue.has_where(|job| &job.authority == authority)
        });
        self.running_by_authority.retain(|authority, running| {
            *running != 0 || self.queue.has_where(|job| &job.authority == authority)
        });
    }

    fn evict_for_new_authority_interaction(
        &mut self,
        incoming: &remote_fs::FilesystemIdentity,
    ) -> Option<ScanJob> {
        let mut counts = std::collections::HashMap::new();
        for job in self.queue.lanes.iter().flatten() {
            if &job.authority != incoming {
                *counts.entry(job.authority.clone()).or_insert(0_usize) += 1;
            }
        }
        let victim = counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .max_by_key(|(_, count)| *count)
            .map(|(authority, _)| authority)?;
        self.queue.remove_newest_where(
            &[ScanPriority::Lazy, ScanPriority::Manual, ScanPriority::Root],
            |job| job.authority == victim,
        )
    }

    fn admit(&mut self, priority: ScanPriority, mut job: ScanJob) -> Result<Vec<ScanJob>, ()> {
        let authority = job.authority.clone();
        let authority_pending = self
            .queue
            .count_where(|queued| queued.authority == authority);
        let mut evicted = Vec::new();
        if authority_pending >= scan_authority_pending_limit(&authority) {
            if priority != ScanPriority::Lazy {
                if let Some(job) = self
                    .queue
                    .remove_newest_where(&[ScanPriority::Lazy], |queued| {
                        queued.authority == authority
                    })
                {
                    evicted.push(job);
                } else {
                    return Err(());
                }
            } else {
                return Err(());
            }
        }

        let incoming_has_no_pending = authority_pending == 0;
        let has_global_lazy = !self.queue.lanes[ScanPriority::Lazy.lane()].is_empty();
        if self.queue.len() == self.queue.capacity
            && priority != ScanPriority::Lazy
            && incoming_has_no_pending
            && !has_global_lazy
        {
            if let Some(job) = self.evict_for_new_authority_interaction(&authority) {
                evicted.push(job);
            }
        }

        job.queued_depth = self.queue.len();
        self.register_authority(&authority);
        match self.queue.push(priority, job) {
            Ok(Some(job)) => {
                evicted.push(job);
                Ok(evicted)
            }
            Ok(None) => Ok(evicted),
            Err(_) => Err(()),
        }
    }

    fn pop_next(&mut self) -> Option<ScanJob> {
        let authority_count = self.authority_order.len();
        for _ in 0..authority_count {
            let authority = self.authority_order.pop_front()?;
            self.authority_order.push_back(authority.clone());
            let running = self
                .running_by_authority
                .get(&authority)
                .copied()
                .unwrap_or(0);
            if running >= scan_authority_limit(&authority) {
                continue;
            }
            if let Some(job) = self.queue.pop_where(|job| job.authority == authority) {
                *self.running_by_authority.entry(authority).or_insert(0) += 1;
                return Some(job);
            }
        }
        None
    }

    fn finish(&mut self, authority: &remote_fs::FilesystemIdentity) {
        if let Some(running) = self.running_by_authority.get_mut(authority) {
            *running = running.saturating_sub(1);
        }
        self.prune_authorities();
    }
}

struct ScanScheduler {
    shared: Arc<(Mutex<ScanSchedulerState>, Condvar)>,
}

impl ScanScheduler {
    fn new(worker_count: usize, capacity: usize) -> Self {
        let shared = Arc::new((
            Mutex::new(ScanSchedulerState {
                queue: ScanQueue::new(capacity),
                authority_order: VecDeque::new(),
                running_by_authority: std::collections::HashMap::new(),
            }),
            Condvar::new(),
        ));
        for index in 0..worker_count {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name(format!("forge-file-tree-scan-{index}"))
                .spawn(move || scan_worker(shared))
                .expect("file-tree scan worker must start");
        }
        Self { shared }
    }

    fn global() -> &'static Self {
        static SCHEDULER: OnceLock<ScanScheduler> = OnceLock::new();
        SCHEDULER.get_or_init(|| Self::new(MAX_CONCURRENT_SCANS, MAX_PENDING_SCANS))
    }

    fn enqueue(&self, priority: ScanPriority, job: ScanJob) -> io::Result<()> {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let cancelled = state
            .queue
            .remove_where(|queued| queued.cancel.is_cancelled());
        let admission = state.admit(priority, job);
        let (evicted, error) = match admission {
            Ok(evicted) => (evicted, None),
            Err(()) => (
                Vec::new(),
                Some(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "directory scan admission limit reached",
                )),
            ),
        };
        if error.is_none() {
            wake.notify_all();
        }
        state.prune_authorities();
        drop(state);
        for job in cancelled {
            job.retire(remote_fs::cancelled_error());
        }
        for job in evicted {
            job.retire(io::Error::new(
                io::ErrorKind::WouldBlock,
                "directory scan was preempted to preserve interactive capacity",
            ));
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn retire_cancelled(&self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let cancelled = state
            .queue
            .remove_where(|queued| queued.cancel.is_cancelled());
        state.prune_authorities();
        drop(state);
        for job in cancelled {
            job.retire(remote_fs::cancelled_error());
        }
        wake.notify_all();
    }
}

fn scan_worker(shared: Arc<(Mutex<ScanSchedulerState>, Condvar)>) {
    loop {
        let job = {
            let (lock, wake) = &*shared;
            let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(job) = state.pop_next() {
                    break job;
                }
                state = wake
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let authority = job.authority.clone();
        job.run();
        let (lock, wake) = &*shared;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.finish(&authority);
        wake.notify_all();
    }
}

type FsOpJob = Box<dyn FnOnce() + Send + 'static>;

struct FsOpScheduler {
    shared: Arc<(Mutex<VecDeque<FsOpJob>>, Condvar)>,
    capacity: usize,
}

impl FsOpScheduler {
    fn new(worker_count: usize, capacity: usize) -> Self {
        let shared = Arc::new((Mutex::new(VecDeque::<FsOpJob>::new()), Condvar::new()));
        for index in 0..worker_count {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name(format!("forge-file-tree-op-{index}"))
                .spawn(move || fs_op_worker(shared))
                .expect("file-tree operation worker must start");
        }
        Self { shared, capacity }
    }

    fn global() -> &'static Self {
        static SCHEDULER: OnceLock<FsOpScheduler> = OnceLock::new();
        SCHEDULER.get_or_init(|| Self::new(MAX_CONCURRENT_FS_OPS, MAX_PENDING_FS_OPS))
    }

    fn enqueue(&self, job: FsOpJob) -> io::Result<()> {
        let (lock, wake) = &*self.shared;
        let mut queue = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if queue.len() >= self.capacity {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "file operation queue is full",
            ));
        }
        queue.push_back(job);
        wake.notify_one();
        Ok(())
    }
}

fn fs_op_worker(shared: Arc<(Mutex<VecDeque<FsOpJob>>, Condvar)>) {
    loop {
        let job = {
            let (lock, wake) = &*shared;
            let mut queue = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            while queue.is_empty() {
                queue = wake
                    .wait(queue)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            queue.pop_front().expect("non-empty file operation queue")
        };
        job();
    }
}

/// Poll a worker channel on the main loop and hand the outcome to `apply`
/// exactly once, including the worker-disconnected failure case.
fn poll_worker<T: 'static>(
    rx: mpsc::Receiver<io::Result<T>>,
    apply: impl FnOnce(io::Result<T>) + 'static,
    disconnected_message: &'static str,
) {
    let mut apply = Some(apply);
    glib::timeout_add_local(SCAN_POLL_INTERVAL, move || match rx.try_recv() {
        Ok(result) => {
            if let Some(apply) = apply.take() {
                apply(result);
            }
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            if let Some(apply) = apply.take() {
                apply(Err(io::Error::other(disconnected_message)));
            }
            glib::ControlFlow::Break
        }
    });
}

/// Scan `dir` on a worker thread for a snapshot of (location, hosts) and
/// deliver the entries through `apply` on the main loop.
fn request_dir_scan<F>(
    loc: FsLocation,
    hosts: Vec<RemoteHost>,
    overlay: FsExecutionOverlay,
    dir: PathBuf,
    cancel: remote_fs::CancelToken,
    priority: ScanPriority,
    apply: F,
) -> io::Result<()>
where
    F: FnOnce(io::Result<DirectoryScan>) + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    let authority = remote_fs::filesystem_identity(&loc, &hosts)?;
    ScanScheduler::global().enqueue(
        priority,
        ScanJob {
            authority,
            loc,
            hosts,
            overlay,
            dir,
            cancel,
            tx,
            enqueued_at: Instant::now(),
            queued_depth: 0,
        },
    )?;
    poll_worker(rx, apply, "file-tree scan worker disconnected");
    Ok(())
}

/// Run one blocking file operation (create/rename/delete/copy) on a worker
/// thread, bounded separately from scans, and deliver the outcome to `apply`.
fn request_fs_op<T, F, W>(work: W, apply: F) -> io::Result<()>
where
    T: Send + 'static,
    F: FnOnce(io::Result<T>) + 'static,
    W: FnOnce() -> io::Result<T> + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    FsOpScheduler::global().enqueue(Box::new(move || {
        let _ = tx.send(work());
    }))?;
    poll_worker(rx, apply, "file operation worker disconnected");
    Ok(())
}

fn append_entries(store: &gio::ListStore, entries: Vec<FileEntry>) {
    for entry in entries {
        store.append(&glib::BoxedAnyObject::new(entry));
    }
}

fn entry_from_row(row: &TreeListRow) -> Option<FileEntry> {
    let object = row.item()?;
    let boxed = object.downcast::<glib::BoxedAnyObject>().ok()?;
    let entry = boxed.try_borrow::<FileEntry>().ok()?;
    Some((*entry).clone())
}

/// Latest issued scan revision for each directory in the current tree
/// generation. The tree-wide generation rejects results from an older root or
/// location; this finer-grained revision also rejects an older refresh of the
/// same directory when two requests overlap on a slow remote connection.
#[derive(Clone)]
struct DirectoryScanRequest {
    revision: u64,
    cancel: remote_fs::CancelToken,
    pending: bool,
}

type DirectoryScanRevisions = std::collections::HashMap<PathBuf, DirectoryScanRequest>;

fn issue_directory_scan_revision(
    revisions: &mut DirectoryScanRevisions,
    dir: &Path,
) -> DirectoryScanRequest {
    let revision = revisions
        .get(dir)
        .map(|request| request.revision)
        .unwrap_or(0)
        .wrapping_add(1);
    if let Some(previous) = revisions.get(dir) {
        previous.cancel.cancel();
    }
    let request = DirectoryScanRequest {
        revision,
        cancel: remote_fs::CancelToken::default(),
        pending: true,
    };
    revisions.insert(dir.to_path_buf(), request.clone());
    request
}

fn directory_scan_revision_is_current(
    revisions: &DirectoryScanRevisions,
    dir: &Path,
    revision: u64,
) -> bool {
    revisions.get(dir).map(|request| request.revision) == Some(revision)
}

fn complete_directory_scan_revision(
    revisions: &mut DirectoryScanRevisions,
    dir: &Path,
    revision: u64,
) -> bool {
    let Some(request) = revisions.get_mut(dir) else {
        return false;
    };
    if request.revision != revision {
        return false;
    }
    request.pending = false;
    true
}

fn directory_scan_revision_is_pending(
    revisions: &DirectoryScanRevisions,
    dir: &Path,
    revision: u64,
) -> bool {
    revisions
        .get(dir)
        .is_some_and(|request| request.revision == revision && request.pending)
}

fn cancel_directory_scans(revisions: &DirectoryScanRevisions) {
    for request in revisions.values() {
        request.cancel.cancel();
    }
}

fn cached_materialized_store(
    root: &Path,
    dir: &Path,
    root_store: &gio::ListStore,
    child_stores: &std::collections::HashMap<PathBuf, glib::WeakRef<gio::ListStore>>,
) -> Option<gio::ListStore> {
    if dir == root {
        Some(root_store.clone())
    } else {
        child_stores.get(dir).and_then(glib::WeakRef::upgrade)
    }
}

type DirectoryFailureKey = (remote_fs::FilesystemIdentity, PathBuf);

fn committed_authority_matches(
    expected: Option<&remote_fs::FilesystemIdentity>,
    location: &FsLocation,
    hosts: &[RemoteHost],
) -> bool {
    expected.is_some_and(|expected| {
        remote_fs::filesystem_identity(location, hosts).is_ok_and(|current| &current == expected)
    })
}

fn location_home_probe_is_current(
    expected_guard: u64,
    current_guard: u64,
    expected_location: &FsLocation,
    expected_authority: &remote_fs::FilesystemIdentity,
    current_hosts: &[RemoteHost],
) -> bool {
    expected_guard == current_guard
        && remote_fs::filesystem_identity(expected_location, current_hosts)
            .is_ok_and(|current| &current == expected_authority)
}

/// Owns the root list, flattened tree model, and cancellation generation for the
/// current sidebar root. Cloning this value only clones the underlying GLib
/// objects and shared generation counter.
#[derive(Clone)]
pub(crate) struct FileTreeModel {
    root_store: gio::ListStore,
    tree_model: TreeListModel,
    /// The multi-selection the ListView runs on; stored here so context-menu
    /// batch operations and the filter wrap can read and swap its model.
    selection: gtk4::MultiSelection,
    /// Stable visibility layer over `tree_model`; combines the dotfile policy
    /// with the optional type-to-filter query.
    filter_model: gtk4::FilterListModel,
    filter: gtk4::CustomFilter,
    filter_state: Rc<RefCell<FilterState>>,
    /// Every still-live child store the lazy expansion factory has created,
    /// by parent path. Weak references let filtering/targeted refresh find
    /// GTK-owned stores without pinning subtrees that GTK has reclaimed.
    child_stores: Rc<RefCell<std::collections::HashMap<PathBuf, glib::WeakRef<gio::ListStore>>>>,
    /// Stable identity of the filesystem that produced the committed rows.
    /// If an index-backed profile changes before transactional navigation
    /// commits, old rows remain readable but no scan/mutation can reinterpret
    /// their paths against the new profile at that numeric index.
    committed_authority: Rc<RefCell<Option<remote_fs::FilesystemIdentity>>>,
    /// Completion time of the last successfully published snapshot per path.
    /// Error/refresh rows expose its age so retained content is never mistaken
    /// for a fresh remote listing.
    snapshot_completed: Rc<RefCell<std::collections::HashMap<PathBuf, SnapshotMeta>>>,
    directory_failures:
        Rc<RefCell<std::collections::HashMap<DirectoryFailureKey, DirectoryFailureState>>>,
    /// Paths a successful mutation wants selected after its parent listing is
    /// reconciled. The intent is consumed only by a successful refresh, so a
    /// transient remote failure does not lose it.
    selection_after_refresh: Rc<RefCell<std::collections::HashMap<PathBuf, Vec<PathBuf>>>>,
    /// Current root duplicated here so a Retry button created by the row
    /// factory can resolve the root store without capturing the whole UiState.
    root_path: Rc<RefCell<PathBuf>>,
    generation: Rc<Cell<u64>>,
    /// Per-directory latest-wins guard inside one tree generation. A manual
    /// refresh or mutation-triggered refresh supersedes an earlier expansion
    /// scan without clearing the store that is already on screen.
    directory_scan_revisions: Rc<RefCell<DirectoryScanRevisions>>,
    /// The row currently highlighted as an external-file drop target. Any
    /// listing reconciliation clears it before GTK can recycle a removed row
    /// widget for a surviving or newly inserted entry.
    drop_hover: Rc<RefCell<Option<gtk4::Widget>>>,
    /// Browsed filesystem, shared with UiState; scans snapshot it at request
    /// time and stale results are dropped when it has moved on.
    location: Rc<RefCell<FsLocation>>,
    /// Ephemeral connection material, snapshotted with every scan but kept
    /// outside `location` so it cannot affect target identity.
    execution_overlay: Rc<RefCell<FsExecutionOverlay>>,
    /// Host list source; each scan snapshots `remote_hosts` so a mid-scan
    /// config reload cannot redirect an in-flight listing at another host.
    config: Rc<RefCell<crate::config::Config>>,
}

/// Live type-to-filter state: the current query, the path set staying visible
/// (matches + ancestors), and which rows the filter auto-expanded (collapsed
/// again on clear; stale paths dropped silently).
#[derive(Default)]
struct FilterState {
    query: String,
    visible: std::collections::HashSet<PathBuf>,
    filter_expanded: std::collections::HashSet<PathBuf>,
    show_hidden: bool,
}

fn file_name_is_hidden(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

impl FileTreeModel {
    fn new(
        location: Rc<RefCell<FsLocation>>,
        execution_overlay: Rc<RefCell<FsExecutionOverlay>>,
        config: Rc<RefCell<crate::config::Config>>,
    ) -> Self {
        let root_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let generation = Rc::new(Cell::new(0_u64));
        let child_stores = Rc::new(RefCell::new(std::collections::HashMap::<
            PathBuf,
            glib::WeakRef<gio::ListStore>,
        >::new()));
        let snapshot_completed = Rc::new(RefCell::new(std::collections::HashMap::<
            PathBuf,
            SnapshotMeta,
        >::new()));
        let committed_authority = Rc::new(RefCell::new(None));
        let directory_failures = Rc::new(RefCell::new(std::collections::HashMap::new()));
        let selection_after_refresh = Rc::new(RefCell::new(std::collections::HashMap::new()));
        let root_path = Rc::new(RefCell::new(PathBuf::new()));
        let directory_scan_revisions = Rc::new(RefCell::new(DirectoryScanRevisions::new()));
        let drop_hover = Rc::new(RefCell::new(None));
        let filter_state = Rc::new(RefCell::new(FilterState::default()));
        let filter = gtk4::CustomFilter::new({
            let filter_state = filter_state.clone();
            move |object| {
                let state = filter_state.borrow();
                let Some(row) = object.downcast_ref::<TreeListRow>() else {
                    return true;
                };
                let Some(entry) = entry_from_row(row) else {
                    return false;
                };
                if !entry.is_item() {
                    return true;
                }
                (state.show_hidden || !file_name_is_hidden(&entry.name))
                    && (state.query.is_empty() || state.visible.contains(&entry.path))
            }
        });
        let tree_model = TreeListModel::new(root_store.clone(), false, false, {
            let generation = generation.clone();
            let location = location.clone();
            let execution_overlay = execution_overlay.clone();
            let config = config.clone();
            let child_stores = child_stores.clone();
            let snapshot_completed = snapshot_completed.clone();
            let committed_authority = committed_authority.clone();
            let directory_failures = directory_failures.clone();
            let selection_after_refresh = selection_after_refresh.clone();
            let directory_scan_revisions = directory_scan_revisions.clone();
            // A scan completing while the filter is active re-evaluates the
            // visible set so user-expanded subtrees get filtered too. These
            // captures point only "downward" (stores, plain state, filter),
            // so nothing here creates a reference cycle with the model.
            let filter_state = filter_state.clone();
            let filter = filter.clone();
            let root_store_for_filter = root_store.clone();
            move |object| {
                let boxed = object.downcast_ref::<glib::BoxedAnyObject>()?;
                let entry = boxed.try_borrow::<FileEntry>().ok()?;
                if !entry.is_dir {
                    return None;
                }
                let path = entry.path.clone();
                drop(entry);
                let scan_location = location.borrow().clone();
                let scan_overlay = execution_overlay.borrow().clone();
                let scan_hosts = config.borrow().remote_hosts.clone();
                if !committed_authority_matches(
                    committed_authority.borrow().as_ref(),
                    &scan_location,
                    &scan_hosts,
                ) {
                    return None;
                }

                let children = gio::ListStore::new::<glib::BoxedAnyObject>();
                {
                    let mut map = child_stores.borrow_mut();
                    map.retain(|_, store| store.upgrade().is_some());
                    map.insert(path.clone(), children.downgrade());
                }
                set_directory_status(&children, &path, DirectoryRowStatus::Loading);
                let scan_request = issue_directory_scan_revision(
                    &mut directory_scan_revisions.borrow_mut(),
                    &path,
                );
                let scan_revision = scan_request.revision;
                let children_for_scan = children.clone();
                let generation_for_scan = generation.clone();
                let expected_generation = generation.get();
                let location_for_scan = location.clone();
                let overlay_for_scan = execution_overlay.clone();
                let path_for_result = path.clone();
                let path_for_error = path.clone();
                let filter_state_for_scan = filter_state.clone();
                let filter_for_scan = filter.clone();
                let root_store_for_filter = root_store_for_filter.clone();
                let child_stores_for_filter = child_stores.clone();
                let snapshots_for_scan = snapshot_completed.clone();
                let revisions_for_scan = directory_scan_revisions.clone();
                let selection_for_scan = selection_after_refresh.clone();
                let failures_for_scan = directory_failures.clone();
                if let Err(error) = request_dir_scan(
                    scan_location.clone(),
                    scan_hosts,
                    scan_overlay.clone(),
                    path,
                    scan_request.cancel.clone(),
                    ScanPriority::Lazy,
                    move |result| {
                        if generation_for_scan.get() != expected_generation {
                            return;
                        }
                        if *location_for_scan.borrow() != scan_location {
                            return;
                        }
                        if *overlay_for_scan.borrow() != scan_overlay {
                            return;
                        }
                        if !directory_scan_revision_is_current(
                            &revisions_for_scan.borrow(),
                            &path_for_result,
                            scan_revision,
                        ) {
                            return;
                        }
                        complete_directory_scan_revision(
                            &mut revisions_for_scan.borrow_mut(),
                            &path_for_result,
                            scan_revision,
                        );
                        match result {
                            Ok(listing) => {
                                let timing = listing.timing;
                                let reconcile_started = Instant::now();
                                snapshots_for_scan
                                    .borrow_mut()
                                    .insert(path_for_result.clone(), SnapshotMeta::now());
                                if listing.truncated {
                                    log::warn!(
                                        "directory {} has more than {} entries; showing a bounded prefix",
                                        path_for_result.display(),
                                        remote_fs::MAX_DIRECTORY_ENTRIES
                                    );
                                }
                                let delta =
                                    update_store_in_place(&children_for_scan, listing.entries);
                                invalidate_removed_subtrees_parts(
                                    &delta.removed_directories,
                                    &child_stores_for_filter,
                                    &snapshots_for_scan,
                                    &revisions_for_scan,
                                    &selection_for_scan,
                                    &failures_for_scan,
                                );
                                log_scan_timing(
                                    &path_for_result,
                                    timing,
                                    reconcile_started.elapsed(),
                                    &delta,
                                );
                                reapply_filter_parts(
                                    &root_store_for_filter,
                                    &child_stores_for_filter,
                                    &filter_state_for_scan,
                                    &filter_for_scan,
                                );
                            }
                            Err(error) => {
                                if error.kind() != io::ErrorKind::Interrupted {
                                    log::warn!(
                                        "failed to scan directory {}: {error}",
                                        path_for_result.display()
                                    );
                                    set_directory_status(
                                        &children_for_scan,
                                        &path_for_result,
                                        directory_error_status(
                                            &error,
                                            snapshots_for_scan
                                                .borrow()
                                                .get(&path_for_result)
                                                .map(|snapshot| snapshot.completed_wall),
                                        ),
                                    );
                                    reapply_filter_parts(
                                        &root_store_for_filter,
                                        &child_stores_for_filter,
                                        &filter_state_for_scan,
                                        &filter_for_scan,
                                    );
                                }
                            }
                        }
                    },
                ) {
                    complete_directory_scan_revision(
                        &mut directory_scan_revisions.borrow_mut(),
                        &path_for_error,
                        scan_revision,
                    );
                    log::warn!(
                        "failed to start directory scan for {}: {error}",
                        path_for_error.display()
                    );
                    set_directory_status(
                        &children,
                        &path_for_error,
                        directory_error_status(
                            &error,
                            snapshot_completed
                                .borrow()
                                .get(&path_for_error)
                                .map(|snapshot| snapshot.completed_wall),
                        ),
                    );
                }

                Some(children.upcast())
            }
        });

        // The filter wrap consults a precomputed visible-path set (matches +
        // ancestors) plus the dotfile preference, so TreeListRow identity —
        // and with it expansion state — is untouched by either filter.
        let filter_model =
            gtk4::FilterListModel::new(Some(tree_model.clone()), Some(filter.clone()));
        let selection = gtk4::MultiSelection::new(Some(filter_model.clone()));
        // MultiSelection never autoselects; ctrl+click toggles and
        // shift+click ranges are built in.

        Self {
            root_store,
            tree_model,
            selection,
            filter_model,
            filter,
            filter_state,
            child_stores,
            committed_authority,
            snapshot_completed,
            directory_failures,
            selection_after_refresh,
            root_path,
            generation,
            directory_scan_revisions,
            drop_hover,
            location,
            execution_overlay,
            config,
        }
    }

    fn reset(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.selection.unselect_all();
        self.root_store.remove_all();
        self.child_stores.borrow_mut().clear();
        *self.committed_authority.borrow_mut() = None;
        self.snapshot_completed.borrow_mut().clear();
        self.directory_failures.borrow_mut().clear();
        self.selection_after_refresh.borrow_mut().clear();
        self.root_path.borrow_mut().clear();
        cancel_directory_scans(&self.directory_scan_revisions.borrow());
        ScanScheduler::global().retire_cancelled();
        self.directory_scan_revisions.borrow_mut().clear();
        set_drop_hover(&self.drop_hover, None);
        generation
    }

    fn set_committed_authority(&self, authority: remote_fs::FilesystemIdentity) {
        *self.committed_authority.borrow_mut() = Some(authority);
    }

    fn committed_authority_is_current(&self) -> bool {
        committed_authority_matches(
            self.committed_authority.borrow().as_ref(),
            &self.location.borrow(),
            &self.config.borrow().remote_hosts,
        )
    }

    fn cancel_pending_scans_preserve_tree(&self) {
        cancel_directory_scans(&self.directory_scan_revisions.borrow());
        ScanScheduler::global().retire_cancelled();
        self.directory_scan_revisions.borrow_mut().clear();
    }

    fn issue_directory_scan(&self, dir: &Path) -> DirectoryScanRequest {
        issue_directory_scan_revision(&mut self.directory_scan_revisions.borrow_mut(), dir)
    }

    fn directory_scan_is_current(&self, dir: &Path, revision: u64) -> bool {
        directory_scan_revision_is_current(&self.directory_scan_revisions.borrow(), dir, revision)
    }

    fn complete_directory_scan(&self, dir: &Path, revision: u64) -> bool {
        complete_directory_scan_revision(
            &mut self.directory_scan_revisions.borrow_mut(),
            dir,
            revision,
        )
    }

    fn materialized_entries_are_current(&self, expected: &[FileEntry]) -> bool {
        if expected.is_empty() {
            return true;
        }
        entries_remain_current(expected, &self.materialized_entries())
    }

    fn materialized_entries(&self) -> Vec<FileEntry> {
        (0..self.tree_model.n_items())
            .filter_map(|position| self.tree_model.row(position))
            .filter_map(|row| entry_from_row(&row))
            .filter(FileEntry::is_item)
            .collect()
    }

    fn selected_entries_snapshot(&self) -> Vec<FileEntry> {
        self.selected_entries()
            .into_iter()
            .map(|(_, entry)| entry)
            .collect()
    }

    fn reconcile_selection(&self, selected_before: &[FileEntry], preferred: Option<&[PathBuf]>) {
        let current = self.materialized_entries();
        let survivors = selection_paths_after_reconcile(selected_before, &current, preferred);
        self.selection.unselect_all();
        for path in survivors {
            if let Some(position) = self.flat_position_of(&path) {
                self.selection.select_item(position, false);
            }
        }
    }

    fn materialized_directory_is_current(&self, root: &Path, dir: &Path) -> bool {
        dir == root
            || self.materialized_entries_are_current(&[FileEntry {
                name: String::new(),
                path: dir.to_path_buf(),
                is_dir: true,
                status: None,
            }])
    }

    fn set_root_path(&self, root: &Path) {
        *self.root_path.borrow_mut() = root.to_path_buf();
    }

    fn last_good_snapshot(&self, dir: &Path) -> Option<SystemTime> {
        self.snapshot_completed
            .borrow()
            .get(dir)
            .map(|snapshot| snapshot.completed_wall)
    }

    fn mark_snapshot_completed(&self, dir: &Path) {
        self.snapshot_completed
            .borrow_mut()
            .insert(dir.to_path_buf(), SnapshotMeta::now());
    }

    fn snapshot_is_stale(&self, dir: &Path, now: Instant) -> bool {
        self.snapshot_completed
            .borrow()
            .get(dir)
            .copied()
            .is_some_and(|snapshot| snapshot_meta_is_stale(snapshot, now))
    }

    fn directory_scan_is_pending(&self, dir: &Path) -> bool {
        self.directory_scan_revisions
            .borrow()
            .get(dir)
            .is_some_and(|request| request.pending)
    }

    fn visible_stale_directories(&self, now: Instant, limit: usize) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let root = self.root_path.borrow().clone();
        if !root.as_os_str().is_empty()
            && self.snapshot_is_stale(&root, now)
            && !self.directory_scan_is_pending(&root)
        {
            paths.push(root);
        }
        for index in 0..self.tree_model.n_items() {
            if paths.len() == limit {
                break;
            }
            let Some(row) = self.tree_model.row(index) else {
                continue;
            };
            let Some(entry) = entry_from_row(&row) else {
                continue;
            };
            if entry.is_item()
                && entry.is_dir
                && row.is_expanded()
                && self.snapshot_is_stale(&entry.path, now)
                && !self.directory_scan_is_pending(&entry.path)
                && !paths.contains(&entry.path)
            {
                paths.push(entry.path);
            }
        }
        paths
    }

    fn request_selection_after_refresh(&self, dir: &Path, paths: Vec<PathBuf>) {
        let root = self.root_path.borrow().clone();
        if self.materialized_children_of(&root, dir).is_none() {
            return;
        }
        self.selection_after_refresh
            .borrow_mut()
            .insert(dir.to_path_buf(), paths);
    }

    fn invalidate_removed_subtrees(&self, removed: &[PathBuf]) {
        invalidate_removed_subtrees_parts(
            removed,
            &self.child_stores,
            &self.snapshot_completed,
            &self.directory_scan_revisions,
            &self.selection_after_refresh,
            &self.directory_failures,
        );
    }

    fn set_root_status(&self, generation: u64, root: &Path, status: DirectoryRowStatus) -> bool {
        if self.generation.get() != generation || *self.root_path.borrow() != root {
            return false;
        }
        set_directory_status(&self.root_store, root, status);
        self.reapply_filter();
        true
    }

    fn replace_root(&self, generation: u64, entries: Vec<FileEntry>) -> bool {
        if self.generation.get() != generation {
            return false;
        }
        self.root_store.remove_all();
        append_entries(&self.root_store, entries);
        self.reapply_filter();
        true
    }

    /// Re-evaluate an active filter after directory contents changed:
    /// recompute the visible set from the now-loaded stores and re-filter.
    /// No-op while no query is active.
    fn reapply_filter(&self) {
        reapply_filter_parts(
            &self.root_store,
            &self.child_stores,
            &self.filter_state,
            &self.filter,
        );
    }

    /// The currently materialized ListStore holding `dir`'s children: the
    /// root store when `dir` is the tree root, else the child store of an
    /// expanded or previously expanded row for `dir`. A collapsed cached
    /// directory remains refreshable; only never-materialized rows return
    /// `None`.
    fn materialized_children_of(&self, root: &Path, dir: &Path) -> Option<gio::ListStore> {
        if let Some(store) =
            cached_materialized_store(root, dir, &self.root_store, &self.child_stores.borrow())
        {
            return Some(store);
        }
        for index in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_model.row(index) else {
                continue;
            };
            let Some(entry) = entry_from_row(&row) else {
                continue;
            };
            if entry.is_item() && entry.path == dir {
                return row
                    .children()
                    .and_then(|model| model.downcast::<gio::ListStore>().ok());
            }
        }
        None
    }

    /// Re-list one already-materialized directory with a visible
    /// stale-while-revalidate state row. A retry button can call this directly
    /// through the model, without retaining/cycling the surrounding UiState.
    fn refresh_directory(&self, dir: &Path, toast_overlay: Option<adw::ToastOverlay>) -> bool {
        self.refresh_directory_with_cause(dir, toast_overlay, DirectoryRefreshCause::Manual)
    }

    fn retry_directory(&self, dir: &Path) -> bool {
        self.refresh_directory_with_cause(dir, None, DirectoryRefreshCause::Retry)
    }

    fn refresh_directory_with_cause(
        &self,
        dir: &Path,
        toast_overlay: Option<adw::ToastOverlay>,
        cause: DirectoryRefreshCause,
    ) -> bool {
        if !self.committed_authority_is_current() {
            return false;
        }
        let root = self.root_path.borrow().clone();
        let Some(store) = self.materialized_children_of(&root, dir) else {
            return false;
        };

        let location = self.location.borrow().clone();
        let hosts = self.config.borrow().remote_hosts.clone();
        let overlay = self.execution_overlay.borrow().clone();
        let authority = match remote_fs::filesystem_identity(&location, &hosts) {
            Ok(authority) => authority,
            Err(error) => {
                set_directory_status(
                    &store,
                    dir,
                    directory_error_status(&error, self.last_good_snapshot(dir)),
                );
                return true;
            }
        };
        let failure_key = (authority, dir.to_path_buf());
        if let Some(remaining) = directory_refresh_cooldown(
            cause,
            &self.directory_failures.borrow(),
            &failure_key,
            Instant::now(),
        ) {
            set_directory_status(
                &store,
                dir,
                DirectoryRowStatus::Error {
                    message: retry_wait_label(remaining),
                    last_good: self.last_good_snapshot(dir),
                },
            );
            self.reapply_filter();
            return true;
        }
        let generation = self.generation.get();
        let scan_request = self.issue_directory_scan(dir);
        let scan_revision = scan_request.revision;
        set_directory_status(
            &store,
            dir,
            DirectoryRowStatus::Refreshing {
                last_good: self.last_good_snapshot(dir),
            },
        );
        self.reapply_filter();

        let generation_for_scan = self.generation.clone();
        let location_for_scan = self.location.clone();
        let overlay_for_scan = self.execution_overlay.clone();
        let scan_location = location.clone();
        let scan_overlay = overlay.clone();
        let model_for_refresh = self.clone();
        let failure_key_for_scan = failure_key.clone();
        let dir_for_result = dir.to_path_buf();
        let store_for_scan = store.clone();
        let request = request_dir_scan(
            location,
            hosts,
            overlay,
            dir.to_path_buf(),
            scan_request.cancel.clone(),
            cause.priority(),
            move |result| {
                if generation_for_scan.get() != generation
                    || *location_for_scan.borrow() != scan_location
                    || *overlay_for_scan.borrow() != scan_overlay
                    || !model_for_refresh.directory_scan_is_current(&dir_for_result, scan_revision)
                {
                    return;
                }
                model_for_refresh.complete_directory_scan(&dir_for_result, scan_revision);
                match result {
                    Ok(listing) => {
                        let timing = listing.timing;
                        let reconcile_started = Instant::now();
                        model_for_refresh
                            .directory_failures
                            .borrow_mut()
                            .remove(&failure_key_for_scan);
                        model_for_refresh.mark_snapshot_completed(&dir_for_result);
                        if listing.truncated {
                            log::warn!(
                                "directory {} has more than {} entries; showing a bounded prefix",
                                dir_for_result.display(),
                                remote_fs::MAX_DIRECTORY_ENTRIES
                            );
                            if let Some(toast_overlay) = &toast_overlay {
                                let path = root_display_label(&scan_location, &dir_for_result);
                                toast_overlay.add_toast(adw::Toast::new(&format!(
                                    "{path} has more than {} entries; showing the first {}",
                                    remote_fs::MAX_DIRECTORY_ENTRIES,
                                    remote_fs::MAX_DIRECTORY_ENTRIES
                                )));
                            }
                        }
                        let selected_before = model_for_refresh.selected_entries_snapshot();
                        let preferred = model_for_refresh
                            .selection_after_refresh
                            .borrow_mut()
                            .remove(&dir_for_result);
                        set_drop_hover(&model_for_refresh.drop_hover, None);
                        let delta = update_store_in_place(&store_for_scan, listing.entries);
                        model_for_refresh.invalidate_removed_subtrees(&delta.removed_directories);
                        model_for_refresh
                            .reconcile_selection(&selected_before, preferred.as_deref());
                        log_scan_timing(
                            &dir_for_result,
                            timing,
                            reconcile_started.elapsed(),
                            &delta,
                        );
                        model_for_refresh.reapply_filter();
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        record_directory_failure(
                            &mut model_for_refresh.directory_failures.borrow_mut(),
                            failure_key_for_scan.clone(),
                            &error,
                            Instant::now(),
                        );
                        log::warn!(
                            "failed to refresh directory {}: {error}",
                            dir_for_result.display()
                        );
                        set_directory_status(
                            &store_for_scan,
                            &dir_for_result,
                            directory_error_status(
                                &error,
                                model_for_refresh.last_good_snapshot(&dir_for_result),
                            ),
                        );
                        model_for_refresh.reapply_filter();
                    }
                }
            },
        );
        if let Err(error) = request {
            self.complete_directory_scan(dir, scan_revision);
            record_directory_failure(
                &mut self.directory_failures.borrow_mut(),
                failure_key,
                &error,
                Instant::now(),
            );
            log::warn!(
                "failed to start directory refresh for {}: {error}",
                dir.display()
            );
            set_directory_status(
                &store,
                dir,
                directory_error_status(&error, self.last_good_snapshot(dir)),
            );
            self.reapply_filter();
        }
        true
    }

    fn row_entry(&self, position: u32) -> Option<(TreeListRow, FileEntry)> {
        // Read through the stable filtered selection model; positions always
        // index the same FilterListModel even when its predicate changes.
        let model = self.selection.model()?;
        let row = model.item(position)?.downcast::<TreeListRow>().ok()?;
        let entry = entry_from_row(&row)?;
        Some((row, entry))
    }

    /// The selected entries in flat-model order, with their positions in the
    /// selection's current model.
    fn selected_entries(&self) -> Vec<(u32, FileEntry)> {
        let bitset = self.selection.selection();
        let mut positions = Vec::new();
        if let Some((iter, first)) = gtk4::BitsetIter::init_first(&bitset) {
            positions.push(first);
            positions.extend(iter);
        }
        positions
            .into_iter()
            .filter_map(|position| {
                self.row_entry(position)
                    .filter(|(_, entry)| entry.is_item())
                    .map(|(_, entry)| (position, entry))
            })
            .collect()
    }

    /// The position of `path` in the selection's current model, if visible.
    fn flat_position_of(&self, path: &Path) -> Option<u32> {
        let model = self.selection.model()?;
        for index in 0..model.n_items() {
            let Some(row) = model
                .item(index)
                .and_then(|item| item.downcast::<TreeListRow>().ok())
            else {
                continue;
            };
            if entry_from_row(&row).is_some_and(|entry| entry.is_item() && entry.path == path) {
                return Some(index);
            }
        }
        None
    }

    /// Apply the type-to-filter query: recompute the visible path set from
    /// the LOADED stores only (never a new scan), auto-expand materialized
    /// ancestors of matches, and re-evaluate the stable filter wrap. An empty
    /// query collapses exactly the rows the name filter expanded.
    fn apply_filter(&self, query: String) {
        if query.is_empty() {
            let expanded = {
                let mut state = self.filter_state.borrow_mut();
                state.query.clear();
                state.visible.clear();
                std::mem::take(&mut state.filter_expanded)
            };
            // Restore expansion: collapse the rows the filter opened. Rows
            // whose path vanished meanwhile are dropped silently.
            for path in expanded {
                if let Some(position) = self.flat_position_of(&path) {
                    if let Some((row, _)) = self.row_entry(position) {
                        row.set_expanded(false);
                    }
                }
            }
            self.filter.changed(gtk4::FilterChange::Different);
            return;
        }
        // Compute the new visible set and publish it before touching the
        // model: expansion and `emit_changed` re-enter the filter closure,
        // which borrows `filter_state` — so no borrow may be held here.
        let visible = {
            let roots = store_entries(&self.root_store);
            let visible = {
                let child_stores = self.child_stores.borrow();
                collect_visible_paths(
                    &roots,
                    &|path| {
                        child_stores
                            .get(path)
                            .and_then(glib::WeakRef::upgrade)
                            .map(|store| store_entries(&store))
                    },
                    &query,
                )
            };
            let mut state = self.filter_state.borrow_mut();
            state.query = query;
            state.visible = visible.clone();
            visible
        };

        // Auto-expand ancestors of matches — but only rows whose children are
        // already materialized, so filtering never triggers a fresh scan.
        let mut newly_expanded = Vec::new();
        for index in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_model.row(index) else {
                continue;
            };
            let Some(entry) = entry_from_row(&row) else {
                continue;
            };
            if !entry.is_item() || !entry.is_dir || !visible.contains(&entry.path) {
                continue;
            }
            if !row.is_expanded() && row.children().is_some() {
                row.set_expanded(true);
                newly_expanded.push(entry.path.clone());
            }
        }
        self.filter_state
            .borrow_mut()
            .filter_expanded
            .extend(newly_expanded);

        self.filter.changed(gtk4::FilterChange::Different);
    }

    /// Whether the type-to-filter query is currently active.
    fn filter_is_active(&self) -> bool {
        !self.filter_state.borrow().query.is_empty()
    }

    /// Toggle dot-prefixed rows over the materialized models. This never
    /// scans, resets the root, or changes remote/file-operation authority.
    pub(crate) fn set_show_hidden(&self, show_hidden: bool) {
        {
            let mut state = self.filter_state.borrow_mut();
            if state.show_hidden == show_hidden {
                return;
            }
            state.show_hidden = show_hidden;
        }
        self.filter.changed(gtk4::FilterChange::Different);
    }
}

fn public_directory_error_message(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "Directory not found or unavailable",
        io::ErrorKind::PermissionDenied => "Permission denied",
        io::ErrorKind::TimedOut => "Connection timed out",
        io::ErrorKind::WouldBlock => "Too many directory scans are pending; retry",
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
            "The remote returned an invalid directory response"
        }
        io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::UnexpectedEof => "Remote connection failed",
        _ => "Directory could not be loaded",
    }
}

fn public_file_operation_error_message(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::AlreadyExists => "An item with this name already exists",
        io::ErrorKind::NotFound => "The item no longer exists",
        io::ErrorKind::PermissionDenied => "Permission denied",
        io::ErrorKind::TimedOut => "The remote operation timed out",
        io::ErrorKind::WouldBlock => "Too many file operations are pending; retry",
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
            "The operation or remote response was invalid"
        }
        io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::UnexpectedEof => "Remote connection failed",
        _ => "The operation could not be completed",
    }
}

fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn directory_error_status(error: &io::Error, last_good: Option<SystemTime>) -> DirectoryRowStatus {
    // Raw probe/ssh stderr may contain endpoints, option values or other
    // sensitive diagnostics. It remains in bounded logs; the tree gets only a
    // stable category, which is valid UTF-8 and bounded by construction.
    DirectoryRowStatus::Error {
        message: safe_file_label(public_directory_error_message(error)),
        last_good,
    }
}

fn directory_failure_remaining(
    failures: &std::collections::HashMap<DirectoryFailureKey, DirectoryFailureState>,
    key: &DirectoryFailureKey,
    now: Instant,
) -> Option<Duration> {
    failures
        .get(key)
        .and_then(|failure| failure.retry_not_before.checked_duration_since(now))
        .filter(|remaining| !remaining.is_zero())
}

fn directory_refresh_cooldown(
    cause: DirectoryRefreshCause,
    failures: &std::collections::HashMap<DirectoryFailureKey, DirectoryFailureState>,
    key: &DirectoryFailureKey,
    now: Instant,
) -> Option<Duration> {
    if cause == DirectoryRefreshCause::Retry {
        None
    } else {
        directory_failure_remaining(failures, key, now)
    }
}

fn record_directory_failure(
    failures: &mut std::collections::HashMap<DirectoryFailureKey, DirectoryFailureState>,
    key: DirectoryFailureKey,
    error: &io::Error,
    now: Instant,
) {
    if matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    ) {
        return;
    }
    let next = next_directory_failure_state(failures.get(&key).copied(), error, now);
    failures.insert(key, next);
}

fn retry_wait_label(remaining: Duration) -> String {
    let seconds = remaining
        .as_secs()
        .saturating_add(u64::from(remaining.subsec_nanos() != 0));
    format!("Retry available in {seconds}s")
}

/// Replace only the transient state row; every real child retains its object
/// identity and remains usable while a refresh runs or after it fails.
fn set_directory_status(store: &gio::ListStore, dir: &Path, status: DirectoryRowStatus) {
    clear_directory_status(store);
    store.append(&glib::BoxedAnyObject::new(FileEntry::directory_status(
        dir, status,
    )));
}

fn clear_directory_status(store: &gio::ListStore) {
    let mut index = store.n_items();
    while index > 0 {
        index -= 1;
        let is_status = store
            .item(index)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .is_some_and(|boxed| {
                boxed
                    .try_borrow::<FileEntry>()
                    .is_ok_and(|entry| !entry.is_item())
            });
        if is_status {
            store.remove(index);
        }
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
struct StoreReconcileDelta {
    removed_directories: Vec<PathBuf>,
    removed_rows: usize,
    inserted_rows: usize,
}

fn path_is_in_removed_subtree(path: &Path, removed: &[PathBuf]) -> bool {
    removed.iter().any(|root| path.starts_with(root))
}

fn invalidate_removed_subtrees_parts(
    removed: &[PathBuf],
    child_stores: &Rc<RefCell<std::collections::HashMap<PathBuf, glib::WeakRef<gio::ListStore>>>>,
    snapshots: &Rc<RefCell<std::collections::HashMap<PathBuf, SnapshotMeta>>>,
    revisions: &Rc<RefCell<DirectoryScanRevisions>>,
    selection_after_refresh: &Rc<RefCell<std::collections::HashMap<PathBuf, Vec<PathBuf>>>>,
    failures: &Rc<RefCell<std::collections::HashMap<DirectoryFailureKey, DirectoryFailureState>>>,
) {
    if removed.is_empty() {
        return;
    }
    {
        let mut revisions = revisions.borrow_mut();
        revisions.retain(|path, request| {
            let keep = !path_is_in_removed_subtree(path, removed);
            if !keep {
                request.cancel.cancel();
            }
            keep
        });
    }
    // Cancelled queued work is removed immediately instead of consuming one
    // of the bounded queue's physical slots until a worker happens to pop it.
    ScanScheduler::global().retire_cancelled();
    child_stores
        .borrow_mut()
        .retain(|path, _| !path_is_in_removed_subtree(path, removed));
    snapshots
        .borrow_mut()
        .retain(|path, _| !path_is_in_removed_subtree(path, removed));
    selection_after_refresh
        .borrow_mut()
        .retain(|path, _| !path_is_in_removed_subtree(path, removed));
    failures
        .borrow_mut()
        .retain(|(_, path), _| !path_is_in_removed_subtree(path, removed));
}

/// Replace a store's contents with the minimal set of removals/insertions so
/// surviving rows keep their `TreeListRow` identity — and with it their
/// expansion state and cached child models. Both the old and new contents
/// are sorted by the same comparator, so after vanished paths are removed
/// the survivors already stand at their final positions and newcomers slot
/// in at their sorted index. A path whose name/type changed is deliberately
/// replaced so a directory that became a symlink cannot retain expandable
/// state. This is what lets a mutation refresh exactly one directory without
/// collapsing unrelated expansion anywhere in the tree.
fn update_store_in_place(store: &gio::ListStore, entries: Vec<FileEntry>) -> StoreReconcileDelta {
    clear_directory_status(store);
    let mut delta = StoreReconcileDelta::default();
    let store_entry = |index: u32| -> Option<FileEntry> {
        store
            .item(index)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .and_then(|boxed| {
                boxed
                    .try_borrow::<FileEntry>()
                    .ok()
                    .map(|entry| (*entry).clone())
            })
    };
    let new_entries: std::collections::HashMap<&Path, (&str, bool)> = entries
        .iter()
        .filter(|entry| entry.is_item())
        .map(|entry| (entry.path.as_path(), (entry.name.as_str(), entry.is_dir)))
        .collect();

    // Remove vanished rows back-to-front so earlier indices stay valid.
    let mut index = store.n_items();
    while index > 0 {
        index -= 1;
        let old_entry = store_entry(index);
        let keep = old_entry.as_ref().is_some_and(|entry| {
            new_entries
                .get(entry.path.as_path())
                .is_some_and(|(name, is_dir)| *name == entry.name && *is_dir == entry.is_dir)
        });
        if !keep {
            if let Some(entry) = old_entry.filter(|entry| entry.is_item() && entry.is_dir) {
                delta.removed_directories.push(entry.path);
            }
            store.remove(index);
            delta.removed_rows += 1;
        }
    }

    let survivors: std::collections::HashSet<PathBuf> = (0..store.n_items())
        .filter_map(store_entry)
        .map(|entry| entry.path)
        .collect();
    for (position, entry) in entries.into_iter().enumerate() {
        if !survivors.contains(&entry.path) {
            store.insert(position as u32, &glib::BoxedAnyObject::new(entry));
            delta.inserted_rows += 1;
        }
    }
    delta
}

fn log_scan_timing(
    dir: &Path,
    timing: ScanTiming,
    reconcile_elapsed: Duration,
    delta: &StoreReconcileDelta,
) {
    let path = safe_file_label(&dir.to_string_lossy());
    let total = timing
        .queued_for
        .saturating_add(timing.listed_for)
        .saturating_add(reconcile_elapsed);
    if scan_timing_is_slow(timing, reconcile_elapsed) {
        log::warn!(
            "slow file-tree scan path={path:?} queue={:?} list={:?} reconcile={:?} depth={} inserted={} removed={} total={:?}",
            timing.queued_for,
            timing.listed_for,
            reconcile_elapsed,
            timing.queued_depth,
            delta.inserted_rows,
            delta.removed_rows,
            total
        );
    } else {
        log::debug!(
            "file-tree scan path={path:?} queue={:?} list={:?} reconcile={:?} depth={} inserted={} removed={}",
            timing.queued_for,
            timing.listed_for,
            reconcile_elapsed,
            timing.queued_depth,
            delta.inserted_rows,
            delta.removed_rows
        );
    }
}

fn scan_timing_is_slow(timing: ScanTiming, reconcile_elapsed: Duration) -> bool {
    timing.queued_for >= Duration::from_secs(1)
        || timing.listed_for >= Duration::from_secs(2)
        || reconcile_elapsed >= Duration::from_millis(100)
}

/// Flatten one store into (name, path, is_dir) triples — the shape the
/// filter's pure matching core consumes.
fn store_entries(store: &gio::ListStore) -> Vec<FilterNode> {
    (0..store.n_items())
        .filter_map(|index| {
            store
                .item(index)
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                .and_then(|boxed| {
                    boxed
                        .try_borrow::<FileEntry>()
                        .ok()
                        .filter(|entry| entry.is_item())
                        .map(|entry| (entry.name.clone(), entry.path.clone(), entry.is_dir))
                })
        })
        .collect()
}

/// One node of the loaded tree as the filter sees it: name, path, is_dir.
type FilterNode = (String, PathBuf, bool);
/// Yields the LOADED children of a directory path (`None` when its store was
/// never materialized), so filtering never scans.
type FilterChildrenOf<'a> = dyn Fn(&Path) -> Option<Vec<FilterNode>> + 'a;

/// The filter's pure core, separate for tests: the paths that stay visible
/// for `query` — case-insensitive substring matches on names, plus all their
/// ancestors. An empty query matches everything, i.e. the identity filter.
fn collect_visible_paths(
    roots: &[FilterNode],
    children_of: &FilterChildrenOf,
    query: &str,
) -> std::collections::HashSet<PathBuf> {
    let query = query.to_lowercase();
    let mut visible = std::collections::HashSet::new();
    let mut ancestors: Vec<PathBuf> = Vec::new();
    collect_visible_into(roots, children_of, &query, &mut ancestors, &mut visible);
    visible
}

fn collect_visible_into(
    entries: &[FilterNode],
    children_of: &FilterChildrenOf,
    query: &str,
    ancestors: &mut Vec<PathBuf>,
    visible: &mut std::collections::HashSet<PathBuf>,
) {
    for (name, path, is_dir) in entries {
        if name.to_lowercase().contains(query) {
            visible.insert(path.clone());
            visible.extend(ancestors.iter().cloned());
        }
        if *is_dir {
            if let Some(children) = children_of(path) {
                ancestors.push(path.clone());
                collect_visible_into(&children, children_of, query, ancestors, visible);
                ancestors.pop();
            }
        }
    }
}

/// Re-evaluate an active filter after new directory contents landed:
/// recompute the visible set from the now-loaded stores and re-filter. The
/// ancestor rows are already expanded by construction (a scan only starts
/// when a row expands), so no expansion work happens here.
fn reapply_filter_parts(
    root_store: &gio::ListStore,
    child_stores: &Rc<RefCell<std::collections::HashMap<PathBuf, glib::WeakRef<gio::ListStore>>>>,
    filter_state: &Rc<RefCell<FilterState>>,
    filter: &gtk4::CustomFilter,
) {
    let query = filter_state.borrow().query.clone();
    if query.is_empty() {
        return;
    }
    let roots = store_entries(root_store);
    let visible = {
        let child_stores = child_stores.borrow();
        collect_visible_paths(
            &roots,
            &|path| {
                child_stores
                    .get(path)
                    .and_then(glib::WeakRef::upgrade)
                    .map(|store| store_entries(&store))
            },
            &query,
        )
    };
    {
        let mut state = filter_state.borrow_mut();
        // The query may have moved on while the scan was in flight.
        if state.query != query {
            return;
        }
        state.visible = visible;
    }
    filter.changed(gtk4::FilterChange::Different);
}

/// Which entries a context-menu action applies to, and whether the selection
/// must first collapse to the right-clicked row. Right-clicking a row that is
/// in the current selection targets the whole selection; right-clicking
/// anywhere else targets just that row. Returns the affected entries and the
/// position to collapse to, if any.
fn resolve_menu_target(
    target: Option<(u32, FileEntry)>,
    selected: &[(u32, FileEntry)],
) -> (Vec<FileEntry>, Option<u32>) {
    match target {
        None => (Vec::new(), None),
        Some((position, entry)) => {
            if selected
                .iter()
                .any(|(selected_pos, _)| *selected_pos == position)
            {
                (
                    selected.iter().map(|(_, entry)| entry.clone()).collect(),
                    None,
                )
            } else {
                (vec![entry], Some(position))
            }
        }
    }
}

/// Directory targeted by context actions that operate on children. A
/// directory row targets itself, a file row targets its parent, and empty
/// space targets the visible tree root.
fn directory_action_target(root: &Path, target: Option<&FileEntry>) -> PathBuf {
    match target {
        Some(entry) if entry.is_dir => entry.path.clone(),
        Some(entry) => entry
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf()),
        None => root.to_path_buf(),
    }
}

/// One planned paste of a clipboard item: destination, and whether the
/// destination already exists (checked for Local targets only — remote
/// destinations are refused atomically by the probe at op time).
struct PastePlanItem {
    src: PathBuf,
    dst: PathBuf,
    is_dir: bool,
    /// Regular-file bytes below `src`, measured only for local sources
    /// (upload progress totals); zero otherwise.
    size: u64,
    /// Destination exists already (Local target only).
    collides: bool,
    /// dst == src: pasting an item onto itself.
    self_paste: bool,
}

/// Plan a batch paste into `target_dir`: per-item destination join, self-paste
/// detection, (for Local targets) existence pre-flags, and (for local
/// sources) size measurement. Pure apart from those local fs reads.
fn plan_paste(
    items: &[remote_fs::FsClipboardItem],
    target_dir: &Path,
    target_is_local: bool,
    measure_sources: bool,
) -> Vec<PastePlanItem> {
    items
        .iter()
        .map(|item| {
            let dst = remote_fs::paste_destination(target_dir, &item.path);
            PastePlanItem {
                self_paste: dst == item.path,
                collides: target_is_local && std::fs::symlink_metadata(&dst).is_ok(),
                size: if measure_sources {
                    remote_fs::drop_entry_size(&item.path, 0)
                } else {
                    0
                },
                is_dir: item.is_dir,
                src: item.path.clone(),
                dst,
            }
        })
        .collect()
}

/// The batch-operation summary line: `None` when nothing failed, else
/// "2 of 5 failed: <first>" (or just the bare failure for a single item).
fn failure_summary(failed: usize, total: usize, first: &str) -> Option<String> {
    if failed == 0 {
        return None;
    }
    if total <= 1 {
        Some(format!("Failed: {first}"))
    } else {
        Some(format!("{failed} of {total} failed: {first}"))
    }
}

/// The delete confirmation copy for one or more entries: the title names the
/// count, the body lists up to five names and a remainder. Names are display
/// names (already spoofing-sanitized at scan time).
fn delete_confirmation_text(entries: &[FileEntry]) -> (String, String) {
    debug_assert!(!entries.is_empty());
    if entries.len() == 1 {
        let display =
            jterm_core::review_input::safe_inline_display(&entries[0].path.to_string_lossy(), 1024);
        let detail = if entries[0].is_dir {
            format!("“{display}” and everything inside it will be permanently deleted.")
        } else {
            format!("“{display}” will be permanently deleted.")
        };
        return ("Delete this item?".to_string(), detail);
    }
    let mut lines: Vec<String> = entries
        .iter()
        .take(5)
        .map(|entry| jterm_core::review_input::safe_inline_display(&entry.name, 256))
        .collect();
    if entries.len() > 5 {
        lines.push(format!("…and {} more", entries.len() - 5));
    }
    (
        format!("Delete {} items?", entries.len()),
        format!(
            "These {} items will be permanently deleted:\n{}",
            entries.len(),
            lines.join("\n")
        ),
    )
}

/// Build the modern GTK4 list-model file browser.
pub(crate) fn build_file_tree_widgets(
    location: Rc<RefCell<FsLocation>>,
    execution_overlay: Rc<RefCell<FsExecutionOverlay>>,
    config: Rc<RefCell<crate::config::Config>>,
) -> (FileTreeModel, ListView) {
    let model = FileTreeModel::new(location, execution_overlay, config);
    let factory = SignalListItemFactory::new();

    factory.connect_setup({
        let model = model.clone();
        move |_, object| {
            let Some(list_item) = object.downcast_ref::<gtk4::ListItem>() else {
                return;
            };

            let icon = gtk4::Image::new();
            icon.set_pixel_size(16);
            let label = gtk4::Label::new(None);
            label.set_hexpand(true);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk4::pango::EllipsizeMode::End);

            let retry = gtk4::Button::with_label("Retry");
            retry.set_focusable(true);
            retry.set_tooltip_text(Some("Retry loading this directory"));
            retry.update_property(&[gtk4::accessible::Property::Label("Retry loading directory")]);
            retry.add_css_class("flat");
            retry.set_visible(false);
            let item = list_item.downgrade();
            let model_for_retry = model.clone();
            retry.connect_clicked(move |_| {
                let Some(list_item) = item.upgrade() else {
                    return;
                };
                let Some(row) = list_item
                    .item()
                    .and_then(|item| item.downcast::<TreeListRow>().ok())
                else {
                    return;
                };
                let Some(entry) = entry_from_row(&row) else {
                    return;
                };
                if entry
                    .status
                    .as_ref()
                    .is_some_and(DirectoryRowStatus::is_retryable)
                {
                    model_for_retry.retry_directory(&entry.path);
                }
            });

            let row_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
            row_box.append(&icon);
            row_box.append(&label);
            row_box.append(&retry);

            let expander = gtk4::TreeExpander::new();
            expander.set_child(Some(&row_box));
            list_item.set_child(Some(&expander));
        }
    });

    factory.connect_bind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let Some(row) = list_item
            .item()
            .and_then(|item| item.downcast::<TreeListRow>().ok())
        else {
            return;
        };
        let Some(expander) = list_item
            .child()
            .and_then(|child| child.downcast::<gtk4::TreeExpander>().ok())
        else {
            return;
        };
        expander.set_list_row(Some(&row));

        let Some(entry) = entry_from_row(&row) else {
            return;
        };
        let Some(row_box) = expander
            .child()
            .and_then(|child| child.downcast::<gtk4::Box>().ok())
        else {
            return;
        };
        let Some(icon) = row_box
            .first_child()
            .and_then(|child| child.downcast::<gtk4::Image>().ok())
        else {
            return;
        };
        let Some(label) = icon
            .next_sibling()
            .and_then(|child| child.downcast::<gtk4::Label>().ok())
        else {
            return;
        };
        let Some(retry) = label
            .next_sibling()
            .and_then(|child| child.downcast::<gtk4::Button>().ok())
        else {
            return;
        };

        let icon_name = match &entry.status {
            Some(DirectoryRowStatus::Loading) => "content-loading-symbolic",
            Some(DirectoryRowStatus::Refreshing { .. }) => "view-refresh-symbolic",
            Some(DirectoryRowStatus::Error { .. }) => "dialog-error-symbolic",
            None if entry.is_dir => "folder-symbolic",
            None => "text-x-generic-symbolic",
        };
        icon.set_icon_name(Some(icon_name));
        label.set_text(&entry.name);
        let path = safe_file_label(&entry.path.to_string_lossy());
        label.set_tooltip_text(Some(&path));
        let retryable = entry
            .status
            .as_ref()
            .is_some_and(DirectoryRowStatus::is_retryable);
        retry.set_visible(retryable);
        list_item.set_selectable(entry.is_item());
        list_item.set_activatable(entry.is_item() || retryable);
    });

    factory.connect_unbind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let Some(expander) = list_item
            .child()
            .and_then(|child| child.downcast::<gtk4::TreeExpander>().ok())
        else {
            return;
        };
        expander.set_list_row(None);
        list_item.set_selectable(true);
        list_item.set_activatable(true);
    });

    // MultiSelection: ctrl+click toggles and shift+click ranges come from
    // GTK; activation (double-click/Enter) is unaffected.
    let file_tree = ListView::new(Some(model.selection.clone()), Some(factory));
    file_tree.set_single_click_activate(false);
    file_tree.set_show_separators(false);
    file_tree.set_can_focus(true);
    file_tree.update_property(&[gtk4::accessible::Property::Label(
        "Files in current directory",
    )]);
    file_tree.add_css_class("file-tree");

    (model, file_tree)
}

/// Keep an index-backed remote location bound to the exact profile it meant
/// before a configuration edit. Reordering is harmless; replacement,
/// removal, or an ambiguous duplicate fails closed instead of silently
/// redirecting old tree rows and clipboard paths to another machine.
fn remap_remote_location(
    location: &FsLocation,
    previous_hosts: &[RemoteHost],
    current_hosts: &[RemoteHost],
) -> Option<FsLocation> {
    let previous_index = match location {
        FsLocation::Local => return Some(FsLocation::Local),
        FsLocation::Transient(target) => {
            return remote_fs::transient_remote_host(target)
                .is_ok()
                .then(|| location.clone())
        }
        FsLocation::Remote(previous_index) => previous_index,
    };
    if *previous_index >= crate::config::MAX_REMOTE_HOSTS {
        return None;
    }
    let profile = previous_hosts.get(*previous_index)?;
    let mut matches = current_hosts
        .iter()
        .take(crate::config::MAX_REMOTE_HOSTS)
        .enumerate()
        .filter_map(|(index, candidate)| (candidate == profile).then_some(index));
    let index = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    crate::config::checked_remote_host(current_hosts, index).ok()?;
    Some(FsLocation::Remote(index))
}

/// Resolve the concrete profile a live managed tab was launched from without
/// trusting its display name. Restored tabs intentionally replace only the
/// profile's jsh session id with the saved tab id. That exception is explicit;
/// fresh connections still compare every field. In both modes every
/// filesystem-authority field and remaining profile policy must match exactly
/// once and the current candidate must pass the execution gate.
fn unique_remote_connection_profile_index(
    connected: &RemoteHost,
    current_hosts: &[RemoteHost],
    profile_session_overridden: bool,
) -> Option<usize> {
    let mut matches = current_hosts
        .iter()
        .take(crate::config::MAX_REMOTE_HOSTS)
        .enumerate()
        .filter_map(|(index, candidate)| {
            let same_profile = if profile_session_overridden {
                let mut normalized = connected.clone();
                normalized.session = candidate.session.clone();
                &normalized == candidate
            } else {
                connected == candidate
            };
            (same_profile && crate::config::validate_remote_host(candidate).is_ok())
                .then_some(index)
        });
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
}

fn file_tree_context_matches(
    expected_generation: u64,
    expected_location: &FsLocation,
    current_generation: u64,
    current_location: &FsLocation,
) -> bool {
    expected_generation == current_generation && expected_location == current_location
}

/// A completed cut clears only the user intent it started from. Payload value
/// equality is insufficient: a later Copy/Cut may deliberately select the
/// same paths, while an exact remote-profile reorder may change only `loc` on
/// the original intent.
fn clear_clipboard_if_intent_matches(
    slot: &mut Option<FsClipboard>,
    expected_intent_id: u64,
) -> bool {
    if slot.as_ref().map(|clipboard| clipboard.intent_id) != Some(expected_intent_id) {
        return false;
    }
    *slot = None;
    true
}

fn clipboard_for_intent(
    slot: &Option<FsClipboard>,
    expected_intent_id: u64,
) -> Option<FsClipboard> {
    slot.as_ref()
        .filter(|clipboard| clipboard.intent_id == expected_intent_id)
        .cloned()
}

/// An observed SSH process does not carry a Forge profile name or launch-only
/// policy. Match exactly the fields the file-tree probe actually executes,
/// and only when that authority is unique in the current validated config.
/// Session/deploy/remote-shell settings cannot affect a sidecar filesystem
/// probe and therefore must not prevent a saved endpoint from being reused.
fn unique_observed_profile_index(target: &RemoteHostConfig, hosts: &[RemoteHost]) -> Option<usize> {
    remote_fs::transient_remote_host(target).ok()?;
    let mut matches = hosts
        .iter()
        .take(crate::config::MAX_REMOTE_HOSTS)
        .enumerate()
        .filter_map(|(index, candidate)| {
            (!candidate.docker
                && candidate.host == target.host
                && candidate.user == target.user
                && remote_fs::stable_ssh_args(&candidate.ssh_args)
                    .is_ok_and(|args| args == target.ssh_args)
                && crate::config::validate_remote_host(candidate).is_ok())
            .then_some(index)
        });
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
}

fn observed_target_location(target: &RemoteHostConfig, hosts: &[RemoteHost]) -> FsLocation {
    unique_observed_profile_index(target, hosts)
        .map(FsLocation::Remote)
        .unwrap_or_else(|| FsLocation::Transient(target.clone()))
}

/// Equality at the filesystem transport boundary. Saved-profile policy and
/// the transient/saved representation do not change which account and SSH
/// option vector the tree addresses.
fn location_matches_observed_target(
    location: &FsLocation,
    target: &RemoteHostConfig,
    hosts: &[RemoteHost],
) -> bool {
    match location {
        FsLocation::Local => false,
        FsLocation::Transient(current) => current == target,
        FsLocation::Remote(index) => {
            crate::config::checked_remote_host(hosts, *index).is_ok_and(|host| {
                !host.docker
                    && host.host == target.host
                    && host.user == target.user
                    && remote_fs::stable_ssh_args(&host.ssh_args)
                        .is_ok_and(|args| args == target.ssh_args)
            })
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteFollowContext {
    intent: u64,
    operation_intent: u64,
    tree_generation: u64,
    tab_focus_generation: u64,
    source_focus_serial: u64,
    location: FsLocation,
    root: PathBuf,
}

impl RemoteFollowContext {
    fn matches(&self, current: &Self) -> bool {
        self == current
    }
}

impl UiState {
    fn next_file_tree_clipboard_intent(&self) -> Option<u64> {
        let current = self.file_tree_clipboard_intent.get();
        let Some(next) = current.checked_add(1) else {
            self.toast_overlay.add_toast(adw::Toast::new(
                "Copy/Cut is unavailable because its intent counter is exhausted",
            ));
            return None;
        };
        self.file_tree_clipboard_intent.set(next);
        Some(next)
    }

    fn next_file_tree_remote_follow_intent(&self) -> Option<u64> {
        let current = self.file_tree_remote_follow_intent.get();
        let Some(next) = current.checked_add(1) else {
            self.toast_overlay.add_toast(adw::Toast::new(
                "Automatic Remote Files is unavailable because its intent counter is exhausted",
            ));
            return None;
        };
        self.file_tree_remote_follow_intent.set(next);
        Some(next)
    }

    pub(crate) fn invalidate_file_tree_remote_follow(&self) {
        let _ = self.next_file_tree_remote_follow_intent();
    }

    /// Read the active pane's actual process tree. No terminal bytes, prompt
    /// text, OSC metadata, or generic shell-command reconstruction participate
    /// in this authority decision; Core also verifies jsh launcher provenance.
    fn current_observed_ssh_command(
        &self,
    ) -> Option<(String, u64, jterm_core::process::ObservedSshCommand)> {
        let leaf = self.current_pane_leaf()?;
        if leaf.is_remote() {
            return None;
        }
        let session = leaf.session_id()?;
        let focus_serial = leaf.focus_serial();
        Some((session, focus_serial, leaf.observed_ssh_command()?))
    }

    fn observed_ssh_identity_is_current(
        &self,
        source_session: &str,
        source_argv: &[String],
        target: &RemoteHostConfig,
        execution_overlay: Option<&FsExecutionOverlay>,
    ) -> bool {
        self.current_observed_ssh_command()
            .is_some_and(|(session, _, command)| {
                let current = match command.target {
                    jterm_core::jsh_remote::ObservedSshTarget::Target(target) => {
                        remote_fs::observed_target_and_overlay(
                            target,
                            command.reusable_control_path,
                        )
                        .ok()
                    }
                    _ => None,
                };
                session == source_session
                    && command.argv == source_argv
                    && current
                        .as_ref()
                        .is_some_and(|(current_target, current_overlay)| {
                            current_target == target
                                && execution_overlay
                                    .is_none_or(|expected| current_overlay == expected)
                        })
            })
    }

    fn show_remote_follow_unsupported(&self, reason: &'static str) {
        let reason = jterm_core::review_input::safe_inline_display(reason, 512);
        let toast = adw::Toast::new(&format!("Remote Files did not follow SSH: {reason}"));
        toast.set_button_label(Some("Choose Profile"));
        let ui = self.clone();
        toast.connect_button_clicked(move |_| {
            ui.set_sidebar_visible(true, false);
            ui.apply_sidebar_view(crate::config::SidebarView::Files, false);
            ui.file_tree_location_selector.grab_focus();
        });
        self.toast_overlay.add_toast(toast);
    }

    fn show_remote_follow_failure(
        &self,
        source_session: String,
        source_argv: Vec<String>,
        target: RemoteHostConfig,
        error: io::Error,
    ) {
        let name = jterm_core::review_input::safe_inline_display(target.display_name(), 256);
        let detail = public_directory_error_message(&error);
        let toast = adw::Toast::new(&format!("Cannot open Remote Files for {name}: {detail}"));
        toast.set_button_label(Some("Retry"));
        let ui = self.clone();
        toast.connect_button_clicked(move |_| {
            let current = ui.current_observed_ssh_command();
            if ui.observed_ssh_identity_is_current(&source_session, &source_argv, &target, None) {
                let Some((session, _, command)) = current else {
                    return;
                };
                // Use the freshly observed execution overlay: the socket can
                // become available between the failed probe and this click.
                ui.stage_observed_remote_files(session, command);
            } else {
                ui.toast_overlay.add_toast(adw::Toast::new(
                    "That SSH process is no longer active; reconnect and retry",
                ));
            }
        });
        self.toast_overlay.add_toast(toast);
    }

    fn stage_observed_remote_files(
        &self,
        source_session: String,
        command: jterm_core::process::ObservedSshCommand,
    ) {
        if std::env::var_os("FORGE_SAFE_MODE").is_some() {
            return;
        }
        let jterm_core::process::ObservedSshCommand {
            argv: source_argv,
            target,
            reusable_control_path,
        } = command;
        let jterm_core::jsh_remote::ObservedSshTarget::Target(raw_target) = target else {
            return;
        };
        let (target, overlay) =
            match remote_fs::observed_target_and_overlay(raw_target.clone(), reusable_control_path)
            {
                Ok(value) => value,
                Err(error) => {
                    self.show_remote_follow_failure(source_session, source_argv, raw_target, error);
                    return;
                }
            };
        if !self.observed_ssh_identity_is_current(
            &source_session,
            &source_argv,
            &target,
            Some(&overlay),
        ) {
            return;
        }
        if self.file_tree_active_operations.get() != 0 {
            // The observation stays consumed. A user file operation is an
            // explicit cancellation boundary, not permission to retry this
            // same process automatically when that operation finishes.
            return;
        }
        let Some(operation_intent) = self.file_tree_operation_intent.get() else {
            return;
        };
        let Some(source_leaf) = self.current_pane_leaf() else {
            return;
        };
        let source_focus_serial = source_leaf.focus_serial();
        let source_root = source_leaf.root_widget().downgrade();
        let hosts = self.config.borrow().remote_hosts.clone();
        let location = observed_target_location(&target, &hosts);
        if let Err(error) = remote_fs::transient_remote_host(&target) {
            self.show_remote_follow_failure(source_session, source_argv, target, error);
            return;
        }

        // Re-running SSH for a target the user is already browsing still
        // probes the newly observed socket before replacing execution state.
        // On success the visible root/rows/expansion stay intact; only the
        // location representation and overlay are upgraded, which also makes
        // every older in-flight scan fail its immutable snapshot check.
        let same_target =
            location_matches_observed_target(&self.file_tree_location.borrow(), &target, &hosts)
                && !self.file_tree_root.borrow().as_os_str().is_empty();

        let Some(intent) = self.next_file_tree_remote_follow_intent() else {
            return;
        };
        let expected_context = RemoteFollowContext {
            intent,
            operation_intent,
            tree_generation: self.file_tree_model.generation.get(),
            tab_focus_generation: self.tab_focus_generation.get(),
            source_focus_serial,
            location: self.file_tree_location.borrow().clone(),
            root: self.file_tree_root.borrow().clone(),
        };
        let location_for_work = location.clone();
        let hosts_for_work = hosts.clone();
        let ui = self.clone();
        let source_for_apply = source_session.clone();
        let argv_for_apply = source_argv.clone();
        let target_for_apply = target.clone();
        let overlay_for_apply = overlay.clone();
        let apply = move |result: io::Result<PathBuf>| {
            if std::env::var_os("FORGE_SAFE_MODE").is_some() {
                return;
            }
            let Some(source_root) = source_root.upgrade() else {
                return;
            };
            let operation_changed = Some(expected_context.operation_intent)
                != ui.file_tree_operation_intent.get()
                || ui.file_tree_active_operations.get() != 0;
            if operation_changed {
                // Keep the exact observation deduplicated. Retry remains
                // available explicitly on failures; a new focus epoch or a
                // genuinely new process argv will stage a fresh probe.
                return;
            }
            let Some(current_operation_intent) = ui.file_tree_operation_intent.get() else {
                return;
            };
            let current_context = RemoteFollowContext {
                intent: ui.file_tree_remote_follow_intent.get(),
                operation_intent: current_operation_intent,
                tree_generation: ui.file_tree_model.generation.get(),
                tab_focus_generation: ui.tab_focus_generation.get(),
                source_focus_serial: ui.current_pane_leaf().map_or(0, |leaf| leaf.focus_serial()),
                location: ui.file_tree_location.borrow().clone(),
                root: ui.file_tree_root.borrow().clone(),
            };
            if !expected_context.matches(&current_context)
                || !ui.observed_ssh_identity_is_current(
                    &source_for_apply,
                    &argv_for_apply,
                    &target_for_apply,
                    Some(&overlay_for_apply),
                )
                || ui
                    .current_pane_leaf()
                    .is_none_or(|leaf| leaf.root_widget() != source_root)
            {
                return;
            }

            match result {
                Ok(root) => {
                    let current_hosts = ui.config.borrow().remote_hosts.clone();
                    let Some(current_location) =
                        remap_remote_location(&location, &hosts, &current_hosts)
                    else {
                        ui.show_remote_follow_failure(
                            source_for_apply,
                            argv_for_apply,
                            target_for_apply,
                            io::Error::other(
                                "the matching Remote Host profile changed; retry to use its current identity",
                            ),
                        );
                        return;
                    };
                    // Recompute transport uniqueness at commit time too. A new
                    // same-transport/different-policy profile must turn a
                    // previously unique saved match into a transient target,
                    // never publish the old managed identity.
                    if current_location
                        != observed_target_location(&target_for_apply, &current_hosts)
                    {
                        ui.show_remote_follow_failure(
                            source_for_apply,
                            argv_for_apply,
                            target_for_apply,
                            io::Error::other(
                                "the matching Remote Host profiles changed; retry to resolve the current target",
                            ),
                        );
                        return;
                    }
                    let root = if same_target {
                        ui.file_tree_root.borrow().clone()
                    } else {
                        root
                    };
                    ui.navigate_file_tree_point(
                        FileTreeNavigationPoint {
                            location: current_location,
                            overlay: overlay_for_apply,
                            root,
                        },
                        if same_target {
                            FileTreeNavigationAction::Replace
                        } else {
                            FileTreeNavigationAction::Push
                        },
                    );
                }
                Err(error) => ui.show_remote_follow_failure(
                    source_for_apply,
                    argv_for_apply,
                    target_for_apply,
                    error,
                ),
            }
        };
        if let Err(error) = request_fs_op(
            move || {
                remote_fs::start_dir_with_overlay(&location_for_work, &hosts_for_work, &overlay)
            },
            apply,
        ) {
            self.show_remote_follow_failure(source_session, source_argv, target, error);
        }
    }

    /// Called by the existing single window heartbeat. It observes only the
    /// active pane and deduplicates its exact `/proc` argv, so all terminal
    /// render backends gain the behavior without a timer per pane or any
    /// dependency on shell-integration lifecycle completeness.
    pub(crate) fn poll_file_tree_remote_follow(&self) {
        if std::env::var_os("FORGE_SAFE_MODE").is_some() {
            if self
                .file_tree_remote_follow_observed
                .borrow_mut()
                .take()
                .is_some()
            {
                self.invalidate_file_tree_remote_follow();
            }
            return;
        }
        if self.file_tree_active_operations.get() != 0
            || self.file_tree_operation_intent.get().is_none()
        {
            return;
        }
        let Some((session, source_focus_serial, command)) = self.current_observed_ssh_command()
        else {
            if self
                .file_tree_remote_follow_observed
                .borrow_mut()
                .take()
                .is_some()
            {
                self.invalidate_file_tree_remote_follow();
            }
            return;
        };
        let argv = command.argv.clone();
        let tab_focus_generation = self.tab_focus_generation.get();
        if self
            .file_tree_remote_follow_observed
            .borrow()
            .as_ref()
            .is_some_and(|seen| {
                seen.matches(&session, &argv, tab_focus_generation, source_focus_serial)
            })
        {
            return;
        }
        *self.file_tree_remote_follow_observed.borrow_mut() =
            Some(crate::ui::FileTreeRemoteObservation {
                source_session: session.clone(),
                argv,
                tab_focus_generation,
                source_focus_serial,
            });
        // A changed foreground process invalidates any probe queued for its
        // predecessor before this new observation can start another one.
        self.invalidate_file_tree_remote_follow();
        match &command.target {
            jterm_core::jsh_remote::ObservedSshTarget::NotSsh => {}
            jterm_core::jsh_remote::ObservedSshTarget::Unsupported(reason) => {
                self.show_remote_follow_unsupported(reason);
            }
            jterm_core::jsh_remote::ObservedSshTarget::Target(_) => {
                self.stage_observed_remote_files(session, command);
            }
        }
    }

    /// Set up the initial file tree root (current tab cwd, else $HOME).
    pub(crate) fn init_file_tree(&self) {
        let start = self
            .current_terminal()
            .as_ref()
            .and_then(terminal_working_directory)
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.set_file_tree_root(start);
    }

    /// Navigate within the current filesystem. Once a tree exists the target
    /// listing is staged first; failure or an out-of-order answer leaves the
    /// current root, rows, selection and history untouched.
    pub(crate) fn set_file_tree_root(&self, root: PathBuf) {
        if !self.file_tree_root.borrow().as_os_str().is_empty()
            && !self.file_tree_model.committed_authority_is_current()
        {
            self.toast_overlay.add_toast(adw::Toast::new(
                "The filesystem profile changed; choose a location before navigating",
            ));
            return;
        }
        let target = FileTreeNavigationPoint {
            location: self.file_tree_location.borrow().clone(),
            overlay: self.file_tree_execution_overlay.borrow().clone(),
            root,
        };
        if self.file_tree_root.borrow().as_os_str().is_empty() {
            self.load_file_tree_root_immediately(target);
        } else {
            self.navigate_file_tree_point(target, FileTreeNavigationAction::Push);
        }
    }

    fn refresh_file_tree_root_header(&self) {
        let location = self.file_tree_location.borrow().clone();
        let root = self.file_tree_root.borrow().clone();
        if !root.as_os_str().is_empty() && !self.file_tree_model.committed_authority_is_current() {
            self.file_tree_root_label.set_text(&format!(
                "Unavailable snapshot: {}",
                safe_file_label(&root.to_string_lossy())
            ));
            self.file_tree_root_label.set_tooltip_text(Some(
                "The filesystem profile changed; existing rows are read-only until another location commits",
            ));
            return;
        }
        self.file_tree_root_label
            .set_text(&root_display_label(&location, &root));
        let root_tooltip = safe_file_label(&root.to_string_lossy());
        self.file_tree_root_label
            .set_tooltip_text(Some(&root_tooltip));
    }

    fn load_file_tree_root_immediately(&self, target: FileTreeNavigationPoint) {
        // Root changes are user/navigation authority too. Advancing the
        // independent follow token closes the theoretical generation-wrap ABA
        // and ensures a staged SSH probe can never steal the tree afterward.
        self.invalidate_file_tree_remote_follow();
        *self.file_tree_location.borrow_mut() = target.location.clone();
        *self.file_tree_execution_overlay.borrow_mut() = target.overlay.clone();
        self.file_tree_navigation
            .borrow_mut()
            .install_initial(target.clone());
        let generation = self.file_tree_model.reset();
        let location = target.location.clone();
        let root = target.root;
        self.file_tree_root_label
            .set_text(&root_display_label(&location, &root));
        let root_tooltip = safe_file_label(&root.to_string_lossy());
        self.file_tree_root_label
            .set_tooltip_text(Some(&root_tooltip));
        *self.file_tree_root.borrow_mut() = root.clone();

        let model = self.file_tree_model.clone();
        model.set_root_path(&root);
        model.set_root_status(generation, &root, DirectoryRowStatus::Loading);
        let model_for_start_error = model.clone();
        let expected_root = root.clone();
        let scan_request = model.issue_directory_scan(&expected_root);
        let scan_revision = scan_request.revision;
        let active_root = self.file_tree_root.clone();
        let toast_overlay = self.toast_overlay.clone();
        let scan_hosts = self.config.borrow().remote_hosts.clone();
        if let Ok(authority) = remote_fs::filesystem_identity(&location, &scan_hosts) {
            model.set_committed_authority(authority);
        }
        let scan_location = location.clone();
        let scan_overlay = self.file_tree_execution_overlay.borrow().clone();
        let location_for_scan = self.file_tree_location.clone();
        let overlay_for_scan = self.file_tree_execution_overlay.clone();
        if let Err(error) = request_dir_scan(
            location,
            scan_hosts,
            scan_overlay.clone(),
            root,
            scan_request.cancel.clone(),
            ScanPriority::Root,
            move |result| {
                if *active_root.borrow() != expected_root {
                    return;
                }
                // A location switch after this scan was queued makes its entries
                // meaningless for the tree now on screen.
                if *location_for_scan.borrow() != scan_location {
                    return;
                }
                if *overlay_for_scan.borrow() != scan_overlay {
                    return;
                }
                if !model.directory_scan_is_current(&expected_root, scan_revision) {
                    return;
                }
                model.complete_directory_scan(&expected_root, scan_revision);
                match result {
                    Ok(listing) => {
                        let timing = listing.timing;
                        let entry_count = listing.entries.len();
                        let reconcile_started = Instant::now();
                        model.mark_snapshot_completed(&expected_root);
                        if listing.truncated {
                            let path = root_display_label(&scan_location, &expected_root);
                            toast_overlay.add_toast(adw::Toast::new(&format!(
                                "{path} has more than {} entries; showing the first {}",
                                remote_fs::MAX_DIRECTORY_ENTRIES,
                                remote_fs::MAX_DIRECTORY_ENTRIES
                            )));
                        }
                        model.replace_root(generation, listing.entries);
                        log_scan_timing(
                            &expected_root,
                            timing,
                            reconcile_started.elapsed(),
                            &StoreReconcileDelta {
                                inserted_rows: entry_count,
                                ..StoreReconcileDelta::default()
                            },
                        );
                    }
                    Err(error) => {
                        log::warn!(
                            "failed to scan file-tree root {}: {error}",
                            expected_root.display()
                        );
                        // An empty directory and an unreadable directory must
                        // not look identical. Publish a retryable tree row;
                        // refresh failures elsewhere retain last-good rows too.
                        if model.set_root_status(
                            generation,
                            &expected_root,
                            directory_error_status(
                                &error,
                                model.last_good_snapshot(&expected_root),
                            ),
                        ) {
                            let path = root_display_label(&scan_location, &expected_root);
                            let error = public_directory_error_message(&error);
                            toast_overlay.add_toast(adw::Toast::new(&format!(
                                "Cannot open {path}: {error}"
                            )));
                        }
                    }
                }
            },
        ) {
            model_for_start_error
                .complete_directory_scan(&self.file_tree_root.borrow(), scan_revision);
            log::warn!("failed to start file-tree scan: {error}");
            // The start error is synchronous, but still respect the current
            // generation in case this function is re-entered by UI callbacks.
            if model_for_start_error.set_root_status(
                generation,
                &self.file_tree_root.borrow(),
                directory_error_status(
                    &error,
                    model_for_start_error.last_good_snapshot(&self.file_tree_root.borrow()),
                ),
            ) {
                let location = self.file_tree_location.borrow().clone();
                let path = root_display_label(&location, &self.file_tree_root.borrow());
                let error = public_directory_error_message(&error);
                self.toast_overlay
                    .add_toast(adw::Toast::new(&format!("Cannot open {path}: {error}")));
            }
        }
    }

    fn navigate_file_tree_point(
        &self,
        target: FileTreeNavigationPoint,
        action: FileTreeNavigationAction,
    ) {
        if !target.root.is_absolute() {
            self.toast_overlay
                .add_toast(adw::Toast::new("File-tree paths must be absolute"));
            return;
        }
        self.invalidate_file_tree_remote_follow();
        let request = self
            .file_tree_navigation
            .borrow_mut()
            .begin(target.clone(), action);
        let hosts = self.config.borrow().remote_hosts.clone();
        let expected_authority = match remote_fs::filesystem_identity(&target.location, &hosts) {
            Ok(authority) => authority,
            Err(error) => {
                self.file_tree_navigation.borrow_mut().fail(&request);
                ScanScheduler::global().retire_cancelled();
                self.refresh_file_tree_root_header();
                self.refresh_file_tree_location_selector();
                let detail = public_directory_error_message(&error);
                self.toast_overlay
                    .add_toast(adw::Toast::new(&format!("Cannot open path: {detail}")));
                return;
            }
        };
        let target_label = root_display_label(&target.location, &target.root);
        self.file_tree_root_label
            .set_text(&format!("Opening {target_label}…"));
        self.file_tree_root_label.set_tooltip_text(Some(
            "The current tree remains available until navigation succeeds",
        ));

        let ui = self.clone();
        let request_for_result = request.clone();
        let target_for_error = target.clone();
        let start = request_dir_scan(
            target.location.clone(),
            hosts,
            target.overlay.clone(),
            target.root.clone(),
            request.cancel.clone(),
            ScanPriority::Root,
            move |result| {
                if !ui
                    .file_tree_navigation
                    .borrow()
                    .is_current(&request_for_result)
                {
                    return;
                }
                let live_hosts = ui.config.borrow().remote_hosts.clone();
                if remote_fs::filesystem_identity(&request_for_result.target.location, &live_hosts)
                    .ok()
                    .as_ref()
                    != Some(&expected_authority)
                {
                    ui.file_tree_navigation
                        .borrow_mut()
                        .fail(&request_for_result);
                    ui.refresh_file_tree_root_header();
                    ui.refresh_file_tree_location_selector();
                    ui.toast_overlay.add_toast(adw::Toast::new(
                        "The remote profile changed while navigation was pending",
                    ));
                    return;
                }
                match result {
                    Ok(listing) => {
                        if !ui
                            .file_tree_navigation
                            .borrow_mut()
                            .commit(&request_for_result)
                        {
                            return;
                        }
                        let reconcile_started = Instant::now();
                        let generation = ui.file_tree_model.reset();
                        ui.file_tree_model
                            .set_committed_authority(expected_authority.clone());
                        *ui.file_tree_location.borrow_mut() =
                            request_for_result.target.location.clone();
                        *ui.file_tree_execution_overlay.borrow_mut() =
                            request_for_result.target.overlay.clone();
                        *ui.file_tree_root.borrow_mut() = request_for_result.target.root.clone();
                        ui.file_tree_model
                            .set_root_path(&request_for_result.target.root);
                        ui.file_tree_model
                            .mark_snapshot_completed(&request_for_result.target.root);
                        let timing = listing.timing;
                        let entry_count = listing.entries.len();
                        ui.file_tree_model.replace_root(generation, listing.entries);
                        ui.refresh_file_tree_location_selector();
                        ui.refresh_file_tree_root_header();
                        let delta = StoreReconcileDelta {
                            inserted_rows: entry_count,
                            ..StoreReconcileDelta::default()
                        };
                        log_scan_timing(
                            &request_for_result.target.root,
                            timing,
                            reconcile_started.elapsed(),
                            &delta,
                        );
                        if listing.truncated {
                            ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                                "{} has more than {} entries; showing the first {}",
                                root_display_label(
                                    &request_for_result.target.location,
                                    &request_for_result.target.root
                                ),
                                remote_fs::MAX_DIRECTORY_ENTRIES,
                                remote_fs::MAX_DIRECTORY_ENTRIES
                            )));
                        }
                    }
                    Err(error) => {
                        ui.file_tree_navigation
                            .borrow_mut()
                            .fail(&request_for_result);
                        ui.refresh_file_tree_root_header();
                        ui.refresh_file_tree_location_selector();
                        log::warn!(
                            "failed transactional file-tree navigation to {}: {error}",
                            request_for_result.target.root.display()
                        );
                        let detail = public_directory_error_message(&error);
                        ui.toast_overlay
                            .add_toast(adw::Toast::new(&format!("Cannot open path: {detail}")));
                    }
                }
            },
        );
        if let Err(error) = start {
            self.file_tree_navigation.borrow_mut().fail(&request);
            self.refresh_file_tree_root_header();
            self.refresh_file_tree_location_selector();
            log::warn!(
                "failed to start transactional navigation to {}: {error}",
                target_for_error.root.display()
            );
            let detail = public_directory_error_message(&error);
            self.toast_overlay
                .add_toast(adw::Toast::new(&format!("Cannot open path: {detail}")));
        }
    }

    /// Switch the tree to another filesystem (local disk or one of the
    /// configured remote hosts) and root it at that location's start
    /// directory. Remote home discovery and the first listing are both
    /// staged; failure leaves the previous filesystem and tree committed.
    pub(crate) fn set_file_tree_location(&self, location: FsLocation) {
        if *self.file_tree_location.borrow() == location {
            // Choosing the still-committed location is an explicit way to
            // back out of a staged selector/navigation request. Restore the
            // selector/header immediately and retire its queued scan.
            self.invalidate_file_tree_remote_follow();
            if self.file_tree_navigation.borrow_mut().cancel_pending() {
                ScanScheduler::global().retire_cancelled();
            }
            self.refresh_file_tree_root_header();
            self.refresh_file_tree_location_selector();
            return;
        }
        // Selector/navigation changes choose stable authority explicitly; an
        // accelerator observed for a previous SSH process must not follow the
        // user onto that new endpoint.
        let hosts = self.config.borrow().remote_hosts.clone();
        if location == FsLocation::Local {
            let root = remote_fs::start_dir_with_overlay(
                &FsLocation::Local,
                &hosts,
                &FsExecutionOverlay::default(),
            )
            .or_else(|_| home_dir().ok_or_else(|| io::Error::other("home is unavailable")))
            .unwrap_or_else(|_| PathBuf::from("/"));
            self.navigate_file_tree_point(
                FileTreeNavigationPoint {
                    location: FsLocation::Local,
                    overlay: FsExecutionOverlay::default(),
                    root,
                },
                FileTreeNavigationAction::Push,
            );
            return;
        }
        let expected_authority = match remote_fs::filesystem_identity(&location, &hosts) {
            Ok(authority) => authority,
            Err(error) => {
                self.fail_file_tree_location_change(&location, &error);
                return;
            }
        };

        let label = location.label(&hosts);
        self.file_tree_root_label
            .set_text(&format!("Connecting to {label}…"));
        self.file_tree_root_label
            .set_tooltip_text(Some("Resolving the remote start directory"));
        self.invalidate_file_tree_remote_follow();
        let navigation_guard = self.file_tree_remote_follow_intent.get();

        let ui = self.clone();
        let expected_location = location.clone();
        let location_for_work = location.clone();
        let hosts_for_work = hosts.clone();
        let apply = move |result: io::Result<PathBuf>| {
            let current_hosts = ui.config.borrow().remote_hosts.clone();
            if !location_home_probe_is_current(
                navigation_guard,
                ui.file_tree_remote_follow_intent.get(),
                &expected_location,
                &expected_authority,
                &current_hosts,
            ) {
                return;
            }
            match result {
                Ok(root) => ui.navigate_file_tree_point(
                    FileTreeNavigationPoint {
                        location: expected_location.clone(),
                        overlay: FsExecutionOverlay::default(),
                        root,
                    },
                    FileTreeNavigationAction::Push,
                ),
                Err(error) => ui.fail_file_tree_location_change(&expected_location, &error),
            }
        };
        if let Err(error) = request_fs_op(
            move || {
                remote_fs::start_dir_with_overlay(
                    &location_for_work,
                    &hosts_for_work,
                    &FsExecutionOverlay::default(),
                )
            },
            apply,
        ) {
            self.fail_file_tree_location_change(&location, &error);
        }
    }

    fn fail_file_tree_location_change(&self, location: &FsLocation, error: &io::Error) {
        let hosts = self.config.borrow().remote_hosts.clone();
        let label = location.label(&hosts);
        log::warn!("failed to resolve start directory for {label}: {error}");
        let detail = public_directory_error_message(error);
        self.refresh_file_tree_root_header();
        self.refresh_file_tree_location_selector();
        self.toast_overlay
            .add_toast(adw::Toast::new(&format!("Cannot open {label}: {detail}")));
    }

    /// Reconcile the tree and its clipboard after `remote_hosts` changes.
    /// Exact profiles may move to another index; anything else returns the
    /// visible tree to Local and drops clipboard paths whose target identity
    /// can no longer be proven.
    pub(crate) fn reconcile_file_tree_remote_hosts(&self, previous_hosts: &[RemoteHost]) {
        let current_hosts = self.config.borrow().remote_hosts.clone();

        {
            let mut clipboard = self.file_tree_clipboard.borrow_mut();
            if let Some(payload) = clipboard.as_mut() {
                match remap_remote_location(&payload.loc, previous_hosts, &current_hosts) {
                    Some(location) => payload.loc = location,
                    None => *clipboard = None,
                }
            }
        }
        self.file_tree_navigation
            .borrow_mut()
            .remap_history_locations(|location| {
                remap_remote_location(location, previous_hosts, &current_hosts)
            });

        let previous_location = self.file_tree_location.borrow().clone();
        match remap_remote_location(&previous_location, previous_hosts, &current_hosts) {
            Some(location) => {
                if location == previous_location {
                    self.refresh_file_tree_location_selector();
                } else {
                    self.file_tree_model.cancel_pending_scans_preserve_tree();
                    let root = self.file_tree_root.borrow().clone();
                    if root.as_os_str().is_empty() {
                        // A home probe queued with the old index will be
                        // discarded by its location check; start a fresh one
                        // against the exact profile's new index.
                        self.set_file_tree_location(location);
                    } else {
                        self.navigate_file_tree_point(
                            FileTreeNavigationPoint {
                                location,
                                overlay: self.file_tree_execution_overlay.borrow().clone(),
                                root,
                            },
                            FileTreeNavigationAction::Replace,
                        );
                    }
                }
            }
            None => {
                self.file_tree_model.cancel_pending_scans_preserve_tree();
                let root = home_dir().unwrap_or_else(|| PathBuf::from("/"));
                self.navigate_file_tree_point(
                    FileTreeNavigationPoint {
                        location: FsLocation::Local,
                        overlay: FsExecutionOverlay::default(),
                        root,
                    },
                    FileTreeNavigationAction::Replace,
                );
                self.toast_overlay.add_toast(adw::Toast::new(
                    "Remote file-tree target changed; returned to Local",
                ));
            }
        }
    }

    /// Bridge the filesystem browser to a terminal in one action. Local opens
    /// exactly at the visible root. A remote location opens its managed SSH or
    /// Docker profile; the remote shell chooses its configured/default start
    /// directory because that launcher has no portable cwd contract.
    pub(crate) fn open_file_tree_terminal(&self) {
        if !self.file_tree_model.committed_authority_is_current() {
            self.toast_overlay.add_toast(adw::Toast::new(
                "The filesystem profile changed; choose a location before opening a terminal",
            ));
            return;
        }
        match self.file_tree_location.borrow().clone() {
            FsLocation::Local => {
                let root = self.file_tree_root.borrow().clone();
                if root.as_os_str().is_empty() {
                    self.toast_overlay
                        .add_toast(adw::Toast::new("The file-tree location is still loading"));
                    return;
                }
                let Some(cwd) = root.to_str() else {
                    self.toast_overlay.add_toast(adw::Toast::new(
                        "This directory cannot cross the terminal's UTF-8 cwd boundary",
                    ));
                    return;
                };
                let startup = self.config.borrow().startup_commands.clone();
                self.add_new_tab(
                    Some(cwd.to_string()),
                    None,
                    None,
                    crate::terminal::InitialCommands::from_config(startup.as_deref()),
                );
            }
            FsLocation::Remote(index) => {
                let host = {
                    let config = self.config.borrow();
                    crate::config::checked_remote_host(&config.remote_hosts, index).cloned()
                };
                match host {
                    Ok(host) => {
                        self.connect_remote(&host);
                    }
                    Err(message) => {
                        self.toast_overlay.add_toast(adw::Toast::new(message));
                    }
                }
            }
            FsLocation::Transient(target) => {
                let overlay = self.file_tree_execution_overlay.borrow().clone();
                self.connect_transient_plain_ssh(&target, &overlay);
            }
        }
    }

    fn file_tree_context_is_current(&self, generation: u64, location: &FsLocation) -> bool {
        file_tree_context_matches(
            generation,
            location,
            self.file_tree_model.generation.get(),
            &self.file_tree_location.borrow(),
        ) && self.file_tree_model.committed_authority_is_current()
    }

    fn require_current_file_tree_context(&self, generation: u64, location: &FsLocation) -> bool {
        if self.file_tree_context_is_current(generation, location) {
            return true;
        }
        self.toast_overlay.add_toast(adw::Toast::new(
            "The file-tree location changed; reopen the action",
        ));
        false
    }

    fn require_current_file_tree_entries(
        &self,
        generation: u64,
        location: &FsLocation,
        entries: &[FileEntry],
    ) -> bool {
        if !self.require_current_file_tree_context(generation, location) {
            return false;
        }
        if self
            .file_tree_model
            .materialized_entries_are_current(entries)
        {
            return true;
        }
        self.toast_overlay.add_toast(adw::Toast::new(
            "A selected file changed or disappeared; reopen the action",
        ));
        false
    }

    /// Toggle the type-to-filter row open/closed. Closing clears the query,
    /// removes the filter wrap, and restores pre-filter expansion.
    pub(crate) fn toggle_file_tree_filter(&self) {
        if self.file_tree_filter_bar.is_visible() {
            self.close_file_tree_filter();
        } else {
            self.file_tree_filter_bar.set_visible(true);
            self.file_tree_filter_toggle.set_active(true);
            self.file_tree_filter_entry.grab_focus();
        }
    }

    /// Esc / toggle-off path: clear the entry (which re-applies an empty
    /// query through the changed signal) and hide the row.
    fn close_file_tree_filter(&self) {
        self.file_tree_filter_entry.set_text("");
        self.file_tree_model.apply_filter(String::new());
        self.file_tree_filter_bar.set_visible(false);
        self.file_tree_filter_toggle.set_active(false);
    }

    /// Wire the filter row: text changes drive the model filter, Esc closes.
    pub(crate) fn connect_file_tree_filter_bar(&self) {
        let model = self.file_tree_model.clone();
        self.file_tree_filter_entry.connect_changed(move |entry| {
            model.apply_filter(entry.text().to_string());
        });
        let key = gtk4::EventControllerKey::new();
        let ui = self.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk4::gdk::Key::Escape {
                ui.close_file_tree_filter();
                return true.into();
            }
            false.into()
        });
        self.file_tree_filter_entry.add_controller(key);
    }

    /// Rebuild the location selector's entries from the configured hosts and
    /// re-select the active location. Model replacement may emit intermediate
    /// selections, so the notify handler is explicitly suppressed until both
    /// the model and selected index are coherent.
    pub(crate) fn refresh_file_tree_location_selector(&self) {
        let config = self.config.borrow();
        let hosts = &config.remote_hosts;
        let active_count = hosts.len().min(crate::config::MAX_REMOTE_HOSTS);
        let mut labels = vec![FsLocation::Local.label(hosts)];
        labels.extend((0..active_count).map(|index| FsLocation::Remote(index).label(hosts)));
        if let FsLocation::Transient(target) = &*self.file_tree_location.borrow() {
            labels.push(FsLocation::Transient(target.clone()).label(hosts));
        }
        let authority_current = self.file_tree_root.borrow().as_os_str().is_empty()
            || self.file_tree_model.committed_authority_is_current();
        let selected = if authority_current {
            match &*self.file_tree_location.borrow() {
                FsLocation::Local => 0,
                FsLocation::Remote(index)
                    if *index < active_count
                        && crate::config::checked_remote_host(hosts, *index).is_ok() =>
                {
                    *index as u32 + 1
                }
                FsLocation::Remote(_) => gtk4::INVALID_LIST_POSITION,
                FsLocation::Transient(_) => active_count as u32 + 1,
            }
        } else {
            gtk4::INVALID_LIST_POSITION
        };
        let selected_tooltip = if authority_current {
            labels
                .get(selected as usize)
                .cloned()
                .unwrap_or_else(|| "Choose which filesystem to browse".to_string())
        } else {
            "The previous filesystem profile changed; choose a location".to_string()
        };
        drop(config);
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let model = gtk4::StringList::new(&label_refs);
        self.file_tree_location_selector_syncing.set(true);
        self.file_tree_location_selector.set_model(Some(&model));
        self.file_tree_location_selector.set_selected(selected);
        self.file_tree_location_selector
            .set_tooltip_text(Some(&selected_tooltip));
        self.file_tree_location_selector_syncing.set(false);
    }

    /// React to a user selection in the header location dropdown.
    pub(crate) fn connect_file_tree_location_selector(&self) {
        let ui = self.clone();
        self.file_tree_location_selector
            .connect_selected_notify(move |selector| {
                if ui.file_tree_location_selector_syncing.get() {
                    return;
                }
                let current = ui.file_tree_location.borrow().clone();
                let active_remote_count = ui
                    .config
                    .borrow()
                    .remote_hosts
                    .len()
                    .min(crate::config::MAX_REMOTE_HOSTS);
                let Some(location) = file_tree_location_from_selection(
                    selector.selected(),
                    active_remote_count,
                    &current,
                ) else {
                    return;
                };
                // Dispatch even when `location == current`: the user may be
                // re-selecting committed A specifically to cancel a staged B
                // home/list request. The setter owns that cancellation.
                ui.set_file_tree_location(location);
            });
    }

    /// The frozen profile for the active managed remote tab. Keeping this
    /// separate from profile resolution lets callers distinguish a local tab
    /// from a remote tab whose profile was removed or became ambiguous; the
    /// latter must not fall through to the ssh client's local `/proc` cwd.
    fn current_tab_remote_connection(&self) -> Option<TabConnection> {
        let page_num = self.notebook.current_page()?;
        let page = self.notebook.nth_page(Some(page_num))?;
        let tab_num = page
            .widget_name()
            .strip_prefix("tab-")
            .and_then(|value| value.parse::<u32>().ok())?;
        self.tab_connections.borrow().get(&tab_num).cloned()
    }

    /// Jump the file tree to the active tab's working directory. A remote
    /// tab whose shell reports its cwd through OSC 7 pulls the tree onto its
    /// host, rooted at the reported directory; one that reports nothing
    /// leaves the tree alone, exactly as before.
    pub(crate) fn file_tree_goto_current_cwd(&self) {
        let Some(active_leaf) = self.current_pane_leaf() else {
            return;
        };
        if active_leaf.is_remote() {
            let Some(connection) = self.current_tab_remote_connection() else {
                // A remote pane without its managed connection record has no
                // authority that can safely interpret an OSC 7 path.
                return;
            };
            let Some(index) = unique_remote_connection_profile_index(
                &connection.host,
                &self.config.borrow().remote_hosts,
                connection.profile_session_overridden,
            ) else {
                // The tab is still managed remote, but its current profile is
                // no longer provably unique. Keep the tree where it is rather
                // than treating ssh's local process cwd as a safe fallback.
                return;
            };
            // Only the OSC 7 report is trusted here: the /proc fallback in
            // `terminal_working_directory` would resolve the ssh client's
            // LOCAL cwd, which is not a path on the remote host.
            let reported = self
                .current_terminal()
                .and_then(|terminal| terminal.current_directory_uri())
                .and_then(|uri| gio::File::for_uri(uri.as_str()).path())
                .filter(|path| path.is_absolute());
            if let Some(cwd) = reported {
                let location = FsLocation::Remote(index);
                if *self.file_tree_location.borrow() != location
                    || *self.file_tree_root.borrow() != cwd
                {
                    self.navigate_file_tree_point(
                        FileTreeNavigationPoint {
                            location,
                            overlay: FsExecutionOverlay::default(),
                            root: cwd,
                        },
                        FileTreeNavigationAction::Push,
                    );
                }
            }
            return;
        }
        let cwd = self
            .current_terminal()
            .as_ref()
            .and_then(terminal_working_directory)
            .map(PathBuf::from)
            .filter(|path| path.is_dir());
        match cwd {
            Some(dir) => {
                if *self.file_tree_location.borrow() != FsLocation::Local
                    || *self.file_tree_root.borrow() != dir
                {
                    self.navigate_file_tree_point(
                        FileTreeNavigationPoint {
                            location: FsLocation::Local,
                            overlay: FsExecutionOverlay::default(),
                            root: dir,
                        },
                        FileTreeNavigationAction::Push,
                    );
                }
            }
            None => {
                // No reportable cwd (for example, a remote shell). Keep the
                // current tree unless it has never been initialized.
                if self.file_tree_root.borrow().as_os_str().is_empty() {
                    if let Some(home) = home_dir() {
                        self.set_file_tree_root(home);
                    }
                }
            }
        }
    }

    /// Navigate through committed roots; a failed history target leaves both
    /// stacks and the visible tree untouched.
    pub(crate) fn file_tree_go_back(&self) {
        let target = self.file_tree_navigation.borrow().back_target();
        if let Some(target) = target {
            self.navigate_file_tree_point(target, FileTreeNavigationAction::Back);
        }
    }

    pub(crate) fn file_tree_go_forward(&self) {
        let target = self.file_tree_navigation.borrow().forward_target();
        if let Some(target) = target {
            self.navigate_file_tree_point(target, FileTreeNavigationAction::Forward);
        }
    }

    /// Move the root up to the parent directory.
    pub(crate) fn file_tree_go_up(&self) {
        let parent = self.file_tree_root.borrow().parent().map(Path::to_path_buf);
        if let Some(parent) = parent {
            self.set_file_tree_root(parent);
        }
    }

    /// Resolve the selected filesystem's home. Remote home lookup runs off
    /// the GTK thread and is guarded by generation/location/root snapshots so
    /// a late probe cannot undo newer navigation.
    pub(crate) fn file_tree_go_home(&self) {
        if !self.file_tree_model.committed_authority_is_current() {
            self.toast_overlay.add_toast(adw::Toast::new(
                "The filesystem profile changed; choose a location before opening Home",
            ));
            return;
        }
        let location = self.file_tree_location.borrow().clone();
        let overlay = self.file_tree_execution_overlay.borrow().clone();
        let hosts = self.config.borrow().remote_hosts.clone();
        if location == FsLocation::Local {
            let home = remote_fs::start_dir_with_overlay(&location, &hosts, &overlay)
                .unwrap_or_else(|_| PathBuf::from("/"));
            self.set_file_tree_root(home);
            return;
        }

        let expected_generation = self.file_tree_model.generation.get();
        let expected_navigation_revision = self.file_tree_navigation.borrow().revision();
        let expected_root = self.file_tree_root.borrow().clone();
        let ui = self.clone();
        let location_for_work = location.clone();
        let overlay_for_work = overlay.clone();
        let apply = move |result: io::Result<PathBuf>| {
            if ui.file_tree_model.generation.get() != expected_generation
                || ui.file_tree_navigation.borrow().revision() != expected_navigation_revision
                || *ui.file_tree_location.borrow() != location
                || *ui.file_tree_execution_overlay.borrow() != overlay
                || *ui.file_tree_root.borrow() != expected_root
            {
                return;
            }
            match result {
                Ok(home) => ui.set_file_tree_root(home),
                Err(error) => {
                    log::warn!("failed to resolve Remote Files home: {error}");
                    let detail = public_directory_error_message(&error);
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&format!("Cannot open Home: {detail}")));
                }
            }
        };
        if let Err(error) = request_fs_op(
            move || {
                remote_fs::start_dir_with_overlay(&location_for_work, &hosts, &overlay_for_work)
            },
            apply,
        ) {
            log::warn!("failed to start Remote Files home lookup: {error}");
            let detail = public_directory_error_message(&error);
            self.toast_overlay
                .add_toast(adw::Toast::new(&format!("Cannot open Home: {detail}")));
        }
    }

    pub(crate) fn present_file_tree_path_dialog(&self) {
        let dialog = adw::Dialog::builder()
            .title("Open Filesystem Path")
            .content_width(440)
            .build();
        let entry = adw::EntryRow::new();
        entry.set_title("Absolute path");
        if let Some(root) = self.file_tree_root.borrow().to_str() {
            entry.set_text(root);
        }
        let error = gtk4::Label::new(None);
        error.add_css_class("error");
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.set_visible(false);

        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.append(&entry);
        content.append(&error);

        let breadcrumbs = gtk4::FlowBox::new();
        breadcrumbs.set_selection_mode(gtk4::SelectionMode::None);
        breadcrumbs.set_column_spacing(4);
        breadcrumbs.set_row_spacing(4);
        for ancestor in navigation_breadcrumbs(&self.file_tree_root.borrow()) {
            let label = if ancestor == Path::new("/") {
                "/".to_string()
            } else {
                ancestor
                    .file_name()
                    .map(|name| safe_file_label(&name.to_string_lossy()))
                    .unwrap_or_else(|| "/".to_string())
            };
            let button = gtk4::Button::with_label(&label);
            button.add_css_class("flat");
            let ui = self.clone();
            let dialog_for_ancestor = dialog.clone();
            button.connect_clicked(move |_| {
                dialog_for_ancestor.close();
                ui.set_file_tree_root(ancestor.clone());
            });
            breadcrumbs.insert(&button, -1);
        }
        content.append(&breadcrumbs);

        let header = adw::HeaderBar::new();
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);
        let cancel = gtk4::Button::with_label("Cancel");
        let navigate = gtk4::Button::with_label("Open");
        navigate.add_css_class("suggested-action");
        header.pack_start(&cancel);
        header.pack_end(&navigate);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        dialog.set_child(Some(&toolbar));

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog_for_cancel.close();
        });
        let ui = self.clone();
        let dialog_for_navigate = dialog.clone();
        navigate.connect_clicked(
            move |_| match validate_absolute_navigation_path(&entry.text()) {
                Ok(path) => {
                    dialog_for_navigate.close();
                    ui.set_file_tree_root(path);
                }
                Err(message) => {
                    error.set_text(message);
                    error.set_visible(true);
                }
            },
        );
        dialog.present(Some(&self.window));
    }

    fn file_tree_enter_selected_directory(&self) -> bool {
        let selected = self.file_tree_model.selected_entries_snapshot();
        let [entry] = selected.as_slice() else {
            return false;
        };
        if !entry.is_dir
            || !self
                .file_tree_model
                .materialized_entries_are_current(std::slice::from_ref(entry))
        {
            return false;
        }
        self.set_file_tree_root(entry.path.clone());
        true
    }

    /// Connect activation after UiState exists. Directory activation toggles the
    /// corresponding TreeListRow; file activation inserts a shell-quoted path.
    /// Button 3 opens the file-operations context menu for the row under the
    /// pointer (or the tree root when the pointer is over empty space).
    pub(crate) fn connect_file_tree_handlers(&self, file_tree: &ListView) {
        let ui = self.clone();
        file_tree.connect_activate(move |_, position| {
            let Some((row, entry)) = ui.file_tree_model.row_entry(position) else {
                return;
            };
            if entry
                .status
                .as_ref()
                .is_some_and(DirectoryRowStatus::is_retryable)
            {
                ui.file_tree_model.retry_directory(&entry.path);
                return;
            }
            if !entry.is_item() {
                return;
            }
            if entry.is_dir {
                row.set_expanded(!row.is_expanded());
                return;
            }

            if crate::notebook::is_notebook_path(&entry.path) {
                ui.open_notebook(&entry.path);
                return;
            }

            let file_path = entry.path.to_string_lossy();
            if let Some(snippet) = file_insert_snippet(file_path.as_ref()) {
                if let Some(pane) = ui.current_pane_leaf() {
                    ui.insert_review_text(&pane, &snippet);
                }
            } else {
                log::warn!("refusing to insert a path with unsafe display text");
                ui.toast_overlay
                    .add_toast(adw::Toast::new("Path contains hidden or control text"));
            }
        });

        // Scope navigation to the Files ListView. The window/terminal never
        // sees these claimed keys only while focus is within this tree.
        let refresh_keys = gtk4::EventControllerKey::new();
        refresh_keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let ui_for_refresh = self.clone();
        refresh_keys.connect_key_pressed(
            move |_, keyval, _, state| match file_tree_navigation_key(keyval, state) {
                Some(FileTreeNavigationKey::Refresh) => {
                    let root = ui_for_refresh.file_tree_root.borrow().clone();
                    if !root.as_os_str().is_empty() {
                        ui_for_refresh.refresh_dir_listing(&root);
                    }
                    true.into()
                }
                Some(FileTreeNavigationKey::Up) => {
                    ui_for_refresh.file_tree_go_up();
                    true.into()
                }
                Some(FileTreeNavigationKey::Home) => {
                    ui_for_refresh.file_tree_go_home();
                    true.into()
                }
                Some(FileTreeNavigationKey::EnterDirectory) => {
                    ui_for_refresh.file_tree_enter_selected_directory().into()
                }
                None => false.into(),
            },
        );
        file_tree.add_controller(refresh_keys);

        // Remote snapshots remain useful while stale; revalidate only visible
        // materialized directories, in a small batch, and coalesce with any
        // revision already pending for that path.
        let tree_for_ttl = file_tree.downgrade();
        let ui_for_ttl = self.clone();
        glib::timeout_add_local(Duration::from_secs(30), move || {
            let Some(tree) = tree_for_ttl.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if tree.is_mapped() {
                ui_for_ttl.refresh_visible_stale_file_tree_directories();
            }
            glib::ControlFlow::Continue
        });

        let right_click = gtk4::GestureClick::new();
        right_click.set_button(3);
        let ui = self.clone();
        let file_tree_for_menu = file_tree.clone();
        right_click.connect_pressed(move |gesture, _n_press, x, y| {
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            let target =
                file_tree_row_at(&file_tree_for_menu, x, y).filter(|(_, entry)| entry.is_item());
            ui.show_file_tree_context_menu(&file_tree_for_menu, x, y, target);
        });
        file_tree.add_controller(right_click);

        // Drag-and-drop import from the OS file manager. Only local files are
        // accepted (Gdk::FileList is exactly the local-file format); the drop
        // lands in the row's directory, or the tree root over empty space.
        let drop_target = gtk4::DropTarget::new(
            gtk4::gdk::FileList::static_type(),
            gtk4::gdk::DragAction::COPY,
        );
        let drop_hover = self.file_tree_model.drop_hover.clone();
        {
            let hover = drop_hover.clone();
            let tree = file_tree.clone();
            drop_target.connect_enter(move |_, x, y| {
                set_drop_hover(&hover, file_tree_row_widget_at(&tree, x, y));
                gtk4::gdk::DragAction::COPY
            });
        }
        {
            let hover = drop_hover.clone();
            let tree = file_tree.clone();
            drop_target.connect_motion(move |_, x, y| {
                set_drop_hover(&hover, file_tree_row_widget_at(&tree, x, y));
                gtk4::gdk::DragAction::COPY
            });
        }
        {
            let hover = drop_hover.clone();
            drop_target.connect_leave(move |_| {
                set_drop_hover(&hover, None);
            });
        }
        {
            let ui = self.clone();
            let tree = file_tree.clone();
            drop_target.connect_drop(move |_, value, x, y| {
                set_drop_hover(&drop_hover, None);
                let Ok(file_list) = value.get::<gtk4::gdk::FileList>() else {
                    return false;
                };
                let paths: Vec<PathBuf> = file_list
                    .files()
                    .iter()
                    .filter_map(|file| file.path())
                    .collect();
                if paths.is_empty() {
                    return false;
                }
                let target_dir = match file_tree_row_at(&tree, x, y) {
                    Some((_, entry)) if entry.is_dir => entry.path,
                    Some((_, entry)) if !entry.is_item() => entry.path,
                    Some((_, entry)) => entry
                        .path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| ui.file_tree_root.borrow().clone()),
                    None => ui.file_tree_root.borrow().clone(),
                };
                ui.import_dropped_paths(paths, target_dir);
                true
            });
        }
        file_tree.add_controller(drop_target);
    }

    /// Import OS-dragged local paths into `target_dir`: plan first (refusing
    /// oversized or malformed drops wholesale), then run the per-item copies
    /// or uploads on one op worker with the transfer progress/cancel wiring,
    /// exactly like a cross-location paste.
    fn import_dropped_paths(&self, paths: Vec<PathBuf>, target_dir: PathBuf) {
        let location = self.file_tree_location.borrow().clone();
        let overlay = self.file_tree_execution_overlay.borrow().clone();
        let hosts = self.config.borrow().remote_hosts.clone();
        let (items, action, total_bytes) =
            match remote_fs::plan_drop(&paths, &location, &target_dir) {
                remote_fs::DropPlan::Refuse(reason) => {
                    self.toast_overlay.add_toast(adw::Toast::new(&reason));
                    return;
                }
                remote_fs::DropPlan::Import {
                    items,
                    action,
                    total_bytes,
                } => (items, action, total_bytes),
            };
        let count = items.len();
        let (verb, verb_ing): (&'static str, &'static str) = match action {
            remote_fs::DropAction::Copy => ("Copy", "Copying"),
            remote_fs::DropAction::Upload => ("Upload", "Uploading"),
        };
        let target_desc = if location.is_remote() {
            location.label(&hosts)
        } else {
            display_path(&target_dir)
        };
        let title = format!("{verb_ing} {count} items to {target_desc}…");

        // Upload progress reports carry a cumulative byte count across items;
        // uploads know the drop-wide total up-front, copies report nothing.
        let total = match action {
            remote_fs::DropAction::Upload => Some(total_bytes),
            remote_fs::DropAction::Copy => None,
        };
        let (busy, token, progress_tx) = self.build_transfer_feedback(&title, total);

        let failures: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let failures_for_work = failures.clone();
        let token_for_work = token.clone();
        let to = location.clone();
        let affected = vec![target_dir];
        let ui = self.clone();
        self.execute_fs_op(
            verb,
            Some(busy),
            affected,
            move || {
                let mut completed_bytes = 0_u64;
                for item in &items {
                    if token_for_work.is_cancelled() {
                        return Err(remote_fs::cancelled_error());
                    }
                    let name = jterm_core::review_input::safe_inline_display(
                        &item
                            .src
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        256,
                    );
                    if item.collides {
                        failures_for_work
                            .lock()
                            .unwrap()
                            .push(format!("{name}: already exists"));
                        completed_bytes += item.size;
                        continue;
                    }
                    // Cumulative progress across items: each transfer reports
                    // its own bytes; the sink shifts them by the bytes of the
                    // items that already finished.
                    let base = completed_bytes;
                    let progress_tx = progress_tx.clone();
                    let item_control = remote_fs::TransferControl {
                        token: token_for_work.clone(),
                        progress: Some(std::sync::Arc::new(std::sync::Mutex::new(
                            move |bytes: u64| {
                                let _ = progress_tx.send(base + bytes);
                            },
                        ))),
                    };
                    let result = match action {
                        remote_fs::DropAction::Copy => {
                            remote_fs::copy(&FsLocation::Local, &[], &item.src, &item.dst)
                        }
                        remote_fs::DropAction::Upload => remote_fs::transfer_with_overlays(
                            &FsLocation::Local,
                            &FsExecutionOverlay::default(),
                            &hosts,
                            &item.src,
                            &to,
                            &overlay,
                            &item.dst,
                            item.is_dir,
                            &item_control,
                        ),
                    };
                    if let Err(error) = result {
                        if error.kind() == io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                        let detail = public_file_operation_error_message(&error);
                        failures_for_work
                            .lock()
                            .unwrap()
                            .push(format!("{name}: {detail}"));
                    }
                    completed_bytes += item.size;
                }
                Ok(())
            },
            move || {
                let failures = failures.lock().unwrap();
                if failures.is_empty() {
                    return;
                }
                let first = jterm_core::review_input::safe_inline_display(&failures[0], 256);
                let text = if failures.len() == 1 {
                    format!("1 item failed: {first}")
                } else {
                    format!("{} items failed: {first}", failures.len())
                };
                ui.toast_overlay.add_toast(adw::Toast::new(&text));
            },
        );
    }

    /// Right-click file-operations menu for `target`. Right-clicking a row in
    /// the current selection applies row actions to the whole selection;
    /// right-clicking elsewhere collapses the selection to that row first.
    /// New File/Folder, Paste and Refresh act on the target directory (a file
    /// row contributes its parent); Rename needs exactly one row;
    /// Delete/Copy/Cut/Copy Path work on one or more.
    fn show_file_tree_context_menu(
        &self,
        file_tree: &ListView,
        x: f64,
        y: f64,
        target: Option<(TreeListRow, FileEntry)>,
    ) {
        // Plain Popover + Buttons, matching the terminal and tab context
        // menus: the GAction-based PopoverMenu dispatch does not fire in this
        // GTK build, so direct connect_clicked closures are used.
        let location = self.file_tree_location.borrow().clone();
        let context_generation = self.file_tree_model.generation.get();
        let selected = self.file_tree_model.selected_entries();
        let target_with_pos = target.clone().and_then(|(_, entry)| {
            self.file_tree_model
                .flat_position_of(&entry.path)
                .map(|position| (position, entry))
        });
        let (entries, collapse_to) = resolve_menu_target(target_with_pos, &selected);
        if let Some(position) = collapse_to {
            // Right-clicked a row outside the selection: the selection
            // collapses to that row before any action runs.
            self.file_tree_model.selection.unselect_all();
            self.file_tree_model.selection.select_item(position, true);
        }
        let has_target = !entries.is_empty();
        let multi = entries.len() > 1;
        let target_dir = directory_action_target(
            &self.file_tree_root.borrow(),
            target.as_ref().map(|(_, entry)| entry),
        );

        let popover = gtk4::Popover::new();
        popover.set_parent(file_tree);
        popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.set_has_arrow(false);

        let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        vbox.add_css_class("menu");

        let make_item = |label: &str| -> gtk4::Button {
            let btn = gtk4::Button::with_label(label);
            btn.set_has_frame(false);
            btn.set_halign(gtk4::Align::Fill);
            if let Some(child) = btn.child() {
                child.set_halign(gtk4::Align::Start);
            }
            btn.add_css_class("flat");
            btn
        };

        {
            let item = make_item("Open Folder");
            let folder = entries
                .first()
                .cloned()
                .filter(|entry| !multi && entry.is_dir);
            item.set_sensitive(folder.is_some());
            if let Some(folder) = folder {
                let popover_c = popover.clone();
                let ui = self.clone();
                let expected_location = location.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    if ui.require_current_file_tree_entries(
                        context_generation,
                        &expected_location,
                        std::slice::from_ref(&folder),
                    ) {
                        ui.set_file_tree_root(folder.path.clone());
                    }
                });
            }
            vbox.append(&item);
        }

        for (label, kind) in [
            ("New File", NameDialogKind::NewFile),
            ("New Folder", NameDialogKind::NewFolder),
        ] {
            let item = make_item(label);
            // Creation dialogs name one entry; under multi-selection they
            // would be ambiguous about which rows they relate to.
            item.set_sensitive(!multi);
            let popover_c = popover.clone();
            let ui = self.clone();
            let dir = target_dir.clone();
            let expected_location = location.clone();
            let expected_entries = entries.clone();
            item.connect_clicked(move |_| {
                popover_c.popdown();
                if !ui.require_current_file_tree_entries(
                    context_generation,
                    &expected_location,
                    &expected_entries,
                ) {
                    return;
                }
                ui.present_file_tree_name_dialog(kind, dir.clone(), None);
            });
            vbox.append(&item);
        }

        {
            let item = make_item("Rename");
            item.set_sensitive(has_target && !multi);
            if let Some(entry) = entries.first().cloned().filter(|_| !multi) {
                let popover_c = popover.clone();
                let ui = self.clone();
                let expected_location = location.clone();
                let expected_entry = entry.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    if !ui.require_current_file_tree_entries(
                        context_generation,
                        &expected_location,
                        std::slice::from_ref(&expected_entry),
                    ) {
                        return;
                    }
                    let dir = entry
                        .path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_default();
                    ui.present_file_tree_name_dialog(
                        NameDialogKind::Rename,
                        dir,
                        Some(entry.clone()),
                    );
                });
            }
            vbox.append(&item);
        }

        {
            let delete_label = if multi {
                format!("Delete {} items", entries.len())
            } else {
                "Delete".to_string()
            };
            let item = make_item(&delete_label);
            item.set_sensitive(has_target);
            if has_target {
                let popover_c = popover.clone();
                let ui = self.clone();
                let entries = entries.clone();
                let expected_location = location.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    if !ui.require_current_file_tree_entries(
                        context_generation,
                        &expected_location,
                        &entries,
                    ) {
                        return;
                    }
                    ui.confirm_file_tree_delete(entries.clone());
                });
            }
            vbox.append(&item);
        }

        for (label, cut) in [("Copy", false), ("Cut", true)] {
            let item = make_item(label);
            item.set_sensitive(has_target);
            if has_target {
                let popover_c = popover.clone();
                let ui = self.clone();
                let location = location.clone();
                let entries = entries.clone();
                let expected_location = location.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    if !ui.require_current_file_tree_entries(
                        context_generation,
                        &expected_location,
                        &entries,
                    ) {
                        return;
                    }
                    let Some(intent_id) = ui.next_file_tree_clipboard_intent() else {
                        return;
                    };
                    *ui.file_tree_clipboard.borrow_mut() = Some(FsClipboard {
                        intent_id,
                        loc: location.clone(),
                        overlay: ui.file_tree_execution_overlay.borrow().clone(),
                        items: entries
                            .iter()
                            .map(|entry| remote_fs::FsClipboardItem {
                                path: entry.path.clone(),
                                is_dir: entry.is_dir,
                            })
                            .collect(),
                        cut,
                    });
                });
            }
            vbox.append(&item);
        }

        {
            // Full path text as-is — remote rows copy the plain remote path
            // without any prefix; multi-selection joins paths with newlines.
            let item = make_item("Copy Path");
            item.set_sensitive(has_target);
            if has_target {
                let popover_c = popover.clone();
                let ui = self.clone();
                let entries = entries.clone();
                let expected_location = location.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    if !ui.require_current_file_tree_entries(
                        context_generation,
                        &expected_location,
                        &entries,
                    ) {
                        return;
                    }
                    let mut texts = Vec::with_capacity(entries.len());
                    for entry in &entries {
                        match copy_path_payload(&entry.path) {
                            Some(text) => texts.push(text),
                            None => {
                                log::warn!("refusing to copy a path with unsafe display text");
                                ui.toast_overlay.add_toast(adw::Toast::new(
                                    "A path contains hidden or control text",
                                ));
                                return;
                            }
                        }
                    }
                    ui.window.clipboard().set_text(&texts.join("\n"));
                });
            }
            vbox.append(&item);
        }

        {
            // Cross-location paste is a streaming transfer: label it so the
            // direction is visible before committing to it.
            let clipboard = self.file_tree_clipboard.borrow().clone();
            let transfer_hosts = self.config.borrow().remote_hosts.clone();
            let label = match clipboard.as_ref().and_then(|clip| {
                remote_fs::transfer_plan_with_hosts(&clip.loc, &location, &transfer_hosts)
            }) {
                Some(remote_fs::TransferPlan::Download) => "Paste (download)",
                Some(remote_fs::TransferPlan::Upload) => "Paste (upload)",
                Some(remote_fs::TransferPlan::Relay) => "Paste (via local relay)",
                None => "Paste",
            };
            let item = make_item(label);
            let pasteable = clipboard.as_ref().is_some_and(|clip| {
                !clip.items.is_empty()
                    && clip
                        .items
                        .iter()
                        .all(|item| item.path.file_name().is_some())
            });
            item.set_sensitive(pasteable);
            if clipboard.is_none() {
                item.set_tooltip_text(Some("Copy or cut an item first"));
            }
            if let Some(clip) = clipboard.filter(|clip| {
                !clip.items.is_empty()
                    && clip
                        .items
                        .iter()
                        .all(|item| item.path.file_name().is_some())
            }) {
                let popover_c = popover.clone();
                let ui = self.clone();
                let dir = target_dir.clone();
                let expected_location = location.clone();
                let expected_entries = entries.clone();
                let expected_clipboard_intent = clip.intent_id;
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    if !ui.require_current_file_tree_entries(
                        context_generation,
                        &expected_location,
                        &expected_entries,
                    ) {
                        return;
                    }
                    // The menu may outlive a config reload or a newer
                    // Copy/Cut. Resolve the frozen intent through the live
                    // clipboard so a removed source profile cannot be
                    // reinterpreted at the same numeric index. Exact profile
                    // reorders preserve the token and contribute the remapped
                    // location here.
                    let current_clipboard = clipboard_for_intent(
                        &ui.file_tree_clipboard.borrow(),
                        expected_clipboard_intent,
                    );
                    let Some(current_clipboard) = current_clipboard else {
                        ui.toast_overlay
                            .add_toast(adw::Toast::new("The file clipboard changed; reopen Paste"));
                        return;
                    };
                    ui.paste_file_tree_clipboard(current_clipboard, dir.clone());
                });
            }
            vbox.append(&item);
        }

        {
            let item = make_item("Refresh");
            let popover_c = popover.clone();
            let ui = self.clone();
            let expected_location = location;
            let dir = target_dir;
            let expected_entries = entries;
            item.connect_clicked(move |_| {
                popover_c.popdown();
                if !ui.require_current_file_tree_entries(
                    context_generation,
                    &expected_location,
                    &expected_entries,
                ) {
                    return;
                }
                // In-place re-list: expanded rows stay expanded.
                ui.refresh_dir_listing(&dir);
            });
            vbox.append(&item);
        }

        popover.set_child(Some(&vbox));
        popover.popup();
    }

    /// The shared name-entry dialog for New File / New Folder / Rename,
    /// styled after the remote-host dialog: an EntryRow, an inline error
    /// label, and a header bar carrying Cancel plus the confirm action.
    fn present_file_tree_name_dialog(
        &self,
        kind: NameDialogKind,
        dir: PathBuf,
        existing: Option<FileEntry>,
    ) {
        let expected_generation = self.file_tree_model.generation.get();
        let expected_location = self.file_tree_location.borrow().clone();
        let dialog = adw::Dialog::builder()
            .title(kind.title())
            .content_width(360)
            .build();

        let name_row = adw::EntryRow::new();
        name_row.set_title("Name");
        if let Some(entry) = &existing {
            // Prefill with the real file name from the path rather than the
            // sanitized display label.
            if let Some(name) = entry.path.file_name() {
                name_row.set_text(&name.to_string_lossy());
            }
        }

        let error_label = gtk4::Label::new(None);
        error_label.add_css_class("error");
        error_label.set_wrap(true);
        error_label.set_xalign(0.0);
        error_label.set_visible(false);

        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.append(&name_row);
        content.append(&error_label);

        let header = adw::HeaderBar::new();
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);
        let cancel_btn = gtk4::Button::with_label("Cancel");
        let confirm_btn = gtk4::Button::with_label(kind.confirm_label());
        confirm_btn.add_css_class("suggested-action");
        header.pack_start(&cancel_btn);
        header.pack_end(&confirm_btn);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&content));
        dialog.set_child(Some(&toolbar_view));

        let dialog_for_cancel = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            dialog_for_cancel.close();
        });

        let ui = self.clone();
        let dialog_for_confirm = dialog.clone();
        confirm_btn.connect_clicked(move |_| {
            if !ui.file_tree_context_is_current(expected_generation, &expected_location) {
                error_label
                    .set_text("The file-tree location changed; close and reopen this action.");
                error_label.set_visible(true);
                return;
            }
            if !ui
                .file_tree_model
                .materialized_directory_is_current(&ui.file_tree_root.borrow(), &dir)
            {
                error_label.set_text("The target directory changed; close and reopen this action.");
                error_label.set_visible(true);
                return;
            }
            if existing.as_ref().is_some_and(|entry| {
                !ui.file_tree_model
                    .materialized_entries_are_current(std::slice::from_ref(entry))
            }) {
                error_label
                    .set_text("The item changed or disappeared; close and reopen this action.");
                error_label.set_visible(true);
                return;
            }
            let name = name_row.text();
            if let Err(message) = remote_fs::validate_new_name(&name) {
                error_label.set_text(message);
                error_label.set_visible(true);
                return;
            }
            let location = ui.file_tree_location.borrow().clone();
            let overlay = ui.file_tree_execution_overlay.borrow().clone();
            let hosts = ui.config.borrow().remote_hosts.clone();
            match kind {
                NameDialogKind::NewFile | NameDialogKind::NewFolder => {
                    let path = dir.join(name.as_str());
                    let path_for_work = path.clone();
                    let ui_for_selection = ui.clone();
                    let expected_location_for_selection = expected_location.clone();
                    let dir_for_selection = dir.clone();
                    ui.execute_fs_op(
                        kind.verb(),
                        None,
                        vec![dir.clone()],
                        move || {
                            if kind == NameDialogKind::NewFile {
                                remote_fs::create_file_with_overlay(
                                    &location,
                                    &hosts,
                                    &overlay,
                                    &path_for_work,
                                )
                            } else {
                                remote_fs::create_dir_with_overlay(
                                    &location,
                                    &hosts,
                                    &overlay,
                                    &path_for_work,
                                )
                            }
                        },
                        move || {
                            if ui_for_selection.file_tree_context_is_current(
                                expected_generation,
                                &expected_location_for_selection,
                            ) {
                                ui_for_selection
                                    .file_tree_model
                                    .request_selection_after_refresh(
                                        &dir_for_selection,
                                        vec![path],
                                    );
                            }
                        },
                    );
                }
                NameDialogKind::Rename => {
                    let Some(entry) = &existing else {
                        dialog_for_confirm.close();
                        return;
                    };
                    let src = entry.path.clone();
                    let dst = dir.join(name.as_str());
                    if dst != src {
                        let dst_for_work = dst.clone();
                        let ui_for_selection = ui.clone();
                        let expected_location_for_selection = expected_location.clone();
                        let dir_for_selection = dir.clone();
                        ui.execute_fs_op(
                            kind.verb(),
                            None,
                            vec![dir.clone()],
                            move || {
                                remote_fs::rename_with_overlay(
                                    &location,
                                    &hosts,
                                    &overlay,
                                    &src,
                                    &dst_for_work,
                                )
                            },
                            move || {
                                if ui_for_selection.file_tree_context_is_current(
                                    expected_generation,
                                    &expected_location_for_selection,
                                ) {
                                    ui_for_selection
                                        .file_tree_model
                                        .request_selection_after_refresh(
                                            &dir_for_selection,
                                            vec![dst],
                                        );
                                }
                            },
                        );
                    }
                }
            }
            dialog_for_confirm.close();
        });

        dialog.present(Some(&self.window));
    }

    /// Destructive-delete confirmation for one or more entries: the title
    /// carries the count, the body up to five names. One worker job deletes
    /// the items in order, continuing past per-item errors, then every
    /// affected parent refreshes in place.
    fn confirm_file_tree_delete(&self, entries: Vec<FileEntry>) {
        if entries.is_empty() {
            return;
        }
        let expected_generation = self.file_tree_model.generation.get();
        let expected_location = self.file_tree_location.borrow().clone();
        let (title, detail) = delete_confirmation_text(&entries);
        let dialog = adw::AlertDialog::new(Some(&title), Some(&detail));
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            if !ui.require_current_file_tree_entries(
                expected_generation,
                &expected_location,
                &entries,
            ) {
                return;
            }
            // The response closure is Fn: clone the entry list into the
            // one-shot worker.
            let entries = entries.clone();
            let location = ui.file_tree_location.borrow().clone();
            let overlay = ui.file_tree_execution_overlay.borrow().clone();
            let hosts = ui.config.borrow().remote_hosts.clone();
            let total = entries.len();
            let mut affected: Vec<PathBuf> = Vec::new();
            for entry in &entries {
                if let Some(parent) = entry.path.parent() {
                    let parent = parent.to_path_buf();
                    if !affected.contains(&parent) {
                        affected.push(parent);
                    }
                }
            }
            let failures: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let failures_for_work = failures.clone();
            let ui_for_success = ui.clone();
            ui.execute_fs_op(
                "Delete",
                None,
                affected,
                move || {
                    for entry in &entries {
                        if let Err(error) =
                            remote_fs::delete_with_overlay(&location, &hosts, &overlay, &entry.path)
                        {
                            let name =
                                jterm_core::review_input::safe_inline_display(&entry.name, 256);
                            let detail = public_file_operation_error_message(&error);
                            failures_for_work
                                .lock()
                                .unwrap()
                                .push(format!("{name}: {detail}"));
                        }
                    }
                    Ok(())
                },
                move || {
                    let failures = failures.lock().unwrap();
                    if let Some(summary) = failure_summary(
                        failures.len(),
                        total,
                        failures.first().map(String::as_str).unwrap_or_default(),
                    ) {
                        ui_for_success
                            .toast_overlay
                            .add_toast(adw::Toast::new(&summary));
                    }
                },
            );
        });
        dialog.present(Some(&self.window));
    }

    /// The persistent progress toast for a batch transfer: Cancel wired to
    /// the returned token, progress reports polled into the title. Shared by
    /// paste and drag-and-drop import.
    fn build_transfer_feedback(
        &self,
        title: &str,
        total: Option<u64>,
    ) -> (adw::Toast, remote_fs::CancelToken, mpsc::Sender<u64>) {
        let (progress_tx, progress_rx) = mpsc::channel::<u64>();
        let token = remote_fs::CancelToken::default();
        let busy = adw::Toast::new(title);
        busy.set_timeout(0);
        busy.set_button_label(Some("Cancel"));
        {
            let token = token.clone();
            let busy_for_cancel = busy.clone();
            busy.connect_button_clicked(move |_| {
                // Idempotent: racing a completion leaves a flag nobody reads
                // any more. The completion path reports the cancel neutrally.
                token.cancel();
                busy_for_cancel.dismiss();
            });
        }
        // The toast leaving the screen (completion, cancel, swipe) stops the
        // progress forwarding for good.
        let poll_done = Rc::new(Cell::new(false));
        {
            let poll_done = poll_done.clone();
            busy.connect_dismissed(move |_| poll_done.set(true));
        }
        {
            let busy_for_poll = busy.clone();
            let title = title.to_string();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                if poll_done.get() {
                    return glib::ControlFlow::Break;
                }
                let mut latest = None;
                while let Ok(bytes) = progress_rx.try_recv() {
                    latest = Some(bytes);
                }
                if let Some(bytes) = latest {
                    let progress_text = match total {
                        Some(total) => format!(
                            "{} / {}",
                            remote_fs::human_bytes(bytes),
                            remote_fs::human_bytes(total)
                        ),
                        None => remote_fs::human_bytes(bytes),
                    };
                    busy_for_poll.set_title(&format!("{title} {progress_text}"));
                }
                glib::ControlFlow::Continue
            });
        }
        self.toast_overlay.add_toast(busy.clone());
        (busy, token, progress_tx)
    }

    /// Paste `clip` into `target_dir`. Same location: a cut moves (rename), a
    /// copy duplicates, per item, continuing past failures. Different
    /// locations: streaming transfers through the round-2 machinery, with a
    /// cut deleting only the sources whose transfer succeeded. The clipboard
    /// clears only when a cut-paste fully succeeded.
    fn paste_file_tree_clipboard(&self, clip: FsClipboard, target_dir: PathBuf) {
        let location = self.file_tree_location.borrow().clone();
        let to_overlay = self.file_tree_execution_overlay.borrow().clone();
        let hosts = self.config.borrow().remote_hosts.clone();
        let cut = clip.cut;
        let total_items = clip.items.len();
        // Uploads (local sources) know their sizes up-front; downloads and
        // relays report transferred bytes only.
        let from = clip.loc.clone();
        let measure = from == FsLocation::Local;
        let same_filesystem = remote_fs::same_filesystem(&from, &location, &hosts);
        let plan_items = plan_paste(
            &clip.items,
            &target_dir,
            location == FsLocation::Local,
            measure,
        );

        if same_filesystem {
            // Same-location batch: per-item rename/copy, summary at the end.
            let mut affected: Vec<PathBuf> = vec![target_dir.clone()];
            if cut {
                for item in &clip.items {
                    if let Some(parent) = item.path.parent() {
                        let parent = parent.to_path_buf();
                        if !affected.contains(&parent) {
                            affected.push(parent);
                        }
                    }
                }
            }
            let failures: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let failures_for_work = failures.clone();
            let failures_for_success = failures.clone();
            // A saved and temporary representation can name the same remote
            // namespace. Prefer a live current socket, then a live clipboard
            // socket, then whichever saved profile can carry its configured
            // ControlPath. Every item uses this one immutable endpoint for
            // both source and destination, so cut is a direct rename.
            let (loc, operation_overlay) = remote_fs::same_filesystem_execution_endpoint(
                &from,
                &clip.overlay,
                &location,
                &to_overlay,
                &hosts,
            );
            let loc = loc.clone();
            let operation_overlay = operation_overlay.clone();
            let ui = self.clone();
            let ui_for_success = self.clone();
            let clipboard_to_clear = clip.clone();
            self.execute_fs_op(
                "Paste",
                None,
                affected,
                move || {
                    for item in &plan_items {
                        let name = jterm_core::review_input::safe_inline_display(
                            &item
                                .src
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            256,
                        );
                        if item.self_paste {
                            failures_for_work
                                .lock()
                                .unwrap()
                                .push(format!("{name}: source and target are the same"));
                            continue;
                        }
                        if item.collides {
                            failures_for_work
                                .lock()
                                .unwrap()
                                .push(format!("{name}: already exists"));
                            continue;
                        }
                        let result = if cut {
                            remote_fs::rename_with_overlay(
                                &loc,
                                &hosts,
                                &operation_overlay,
                                &item.src,
                                &item.dst,
                            )
                        } else {
                            remote_fs::copy_with_overlay(
                                &loc,
                                &hosts,
                                &operation_overlay,
                                &item.src,
                                &item.dst,
                            )
                        };
                        if let Err(error) = result {
                            let detail = public_file_operation_error_message(&error);
                            failures_for_work
                                .lock()
                                .unwrap()
                                .push(format!("{name}: {detail}"));
                        }
                    }
                    Ok(())
                },
                move || {
                    let failures = failures_for_success.lock().unwrap();
                    match failure_summary(
                        failures.len(),
                        total_items,
                        failures.first().map(String::as_str).unwrap_or_default(),
                    ) {
                        Some(summary) => {
                            ui_for_success
                                .toast_overlay
                                .add_toast(adw::Toast::new(&summary));
                        }
                        None if cut => {
                            let mut clipboard = ui.file_tree_clipboard.borrow_mut();
                            clear_clipboard_if_intent_matches(
                                &mut clipboard,
                                clipboard_to_clear.intent_id,
                            );
                        }
                        None => {}
                    }
                },
            );
            return;
        }

        // Cross-location batch: per-item streaming transfers with cumulative
        // progress, cancellation, and per-item failure collection.
        let plan = remote_fs::transfer_plan_with_hosts(&from, &location, &hosts);
        let verb: &'static str = if cut {
            "Move"
        } else {
            match plan {
                Some(remote_fs::TransferPlan::Download) => "Download",
                Some(remote_fs::TransferPlan::Upload) => "Upload",
                _ => "Transfer",
            }
        };
        let verb_ing: &'static str = if cut {
            "Moving"
        } else {
            match plan {
                Some(remote_fs::TransferPlan::Download) => "Downloading",
                Some(remote_fs::TransferPlan::Upload) => "Uploading",
                _ => "Transferring",
            }
        };
        let title = if total_items == 1 {
            let display = jterm_core::review_input::safe_inline_display(
                &clip.items[0]
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                256,
            );
            format!("{verb_ing} {display}…")
        } else {
            format!("{verb_ing} {total_items} items…")
        };
        let total = measure.then(|| plan_items.iter().map(|item| item.size).sum::<u64>());
        let (busy, token, progress_tx) = self.build_transfer_feedback(&title, total);

        // Only the destination parent is visible in this tree; the source
        // side lives on another location.
        let affected = vec![target_dir];
        let failures: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let failures_for_work = failures.clone();
        let failures_for_success = failures.clone();
        let to = location.clone();
        let ui = self.clone();
        let ui_for_success = self.clone();
        let clipboard_to_clear = clip.clone();
        self.execute_fs_op(
            verb,
            Some(busy),
            affected,
            move || {
                let mut completed_bytes = 0_u64;
                for item in &plan_items {
                    if token.is_cancelled() {
                        return Err(remote_fs::cancelled_error());
                    }
                    let name = jterm_core::review_input::safe_inline_display(
                        &item
                            .src
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        256,
                    );
                    if item.self_paste {
                        failures_for_work
                            .lock()
                            .unwrap()
                            .push(format!("{name}: source and target are the same"));
                        completed_bytes += item.size;
                        continue;
                    }
                    if item.collides {
                        failures_for_work
                            .lock()
                            .unwrap()
                            .push(format!("{name}: already exists"));
                        completed_bytes += item.size;
                        continue;
                    }
                    // Cumulative progress across items: each transfer reports
                    // its own bytes; the sink shifts them by the bytes of the
                    // items that already finished.
                    let base = completed_bytes;
                    let progress_tx = progress_tx.clone();
                    let item_control = remote_fs::TransferControl {
                        token: token.clone(),
                        progress: Some(std::sync::Arc::new(std::sync::Mutex::new(
                            move |bytes: u64| {
                                let _ = progress_tx.send(base + bytes);
                            },
                        ))),
                    };
                    match remote_fs::transfer_with_overlays(
                        &from,
                        &clip.overlay,
                        &hosts,
                        &item.src,
                        &to,
                        &to_overlay,
                        &item.dst,
                        item.is_dir,
                        &item_control,
                    ) {
                        Ok(()) => {
                            // A cut deletes only the source whose transfer
                            // actually succeeded.
                            if cut {
                                if let Err(error) =
                                    remote_fs::delete_with_overlay(
                                        &from,
                                        &hosts,
                                        &clip.overlay,
                                        &item.src,
                                    )
                                {
                                    let detail = public_file_operation_error_message(&error);
                                    failures_for_work.lock().unwrap().push(format!(
                                        "{name}: transferred, but the source could not be deleted: {detail}"
                                    ));
                                }
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            return Err(error);
                        }
                        Err(error) => {
                            let detail = public_file_operation_error_message(&error);
                            failures_for_work
                                .lock()
                                .unwrap()
                                .push(format!("{name}: {detail}"));
                        }
                    }
                    completed_bytes += item.size;
                }
                Ok(())
            },
            move || {
                let failures = failures_for_success.lock().unwrap();
                match failure_summary(
                    failures.len(),
                    total_items,
                    failures.first().map(String::as_str).unwrap_or_default(),
                ) {
                    Some(summary) => {
                        ui_for_success
                            .toast_overlay
                            .add_toast(adw::Toast::new(&summary));
                    }
                    None if cut => {
                        let mut clipboard = ui.file_tree_clipboard.borrow_mut();
                        clear_clipboard_if_intent_matches(
                            &mut clipboard,
                            clipboard_to_clear.intent_id,
                        );
                    }
                    None => {}
                }
            },
        );
    }

    /// Re-list one directory into its already-materialized store, in place:
    /// surviving rows keep their TreeListRow identity, so expansion state
    /// and cached child models everywhere else in the tree are untouched.
    /// A collapsed directory with a cached store is still refreshed; a
    /// never-materialized directory has no last-good snapshot to invalidate.
    fn refresh_dir_listing(&self, dir: &Path) {
        self.file_tree_model
            .refresh_directory(dir, Some(self.toast_overlay.clone()));
    }

    fn refresh_visible_stale_file_tree_directories(&self) {
        if !self.file_tree_location.borrow().is_remote() {
            return;
        }
        let candidates = self
            .file_tree_model
            .visible_stale_directories(Instant::now(), MAX_TTL_REFRESHES_PER_TICK);
        for dir in candidates {
            self.file_tree_model.refresh_directory_with_cause(
                &dir,
                Some(self.toast_overlay.clone()),
                DirectoryRefreshCause::AutoTtl,
            );
        }
    }

    /// Queue one blocking file operation on a worker thread. On success only
    /// the affected parent directories are re-listed, in place; failures
    /// toast and log. `busy`, when given, is dismissed on any completion.
    fn execute_fs_op<W, S>(
        &self,
        verb: &'static str,
        busy: Option<adw::Toast>,
        affected: Vec<PathBuf>,
        work: W,
        on_success: S,
    ) where
        W: FnOnce() -> io::Result<()> + Send + 'static,
        S: FnOnce() + 'static,
    {
        if !self.file_tree_model.committed_authority_is_current() {
            if let Some(busy) = &busy {
                busy.dismiss();
            }
            self.toast_overlay.add_toast(adw::Toast::new(
                "The filesystem profile changed; choose a location and retry",
            ));
            return;
        }
        let affected = unique_paths(affected);
        if let Some(current) = self.file_tree_operation_intent.get() {
            let next = current.checked_add(1);
            self.file_tree_operation_intent.set(next);
            if next.is_none() {
                self.toast_overlay.add_toast(adw::Toast::new(
                    "Automatic Remote Files was disabled after its operation counter was exhausted",
                ));
                self.invalidate_file_tree_remote_follow();
            }
        }
        self.file_tree_active_operations
            .set(self.file_tree_active_operations.get().saturating_add(1));
        let ui = self.clone();
        let expected_generation = self.file_tree_model.generation.get();
        let expected_location = self.file_tree_location.borrow().clone();
        let busy_for_apply = busy.clone();
        let apply = move |result: io::Result<()>| {
            ui.file_tree_active_operations
                .set(ui.file_tree_active_operations.get().saturating_sub(1));
            if let Some(busy) = &busy_for_apply {
                busy.dismiss();
            }
            let still_visible = ui.file_tree_model.generation.get() == expected_generation
                && *ui.file_tree_location.borrow() == expected_location;
            match result {
                Ok(()) => {
                    on_success();
                    if still_visible {
                        for dir in &affected {
                            ui.refresh_dir_listing(dir);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    // A deliberate cancel is not a failure: neutral note, no
                    // warning in the log. Work completed before the cancel
                    // still becomes visible, so refresh like a success would.
                    log::info!("sidebar {verb} cancelled");
                    if still_visible {
                        for dir in &affected {
                            ui.refresh_dir_listing(dir);
                        }
                    }
                    ui.toast_overlay.add_toast(adw::Toast::new("Cancelled"));
                }
                Err(error) => {
                    log::warn!("sidebar file operation {verb} failed: {error}");
                    // A remote process may have committed just before its
                    // connection failed. Re-list exact affected parents even
                    // on ambiguous failures; last-good content remains on
                    // screen if that reconciliation also fails.
                    if still_visible {
                        for dir in &affected {
                            ui.refresh_dir_listing(dir);
                        }
                    }
                    let detail = public_file_operation_error_message(&error);
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&format!("{verb} failed: {detail}")));
                }
            }
        };
        if let Err(error) = request_fs_op(work, apply) {
            self.file_tree_active_operations
                .set(self.file_tree_active_operations.get().saturating_sub(1));
            if let Some(busy) = &busy {
                busy.dismiss();
            }
            log::warn!("failed to start sidebar file operation {verb}: {error}");
            let detail = public_file_operation_error_message(&error);
            self.toast_overlay
                .add_toast(adw::Toast::new(&format!("{verb} failed: {detail}")));
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The FileEntry under a pointer position, resolved through the bound
/// TreeExpander rather than row-height math. Empty space yields `None`.
fn file_tree_row_at(file_tree: &ListView, x: f64, y: f64) -> Option<(TreeListRow, FileEntry)> {
    let root: gtk4::Widget = file_tree.clone().upcast();
    let mut widget = file_tree.pick(x, y, gtk4::PickFlags::DEFAULT)?;
    loop {
        if widget == root {
            return None;
        }
        if let Ok(expander) = widget.clone().downcast::<gtk4::TreeExpander>() {
            let row = expander.list_row()?;
            let entry = entry_from_row(&row)?;
            return Some((row, entry));
        }
        widget = widget.parent()?;
    }
}

/// The visible row widget under a pointer position (for drop hover
/// highlight), resolved the same way as `file_tree_row_at`.
fn file_tree_row_widget_at(file_tree: &ListView, x: f64, y: f64) -> Option<gtk4::Widget> {
    let root: gtk4::Widget = file_tree.clone().upcast();
    let mut widget = file_tree.pick(x, y, gtk4::PickFlags::DEFAULT)?;
    loop {
        if widget == root {
            return None;
        }
        if let Ok(expander) = widget.clone().downcast::<gtk4::TreeExpander>() {
            return Some(expander.child().unwrap_or_else(|| expander.upcast()));
        }
        widget = widget.parent()?;
    }
}

/// Move the drop hover highlight to `widget` (or nowhere). Only one row ever
/// carries the class.
fn set_drop_hover(hover: &Rc<RefCell<Option<gtk4::Widget>>>, widget: Option<gtk4::Widget>) {
    let mut hover = hover.borrow_mut();
    if *hover == widget {
        return;
    }
    if let Some(old) = hover.take() {
        old.remove_css_class("file-tree-drop-hover");
    }
    if let Some(new) = widget {
        new.add_css_class("file-tree-drop-hover");
        *hover = Some(new);
    }
}

/// Which name-taking dialog to present: the create operations target a
/// directory, rename targets an existing entry and prefills its name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameDialogKind {
    NewFile,
    NewFolder,
    Rename,
}

impl NameDialogKind {
    fn title(self) -> &'static str {
        match self {
            NameDialogKind::NewFile => "New File",
            NameDialogKind::NewFolder => "New Folder",
            NameDialogKind::Rename => "Rename",
        }
    }

    fn confirm_label(self) -> &'static str {
        match self {
            NameDialogKind::Rename => "Save",
            _ => "Create",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            NameDialogKind::NewFile => "New file",
            NameDialogKind::NewFolder => "New folder",
            NameDialogKind::Rename => "Rename",
        }
    }
}

/// Header label for the tree root: local roots abbreviate $HOME to `~`,
/// remote paths render in full (`~` would mean the wrong home).
fn root_display_label(location: &FsLocation, root: &Path) -> String {
    if *location == FsLocation::Local {
        display_path(root)
    } else {
        safe_file_label(&root.to_string_lossy())
    }
}

/// Build the location DropDown for the file-tree header. Its contents are
/// (re)filled by `UiState::refresh_file_tree_location_selector`.
pub(crate) fn build_file_tree_location_selector() -> gtk4::DropDown {
    let selector = gtk4::DropDown::default();
    selector.set_tooltip_text(Some("Choose which filesystem to browse"));
    selector.add_css_class("flat");
    selector.set_hexpand(false);
    selector.set_factory(Some(&file_tree_location_label_factory()));
    selector.set_list_factory(Some(&file_tree_location_label_factory()));
    selector.update_property(&[gtk4::accessible::Property::Label(
        "Choose file tree location",
    )]);
    selector
}

fn middle_elide_location_label(label: &str) -> String {
    let count = label.chars().count();
    if count <= MAX_LOCATION_LABEL_CHARS {
        return label.to_string();
    }
    let remaining = MAX_LOCATION_LABEL_CHARS - 1;
    // Give the suffix the odd spare character: cloud-provider domains and
    // the `(temporary)` status are especially useful at the right edge.
    let left = remaining / 2;
    let right = remaining - left;
    let mut compact = String::with_capacity(label.len().min(MAX_LOCATION_LABEL_CHARS * 4));
    compact.extend(label.chars().take(left));
    compact.push('…');
    compact.extend(label.chars().skip(count - right));
    compact
}

fn file_tree_location_label_factory() -> SignalListItemFactory {
    let factory = SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let label = gtk4::Label::new(None);
        label.set_xalign(0.0);
        label.set_max_width_chars(MAX_LOCATION_LABEL_CHARS as i32);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        list_item.set_child(Some(&label));
    });
    factory.connect_bind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let Some(label) = list_item
            .child()
            .and_then(|child| child.downcast::<gtk4::Label>().ok())
        else {
            return;
        };
        let Some(string) = list_item
            .item()
            .and_then(|item| item.downcast::<gtk4::StringObject>().ok())
        else {
            return;
        };
        let full = string.string();
        label.set_text(&middle_elide_location_label(&full));
        label.set_tooltip_text(Some(&full));
    });
    factory.connect_unbind(|_, object| {
        let Some(label) = object
            .downcast_ref::<gtk4::ListItem>()
            .and_then(gtk4::ListItem::child)
            .and_then(|child| child.downcast::<gtk4::Label>().ok())
        else {
            return;
        };
        label.set_text("");
        label.set_tooltip_text(None);
    });
    factory
}

/// Abbreviate the home directory to `~` for the header label.
fn display_path(path: &Path) -> String {
    if let Some(home) = home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            if relative.as_os_str().is_empty() {
                return "~".to_string();
            }
            return safe_file_label(&format!("~/{}", relative.to_string_lossy()));
        }
    }
    safe_file_label(&path.to_string_lossy())
}

/// Shell-quoted path plus the trailing space that separates it from whatever
/// the user types next. Obviously safe paths stay unquoted for readability.
fn file_insert_snippet(path: &str) -> Option<String> {
    crate::process::try_shell_quote_path(path).map(|path| format!("{path} "))
}

/// The clipboard payload for Copy Path: the full path text as-is, or `None`
/// when the path carries hidden or control text — the same refusal as file
/// activation, so neither path text channel can smuggle spoofing out.
fn copy_path_payload(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    crate::process::try_shell_quote_path(text.as_ref()).map(|_| text.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn navigation_point(path: &str) -> FileTreeNavigationPoint {
        FileTreeNavigationPoint {
            location: FsLocation::Local,
            overlay: FsExecutionOverlay::default(),
            root: PathBuf::from(path),
        }
    }

    fn remote_authority(host: &str) -> remote_fs::FilesystemIdentity {
        remote_fs::FilesystemIdentity::Remote {
            docker: false,
            host: host.to_string(),
            user: Some("dev".to_string()),
            stable_ssh_args: Vec::new(),
        }
    }

    fn queued_scan(authority: remote_fs::FilesystemIdentity, id: usize) -> ScanJob {
        let (tx, _rx) = mpsc::sync_channel(1);
        ScanJob {
            authority,
            loc: FsLocation::Local,
            hosts: Vec::new(),
            overlay: FsExecutionOverlay::default(),
            dir: PathBuf::from(format!("/{id}")),
            cancel: remote_fs::CancelToken::default(),
            tx,
            enqueued_at: Instant::now(),
            queued_depth: id,
        }
    }

    fn admitted(result: Result<Vec<ScanJob>, ()>) -> Vec<ScanJob> {
        match result {
            Ok(evicted) => evicted,
            Err(_) => panic!("scan should be admitted"),
        }
    }

    fn remote_profile(name: &str, target: &str) -> RemoteHost {
        RemoteHost {
            name: name.to_string(),
            host: target.to_string(),
            user: Some("dev".to_string()),
            docker: false,
            deploy_artifact: None,
            remote_shell: "jsh".to_string(),
            session: None,
            ssh_args: Vec::new(),
            login_shell: true,
            multiplex: true,
            deploy: jterm_core::jsh_remote::Deploy::Off,
        }
    }

    fn observed_target(target: &str) -> RemoteHostConfig {
        RemoteHostConfig {
            name: format!("dev@{target}"),
            host: target.to_string(),
            user: Some("dev".to_string()),
            docker: false,
            remote_shell: "jsh".to_string(),
            session: None,
            ssh_args: vec!["-p".to_string(), "2222".to_string()],
            deploy: "off".to_string(),
            deploy_artifact: None,
        }
    }

    #[test]
    fn long_dsw_location_label_keeps_identifying_prefix_and_suffix() {
        let full = "ssh: root@dsw-notebook-dsw-l8rnh0wm7vs81o7z6j-22.vpc-0jlbz3pri2042fd5xw2ov.instance-forward.dsw.cn-wulanchabu.aliyuncs.com (temporary)";
        let compact = middle_elide_location_label(full);
        assert!(compact.chars().count() <= MAX_LOCATION_LABEL_CHARS);
        assert!(compact.starts_with("ssh: root@dsw"));
        assert!(compact.ends_with("aliyuncs.com (temporary)"));
        assert!(compact.contains('…'));
    }

    #[test]
    fn scan_queue_has_hard_backpressure_and_navigation_can_preempt_lazy_work() {
        let mut queue = ScanQueue::new(2);
        assert_eq!(queue.push(ScanPriority::Lazy, 1), Ok(None));
        assert_eq!(queue.push(ScanPriority::Lazy, 2), Ok(None));
        assert_eq!(queue.push(ScanPriority::Lazy, 3), Err(3));
        assert_eq!(queue.len(), 2);

        assert_eq!(queue.push(ScanPriority::Root, 3), Ok(Some(2)));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop(), Some(3));
        assert_eq!(queue.pop(), Some(1));
    }

    #[test]
    fn scan_queue_weighting_prioritizes_navigation_without_starving_lazy_work() {
        let mut queue = ScanQueue::new(32);
        for index in 0..8 {
            for priority in [ScanPriority::Root, ScanPriority::Manual, ScanPriority::Lazy] {
                queue.push(priority, (priority, index)).unwrap();
            }
        }
        let served: Vec<_> = (0..SCAN_SERVICE_ORDER.len())
            .map(|_| queue.pop().expect("all priority lanes remain non-empty").0)
            .collect();
        assert_eq!(served, SCAN_SERVICE_ORDER);
        assert!(served.contains(&ScanPriority::Lazy));
    }

    #[test]
    fn scan_scheduler_round_robins_authorities_and_caps_each_remote_at_two() {
        let authority_a = remote_authority("a.example");
        let authority_b = remote_authority("b.example");
        let mut state = ScanSchedulerState {
            queue: ScanQueue::new(16),
            authority_order: VecDeque::new(),
            running_by_authority: std::collections::HashMap::new(),
        };
        for id in 0..3 {
            for authority in [&authority_a, &authority_b] {
                state.register_authority(authority);
                state
                    .queue
                    .push(ScanPriority::Lazy, queued_scan(authority.clone(), id))
                    .map_err(|_| ())
                    .unwrap();
            }
        }

        let first = state.pop_next().unwrap();
        let second = state.pop_next().unwrap();
        let third = state.pop_next().unwrap();
        let fourth = state.pop_next().unwrap();
        assert_eq!(first.authority, authority_a);
        assert_eq!(second.authority, authority_b);
        assert_eq!(third.authority, authority_a);
        assert_eq!(fourth.authority, authority_b);
        assert!(state.pop_next().is_none(), "both authorities are at cap 2");

        state.finish(&authority_a);
        assert_eq!(state.pop_next().unwrap().authority, authority_a);
        assert_eq!(scan_authority_limit(&authority_a), 2);
        assert_eq!(
            scan_authority_limit(&remote_fs::FilesystemIdentity::Local),
            MAX_CONCURRENT_SCANS
        );
    }

    #[test]
    fn scan_admission_caps_one_remote_without_blocking_another_root() {
        let authority_a = remote_authority("a.example");
        let authority_b = remote_authority("b.example");
        let mut state = ScanSchedulerState {
            queue: ScanQueue::new(MAX_PENDING_SCANS),
            authority_order: VecDeque::new(),
            running_by_authority: std::collections::HashMap::new(),
        };
        for id in 0..MAX_PENDING_SCANS_PER_REMOTE_AUTHORITY {
            assert!(admitted(
                state.admit(ScanPriority::Lazy, queued_scan(authority_a.clone(), id),)
            )
            .is_empty());
        }
        assert!(state
            .admit(
                ScanPriority::Lazy,
                queued_scan(authority_a.clone(), usize::MAX),
            )
            .is_err());
        assert_eq!(
            state.queue.count_where(|job| job.authority == authority_a),
            MAX_PENDING_SCANS_PER_REMOTE_AUTHORITY
        );

        assert!(admitted(state.admit(
            ScanPriority::Root,
            queued_scan(authority_b.clone(), usize::MAX - 1),
        ))
        .is_empty());
        assert_eq!(
            state.queue.count_where(|job| job.authority == authority_b),
            1,
            "A's pending burst cannot make B's first Root return WouldBlock"
        );

        let evicted = admitted(state.admit(
            ScanPriority::Root,
            queued_scan(authority_a.clone(), usize::MAX - 2),
        ));
        assert_eq!(evicted.len(), 1, "Root replaces A's newest Lazy at its cap");
        assert_eq!(
            state.queue.count_where(|job| job.authority == authority_a),
            MAX_PENDING_SCANS_PER_REMOTE_AUTHORITY
        );
    }

    #[test]
    fn global_saturation_preserves_first_interactive_slot_for_a_new_authority() {
        let mut state = ScanSchedulerState {
            queue: ScanQueue::new(MAX_PENDING_SCANS),
            authority_order: VecDeque::new(),
            running_by_authority: std::collections::HashMap::new(),
        };
        for authority_index in 0..4 {
            let authority = remote_authority(&format!("{authority_index}.example"));
            for job_index in 0..MAX_PENDING_SCANS_PER_REMOTE_AUTHORITY {
                admitted(state.admit(
                    ScanPriority::Root,
                    queued_scan(
                        authority.clone(),
                        authority_index * MAX_PENDING_SCANS_PER_REMOTE_AUTHORITY + job_index,
                    ),
                ));
            }
        }
        assert_eq!(state.queue.len(), MAX_PENDING_SCANS);

        let newcomer = remote_authority("new.example");
        let evicted = admitted(state.admit(
            ScanPriority::Root,
            queued_scan(newcomer.clone(), usize::MAX),
        ));
        assert_eq!(evicted.len(), 1);
        assert_eq!(state.queue.len(), MAX_PENDING_SCANS);
        assert_eq!(state.queue.count_where(|job| job.authority == newcomer), 1);
    }

    #[test]
    fn transactional_navigation_rejects_old_results_and_failure_keeps_commit() {
        let mut state = FileTreeNavigationState::default();
        let original = navigation_point("/original");
        state.install_initial(original.clone());

        let old = state.begin(navigation_point("/slow"), FileTreeNavigationAction::Push);
        let latest = state.begin(navigation_point("/latest"), FileTreeNavigationAction::Push);
        assert!(old.cancel.is_cancelled());
        assert!(!state.commit(&old), "an out-of-order answer is rejected");
        assert!(state.fail(&latest));
        assert_eq!(state.current, Some(original));
        assert!(state.back.is_empty());
        assert!(state.forward.is_empty());
        assert!(state.pending.is_none());
    }

    #[test]
    fn transactional_navigation_history_is_bounded_and_branches_on_commit() {
        let mut state = FileTreeNavigationState::default();
        state.install_initial(navigation_point("/0"));
        for index in 1..=70 {
            let request = state.begin(
                navigation_point(&format!("/{index}")),
                FileTreeNavigationAction::Push,
            );
            assert!(state.commit(&request));
        }
        assert_eq!(state.back.len(), MAX_FILE_TREE_HISTORY);
        assert_eq!(state.back.front(), Some(&navigation_point("/6")));

        let back = state.back_target().unwrap();
        let request = state.begin(back.clone(), FileTreeNavigationAction::Back);
        assert!(state.commit(&request));
        assert_eq!(state.current, Some(back));
        assert_eq!(state.forward_target(), Some(navigation_point("/70")));

        let branch = state.begin(navigation_point("/branch"), FileTreeNavigationAction::Push);
        assert!(state.commit(&branch));
        assert!(
            state.forward.is_empty(),
            "a committed branch clears Forward"
        );
    }

    #[test]
    fn reselecting_committed_location_dispatches_and_cancels_pending_navigation() {
        assert_eq!(
            file_tree_location_from_selection(0, 2, &FsLocation::Local),
            Some(FsLocation::Local),
            "the dropdown must dispatch committed A while staged B is pending"
        );
        let mut state = FileTreeNavigationState::default();
        let original = navigation_point("/original");
        state.install_initial(original.clone());
        let pending = state.begin(navigation_point("/pending"), FileTreeNavigationAction::Push);

        assert!(state.cancel_pending());
        assert!(pending.cancel.is_cancelled());
        assert!(!state.commit(&pending));
        assert_eq!(state.current, Some(original));
        assert!(!state.cancel_pending());
    }

    #[test]
    fn history_profile_remap_updates_exact_targets_and_drops_unprovable_ones() {
        let mut state = FileTreeNavigationState::default();
        let mut saved_zero = navigation_point("/zero");
        saved_zero.location = FsLocation::Remote(0);
        let local = navigation_point("/local");
        let mut removed = navigation_point("/removed");
        removed.location = FsLocation::Remote(1);
        state.back = VecDeque::from([saved_zero, local.clone(), removed]);

        state.remap_history_locations(|location| match location {
            FsLocation::Remote(0) => Some(FsLocation::Remote(7)),
            FsLocation::Remote(_) => None,
            _ => Some(location.clone()),
        });

        assert_eq!(state.back.len(), 2);
        assert_eq!(state.back[0].location, FsLocation::Remote(7));
        assert_eq!(state.back[1], local);
    }

    #[test]
    fn navigation_path_validation_is_absolute_normalized_and_spoof_safe() {
        assert_eq!(
            validate_absolute_navigation_path("/srv/./app/../logs"),
            Ok(PathBuf::from("/srv/logs"))
        );
        assert_eq!(
            navigation_breadcrumbs(Path::new("/srv/logs")),
            vec![
                PathBuf::from("/"),
                PathBuf::from("/srv"),
                PathBuf::from("/srv/logs")
            ]
        );
        assert!(validate_absolute_navigation_path("relative/path").is_err());
        assert!(validate_absolute_navigation_path("/../../escape").is_err());
        assert!(validate_absolute_navigation_path("/safe\nspoof").is_err());
        assert!(validate_absolute_navigation_path("/safe\u{202e}spoof").is_err());
        assert!(validate_absolute_navigation_path(&format!(
            "/{}",
            "x".repeat(MAX_NAVIGATION_PATH_BYTES)
        ))
        .is_err());
    }

    #[test]
    fn cancelled_queued_scans_are_physically_removed_and_capacity_is_stable() {
        #[derive(Clone)]
        struct Queued {
            token: remote_fs::CancelToken,
            id: usize,
        }

        let mut queue = ScanQueue::new(MAX_PENDING_SCANS);
        let mut tokens = Vec::new();
        for id in 0..MAX_PENDING_SCANS {
            let token = remote_fs::CancelToken::default();
            tokens.push(token.clone());
            queue
                .push(ScanPriority::Lazy, Queued { token, id })
                .map_err(|_| ())
                .unwrap();
        }
        for token in tokens.iter().step_by(2) {
            token.cancel();
        }
        let retired = queue.remove_where(|job| job.token.is_cancelled());
        assert_eq!(retired.len(), MAX_PENDING_SCANS / 2);
        assert_eq!(queue.len(), MAX_PENDING_SCANS / 2);
        assert!(retired.iter().all(|job| job.id % 2 == 0));

        for id in MAX_PENDING_SCANS..10_000 {
            let token = remote_fs::CancelToken::default();
            let _ = queue.push(ScanPriority::Lazy, Queued { token, id });
            assert!(queue.len() <= MAX_PENDING_SCANS);
        }
    }

    #[test]
    fn fs_op_scheduler_uses_fixed_workers_and_hard_queue_backpressure() {
        let scheduler = FsOpScheduler::new(1, 1);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        scheduler
            .enqueue(Box::new(move || {
                started_tx.send(()).expect("worker reports startup");
                release_rx.recv().expect("test releases worker");
            }))
            .unwrap();
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fixed worker starts the first operation");

        scheduler.enqueue(Box::new(|| {})).unwrap();
        let error = scheduler
            .enqueue(Box::new(|| {}))
            .expect_err("the bounded queue rejects excess work");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        release_tx.send(()).unwrap();
    }

    #[test]
    fn observed_ssh_prefers_one_valid_configured_filesystem_authority() {
        let target = observed_target("build.example");
        let mut matching = remote_profile("saved", "build.example");
        matching.ssh_args = target.ssh_args.clone();
        let other = remote_profile("other", "other.example");
        assert_eq!(
            observed_target_location(&target, &[other, matching]),
            FsLocation::Remote(1)
        );
    }

    #[test]
    fn committed_authority_rejects_numeric_profile_reinterpretation() {
        let original = remote_profile("original", "a.example");
        let expected =
            remote_fs::filesystem_identity(&FsLocation::Remote(0), std::slice::from_ref(&original))
                .unwrap();
        assert!(committed_authority_matches(
            Some(&expected),
            &FsLocation::Remote(0),
            std::slice::from_ref(&original)
        ));

        let replacement = remote_profile("replacement", "b.example");
        assert!(!committed_authority_matches(
            Some(&expected),
            &FsLocation::Remote(0),
            &[replacement]
        ));
        assert!(!committed_authority_matches(None, &FsLocation::Local, &[]));
    }

    #[test]
    fn observed_process_argv_for_user_ssh_command_becomes_a_temporary_location() {
        let argv = ["ssh", "root@dsw-notebook.example.com", "-p", "22"].map(str::to_string);
        let jterm_core::jsh_remote::ObservedSshTarget::Target(target) =
            jterm_core::jsh_remote::observed_ssh_target(&argv)
        else {
            panic!("expected an interactive SSH target");
        };
        assert_eq!(target.host, "dsw-notebook.example.com");
        assert_eq!(target.user.as_deref(), Some("root"));
        assert_eq!(target.ssh_args, ["-p", "22"]);
        assert_eq!(
            observed_target_location(&target, &[]),
            FsLocation::Transient(target)
        );
    }

    #[test]
    fn actual_jsh_upgrade_launcher_keeps_socket_out_of_target_identity() {
        let argv = [
            "/bin/sh",
            "/home/alice/.cache/jsh/jsh-remote.sh",
            "--persist",
            "--local-jsh",
            "/home/alice/.local/bin/jsh",
            "root@dsw-notebook-dsw-l8rnh0wm7vs81o7z6j-22.vpc.example.com",
            "--",
            "-p",
            "22",
        ]
        .map(str::to_string)
        .to_vec();
        let raw_target = match jterm_core::jsh_remote::observed_ssh_target(&argv) {
            jterm_core::jsh_remote::ObservedSshTarget::Target(target) => target,
            other => panic!("expected launcher target, got {other:?}"),
        };
        let command = jterm_core::process::ObservedSshCommand {
            argv: argv.clone(),
            target: jterm_core::jsh_remote::ObservedSshTarget::Target(raw_target),
            reusable_control_path: Some("/run/user/1000/anvil/cm-%C".to_string()),
        };
        let jterm_core::process::ObservedSshCommand {
            target: jterm_core::jsh_remote::ObservedSshTarget::Target(raw_target),
            reusable_control_path,
            ..
        } = command
        else {
            unreachable!()
        };
        let (target, overlay) =
            remote_fs::observed_target_and_overlay(raw_target, reusable_control_path).unwrap();
        assert_eq!(target.user.as_deref(), Some("root"));
        assert_eq!(target.ssh_args, ["-p", "22"]);
        assert_eq!(
            observed_target_location(&target, &[]),
            FsLocation::Transient(target.clone())
        );

        let (_, plain) = remote_fs::plain_interactive_ssh_argv(&target, &overlay).unwrap();
        assert_eq!(plain[0..2], ["ssh", "-t"]);
        assert!(plain
            .windows(2)
            .any(|pair| { pair == ["-S".to_string(), "/run/user/1000/anvil/cm-%C".to_string()] }));
        assert_eq!(
            plain.last().map(String::as_str),
            Some("root@dsw-notebook-dsw-l8rnh0wm7vs81o7z6j-22.vpc.example.com")
        );
        assert_eq!(plain.len(), 8, "plain SSH must not append a remote command");
    }

    #[test]
    fn explicit_control_path_is_execution_only_and_saved_matching_ignores_it() {
        let argv = [
            "ssh",
            "-S",
            "/run/user/1000/live-cm",
            "-p",
            "2222",
            "dev@build.example",
        ]
        .map(str::to_string);
        let raw_target = match jterm_core::jsh_remote::observed_ssh_target(&argv) {
            jterm_core::jsh_remote::ObservedSshTarget::Target(target) => target,
            other => panic!("expected SSH target, got {other:?}"),
        };
        assert!(raw_target.ssh_args.windows(2).any(|pair| pair[0] == "-S"));
        let (target, overlay) = remote_fs::observed_target_and_overlay(raw_target, None).unwrap();
        assert_eq!(target.ssh_args, ["-p", "2222"]);

        let mut saved = remote_profile("saved", "build.example");
        saved.ssh_args = vec![
            "-p".to_string(),
            "2222".to_string(),
            "-o".to_string(),
            "ControlPath=/run/user/1000/saved-cm".to_string(),
        ];
        assert_eq!(
            observed_target_location(&target, &[saved]),
            FsLocation::Remote(0)
        );
        let (_, plain) = remote_fs::plain_interactive_ssh_argv(&target, &overlay).unwrap();
        assert!(plain
            .windows(2)
            .any(|pair| { pair == ["-S".to_string(), "/run/user/1000/live-cm".to_string()] }));
    }

    #[test]
    fn observed_ssh_uses_immutable_transient_for_missing_or_ambiguous_profiles() {
        let target = observed_target("build.example");
        let mut first = remote_profile("first", "build.example");
        first.ssh_args = target.ssh_args.clone();
        let mut second = first.clone();
        second.name = "second".to_string();

        assert_eq!(
            observed_target_location(&target, &[]),
            FsLocation::Transient(target.clone())
        );
        assert_eq!(
            observed_target_location(&target, &[first, second]),
            FsLocation::Transient(target)
        );
    }

    #[test]
    fn transient_locations_survive_unrelated_config_reconciliation() {
        let target = observed_target("build.example");
        let location = FsLocation::Transient(target);
        assert_eq!(
            remap_remote_location(
                &location,
                &[remote_profile("old", "old.example")],
                &[remote_profile("new", "new.example")],
            ),
            Some(location)
        );
    }

    #[test]
    fn remote_follow_commit_requires_token_and_unchanged_user_navigation() {
        let location = FsLocation::Local;
        let root = Path::new("/work");
        let expected = RemoteFollowContext {
            intent: 7,
            operation_intent: 9,
            tree_generation: 11,
            tab_focus_generation: 13,
            source_focus_serial: 17,
            location: location.clone(),
            root: root.to_path_buf(),
        };
        assert!(expected.matches(&expected));

        for stale in [
            RemoteFollowContext {
                intent: 8,
                ..expected.clone()
            },
            RemoteFollowContext {
                operation_intent: 10,
                ..expected.clone()
            },
            RemoteFollowContext {
                tree_generation: 12,
                ..expected.clone()
            },
            RemoteFollowContext {
                tab_focus_generation: 14,
                ..expected.clone()
            },
            RemoteFollowContext {
                source_focus_serial: 18,
                ..expected.clone()
            },
            RemoteFollowContext {
                location: FsLocation::Remote(0),
                ..expected.clone()
            },
            RemoteFollowContext {
                root: PathBuf::from("/elsewhere"),
                ..expected.clone()
            },
        ] {
            assert!(!expected.matches(&stale));
        }
    }

    #[test]
    fn remote_follow_dedupe_rearms_for_focus_aba_but_not_file_operations() {
        let argv = ["ssh", "dev@build.example"].map(str::to_string).to_vec();
        let observed = crate::ui::FileTreeRemoteObservation {
            source_session: "pane-a".to_string(),
            argv: argv.clone(),
            tab_focus_generation: 41,
            source_focus_serial: 43,
        };

        // An unrelated Files operation advances its own intent but must leave
        // this exact process consumed, preventing an implicit retry afterward.
        let _operation_intent_after_user_action = 10_u64;
        assert!(observed.matches("pane-a", &argv, 41, 43));

        // Returning to pane A after even a fast A -> B -> A focus round trip
        // carries a new epoch and therefore permits a fresh staged probe.
        assert!(!observed.matches("pane-a", &argv, 42, 43));
        assert!(!observed.matches("pane-a", &argv, 41, 44));
        assert!(!observed.matches("pane-b", &argv, 41, 43));
        assert!(!observed.matches(
            "pane-a",
            &["ssh", "dev@other.example"].map(str::to_string),
            41,
            43,
        ));
    }

    #[test]
    fn pending_location_home_probe_rejects_reused_remote_index() {
        let profile_b = remote_profile("B", "b.example");
        let expected_location = FsLocation::Remote(0);
        let expected_authority =
            remote_fs::filesystem_identity(&expected_location, std::slice::from_ref(&profile_b))
                .unwrap();
        assert!(location_home_probe_is_current(
            41,
            41,
            &expected_location,
            &expected_authority,
            std::slice::from_ref(&profile_b),
        ));

        let profile_c = remote_profile("C", "c.example");
        assert!(
            !location_home_probe_is_current(
                41,
                41,
                &expected_location,
                &expected_authority,
                &[profile_c],
            ),
            "a B home result is inert after Remote(0) is reused for C"
        );
        assert!(
            !location_home_probe_is_current(
                41,
                42,
                &expected_location,
                &expected_authority,
                &[profile_b],
            ),
            "a newer user/follow intent also retires the probe"
        );
    }

    #[test]
    fn remote_location_remaps_only_the_exact_unique_profile() {
        let alpha = remote_profile("alpha", "alpha.example");
        let beta = remote_profile("beta", "beta.example");
        let previous = vec![alpha.clone(), beta.clone()];

        assert_eq!(
            remap_remote_location(
                &FsLocation::Remote(0),
                &previous,
                &[beta.clone(), alpha.clone()],
            ),
            Some(FsLocation::Remote(1))
        );
        assert_eq!(
            remap_remote_location(
                &FsLocation::Remote(0),
                &previous,
                std::slice::from_ref(&beta),
            ),
            None
        );

        let mut edited = alpha.clone();
        edited.host = "replacement.example".to_string();
        assert_eq!(
            remap_remote_location(&FsLocation::Remote(0), &previous, &[edited, beta]),
            None
        );
        assert_eq!(
            remap_remote_location(&FsLocation::Remote(0), &previous, &[alpha.clone(), alpha],),
            None
        );
        assert_eq!(
            remap_remote_location(&FsLocation::Local, &previous, &[]),
            Some(FsLocation::Local)
        );

        let mut invalid = remote_profile("invalid", "-not-a-target");
        let invalid_previous = vec![invalid.clone()];
        assert_eq!(
            remap_remote_location(
                &FsLocation::Remote(0),
                &invalid_previous,
                std::slice::from_ref(&invalid),
            ),
            None
        );
        invalid.host = "valid.example".to_string();
        assert_eq!(
            remap_remote_location(
                &FsLocation::Remote(crate::config::MAX_REMOTE_HOSTS),
                &invalid_previous,
                &[invalid],
            ),
            None
        );
    }

    #[test]
    fn managed_tab_follow_requires_one_exact_profile_except_for_saved_session() {
        let alpha = remote_profile("alpha", "alpha.example");
        let beta = remote_profile("beta", "beta.example");
        let mut restored_connection = alpha.clone();
        restored_connection.session = Some("saved-tab-session".to_string());

        assert_eq!(
            unique_remote_connection_profile_index(
                &restored_connection,
                &[beta.clone(), alpha.clone()],
                true,
            ),
            Some(1)
        );
        assert_eq!(
            unique_remote_connection_profile_index(
                &restored_connection,
                std::slice::from_ref(&alpha),
                false,
            ),
            None,
            "a fresh connection receives no session-identity exemption"
        );

        let mut same_name_replacement = alpha.clone();
        same_name_replacement.host = "replacement.example".to_string();
        assert_eq!(
            unique_remote_connection_profile_index(
                &restored_connection,
                &[same_name_replacement],
                true,
            ),
            None,
            "a live tab name must not redirect its OSC 7 cwd to a replacement target"
        );
        assert_eq!(
            unique_remote_connection_profile_index(
                &restored_connection,
                &[alpha.clone(), alpha],
                true,
            ),
            None,
            "ambiguous duplicate profiles fail closed"
        );
    }

    #[test]
    fn delayed_file_tree_actions_require_the_same_generation_and_location() {
        assert!(file_tree_context_matches(
            7,
            &FsLocation::Remote(2),
            7,
            &FsLocation::Remote(2),
        ));
        assert!(!file_tree_context_matches(
            7,
            &FsLocation::Remote(2),
            8,
            &FsLocation::Remote(2),
        ));
        assert!(!file_tree_context_matches(
            7,
            &FsLocation::Remote(2),
            7,
            &FsLocation::Local,
        ));
    }

    #[test]
    fn completed_cut_retires_only_its_original_clipboard_intent() {
        let original = FsClipboard {
            intent_id: 7,
            loc: FsLocation::Remote(0),
            overlay: FsExecutionOverlay::default(),
            items: vec![remote_fs::FsClipboardItem {
                path: PathBuf::from("/old"),
                is_dir: false,
            }],
            cut: true,
        };
        let newer = FsClipboard {
            intent_id: 8,
            ..original.clone()
        };
        let mut slot = Some(newer.clone());
        assert!(clipboard_for_intent(&slot, original.intent_id).is_none());
        assert!(!clear_clipboard_if_intent_matches(
            &mut slot,
            original.intent_id
        ));
        assert_eq!(slot, Some(newer));

        // Reordering the exact remote profile changes its numeric location,
        // but it is still the same clipboard intent and must be retired.
        let mut remapped_original = original.clone();
        remapped_original.loc = FsLocation::Remote(3);
        let mut slot = Some(remapped_original);
        assert_eq!(
            clipboard_for_intent(&slot, original.intent_id)
                .expect("the live remapped payload resolves")
                .loc,
            FsLocation::Remote(3)
        );
        assert!(clear_clipboard_if_intent_matches(
            &mut slot,
            original.intent_id
        ));
        assert!(slot.is_none());
    }

    #[test]
    fn entries_sort_directories_first_then_by_name() {
        let mut entries = vec![
            FileEntry {
                name: "Zulu.txt".into(),
                path: PathBuf::from("Zulu.txt"),
                is_dir: false,
                status: None,
            },
            FileEntry {
                name: "beta".into(),
                path: PathBuf::from("beta"),
                is_dir: true,
                status: None,
            },
            FileEntry {
                name: "Alpha.txt".into(),
                path: PathBuf::from("Alpha.txt"),
                is_dir: false,
                status: None,
            },
            FileEntry {
                name: "Able".into(),
                path: PathBuf::from("Able"),
                is_dir: true,
                status: None,
            },
        ];

        sort_entries(&mut entries);

        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["Able", "beta", "Alpha.txt", "Zulu.txt"]);
    }

    #[test]
    fn inserted_snippets_quote_unsafe_paths_and_keep_safe_paths_readable() {
        assert_eq!(
            file_insert_snippet("a'b c").as_deref(),
            Some("'a'\"'\"'b c' ")
        );
        assert_eq!(
            file_insert_snippet("/home/u/notes.txt").as_deref(),
            Some("/home/u/notes.txt ")
        );
        assert_eq!(file_insert_snippet("left\nright"), None);
        assert_eq!(file_insert_snippet("left\u{202e}right"), None);
    }

    #[test]
    fn file_labels_make_hidden_text_visible_and_stay_bounded() {
        assert_eq!(safe_file_label("safe\u{202e}\x1btxt"), "safe��txt");
        assert!(safe_file_label(&"界".repeat(MAX_FILE_LABEL_BYTES)).len() <= MAX_FILE_LABEL_BYTES);
    }

    #[test]
    fn hidden_name_detection_only_matches_dot_prefixed_entries() {
        assert!(file_name_is_hidden(".git"));
        assert!(file_name_is_hidden(".env.local"));
        assert!(!file_name_is_hidden("README.md"));
        assert!(!file_name_is_hidden("dot.file"));
        assert!(!file_name_is_hidden("."));
        assert!(!file_name_is_hidden(".."));
    }

    #[test]
    fn file_tree_refresh_owns_only_unmodified_f5() {
        use gtk4::gdk::{Key, ModifierType};
        assert!(file_tree_is_plain_refresh_key(
            Key::F5,
            ModifierType::empty()
        ));
        assert!(file_tree_is_plain_refresh_key(
            Key::F5,
            ModifierType::LOCK_MASK | ModifierType::BUTTON1_MASK
        ));
        for modifier in [
            ModifierType::CONTROL_MASK,
            ModifierType::SHIFT_MASK,
            ModifierType::ALT_MASK,
            ModifierType::SUPER_MASK,
            ModifierType::HYPER_MASK,
            ModifierType::META_MASK,
        ] {
            assert!(!file_tree_is_plain_refresh_key(Key::F5, modifier));
        }
        assert!(!file_tree_is_plain_refresh_key(
            Key::F4,
            ModifierType::empty()
        ));
        assert_eq!(
            file_tree_navigation_key(Key::F5, ModifierType::empty()),
            Some(FileTreeNavigationKey::Refresh)
        );
        assert_eq!(
            file_tree_navigation_key(Key::Up, ModifierType::ALT_MASK),
            Some(FileTreeNavigationKey::Up)
        );
        assert_eq!(
            file_tree_navigation_key(Key::Home, ModifierType::ALT_MASK),
            Some(FileTreeNavigationKey::Home)
        );
        assert_eq!(
            file_tree_navigation_key(Key::Right, ModifierType::ALT_MASK),
            Some(FileTreeNavigationKey::EnterDirectory)
        );
        assert_eq!(
            file_tree_navigation_key(Key::Home, ModifierType::empty()),
            None,
            "plain Home remains GTK list navigation"
        );
        assert_eq!(
            file_tree_navigation_key(Key::Up, ModifierType::ALT_MASK | ModifierType::CONTROL_MASK),
            None,
            "modified shortcuts are not over-claimed"
        );
    }

    #[test]
    fn directory_scan_revisions_accept_only_the_latest_request_per_directory() {
        let mut revisions = DirectoryScanRevisions::new();
        let logs = Path::new("/remote/logs");
        let src = Path::new("/remote/src");

        let old_logs = issue_directory_scan_revision(&mut revisions, logs);
        let src_revision = issue_directory_scan_revision(&mut revisions, src);
        let new_logs = issue_directory_scan_revision(&mut revisions, logs);

        assert!(old_logs.cancel.is_cancelled());
        assert!(!new_logs.cancel.is_cancelled());
        assert!(!src_revision.cancel.is_cancelled());
        assert!(!directory_scan_revision_is_current(
            &revisions,
            logs,
            old_logs.revision
        ));
        assert!(directory_scan_revision_is_current(
            &revisions,
            logs,
            new_logs.revision
        ));
        assert!(directory_scan_revision_is_current(
            &revisions,
            src,
            src_revision.revision
        ));
        assert!(directory_scan_revision_is_pending(
            &revisions,
            logs,
            new_logs.revision
        ));
        assert!(complete_directory_scan_revision(
            &mut revisions,
            logs,
            new_logs.revision
        ));
        assert!(!directory_scan_revision_is_pending(
            &revisions,
            logs,
            new_logs.revision
        ));
        assert!(!complete_directory_scan_revision(
            &mut revisions,
            logs,
            old_logs.revision
        ));

        cancel_directory_scans(&revisions);
        assert!(new_logs.cancel.is_cancelled());
        assert!(src_revision.cancel.is_cancelled());
    }

    #[test]
    fn directory_scan_has_a_hard_entry_limit() {
        let root = std::env::temp_dir().join(format!(
            "forge-file-tree-limit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..MAX_DIRECTORY_ENTRIES + 8 {
            std::fs::write(root.join(format!("entry-{index}")), []).unwrap();
        }
        let entries = scan_dir(&root).unwrap();
        assert_eq!(entries.len(), MAX_DIRECTORY_ENTRIES);
        let _ = std::fs::remove_dir_all(root);
    }

    fn store_paths(store: &gio::ListStore) -> Vec<PathBuf> {
        (0..store.n_items())
            .filter_map(|index| {
                store
                    .item(index)
                    .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                    .and_then(|boxed| {
                        boxed
                            .try_borrow::<FileEntry>()
                            .ok()
                            .filter(|entry| entry.is_item())
                            .map(|entry| entry.path.clone())
                    })
            })
            .collect()
    }

    fn store_entry_for_test(store: &gio::ListStore, index: u32) -> FileEntry {
        let boxed = store
            .item(index)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .expect("test store entry");
        let entry = boxed.try_borrow::<FileEntry>().expect("FileEntry payload");
        (*entry).clone()
    }

    fn test_entry(path: &str, is_dir: bool) -> FileEntry {
        FileEntry {
            name: path.to_string(),
            path: PathBuf::from(path),
            is_dir,
            status: None,
        }
    }

    #[test]
    fn copy_path_payload_keeps_safe_paths_and_refuses_unsafe_ones() {
        assert_eq!(
            copy_path_payload(Path::new("/home/u/notes.txt")).as_deref(),
            Some("/home/u/notes.txt")
        );
        assert_eq!(
            copy_path_payload(Path::new("/data/a'b c")).as_deref(),
            Some("/data/a'b c")
        );
        assert_eq!(copy_path_payload(Path::new("/data/left\nright")), None);
        assert_eq!(
            copy_path_payload(Path::new("/data/left\u{202e}right")),
            None
        );
    }

    #[test]
    fn in_place_update_keeps_surviving_rows_identical() {
        // Old contents: dirs Able, beta, gamma; files Alpha, Zulu (sorted).
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        for entry in [
            test_entry("/p/Able", true),
            test_entry("/p/beta", true),
            test_entry("/p/gamma", true),
            test_entry("/p/Alpha.txt", false),
            test_entry("/p/Zulu.txt", false),
        ] {
            store.append(&glib::BoxedAnyObject::new(entry));
        }
        // Identity of the survivors, to compare after the update.
        let able_item = store.item(0).unwrap();
        let gamma_item = store.item(2).unwrap();
        let zulu_item = store.item(4).unwrap();

        // New listing: beta removed, delta inserted, files unchanged.
        let delta = update_store_in_place(
            &store,
            vec![
                test_entry("/p/Able", true),
                test_entry("/p/delta", true),
                test_entry("/p/gamma", true),
                test_entry("/p/Alpha.txt", false),
                test_entry("/p/Zulu.txt", false),
            ],
        );
        assert_eq!(delta.removed_directories, vec![PathBuf::from("/p/beta")]);

        assert_eq!(
            store_paths(&store),
            vec![
                PathBuf::from("/p/Able"),
                PathBuf::from("/p/delta"),
                PathBuf::from("/p/gamma"),
                PathBuf::from("/p/Alpha.txt"),
                PathBuf::from("/p/Zulu.txt"),
            ]
        );
        // Surviving rows keep their object identity — this is what preserves
        // expansion state and cached child models in the TreeListModel.
        assert_eq!(store.item(0).unwrap(), able_item);
        assert_eq!(store.item(2).unwrap(), gamma_item);
        assert_eq!(store.item(4).unwrap(), zulu_item);
        // Inserted and surviving rows did not swap positions.
        assert_ne!(store.item(1).unwrap(), gamma_item);
    }

    #[test]
    fn in_place_update_handles_empty_and_full_replacement() {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        update_store_in_place(&store, Vec::new());
        assert_eq!(store.n_items(), 0);

        // Everything vanished.
        for entry in [test_entry("/p/a", true), test_entry("/p/b", false)] {
            store.append(&glib::BoxedAnyObject::new(entry));
        }
        update_store_in_place(&store, Vec::new());
        assert_eq!(store.n_items(), 0);

        // Everything new.
        update_store_in_place(
            &store,
            vec![test_entry("/p/dir", true), test_entry("/p/file", false)],
        );
        assert_eq!(
            store_paths(&store),
            vec![PathBuf::from("/p/dir"), PathBuf::from("/p/file")]
        );
    }

    #[test]
    fn refresh_status_preserves_last_good_rows_and_error_is_retryable() {
        let refreshing = DirectoryRowStatus::Refreshing { last_good: None };
        assert_eq!(DirectoryRowStatus::Loading.label(), "Loading…");
        assert_eq!(refreshing.label(), "Refreshing…");
        assert!(!DirectoryRowStatus::Loading.is_retryable());
        assert!(!refreshing.is_retryable());

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let first = glib::BoxedAnyObject::new(test_entry("/p/a", false));
        let second = glib::BoxedAnyObject::new(test_entry("/p/b", true));
        store.append(&first);
        store.append(&second);

        set_directory_status(&store, Path::new("/p"), refreshing.clone());
        assert_eq!(
            store_paths(&store),
            [PathBuf::from("/p/a"), PathBuf::from("/p/b")]
        );
        assert_eq!(store.n_items(), 3);
        assert_eq!(store.item(0).unwrap(), first);
        assert_eq!(store.item(1).unwrap(), second);
        assert_eq!(store_entry_for_test(&store, 2).status, Some(refreshing));

        set_directory_status(
            &store,
            Path::new("/p"),
            DirectoryRowStatus::Error {
                message: "Remote connection failed".to_string(),
                last_good: None,
            },
        );
        assert_eq!(store.n_items(), 3, "the prior status row is replaced");
        let error = store_entry_for_test(&store, 2);
        assert!(error
            .status
            .as_ref()
            .is_some_and(DirectoryRowStatus::is_retryable));
        assert_eq!(store.item(0).unwrap(), first);
        assert_eq!(store.item(1).unwrap(), second);

        update_store_in_place(
            &store,
            vec![test_entry("/p/a", false), test_entry("/p/c", true)],
        );
        assert_eq!(store.n_items(), 2, "success removes transient status");
        assert_eq!(
            store.item(0).unwrap(),
            first,
            "surviving row keeps identity"
        );
        assert_eq!(
            store_paths(&store),
            [PathBuf::from("/p/a"), PathBuf::from("/p/c")]
        );
    }

    #[test]
    fn snapshot_age_and_public_errors_are_stable_bounded_and_redacted() {
        let completed = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        assert_eq!(
            snapshot_age(completed, completed + Duration::from_secs(45)),
            "45s ago"
        );
        assert_eq!(
            snapshot_age(completed, completed + Duration::from_secs(125)),
            "2m ago"
        );
        assert_eq!(
            snapshot_age(completed, completed + Duration::from_secs(7_201)),
            "2h ago"
        );

        let raw = io::Error::other(format!(
            "ssh token=secret\n{}",
            "界".repeat(MAX_FILE_LABEL_BYTES)
        ));
        let status = directory_error_status(&raw, Some(completed));
        let DirectoryRowStatus::Error { message, last_good } = status else {
            panic!("expected a retryable error status");
        };
        assert_eq!(message, "Directory could not be loaded");
        assert_eq!(last_good, Some(completed));
        assert!(!message.contains("secret"));
        assert!(!message.contains('\n'));
        assert!(message.len() <= MAX_FILE_LABEL_BYTES);

        let invalid = io::Error::new(io::ErrorKind::InvalidData, "password=hidden");
        assert_eq!(
            public_directory_error_message(&invalid),
            "The remote returned an invalid directory response"
        );
        assert_eq!(
            public_file_operation_error_message(&invalid),
            "The operation or remote response was invalid"
        );
    }

    #[test]
    fn directory_failure_backoff_classifies_caps_and_retry_bypasses_once() {
        let now = Instant::now();
        let transient = io::Error::new(io::ErrorKind::ConnectionReset, "endpoint secret");
        let first = next_directory_failure_state(None, &transient, now);
        assert_eq!(first.class, DirectoryFailureClass::Transient);
        assert_eq!(first.consecutive, 1);
        assert_eq!(
            first.retry_not_before.duration_since(now),
            Duration::from_secs(1)
        );
        let second = next_directory_failure_state(Some(first), &transient, now);
        assert_eq!(second.consecutive, 2);
        assert_eq!(
            second.retry_not_before.duration_since(now),
            Duration::from_secs(2)
        );
        let capped = next_directory_failure_state(
            Some(DirectoryFailureState {
                class: DirectoryFailureClass::Transient,
                consecutive: 99,
                retry_not_before: now,
            }),
            &transient,
            now,
        );
        assert_eq!(
            capped.retry_not_before.duration_since(now),
            Duration::from_secs(30)
        );

        let key = (remote_authority("retry.example"), PathBuf::from("/srv"));
        let failures = std::collections::HashMap::from([(key.clone(), first)]);
        assert_eq!(
            directory_refresh_cooldown(DirectoryRefreshCause::Manual, &failures, &key, now),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            directory_refresh_cooldown(DirectoryRefreshCause::AutoTtl, &failures, &key, now),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            directory_refresh_cooldown(DirectoryRefreshCause::Retry, &failures, &key, now),
            None,
            "the explicit Retry intent gets one immediate attempt"
        );
        assert_eq!(
            directory_refresh_cooldown(
                DirectoryRefreshCause::Manual,
                &failures,
                &key,
                now + Duration::from_secs(1)
            ),
            None,
            "an expired cooldown no longer renders a zero-second block"
        );

        let persistent = next_directory_failure_state(
            Some(second),
            &io::Error::new(io::ErrorKind::PermissionDenied, "private"),
            now,
        );
        assert_eq!(persistent.class, DirectoryFailureClass::Persistent);
        assert_eq!(
            persistent.consecutive, 1,
            "changing class starts a new series"
        );
        assert_eq!(
            persistent.retry_not_before.duration_since(now),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn cancelled_or_backpressured_scans_do_not_poison_failure_cooldown() {
        let key = (remote_authority("cancel.example"), PathBuf::from("/srv"));
        let mut failures = std::collections::HashMap::new();
        record_directory_failure(
            &mut failures,
            key.clone(),
            &io::Error::new(io::ErrorKind::Interrupted, "superseded"),
            Instant::now(),
        );
        record_directory_failure(
            &mut failures,
            key,
            &io::Error::new(io::ErrorKind::WouldBlock, "queue full"),
            Instant::now(),
        );
        assert!(failures.is_empty());
    }

    #[test]
    fn remote_snapshot_ttl_uses_monotonic_completion_time() {
        let now = Instant::now();
        let fresh = SnapshotMeta {
            completed_wall: SystemTime::UNIX_EPOCH,
            completed_monotonic: now - (REMOTE_SNAPSHOT_TTL - Duration::from_secs(1)),
        };
        let stale = SnapshotMeta {
            completed_wall: SystemTime::now(),
            completed_monotonic: now - REMOTE_SNAPSHOT_TTL,
        };
        assert!(!snapshot_meta_is_stale(fresh, now));
        assert!(snapshot_meta_is_stale(stale, now));
    }

    #[test]
    fn scan_timing_flags_each_latency_budget_independently() {
        assert!(!scan_timing_is_slow(
            ScanTiming {
                queued_for: Duration::from_millis(999),
                listed_for: Duration::from_millis(1_999),
                queued_depth: 63,
            },
            Duration::from_millis(99)
        ));
        assert!(scan_timing_is_slow(
            ScanTiming {
                queued_for: Duration::from_secs(1),
                ..ScanTiming::default()
            },
            Duration::ZERO
        ));
        assert!(scan_timing_is_slow(
            ScanTiming {
                listed_for: Duration::from_secs(2),
                ..ScanTiming::default()
            },
            Duration::ZERO
        ));
        assert!(scan_timing_is_slow(
            ScanTiming::default(),
            Duration::from_millis(100)
        ));
    }

    #[test]
    fn refresh_selection_keeps_only_paths_that_survive_reconciliation() {
        let selected = vec![test_entry("/p/a", false), test_entry("/p/b", false)];
        let current = vec![
            test_entry("/p/a", false),
            test_entry("/p/c", false),
            test_entry("/p/d", false),
        ];

        assert_eq!(
            surviving_selected_paths(&selected, &current),
            vec![PathBuf::from("/p/a")]
        );

        let retyped = vec![test_entry("/p/a", true), test_entry("/p/b", false)];
        assert_eq!(
            surviving_selected_paths(&selected, &retyped),
            vec![PathBuf::from("/p/b")],
            "a path retyped from file to directory is a replacement row"
        );

        let created = vec![PathBuf::from("/p/c")];
        assert_eq!(
            selection_paths_after_reconcile(&selected, &current, Some(&created)),
            created,
            "successful create/rename selects its reconciled destination"
        );
        assert!(selection_paths_after_reconcile(
            &selected,
            &current,
            Some(&[PathBuf::from("/p/missing")])
        )
        .is_empty());
    }

    #[test]
    fn in_place_update_replaces_a_row_when_its_directory_kind_changes() {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        store.append(&glib::BoxedAnyObject::new(test_entry("/p/link", true)));
        let old = store.item(0).unwrap();

        let delta = update_store_in_place(&store, vec![test_entry("/p/link", false)]);

        assert_ne!(store.item(0).unwrap(), old);
        let entry = store_entry_for_test(&store, 0);
        assert!(!entry.is_dir, "the refreshed symlink is not expandable");
        assert_eq!(
            delta.removed_directories,
            vec![PathBuf::from("/p/link")],
            "retyping a directory invalidates its materialized descendants"
        );
    }

    #[test]
    fn removed_directory_invalidation_is_component_aware_and_recursive() {
        let removed = vec![PathBuf::from("/remote/src")];
        assert!(path_is_in_removed_subtree(
            Path::new("/remote/src"),
            &removed
        ));
        assert!(path_is_in_removed_subtree(
            Path::new("/remote/src/deep/cache"),
            &removed
        ));
        assert!(!path_is_in_removed_subtree(
            Path::new("/remote/src-old"),
            &removed
        ));
    }

    #[test]
    fn collapsed_materialized_directory_still_resolves_its_cached_store() {
        let root_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let collapsed_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let mut stores = std::collections::HashMap::new();
        stores.insert(PathBuf::from("/remote/src"), collapsed_store.downgrade());

        assert_eq!(
            cached_materialized_store(
                Path::new("/remote"),
                Path::new("/remote"),
                &root_store,
                &stores,
            ),
            Some(root_store)
        );
        assert_eq!(
            cached_materialized_store(
                Path::new("/remote"),
                Path::new("/remote/src"),
                &gio::ListStore::new::<glib::BoxedAnyObject>(),
                &stores,
            ),
            Some(collapsed_store)
        );
        assert!(cached_materialized_store(
            Path::new("/remote"),
            Path::new("/remote/never-opened"),
            &gio::ListStore::new::<glib::BoxedAnyObject>(),
            &stores,
        )
        .is_none());
    }

    #[test]
    fn materialized_store_registry_does_not_pin_evicted_subtrees() {
        let root_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let child_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let mut stores = std::collections::HashMap::new();
        stores.insert(PathBuf::from("/remote/src"), child_store.downgrade());
        assert!(cached_materialized_store(
            Path::new("/remote"),
            Path::new("/remote/src"),
            &root_store,
            &stores,
        )
        .is_some());

        drop(child_store);
        assert!(cached_materialized_store(
            Path::new("/remote"),
            Path::new("/remote/src"),
            &root_store,
            &stores,
        )
        .is_none());
    }

    #[test]
    fn delayed_actions_reject_entries_removed_or_retyped_by_refresh() {
        let file = test_entry("/p/item", false);
        let sibling = test_entry("/p/sibling", false);
        assert!(entries_remain_current(
            std::slice::from_ref(&file),
            &[file.clone(), sibling.clone()]
        ));
        assert!(!entries_remain_current(
            std::slice::from_ref(&file),
            std::slice::from_ref(&sibling)
        ));
        assert!(!entries_remain_current(
            std::slice::from_ref(&file),
            &[test_entry("/p/item", true)]
        ));
    }

    #[test]
    fn menu_target_uses_selection_or_collapses_to_the_clicked_row() {
        let a = test_entry("/p/a", false);
        let b = test_entry("/p/b", true);
        let c = test_entry("/p/c", false);
        let selected = vec![(0_u32, a.clone()), (2_u32, c.clone())];

        // Right-click on a selected row: the whole selection is the target.
        let (entries, collapse) = resolve_menu_target(Some((2, c.clone())), &selected);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("/p/a"), Path::new("/p/c")]
        );
        assert_eq!(collapse, None);

        // Right-click outside the selection: just that row, and the caller
        // collapses the selection to it.
        let (entries, collapse) = resolve_menu_target(Some((1, b.clone())), &selected);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("/p/b")]
        );
        assert_eq!(collapse, Some(1));

        // Empty space: no row targets, nothing to collapse.
        let (entries, collapse) = resolve_menu_target(None, &selected);
        assert!(entries.is_empty());
        assert_eq!(collapse, None);

        // Nothing selected: the clicked row alone, collapsed to.
        let (entries, collapse) = resolve_menu_target(Some((1, b)), &[]);
        assert_eq!(entries.len(), 1);
        assert_eq!(collapse, Some(1));
    }

    #[test]
    fn directory_actions_target_the_clicked_child_instead_of_the_tree_root() {
        let root = Path::new("/remote");
        let child = test_entry("/remote/project", true);
        let file = test_entry("/remote/project/main.rs", false);

        assert_eq!(
            directory_action_target(root, Some(&child)),
            PathBuf::from("/remote/project")
        );
        assert_eq!(
            directory_action_target(root, Some(&file)),
            PathBuf::from("/remote/project")
        );
        assert_eq!(directory_action_target(root, None), root);
    }

    /// A tiny (name, path, is_dir) node for the filter tests.
    fn node(name: &str, path: &str, is_dir: bool) -> (String, PathBuf, bool) {
        (name.to_string(), PathBuf::from(path), is_dir)
    }

    #[test]
    fn filter_keeps_matches_and_ancestors_from_loaded_subtrees() {
        // Tree: /r/{a/{b/{match.txt}}, c/{other.txt}, MatchDir/{inner.txt},
        // ghost/{…never loaded…}}
        let children: std::collections::HashMap<PathBuf, Vec<(String, PathBuf, bool)>> =
            std::collections::HashMap::from([
                (
                    PathBuf::from("/r"),
                    vec![
                        node("a", "/r/a", true),
                        node("c", "/r/c", true),
                        node("MatchDir", "/r/MatchDir", true),
                        node("ghost", "/r/ghost", true),
                    ],
                ),
                (PathBuf::from("/r/a"), vec![node("b", "/r/a/b", true)]),
                (
                    PathBuf::from("/r/a/b"),
                    vec![node("match.txt", "/r/a/b/match.txt", false)],
                ),
                (
                    PathBuf::from("/r/c"),
                    vec![node("other.txt", "/r/c/other.txt", false)],
                ),
                (
                    PathBuf::from("/r/MatchDir"),
                    vec![node("inner.txt", "/r/MatchDir/inner.txt", false)],
                ),
                // /r/ghost has no entry: its store was never materialized.
            ]);
        let roots = children[&PathBuf::from("/r")].clone();
        let children_of = |path: &Path| children.get(path).cloned();

        let visible = collect_visible_paths(&roots, &children_of, "match");
        // The match itself, its ancestors, and the case-insensitive dir match.
        for path in ["/r/a", "/r/a/b", "/r/a/b/match.txt", "/r/MatchDir"] {
            assert!(visible.contains(Path::new(path)), "{path} must be visible");
        }
        // Children of a matching dir are not auto-visible, unrelated branches
        // stay hidden, and a never-loaded subtree is not descended into.
        for path in [
            "/r/MatchDir/inner.txt",
            "/r/c",
            "/r/c/other.txt",
            "/r/ghost",
        ] {
            assert!(!visible.contains(Path::new(path)), "{path} must be hidden");
        }

        // Case-insensitivity runs both ways.
        let visible = collect_visible_paths(&roots, &children_of, "MATCH.TXT");
        assert!(visible.contains(Path::new("/r/a/b/match.txt")));

        // An empty query is the identity filter over loaded rows.
        let visible = collect_visible_paths(&roots, &children_of, "");
        for path in [
            "/r/a",
            "/r/a/b",
            "/r/a/b/match.txt",
            "/r/c",
            "/r/c/other.txt",
            "/r/MatchDir",
            "/r/MatchDir/inner.txt",
            "/r/ghost",
        ] {
            assert!(visible.contains(Path::new(path)), "{path} must be visible");
        }

        // A query with no match keeps nothing.
        let visible = collect_visible_paths(&roots, &children_of, "absent");
        assert!(visible.is_empty());
    }

    #[test]
    fn paste_plan_flags_collisions_self_pastes_and_sizes() {
        let root = std::env::temp_dir().join(format!(
            "forge-paste-plan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.txt"), b"12345").unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::fs::write(root.join("dir/inner.txt"), b"678").unwrap();
        // A destination that already exists (Local target).
        std::fs::write(root.join("exists.txt"), b"old").unwrap();

        let items = vec![
            remote_fs::FsClipboardItem {
                path: root.join("file.txt"),
                is_dir: false,
            },
            remote_fs::FsClipboardItem {
                path: root.join("dir"),
                is_dir: true,
            },
            remote_fs::FsClipboardItem {
                path: root.join("exists.txt"),
                is_dir: false,
            },
        ];
        let plan = plan_paste(&items, &root, true, true);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].dst, root.join("file.txt"));
        assert!(plan[0].self_paste, "same dir means dst == src");
        assert_eq!(plan[0].size, 5);
        assert!(plan[1].is_dir);
        assert_eq!(plan[1].size, 3);
        // exists.txt already exists in the target dir, so it is flagged.
        assert!(plan[2].collides);

        // Remote target: no local collision check, no sizes measured.
        let plan = plan_paste(&items, &root, false, false);
        assert!(plan.iter().all(|item| !item.collides));
        assert!(plan.iter().all(|item| item.size == 0));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failure_summary_counts_and_single_item_form() {
        assert_eq!(failure_summary(0, 3, "boom"), None);
        assert_eq!(
            failure_summary(1, 1, "boom"),
            Some("Failed: boom".to_string())
        );
        assert_eq!(
            failure_summary(2, 5, "boom"),
            Some("2 of 5 failed: boom".to_string())
        );
    }

    #[test]
    fn delete_confirmation_names_count_and_bounded_names() {
        let (title, body) = delete_confirmation_text(&[test_entry("/p/solo.txt", false)]);
        assert_eq!(title, "Delete this item?");
        assert!(body.contains("/p/solo.txt"), "{body}");

        let (_title, body) = delete_confirmation_text(&[test_entry("/p/dir", true)]);
        assert!(body.contains("everything inside"), "{body}");

        let entries: Vec<FileEntry> = (0..7)
            .map(|index| test_entry(&format!("/p/item-{index}"), false))
            .collect();
        let (title, body) = delete_confirmation_text(&entries);
        assert_eq!(title, "Delete 7 items?");
        for index in 0..5 {
            assert!(body.contains(&format!("item-{index}")), "{body}");
        }
        assert!(!body.contains("item-5"), "{body}");
        assert!(body.contains("…and 2 more"), "{body}");

        // Exactly five: no remainder line.
        let entries: Vec<FileEntry> = (0..5)
            .map(|index| test_entry(&format!("/p/item-{index}"), false))
            .collect();
        let (_, body) = delete_confirmation_text(&entries);
        assert!(!body.contains("…and"), "{body}");
    }
}
