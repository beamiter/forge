//! Display-free regression fixture for the actual automatic/manual Files callbacks.
//!
//! Run with `python3 scripts/test-files-follow.py`. The runner inserts current
//! production bodies at the markers below; no callback implementation is copied.
//! GTK, process observation, config validation, and SSH/filesystem transport are
//! deterministic doubles. Worker/apply queues expose the asynchronous boundaries,
//! and hooks inside home/list force source-focus, cancellation, and operation races.
//! This suite does not replace real GTK rendering or live SSH integration tests.

#![allow(dead_code)]
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    time::{Duration, Instant},
};
extern crate self as log;
#[macro_export]
macro_rules! warn { ($($t:tt)*) => {{let _=format_args!($($t)*);}}; }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHostConfig {
    host: String,
    user: Option<String>,
    ssh_args: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteHost {
    host: String,
    user: Option<String>,
    ssh_args: Vec<String>,
    docker: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum FsLocation {
    Local,
    Remote(usize),
    Transient(RemoteHostConfig),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FsExecutionOverlay {
    control_path: Option<PathBuf>,
}
mod jterm_core {
    pub mod jsh_remote {
        pub use crate::RemoteHostConfig;
        #[derive(Clone)]
        pub enum ObservedSshTarget {
            Target(RemoteHostConfig),
            Unsupported,
        }
    }
    pub mod process {
        use super::jsh_remote::*;
        #[derive(Clone)]
        pub struct ObservedSshCommand {
            pub argv: Vec<String>,
            pub target: ObservedSshTarget,
            pub reusable_control_path: Option<std::path::PathBuf>,
        }
    }
}
mod config {
    use super::*;
    pub const MAX_REMOTE_HOSTS: usize = 64;
    pub fn validate_remote_host(_: &RemoteHost) -> io::Result<()> {
        Ok(())
    }
    pub fn checked_remote_host(hosts: &[RemoteHost], i: usize) -> io::Result<&RemoteHost> {
        hosts.get(i).ok_or_else(|| io::Error::other("missing host"))
    }
}
mod remote_fs {
    use super::*;
    pub const MAX_DIRECTORY_ENTRIES: usize = 4096;
    // @files-follow:cancel-token
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FilesystemIdentity {
        Local,
        Remote(RemoteHostConfig),
    }
    pub fn filesystem_identity(
        location: &FsLocation,
        hosts: &[RemoteHost],
    ) -> io::Result<FilesystemIdentity> {
        Ok(match location {
            FsLocation::Local => FilesystemIdentity::Local,
            FsLocation::Transient(t) => FilesystemIdentity::Remote(t.clone()),
            FsLocation::Remote(i) => {
                let h = config::checked_remote_host(hosts, *i)?;
                FilesystemIdentity::Remote(RemoteHostConfig {
                    host: h.host.clone(),
                    user: h.user.clone(),
                    ssh_args: h.ssh_args.clone(),
                })
            }
        })
    }
    pub fn transient_remote_host(t: &RemoteHostConfig) -> io::Result<RemoteHost> {
        Ok(RemoteHost {
            host: t.host.clone(),
            user: t.user.clone(),
            ssh_args: t.ssh_args.clone(),
            docker: false,
        })
    }
    pub fn stable_ssh_args(args: &[String]) -> io::Result<Vec<String>> {
        Ok(args.to_vec())
    }
    pub fn observed_target_and_overlay(
        t: RemoteHostConfig,
        p: Option<PathBuf>,
    ) -> io::Result<(RemoteHostConfig, FsExecutionOverlay)> {
        Ok((t, FsExecutionOverlay { control_path: p }))
    }
    pub fn cancelled_error() -> io::Error {
        io::Error::new(io::ErrorKind::Interrupted, "cancelled")
    }
    pub fn start_dir_with_overlay_cancel(
        location: &FsLocation,
        hosts: &[RemoteHost],
        overlay: &FsExecutionOverlay,
        cancellation: &CancelToken,
    ) -> io::Result<PathBuf> {
        if cancellation.is_cancelled() {
            return Err(cancelled_error());
        }
        start_dir_with_overlay(location, hosts, overlay)
    }
    pub fn start_dir_with_overlay(
        _: &FsLocation,
        _: &[RemoteHost],
        _: &FsExecutionOverlay,
    ) -> io::Result<PathBuf> {
        HOME_CALLS.with(|n| n.set(n.get() + 1));
        run_hook(&HOME_HOOK);
        Ok(PathBuf::from("/remote/home"))
    }
    pub struct Listing {
        pub entries: Vec<FileEntry>,
        pub truncated: bool,
    }
    pub fn list_dir_with_overlay_cancel(
        _: &FsLocation,
        _: &[RemoteHost],
        _: &FsExecutionOverlay,
        _: &Path,
        cancel: &CancelToken,
    ) -> io::Result<Listing> {
        LIST_CALLS.with(|n| n.set(n.get() + 1));
        run_hook(&LIST_HOOK);
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        if LIST_FAIL.with(Cell::get) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "listing failed",
            ));
        }
        Ok(Listing {
            entries: vec![FileEntry("remote child")],
            truncated: false,
        })
    }
}
mod adw {
    pub struct Toast;
    impl Toast {
        pub fn new(_: &str) -> Self {
            Self
        }
    }
}
#[derive(Clone, Default)]
struct ToastOverlay {
    count: Rc<Cell<u64>>,
}
impl ToastOverlay {
    fn add_toast(&self, _: adw::Toast) {
        self.count.set(self.count.get() + 1)
    }
}
#[derive(Clone, Default)]
struct Label;
impl Label {
    fn set_text(&self, _: &str) {}
    fn set_tooltip_text(&self, _: Option<&str>) {}
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileEntry(&'static str);
#[derive(Clone, Copy, Default)]
struct ScanTiming;
struct DirectoryScan {
    entries: Vec<FileEntry>,
    timing: ScanTiming,
    truncated: bool,
}
#[derive(Clone, Default)]
struct Model {
    generation: Rc<Cell<u64>>,
    rows: Rc<RefCell<Vec<FileEntry>>>,
    resets: Rc<Cell<u64>>,
    rebinds: Rc<Cell<u64>>,
    authority: Rc<RefCell<Option<remote_fs::FilesystemIdentity>>>,
}
impl Model {
    fn reset(&self) -> u64 {
        self.generation.set(self.generation.get() + 1);
        self.resets.set(self.resets.get() + 1);
        self.generation.get()
    }
    fn set_committed_authority(&self, a: remote_fs::FilesystemIdentity) {
        *self.authority.borrow_mut() = Some(a)
    }
    fn set_root_path(&self, _: &Path) {}
    fn mark_snapshot_completed(&self, _: &Path) {}
    fn replace_root(&self, _: u64, entries: Vec<FileEntry>) {
        *self.rows.borrow_mut() = entries
    }
    fn cancel_pending_scans_preserve_tree(&self) {
        self.rebinds.set(self.rebinds.get() + 1)
    }
}
#[derive(Clone)]
struct Root(Rc<()>);
impl PartialEq for Root {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
struct WeakRoot(Weak<()>);
impl Root {
    fn downgrade(&self) -> WeakRoot {
        WeakRoot(Rc::downgrade(&self.0))
    }
}
impl WeakRoot {
    fn upgrade(&self) -> Option<Root> {
        self.0.upgrade().map(Root)
    }
}
#[derive(Clone)]
struct Leaf {
    root: Root,
    focus: Rc<Cell<u64>>,
    command: Rc<RefCell<jterm_core::process::ObservedSshCommand>>,
}
impl Leaf {
    fn is_remote(&self) -> bool {
        false
    }
    fn session_id(&self) -> Option<String> {
        Some("source-pane".into())
    }
    fn focus_serial(&self) -> u64 {
        self.focus.get()
    }
    fn root_widget(&self) -> Root {
        self.root.clone()
    }
    fn observed_ssh_command(&self) -> Option<jterm_core::process::ObservedSshCommand> {
        Some(self.command.borrow().clone())
    }
}
struct Config {
    remote_hosts: Vec<RemoteHost>,
}
#[derive(Default)]
struct StoreReconcileDelta {
    inserted_rows: usize,
}
struct ScanScheduler;
impl ScanScheduler {
    fn global() -> std::io::Result<Self> {
        Ok(Self)
    }
    fn retire_cancelled(&self) {}
}
enum ScanPriority {
    Root,
}
fn root_display_label(_: &FsLocation, p: &Path) -> String {
    p.display().to_string()
}
fn public_directory_error_message(_: &io::Error) -> &'static str {
    "failed"
}
fn log_scan_timing(_: &Path, _: ScanTiming, _: Duration, _: &StoreReconcileDelta) {}
type Job = Box<dyn FnOnce()>;
thread_local! {
 static WORKERS:RefCell<VecDeque<Job>>=RefCell::new(VecDeque::new());
 static COMPLETIONS:RefCell<VecDeque<Job>>=RefCell::new(VecDeque::new());
 static HOME_HOOK:RefCell<Option<Job>>=RefCell::new(None);
 static LIST_HOOK:RefCell<Option<Job>>=RefCell::new(None);
 static HOME_CALLS:Cell<usize>=const{Cell::new(0)};
 static LIST_CALLS:Cell<usize>=const{Cell::new(0)};
 static LIST_FAIL:Cell<bool>=const{Cell::new(false)};
 static ADMISSION_FAIL:Cell<bool>=const{Cell::new(false)};
}
fn run_hook(slot: &'static std::thread::LocalKey<RefCell<Option<Job>>>) {
    if let Some(f) = slot.with(|s| s.borrow_mut().take()) {
        f()
    }
}
fn request_fs_op<T, F, W>(work: W, apply: F) -> io::Result<()>
where
    T: Send + 'static,
    F: FnOnce(io::Result<T>) + 'static,
    W: FnOnce() -> io::Result<T> + Send + 'static,
{
    if ADMISSION_FAIL.with(Cell::get) {
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "queue full"));
    }
    WORKERS.with(|q| {
        q.borrow_mut().push_back(Box::new(move || {
            let result = work();
            COMPLETIONS.with(|q| q.borrow_mut().push_back(Box::new(move || apply(result))))
        }))
    });
    Ok(())
}
fn request_dir_scan<F: FnOnce(io::Result<DirectoryScan>) + 'static>(
    l: FsLocation,
    h: Vec<RemoteHost>,
    o: FsExecutionOverlay,
    p: PathBuf,
    c: remote_fs::CancelToken,
    _: ScanPriority,
    apply: F,
) -> io::Result<()> {
    request_fs_op(move || scan_entries(&l, &h, &o, &p, &c), apply)
}
const MAX_FILE_TREE_HISTORY: usize = 64;
// @files-follow:navigation
// @files-follow:context
// @files-follow:helpers

