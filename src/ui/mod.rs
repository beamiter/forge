use gtk4::glib::prelude::ObjectExt;
use gtk4::Notebook;
use gtk4::{CssProvider, ScrolledWindow, SearchBar, SearchEntry, Stack, ToggleButton};
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use vte4::Terminal;

use crate::config::{Config, SidebarView, TabPlacement, Theme};
use crate::keybindings::KeybindingMap;

mod actions;
mod agent_panel;
mod ai_chat_store;
mod ai_panel;
mod bottom_bar;
mod bounded_text;
mod command_correction;
mod command_palette;
mod command_review;
mod config_apply;
mod dialogs;
mod file_tree;
pub(crate) mod history_notice;
mod jsh;
mod layout;
mod notebooks;
mod organism;
mod pane_dnd;
mod pane_header;
mod pane_leaf;
mod pane_node;
mod pane_tree_edit;
mod panes;
mod remote_fs;
mod search;
mod session;
mod sidebar_tabs;
mod tab_strip;
mod tabs;
mod task_ops;
mod tasks_panel;
mod zoom;

pub(crate) use agent_panel::{AgentHandle, AgentUiLifetime};
pub(crate) use ai_panel::AiPanel;
pub(crate) use bottom_bar::build_bottom_bar;
pub(crate) use command_palette::CommandSuggestionHandle;
pub(crate) use config_apply::ConfigDirtyEpoch;
pub(crate) use file_tree::{
    build_file_tree_location_selector, build_file_tree_widgets, FileTreeModel,
    FileTreeNavigationState,
};
pub(crate) use organism::{
    pane_token, OrganismActivity, OrganismAgentSignal, OrganismCorrectionSignal, OrganismPresence,
};
pub(crate) use pane_header::{PaneHeader, PANE_HEADER_CSS};
pub(crate) use pane_leaf::PaneLeaf;
pub(crate) use pane_node::PaneNode;
pub(crate) use pane_tree_edit::{
    detach_leaf_and_promote, detach_leaf_for_zoom, plan_existing_leaf_split, restore_zoomed_leaf,
    ZoomPageSwap,
};
pub(crate) use remote_fs::{FsClipboard, FsExecutionOverlay, FsLocation};
pub(crate) use task_ops::AgentTaskDomain;
pub(crate) use tasks_panel::TasksPanel;

/// Quiet period after the last font-scale step before the config is written.
pub(crate) const FONT_PERSIST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
/// Quiet period after the last generic settings mutation before the snapshot
/// is queued for background persistence.
pub(crate) const CONFIG_PERSIST_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(250);
pub(crate) const CONFIG_PERSIST_OPERATION: &str = "Save settings";

/// Exact process observation already handled by the Files follower. The focus
/// epoch is part of deduplication: A -> B -> A must stage a fresh probe even
/// when A's session/argv never changed, while Files/chrome intent changes keep
/// the same observation consumed and therefore cannot cause an automatic
/// retry after deliberately cancelling a probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileTreeRemoteObservation {
    pub(crate) source_session: String,
    pub(crate) argv: Vec<String>,
    pub(crate) tab_focus_generation: u64,
    pub(crate) source_focus_serial: u64,
}

impl FileTreeRemoteObservation {
    pub(crate) fn matches(
        &self,
        source_session: &str,
        argv: &[String],
        tab_focus_generation: u64,
        source_focus_serial: u64,
    ) -> bool {
        self.source_session == source_session
            && self.argv == argv
            && self.tab_focus_generation == tab_focus_generation
            && self.source_focus_serial == source_focus_serial
    }
}

/// One configured `block:search` key press can toggle the picker at most once.
///
/// `AdwDialog` is a widget overlay inside the application window, so its key
/// events normally traverse the window's capture controller first. Keep this
/// state on [`UiState`] rather than on either controller: the opener press is
/// then visible to the dialog controller as a defensive fallback, and the
/// matching physical release can clear it no matter which surface owns focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CrossBlockSearchToggleRoute {
    Open,
    Close,
    SuppressRepeat,
    Proceed,
}

#[derive(Debug, Default)]
pub(crate) struct CrossBlockSearchToggleLatch {
    held_keycodes: HashSet<u32>,
}

