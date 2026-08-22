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
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;
use vte4::TerminalExt;

use super::remote_fs::{self, FsClipboard, FsEntry, FsLocation};
use super::*;
use crate::config::RemoteHost;
use crate::terminal::terminal_working_directory;

const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(16);
const MAX_FILE_LABEL_BYTES: usize = 512;
const MAX_CONCURRENT_SCANS: usize = 8;
static ACTIVE_SCANS: AtomicUsize = AtomicUsize::new(0);
/// Mutating file operations get their own, smaller bound so a burst of
/// context-menu actions cannot crowd out directory scans.
const MAX_CONCURRENT_FS_OPS: usize = 4;
static ACTIVE_FS_OPS: AtomicUsize = AtomicUsize::new(0);

// Re-exported so the existing tests keep one obvious name for the listing cap.
#[cfg(test)]
use super::remote_fs::MAX_DIRECTORY_ENTRIES;

#[derive(Clone, Debug)]
struct FileEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

impl From<FsEntry> for FileEntry {
    /// Display names are sanitized on the way in; `path` keeps the exact
    /// bytes so file operations round-trip even for hostile names.
    fn from(entry: FsEntry) -> Self {
        FileEntry {
            name: safe_file_label(&entry.name),
            path: entry.path,
            is_dir: entry.is_dir,
        }
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
    scan_entries(&FsLocation::Local, &[], dir)
}

fn scan_entries(loc: &FsLocation, hosts: &[RemoteHost], dir: &Path) -> io::Result<Vec<FileEntry>> {
    remote_fs::list_dir(loc, hosts, dir)
        .map(|entries| entries.into_iter().map(FileEntry::from).collect())
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

struct ActiveScan;

impl Drop for ActiveScan {
    fn drop(&mut self) {
        ACTIVE_SCANS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ActiveFsOp;

impl Drop for ActiveFsOp {
    fn drop(&mut self) {
        ACTIVE_FS_OPS.fetch_sub(1, Ordering::AcqRel);
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
    dir: PathBuf,
    apply: F,
) -> io::Result<()>
where
    F: FnOnce(io::Result<Vec<FileEntry>>) + 'static,
{
    ACTIVE_SCANS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < MAX_CONCURRENT_SCANS).then_some(active + 1)
        })
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "file-tree scan limit reached"))?;
    let (tx, rx) = mpsc::sync_channel(1);
    if let Err(error) = std::thread::Builder::new()
        .name("forge-file-tree-scan".to_string())
        .spawn(move || {
            let _active = ActiveScan;
            let _ = tx.send(scan_entries(&loc, &hosts, &dir));
        })
    {
        ACTIVE_SCANS.fetch_sub(1, Ordering::AcqRel);
        return Err(error);
    }
    poll_worker(rx, apply, "file-tree scan worker disconnected");
    Ok(())
}

/// Run one blocking file operation (create/rename/delete/copy) on a worker
/// thread, bounded separately from scans, and deliver the outcome to `apply`.
fn request_fs_op<F, W>(work: W, apply: F) -> io::Result<()>
where
    F: FnOnce(io::Result<()>) + 'static,
    W: FnOnce() -> io::Result<()> + Send + 'static,
{
    ACTIVE_FS_OPS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < MAX_CONCURRENT_FS_OPS).then_some(active + 1)
        })
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "file operation limit reached"))?;
    let (tx, rx) = mpsc::sync_channel(1);
    if let Err(error) = std::thread::Builder::new()
        .name("forge-file-tree-op".to_string())
        .spawn(move || {
            let _active = ActiveFsOp;
            let _ = tx.send(work());
        })
    {
        ACTIVE_FS_OPS.fetch_sub(1, Ordering::AcqRel);
        return Err(error);
    }
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
    /// Active only while the type-to-filter row is open: wraps `tree_model`
    /// with the visibility predicate.
    filter_model: gtk4::FilterListModel,
    filter: gtk4::CustomFilter,
    filter_state: Rc<RefCell<FilterState>>,
    /// Every child store the lazy expansion factory has created, by parent
    /// path — the filter's descendant walk reads it instead of triggering
    /// new scans. Cleared on reset; bounded so long browsing sessions cannot
    /// grow it without limit.
    child_stores: Rc<RefCell<std::collections::HashMap<PathBuf, gio::ListStore>>>,
    generation: Rc<Cell<u64>>,
    /// Browsed filesystem, shared with UiState; scans snapshot it at request
    /// time and stale results are dropped when it has moved on.
    location: Rc<RefCell<FsLocation>>,
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
}

