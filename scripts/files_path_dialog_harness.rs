//! Actual path-dialog event callbacks with deterministic GTK widget holders.
//!
//! The runner inserts the current dialog method, context guards, path helpers,
//! and set_file_tree_root verbatim. Navigation is a dispatch spy; this verifies
//! which filesystem/path is requested, not real GTK layout or remote listing.

#![allow(dead_code)]
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};
#[derive(Clone, Debug, PartialEq, Eq)]
enum FsLocation {
    Local,
    Remote(usize),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FsExecutionOverlay;
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileTreeNavigationPoint {
    location: FsLocation,
    overlay: FsExecutionOverlay,
    root: PathBuf,
}
enum FileTreeNavigationAction {
    Push,
}
#[derive(Clone)]
struct Model {
    generation: Rc<Cell<u64>>,
    authority_current: Rc<Cell<bool>>,
}
impl Model {
    fn committed_authority_is_current(&self) -> bool {
        self.authority_current.get()
    }
}
#[derive(Clone, Default)]
struct ToastOverlay(Rc<Cell<usize>>);
impl ToastOverlay {
    fn add_toast(&self, _: adw::Toast) {
        self.0.set(self.0.get() + 1)
    }
}
mod jterm_core {
    pub mod review_input {
        pub fn is_visual_spoofing_character(_: char) -> bool {
            false
        }
    }
}
mod gtk4 {
    use super::*;
    type Click = Rc<dyn Fn(&Button)>;
    #[derive(Clone)]
    pub struct Button {
        pub label: String,
        callback: Rc<RefCell<Option<Click>>>,
    }
    impl Button {
        pub fn with_label(label: &str) -> Self {
            let b = Self {
                label: label.into(),
                callback: Rc::new(RefCell::new(None)),
            };
            BUTTONS.with(|all| all.borrow_mut().push(b.clone()));
            b
        }
        pub fn add_css_class(&self, _: &str) {}
        pub fn connect_clicked(&self, f: impl Fn(&Button) + 'static) {
            *self.callback.borrow_mut() = Some(Rc::new(f))
        }
        pub fn emit_clicked(&self) {
            let f = self.callback.borrow().clone().unwrap();
            f(self)
        }
    }
    pub struct Widget;
    #[derive(Clone)]
    pub struct Label;
    impl Label {
        pub fn new(_: Option<&str>) -> Self {
            Self
        }
        pub fn add_css_class(&self, _: &str) {}
        pub fn set_xalign(&self, _: f32) {}
        pub fn set_wrap(&self, _: bool) {}
        pub fn set_visible(&self, _: bool) {}
        pub fn set_text(&self, _: &str) {}
    }
    pub enum SelectionMode {
        None,
    }
    pub struct FlowBox;
    impl FlowBox {
        pub fn new() -> Self {
            Self
        }
        pub fn set_selection_mode(&self, _: SelectionMode) {}
        pub fn set_column_spacing(&self, _: u32) {}
        pub fn set_row_spacing(&self, _: u32) {}
        pub fn insert(&self, _: &Button, _: i32) {}
    }
}
mod adw {
    use super::*;
    pub struct Toast;
    impl Toast {
        pub fn new(_: &str) -> Self {
            Self
        }
    }
    #[derive(Clone)]
    pub struct Dialog(Rc<Cell<bool>>);
    pub struct DialogBuilder;
    impl DialogBuilder {
        pub fn title(self, _: &str) -> Self {
            self
        }
        pub fn content_width(self, _: i32) -> Self {
            self
        }
        pub fn build(self) -> Dialog {
            let dialog = Dialog(Rc::new(Cell::new(false)));
            DIALOGS.with(|all| all.borrow_mut().push(dialog.clone()));
            dialog
        }
    }
    impl Dialog {
        pub fn is_closed(&self) -> bool {
            self.0.get()
        }
        pub fn builder() -> DialogBuilder {
            DialogBuilder
        }
        pub fn close(&self) {
            self.0.set(true)
        }
        pub fn set_child(&self, _: Option<&ToolbarView>) {}
        pub fn present(&self, _: Option<&()>) {}
    }
    #[derive(Clone)]
    pub struct EntryRow(Rc<RefCell<String>>);
    impl EntryRow {
        pub fn new() -> Self {
            let e = Self(Rc::new(RefCell::new(String::new())));
            ENTRIES.with(|all| all.borrow_mut().push(e.clone()));
            e
        }
        pub fn set_title(&self, _: &str) {}
        pub fn set_text(&self, s: &str) {
            *self.0.borrow_mut() = s.into()
        }
        pub fn text(&self) -> String {
            self.0.borrow().clone()
        }
    }
    pub struct HeaderBar;
    impl HeaderBar {
        pub fn new() -> Self {
            Self
        }
        pub fn set_show_start_title_buttons(&self, _: bool) {}
        pub fn set_show_end_title_buttons(&self, _: bool) {}
        pub fn pack_start(&self, _: &gtk4::Button) {}
        pub fn pack_end(&self, _: &gtk4::Button) {}
    }
    pub struct ToolbarView;
    impl ToolbarView {
        pub fn new() -> Self {
            Self
        }
        pub fn add_top_bar(&self, _: &HeaderBar) {}
        pub fn set_content(&self, _: Option<&gtk4::Widget>) {}
    }
}
thread_local! {static DIALOGS:RefCell<Vec<adw::Dialog>>=RefCell::new(vec![]);static BUTTONS:RefCell<Vec<gtk4::Button>>=RefCell::new(vec![]);static ENTRIES:RefCell<Vec<adw::EntryRow>>=RefCell::new(vec![]);}
fn dialog_closed() -> bool {
    DIALOGS.with(|all| all.borrow().last().unwrap().is_closed())
}
fn click(label: &str) {
    let b = BUTTONS.with(|all| {
        all.borrow()
            .iter()
            .find(|b| b.label == label)
            .unwrap()
            .clone()
    });
    b.emit_clicked()
}
fn navigation_breadcrumb_button(p: &Path) -> gtk4::Button {
    gtk4::Button::with_label(&p.display().to_string())
}
fn navigation_path_content(_: &adw::EntryRow, _: &gtk4::Label, _: &gtk4::FlowBox) -> gtk4::Widget {
    gtk4::Widget
}
const MAX_NAVIGATION_PATH_BYTES: usize = 4096;
#[derive(Clone)]
struct UiState {
    file_tree_root: Rc<RefCell<PathBuf>>,
    file_tree_location: Rc<RefCell<FsLocation>>,
    file_tree_execution_overlay: Rc<RefCell<FsExecutionOverlay>>,
    file_tree_model: Model,
    toast_overlay: ToastOverlay,
    window: (),
    requests: Rc<RefCell<Vec<FileTreeNavigationPoint>>>,
}
impl UiState {
    fn navigate_file_tree_point(&self, p: FileTreeNavigationPoint, _: FileTreeNavigationAction) {
        self.requests.borrow_mut().push(p)
    }
    fn load_file_tree_root_immediately(&self, p: FileTreeNavigationPoint) {
        self.requests.borrow_mut().push(p)
    }
    // @files-path:item-0
    // @files-path:item-1
    // @files-path:item-2
    // @files-path:item-3
}
// @files-path:item-4
// @files-path:item-5
// @files-path:item-6

fn setup() -> UiState {
    UiState {
        file_tree_root: Rc::new(RefCell::new("/alpha/project".into())),
        file_tree_location: Rc::new(RefCell::new(FsLocation::Remote(0))),
        file_tree_execution_overlay: Rc::new(RefCell::new(FsExecutionOverlay)),
        file_tree_model: Model {
            generation: Rc::new(Cell::new(7)),
            authority_current: Rc::new(Cell::new(true)),
        },
        toast_overlay: ToastOverlay::default(),
        window: (),
        requests: Rc::new(RefCell::new(vec![])),
    }
}
fn switch_to_valid_new_tree(ui: &UiState) {
    *ui.file_tree_location.borrow_mut() = FsLocation::Remote(1);
    *ui.file_tree_root.borrow_mut() = "/beta/kept".into();
    ui.file_tree_model.generation.set(8);
    assert!(ui.file_tree_model.committed_authority_is_current())
}
#[test]
fn current_open_uses_original_tree() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    click("Open");
    assert_eq!(
        ui.requests.borrow().as_slice(),
        &[FileTreeNavigationPoint {
            location: FsLocation::Remote(0),
            overlay: FsExecutionOverlay,
            root: "/alpha/project".into()
        }]
    )
}
#[test]
fn current_breadcrumb_uses_original_tree() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    click("/alpha");
    assert_eq!(ui.requests.borrow()[0].location, FsLocation::Remote(0));
    assert_eq!(ui.requests.borrow()[0].root, PathBuf::from("/alpha"))
}
#[test]
fn stale_open_must_not_reinterpret_old_path_in_new_filesystem() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    switch_to_valid_new_tree(&ui);
    click("Open");
    assert!(!dialog_closed(), "stale dialog must remain open");
    assert!(
        ui.requests.borrow().is_empty(),
        "old dialog dispatched navigation against new authority: {:?}",
        ui.requests.borrow()
    )
}
#[test]
fn stale_breadcrumb_must_not_reinterpret_old_path_in_new_filesystem() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    switch_to_valid_new_tree(&ui);
    click("/alpha");
    assert!(!dialog_closed(), "stale dialog must remain open");
    assert!(
        ui.requests.borrow().is_empty(),
        "old breadcrumb dispatched navigation against new authority: {:?}",
        ui.requests.borrow()
    )
}
#[test]
fn stale_dialog_same_location_new_generation_must_not_override_newer_root() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    *ui.file_tree_root.borrow_mut() = "/alpha/newer".into();
    ui.file_tree_model.generation.set(8);
    click("Open");
    assert!(!dialog_closed(), "stale dialog must remain open");
    assert!(
        ui.requests.borrow().is_empty(),
        "old dialog overrides newer committed root: {:?}",
        ui.requests.borrow()
    )
}
#[test]
fn existing_generation_location_guard_rejects_both_stale_cases() {
    let ui = setup();
    let generation = ui.file_tree_model.generation.get();
    let location = ui.file_tree_location.borrow().clone();
    assert!(ui.require_current_file_tree_context(generation, &location));
    switch_to_valid_new_tree(&ui);
    assert!(!ui.require_current_file_tree_context(generation, &location));
    *ui.file_tree_location.borrow_mut() = location.clone();
    assert!(!ui.require_current_file_tree_context(generation, &location))
}
#[test]
fn current_authority_check_only_rejects_invalid_current_profile() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    switch_to_valid_new_tree(&ui);
    ui.file_tree_model.authority_current.set(false);
    click("Open");
    assert!(ui.requests.borrow().is_empty());
    assert_eq!(ui.toast_overlay.0.get(), 1)
}
#[test]
fn cancel_does_not_dispatch_navigation() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    switch_to_valid_new_tree(&ui);
    click("Cancel");
    assert!(ui.requests.borrow().is_empty())
}

