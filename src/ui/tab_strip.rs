//! tab_strip — UiState methods extracted from ui (mechanical split, no logic changes)
use adw::prelude::*;
use gtk4::glib;
use gtk4::ToggleButton;
use libadwaita as adw;

use super::pane_dnd::{tab_payload_can_split, PaneDragPayload, TabDragPayload};
use super::tabs::notebook_page_named;
use super::*;

const TAB_WIDTH_HANDLE_DATA: &str = "tab-width-resize-handle";

fn tab_width_after_drag(start_width: u32, offset_x: f64) -> u32 {
    let delta = if offset_x.is_finite() {
        offset_x.round() as i64
    } else {
        0
    };
    (i64::from(start_width) + delta).clamp(
        i64::from(crate::config::MIN_TAB_WIDTH),
        i64::from(crate::config::MAX_TAB_WIDTH),
    ) as u32
}

fn command_finish_needs_failure_attention(exit_code: Option<i32>) -> bool {
    exit_code.is_some_and(|code| code != 0)
}

/// Translate a before/after drop on a target in the original ordering into
/// the destination index expected after removing the source item.
fn dropped_tab_index(source: u32, target: u32, after: bool) -> u32 {
    match (source < target, after) {
        (true, false) => target.saturating_sub(1),
        (true, true) => target,
        (false, false) => target,
        (false, true) => target.saturating_add(1),
    }
}

/// Clamp a native reorder to the source tab's pinned/unpinned partition.
/// `pinned_count` includes the source before it is removed.
fn dropped_tab_index_in_pinned_partition(
    source: u32,
    target: u32,
    after: bool,
    source_pinned: bool,
    pinned_count: u32,
    tab_count: u32,
) -> u32 {
    let requested = dropped_tab_index(source, target, after);
    let last = tab_count.saturating_sub(1);
    if source_pinned {
        requested.min(pinned_count.saturating_sub(1)).min(last)
    } else {
        requested.max(pinned_count.min(last)).min(last)
    }
}

fn clear_tab_drop_classes(widget: &gtk4::Widget) {
    widget.remove_css_class("tab-drop-target");
    widget.remove_css_class("tab-drop-before");
    widget.remove_css_class("tab-drop-after");
}

