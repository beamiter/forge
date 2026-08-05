//! file_tree — asynchronous GTK4 sidebar file browser for UiState.
//!
//! The browser uses `TreeListModel` + `ListView`, the supported GTK4 model-view
//! stack. Directory enumeration remains off the UI thread and is created lazily
//! when a directory row is expanded.

use gtk4::prelude::*;
use gtk4::{gio, glib, ListView, SignalListItemFactory, TreeListModel, TreeListRow};
use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::terminal::terminal_working_directory;

const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(16);
const MAX_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_FILE_LABEL_BYTES: usize = 512;
const MAX_CONCURRENT_SCANS: usize = 8;
static ACTIVE_SCANS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
struct FileEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn scan_dir(dir: &Path) -> io::Result<Vec<FileEntry>> {
    let mut entries = Vec::with_capacity(256);
    for entry in std::fs::read_dir(dir)?
        .take(MAX_DIRECTORY_ENTRIES)
        .flatten()
    {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        entries.push(FileEntry {
            name: safe_file_label(&entry.file_name().to_string_lossy()),
            is_dir: file_type.is_dir(),
            path,
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

fn safe_file_label(value: &str) -> String {
    let mut label = String::with_capacity(value.len().min(MAX_FILE_LABEL_BYTES));
    for ch in value.chars() {
        let rendered = if ch.is_control() || crate::review_input::is_visual_spoof_character(ch) {
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

fn request_dir_scan<F>(dir: PathBuf, apply: F) -> io::Result<()>
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
            let _ = tx.send(scan_dir(&dir));
        })
    {
        ACTIVE_SCANS.fetch_sub(1, Ordering::AcqRel);
        return Err(error);
    }

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
                apply(Err(io::Error::other("file-tree scan worker disconnected")));
            }
            glib::ControlFlow::Break
        }
    });
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
}

impl FileTreeModel {
    fn new() -> Self {
        let root_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let generation = Rc::new(Cell::new(0_u64));
        let tree_model = TreeListModel::new(root_store.clone(), false, false, {
            let generation = generation.clone();
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
                let path_for_result = path.clone();
                let path_for_error = path.clone();
                if let Err(error) = request_dir_scan(path, move |result| {
                    if generation_for_scan.get() != expected_generation {
                        return;
                    }
                    match result {
                        Ok(entries) => append_entries(&children_for_scan, entries),
                        Err(error) => log::warn!(
                            "failed to scan directory {}: {error}",
                            path_for_result.display()
                        ),
                    }
                }) {
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

    fn row_entry(&self, position: u32) -> Option<(TreeListRow, FileEntry)> {
        let row = self.tree_model.row(position)?;
        let entry = entry_from_row(&row)?;
        Some((row, entry))
    }
}

/// Build the modern GTK4 list-model file browser.
pub(crate) fn build_file_tree_widgets() -> (FileTreeModel, ListView) {
    let model = FileTreeModel::new();
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
        self.file_tree_root_label.set_text(&display_path(&root));
        let root_tooltip = safe_file_label(&root.to_string_lossy());
        self.file_tree_root_label
            .set_tooltip_text(Some(&root_tooltip));
        *self.file_tree_root.borrow_mut() = root.clone();

        let model = self.file_tree_model.clone();
        let expected_root = root.clone();
        let active_root = self.file_tree_root.clone();
        if let Err(error) = request_dir_scan(root, move |result| {
            if *active_root.borrow() != expected_root {
                return;
            }
            match result {
                Ok(entries) => {
                    model.replace_root(generation, entries);
                }
                Err(error) => log::warn!(
                    "failed to scan file-tree root {}: {error}",
                    expected_root.display()
                ),
            }
        }) {
            log::warn!("failed to start file-tree scan: {error}");
        }
    }

    /// Jump the file tree to the active tab's working directory.
    pub(crate) fn file_tree_goto_current_cwd(&self) {
        let cwd = self
            .current_terminal()
            .as_ref()
            .and_then(terminal_working_directory)
            .map(PathBuf::from)
            .filter(|path| path.is_dir());
        match cwd {
            Some(dir) => {
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
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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
}