impl CrossBlockSearchToggleLatch {
    pub(crate) fn press(
        &mut self,
        keycode: u32,
        is_toggle: bool,
        dialog_open: bool,
    ) -> CrossBlockSearchToggleRoute {
        if !self.held_keycodes.insert(keycode) {
            CrossBlockSearchToggleRoute::SuppressRepeat
        } else if !is_toggle {
            // Ordinary keys must keep their normal repeat behavior. Only a
            // physical key that first matched the toggle belongs in the latch.
            self.held_keycodes.remove(&keycode);
            CrossBlockSearchToggleRoute::Proceed
        } else if dialog_open {
            CrossBlockSearchToggleRoute::Close
        } else {
            CrossBlockSearchToggleRoute::Open
        }
    }

    pub(crate) fn release(&mut self, keycode: u32) {
        self.held_keycodes.remove(&keycode);
    }

    pub(crate) fn reset(&mut self) {
        self.held_keycodes.clear();
    }
}

pub(crate) struct ZoomState {
    pub(crate) swap: ZoomPageSwap,
    pub(crate) zoomed_terminal: Terminal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnStatus {
    Connecting,
    Connected,
    Disconnected,
}

/// Per-tab record for a remote (ssh) connection, enabling status display and
/// auto-reconnect. Keyed by tab_num in `UiState::tab_connections`.
#[derive(Clone)]
pub(crate) struct TabConnection {
    /// Stable identity for this concrete connection attempt. The map key may
    /// change when its pane moves to another tab while a reconnect timer is
    /// pending, but this value moves with the record.
    pub(crate) identity: u32,
    /// The host this tab connects to — used to rebuild the same argv (and thus
    /// the same remote `--session` id) on reconnect.
    pub(crate) host: crate::config::RemoteHost,
    /// Workspace restore deliberately replaces the configured profile session
    /// with the saved tab session. Only those connections may ignore `session`
    /// while matching the live tab back to a current filesystem profile.
    pub(crate) profile_session_overridden: bool,
    /// `Some` identifies a temporary plain-interactive SSH launch and freezes
    /// its execution-only socket overlay for reconnect. Saved managed remotes
    /// remain `None` and rebuild through their configured jsh launcher.
    pub(crate) plain_ssh_overlay: Option<remote_fs::FsExecutionOverlay>,
    pub(crate) status: ConnStatus,
    /// Reconnect backoff counter; a session that stayed up long enough resets it.
    pub(crate) attempt: u32,
    /// When this connection attempt was spawned — used to distinguish a brief
    /// failed handshake (grow backoff) from a long-lived session that dropped
    /// (reset backoff).
    pub(crate) spawn_at: std::time::Instant,
}

/// GTK object-data key shared by tab construction, split-page replacement and
/// window snapshotting.  The `Rc<Cell<_>>` is also captured by rename/title
/// callbacks, so replacement page widgets must carry the same cell rather than
/// a copied boolean.
pub(crate) const CUSTOM_TITLE_DATA: &str = "forge-custom-title";
pub(crate) const PRIVATE_TITLE_DATA: &str = "forge-private-title";

pub(crate) fn tab_custom_title_cell(widget: &gtk4::Widget) -> Option<Rc<Cell<bool>>> {
    unsafe {
        widget
            .data::<Rc<Cell<bool>>>(CUSTOM_TITLE_DATA)
            .map(|value| value.as_ref().clone())
    }
}

pub(crate) fn attach_tab_custom_title_cell(widget: &gtk4::Widget, value: Rc<Cell<bool>>) {
    unsafe {
        widget.set_data::<Rc<Cell<bool>>>(CUSTOM_TITLE_DATA, value);
    }
}

pub(crate) fn set_tab_custom_title(widget: &gtk4::Widget, value: bool) {
    if let Some(cell) = tab_custom_title_cell(widget) {
        cell.set(value);
    } else {
        attach_tab_custom_title_cell(widget, Rc::new(Cell::new(value)));
    }
}

pub(crate) fn tab_private_title_cell(widget: &gtk4::Widget) -> Option<Rc<Cell<bool>>> {
    unsafe {
        widget
            .data::<Rc<Cell<bool>>>(PRIVATE_TITLE_DATA)
            .map(|value| value.as_ref().clone())
    }
}

pub(crate) fn attach_tab_private_title_cell(widget: &gtk4::Widget, value: Rc<Cell<bool>>) {
    unsafe {
        widget.set_data::<Rc<Cell<bool>>>(PRIVATE_TITLE_DATA, value);
    }
}

pub(crate) fn set_tab_private_title(widget: &gtk4::Widget, value: bool) {
    if let Some(cell) = tab_private_title_cell(widget) {
        cell.set(value);
    } else {
        attach_tab_private_title_cell(widget, Rc::new(Cell::new(value)));
    }
}

pub(crate) fn tab_display_title(
    notebook: &gtk4::Notebook,
    widget: &gtk4::Widget,
) -> Option<String> {
    if tab_private_title_cell(widget).is_some_and(|flag| flag.get()) {
        Some("Private".to_string())
    } else {
        crate::state::tab_label_text(notebook, widget)
    }
}

#[derive(Clone)]
pub(crate) struct UiState {
    pub(crate) window: adw::ApplicationWindow,
    /// Window-level toast host wrapping the main layout.
    pub(crate) toast_overlay: adw::ToastOverlay,
    /// The live opacity-hotkey toast, if one is currently shown. Held so rapid
    /// Ctrl+Alt+=/- presses update one toast in place instead of queueing a
    /// separate toast per step.
    pub(crate) opacity_toast: Rc<RefCell<Option<adw::Toast>>>,
    pub(crate) notebook: Notebook,
    pub(crate) tab_counter: Rc<Cell<u32>>,
    pub(crate) font_scale: Rc<Cell<f64>>,
    /// Generation token for generic debounced config writes. Sliders, paned
    /// drags and bursty hotkeys should enqueue only their final settled state.
    pub(crate) config_persist_generation: Rc<Cell<u64>>,
    /// Generation token for the debounced font-scale config write. Ctrl+wheel
    /// emits a step per notch, so only the last step in a burst reaches disk.
    pub(crate) font_persist_generation: Rc<Cell<u64>>,
    /// Font scale a coalesced sweep has yet to apply to the widget tree. One
    /// wheel gesture delivers a burst of 0.025 notches; applying every one of
    /// them re-measures every VTE in every pane for a scale the user passed
    /// through in a few milliseconds. `Some` means a sweep is already queued
    /// and only needs its target updated.
    pub(crate) pending_font_scale: Rc<Cell<Option<f64>>>,
    pub(crate) window_opacity: Rc<Cell<f64>>,
    pub(crate) shell_argv: Rc<RefCell<Vec<String>>>,
    pub(crate) config: Rc<RefCell<Config>>,
    pub(crate) available_themes: Rc<Vec<Theme>>,
    pub(crate) search_bar: SearchBar,
    pub(crate) search_entry: SearchEntry,
    pub(crate) search_status: gtk4::Label,
    pub(crate) search_debounce_source: Rc<RefCell<Option<gtk4::glib::SourceId>>>,
    pub(crate) search_generation: Rc<Cell<u64>>,
    pub(crate) tab_strip: gtk4::Box,
    pub(crate) sidebar: gtk4::Box,
    /// Sidebar scroll holder for the (vertical) tab strip.
    pub(crate) tab_strip_scroll: ScrolledWindow,
    /// Sidebar list mirroring the strip while the strip lives in the top bar,
    /// so the Tabs view stays usable in both placements. See `sidebar_tabs`.
    pub(crate) sidebar_tab_mirror: gtk4::Box,
    pub(crate) sidebar_tab_mirror_scroll: ScrolledWindow,
    /// Top-bar scroll holder for the (horizontal) tab strip.
    pub(crate) top_tab_scroll: ScrolledWindow,
    /// Window-global bottom status bar (the `jterm_core::bottom_bar`
    /// contract), spanning sidebar and content at the very bottom.
    pub(crate) bottom_bar: gtk4::Box,
    /// Left-packed segment container inside the bar.
    pub(crate) bottom_bar_left: gtk4::Box,
    /// Right-aligned segment container inside the bar.
    pub(crate) bottom_bar_right: gtk4::Box,
    /// Last composed bar content, so the 1s poll skips repaints that would
    /// change nothing.
    pub(crate) bottom_bar_content: Rc<RefCell<jterm_core::bottom_bar::Content>>,
    /// Current tab placement (sidebar vs top bar).
    pub(crate) tab_placement: Rc<Cell<TabPlacement>>,
    /// Sidebar content stack (one of: tab list, file tree).
    pub(crate) sidebar_stack: Stack,
    pub(crate) sidebar_tabs_btn: ToggleButton,
    pub(crate) sidebar_files_btn: ToggleButton,
    /// Which sidebar view the user prefers (persisted).
    pub(crate) sidebar_view: Rc<Cell<SidebarView>>,
    pub(crate) file_tree_model: FileTreeModel,
    pub(crate) file_tree_root: Rc<RefCell<PathBuf>>,
    pub(crate) file_tree_root_label: gtk4::Label,
    pub(crate) file_tree_navigation: Rc<RefCell<FileTreeNavigationState>>,
    /// Which filesystem the file tree browses (local disk or one of the
    /// configured ssh/docker remote hosts).
    pub(crate) file_tree_location: Rc<RefCell<FsLocation>>,
    /// Execution-only material for the visible filesystem endpoint. Stable
    /// location identity never includes this overlay; every scan/operation
    /// takes an immutable clone alongside its location/host snapshot.
    pub(crate) file_tree_execution_overlay: Rc<RefCell<remote_fs::FsExecutionOverlay>>,
    /// Location selector in the file-tree header; rebuilt when the hosts list
    /// or the current location changes.
    pub(crate) file_tree_location_selector: gtk4::DropDown,
    /// Suppress intermediate `selected` notifications while replacing the
    /// dropdown model; those are not user requests to switch filesystems.
    pub(crate) file_tree_location_selector_syncing: Rc<Cell<bool>>,
    /// Type-to-filter row under the file-tree header (hidden until toggled).
    pub(crate) file_tree_filter_bar: gtk4::Box,
    pub(crate) file_tree_filter_entry: gtk4::Entry,
    /// Header toggle opening/closing the filter row; active while it is open.
    pub(crate) file_tree_filter_toggle: gtk4::ToggleButton,
    /// Sidebar cut/copy payload for file operations; paste is offered only
    /// while the clipboard location matches the tree location.
    pub(crate) file_tree_clipboard: Rc<RefCell<Option<FsClipboard>>>,
    /// Allocates a distinct identity for every user Copy/Cut action so an old
    /// async paste cannot consume a newer, even byte-identical, payload.
    pub(crate) file_tree_clipboard_intent: Rc<Cell<u64>>,
    /// Monotonic user file-operation intent and a live-operation count. A
    /// staged SSH home probe may not replace the tree across either boundary.
    /// `None` permanently disables auto-follow after theoretical exhaustion
    /// without disabling file operations themselves.
    pub(crate) file_tree_operation_intent: Rc<Cell<Option<u64>>>,
    pub(crate) file_tree_active_operations: Rc<Cell<u32>>,
    /// Monotonic identity of the latest foreground-SSH follow attempt. A slow
    /// home probe may publish only while this token, the source pane, and the
    /// user's file-tree navigation generation all still match.
    pub(crate) file_tree_remote_follow_intent: Rc<Cell<u64>>,
    /// Last real foreground argv observed for one pane session. Keeping argv
    /// boundaries (rather than command text) both deduplicates the 1s window
    /// heartbeat and lets the same SSH command trigger again after it exits.
    pub(crate) file_tree_remote_follow_observed: Rc<RefCell<Option<FileTreeRemoteObservation>>>,
    pub(crate) tab_search_entry: SearchEntry,
    pub(crate) selected_tabs: Rc<RefCell<Vec<String>>>,
    /// Global identity/generation for one native tab drag. Delayed hover work
    /// must be bound to this state rather than to a target button alone.
    pub(crate) tab_drag_state: Rc<RefCell<pane_dnd::TabDragState>>,
    /// Invalidates frame-clock focus requests when a layout mutation keeps the
    /// same Notebook page selected but changes which live pane should own focus.
    pub(crate) tab_focus_generation: Rc<Cell<u64>>,
    pub(crate) command_palette_dialog: Rc<RefCell<Option<adw::Dialog>>>,
    pub(crate) remote_picker_dialog: Rc<RefCell<Option<adw::Dialog>>>,
    pub(crate) history_palette_dialog: Rc<RefCell<Option<adw::Dialog>>>,
    pub(crate) cross_block_search_dialog: Rc<RefCell<Option<adw::Dialog>>>,
    /// Window/dialog-shared physical-key latch for the configurable picker
    /// toggle. It deliberately survives dialog close until key release so a
    /// repeat cannot reopen a picker that the fresh press just closed.
    pub(crate) cross_block_search_toggle_latch: Rc<RefCell<CrossBlockSearchToggleLatch>>,
    /// Window-lifetime, memory-only search intent; never persisted in config
    /// or workspace snapshots.
    pub(crate) cross_block_search_memory: Rc<RefCell<dialogs::CrossBlockSearchMemory>>,
    pub(crate) workflows_palette_dialog: Rc<RefCell<Option<adw::Dialog>>>,
    pub(crate) settings_dialog: Rc<RefCell<Option<adw::PreferencesDialog>>>,
    pub(crate) debug_dashboard_dialog: Rc<RefCell<Option<adw::Dialog>>>,
    /// The single active Shell Agent session, rendered as an inline card in
    /// its bound Block pane's conversation (not a dialog).
    pub(crate) agent_session: Rc<RefCell<Option<AgentHandle>>>,
    /// Window-lifetime bounded Agent activity and one-shot TermView bridges.
    /// This deliberately outlives individual Agent sessions and fresh tasks.
    pub(crate) agent_ui_lifetime: Rc<AgentUiLifetime>,
    /// The single in-flight or reviewable natural-language command suggestion.
    /// Like the Shell Agent, it is pinned to the Block pane where it started.
    pub(crate) command_suggestion: Rc<RefCell<Option<CommandSuggestionHandle>>>,
    /// Window-shared, repo/day-scoped memory for every native organism body.
    /// `None` means persistence failed closed; the visual reducer remains
    /// usable but must not overwrite the unreadable state file.
    pub(crate) organism_memory: Rc<RefCell<Option<jterm_core::organism_memory::OrganismMemory>>>,
    /// Continuous state is shared across pane-local behavior reducers, so the
    /// last pane to finish cannot overwrite another pane's earlier changes.
    pub(crate) organism_life: Rc<Cell<jterm_core::organism::LifeState>>,
    /// Content-free accept/dismiss pulses from the command-correction card,
    /// shared window-wide like the life state they feed.
    pub(crate) organism_correction: Rc<OrganismCorrectionSignal>,
    /// Window-shared activity aggregate and tick clock for the continuous
    /// life simulation: one mind, one clock, however many pane bodies.
    pub(crate) organism_activity: Rc<OrganismActivity>,
    /// Focus-backed visibility arbiter: pane-local inline/sticky forms remain,
    /// but at most one local Block pane owns the spatial live body.
    pub(crate) organism_presence: Rc<OrganismPresence>,
    /// Content-free Shell Agent lifecycle phases feeding the shared mind.
    pub(crate) organism_agent: Rc<OrganismAgentSignal>,
    /// Visible top-bar control reflecting whether a Shell Agent session is
    /// currently active.
    pub(crate) agent_toggle: ToggleButton,
    /// Suppresses a storm of identical persistence alerts while a continuous
    /// setting (opacity/font size) emits multiple change notifications.
    pub(crate) config_save_error_visible: Rc<Cell<bool>>,
    /// Deduplicates safe-mode informational toasts without suppressing a real
    /// persistence or reload error that happens while the toast is visible.
    pub(crate) safe_mode_config_notice_visible: Rc<Cell<bool>>,
    /// One save/reload conflict dialog at a time. A single external write can
    /// deliver several monitor events, and each of them would otherwise stack
    /// another modal on top of the answer the user is already reading.
    pub(crate) config_reload_conflict_visible: Rc<Cell<bool>>,
    /// Persistent Block-history failure bar and its reason label. Held here
    /// rather than built and forgotten, because the persistence poll raises it
    /// long after the window was constructed.
    pub(crate) block_history_notice: gtk4::Box,
    pub(crate) block_history_notice_label: gtk4::Label,
    /// Whether the live `Config` holds settings changes the file has not seen.
    /// Consulted before a reload replaces it, so an edit still inside its
    /// persist debounce is never discarded without the user being asked.
    pub(crate) config_dirty: ConfigDirtyEpoch,
    pub(crate) keybinding_map: Rc<RefCell<KeybindingMap>>,
    pub(crate) zoom_state: Rc<RefCell<Option<ZoomState>>>,
    pub(crate) scrollbar_css: CssProvider,
    /// Maps tab_num → session_id for jsh session persistence.
    pub(crate) session_ids: Rc<RefCell<HashMap<u32, String>>>,
    /// Maps tab_num → remote connection record (status + reconnect info).
    pub(crate) tab_connections: Rc<RefCell<HashMap<u32, TabConnection>>>,
    /// Right-side AI chat panel. Always built; visibility lives in the
    /// outer `ai_paned` (and `config.ai_panel_visible` for persistence).
    pub(crate) ai_panel: AiPanel,
    /// Right-side stack holding the AI Chats panel and the agent Tasks panel;
    /// it is the `ai_paned` end child while either panel is open, so both
    /// share the persisted width.
    pub(crate) side_stack: Stack,
    /// Native Codex agent Tasks panel. Always built like the AI panel; the
    /// opt-in `agent_tasks_enabled` config flag gates its actions instead.
    pub(crate) tasks_panel: TasksPanel,
    /// Window-owned native agent task domain (task reducer, app-server
    /// runtime, diff worker, panel preference).
    pub(crate) agent_tasks: Rc<RefCell<AgentTaskDomain>>,
    /// Horizontal Paned that puts the AI panel to the right of the notebook
    /// area. Toggling visibility flips the end child + resize start_child.
    pub(crate) ai_paned: gtk4::Paned,
    pub(crate) ai_panel_visible: Rc<Cell<bool>>,
    /// Suppresses divider notifications caused by restoring a configured
    /// width; only user-driven positions should flow back into Config.
    pub(crate) ai_panel_width_restoring: Rc<Cell<bool>>,
}

#[cfg(test)]
mod tests {
    use super::{CrossBlockSearchToggleLatch, CrossBlockSearchToggleRoute};

