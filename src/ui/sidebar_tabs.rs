//! sidebar_tabs — the sidebar's mirror of the tab strip.
//!
//! `tab_strip` is a single widget that gets reparented into whichever holder
//! the current [`TabPlacement`](crate::config::TabPlacement) names, so it
//! cannot be in the top bar and in the sidebar at the same time. This module
//! owns a second, always-vertical list that keeps the sidebar's Tabs view
//! usable while the strip is docked to the top bar.
//!
//! The mirror derives everything from the real strip (order, titles, pin
//! state, filter visibility), so tab state still has exactly one owner. Rows
//! are patched in place while the page names line up: rebuilding a row would
//! destroy its button between pointer press and release, and GTK would never
//! deliver the click.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Label, Orientation, ToggleButton};

use super::*;

/// What the mirror needs to know about one strip button.
struct MirrorRow {
    /// Notebook page widget name (`tab-N`) — the identity every tab operation
    /// is keyed on.
    page_name: String,
    /// The strip's own title label. The mirror binds to it rather than copying
    /// the text, so OSC title updates need no rebuild.
    title: Label,
    pinned: bool,
    /// False when the tab filter has hidden this tab. This is the button's own
    /// visibility flag, never `is_visible()`: that one also answers for every
    /// ancestor, so a strip whose holder happens to be hidden would report all
    /// of its tabs as filtered out and empty the sidebar list.
    visible: bool,
}

impl UiState {
    /// Read the current strip contents. The strip is the source of truth: it
    /// already carries the filter's visibility decisions and the pin classes.
    fn mirror_rows(&self) -> Vec<MirrorRow> {
        let mut rows = Vec::new();
        let mut child = self.tab_strip.first_child();
        while let Some(widget) = child {
            if let Ok(button) = widget.clone().downcast::<ToggleButton>() {
                let title = unsafe { button.data::<Label>("tab-title-label") }
                    .map(|l| unsafe { l.as_ref() }.clone());
                if let Some(title) = title {
                    rows.push(MirrorRow {
                        page_name: button.widget_name().to_string(),
                        title,
                        pinned: button.has_css_class("tab-pinned"),
                        visible: button.get_visible(),
                    });
                }
            }
            child = widget.next_sibling();
        }
        rows
    }

    /// Bring the sidebar mirror in line with the strip. Cheap and idempotent:
    /// when the page names and their order are unchanged it only refreshes
    /// per-row state, leaving the widgets (and any in-flight click) alone.
    pub(crate) fn refresh_sidebar_tab_mirror(&self) {
        let rows = self.mirror_rows();

        let mut existing: Vec<gtk4::Widget> = Vec::new();
        let mut child = self.sidebar_tab_mirror.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            existing.push(widget);
        }

        let same = existing.len() == rows.len()
            && existing
                .iter()
                .zip(rows.iter())
                .all(|(widget, row)| widget.widget_name() == row.page_name);

        if !same {
            for widget in &existing {
                self.sidebar_tab_mirror.remove(widget);
            }
            for row in &rows {
                let widget = self.build_mirror_row(row);
                self.sidebar_tab_mirror.append(&widget);
            }
        }

        self.sync_sidebar_tab_mirror_state(&rows);
        self.sync_sidebar_tab_mirror_active(self.active_page_name().as_deref());
    }

    /// Selection does not change the mirror's structure, titles or pin state.
    /// Use the switch-page target; Notebook::current_page is still the old page
    /// while that signal is being delivered.
    pub(crate) fn sync_sidebar_tab_mirror_active(&self, active: Option<&str>) {
        sync_mirror_active(&self.sidebar_tab_mirror, active);
    }

    /// Refresh only the parts that change without restructuring the list.
    fn sync_sidebar_tab_mirror_state(&self, rows: &[MirrorRow]) {
        let mut child = self.sidebar_tab_mirror.first_child();
        let mut index = 0usize;
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Some(row) = rows.get(index) else { break };
            index += 1;
            widget.set_visible(row.visible);
            if let Some(button) = mirror_row_button(&widget) {
                if row.pinned {
                    button.add_css_class("tab-pinned");
                } else {
                    button.remove_css_class("tab-pinned");
                }
            }
        }
    }

    fn active_page_name(&self) -> Option<String> {
        self.notebook
            .current_page()
            .and_then(|page| self.notebook.nth_page(Some(page)))
            .map(|page| page.widget_name().to_string())
    }

    fn build_mirror_row(&self, row: &MirrorRow) -> gtk4::Box {
        let container = gtk4::Box::new(Orientation::Horizontal, 2);
        container.set_widget_name(&row.page_name);

        let button = ToggleButton::new();
        button.set_hexpand(true);
        button.add_css_class("tab-strip-btn");
        button.set_widget_name("mirror-button");
        // Selecting a tab must leave the keyboard on the terminal — the
        // notebook's switch-page handler owns focus. Keyboard traversal can
        // still reach the button.
        button.set_focus_on_click(false);

        let label = Label::new(None);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label.set_single_line_mode(true);
        // Follow the strip's label instead of snapshotting its text, so a tab
        // that renames itself mid-session updates here too.
        row.title
            .bind_property("label", &label, "label")
            .sync_create()
            .build();
        button.set_child(Some(&label));

        // Selection hangs off `clicked`, not `toggled`: GTK4 emits `clicked`
        // only for real activation (pointer or keyboard), never for the
        // `set_active` calls the state sync makes, so there is no feedback
        // loop to guard against. An added GestureClick would not work here —
        // the button's own gesture claims the sequence first, so a bubble
        // -phase handler never sees the release.
        let ui = self.clone();
        let page_name = row.page_name.clone();
        button.connect_clicked(move |_| {
            ui.activate_tab_named(&page_name);
        });

        // Clicking a button always flips it, including the active one the
        // user just re-clicked. Put the list back the way the notebook says
        // it should be once the click has been dealt with.
        let ui = self.clone();
        button.connect_clicked(move |_| {
            let ui = ui.clone();
            glib::idle_add_local_once(move || {
                ui.sync_sidebar_tab_mirror_active(ui.active_page_name().as_deref());
            });
        });

        let close = gtk4::Button::from_icon_name("window-close-symbolic");
        close.set_has_frame(false);
        close.add_css_class("flat");
        close.add_css_class("tab-strip-close");
        close.set_tooltip_text(Some("Close tab"));
        close.update_property(&[gtk4::accessible::Property::Label("Close tab")]);
        close.set_focus_on_click(false);
        let ui = self.clone();
        let page_name = row.page_name.clone();
        close.connect_clicked(move |_| {
            if let Some(page) = super::tabs::notebook_page_named(&ui.notebook, &page_name) {
                ui.remove_tab_by_widget(&page);
            }
        });

        container.append(&button);
        container.append(&close);
        container
    }
}

