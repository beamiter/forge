//! Rehost one authoritative navigation subtree without rebuilding its controls.
use gtk4::prelude::*;

pub(super) fn focus_preserving_selection(widget: &gtk4::Widget) -> bool {
    let editable = widget.clone().dynamic_cast::<gtk4::Editable>().ok();
    let selection = editable.as_ref().and_then(|entry| entry.selection_bounds());
    let position = editable.as_ref().map(|entry| entry.position());
    let focused = widget.grab_focus();
    if let Some(entry) = editable {
        if let Some((start, end)) = selection {
            entry.select_region(start, end);
        } else if let Some(position) = position {
            entry.set_position(position);
        }
    }
    focused
}

pub(super) fn search_target(root: &gtk4::Widget) -> Option<gtk4::Widget> {
    if (root.is::<gtk4::SearchEntry>() || root.is::<gtk4::Entry>())
        && root.is_mapped()
        && root.is_sensitive()
    {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(target) = search_target(&widget) {
            return Some(target);
        }
        child = widget.next_sibling();
    }
    None
}

pub(super) fn rehost(
    sidebar: &gtk4::Box,
    home: &gtk4::Paned,
    drawer: &gtk4::Box,
    overlay: bool,
) -> bool {
    let was_overlay = sidebar.parent().as_ref() == Some(drawer.upcast_ref());
    if was_overlay == overlay {
        return false;
    }
    if was_overlay {
        drawer.remove(sidebar);
    } else {
        home.set_start_child(None::<&gtk4::Widget>);
    }
    if overlay {
        drawer.append(sidebar);
    } else {
        home.set_start_child(Some(sidebar));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an isolated GTK display and one test per process"]
    fn rehost_keeps_the_exact_navigation_widgets_draft_selection_and_scroll() {
        gtk4::init().expect("isolated GTK display is required");
        let sidebar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let search = gtk4::Entry::new();
        search.set_text("same query 中文");
        search.select_region(2, 6);
        let scroll = gtk4::ScrolledWindow::new();
        let adjustment = gtk4::Adjustment::new(42.0, 0.0, 1000.0, 1.0, 10.0, 100.0);
        scroll.set_vadjustment(Some(&adjustment));
        scroll.set_child(Some(&search));
        let original_child = scroll.child().unwrap(); // GTK inserts a viewport for Entry.
        sidebar.append(&scroll);
        let home = gtk4::Paned::new(gtk4::Orientation::Horizontal);
        home.set_start_child(Some(&sidebar));
        let drawer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        assert!(rehost(&sidebar, &home, &drawer, true));
        assert!(home.start_child().is_none());
        assert_eq!(drawer.first_child().as_ref(), Some(sidebar.upcast_ref()));
        assert_eq!(scroll.child().as_ref(), Some(&original_child));
        assert_eq!(search.selection_bounds(), Some((2, 6)));
        assert_eq!(adjustment.value(), 42.0);
        assert!(!rehost(&sidebar, &home, &drawer, true));
        assert!(rehost(&sidebar, &home, &drawer, false));
        assert!(drawer.first_child().is_none());
        assert_eq!(home.start_child().as_ref(), Some(sidebar.upcast_ref()));
        assert_eq!(search.text().as_str(), "same query 中文");
        assert_eq!(search.selection_bounds(), Some((2, 6)));
        assert_eq!(adjustment.value(), 42.0);
    }
}
