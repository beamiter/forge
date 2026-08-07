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

    // Clear root focus while both children still belong to GtkPaned. If a
    // focused child is unparented first, GtkPaned can retain a stale private
    // last-focus pointer and warn while the sibling is detached for promotion.
    if let Some(root) = parent.root() {
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
            let inserted = notebook.insert_page(&sibling, label.as_ref(), Some(index));
            notebook.set_tab_reorderable(&sibling, true);
            notebook.set_current_page(Some(inserted));
        }
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
    let current_page = notebook.page_num(&swap.zoomed_page)?;
    let page_name = swap.zoomed_page.widget_name().to_string();
    notebook.remove_page(Some(current_page));

    reattach_terminal_to_tree(&swap.original_page, &swap.zoomed_page);
    swap.original_page.set_widget_name(&page_name);
    let inserted = notebook.insert_page(
        &swap.original_page,
        swap.tab_label.as_ref(),
        Some(swap.page_index),
    );
    notebook.set_tab_reorderable(&swap.original_page, true);
    notebook.set_current_page(Some(inserted));
    Some(inserted)
}
