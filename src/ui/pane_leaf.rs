//! Typed terminal-pane leaf shared by Block and conventional VTE tabs.
//!
//! The GTK widget tree is a rendering detail; callers should not need to know
//! whether a pane root owns a structured `TermView` or a conventional
//! `VteTerminalView` just to get its live terminal, focus it, or retain the
//! controller on a widget. This is the first step toward a native Block-pane
//! model without introducing Relm4 or enabling unsafe Block splits prematurely.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gtk4::glib::prelude::ObjectExt;
use gtk4::prelude::*;
use vte4::Terminal;

use crate::block_view::TermView;
use crate::terminal::{focus_terminal, VteTerminalView};

const PANE_LEAF_DATA_KEY: &str = "terminal-view-type";
const PANE_SESSION_ID_DATA_KEY: &str = "terminal-session-id";
const PANE_REMOTE_DATA_KEY: &str = "terminal-remote-pane";
const PANE_FOCUS_SERIAL_DATA_KEY: &str = "terminal-pane-focus-serial";
static NEXT_PANE_FOCUS_SERIAL: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) enum PaneLeaf {
    Block(Rc<TermView>),
    Vte(Rc<VteTerminalView>),
}

impl PaneLeaf {
    /// Root widget inserted into a Notebook page or Paned leaf.
    pub(crate) fn root_widget(&self) -> gtk4::Widget {
        match self {
            Self::Block(view) => view.widget(),
            Self::Vte(view) => view.widget(),
        }
    }

    /// The live, input-capable VTE owned by this pane.
    ///
    /// Finished Block snapshots contain additional read-only VTE widgets, so
    /// generic widget-tree traversal cannot reliably identify this terminal.
    pub(crate) fn terminal(&self) -> &Terminal {
        match self {
            Self::Block(view) => view.vte(),
            Self::Vte(view) => view.vte(),
        }
    }

    /// This pane's status strip, shown only while its tab is split.
    pub(crate) fn pane_header(&self) -> &super::PaneHeader {
        match self {
            Self::Block(view) => view.pane_header(),
            Self::Vte(view) => view.pane_header(),
        }
    }

    /// Wire this pane for rearranging: its header becomes a drag handle
    /// carrying the pane's session id, and the whole leaf becomes a drop zone.
    ///
    /// Session ids, not indices or tab numbers, cross the drag boundary: both
    /// of those shift when an unrelated pane or tab closes mid-drag.
    /// The drag closure resolves the leaf from a weak root reference instead of
    /// capturing it. A strong capture would form
    /// `root -> controller -> PaneLeaf -> view -> root` and pin the pane's PTY
    /// and scrollback for the life of the process.
    pub(crate) fn install_pane_drag(&self, on_drop: impl Fn(&str) -> bool + 'static) {
        let root = self.root_widget();
        let source_root = root.downgrade();
        self.pane_header()
            .install_drag_source(move || Self::from_widget(&source_root.upgrade()?)?.session_id());
        super::pane_header::install_pane_drop_target(&root, on_drop);
    }

    /// Focus the actual live surface rather than the first VTE found in the tree.
    pub(crate) fn grab_focus(&self) {
        self.mark_active();
        match self {
            Self::Block(view) => view.grab_focus(),
            Self::Vte(view) => focus_terminal(view.vte()),
        }
    }

    /// Whether `widget` is this leaf root or one of its descendants.
    ///
    /// Block panes contain focusable finished-block VTEs and other controls in
    /// addition to their live input VTE. Directional pane navigation must treat
    /// focus on any of those descendants as focus belonging to the Block pane.
    pub(crate) fn owns_widget(&self, widget: &gtk4::Widget) -> bool {
        let root = self.root_widget();
        *widget == root || widget.is_ancestor(&root)
    }

    pub(crate) fn focus_serial(&self) -> u64 {
        let root = self.root_widget();
        unsafe {
            root.data::<std::cell::Cell<u64>>(PANE_FOCUS_SERIAL_DATA_KEY)
                .map(|serial| serial.as_ref().get())
                .unwrap_or(0)
        }
    }

    fn mark_active(&self) {
        let serial = NEXT_PANE_FOCUS_SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = self.root_widget();
        unsafe {
            if let Some(stored) = root.data::<std::cell::Cell<u64>>(PANE_FOCUS_SERIAL_DATA_KEY) {
                stored.as_ref().set(serial);
            }
        }
    }

    /// Insert bytes into the pane's live input surface without submitting a
    /// trailing newline. Palettes and workflows use this review-first path for
    /// both terminal backends.
    #[must_use = "Block-mode PTY input may be rejected by bounded backpressure"]
    pub(crate) fn write_input(&self, data: &[u8]) -> Result<(), crate::pty::PtyWriteError> {
        match self {
            Self::Block(view) => view.write_input(data),
            Self::Vte(view) => {
                view.write_input(data);
                Ok(())
            }
        }
    }