#[derive(Clone)]
struct UiState {
    file_tree_navigation: Rc<RefCell<FileTreeNavigationState>>,
    config: Rc<RefCell<Config>>,
    file_tree_root_label: Label,
    file_tree_model: Model,
    file_tree_location: Rc<RefCell<FsLocation>>,
    file_tree_execution_overlay: Rc<RefCell<FsExecutionOverlay>>,
    file_tree_root: Rc<RefCell<PathBuf>>,
    toast_overlay: ToastOverlay,
    file_tree_remote_follow_intent: Rc<Cell<u64>>,
    file_tree_active_operations: Rc<Cell<u64>>,
    file_tree_operation_intent: Rc<Cell<Option<u64>>>,
    tab_focus_generation: Rc<Cell<u64>>,
    leaf: Rc<RefCell<Option<Leaf>>>,
}
impl UiState {
    fn refresh_file_tree_root_header(&self) {}
    fn refresh_file_tree_location_selector(&self) {}
    fn current_pane_leaf(&self) -> Option<Leaf> {
        self.leaf.borrow().clone()
    }
    fn show_remote_follow_failure(
        &self,
        _: String,
        _: Vec<String>,
        _: RemoteHostConfig,
        _: io::Error,
    ) {
        self.toast_overlay.add_toast(adw::Toast::new("failed"))
    }
    // @files-follow:methods
}

