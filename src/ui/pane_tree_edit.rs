//! Structural edits for the native GTK pane tree.
//!
//! Closing and moving a split leaf both perform the same mutation: detach the
//! target leaf, remove its parent `Paned`, and promote the sibling into either the
//! ancestor `Paned` or the original Notebook page. Keeping that mutation here
//! prevents lifecycle paths from implementing subtly different widget surgery.

use gtk4::prelude::*;
use gtk4::{Notebook, Paned, Widget};

use crate::terminal::reattach_terminal_to_tree;

/// Detach `leaf_root` from its parent split and promote its sibling.
///
/// Returns the promoted sibling. A direct Notebook leaf has no split to collapse
/// and returns `None`; callers can then apply their normal whole-tab behavior.
pub(crate) fn detach_leaf_and_promote(notebook: &Notebook, leaf_root: &Widget) -> Option<Widget> {
    let parent = leaf_root.parent()?.downcast::<Paned>().ok()?;
    let start = parent.start_child();
    let end = parent.end_child();
    let sibling = if start.as_ref() == Some(leaf_root) {
        end?
    } else if end.as_ref() == Some(leaf_root) {
        start?
    } else {
        return None;
    };

    enum Destination {
        Start(Paned),
        End(Paned),
        Page {
            index: u32,
            name: String,
            label: Option<Widget>,
        },
    }

    // Resolve the complete destination before detaching either child. A stale
    // or malformed tree is therefore a no-op instead of a half-collapsed split.
    let parent_widget = parent.clone().upcast::<Widget>();
    let destination = {
        let grandparent = parent_widget.parent()?;
        if let Ok(grandparent) = grandparent.downcast::<Paned>() {
            if grandparent.start_child().as_ref() == Some(&parent_widget) {
                Destination::Start(grandparent)
            } else if grandparent.end_child().as_ref() == Some(&parent_widget) {
                Destination::End(grandparent)
            } else {
                return None;
            }
        } else {
            let index = notebook.page_num(&parent_widget)?;
            Destination::Page {
                index,
                name: parent_widget.widget_name().to_string(),
                label: notebook.tab_label(&parent_widget),
            }
        }
    };

    let selected = notebook
        .current_page()
        .and_then(|page| notebook.nth_page(Some(page)));
    let root = parent.root();
    let focus = root.as_ref().and_then(|root| root.focus());

    // Clear root focus while both children still belong to GtkPaned. If a
    // focused child is unparented first, GtkPaned can retain a stale private
    // last-focus pointer and warn while the sibling is detached for promotion.
    if let Some(root) = root.as_ref().filter(|_| {
        focus
            .as_ref()
            .is_some_and(|focus| widget_is_within(focus, &parent_widget))
    }) {
        root.set_focus(None::<&Widget>);
    }

    parent.set_start_child(None::<&Widget>);
    parent.set_end_child(None::<&Widget>);

    match destination {
        Destination::Start(grandparent) => {
            grandparent.set_start_child(Some(&sibling));
        }
        Destination::End(grandparent) => {
            grandparent.set_end_child(Some(&sibling));
        }
        Destination::Page { index, name, label } => {
            notebook.remove_page(Some(index));
            sibling.set_widget_name(&name);
            notebook.insert_page(&sibling, label.as_ref(), Some(index));
            notebook.set_tab_reorderable(&sibling, true);
            restore_selection_after_replacement(notebook, selected, &parent_widget, &sibling);
        }
    }
    if let Some(root) = root.as_ref() {
        restore_surviving_focus(root, focus.as_ref());
    }
    Some(sibling)
}
enum LeafSplitSlot {
    Start(Paned),
    End(Paned),
    Page {
        index: u32,
        label: Option<Widget>,
        name: String,
    },
}

/// Fully validated destination for inserting one existing leaf beside another.
///
/// Planning holds GTK object identities but changes no parents. Once the source
/// tab is detached, `commit` contains no lookup or fallible branch and therefore
/// cannot strand the live terminal between representations.
pub(crate) struct ExistingLeafSplitPlan {
    target_page: Widget,
    target_leaf: Widget,
    slot: LeafSplitSlot,
}