    /// Insert one command for review without submitting it. Unlike the generic
    /// input path, this rejects all terminal control characters so a history,
    /// workflow, file name, or model response cannot smuggle Enter into the PTY.
    pub(crate) fn write_review_input(&self, text: &str) -> Result<(), String> {
        let text = crate::review_input::validate(text).map_err(|error| error.to_string())?;
        self.write_input(text.as_bytes())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn block_view(&self) -> Option<Rc<TermView>> {
        match self {
            Self::Block(view) => Some(view.clone()),
            Self::Vte(_) => None,
        }
    }

    pub(crate) fn is_block(&self) -> bool {
        matches!(self, Self::Block(_))
    }

    /// `(pty master fd, shell pid)` used for foreground-process inspection.
    /// Block mode owns an `OwnedPty`, while conventional mode lets VTE own it;
    /// keeping that distinction here prevents feature code from accidentally
    /// asking the Block live VTE for a PTY it does not own.
    pub(crate) fn process_probe(&self) -> (i32, i32) {
        match self {
            Self::Block(view) => (view.pty_fd_i32(), view.pid_i32()),
            Self::Vte(view) => (view.pty_fd_i32(), view.pid_i32()),
        }
    }

    /// Persist the per-pane session identity on the actual leaf root. Split
    /// descendants cannot all be represented by the tab-number keyed map.
    pub(crate) fn set_session_id(&self, session_id: &str) {
        unsafe {
            self.root_widget()
                .set_data::<String>(PANE_SESSION_ID_DATA_KEY, session_id.to_owned());
        }
    }

    pub(crate) fn session_id(&self) -> Option<String> {
        unsafe {
            self.root_widget()
                .data::<String>(PANE_SESSION_ID_DATA_KEY)
                .map(|value| value.as_ref().clone())
        }
    }

    /// Mark the process hosted by this exact leaf as a remote connection.
    ///
    /// A tab may later be split, at which point tab-level connection metadata is
    /// no longer enough to tell the remote primary from a local sibling. Keeping
    /// the bit on the leaf lets structural operations avoid moving a controller
    /// whose reconnect callbacks are intentionally bound to its original tab.
    pub(crate) fn set_remote(&self, remote: bool) {
        unsafe {
            self.root_widget()
                .set_data::<bool>(PANE_REMOTE_DATA_KEY, remote);
        }
    }

    pub(crate) fn is_remote(&self) -> bool {
        unsafe {
            self.root_widget()
                .data::<bool>(PANE_REMOTE_DATA_KEY)
                .is_some_and(|value| *value.as_ref())
        }
    }

    pub(crate) fn restorable_command(&self) -> Option<Vec<String>> {
        let (pty_fd, shell_pid) = self.process_probe();
        crate::process::restorable_command(pty_fd, shell_pid)
    }

    pub(crate) fn foreground_process_name(&self) -> Option<String> {
        let (pty_fd, shell_pid) = self.process_probe();
        crate::process::foreground_process_name(pty_fd, shell_pid)
            .map(|name| crate::review_input::safe_inline_display(&name, 256))
    }

    /// Terminate this leaf's shell and everything in its PTY session through
    /// the backend-neutral process teardown path.
    ///
    /// Each backend terminates its own child lifecycle, which knows who reaps
    /// that child: forge for a Block pane's forked PTY, VTE's glib child
    /// watch for a conventional pane. Repeated calls are no-ops, so a pane
    /// closed explicitly and then dropped tears down only once.
    pub(crate) fn kill(&self) {
        match self {
            // Block mode also owns a dedicated PTY input worker. Route teardown
            // through TermView so the channel and cloned master fd close before
            // the escalation ladder starts.
            Self::Block(view) => view.kill(),
            Self::Vte(view) => view.kill(),
        }
    }

    /// Store the typed controller on its GTK leaf root. Keeping the unsafe GTK
    /// object-data boundary here prevents feature code from repeating raw pointer
    /// access and accidentally using different data keys or types.
    pub(crate) fn attach_to(&self, widget: &gtk4::Widget) {
        unsafe {
            widget.set_data::<Self>(PANE_LEAF_DATA_KEY, self.clone());
            widget.set_data::<std::cell::Cell<u64>>(
                PANE_FOCUS_SERIAL_DATA_KEY,
                std::cell::Cell::new(0),
            );
        }
        let root = widget.downgrade();
        self.terminal().connect_has_focus_notify(move |terminal| {
            if terminal.has_focus() {
                let Some(root) = root.upgrade() else {
                    return;
                };
                let serial = NEXT_PANE_FOCUS_SERIAL.fetch_add(1, Ordering::Relaxed);
                unsafe {
                    if let Some(stored) =
                        root.data::<std::cell::Cell<u64>>(PANE_FOCUS_SERIAL_DATA_KEY)
                    {
                        stored.as_ref().set(serial);
                    }
                }
            }
        });
    }

    /// Release the strong controller stored on a leaf root before that widget is
    /// permanently removed from the pane tree.
    ///
    /// The root is also owned by the controller, so leaving the qdata attached
    /// after unparenting would form `root -> PaneLeaf -> controller -> root` and
    /// keep the PTY, callbacks, and finished Block widgets alive indefinitely.
    /// Temporary reparenting (split moves and zoom) must not call this method.
    pub(crate) fn detach_from(widget: &gtk4::Widget) -> Option<Self> {
        unsafe { widget.steal_data::<Self>(PANE_LEAF_DATA_KEY) }
    }

    /// Recover a directly attached pane leaf from a GTK root widget.
    ///
    /// Split roots are `Paned` containers and intentionally return `None`; focus
    /// and navigation code can then select a concrete child leaf geometrically.
    pub(crate) fn from_widget(widget: &gtk4::Widget) -> Option<Self> {
        unsafe {
            widget
                .data::<Self>(PANE_LEAF_DATA_KEY)
                .map(|pointer| pointer.as_ref().clone())
        }
    }
}