fn target() -> RemoteHostConfig {
    RemoteHostConfig {
        host: "host.example".into(),
        user: Some("dev".into()),
        ssh_args: vec![],
    }
}
fn ui() -> UiState {
    assert!(
        std::env::var_os("FORGE_SAFE_MODE").is_none(),
        "test environment must allow automatic follow"
    );
    let location = FsLocation::Local;
    let overlay = FsExecutionOverlay::default();
    let root = PathBuf::from("/local/kept");
    let mut navigation = FileTreeNavigationState::default();
    navigation.install_initial(FileTreeNavigationPoint {
        location: location.clone(),
        overlay: overlay.clone(),
        root: root.clone(),
    });
    UiState {
        file_tree_navigation: Rc::new(RefCell::new(navigation)),
        config: Rc::new(RefCell::new(Config {
            remote_hosts: vec![remote_fs::transient_remote_host(&target()).unwrap()],
        })),
        file_tree_root_label: Label,
        file_tree_model: Model {
            rows: Rc::new(RefCell::new(vec![FileEntry("kept child")])),
            ..Model::default()
        },
        file_tree_location: Rc::new(RefCell::new(location)),
        file_tree_execution_overlay: Rc::new(RefCell::new(overlay)),
        file_tree_root: Rc::new(RefCell::new(root)),
        toast_overlay: ToastOverlay::default(),
        file_tree_remote_follow_intent: Rc::new(Cell::new(7)),
        file_tree_active_operations: Rc::new(Cell::new(0)),
        file_tree_operation_intent: Rc::new(Cell::new(Some(9))),
        tab_focus_generation: Rc::new(Cell::new(11)),
        leaf: Rc::new(RefCell::new(Some(Leaf {
            root: Root(Rc::new(())),
            focus: Rc::new(Cell::new(13)),
            command: Rc::new(RefCell::new(jterm_core::process::ObservedSshCommand {
                argv: vec!["ssh".into(), "dev@host.example".into()],
                target: jterm_core::jsh_remote::ObservedSshTarget::Target(target()),
                reusable_control_path: Some(PathBuf::from("/run/live.sock")),
            })),
        }))),
    }
}
fn start(ui: &UiState) {
    let (session, _, command) = ui.current_observed_ssh_command().unwrap();
    ui.stage_observed_remote_files(session, command)
}
fn run_one(queue: &'static std::thread::LocalKey<RefCell<VecDeque<Job>>>) -> bool {
    let next = queue.with(|q| q.borrow_mut().pop_front());
    if let Some(f) = next {
        f();
        true
    } else {
        false
    }
}
fn settle() {
    for _ in 0..10 {
        let work = run_one(&WORKERS);
        let apply = run_one(&COMPLETIONS);
        if !work && !apply {
            return;
        }
    }
    panic!("callbacks did not settle")
}
fn on_list(f: impl FnOnce() + 'static) {
    LIST_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(f)))
}
fn on_home(f: impl FnOnce() + 'static) {
    HOME_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(f)))
}
fn focus_changed(ui: &UiState) {
    let leaf = ui.current_pane_leaf().unwrap();
    leaf.focus.set(leaf.focus.get() + 1)
}
fn assert_kept(ui: &UiState) {
    assert_eq!(*ui.file_tree_location.borrow(), FsLocation::Local);
    assert_eq!(*ui.file_tree_root.borrow(), PathBuf::from("/local/kept"));
    assert_eq!(
        *ui.file_tree_model.rows.borrow(),
        vec![FileEntry("kept child")]
    );
    assert_eq!(ui.file_tree_model.resets.get(), 0);
    assert!(ui.file_tree_navigation.borrow().back.is_empty())
}
fn same_target_ui() -> UiState {
    let ui = ui();
    let point = FileTreeNavigationPoint {
        location: FsLocation::Remote(0),
        overlay: FsExecutionOverlay {
            control_path: Some(PathBuf::from("/run/old.sock")),
        },
        root: PathBuf::from("/remote/kept"),
    };
    *ui.file_tree_location.borrow_mut() = point.location.clone();
    *ui.file_tree_execution_overlay.borrow_mut() = point.overlay.clone();
    *ui.file_tree_root.borrow_mut() = point.root.clone();
    ui.file_tree_navigation.borrow_mut().install_initial(point);
    ui
}

