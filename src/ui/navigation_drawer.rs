//! Rehost one authoritative navigation subtree without rebuilding its controls.
use gtk4::prelude::*;

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
    fn rehost_keeps_the_exact_navigation_widgets_draft_selection_and_scroll() {
        if gtk4::init().is_err() {
            return;
        }
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