/// Bound on recorded child stores; beyond it the filter simply does not
/// descend into subtrees expanded later.
const MAX_CHILD_STORES: usize = 512;

impl FileTreeModel {
    fn new(location: Rc<RefCell<FsLocation>>, config: Rc<RefCell<crate::config::Config>>) -> Self {
        let root_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let generation = Rc::new(Cell::new(0_u64));
        let child_stores = Rc::new(RefCell::new(std::collections::HashMap::new()));
        let filter_state = Rc::new(RefCell::new(FilterState::default()));
        let filter = gtk4::CustomFilter::new({
            let filter_state = filter_state.clone();
            move |object| {
                let state = filter_state.borrow();
                if state.query.is_empty() {
                    return true;
                }
                let Some(row) = object.downcast_ref::<TreeListRow>() else {
                    return true;
                };
                let Some(entry) = entry_from_row(row) else {
                    return false;
                };
                state.visible.contains(&entry.path)
            }
        });
        let tree_model = TreeListModel::new(root_store.clone(), false, false, {
            let generation = generation.clone();
            let location = location.clone();
            let config = config.clone();
            let child_stores = child_stores.clone();
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

                let children = gio::ListStore::new::<glib::BoxedAnyObject>();
                {
                    let mut map = child_stores.borrow_mut();
                    if map.len() < MAX_CHILD_STORES {
                        map.insert(path.clone(), children.clone());
                    }
                }
                let children_for_scan = children.clone();
                let generation_for_scan = generation.clone();
                let expected_generation = generation.get();
                let scan_location = location.borrow().clone();
                let location_for_scan = location.clone();
                let scan_hosts = config.borrow().remote_hosts.clone();
                let path_for_result = path.clone();
                let path_for_error = path.clone();
                let filter_state_for_scan = filter_state.clone();
                let filter_for_scan = filter.clone();
                let root_store_for_filter = root_store_for_filter.clone();
                let child_stores_for_filter = child_stores.clone();
                if let Err(error) =
                    request_dir_scan(scan_location.clone(), scan_hosts, path, move |result| {
                        if generation_for_scan.get() != expected_generation {
                            return;
                        }
                        if *location_for_scan.borrow() != scan_location {
                            return;
                        }
                        match result {
                            Ok(entries) => {
                                append_entries(&children_for_scan, entries);
                                reapply_filter_parts(
                                    &root_store_for_filter,
                                    &child_stores_for_filter,
                                    &filter_state_for_scan,
                                    &filter_for_scan,
                                );
                            }
                            Err(error) => log::warn!(
                                "failed to scan directory {}: {error}",
                                path_for_result.display()
                            ),
                        }
                    })
                {
                    log::warn!(
                        "failed to start directory scan for {}: {error}",
                        path_for_error.display()
                    );
                }

                Some(children.upcast())
            }
        });

        let selection = gtk4::MultiSelection::new(Some(tree_model.clone()));
        // MultiSelection never autoselects; ctrl+click toggles and
        // shift+click ranges are built in.

        // The filter wrap consults a precomputed visible-path set (matches +
        // ancestors), so TreeListRow identity — and with it expansion state —
        // is untouched by filtering. The wrap is only installed while a query
        // is active; clearing removes it and restores the tree model.
        let filter_model =
            gtk4::FilterListModel::new(Some(tree_model.clone()), Some(filter.clone()));

