use gtk4::glib::prelude::ObjectExt;
use gtk4::Notebook;
use gtk4::{CssProvider, ScrolledWindow, SearchBar, SearchEntry, Stack, ToggleButton};
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
mod search;
mod session;
mod sidebar_tabs;
mod tab_strip;
mod tabs;
mod zoom;

pub(crate) use agent_panel::{AgentHandle, AgentUiLifetime};
pub(crate) use ai_panel::AiPanel;
pub(crate) use bottom_bar::build_bottom_bar;
pub(crate) use command_palette::CommandSuggestionHandle;
pub(crate) use file_tree::{build_file_tree_widgets, FileTreeModel};
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

/// Quiet period after the last font-scale step before the config is written.
pub(crate) const FONT_PERSIST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
/// Quiet period after the last generic settings mutation before the snapshot
/// is queued for background persistence.
pub(crate) const CONFIG_PERSIST_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(250);
pub(crate) const CONFIG_PERSIST_OPERATION: &str = "Save settings";

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
    pub(crate) organism_memory: Rc<RefCell<Option<crate::organism_memory::OrganismMemory>>>,
    /// Continuous state is shared across pane-local behavior reducers, so the
    /// last pane to finish cannot overwrite another pane's earlier changes.
    pub(crate) organism_life: Rc<Cell<crate::organism::LifeState>>,
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
    /// Horizontal Paned that puts the AI panel to the right of the notebook
    /// area. Toggling visibility flips the end child + resize start_child.
    pub(crate) ai_paned: gtk4::Paned,
    pub(crate) ai_panel_visible: Rc<Cell<bool>>,
    /// Suppresses divider notifications caused by restoring a configured
    /// width; only user-driven positions should flow back into Config.
    pub(crate) ai_panel_width_restoring: Rc<Cell<bool>>,
}
