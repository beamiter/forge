//! Persistent surface for Block-history fail-closed states.
//!
//! A Block-history save can refuse for reasons that stay true until somebody
//! acts: the file's revision moved under this window, the load it must not
//! overwrite failed, the volume is full, the advisory lock never cleared.
//! Those used to arrive as the same eight-second toast every other persistence
//! failure gets, so the one class of failure that *needs* a decision was the
//! class most likely to be missed — a toast that has already faded is not a
//! decision. This bar stays until it is answered, and it carries the answer.

use adw::prelude::*;
use gtk4::{Align, Box as GBox, Button};
use libadwaita as adw;
use std::rc::Rc;

use super::{PaneNode, UiState};

/// Where a background persistence failure is shown.
///
/// The default is a toast, and for most operations that is right: the write
/// will be attempted again the next time the thing it saves changes. Two are
/// not like that. A settings save that failed leaves the window running an
/// in-memory setting the file does not have, and a Block-history save that
/// failed has stopped saving that pane until something changes — both stay
/// wrong until somebody acts, so both get a surface that waits for somebody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistenceFailureSurface {
    ConfigDialog,
    BlockHistoryBar,
    Toast,
}

pub(crate) fn persistence_failure_surface(operation: &str) -> PersistenceFailureSurface {
    if operation == super::CONFIG_PERSIST_OPERATION {
        PersistenceFailureSurface::ConfigDialog
    } else if operation == crate::block_view::BLOCK_HISTORY_PERSIST_OPERATION {
        PersistenceFailureSurface::BlockHistoryBar
    } else {
        PersistenceFailureSurface::Toast
    }
}

impl UiState {
    /// Build the (initially hidden) Block-history failure bar. The caller
    /// places the returned widget; the label and the bar itself are held on
    /// `UiState` so the persistence poll can reveal them later.
    pub(crate) fn build_block_history_notice(self: &Rc<Self>) -> GBox {
        let bar = self.block_history_notice.clone();
        bar.add_css_class("toolbar");
        bar.add_css_class("error");
        bar.set_margin_start(6);
        bar.set_margin_end(6);
        bar.set_margin_top(2);
        bar.set_margin_bottom(2);
        bar.set_visible(false);

        let label = self.block_history_notice_label.clone();
        label.set_halign(Align::Start);
        label.set_hexpand(true);
        // One line, shortened in the middle: a notice bar must not grow the
        // header when the window is narrow. The whole reason is in the log.
        label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        label.set_xalign(0.0);
        bar.append(&label);

        let retry = Button::with_label("Retry");
        retry.add_css_class("suggested-action");
        retry.set_tooltip_text(Some("Reload and save this window's Block history again"));
        bar.append(&retry);

        let dismiss = Button::from_icon_name("window-close-symbolic");
        dismiss.add_css_class("flat");
        dismiss.set_tooltip_text(Some("Hide until the next failure"));
        dismiss.update_property(&[gtk4::accessible::Property::Label(
            "Hide Block history failure notice",
        )]);
        bar.append(&dismiss);

        {
            let ui = Rc::clone(self);
            retry.connect_clicked(move |_| ui.retry_block_history());
        }
        {
            let bar = bar.clone();
            dismiss.connect_clicked(move |_| bar.set_visible(false));
        }
        bar
    }

    /// Raise the bar for a Block-history persistence failure.
    ///
    /// The newest reason replaces an older one rather than queueing behind it:
    /// every pane in this window shares one file family, and a stale reason
    /// would send the user after a problem that has already been superseded.
    pub(crate) fn show_block_history_failure(&self, reason: &str) {
        let reason = jterm_core::review_input::safe_inline_display(reason, 2 * 1024);
        log::error!("Block history is not being saved: {reason}");
        self.block_history_notice_label
            .set_text(&format!("Block history was not saved: {reason}"));
        self.block_history_notice.set_visible(true);
    }

    /// Answer the bar: ask every Block pane in this window to try again.
    ///
    /// The bar is hidden optimistically. Nothing here reports success, and
    /// nothing should: the retry is asynchronous, and the only honest signal
    /// that it did not work is the next failure, which raises the bar again
    /// through the same path that raised it the first time.
    pub(crate) fn retry_block_history(&self) {
        self.block_history_notice.set_visible(false);
        for page in 0..self.notebook.n_pages() {
            let Some(widget) = self.notebook.nth_page(Some(page)) else {
                continue;
            };
            let Some(node) = PaneNode::from_widget(&widget) else {
                continue;
            };
            for leaf in node.leaves() {
                let Some(view) = leaf.block_view() else {
                    continue;
                };
                if let Err(error) = view.retry_history_persistence() {
                    // A synchronous refusal is itself a fail-closed state, and
                    // it is the one the user just asked about. Put it straight
                    // back on the bar instead of letting an empty bar imply the
                    // retry was accepted.
                    self.show_block_history_failure(&error.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{persistence_failure_surface, PersistenceFailureSurface};

    /// The two fail-closed operations must not fall through to the toast, and
    /// they must not collide with each other. The other operations still get a
    /// toast, because a retry of those happens by itself.
    #[test]
    fn only_the_fail_closed_operations_get_a_surface_that_waits() {
        assert_eq!(
            persistence_failure_surface(super::super::CONFIG_PERSIST_OPERATION),
            PersistenceFailureSurface::ConfigDialog
        );
        assert_eq!(
            persistence_failure_surface(crate::block_view::BLOCK_HISTORY_PERSIST_OPERATION),
            PersistenceFailureSurface::BlockHistoryBar
        );
        assert_ne!(
            super::super::CONFIG_PERSIST_OPERATION,
            crate::block_view::BLOCK_HISTORY_PERSIST_OPERATION
        );
        for routine in [
            "Load Block history",
            "Save window session",
            "Save AI conversation",
            "",
        ] {
            assert_eq!(
                persistence_failure_surface(routine),
                PersistenceFailureSurface::Toast,
                "{routine}"
            );
        }
    }
}