        Self {
            root_store,
            tree_model,
            selection,
            filter_model,
            filter,
            filter_state,
            child_stores,
            generation,
            location,
            config,
        }
    }

    fn reset(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.root_store.remove_all();
        self.child_stores.borrow_mut().clear();
        generation
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
    /// expanded row for `dir`. `None` when `dir` is not visible right now —
    /// a collapsed or never-expanded row needs no refresh (its cached store
    /// is reused on re-expansion, the pre-existing TreeListModel behavior).
    fn materialized_children_of(&self, root: &Path, dir: &Path) -> Option<gio::ListStore> {
        if dir == root {
            return Some(self.root_store.clone());
        }
        for index in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_model.row(index) else {
                continue;
            };
            let Some(entry) = entry_from_row(&row) else {
                continue;
            };
            if entry.path == dir && row.is_expanded() {
                return row
                    .children()
                    .and_then(|model| model.downcast::<gio::ListStore>().ok());
            }
        }
        None
    }

    fn row_entry(&self, position: u32) -> Option<(TreeListRow, FileEntry)> {
        // Read through the selection's current model: while the type-to-
        // filter row is open, positions index the FilterListModel wrap.
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
            .filter_map(|position| self.row_entry(position).map(|(_, entry)| (position, entry)))
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
            if entry_from_row(&row).is_some_and(|entry| entry.path == path) {
                return Some(index);
            }
        }
        None
    }

    /// Apply the type-to-filter query: recompute the visible path set from
    /// the LOADED stores only (never a new scan), auto-expand materialized
    /// ancestors of matches, and install the filter wrap. An empty query
    /// removes the wrap and collapses exactly the rows the filter expanded.
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
            self.selection.set_model(Some(&self.tree_model));
            return;
        }
        // Compute the new visible set and publish it before touching the
        // model: expansion and `emit_changed` re-enter the filter closure,
        // which borrows `filter_state` — so no borrow may be held here.
        let (visible, was_inactive) = {
            let roots = store_entries(&self.root_store);
            let visible = {
                let child_stores = self.child_stores.borrow();
                collect_visible_paths(
                    &roots,
                    &|path| child_stores.get(path).map(store_entries),
                    &query,
                )
            };
            let mut state = self.filter_state.borrow_mut();
            let was_inactive = state.query.is_empty();
            state.query = query;
            state.visible = visible.clone();
            (visible, was_inactive)
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
            if !entry.is_dir || !visible.contains(&entry.path) {
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

        if was_inactive {
            self.selection.set_model(Some(&self.filter_model));
        } else {
            self.filter.changed(gtk4::FilterChange::Different);
        }
    }

    /// Whether the type-to-filter query is currently active.
    fn filter_is_active(&self) -> bool {
        !self.filter_state.borrow().query.is_empty()
    }
}