fn tab_strip_has_active_drag(tab_strip: &gtk4::Box) -> bool {
    let mut child = tab_strip.first_child();
    while let Some(widget) = child {
        if widget.has_css_class("tab-dragging") {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}

fn tab_hover_target_can_activate(
    target_is_mapped: bool,
    has_drop_feedback: bool,
    has_active_drag: bool,
) -> bool {
    target_is_mapped && has_drop_feedback && has_active_drag
}

fn tab_drag_payload(ui: &UiState, button: &ToggleButton) -> TabDragPayload {
    let tab_name = button.widget_name().to_string();
    let pane_session_id = notebook_page_named(&ui.notebook, &tab_name)
        .and_then(|page| {
            let node = PaneNode::from_widget(&page)?;
            (!node.is_split()).then(|| node.active_leaf()).flatten()
        })
        .and_then(|leaf| leaf.session_id());
    TabDragPayload {
        tab_name,
        pane_session_id,
    }
}

fn tab_drag_drop_target_preload() -> bool {
    true
}

fn tab_drag_drop_target() -> gtk4::DropTarget {
    let target = gtk4::DropTarget::new(TabDragPayload::static_type(), gtk4::gdk::DragAction::MOVE);
    // Hover preview reads the typed value from `connect_motion`. GtkDropTarget
    // does not make that value available before release unless preloading is
    // enabled (the default is false); the payload is process-local and tiny.
    target.set_preload(tab_drag_drop_target_preload());
    target
}

fn install_pane_to_tab_drop_target(widget: &gtk4::Widget, ui: UiState) {
    let target = gtk4::DropTarget::new(PaneDragPayload::static_type(), gtk4::gdk::DragAction::MOVE);

    let highlighted = widget.downgrade();
    target.connect_motion(move |_, _, _| {
        if let Some(widget) = highlighted.upgrade() {
            widget.add_css_class("pane-to-tab-drop-target");
        }
        gtk4::gdk::DragAction::MOVE
    });

    let highlighted = widget.downgrade();
    target.connect_leave(move |_| {
        if let Some(widget) = highlighted.upgrade() {
            widget.remove_css_class("pane-to-tab-drop-target");
        }
    });

    let highlighted = widget.downgrade();
    target.connect_drop(move |_, value, _, _| {
        if let Some(widget) = highlighted.upgrade() {
            widget.remove_css_class("pane-to-tab-drop-target");
        }
        let Ok(payload) = value.get::<PaneDragPayload>() else {
            return false;
        };
        ui.move_pane_to_new_tab_by_session(&payload.0)
    });
    widget.add_controller(target);
}

fn tab_title_matches(query: &str, title: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || title.to_lowercase().contains(&query)
}

fn resolved_tab_pinned(page: bool, strip_button: bool, leaves: &[bool]) -> bool {
    page || strip_button || leaves.iter().copied().any(std::convert::identity)
}

fn widget_pinned(widget: &gtk4::Widget) -> bool {
    unsafe {
        widget
            .data::<bool>("pinned")
            .is_some_and(|value| *value.as_ref())
    }
}

impl UiState {
    /// Route authoritative Block completion to the existing inactive-tab
    /// attention styles. A reported non-zero status is bell-strength; success
    /// and unreported status are ordinary activity, so unknown never masquerades
    /// as a failure or success.
    pub(crate) fn connect_block_tab_attention(
        &self,
        view: &Rc<crate::block_view::TermView>,
        root: &gtk4::Widget,
    ) {
        let ui = self.clone();
        let root = root.downgrade();
        view.connect_block_finished(move |_command, exit_code, _, _, _| {
            let Some(root) = root.upgrade() else {
                return;
            };
            if command_finish_needs_failure_attention(exit_code) {
                ui.mark_tab_bell(&root.widget_name());
            } else {
                ui.mark_tab_activity(&root.widget_name());
            }
        });
    }

    fn tab_width_handle(button: &ToggleButton) -> Option<gtk4::Box> {
        unsafe {
            button
                .data::<gtk4::Box>(TAB_WIDTH_HANDLE_DATA)
                .map(|handle| handle.as_ref().clone())
        }
    }

    /// Preview one width across the native horizontal strip while the pointer
    /// is moving. The config write is deferred until drag end so a smooth drag
    /// does not rewrite the file for every motion event.
    fn preview_tab_width(&self, width: u32) {
        let width = width.clamp(crate::config::MIN_TAB_WIDTH, crate::config::MAX_TAB_WIDTH) as i32;
        let mut child = self.tab_strip.first_child();
        while let Some(widget) = child {
            if let Ok(button) = widget.clone().downcast::<ToggleButton>() {
                if self.tab_placement.get() == crate::config::TabPlacement::TopBar {
                    button.set_width_request(width);
                }
            }
            child = widget.next_sibling();
        }
    }

    fn commit_tab_width(&self, width: u32) {
        let width = width.clamp(crate::config::MIN_TAB_WIDTH, crate::config::MAX_TAB_WIDTH);
        self.preview_tab_width(width);
        if self.config.borrow().tab_width == width {
            return;
        }
        self.config.borrow_mut().tab_width = width;
        self.persist_config();
    }

    pub(crate) fn set_tab_width_handle_visible(&self, button: &ToggleButton, visible: bool) {
        if let Some(handle) = Self::tab_width_handle(button) {
            handle.set_visible(visible);
        }
    }

    /// Add a narrow native GTK drag target at the trailing edge of a tab. It
    /// deliberately lives inside the button so it travels with tab reorders,
    /// while claiming its own gesture prevents a resize from becoming a tab
    /// drag or activation click.
    pub(crate) fn install_tab_width_resize(&self, button: &ToggleButton) {
        if Self::tab_width_handle(button).is_some() {
            return;
        }
        let Some(content) = button
            .child()
            .and_then(|child| child.downcast::<gtk4::Box>().ok())
        else {
            return;
        };

        let handle = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        handle.add_css_class("tab-resize-handle");
        handle.set_width_request(8);
        handle.set_vexpand(true);
        handle.set_cursor_from_name(Some("col-resize"));
        handle.set_tooltip_text(Some("Drag to resize tabs"));
        handle.update_property(&[gtk4::accessible::Property::Label("Resize tabs")]);
        content.append(&handle);
        unsafe {
            button.set_data::<gtk4::Box>(TAB_WIDTH_HANDLE_DATA, handle.clone());
        }

        let start_width = Rc::new(Cell::new(self.config.borrow().tab_width));
        let drag = gtk4::GestureDrag::new();
        drag.set_button(gtk4::gdk::BUTTON_PRIMARY);
        drag.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let config_for_begin = self.config.clone();
        let start_for_begin = start_width.clone();
        drag.connect_drag_begin(move |gesture, _, _| {
            start_for_begin.set(config_for_begin.borrow().tab_width);
            gesture.set_state(gtk4::EventSequenceState::Claimed);
        });
        let ui_for_update = self.clone();
        let start_for_update = start_width.clone();
        drag.connect_drag_update(move |gesture, offset_x, _| {
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            ui_for_update.preview_tab_width(tab_width_after_drag(start_for_update.get(), offset_x));
        });
        let ui_for_end = self.clone();
        drag.connect_drag_end(move |gesture, offset_x, _| {
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            ui_for_end.commit_tab_width(tab_width_after_drag(start_width.get(), offset_x));
        });
        handle.add_controller(drag);
    }

    fn begin_tab_drag(&self, payload: TabDragPayload) {
        self.clear_tab_drag_feedback();
        let origin_tab_name = self
            .notebook
            .current_page()
            .and_then(|index| self.notebook.nth_page(Some(index)))
            .map(|page| page.widget_name().to_string());
        let _started = self
            .tab_drag_state
            .borrow_mut()
            .begin(payload, origin_tab_name);
    }

    pub(crate) fn invalidate_tab_drag_hover(&self) {
        self.tab_drag_state.borrow_mut().invalidate();
    }

    fn end_tab_drag(&self) {
        let restore = self.tab_drag_state.borrow_mut().end();
        self.clear_tab_drag_feedback();
        if let Some(tab_name) =
            restore.filter(|name| notebook_page_named(&self.notebook, name).is_some())
        {
            self.activate_tab_named(&tab_name);
        }
    }

    pub(crate) fn commit_tab_split_drag(&self, dragged_session: &str) {
        let _committed = self
            .tab_drag_state
            .borrow_mut()
            .commit_topology(dragged_session);
    }

    fn clear_tab_drag_feedback(&self) {
        clear_tab_drop_classes(self.tab_strip.upcast_ref());
        let mut child = self.tab_strip.first_child();
        while let Some(widget) = child {
            widget.remove_css_class("tab-dragging");
            clear_tab_drop_classes(&widget);
            child = widget.next_sibling();
        }
    }

    fn tab_drag_payload_is_live(&self, payload: &TabDragPayload) -> bool {
        let Some(expected_session) = payload.pane_session_id.as_deref() else {
            return false;
        };
        notebook_page_named(&self.notebook, &payload.tab_name)
            .and_then(|page| {
                let node = PaneNode::from_widget(&page)?;
                let leaf = node.active_leaf()?;
                (!node.is_split() && leaf.root_widget() == page).then_some(leaf)
            })
            .and_then(|leaf| leaf.session_id())
            .as_deref()
            == Some(expected_session)
    }

    /// Update which tab strip button is :checked to match the active notebook page.
    pub(crate) fn sync_tab_strip_active(&self, active_page: Option<u32>) {
        let active = active_page.or(self.notebook.current_page()).unwrap_or(0);
        let mut idx = 0u32;
        let mut child = self.tab_strip.first_child();
        while let Some(c) = child {
            if let Ok(btn) = c.clone().downcast::<ToggleButton>() {
                btn.set_active(idx == active);
            }
            idx += 1;
            child = c.next_sibling();
        }
        self.refresh_sidebar_tab_mirror();
    }

    /// Show the tab strip wherever the placement says it lives — including a
    /// lone tab in the top bar, so the bar never changes shape as tabs come
    /// and go and the current tab's title stays on screen. The sidebar itself
    /// stays visible (it always offers the file tree); use Ctrl+\ to hide it.
    pub(crate) fn sync_tab_bar_visibility(&self) {
        use crate::config::TabPlacement;
        // The sidebar's Tabs page holds both the strip's holder and the
        // mirror; exactly one is shown, decided by where the strip lives, so
        // the page never renders an empty holder or a duplicate list. The
        // filter above them stays put in both cases.
        match self.tab_placement.get() {
            TabPlacement::Sidebar => {
                self.tab_strip_scroll.set_visible(true);
                self.sidebar_tab_mirror_scroll.set_visible(false);
                self.top_tab_scroll.set_visible(false);
            }
            TabPlacement::TopBar => {
                self.top_tab_scroll.set_visible(true);
                self.tab_strip_scroll.set_visible(false);
                self.sidebar_tab_mirror_scroll.set_visible(true);
            }
        }
        self.refresh_sidebar_tab_mirror();
    }

    pub(crate) fn apply_tab_filter(&self, query: &str) {
        let mut child = self.tab_strip.first_child();
        while let Some(widget) = child {
            if let Ok(button) = widget.clone().downcast::<ToggleButton>() {
                let title = unsafe {
                    button
                        .data::<gtk4::Label>("tab-title-label")
                        .map(|label| label.as_ref().text().to_string())
                }
                .unwrap_or_default();
                button.set_visible(tab_title_matches(query, &title));
            }
            child = widget.next_sibling();
        }
        self.refresh_sidebar_tab_mirror();
    }

    /// Keep a filtered tab's visibility correct when OSC title/cwd updates or
    /// rename operations change its title without changing the query.
    pub(crate) fn track_tab_title_for_filter(&self, button: &ToggleButton, title: &gtk4::Label) {
        let filter = self.tab_search_entry.clone();
        let button = button.clone();
        let ui = self.clone();
        title.connect_notify_local(Some("label"), move |label, _| {
            button.set_visible(tab_title_matches(
                filter.text().as_str(),
                label.text().as_str(),
            ));
            // The mirror binds to this label for its text, but a filtered
            // tab appearing or disappearing is a visibility change it has to
            // be told about.
            ui.refresh_sidebar_tab_mirror();
        });
    }

    /// Install native mouse drag/drop reordering on a tab strip button.
    ///
    /// The pointer half nearest the leading edge inserts before the target;
    /// the trailing half inserts after it. This works for both the vertical
    /// sidebar and horizontal top bar and is shared by ordinary tabs and tabs
    /// created by moving a split pane into a new tab.
    pub(crate) fn install_tab_drag_drop(&self, button: &ToggleButton) {
        let drag_source = gtk4::DragSource::new();
        drag_source.set_actions(gtk4::gdk::DragAction::MOVE);
        let ui_for_drag = self.clone();
        let button_for_drag = button.clone();
        drag_source.connect_prepare(move |_, _, _| {
            let payload = tab_drag_payload(&ui_for_drag, &button_for_drag);
            Some(gtk4::gdk::ContentProvider::for_value(&payload.to_value()))
        });

        let ui_for_drag_begin = self.clone();
        let button_for_drag_begin = button.clone();
        drag_source.connect_drag_begin(move |_, _| {
            ui_for_drag_begin
                .begin_tab_drag(tab_drag_payload(&ui_for_drag_begin, &button_for_drag_begin));
            button_for_drag_begin.add_css_class("tab-dragging");
        });

        let ui_for_drag_end = self.clone();
        let button_for_drag_end = button.clone();
        drag_source.connect_drag_end(move |_, _, _| {
            button_for_drag_end.remove_css_class("tab-dragging");
            ui_for_drag_end.end_tab_drag();
        });
        button.add_controller(drag_source);

        let drop_target = tab_drag_drop_target();
        let ui_for_drop = self.clone();
        let button_for_drop = button.clone();
        drop_target.connect_drop(move |_, value, x, y| {
            ui_for_drop.invalidate_tab_drag_hover();
            clear_tab_drop_classes(button_for_drop.upcast_ref());
            let Ok(payload) = value.get::<TabDragPayload>() else {
                return false;
            };
            let after = match ui_for_drop.tab_placement.get() {
                crate::config::TabPlacement::TopBar => {
                    x >= f64::from(button_for_drop.width()) / 2.0
                }
                crate::config::TabPlacement::Sidebar => {
                    y >= f64::from(button_for_drop.height()) / 2.0
                }
            };
            ui_for_drop.reorder_tab_from_drop(
                &payload.tab_name,
                button_for_drop.widget_name().as_str(),
                after,
            )
        });

        let placement_for_motion = self.tab_placement.clone();
        let button_for_motion = button.clone();
        let ui_for_hover = self.clone();
        drop_target.connect_motion(move |target, x, y| {
            let after = match placement_for_motion.get() {
                crate::config::TabPlacement::TopBar => {
                    x >= f64::from(button_for_motion.width()) / 2.0
                }
                crate::config::TabPlacement::Sidebar => {
                    y >= f64::from(button_for_motion.height()) / 2.0
                }
            };
            button_for_motion.add_css_class("tab-drop-target");
            if after {
                button_for_motion.remove_css_class("tab-drop-before");
                button_for_motion.add_css_class("tab-drop-after");
            } else {
                button_for_motion.remove_css_class("tab-drop-after");
                button_for_motion.add_css_class("tab-drop-before");
            }

            // A dragged active tab initially hides every possible content
            // target. Reveal a tab only after a deliberate hover; quick passes
            // used for ordinary before/after reordering never reach the timer.
            let payload = target
                .value()
                .and_then(|value| value.get::<TabDragPayload>().ok())
                .filter(tab_payload_can_split);
            let target_name = button_for_motion.widget_name().to_string();
            let Some(payload) = payload.filter(|payload| payload.tab_name != target_name) else {
                return gtk4::gdk::DragAction::MOVE;
            };
            let target_is_current = ui_for_hover
                .notebook
                .current_page()
                .and_then(|page| ui_for_hover.notebook.nth_page(Some(page)))
                .is_some_and(|page| page.widget_name().as_str() == target_name);
            if !target_is_current {
                let Some(drag_token) = ui_for_hover
                    .tab_drag_state
                    .borrow_mut()
                    .schedule_hover(&payload, &target_name)
                else {
                    return gtk4::gdk::DragAction::MOVE;
                };
                let button = button_for_motion.downgrade();
                let ui = ui_for_hover.clone();
                let payload = payload.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    if !ui.tab_drag_state.borrow().is_current(&drag_token)
                        || !ui.tab_drag_payload_is_live(&payload)
                    {
                        return;
                    }
                    let Some(button) = button.upgrade() else {
                        return;
                    };
                    if !tab_hover_target_can_activate(
                        button.is_mapped(),
                        button.has_css_class("tab-drop-target"),
                        tab_strip_has_active_drag(&ui.tab_strip),
                    ) {
                        return;
                    }
                    if !ui.tab_drag_state.borrow_mut().activate_hover(&drag_token) {
                        return;
                    }
                    ui.activate_tab_named(&target_name);
                });
            }
            gtk4::gdk::DragAction::MOVE
        });

        let button_for_leave = button.clone();
        let ui_for_leave = self.clone();
        drop_target.connect_leave(move |_| {
            ui_for_leave.invalidate_tab_drag_hover();
            clear_tab_drop_classes(button_for_leave.upcast_ref());
        });
        button.add_controller(drop_target);
        // A child controller makes drops on the visible label/button explicit;
        // the strip-level target below covers the surrounding blank tab bar.
        install_pane_to_tab_drop_target(button.upcast_ref(), self.clone());
    }

    /// Accept a split-pane header anywhere on the real tab strip.
    ///
    /// The strip moves between sidebar and top bar as one widget, so installing
    /// this controller once covers both placements and blank space around tab
    /// buttons. A cancelled, stale, direct-pane, or ambiguous payload is a
    /// structural no-op and reports an unsuccessful drop to GTK.
    pub(crate) fn install_tab_bar_pane_drop(&self) {
        install_pane_to_tab_drop_target(self.tab_strip.upcast_ref(), self.clone());
    }

    fn reorder_tab_from_drop(&self, source_name: &str, target_name: &str, after: bool) -> bool {
        if source_name == target_name {
            return false;
        }

        let Some(source_page) = notebook_page_named(&self.notebook, source_name) else {
            return false;
        };
        let Some(target_page) = notebook_page_named(&self.notebook, target_name) else {
            return false;
        };
        let Some(source_index) = self.notebook.page_num(&source_page) else {
            return false;
        };
        let Some(target_index) = self.notebook.page_num(&target_page) else {
            return false;
        };
        let source_pinned = self.tab_page_is_pinned(&source_page);
        let pinned_count = (0..self.notebook.n_pages())
            .filter_map(|index| self.notebook.nth_page(Some(index)))
            .filter(|page| self.tab_page_is_pinned(page))
            .count() as u32;
        let destination = dropped_tab_index_in_pinned_partition(
            source_index,
            target_index,
            after,
            source_pinned,
            pinned_count,
            self.notebook.n_pages(),
        );

        let active = self
            .notebook
            .current_page()
            .and_then(|page| self.notebook.nth_page(Some(page)));
        self.notebook.reorder_child(&source_page, Some(destination));
        self.reorder_tab_strip_buttons();

        let active_page = active
            .as_ref()
            .and_then(|widget| self.notebook.page_num(widget));
        self.notebook.set_current_page(active_page);
        self.sync_tab_strip_active(active_page);
        true
    }

    /// Remove the tab strip button that corresponds to a notebook page widget.
    pub(crate) fn remove_strip_button_for(&self, widget: &gtk4::Widget) {
        let name = widget.widget_name();
        let mut child = self.tab_strip.first_child();
        while let Some(c) = child {
            if c.widget_name() == name {
                self.tab_strip.remove(&c);
                return;
            }
            child = c.next_sibling();
        }
    }

    pub(crate) fn switch_tab(&self, direction: i32) {
        if let Some(page) = self.notebook.current_page() {
            let n = self.notebook.n_pages();
            if n == 0 {
                return;
            }
            let next = if direction > 0 {
                if page < n - 1 {
                    page + 1
                } else {
                    0
                }
            } else {
                if page > 0 {
                    page - 1
                } else {
                    n.saturating_sub(1)
                }
            };
            self.notebook.set_current_page(Some(next));
        }
    }

    pub(crate) fn clear_tab_selection(&self) {
        for tab_name in self.selected_tabs.borrow().iter() {
            if let Some(mut child) = self.tab_strip.first_child() {
                loop {
                    if child.widget_name().as_str() == tab_name {
                        if let Ok(btn) = child.clone().downcast::<ToggleButton>() {
                            btn.remove_css_class("tab-selected");
                        }
                        break;
                    }
                    match child.next_sibling() {
                        Some(next) => child = next,
                        None => break,
                    }
                }
            }
        }
        self.selected_tabs.borrow_mut().clear();
    }

    pub(crate) fn toggle_tab_selection(&self, tab_name: &str) {
        let mut selected = self.selected_tabs.borrow_mut();
        if let Some(pos) = selected.iter().position(|x| x == tab_name) {
            selected.remove(pos);
            // Remove CSS class
            if let Some(mut child) = self.tab_strip.first_child() {
                loop {
                    if child.widget_name().as_str() == tab_name {
                        if let Ok(btn) = child.clone().downcast::<ToggleButton>() {
                            btn.remove_css_class("tab-selected");
                        }
                        break;
                    }
                    match child.next_sibling() {
                        Some(next) => child = next,
                        None => break,
                    }
                }
            }
        } else {
            selected.push(tab_name.to_string());
            // Add CSS class
            if let Some(mut child) = self.tab_strip.first_child() {
                loop {
                    if child.widget_name().as_str() == tab_name {
                        if let Ok(btn) = child.clone().downcast::<ToggleButton>() {
                            btn.add_css_class("tab-selected");
                        }
                        break;
                    }
                    match child.next_sibling() {
                        Some(next) => child = next,
                        None => break,
                    }
                }
            }
        }
    }

    pub(crate) fn select_tab_range(&self, from_name: &str, to_name: &str) {
        self.clear_tab_selection();
        let mut selected = self.selected_tabs.borrow_mut();
        let mut in_range = false;

        if let Some(mut child) = self.tab_strip.first_child() {
            loop {
                let child_name = child.widget_name();
                if child_name.as_str() == from_name {
                    in_range = true;
                }
                if in_range {
                    selected.push(child_name.to_string());
                    if let Ok(btn) = child.clone().downcast::<ToggleButton>() {
                        btn.add_css_class("tab-selected");
                    }
                }
                if child_name.as_str() == to_name {
                    in_range = false;
                }
                match child.next_sibling() {
                    Some(next) => child = next,
                    None => break,
                }
            }
        }
    }

    pub(crate) fn close_selected_tabs(&self) {
        let selected = self.selected_tabs.borrow().clone();
        if selected.is_empty() {
            return;
        }

        let mut running = Vec::new();
        for tab_name in &selected {
            for page in 0..self.notebook.n_pages() {
                let Some(page_widget) = self.notebook.nth_page(Some(page)) else {
                    continue;
                };
                if page_widget.widget_name().as_str() != tab_name {
                    continue;
                }
                let label = crate::state::tab_label_text(&self.notebook, &page_widget)
                    .unwrap_or_else(|| format!("Tab {}", page + 1));
                for process in Self::running_processes_in_widget(&page_widget) {
                    running.push(format!("{label} — {process}"));
                }
                break;
            }
        }

        let close_selected = {
            let ui = self.clone();
            move || {
                // Resolve names again: tabs may have exited while a confirmation
                // dialog was open. Removing by the original stale widget could
                // otherwise tear down bookkeeping for an already-closed page.
                for tab_name in &selected {
                    let page_widget = (0..ui.notebook.n_pages()).find_map(|page| {
                        let widget = ui.notebook.nth_page(Some(page))?;
                        (widget.widget_name().as_str() == tab_name).then_some(widget)
                    });
                    if let Some(widget) = page_widget {
                        ui.remove_tab_by_widget_internal(&widget);
                    }
                }
                ui.clear_tab_selection();
            }
        };

        if running.is_empty() {
            close_selected();
            return;
        }

        const MAX_SHOWN: usize = 8;
        let hidden = running.len().saturating_sub(MAX_SHOWN);
        running.truncate(MAX_SHOWN);
        if hidden > 0 {
            running.push(format!("…and {hidden} more"));
        }
        let process_info = running.join("\n");
        let window = self.window.clone();
        glib::MainContext::default().spawn_local(async move {
            if Self::confirm_close_with_processes(
                &window,
                "Close selected tabs with running processes?",
                "Close Tabs",
                &process_info,
            )
            .await
            {
                close_selected();
            }
        });
    }

    pub(crate) fn move_tab_left(&self) {
        if let Some(current_page) = self.notebook.current_page() {
            if current_page > 0 {
                let new_page = current_page - 1;
                self.notebook.reorder_child(
                    &self.notebook.nth_page(Some(current_page)).unwrap(),
                    Some(new_page),
                );
                self.reorder_tab_strip_buttons();
                self.notebook.set_current_page(Some(new_page));
                self.sync_tab_strip_active(Some(new_page));
            }
        }
    }

    pub(crate) fn move_tab_right(&self) {
        if let Some(current_page) = self.notebook.current_page() {
            let n_pages = self.notebook.n_pages();
            if current_page < n_pages - 1 {
                let new_page = current_page + 1;
                self.notebook.reorder_child(
                    &self.notebook.nth_page(Some(current_page)).unwrap(),
                    Some(new_page),
                );
                self.reorder_tab_strip_buttons();
                self.notebook.set_current_page(Some(new_page));
                self.sync_tab_strip_active(Some(new_page));
            }
        }
    }

    fn reorder_tab_strip_buttons(&self) {
        let mut button_order = Vec::new();
        let mut idx = 0u32;
        while let Some(page) = self.notebook.nth_page(Some(idx)) {
            let name = page.widget_name();
            button_order.push(name);
            idx += 1;
        }

        let mut child = self.tab_strip.first_child();
        let mut button_idx = 0;
        while let Some(c) = child.clone() {
            if button_idx < button_order.len() && c.widget_name() == button_order[button_idx] {
                if button_idx > 0 {
                    let mut prev_child = self.tab_strip.first_child();
                    let mut prev_idx = 0;
                    while let Some(pc) = prev_child {
                        if prev_idx == button_idx - 1 {
                            self.tab_strip.reorder_child_after(&c, Some(&pc));
                            break;
                        }
                        prev_idx += 1;
                        prev_child = pc.next_sibling();
                    }
                } else {
                    self.tab_strip
                        .reorder_child_after(&c, None::<&gtk4::Widget>);
                }
                button_idx += 1;
            }
            child = c.next_sibling();
        }
        self.refresh_sidebar_tab_mirror();
    }

    /// Stable-partition pages so pinned tabs lead, matching anvil while
    /// preserving the relative order within the pinned and unpinned groups.
    /// Keep the same page active across the GTK reorder operation.
    pub(crate) fn reorder_pinned_first(&self) {
        let active = self
            .notebook
            .current_page()
            .and_then(|index| self.notebook.nth_page(Some(index)));
        let mut pages = Vec::new();
        for index in 0..self.notebook.n_pages() {
            if let Some(page) = self.notebook.nth_page(Some(index)) {
                pages.push(page);
            }
        }
        pages.sort_by_key(|page| !self.tab_page_is_pinned(page));
        for (index, page) in pages.iter().enumerate() {
            self.notebook.reorder_child(page, Some(index as u32));
        }
        self.reorder_tab_strip_buttons();
        let active_page = active
            .as_ref()
            .and_then(|page| self.notebook.page_num(page));
        self.notebook.set_current_page(active_page);
        self.sync_tab_strip_active(active_page);
    }

    pub(crate) fn toggle_current_tab_marked(&self) {
        if let Some(page) = self.notebook.current_page() {
            let mut idx = 0u32;
            let mut child = self.tab_strip.first_child();
            while let Some(c) = child {
                if idx == page {
                    if let Ok(btn) = c.clone().downcast::<ToggleButton>() {
                        if btn.has_css_class("tab-marked") {
                            btn.remove_css_class("tab-marked");
                            unsafe {
                                btn.set_data::<bool>("marked", false);
                            }
                        } else {
                            btn.add_css_class("tab-marked");
                            unsafe {
                                btn.set_data::<bool>("marked", true);
                            }
                        }
                    }
                    break;
                }
                idx += 1;
                child = c.next_sibling();
            }
        }
    }

    /// Toggle the "pinned" state of the current tab, mirroring the context-menu
    /// "Pin Tab" item: flips the strip button's css class + `pinned` data, the
    /// pin icon's visibility, and the notebook page's `pinned` data (read by
    /// session save), then stable-partitions pinned tabs to the front.
    pub(crate) fn toggle_current_tab_pinned(&self) {
        let Some(page) = self.notebook.current_page() else {
            return;
        };
        // The notebook page widget is the term wrapper that session save reads.
        if let Some(wrapper) = self.notebook.nth_page(Some(page)) {
            let mut idx = 0u32;
            let mut child = self.tab_strip.first_child();
            while let Some(c) = child {
                if idx == page {
                    if let Ok(btn) = c.clone().downcast::<ToggleButton>() {
                        let pinned = !btn.has_css_class("tab-pinned");
                        if pinned {
                            btn.add_css_class("tab-pinned");
                        } else {
                            btn.remove_css_class("tab-pinned");
                        }
                        unsafe {
                            btn.set_data::<bool>("pinned", pinned);
                        }
                        Self::set_tab_page_pinned(&wrapper, pinned);
                        if let Some(icon) = find_pin_icon(&btn) {
                            icon.set_visible(pinned);
                        }
                        self.reorder_pinned_first();
                    }
                    break;
                }
                idx += 1;
                child = c.next_sibling();
            }
        }
    }

    /// Persist tab pinning on both the notebook page and every concrete pane
    /// leaf. Session serialization walks split trees leaf-by-leaf, so keeping
    /// only the `Paned` root marked would lose the flag after a restart.
    pub(crate) fn set_tab_page_pinned(page: &gtk4::Widget, pinned: bool) {
        unsafe {
            page.set_data::<bool>("pinned", pinned);
        }
        if let Some(node) = PaneNode::from_widget(page) {
            for leaf in node.leaves() {
                unsafe {
                    leaf.root_widget().set_data::<bool>("pinned", pinned);
                }
            }
        }
    }

    /// Resolve pin state across every representation that survives a page-root
    /// replacement. A pre-existing pinned leaf or strip button repairs a new
    /// `Paned` root that has not received qdata yet.
    pub(crate) fn tab_page_is_pinned(&self, page: &gtk4::Widget) -> bool {
        let strip_button = self
            .find_strip_button(page.widget_name().as_str())
            .is_some_and(|button| button.has_css_class("tab-pinned"));
        let leaves = PaneNode::from_widget(page)
            .map(|node| {
                node.leaves()
                    .iter()
                    .map(|leaf| widget_pinned(&leaf.root_widget()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        resolved_tab_pinned(widget_pinned(page), strip_button, &leaves)
    }

    /// Find the strip button widget for a given tab widget name.
    pub(crate) fn find_strip_button(&self, widget_name: &str) -> Option<ToggleButton> {
        let mut child = self.tab_strip.first_child();
        while let Some(c) = child {
            if c.widget_name().as_str() == widget_name {
                return c.downcast::<ToggleButton>().ok();
            }
            child = c.next_sibling();
        }
        None
    }

    /// Mark a tab as having activity (new output on a non-active tab).
    pub(crate) fn mark_tab_activity(&self, tab_widget_name: &str) {
        if let Some(btn) = self.find_strip_button(tab_widget_name) {
            if !btn.is_active() {
                btn.add_css_class("tab-activity");
            }
        }
    }

    /// Mark a tab as having received a bell signal.
    pub(crate) fn mark_tab_bell(&self, tab_widget_name: &str) {
        if let Some(btn) = self.find_strip_button(tab_widget_name) {
            if !btn.is_active() {
                btn.add_css_class("tab-bell");
                btn.add_css_class("tab-bell-flash");
                // Remove flash animation class after it completes
                let btn_clone = btn.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
                    btn_clone.remove_css_class("tab-bell-flash");
                });
            }
        }
    }

    /// Clear activity/bell indicators when a tab becomes active.
    pub(crate) fn clear_tab_indicators(&self, tab_widget_name: &str) {
        if let Some(btn) = self.find_strip_button(tab_widget_name) {
            btn.remove_css_class("tab-activity");
            btn.remove_css_class("tab-bell");
            btn.remove_css_class("tab-bell-flash");
        }
    }

    /// Locate the connection-status dot inside a tab's strip button, if any.
    fn find_conn_dot(&self, tab_num: u32) -> Option<gtk4::Widget> {
        let btn = self.find_strip_button(&format!("tab-{}", tab_num))?;
        let strip_box = btn.child()?;
        let mut child = strip_box.first_child();
        while let Some(c) = child {
            if c.has_css_class("tab-conn-dot") {
                return Some(c);
            }
            child = c.next_sibling();
        }
        None
    }

    /// Update the per-tab connection-status dot (yellow/green/red).
    pub(crate) fn set_tab_conn_status(&self, tab_num: u32, status: super::ConnStatus) {
        if let Some(dot) = self.find_conn_dot(tab_num) {
            dot.remove_css_class("tab-connecting");
            dot.remove_css_class("tab-connected");
            dot.remove_css_class("tab-disconnected");
            match status {
                super::ConnStatus::Connecting => dot.add_css_class("tab-connecting"),
                super::ConnStatus::Connected => dot.add_css_class("tab-connected"),
                super::ConnStatus::Disconnected => dot.add_css_class("tab-disconnected"),
            }
            dot.set_visible(true);
        }
    }

    /// Remove the remote-only affordance after a remote leaf disappears from a
    /// split while a local sibling keeps the tab alive.
    pub(crate) fn clear_tab_conn_status(&self, tab_num: u32) {
        if let Some(dot) = self.find_conn_dot(tab_num) {
            dot.remove_css_class("tab-connecting");
            dot.remove_css_class("tab-connected");
            dot.remove_css_class("tab-disconnected");
            dot.set_visible(false);
        }
    }

    /// Relabel a tab's strip button (used for the reconnect countdown).
    pub(crate) fn set_tab_strip_label(&self, tab_num: u32, text: &str) {
        if let Some(btn) = self.find_strip_button(&format!("tab-{}", tab_num)) {
            if let Some(strip_box) = btn.child() {
                let mut child = strip_box.first_child();
                while let Some(c) = child {
                    if let Ok(label) = c.clone().downcast::<gtk4::Label>() {
                        if !label.has_css_class("tab-conn-dot")
                            && !label.has_css_class("tab-process-indicator")
                        {
                            label.set_text(text);
                            return;
                        }
                    }
                    child = c.next_sibling();
                }
            }
        }
    }
}

/// Locate the pin icon (`tab-pin-icon`) inside a strip button's child box.
fn find_pin_icon(btn: &ToggleButton) -> Option<gtk4::Image> {
    let strip_box = btn.child()?;
    let mut child = strip_box.first_child();
    while let Some(c) = child {
        if let Ok(img) = c.clone().downcast::<gtk4::Image>() {
            if img.has_css_class("tab-pin-icon") {
                return Some(img);
            }
        }
        child = c.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        command_finish_needs_failure_attention, dropped_tab_index,
        dropped_tab_index_in_pinned_partition, resolved_tab_pinned, tab_drag_drop_target_preload,
        tab_title_matches, tab_width_after_drag,
    };

    #[test]
    fn inactive_tab_failure_attention_requires_a_reported_nonzero_status() {
        assert!(!command_finish_needs_failure_attention(Some(0)));
        assert!(!command_finish_needs_failure_attention(None));
        assert!(command_finish_needs_failure_attention(Some(1)));
        assert!(command_finish_needs_failure_attention(Some(-1)));
    }

    #[test]
    fn tab_width_drag_uses_persisted_start_and_clamps_bounds() {
        assert_eq!(tab_width_after_drag(180, 40.4), 220);
        assert_eq!(tab_width_after_drag(180, -200.0), 80);
        assert_eq!(tab_width_after_drag(470, 100.0), 480);
        assert_eq!(tab_width_after_drag(180, f64::NAN), 180);
    }

    #[test]
    fn drop_before_and_after_adjust_for_source_removal() {
        assert_eq!(dropped_tab_index(0, 2, false), 1);
        assert_eq!(dropped_tab_index(0, 2, true), 2);
        assert_eq!(dropped_tab_index(3, 1, false), 1);
        assert_eq!(dropped_tab_index(3, 1, true), 2);
    }

    #[test]
    fn native_reorder_cannot_cross_the_pinned_partition() {
        // [pinned source, pinned, normal, normal] -> after the final normal.
        assert_eq!(
            dropped_tab_index_in_pinned_partition(0, 3, true, true, 2, 4),
            1
        );
        // [pinned, pinned, normal, normal source] -> before the first pinned.
        assert_eq!(
            dropped_tab_index_in_pinned_partition(3, 0, false, false, 2, 4),
            2
        );
        // Reorders within either partition retain their requested position.
        assert_eq!(
            dropped_tab_index_in_pinned_partition(1, 0, false, true, 2, 4),
            0
        );
        assert_eq!(
            dropped_tab_index_in_pinned_partition(2, 3, true, false, 2, 4),
            3
        );
    }

    #[test]
    fn tab_hover_target_preloads_the_typed_payload_before_release() {
        assert!(tab_drag_drop_target_preload());
    }

    #[test]
    fn hidden_hover_target_cannot_activate_after_its_timer_fires() {
        assert!(!super::tab_hover_target_can_activate(false, true, true));
        assert!(super::tab_hover_target_can_activate(true, true, true));
    }

    #[test]
    fn tab_filter_is_trimmed_and_case_insensitive() {
        assert!(tab_title_matches("  SERV  ", "Build Server"));
        assert!(tab_title_matches("", "anything"));
        assert!(!tab_title_matches("prod", "Build Server"));
    }

    #[test]
    fn pin_resolution_survives_a_new_unmarked_paned_root() {
        assert!(resolved_tab_pinned(false, true, &[true, false]));
        assert!(resolved_tab_pinned(false, false, &[true, false]));
        assert!(!resolved_tab_pinned(false, false, &[false, false]));
    }
}