fn mirror_row_button(row: &gtk4::Widget) -> Option<ToggleButton> {
    row.first_child()?.downcast::<ToggleButton>().ok()
}

fn sync_mirror_active(mirror: &gtk4::Box, active: Option<&str>) {
    let mut child = mirror.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(button) = mirror_row_button(&widget) {
            button.set_active(active == Some(widget.widget_name().as_str()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display"]
    fn many_tab_switches_update_mirror_before_notebook_commits() {
        gtk4::init().expect("GTK display");
        let notebook = gtk4::Notebook::new();
        let mirror = gtk4::Box::new(Orientation::Vertical, 0);
        let strip = gtk4::Box::new(Orientation::Horizontal, 0);
        let clicks = Rc::new(Cell::new(0));
        let mut buttons = Vec::new();
        let mut strip_buttons = Vec::new();
        for index in 0..64 {
            let name = format!("tab-{index}");
            let page = gtk4::Box::new(Orientation::Vertical, 0);
            page.set_widget_name(&name);
            notebook.append_page(&page, None::<&gtk4::Widget>);
            let row = gtk4::Box::new(Orientation::Horizontal, 0);
            row.set_widget_name(&name);
            let button = ToggleButton::new();
            let clicks = clicks.clone();
            button.connect_clicked(move |_| clicks.set(clicks.get() + 1));
            row.append(&button);
            mirror.append(&row);
            buttons.push(button);
            let strip_button = ToggleButton::new();
            strip.append(&strip_button);
            strip_buttons.push(strip_button);
        }
        // Selection-only updates must preserve filtered rows and pin styling.
        mirror.first_child().unwrap().set_visible(false);
        buttons[0].add_css_class("tab-pinned");
        notebook.set_current_page(Some(0));
        sync_mirror_active(&mirror, Some("tab-0"));
        let switches = Rc::new(Cell::new(0));
        let observed_switches = switches.clone();
        let mirror_for_switch = mirror.clone();
        let buttons_for_switch = buttons.clone();
        let correct = Rc::new(Cell::new(true));
        let correct_for_switch = correct.clone();
        let handler = notebook.connect_switch_page(move |notebook, page, index| {
            correct_for_switch
                .set(correct_for_switch.get() && notebook.current_page() != Some(index));
            super::super::tab_strip::sync_strip_buttons_active(&strip, index);
            sync_mirror_active(&mirror_for_switch, Some(page.widget_name().as_str()));
            // No main-loop/idle drain: the target must already be selected.
            for (i, button) in buttons_for_switch.iter().enumerate() {
                correct_for_switch.set(
                    correct_for_switch.get()
                        && button.is_active() == (i == index as usize)
                        && strip_buttons[i].is_active() == (i == index as usize),
                );
            }
            observed_switches.set(observed_switches.get() + 1);
        });
        for _ in 0..4 {
            for index in 1..64 {
                notebook.set_current_page(Some(index));
            }
            for index in (0..63).rev() {
                notebook.set_current_page(Some(index));
            }
        }
        // Disposal removes pages and emits further switch signals.
        notebook.disconnect(handler);
        assert!(
            correct.get(),
            "both strips must select the target synchronously"
        );
        assert_eq!(switches.get(), 504);
        assert_eq!(
            clicks.get(),
            0,
            "programmatic switching must not request click repair"
        );
        assert!(!mirror.first_child().unwrap().get_visible());
        assert!(buttons[0].has_css_class("tab-pinned"));
    }
}
