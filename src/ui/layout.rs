//! layout — tab placement (sidebar vs top bar) management for UiState.
use gtk4::prelude::*;
use gtk4::{Orientation, ToggleButton};

use super::*;
use crate::config::{SidebarView, TabPlacement};

impl UiState {
    /// Move the tab strip into the holder matching the current placement and
    /// adjust orientation and per-button sizing.
    pub(crate) fn apply_tab_placement(&self) {
        let placement = self.tab_placement.get();

        // Detach the strip from whichever scroll holder currently owns it. The
        // tab filter is not part of this move: it stays in the sidebar's Tabs
        // view for both placements.
        self.tab_strip_scroll.set_child(None::<&gtk4::Widget>);
        self.top_tab_scroll.set_child(None::<&gtk4::Widget>);

        match placement {
            TabPlacement::Sidebar => {
                self.tab_strip.set_orientation(Orientation::Vertical);
                self.tab_strip.set_valign(gtk4::Align::Start);
                self.tab_strip.set_hexpand(false);
                self.tab_strip.set_vexpand(true);
                self.tab_strip.set_width_request(-1);
                self.tab_strip.remove_css_class("top-tabs");
                self.tab_strip_scroll.set_child(Some(&self.tab_strip));
            }
            TabPlacement::TopBar => {
                self.tab_strip.set_orientation(Orientation::Horizontal);
                self.tab_strip.set_valign(gtk4::Align::Center);
                self.tab_strip.set_hexpand(true);
                self.tab_strip.set_vexpand(false);
                // The scroller owns overflow; the sum of fixed tab widths must
                // not become the application's minimum window width.
                self.tab_strip.set_width_request(1);
                self.tab_strip.add_css_class("top-tabs");
                self.top_tab_scroll.set_child(Some(&self.tab_strip));
            }
        }

        // Resize each existing strip button for the new orientation.
        let mut child = self.tab_strip.first_child();
        while let Some(c) = child {
            if let Ok(btn) = c.clone().downcast::<ToggleButton>() {
                self.apply_strip_btn_placement(&btn);
            }
            child = c.next_sibling();
        }

        // The sidebar Tabs view stays available in both placements: with the
        // strip in the top bar the sidebar shows the mirror list instead, so
        // tabs are visible in two places at once rather than moving.
        self.sidebar_tabs_btn.set_sensitive(true);
        self.apply_sidebar_view(self.sidebar_view.get(), false);

        self.sync_tab_bar_visibility();
    }

    /// Show one sidebar view (tab list vs file tree) and reflect it in the
    /// segmented buttons. When `persist`, remember the choice in config.
    pub(crate) fn apply_sidebar_view(&self, view: SidebarView, persist: bool) {
        self.invalidate_file_tree_remote_follow();
        match view {
            SidebarView::Tabs => self.sidebar_stack.set_visible_child_name("tabs"),
            SidebarView::Files => {
                self.sidebar_stack.set_visible_child_name("files");
                // Hosts may have been added/removed while the tree was hidden.
                self.refresh_file_tree_location_selector();
            }
        }
        // set_active does not refire `clicked`, so this won't recurse.
        self.sidebar_tabs_btn.set_active(view == SidebarView::Tabs);
        self.sidebar_files_btn
            .set_active(view == SidebarView::Files);

        if persist {
            self.sidebar_view.set(view);
            self.config.borrow_mut().sidebar_view = view;
            self.persist_config();
        }
    }

    /// Size a single strip button for the active placement: fill width in the
    /// sidebar, use the persisted draggable width in the top bar.
    pub(crate) fn apply_strip_btn_placement(&self, btn: &ToggleButton) {
        match self.tab_placement.get() {
            TabPlacement::Sidebar => {
                btn.set_hexpand(true);
                btn.set_width_request(-1);
                self.set_tab_width_handle_visible(btn, false);
            }
            TabPlacement::TopBar => {
                btn.set_hexpand(false);
                btn.set_width_request(self.config.borrow().tab_width as i32);
                self.set_tab_width_handle_visible(btn, true);
            }
        }
    }

    /// Flip the tab strip between the sidebar and the top bar, then persist.
    pub(crate) fn toggle_tab_placement(&self) {
        let next = match self.tab_placement.get() {
            TabPlacement::Sidebar => TabPlacement::TopBar,
            TabPlacement::TopBar => TabPlacement::Sidebar,
        };
        self.tab_placement.set(next);
        self.config.borrow_mut().tab_placement = next;
        self.apply_tab_placement();
        self.persist_config();
    }
}
