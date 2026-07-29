//! Feed-hold that keeps a live-VTE text selection alive while output streams.
//!
//! VTE drops a selection the moment the cells under it change. TUI-style
//! programs (claude, spinners, progress bars) repaint the live surface many
//! times per second, so a pointer selection made while a command was running
//! used to be destroyed before it could ever be copied. The fix sits upstream
//! of VTE: while the user drags a selection over the live VTE — and for a
//! short grace period afterwards, while that selection still exists — incoming
//! PTY chunks are parked here instead of being processed. A flush replays the
//! parked bytes through the exact pipeline they were intercepted from, in
//! order, so nothing is lost or reordered; display is merely deferred.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::glib;
use vte4::TerminalExt;

use super::{BlockState, MouseReportingMode};

/// Replay sink for parked bytes, installed by the PTY reader.
type FlushFn = Box<dyn Fn(Vec<u8>)>;

/// Hard cap on parked bytes. A hold that accumulates more than this flushes
/// immediately — the selection is sacrificed — so a firehose command can
/// neither balloon RSS nor stall the block state machine unboundedly.
const MAX_PARKED_BYTES: usize = 2 * 1024 * 1024;

/// How long a finished drag may keep the feed parked while its selection is
/// still alive — the window for pressing Ctrl+Shift+C. Copying, typing,
/// clearing the selection, or this timeout all resume the feed.
const RELEASE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// A live-VTE selection can only survive streaming output while the stream is
/// what destroys it: states where the child owns the surface and repaints.
/// With mouse reporting active the drag belongs to the application, not to
/// VTE's local selection, so parking the feed would only delay the app's own
/// response to its mouse events.
pub(crate) fn feed_hold_eligible(state: BlockState, mouse: MouseReportingMode) -> bool {
    let streaming = matches!(
        state,
        BlockState::CollectingOutput
            | BlockState::PostCommand
            | BlockState::AltScreen
            | BlockState::RawFallback
    );
    streaming && mouse == MouseReportingMode::None
}

pub(crate) struct SelectionFeedHold {
    /// True while PTY chunks are being parked instead of processed.
    holding: Cell<bool>,
    /// True between drag-begin and drag-end of the pointer gesture that
    /// started the hold. Selection-changed events during the drag must not
    /// flush: VTE clears the old selection on press before growing the new one.
    dragging: Cell<bool>,
    parked: RefCell<Vec<u8>>,
    grace_timer: RefCell<Option<glib::SourceId>>,
    flush_cb: RefCell<Option<FlushFn>>,
}

