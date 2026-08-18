//! file_tree — asynchronous GTK4 sidebar file browser for UiState.
//!
//! The browser uses `TreeListModel` + `ListView`, the supported GTK4 model-view
//! stack. Directory enumeration remains off the UI thread and is created lazily
//! when a directory row is expanded. Listing and file operations dispatch
//! through `super::remote_fs`: the tree browses the local disk or any
//! configured ssh/docker remote host, and a right-click context menu offers
//! New File/Folder, Rename, Delete, Copy, Cut, Paste and Refresh on both.
//! Paste across locations streams between the two filesystems (download,
//! upload, or a temp-relayed remote-to-remote hop). After a mutation only
//! the affected parent directories are re-listed, in place with a minimal
//! diff, so expansion state elsewhere in the tree survives.

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
    generation: Rc<Cell<u64>>,
    /// Browsed filesystem, shared with UiState; scans snapshot it at request
    /// time and stale results are dropped when it has moved on.
    location: Rc<RefCell<FsLocation>>,
    /// Host list source; each scan snapshots `remote_hosts` so a mid-scan
    /// config reload cannot redirect an in-flight listing at another host.
    config: Rc<RefCell<crate::config::Config>>,
}

impl FileTreeModel {
    fn new(location: Rc<RefCell<FsLocation>>, config: Rc<RefCell<crate::config::Config>>) -> Self {
        let root_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let generation = Rc::new(Cell::new(0_u64));
        let tree_model = TreeListModel::new(root_store.clone(), false, false, {
            let generation = generation.clone();
            let location = location.clone();
            let config = config.clone();
            move |object| {
                let boxed = object.downcast_ref::<glib::BoxedAnyObject>()?;
                let entry = boxed.try_borrow::<FileEntry>().ok()?;
                if !entry.is_dir {
                    return None;
                }
                let path = entry.path.clone();
                drop(entry);

                let children = gio::ListStore::new::<glib::BoxedAnyObject>();
                let children_for_scan = children.clone();
                let generation_for_scan = generation.clone();
                let expected_generation = generation.get();
                let scan_location = location.borrow().clone();
                let location_for_scan = location.clone();
                let scan_hosts = config.borrow().remote_hosts.clone();
                let path_for_result = path.clone();
                let path_for_error = path.clone();
                if let Err(error) =
                    request_dir_scan(scan_location.clone(), scan_hosts, path, move |result| {
                        if generation_for_scan.get() != expected_generation {
                            return;
                        }
                        if *location_for_scan.borrow() != scan_location {
                            return;
                        }
                        match result {
                            Ok(entries) => append_entries(&children_for_scan, entries),
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

        Self {
            root_store,
            tree_model,
            generation,
            location,
            config,
        }
    }

    fn reset(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.root_store.remove_all();
        generation
    }

    fn replace_root(&self, generation: u64, entries: Vec<FileEntry>) -> bool {
        if self.generation.get() != generation {
            return false;
        }
        self.root_store.remove_all();
        append_entries(&self.root_store, entries);
        true
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
        let row = self.tree_model.row(position)?;
        let entry = entry_from_row(&row)?;
        Some((row, entry))
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

    let selection = gtk4::SingleSelection::new(Some(model.tree_model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);

    let file_tree = ListView::new(Some(selection), Some(factory));
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

    /// Rebuild the location selector's entries from the configured hosts and
    /// re-select the active location. Selection changes that do not match the
    /// active location are left to the notify handler, which performs the
    /// actual switch (a removed host thus falls back to Local).
    pub(crate) fn refresh_file_tree_location_selector(&self) {
        let hosts = self.config.borrow().remote_hosts.clone();
        let mut labels = vec![FsLocation::Local.label(&hosts)];
        labels.extend((0..hosts.len()).map(|index| FsLocation::Remote(index).label(&hosts)));
        let selected = match &*self.file_tree_location.borrow() {
            FsLocation::Local => 0,
            FsLocation::Remote(index) if *index < hosts.len() => *index as u32 + 1,
            FsLocation::Remote(_) => 0,
        };
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
    }

    /// Right-click file-operations menu for `target`. New File/Folder, Paste
    /// and Refresh act on the target directory (a file row contributes its
    /// parent); Rename/Delete/Copy/Cut need a row and are insensitive without
    /// one.
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
        let has_target = target.is_some();
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
            item.set_sensitive(has_target);
            if let Some((_, entry)) = target.clone() {
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
            let item = make_item("Delete");
            item.set_sensitive(has_target);
            if let Some((_, entry)) = target.clone() {
                let popover_c = popover.clone();
                let ui = self.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    ui.confirm_file_tree_delete(entry.clone());
                });
            }
            vbox.append(&item);
        }

        for (label, cut) in [("Copy", false), ("Cut", true)] {
            let item = make_item(label);
            item.set_sensitive(has_target);
            if let Some((_, entry)) = target.clone() {
                let popover_c = popover.clone();
                let ui = self.clone();
                let location = location.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    *ui.file_tree_clipboard.borrow_mut() = Some(FsClipboard {
                        loc: location.clone(),
                        path: entry.path.clone(),
                        is_dir: entry.is_dir,
                        cut,
                    });
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
            let pasteable = clipboard
                .as_ref()
                .is_some_and(|clip| clip.path.file_name().is_some());
            item.set_sensitive(pasteable);
            if clipboard.is_none() {
                item.set_tooltip_text(Some("Copy or cut an item first"));
            }
            if let Some(clip) = clipboard.filter(|clip| clip.path.file_name().is_some()) {
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

    /// Destructive-delete confirmation naming the full path, styled after the
    /// host-removal alert dialog.
    fn confirm_file_tree_delete(&self, entry: FileEntry) {
        let display =
            jterm_core::review_input::safe_inline_display(&entry.path.to_string_lossy(), 1024);
        let detail = if entry.is_dir {
            format!("“{display}” and everything inside it will be permanently deleted.")
        } else {
            format!("“{display}” will be permanently deleted.")
        };
        let dialog = adw::AlertDialog::new(Some("Delete this item?"), Some(&detail));
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            let location = ui.file_tree_location.borrow().clone();
            let hosts = ui.config.borrow().remote_hosts.clone();
            let path = entry.path.clone();
            let affected: Vec<PathBuf> = path.parent().map(Path::to_path_buf).into_iter().collect();
            ui.execute_fs_op(
                "Delete",
                None,
                affected,
                move || remote_fs::delete(&location, &hosts, &path),
                || {},
            );
        });
        dialog.present(Some(&self.window));
    }

    /// Paste `clip` into `target_dir`. Same location: a cut moves (rename), a
    /// copy duplicates. Different locations: a streaming transfer (download,
    /// upload, or temp-relayed remote-to-remote hop), with a cut then
    /// deleting the source only after the transfer landed. The clipboard
    /// clears only after a fully successful cut.
    fn paste_file_tree_clipboard(&self, clip: FsClipboard, target_dir: PathBuf) {
        let location = self.file_tree_location.borrow().clone();
        let dst = remote_fs::paste_destination(&target_dir, &clip.path);
        let hosts = self.config.borrow().remote_hosts.clone();
        let src = clip.path.clone();
        let cut = clip.cut;

        if clip.loc == location {
            if dst == src {
                self.toast_overlay.add_toast(adw::Toast::new(
                    "Paste failed: source and target are the same",
                ));
                return;
            }
            // A copy lands in the target dir; a cut also removes the entry
            // from its old parent. Refresh exactly those.
            let mut affected: Vec<PathBuf> = Vec::new();
            if cut {
                if let Some(parent) = src.parent() {
                    affected.push(parent.to_path_buf());
                }
            }
            if !affected.contains(&target_dir) {
                affected.push(target_dir);
            }
            let ui = self.clone();
            self.execute_fs_op(
                "Paste",
                None,
                affected,
                move || {
                    if cut {
                        remote_fs::rename(&location, &hosts, &src, &dst)
                    } else {
                        remote_fs::copy(&location, &hosts, &src, &dst)
                    }
                },
                move || {
                    if cut {
                        *ui.file_tree_clipboard.borrow_mut() = None;
                    }
                },
            );
            return;
        }

        // Cross-location: a streaming transfer on the same bounded op-worker
        // path, with a persistent busy toast until it finishes.
        let from = clip.loc.clone();
        let is_dir = clip.is_dir;
        let verb: &'static str = if cut {
            "Move"
        } else {
            match remote_fs::transfer_plan(&from, &location) {
                Some(remote_fs::TransferPlan::Download) => "Download",
                Some(remote_fs::TransferPlan::Upload) => "Upload",
                _ => "Transfer",
            }
        };
        let display = jterm_core::review_input::safe_inline_display(
            &src.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            256,
        );
        let busy = adw::Toast::new(&format!("{verb} in progress: {display}"));
        busy.set_timeout(0);
        self.toast_overlay.add_toast(busy.clone());

        // Only the destination parent is visible in this tree; the source
        // side lives on another location.
        let affected = vec![target_dir];
        let hosts_for_copy = hosts.clone();
        let (from_copy, from_delete) = (from.clone(), from.clone());
        let (src_copy, src_delete) = (src.clone(), src.clone());
        let (hosts_delete, dst_copy) = (hosts.clone(), dst.clone());
        let to = location.clone();
        let ui = self.clone();
        self.execute_fs_op(
            verb,
            Some(busy),
            affected,
            move || {
                if cut {
                    remote_fs::copy_then_delete(
                        || {
                            remote_fs::transfer(
                                &from_copy,
                                &hosts_for_copy,
                                &src_copy,
                                &to,
                                &dst_copy,
                                is_dir,
                            )
                        },
                        || remote_fs::delete(&from_delete, &hosts_delete, &src_delete),
                    )
                } else {
                    remote_fs::transfer(
                        &from_copy,
                        &hosts_for_copy,
                        &src_copy,
                        &to,
                        &dst_copy,
                        is_dir,
                    )
                }
            },
            move || {
                if cut {
                    *ui.file_tree_clipboard.borrow_mut() = None;
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
        let dir_for_error = dir.to_path_buf();
        if let Err(error) = request_dir_scan(location, hosts, dir.to_path_buf(), move |result| {
            if generation_for_scan.get() != generation {
                return;
            }
            if *location_for_scan.borrow() != scan_location {
                return;
            }
            match result {
                Ok(entries) => update_store_in_place(&store, entries),
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
                    for dir in affected {
                        ui.refresh_dir_listing(&dir);
                    }
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
}