#[test]
fn current_typed_path_is_validated_and_navigates() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    ENTRIES.with(|all| {
        all.borrow()
            .last()
            .unwrap()
            .set_text("/alpha/new/../chosen")
    });
    click("Open");
    assert!(dialog_closed());
    assert_eq!(ui.requests.borrow()[0].location, FsLocation::Remote(0));
    assert_eq!(ui.requests.borrow()[0].root, PathBuf::from("/alpha/chosen"))
}
#[test]
fn invalid_typed_path_keeps_dialog_open_without_navigation() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    ENTRIES.with(|all| all.borrow().last().unwrap().set_text("relative/path"));
    click("Open");
    assert!(!dialog_closed());
    assert!(ui.requests.borrow().is_empty())
}
#[test]
fn stale_breadcrumb_same_location_new_generation_is_rejected() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    *ui.file_tree_root.borrow_mut() = "/alpha/newer".into();
    ui.file_tree_model.generation.set(8);
    click("/alpha");
    assert!(!dialog_closed());
    assert!(ui.requests.borrow().is_empty())
}
#[test]
fn location_aba_cannot_revive_old_dialog() {
    let ui = setup();
    ui.present_file_tree_path_dialog();
    switch_to_valid_new_tree(&ui);
    *ui.file_tree_location.borrow_mut() = FsLocation::Remote(0);
    *ui.file_tree_root.borrow_mut() = "/alpha/returned".into();
    ui.file_tree_model.generation.set(9);
    click("Open");
    assert!(!dialog_closed());
    assert!(ui.requests.borrow().is_empty())
}