#[test]
fn automatic_current_source_commits_first_listing_in_same_callback() {
    let ui = ui();
    start(&ui);
    assert!(run_one(&WORKERS));
    assert!(run_one(&COMPLETIONS));
    assert_eq!(*ui.file_tree_location.borrow(), FsLocation::Remote(0));
    assert_eq!(*ui.file_tree_root.borrow(), PathBuf::from("/remote/home"));
    assert_eq!(
        *ui.file_tree_model.rows.borrow(),
        vec![FileEntry("remote child")]
    );
    assert_eq!(ui.file_tree_model.resets.get(), 1);
    assert_eq!(ui.file_tree_navigation.borrow().back.len(), 1);
    assert!(!run_one(&WORKERS), "no second worker after source gate");
    assert!(!run_one(&COMPLETIONS));
    assert_eq!(LIST_CALLS.with(Cell::get), 1)
}
#[test]
fn automatic_focus_changed_during_initial_listing_cannot_publish() {
    let ui = ui();
    let source = ui.clone();
    on_list(move || focus_changed(&source));
    start(&ui);
    settle();
    assert_eq!(LIST_CALLS.with(Cell::get), 1);
    assert_kept(&ui);
    assert_eq!(ui.toast_overlay.count.get(), 0)
}
#[test]
fn automatic_source_closed_during_initial_listing_cannot_publish() {
    let ui = ui();
    let source = ui.clone();
    on_list(move || *source.leaf.borrow_mut() = None);
    start(&ui);
    settle();
    assert_kept(&ui)
}
#[test]
fn automatic_command_changed_during_initial_listing_cannot_publish() {
    let ui = ui();
    let source = ui.clone();
    on_list(move || {
        source
            .current_pane_leaf()
            .unwrap()
            .command
            .borrow_mut()
            .argv
            .push("different".into())
    });
    start(&ui);
    settle();
    assert_kept(&ui)
}
#[test]
fn automatic_file_operation_during_initial_listing_cannot_publish() {
    let ui = ui();
    let source = ui.clone();
    on_list(move || source.file_tree_operation_intent.set(Some(10)));
    start(&ui);
    settle();
    assert_kept(&ui)
}
#[test]
fn automatic_focus_changed_after_worker_before_apply_cannot_publish() {
    let ui = ui();
    start(&ui);
    assert!(run_one(&WORKERS));
    focus_changed(&ui);
    settle();
    assert_kept(&ui)
}
#[test]
fn automatic_cancelled_before_worker_skips_all_transport() {
    let ui = ui();
    start(&ui);
    ui.invalidate_file_tree_remote_follow();
    settle();
    assert_eq!(HOME_CALLS.with(Cell::get), 0);
    assert_eq!(LIST_CALLS.with(Cell::get), 0);
    assert_kept(&ui);
    assert_eq!(ui.toast_overlay.count.get(), 0)
}
#[test]
fn automatic_cancelled_after_home_skips_listing() {
    let ui = ui();
    let source = ui.clone();
    on_home(move || source.invalidate_file_tree_remote_follow());
    start(&ui);
    settle();
    assert_eq!(HOME_CALLS.with(Cell::get), 1);
    assert_eq!(LIST_CALLS.with(Cell::get), 0);
    assert_kept(&ui);
    assert_eq!(ui.toast_overlay.count.get(), 0)
}
#[test]
fn automatic_cancelled_during_listing_cannot_publish() {
    let ui = ui();
    let source = ui.clone();
    on_list(move || source.invalidate_file_tree_remote_follow());
    start(&ui);
    settle();
    assert_kept(&ui);
    assert_eq!(ui.toast_overlay.count.get(), 0)
}
#[test]
fn automatic_listing_failure_keeps_committed_tree_and_history() {
    let ui = ui();
    LIST_FAIL.with(|f| f.set(true));
    start(&ui);
    settle();
    assert_kept(&ui);
    assert_eq!(ui.toast_overlay.count.get(), 1)
}
#[test]
fn manual_navigation_survives_source_focus_change_and_follow_revocation() {
    let ui = ui();
    ui.navigate_file_tree_point(
        FileTreeNavigationPoint {
            location: FsLocation::Remote(0),
            overlay: FsExecutionOverlay::default(),
            root: PathBuf::from("/manual/chosen"),
        },
        FileTreeNavigationAction::Push,
    );
    focus_changed(&ui);
    ui.invalidate_file_tree_remote_follow();
    settle();
    assert_eq!(*ui.file_tree_location.borrow(), FsLocation::Remote(0));
    assert_eq!(*ui.file_tree_root.borrow(), PathBuf::from("/manual/chosen"));
    assert_eq!(ui.file_tree_model.resets.get(), 1)
}
#[test]
fn same_target_rebind_preserves_rows_without_listing() {
    let ui = same_target_ui();
    start(&ui);
    settle();
    assert_eq!(HOME_CALLS.with(Cell::get), 1);
    assert_eq!(LIST_CALLS.with(Cell::get), 0);
    assert_eq!(*ui.file_tree_root.borrow(), PathBuf::from("/remote/kept"));
    assert_eq!(
        *ui.file_tree_model.rows.borrow(),
        vec![FileEntry("kept child")]
    );
    assert_eq!(ui.file_tree_model.resets.get(), 0);
    assert_eq!(ui.file_tree_model.rebinds.get(), 1);
    assert_eq!(
        ui.file_tree_execution_overlay
            .borrow()
            .control_path
            .as_deref(),
        Some(Path::new("/run/live.sock"))
    );
    assert!(ui.file_tree_navigation.borrow().back.is_empty())
}
#[test]
fn same_target_stale_source_cannot_rebind_overlay() {
    let ui = same_target_ui();
    let source = ui.clone();
    on_home(move || focus_changed(&source));
    start(&ui);
    settle();
    assert_eq!(HOME_CALLS.with(Cell::get), 1);
    assert_eq!(LIST_CALLS.with(Cell::get), 0);
    assert_eq!(ui.file_tree_model.rebinds.get(), 0);
    assert_eq!(
        ui.file_tree_execution_overlay
            .borrow()
            .control_path
            .as_deref(),
        Some(Path::new("/run/old.sock"))
    );
    assert_eq!(
        *ui.file_tree_model.rows.borrow(),
        vec![FileEntry("kept child")]
    );
    assert_eq!(ui.file_tree_model.resets.get(), 0)
}

fn navigation_point(path: &str) -> FileTreeNavigationPoint {
    FileTreeNavigationPoint {
        location: FsLocation::Local,
        overlay: FsExecutionOverlay::default(),
        root: PathBuf::from(path),
    }
}

// @files-follow:state-test-0

// @files-follow:state-test-1

// @files-follow:state-test-2

#[test]
fn rejected_worker_admission_retires_probe() {
    let ui = ui();
    ADMISSION_FAIL.with(|f| f.set(true));
    start(&ui);
    settle();
    assert!(ui.file_tree_navigation.borrow().automatic_probe.is_none());
    assert_kept(&ui);
    assert_eq!(ui.toast_overlay.count.get(), 1)
}
#[test]
fn late_old_callback_cannot_retire_new_automatic_probe() {
    let ui = ui();
    start(&ui);
    assert!(run_one(&WORKERS));
    start(&ui);
    let latest = ui
        .file_tree_navigation
        .borrow()
        .automatic_probe
        .as_ref()
        .unwrap()
        .1
        .clone();
    assert!(run_one(&COMPLETIONS));
    assert!(!latest.is_cancelled());
    ui.invalidate_file_tree_remote_follow();
    assert!(latest.is_cancelled());
    settle();
    assert_kept(&ui)
}