impl ExistingLeafSplitPlan {
    /// Account for removing a source Notebook page before this plan commits.
    pub(crate) fn after_removing_page(mut self, removed_index: u32) -> Self {
        if let LeafSplitSlot::Page { index, .. } = &mut self.slot {
            if removed_index < *index {
                *index -= 1;
            }
        }
        self
    }

    /// Replace the target slot with a new split containing both existing roots.
    pub(crate) fn commit(
        self,
        notebook: &Notebook,
        incoming: &Widget,
        orientation: gtk4::Orientation,
        incoming_first: bool,
    ) -> Widget {
        let paned = Paned::new(orientation);
        paned.set_hexpand(true);
        paned.set_vexpand(true);
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_shrink_start_child(true);
        paned.set_shrink_end_child(true);

        match &self.slot {
            LeafSplitSlot::Start(parent) => parent.set_start_child(Some(&paned)),
            LeafSplitSlot::End(parent) => parent.set_end_child(Some(&paned)),
            LeafSplitSlot::Page { index, .. } => notebook.remove_page(Some(*index)),
        }

        if incoming_first {
            paned.set_start_child(Some(incoming));
            paned.set_end_child(Some(&self.target_leaf));
        } else {
            paned.set_start_child(Some(&self.target_leaf));
            paned.set_end_child(Some(incoming));
        }

        match self.slot {
            LeafSplitSlot::Start(_) | LeafSplitSlot::End(_) => self.target_page,
            LeafSplitSlot::Page { index, label, name } => {
                paned.set_widget_name(&name);
                let inserted = notebook.insert_page(&paned, label.as_ref(), Some(index));
                notebook.set_tab_reorderable(&paned, true);
                notebook.set_current_page(Some(inserted));
                paned.upcast()
            }
        }
    }
}

/// Validate the exact tree slot that will receive an existing dragged leaf.
pub(crate) fn plan_existing_leaf_split(
    notebook: &Notebook,
    target_page: &Widget,
    target_leaf: &Widget,
) -> Option<ExistingLeafSplitPlan> {
    let slot = if target_page == target_leaf {
        LeafSplitSlot::Page {
            index: notebook.page_num(target_page)?,
            label: notebook.tab_label(target_page),
            name: target_page.widget_name().to_string(),
        }
    } else {
        let parent = target_leaf.parent()?.downcast::<Paned>().ok()?;
        let parent_widget = parent.clone().upcast::<Widget>();
        let mut ancestor = Some(parent_widget);
        let mut belongs_to_page = false;
        while let Some(widget) = ancestor {
            if widget == *target_page {
                belongs_to_page = true;
                break;
            }
            ancestor = widget.parent();
        }
        if !belongs_to_page || notebook.page_num(target_page).is_none() {
            return None;
        }
        if parent.start_child().as_ref() == Some(target_leaf) {
            LeafSplitSlot::Start(parent)
        } else if parent.end_child().as_ref() == Some(target_leaf) {
            LeafSplitSlot::End(parent)
        } else {
            return None;
        }
    };

    Some(ExistingLeafSplitPlan {
        target_page: target_page.clone(),
        target_leaf: target_leaf.clone(),
        slot,
    })
}

/// Notebook-page swap retained while one split leaf is zoomed.
pub(crate) struct ZoomPageSwap {
    pub(crate) original_page: Widget,
    pub(crate) zoomed_page: Widget,
    pub(crate) page_index: u32,
    pub(crate) tab_label: Option<Widget>,
}