impl SelectionFeedHold {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            holding: Cell::new(false),
            dragging: Cell::new(false),
            parked: RefCell::new(Vec::new()),
            grace_timer: RefCell::new(None),
            flush_cb: RefCell::new(None),
        })
    }

    /// Wire the replay path. Called once by the reader installer, which owns
    /// the per-chunk pipeline the parked bytes must flow back through.
    pub(crate) fn set_flush(&self, flush: impl Fn(Vec<u8>) + 'static) {
        *self.flush_cb.borrow_mut() = Some(Box::new(flush));
    }

    /// The VTE-side triggers that end a hold early: the selection being
    /// cleared (click elsewhere, copy paths that unselect) and the user
    /// typing (frozen output under live input reads as a hang).
    pub(crate) fn install_vte_hooks(self: &Rc<Self>, vte: &vte4::Terminal) {
        let weak = Rc::downgrade(self);
        vte.connect_selection_changed(move |vte| {
            if let Some(hold) = weak.upgrade() {
                if !vte.has_selection() {
                    hold.selection_cleared();
                }
            }
        });
        let weak = Rc::downgrade(self);
        vte.connect_commit(move |_, _, _| {
            if let Some(hold) = weak.upgrade() {
                hold.flush_now();
            }
        });
    }

    /// A selection drag over the live VTE has started; park the feed.
    /// Idempotent — a drag that grows into the live VTE calls this per motion.
    pub(crate) fn begin_drag(&self) {
        self.cancel_grace();
        self.dragging.set(true);
        self.holding.set(true);
    }

    /// The drag ended. A surviving selection earns a grace period so the copy
    /// shortcut still has something to read; otherwise resume immediately.
    pub(crate) fn end_drag(self: &Rc<Self>, selection_alive: bool) {
        if !self.dragging.replace(false) {
            return;
        }
        if !self.holding.get() {
            return;
        }
        if selection_alive {
            self.schedule_grace();
        } else {
            self.flush_now();
        }
    }

    /// Park `data` if a hold is active. Returns true when the chunk was
    /// consumed (the caller must not process it); the overflow path has then
    /// already flushed everything, this chunk included, in arrival order.
    pub(crate) fn try_buffer(&self, data: &[u8]) -> bool {
        if !self.holding.get() {
            return false;
        }
        {
            let mut parked = self.parked.borrow_mut();
            parked.extend_from_slice(data);
            if parked.len() <= MAX_PARKED_BYTES {
                return true;
            }
        }
        self.flush_now();
        true
    }

    fn selection_cleared(&self) {
        if self.holding.get() && !self.dragging.get() {
            self.flush_now();
        }
    }

    /// Release the hold and replay the parked bytes through the pipeline.
    /// No-op when no hold is active, so every caller may invoke it blindly.
    pub(crate) fn flush_now(&self) {
        self.cancel_grace();
        if !self.holding.replace(false) {
            return;
        }
        let parked = std::mem::take(&mut *self.parked.borrow_mut());
        if parked.is_empty() {
            return;
        }
        if let Some(flush) = self.flush_cb.borrow().as_ref() {
            flush(parked);
        }
    }

    fn schedule_grace(self: &Rc<Self>) {
        self.cancel_grace();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(RELEASE_GRACE, move || {
            if let Some(hold) = weak.upgrade() {
                hold.grace_timer.borrow_mut().take();
                hold.flush_now();
            }
        });
        *self.grace_timer.borrow_mut() = Some(id);
    }

    fn cancel_grace(&self) {
        if let Some(id) = self.grace_timer.borrow_mut().take() {
            id.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{feed_hold_eligible, SelectionFeedHold, MAX_PARKED_BYTES};
    use crate::block_view::{BlockState, MouseReportingMode};

    type FlushLog = Rc<RefCell<Vec<Vec<u8>>>>;

    fn hold_with_log() -> (Rc<SelectionFeedHold>, FlushLog) {
        let hold = SelectionFeedHold::new();
        let log: FlushLog = Rc::new(RefCell::new(Vec::new()));
        let log_for_flush = log.clone();
        hold.set_flush(move |bytes| log_for_flush.borrow_mut().push(bytes));
        (hold, log)
    }

    #[test]
    fn eligibility_requires_streaming_state_without_mouse_reporting() {
        assert!(feed_hold_eligible(
            BlockState::CollectingOutput,
            MouseReportingMode::None
        ));
        assert!(feed_hold_eligible(
            BlockState::AltScreen,
            MouseReportingMode::None
        ));
        assert!(feed_hold_eligible(
            BlockState::RawFallback,
            MouseReportingMode::None
        ));
        assert!(!feed_hold_eligible(
            BlockState::Idle,
            MouseReportingMode::None
        ));
        assert!(!feed_hold_eligible(
            BlockState::AwaitingCommand,
            MouseReportingMode::None
        ));
        assert!(!feed_hold_eligible(
            BlockState::CollectingOutput,
            MouseReportingMode::Sgr
        ));
    }

    #[test]
    fn parks_bytes_only_while_holding() {
        let (hold, log) = hold_with_log();
        assert!(!hold.try_buffer(b"live"));
        hold.begin_drag();
        assert!(hold.try_buffer(b"parked"));
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn drag_end_without_selection_flushes_in_order() {
        let (hold, log) = hold_with_log();
        hold.begin_drag();
        assert!(hold.try_buffer(b"one "));
        assert!(hold.try_buffer(b"two"));
        hold.end_drag(false);
        assert_eq!(log.borrow().as_slice(), [b"one two".to_vec()]);
        // Hold is over: the feed is live again.
        assert!(!hold.try_buffer(b"after"));
    }

    #[test]
    fn selection_cleared_flushes_only_after_the_drag() {
        let (hold, log) = hold_with_log();
        hold.begin_drag();
        assert!(hold.try_buffer(b"kept"));
        // Press cleared the previous selection mid-drag: must keep parking.
        hold.selection_cleared();
        assert!(log.borrow().is_empty());
        hold.dragging.set(false);
        hold.selection_cleared();
        assert_eq!(log.borrow().as_slice(), [b"kept".to_vec()]);
    }

    #[test]
    fn overflow_releases_the_hold_immediately() {
        let (hold, log) = hold_with_log();
        hold.begin_drag();
        let big = vec![b'x'; MAX_PARKED_BYTES];
        assert!(hold.try_buffer(&big));
        assert!(log.borrow().is_empty());
        // One more byte crosses the cap: everything flushes, chunk included.
        assert!(hold.try_buffer(b"y"));
        assert_eq!(log.borrow().len(), 1);
        assert_eq!(log.borrow()[0].len(), MAX_PARKED_BYTES + 1);
        assert!(!hold.try_buffer(b"live"));
    }

    #[test]
    fn ended_drag_without_hold_is_a_no_op() {
        let (hold, log) = hold_with_log();
        hold.end_drag(true);
        hold.flush_now();
        assert!(log.borrow().is_empty());
        assert!(!hold.try_buffer(b"live"));
    }
}
