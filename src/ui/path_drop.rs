//! A drop owns one short-lived, review-only delivery intent. No GTK object is
//! sent to the validator and pending work never owns a pane/PTY strongly.
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use vte4::TerminalExt;

use super::PaneLeaf;
use crate::image_drop::PathValidation;

#[derive(Clone, Default)]
pub(crate) struct DropIntent(Rc<Cell<u64>>);

impl DropIntent {
    pub(crate) fn invalidate(&self) {
        self.0.set(self.0.get().wrapping_add(1));
    }

    pub(crate) fn install(leaf: &PaneLeaf) -> Self {
        let intent = Self::default();
        let key = gtk4::EventControllerKey::new();
        key.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let cancel = intent.clone();
        key.connect_key_pressed(move |_, _, _, _| {
            cancel.invalidate();
            gtk4::glib::Propagation::Proceed
        });
        leaf.root_widget().add_controller(key);
        let focus = gtk4::EventControllerFocus::new();
        let cancel = intent.clone();
        focus.connect_leave(move |_| cancel.invalidate());
        leaf.root_widget().add_controller(focus);
        let cancel = intent.clone();
        leaf.terminal()
            .connect_commit(move |_, _, _| cancel.invalidate());
        let cancel = intent.clone();
        leaf.terminal().connect_has_focus_notify(move |terminal| {
            if !terminal.has_focus() {
                cancel.invalidate();
            }
        });
        intent
    }
}

pub(crate) struct DropDestination {
    root: gtk4::glib::WeakRef<gtk4::Widget>,
    page: gtk4::glib::WeakRef<gtk4::Widget>,
    focused_widget: gtk4::glib::WeakRef<gtk4::Widget>,
    notebook: gtk4::glib::WeakRef<gtk4::Notebook>,
    session: Option<String>,
    process: (i32, i32),
    focus_serial: u64,
    input_generation: Option<(u64, u64)>,
    tab_generation: Rc<Cell<u64>>,
    tab_generation_at_drop: u64,
    intent: DropIntent,
    intent_at_drop: u64,
}

impl DropDestination {
    pub(crate) fn capture(
        leaf: &PaneLeaf,
        notebook: &gtk4::Notebook,
        tab_generation: Rc<Cell<u64>>,
        intent: &DropIntent,
    ) -> Option<Self> {
        let page = notebook.nth_page(notebook.current_page())?;
        if !super::widget_is_within(&leaf.root_widget(), &page) {
            return None;
        }
        // This focus change belongs to the user's explicit drop, not a delayed
        // result. Subsequent typing, focus loss, or a newer drop cancels it.
        // Do not use the generic pane focus helper here: its deferred fallback
        // could focus this terminal later even if this capture is rejected.
        if !leaf.terminal().grab_focus() {
            return None;
        }
        let focused_widget = leaf.root_widget().root()?.focus()?;
        if focused_widget != *leaf.terminal().upcast_ref::<gtk4::Widget>() {
            return None;
        }
        intent.invalidate();
        Some(Self {
            root: leaf.root_widget().downgrade(),
            page: page.downgrade(),
            focused_widget: focused_widget.downgrade(),
            notebook: notebook.downgrade(),
            session: leaf.session_id(),
            process: leaf.process_probe(),
            focus_serial: leaf.focus_serial(),
            input_generation: leaf.block_view().map(|view| view.path_drop_generation()),
            tab_generation_at_drop: tab_generation.get(),
            tab_generation,
            intent: intent.clone(),
            intent_at_drop: intent.0.get(),
        })
    }

    fn current_leaf(&self) -> Option<PaneLeaf> {
        if self.intent.0.get() != self.intent_at_drop
            || self.tab_generation.get() != self.tab_generation_at_drop
        {
            return None;
        }
        let root = self.root.upgrade()?;
        let page = self.page.upgrade()?;
        let notebook = self.notebook.upgrade()?;
        if !root.is_mapped()
            || notebook.nth_page(notebook.current_page()).as_ref() != Some(&page)
            || !super::widget_is_within(&root, &page)
        {
            return None;
        }
        let leaf = PaneLeaf::from_widget(&root)?;
        if root.root()?.focus()? != self.focused_widget.upgrade()? {
            return None;
        }
        if leaf.session_id() != self.session
            || leaf.process_probe() != self.process
            || leaf.focus_serial() != self.focus_serial
            || leaf.block_view().map(|view| view.path_drop_generation()) != self.input_generation
        {
            return None;
        }
        Some(leaf)
    }

    pub(crate) fn wait(self, validation: PathValidation, report: impl Fn(&str) + 'static) {
        gtk4::glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(leaf) = self.current_leaf() else {
                // Dropping the receiver cancels further checks; a blocked stat
                // still owns its global permit until the kernel returns.
                return gtk4::glib::ControlFlow::Break;
            };
            let Some(result) = validation.poll() else {
                return gtk4::glib::ControlFlow::Continue;
            };
            let result = result
                .map_err(|error| error.to_string())
                .and_then(|payload| match leaf.block_view() {
                    Some(view) => view.paste_text(&payload),
                    None => leaf.write_review_input(&payload),
                });
            if let Err(error) = result {
                report(&error);
            }
            gtk4::glib::ControlFlow::Break
        });
    }
}