/// Detach one leaf from its split tree and expose it as the Notebook page.
pub(crate) fn detach_leaf_for_zoom(
    notebook: &Notebook,
    page_widget: &Widget,
    leaf_root: &Widget,
) -> Option<ZoomPageSwap> {
    let parent = leaf_root.parent()?.downcast::<Paned>().ok()?;
    if parent.start_child().as_ref() == Some(leaf_root) {
        parent.set_start_child(None::<&Widget>);
    } else if parent.end_child().as_ref() == Some(leaf_root) {
        parent.set_end_child(None::<&Widget>);
    } else {
        return None;
    }

    let page_index = notebook.page_num(page_widget)?;
    let page_name = page_widget.widget_name().to_string();
    let tab_label = notebook.tab_label(page_widget);
    notebook.remove_page(Some(page_index));

    leaf_root.set_widget_name(&page_name);
    let inserted = notebook.insert_page(leaf_root, tab_label.as_ref(), Some(page_index));
    notebook.set_tab_reorderable(leaf_root, true);
    notebook.set_current_page(Some(inserted));

    Some(ZoomPageSwap {
        original_page: page_widget.clone(),
        zoomed_page: leaf_root.clone(),
        page_index,
        tab_label,
    })
}

/// Restore a zoomed leaf to its empty split slot and reinstate the original page.
pub(crate) fn restore_zoomed_leaf(notebook: &Notebook, swap: &ZoomPageSwap) -> Option<u32> {
    restore_zoomed_leaf_with_selection(notebook, swap, true)
}

pub(crate) fn restore_zoomed_leaf_preserving_selection(
    notebook: &Notebook,
    swap: &ZoomPageSwap,
) -> Option<u32> {
    restore_zoomed_leaf_with_selection(notebook, swap, false)
}

fn restore_zoomed_leaf_with_selection(
    notebook: &Notebook,
    swap: &ZoomPageSwap,
    activate_restored: bool,
) -> Option<u32> {
    let selected = notebook
        .current_page()
        .and_then(|page| notebook.nth_page(Some(page)));
    let current_page = notebook.page_num(&swap.zoomed_page)?;
    let page_name = swap.zoomed_page.widget_name().to_string();
    notebook.remove_page(Some(current_page));

    reattach_terminal_to_tree(&swap.original_page, &swap.zoomed_page);
    swap.original_page.set_widget_name(&page_name);
    let inserted = notebook.insert_page(
        &swap.original_page,
        swap.tab_label.as_ref(),
        // Tabs can be reordered while zoomed. Replace the live placeholder
        // in its current slot instead of restoring its historical index.
        Some(current_page),
    );
    notebook.set_tab_reorderable(&swap.original_page, true);
    if activate_restored {
        notebook.set_current_page(Some(inserted));
    } else {
        restore_selection_after_replacement(
            notebook,
            selected,
            &swap.zoomed_page,
            &swap.original_page,
        );
    }
    Some(inserted)
}

pub(crate) fn widget_is_within(widget: &Widget, subtree: &Widget) -> bool {
    widget == subtree || widget.is_ancestor(subtree)
}

/// Return focus only to an object that survived in the same native root.
/// Background close must not replace a live search/settings focus with a VTE.
pub(crate) fn restore_surviving_focus(root: &gtk4::Root, focus: Option<&Widget>) -> bool {
    let Some(focus) = focus.filter(|focus| {
        focus.root().as_ref() == Some(root) && focus.is_mapped() && focus.is_sensitive()
    }) else {
        return false;
    };
    root.focus().as_ref() == Some(focus) || focus.grab_focus()
}

fn selection_after_replacement<T: Clone + PartialEq>(
    selected: Option<T>,
    removed: &T,
    replacement: &T,
) -> Option<T> {
    selected.map(|selected| {
        if &selected == removed {
            replacement.clone()
        } else {
            selected
        }
    })
}