    #[test]
    fn cross_block_search_toggle_routes_once_per_physical_press() {
        use CrossBlockSearchToggleRoute::{Close, Open, Proceed, SuppressRepeat};

        let mut latch = CrossBlockSearchToggleLatch::default();
        assert_eq!(latch.press(42, true, false), Open);
        assert_eq!(
            latch.press(42, true, true),
            SuppressRepeat,
            "the opener's first repeat must not close the new dialog"
        );
        assert_eq!(
            latch.press(42, false, true),
            SuppressRepeat,
            "dropping the toggle modifiers must not leak a repeated character"
        );

        latch.release(42);
        assert_eq!(latch.press(42, true, true), Close);
        assert_eq!(
            latch.press(42, true, false),
            SuppressRepeat,
            "a repeat after close must not reopen the dialog"
        );

        latch.release(42);
        assert_eq!(
            latch.press(42, true, true),
            Close,
            "until closed releases the slot, a fresh press must close the claimed dialog again"
        );
        latch.release(42);
        assert_eq!(latch.press(42, false, false), Proceed);
        assert_eq!(
            latch.press(42, false, false),
            Proceed,
            "fresh non-toggle repeats retain ordinary input semantics"
        );
        assert_eq!(latch.press(42, true, false), Open);
    }

    #[test]
    fn cross_block_search_toggle_tracks_physical_keycodes_and_resets_on_deactivate() {
        use CrossBlockSearchToggleRoute::{Close, Open, SuppressRepeat};

        let mut latch = CrossBlockSearchToggleLatch::default();
        assert_eq!(latch.press(7, true, false), Open);
        assert_eq!(latch.press(8, true, true), Close);
        assert_eq!(latch.press(7, true, true), SuppressRepeat);
        latch.release(8);
        assert_eq!(latch.press(8, true, false), Open);

        latch.reset();
        assert_eq!(latch.press(7, true, true), Close);
    }
}
