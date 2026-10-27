//! One background workflow library shared by both native palettes.
use super::UiState;
use adw::prelude::*;
use gtk4::glib;
use libadwaita as adw;
use std::sync::Arc;
use std::time::Duration;

impl UiState {
    pub(super) fn refresh_workflow_library(&self) {
        // Directory policy is frontend-owned; disk access and parsing run on
        // the bounded shared worker, never while presenting a GTK dialog.
        let dirs = crate::workflows::workflow_dirs();
        self.workflow_library
            .refresh(move || jterm_core::workflows::load_all(&dirs, crate::workflows::LOAD_ORDER));
    }

    pub(super) fn watch_workflow_library(
        &self,
        dialog: &adw::Dialog,
        status: &gtk4::Label,
        initial: Arc<Vec<crate::workflows::Workflow>>,
        refresh: impl Fn(&[crate::workflows::Workflow]) + 'static,
    ) {
        let library = self.workflow_library.clone();
        let dialog = dialog.downgrade();
        let status = status.downgrade();
        let mut displayed = initial;
        let mut revision = None;
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let (Some(dialog), Some(status)) = (dialog.upgrade(), status.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            if dialog.parent().is_none() {
                return glib::ControlFlow::Break;
            }
            let state = library.snapshot();
            if revision == Some(state.revision) {
                return glib::ControlFlow::Continue;
            }
            revision = Some(state.revision);
            let message = if state.loading {
                "Refreshing workflows…"
            } else if state.error.is_some() {
                "Workflows could not refresh. Previous results remain available."
            } else if state.value.is_empty() {
                "No workflows yet. Add a TOML or YAML file to your workflows directory."
            } else {
                ""
            };
            status.set_label(message);
            status.set_visible(!message.is_empty());
            // Revisions also describe loading/error transitions. Only a new
            // successful library changes rows; unchanged refreshes leave the
            // user's keyboard selection exactly where it was.
            if !state.loading && *displayed != *state.value {
                refresh(&state.value);
                displayed = state.value;
            }
            glib::ControlFlow::Continue
        });
    }
}