fn restore_selection_after_replacement(
    notebook: &Notebook,
    selected: Option<Widget>,
    removed: &Widget,
    replacement: &Widget,
) {
    if let Some(selected) = selection_after_replacement(selected, removed, replacement) {
        if let Some(index) = notebook.page_num(&selected) {
            notebook.set_current_page(Some(index));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::selection_after_replacement;

    #[test]
    fn background_replacements_preserve_selected_identity_across_index_changes() {
        let selected = selection_after_replacement(Some("B"), &"zoom A", &"split A");
        assert_eq!(selected, Some("B"));
        // Removing/replacing A may change indices; resolve B in the final tree.
        let reordered = ["C", "split A", "B"];
        assert_eq!(
            reordered.iter().position(|id| Some(*id) == selected),
            Some(2)
        );
        assert_eq!(
            selection_after_replacement(Some("B"), &"split A", &"surviving A pane"),
            Some("B")
        );
    }

    #[test]
    fn active_zoom_and_split_replacements_follow_only_the_replaced_page() {
        assert_eq!(
            selection_after_replacement(Some("zoom A"), &"zoom A", &"split A"),
            Some("split A")
        );
        assert_eq!(
            selection_after_replacement(Some("split A"), &"split A", &"survivor A"),
            Some("survivor A")
        );
        assert_eq!(selection_after_replacement::<&str>(None, &"A", &"B"), None);
    }

    #[test]
    fn close_entrypoints_capture_target_before_scoped_zoom_restore() {
        let tabs = include_str!("tabs.rs");
        let close = tabs
            .split_once("pub(crate) fn close_focused_pane_or_tab(&self)")
            .unwrap()
            .1
            .split_once("pub(crate) fn duplicate_current_tab")
            .unwrap()
            .0;
        let restore = close
            .find("self.restore_zoom_before_close(&target);")
            .unwrap();
        assert!(close.find("let leaf =").unwrap() < restore);
        assert!(close.find("let target =").unwrap() < restore);
        assert!(!close[restore..].contains("current_page()"));
        assert!(!close.contains("self.remove_current_tab()"));
        assert!(close.contains("self.remove_tab_by_widget(&target)"));
        let restore = tabs
            .split_once("fn restore_zoom_before_close(")
            .unwrap()
            .1
            .split_once("/// Resolve a descendant")
            .unwrap()
            .0;
        assert!(restore.contains("widget_is_within(target, &state.swap.zoomed_page)"));
        assert!(restore.contains("widget_is_within(target, &state.swap.original_page)"));
        assert!(restore.contains("restore_zoomed_leaf_preserving_selection"));
        assert!(!restore.contains("self.unzoom_pane"));
        assert!(tabs.contains("self.restore_zoom_before_close(widget);"));
        assert!(tabs.contains("self.restore_zoom_before_close(&leaf_root);"));
    }

    #[test]
    fn background_close_never_falls_back_to_terminal_focus() {
        let tabs = include_str!("tabs.rs");
        let exit = tabs
            .split_once("pub(crate) fn handle_terminal_exited_with_code(")
            .unwrap()
            .1
            .split_once("pub(crate) fn remove_current_tab")
            .unwrap()
            .0;
        let close = tabs
            .split_once("pub(crate) fn remove_tab_by_widget_internal(")
            .unwrap()
            .1
            .split_once("pub(crate) fn close_focused_pane_or_tab")
            .unwrap()
            .0;
        assert!(
            close
                .find("self.restore_zoom_before_close(widget);")
                .unwrap()
                < close.find(".notebook_page_for_widget(widget)").unwrap()
        );
        for source in [exit, close] {
            assert!(source
                .contains("!restore_surviving_focus(root, focus.as_ref()) && target_was_active"));
        }
        let tree = include_str!("pane_tree_edit.rs")
            .split_once("#[cfg(test)]")
            .unwrap()
            .0;
        assert!(tree.contains("widget_is_within(focus, &parent_widget)"));
        assert!(tree.contains("focus.root().as_ref() == Some(root)"));
        assert!(tree.contains(
            "restore_selection_after_replacement(notebook, selected, &parent_widget, &sibling)"
        ));
        let zoom = include_str!("zoom.rs");
        assert!(zoom.contains("restore_zoomed_leaf(&self.notebook, &state.swap)"));
        assert!(zoom.contains("state.zoomed_terminal.grab_focus();"));
    }
}