/// Replace a store's contents with the minimal set of removals/insertions so
/// surviving rows keep their `TreeListRow` identity — and with it their
/// expansion state and cached child models. Both the old and new contents
/// are sorted by the same comparator, so after vanished paths are removed
/// the survivors already stand at their final positions and newcomers slot
/// in at their sorted index. This is what lets a mutation refresh exactly
/// one directory without collapsing unrelated expansion anywhere in the tree.
fn update_store_in_place(store: &gio::ListStore, entries: Vec<FileEntry>) {
    let store_entry = |index: u32| -> Option<PathBuf> {
        store
            .item(index)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .and_then(|boxed| {
                boxed
                    .try_borrow::<FileEntry>()
                    .ok()
                    .map(|entry| entry.path.clone())
            })
    };
    let new_paths: std::collections::HashSet<&Path> =
        entries.iter().map(|entry| entry.path.as_path()).collect();

    // Remove vanished rows back-to-front so earlier indices stay valid.
    let mut index = store.n_items();
    while index > 0 {
        index -= 1;
        match store_entry(index) {
            Some(path) if new_paths.contains(path.as_path()) => {}
            _ => store.remove(index),
        }
    }

    let survivors: std::collections::HashSet<PathBuf> =
        (0..store.n_items()).filter_map(store_entry).collect();
    for (position, entry) in entries.into_iter().enumerate() {
        if !survivors.contains(&entry.path) {
            store.insert(position as u32, &glib::BoxedAnyObject::new(entry));
        }
    }
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
    child_stores: &Rc<RefCell<std::collections::HashMap<PathBuf, gio::ListStore>>>,
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
            &|path| child_stores.get(path).map(store_entries),
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
    config: Rc<RefCell<crate::config::Config>>,
) -> (FileTreeModel, ListView) {
    let model = FileTreeModel::new(location, config);
    let factory = SignalListItemFactory::new();

    factory.connect_setup(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk4::ListItem>() else {
            return;
        };

        let icon = gtk4::Image::new();
        icon.set_pixel_size(16);
        let label = gtk4::Label::new(None);
        label.set_hexpand(true);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);

        let row_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        row_box.append(&icon);
        row_box.append(&label);

        let expander = gtk4::TreeExpander::new();
        expander.set_child(Some(&row_box));
        list_item.set_child(Some(&expander));
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
        let Some(label) = row_box
            .last_child()
            .and_then(|child| child.downcast::<gtk4::Label>().ok())
        else {
            return;
        };

        icon.set_icon_name(Some(if entry.is_dir {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        }));
        label.set_text(&entry.name);
        let path = safe_file_label(&entry.path.to_string_lossy());
        label.set_tooltip_text(Some(&path));
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

impl UiState {
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

    /// Rebuild the tree with `root` at the top. Results from older scans are
    /// ignored, so rapid cwd changes cannot repopulate the browser with stale data.
    pub(crate) fn set_file_tree_root(&self, root: PathBuf) {
        let generation = self.file_tree_model.reset();
        let location = self.file_tree_location.borrow().clone();
        self.file_tree_root_label
            .set_text(&root_display_label(&location, &root));
        let root_tooltip = safe_file_label(&root.to_string_lossy());
        self.file_tree_root_label
            .set_tooltip_text(Some(&root_tooltip));
        *self.file_tree_root.borrow_mut() = root.clone();

        let model = self.file_tree_model.clone();
        let model_for_start_error = model.clone();
        let expected_root = root.clone();
        let active_root = self.file_tree_root.clone();
        let toast_overlay = self.toast_overlay.clone();
        let scan_hosts = self.config.borrow().remote_hosts.clone();
        let scan_location = location.clone();
        let location_for_scan = self.file_tree_location.clone();
        if let Err(error) = request_dir_scan(location, scan_hosts, root, move |result| {
            if *active_root.borrow() != expected_root {
                return;
            }
            // A location switch after this scan was queued makes its entries
            // meaningless for the tree now on screen.
            if *location_for_scan.borrow() != scan_location {
                return;
            }
            match result {
                Ok(entries) => {
                    model.replace_root(generation, entries);
                }
                Err(error) => {
                    log::warn!(
                        "failed to scan file-tree root {}: {error}",
                        expected_root.display()
                    );
                    // An empty directory and an unreadable directory must not
                    // look identical. `replace_root` doubles as a generation
                    // check so a late failure cannot toast for a newer scan.
                    if model.replace_root(generation, Vec::new()) {
                        let path = root_display_label(&scan_location, &expected_root);
                        let error =
                            jterm_core::review_input::safe_inline_display(&error.to_string(), 512);
                        toast_overlay
                            .add_toast(adw::Toast::new(&format!("Cannot open {path}: {error}")));
                    }
                }
            }
        }) {
            log::warn!("failed to start file-tree scan: {error}");
            // The start error is synchronous, but still respect the current
            // generation in case this function is re-entered by UI callbacks.
            if model_for_start_error.replace_root(generation, Vec::new()) {
                let location = self.file_tree_location.borrow().clone();
                let path = root_display_label(&location, &self.file_tree_root.borrow());
                let error = jterm_core::review_input::safe_inline_display(&error.to_string(), 512);
                self.toast_overlay
                    .add_toast(adw::Toast::new(&format!("Cannot open {path}: {error}")));
            }
        }
    }

    /// Switch the tree to another filesystem (local disk or one of the
    /// configured remote hosts) and root it at that location's start
    /// directory. On failure the previous location stays active.
    pub(crate) fn set_file_tree_location(&self, location: FsLocation) {
        if *self.file_tree_location.borrow() == location {
            return;
        }
        let hosts = self.config.borrow().remote_hosts.clone();
        match remote_fs::start_dir(&location, &hosts) {
            Ok(root) => {
                *self.file_tree_location.borrow_mut() = location;
                // Reflect programmatic switches (remote-tab follow) in the
                // header selector; the notify handler ignores no-op selections.
                self.refresh_file_tree_location_selector();
                self.set_file_tree_root(root);
            }
            Err(error) => {
                let label = location.label(&hosts);
                log::warn!("failed to resolve start directory for {label}: {error}");
                let error = jterm_core::review_input::safe_inline_display(&error.to_string(), 512);
                self.toast_overlay
                    .add_toast(adw::Toast::new(&format!("Cannot open {label}: {error}")));
                // The user may have picked the failing location in the
                // selector; snap its selection back to the active location.
                self.refresh_file_tree_location_selector();
            }
        }
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
    /// re-select the active location. Selection changes that do not match the
    /// active location are left to the notify handler, which performs the
    /// actual switch (a removed host thus falls back to Local).
    pub(crate) fn refresh_file_tree_location_selector(&self) {
        let config = self.config.borrow();
        let hosts = &config.remote_hosts;
        let active_count = hosts.len().min(crate::config::MAX_REMOTE_HOSTS);
        let mut labels = vec![FsLocation::Local.label(hosts)];
        labels.extend((0..active_count).map(|index| FsLocation::Remote(index).label(hosts)));
        let selected = match &*self.file_tree_location.borrow() {
            FsLocation::Local => 0,
            FsLocation::Remote(index)
                if *index < active_count
                    && crate::config::checked_remote_host(hosts, *index).is_ok() =>
            {
                *index as u32 + 1
            }
            FsLocation::Remote(_) => 0,
        };
        drop(config);
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let model = gtk4::StringList::new(&label_refs);
        self.file_tree_location_selector.set_model(Some(&model));
        self.file_tree_location_selector.set_selected(selected);
    }

    /// React to the header location selector. Guarded against rebuilds: a
    /// selection matching the active location is a no-op, so repopulating the
    /// model can never retrigger a switch.
    pub(crate) fn connect_file_tree_location_selector(&self) {
        let ui = self.clone();
        self.file_tree_location_selector
            .connect_selected_notify(move |selector| {
                let index = selector.selected();
                if index == gtk4::INVALID_LIST_POSITION {
                    return;
                }
                let location = if index == 0 {
                    FsLocation::Local
                } else {
                    FsLocation::Remote((index - 1) as usize)
                };
                if *ui.file_tree_location.borrow() == location {
                    return;
                }
                ui.set_file_tree_location(location);
            });
    }

    /// The index into `config.remote_hosts` of the host the active tab is
    /// connected to, when that tab is a forge-managed remote session.
    fn current_tab_remote_host_index(&self) -> Option<usize> {
        let page_num = self.notebook.current_page()?;
        let page = self.notebook.nth_page(Some(page_num))?;
        let tab_num = page
            .widget_name()
            .strip_prefix("tab-")
            .and_then(|value| value.parse::<u32>().ok())?;
        let connection = self.tab_connections.borrow().get(&tab_num)?.clone();
        let config = self.config.borrow();
        config
            .remote_hosts
            .iter()
            .position(|host| host.name == connection.host.name)
    }

    /// Jump the file tree to the active tab's working directory. A remote
    /// tab whose shell reports its cwd through OSC 7 pulls the tree onto its
    /// host, rooted at the reported directory; one that reports nothing
    /// leaves the tree alone, exactly as before.
    pub(crate) fn file_tree_goto_current_cwd(&self) {
        if let Some(index) = self.current_tab_remote_host_index() {
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
                    *self.file_tree_location.borrow_mut() = location;
                    self.refresh_file_tree_location_selector();
                    self.set_file_tree_root(cwd);
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
                if *self.file_tree_location.borrow() != FsLocation::Local {
                    *self.file_tree_location.borrow_mut() = FsLocation::Local;
                    self.refresh_file_tree_location_selector();
                }
                if *self.file_tree_root.borrow() != dir {
                    self.set_file_tree_root(dir);
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

    /// Move the root up to the parent directory.
    pub(crate) fn file_tree_go_up(&self) {
        let parent = self.file_tree_root.borrow().parent().map(Path::to_path_buf);
        if let Some(parent) = parent {
            self.set_file_tree_root(parent);
        }
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

        let right_click = gtk4::GestureClick::new();
        right_click.set_button(3);
        let ui = self.clone();
        let file_tree_for_menu = file_tree.clone();
        right_click.connect_pressed(move |gesture, _n_press, x, y| {
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            let target = file_tree_row_at(&file_tree_for_menu, x, y);
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
        let drop_hover: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));
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
        let target_desc = match &location {
            FsLocation::Local => display_path(&target_dir),
            FsLocation::Remote(_) => location.label(&hosts),
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
                        remote_fs::DropAction::Upload => remote_fs::transfer(
                            &FsLocation::Local,
                            &hosts,
                            &item.src,
                            &to,
                            &item.dst,
                            item.is_dir,
                            &item_control,
                        ),
                    };
                    if let Err(error) = result {
                        if error.kind() == io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                        let detail =
                            jterm_core::review_input::safe_inline_display(&error.to_string(), 256);
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
        let target_dir = match &target {
            Some((_, entry)) if entry.is_dir => entry.path.clone(),
            Some((_, entry)) => entry
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.file_tree_root.borrow().clone()),
            None => self.file_tree_root.borrow().clone(),
        };

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
            item.connect_clicked(move |_| {
                popover_c.popdown();
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
                item.connect_clicked(move |_| {
                    popover_c.popdown();
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
                item.connect_clicked(move |_| {
                    popover_c.popdown();
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
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    *ui.file_tree_clipboard.borrow_mut() = Some(FsClipboard {
                        loc: location.clone(),
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
                item.connect_clicked(move |_| {
                    popover_c.popdown();
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
            let label = match clipboard
                .as_ref()
                .and_then(|clip| remote_fs::transfer_plan(&clip.loc, &location))
            {
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
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    ui.paste_file_tree_clipboard(clip.clone(), dir.clone());
                });
            }
            vbox.append(&item);
        }

        {
            let item = make_item("Refresh");
            let popover_c = popover.clone();
            let ui = self.clone();
            item.connect_clicked(move |_| {
                popover_c.popdown();
                // In-place re-list: expanded rows stay expanded.
                let root = ui.file_tree_root.borrow().clone();
                ui.refresh_dir_listing(&root);
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
            let name = name_row.text();
            if let Err(message) = remote_fs::validate_new_name(&name) {
                error_label.set_text(message);
                error_label.set_visible(true);
                return;
            }
            let location = ui.file_tree_location.borrow().clone();
            let hosts = ui.config.borrow().remote_hosts.clone();
            match kind {
                NameDialogKind::NewFile | NameDialogKind::NewFolder => {
                    let path = dir.join(name.as_str());
                    ui.execute_fs_op(
                        kind.verb(),
                        None,
                        vec![dir.clone()],
                        move || {
                            if kind == NameDialogKind::NewFile {
                                remote_fs::create_file(&location, &hosts, &path)
                            } else {
                                remote_fs::create_dir(&location, &hosts, &path)
                            }
                        },
                        || {},
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
                        ui.execute_fs_op(
                            kind.verb(),
                            None,
                            vec![dir.clone()],
                            move || remote_fs::rename(&location, &hosts, &src, &dst),
                            || {},
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
            // The response closure is Fn: clone the entry list into the
            // one-shot worker.
            let entries = entries.clone();
            let location = ui.file_tree_location.borrow().clone();
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
                        if let Err(error) = remote_fs::delete(&location, &hosts, &entry.path) {
                            let name =
                                jterm_core::review_input::safe_inline_display(&entry.name, 256);
                            let detail = jterm_core::review_input::safe_inline_display(
                                &error.to_string(),
                                256,
                            );
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
        let hosts = self.config.borrow().remote_hosts.clone();
        let cut = clip.cut;
        let total_items = clip.items.len();
        // Uploads (local sources) know their sizes up-front; downloads and
        // relays report transferred bytes only.
        let from = clip.loc.clone();
        let measure = from == FsLocation::Local;
        let plan_items = plan_paste(
            &clip.items,
            &target_dir,
            location == FsLocation::Local,
            measure,
        );

        if clip.loc == location {
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
            let loc = location.clone();
            let ui = self.clone();
            let ui_for_success = self.clone();
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
                            remote_fs::rename(&loc, &hosts, &item.src, &item.dst)
                        } else {
                            remote_fs::copy(&loc, &hosts, &item.src, &item.dst)
                        };
                        if let Err(error) = result {
                            let detail = jterm_core::review_input::safe_inline_display(
                                &error.to_string(),
                                256,
                            );
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
                            *ui.file_tree_clipboard.borrow_mut() = None;
                        }
                        None => {}
                    }
                },
            );
            return;
        }

        // Cross-location batch: per-item streaming transfers with cumulative
        // progress, cancellation, and per-item failure collection.
        let plan = remote_fs::transfer_plan(&from, &location);
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
                    match remote_fs::transfer(
                        &from,
                        &hosts,
                        &item.src,
                        &to,
                        &item.dst,
                        item.is_dir,
                        &item_control,
                    ) {
                        Ok(()) => {
                            // A cut deletes only the source whose transfer
                            // actually succeeded.
                            if cut {
                                if let Err(error) =
                                    remote_fs::delete(&from, &hosts, &item.src)
                                {
                                    let detail =
                                        jterm_core::review_input::safe_inline_display(
                                            &error.to_string(),
                                            256,
                                        );
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
                            let detail = jterm_core::review_input::safe_inline_display(
                                &error.to_string(),
                                256,
                            );
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
                        *ui.file_tree_clipboard.borrow_mut() = None;
                    }
                    None => {}
                }
            },
        );
    }

    /// Re-list one directory into its already-materialized store, in place:
    /// surviving rows keep their TreeListRow identity, so expansion state
    /// and cached child models everywhere else in the tree are untouched.
    /// Directories that are not currently visible (collapsed or never
    /// expanded) need no refresh and are skipped.
    fn refresh_dir_listing(&self, dir: &Path) {
        let Some(store) = self
            .file_tree_model
            .materialized_children_of(&self.file_tree_root.borrow(), dir)
        else {
            return;
        };
        let location = self.file_tree_location.borrow().clone();
        let hosts = self.config.borrow().remote_hosts.clone();
        let generation = self.file_tree_model.generation.get();
        let generation_for_scan = self.file_tree_model.generation.clone();
        let location_for_scan = self.file_tree_location.clone();
        let scan_location = location.clone();
        let model_for_refresh = self.file_tree_model.clone();
        let dir_for_error = dir.to_path_buf();
        if let Err(error) = request_dir_scan(location, hosts, dir.to_path_buf(), move |result| {
            if generation_for_scan.get() != generation {
                return;
            }
            if *location_for_scan.borrow() != scan_location {
                return;
            }
            match result {
                Ok(entries) => {
                    update_store_in_place(&store, entries);
                    model_for_refresh.reapply_filter();
                }
                Err(error) => log::warn!(
                    "failed to refresh directory {}: {error}",
                    dir_for_error.display()
                ),
            }
        }) {
            log::warn!(
                "failed to start directory refresh for {}: {error}",
                dir.display()
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
        let ui = self.clone();
        let busy_for_apply = busy.clone();
        let apply = move |result: io::Result<()>| {
            if let Some(busy) = &busy_for_apply {
                busy.dismiss();
            }
            match result {
                Ok(()) => {
                    on_success();
                    for dir in &affected {
                        ui.refresh_dir_listing(dir);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    // A deliberate cancel is not a failure: neutral note, no
                    // warning in the log. Work completed before the cancel
                    // still becomes visible, so refresh like a success would.
                    log::info!("sidebar {verb} cancelled");
                    for dir in &affected {
                        ui.refresh_dir_listing(dir);
                    }
                    ui.toast_overlay.add_toast(adw::Toast::new("Cancelled"));
                }
                Err(error) => {
                    log::warn!("sidebar file operation {verb} failed: {error}");
                    let detail = if error.kind() == io::ErrorKind::AlreadyExists {
                        "An item with this name already exists".to_string()
                    } else {
                        jterm_core::review_input::safe_inline_display(&error.to_string(), 512)
                    };
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&format!("{verb} failed: {detail}")));
                }
            }
        };
        if let Err(error) = request_fs_op(work, apply) {
            if let Some(busy) = &busy {
                busy.dismiss();
            }
            log::warn!("failed to start sidebar file operation {verb}: {error}");
            let detail = jterm_core::review_input::safe_inline_display(&error.to_string(), 512);
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
    selector.update_property(&[gtk4::accessible::Property::Label(
        "Choose file tree location",
    )]);
    selector
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

    #[test]
    fn entries_sort_directories_first_then_by_name() {
        let mut entries = vec![
            FileEntry {
                name: "Zulu.txt".into(),
                path: PathBuf::from("Zulu.txt"),
                is_dir: false,
            },
            FileEntry {
                name: "beta".into(),
                path: PathBuf::from("beta"),
                is_dir: true,
            },
            FileEntry {
                name: "Alpha.txt".into(),
                path: PathBuf::from("Alpha.txt"),
                is_dir: false,
            },
            FileEntry {
                name: "Able".into(),
                path: PathBuf::from("Able"),
                is_dir: true,
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
                            .map(|entry| entry.path.clone())
                    })
            })
            .collect()
    }

    fn test_entry(path: &str, is_dir: bool) -> FileEntry {
        FileEntry {
            name: path.to_string(),
            path: PathBuf::from(path),
            is_dir,
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
        update_store_in_place(
            &store,
            vec![
                test_entry("/p/Able", true),
                test_entry("/p/delta", true),
                test_entry("/p/gamma", true),
                test_entry("/p/Alpha.txt", false),
                test_entry("/p/Zulu.txt", false),
            ],
        );

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
