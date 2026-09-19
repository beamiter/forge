//! Feed-hold that keeps a live-VTE text selection alive while output streams.
//!
//! VTE drops a selection the moment the cells under it change. TUI-style
//! programs (claude, spinners, progress bars) repaint the live surface many
//! times per second, so a pointer selection made while a command was running
//! used to be destroyed before it could ever be copied. The fix sits upstream
//! of VTE: while the user drags a selection over the live VTE, and for as long
//! as that selection remains visible, incoming PTY chunks are parked instead
//! of being processed. Copying, typing (a click or wheel aimed at the program
//! counts; the reports libvte sends by itself do not), clearing the selection,
//! or the bounded safety cap resumes the feed. A flush replays the parked
//! bytes through the exact pipeline they were intercepted from, in order, so
//! nothing is lost or reordered; display is merely deferred.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use jterm_core::terminal_report::classify_terminal_report;
use vte4::TerminalExt;

use super::{BlockState, MouseReportingMode};

/// Replay sink for parked bytes, installed by the PTY reader.
type FlushFn = Box<dyn Fn(Vec<u8>)>;

/// Observer for the parked/live indicator, installed by the view.
type StateFn = Box<dyn Fn(bool)>;

/// Hard cap on parked bytes. A hold that accumulates more than this flushes
/// immediately — the selection is sacrificed — so a firehose command can
/// neither balloon RSS nor stall the block state machine unboundedly.
const MAX_PARKED_BYTES: usize = 2 * 1024 * 1024;

/// A live-VTE selection can only survive streaming output while the stream is
/// what destroys it: states where the child owns the surface and repaints.
/// With mouse reporting active the drag belongs to the application, not to
/// VTE's local selection, so parking the feed would only delay the app's own
/// response to its mouse events — except under Shift, which VTE reserves for
/// forcing a local selection over a mouse-reporting app.
pub(crate) fn feed_hold_eligible(
    state: BlockState,
    mouse: MouseReportingMode,
    shift_held: bool,
) -> bool {
    let streaming = matches!(
        state,
        BlockState::CollectingOutput
            | BlockState::PostCommand
            | BlockState::AltScreen
            | BlockState::RawFallback
    );
    streaming && (mouse == MouseReportingMode::None || shift_held)
}

/// Whether a live-VTE `commit` counts as the user acting on the program, which
/// is what releases a hold.
///
/// Not every commit is the user. libvte also commits reports of its own: a
/// focus report under DECSET 1004 on every window (de)activation, a motion
/// report under 1003 on every pointer move, and its answers to the queries a
/// program sent. Counted as typing, the Alt+Tab to paste a selection
/// elsewhere, or the first pointer move after a Shift+drag over claude's
/// fullscreen UI, released the hold and the next repaint wiped the very
/// selection it was protecting. Those reports now leave it parked. Keys,
/// clicks and wheel reports still release it: they are the user acting on the
/// program, whose response must not stay parked behind the selection.
pub(crate) fn commit_releases_hold(commit: &[u8], cpr_outstanding: bool) -> bool {
    !classify_terminal_report(commit, cpr_outstanding)
        .is_some_and(|report| report.is_passive() || report.is_reply())
}

pub(crate) struct SelectionFeedHold {
    /// True while PTY chunks are being parked instead of processed.
    holding: Cell<bool>,
    /// True between drag-begin and drag-end of the pointer gesture that
    /// started the hold. Selection-changed events during the drag must not
    /// flush: VTE clears the old selection on press before growing the new one.
    dragging: Cell<bool>,
    parked: RefCell<Vec<u8>>,
    flush_cb: RefCell<Option<FlushFn>>,
    /// Fires with `true` when the first chunk is actually parked (not on
    /// every drag — a plain click must not flash the indicator) and `false`
    /// when the hold releases.
    state_cb: RefCell<Option<StateFn>>,
}

impl SelectionFeedHold {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            holding: Cell::new(false),
            dragging: Cell::new(false),
            parked: RefCell::new(Vec::new()),
            flush_cb: RefCell::new(None),
            state_cb: RefCell::new(None),
        })
    }

    /// Wire the replay path. Called once by the reader installer, which owns
    /// the per-chunk pipeline the parked bytes must flow back through.
    pub(crate) fn set_flush(&self, flush: impl Fn(Vec<u8>) + 'static) {
        *self.flush_cb.borrow_mut() = Some(Box::new(flush));
    }

    /// Wire the paused-output indicator. Called once by the view.
    pub(crate) fn set_state_listener(&self, listener: impl Fn(bool) + 'static) {
        *self.state_cb.borrow_mut() = Some(Box::new(listener));
    }

    fn notify_state(&self, parked: bool) {
        if let Some(cb) = self.state_cb.borrow().as_ref() {
            cb(parked);
        }
    }

    /// The VTE-side triggers that end a hold early: the selection being
    /// cleared (click elsewhere, copy paths that unselect) and the user
    /// typing (frozen output under live input reads as a hang).
    ///
    /// `cpr_outstanding` is the pane's count of cursor position queries left
    /// to the VTE, which is what tells its CPR answer from Shift+F3; see
    /// [`commit_releases_hold`].
    pub(crate) fn install_vte_hooks(
        self: &Rc<Self>,
        vte: &vte4::Terminal,
        cpr_outstanding: Rc<Cell<u32>>,
    ) {
        let weak = Rc::downgrade(self);
        vte.connect_selection_changed(move |vte| {
            if let Some(hold) = weak.upgrade() {
                if !vte.has_selection() {
                    hold.selection_cleared();
                }
            }
        });
        let weak = Rc::downgrade(self);
        vte.connect_commit(move |_, text, _| {
            if let Some(hold) = weak.upgrade() {
                if commit_releases_hold(text.as_bytes(), cpr_outstanding.get() > 0) {
                    hold.flush_now();
                }
            }
        });
    }

    /// A selection drag over the live VTE has started; park the feed.
    /// Idempotent — a drag that grows into the live VTE calls this per motion.
    pub(crate) fn begin_drag(&self) {
        self.dragging.set(true);
        self.holding.set(true);
    }

    /// The drag ended. A surviving selection keeps ownership until copy,
    /// typing, explicit clearing, or the bounded-buffer safety cap.
    pub(crate) fn end_drag(self: &Rc<Self>, selection_alive: bool) {
        if !self.dragging.replace(false) {
            return;
        }
        if !self.holding.get() {
            return;
        }
        if !selection_alive {
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
        let (first_park, overflow) = {
            let mut parked = self.parked.borrow_mut();
            let was_empty = parked.is_empty();
            parked.extend_from_slice(data);
            (
                was_empty && !parked.is_empty(),
                parked.len() > MAX_PARKED_BYTES,
            )
        };
        if first_park {
            self.notify_state(true);
        }
        if overflow {
            self.flush_now();
        }
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
        if !self.holding.replace(false) {
            return;
        }
        let parked = std::mem::take(&mut *self.parked.borrow_mut());
        if parked.is_empty() {
            return;
        }
        self.notify_state(false);
        if let Some(flush) = self.flush_cb.borrow().as_ref() {
            flush(parked);
        }
    }

    /// Run an irreversible follow-up only after parked bytes have re-entered
    /// the normal parser/VTE pipeline. PTY exit and direct process-control
    /// writes use this boundary so neither teardown nor new input can overtake
    /// the tail that was held for a selection.
    pub(crate) fn flush_then(&self, action: impl FnOnce()) {
        self.flush_now();
        action();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{commit_releases_hold, feed_hold_eligible, SelectionFeedHold, MAX_PARKED_BYTES};
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
            MouseReportingMode::None,
            false
        ));
        assert!(feed_hold_eligible(
            BlockState::AltScreen,
            MouseReportingMode::None,
            false
        ));
        assert!(feed_hold_eligible(
            BlockState::RawFallback,
            MouseReportingMode::None,
            false
        ));
        assert!(!feed_hold_eligible(
            BlockState::Idle,
            MouseReportingMode::None,
            false
        ));
        assert!(!feed_hold_eligible(
            BlockState::AwaitingCommand,
            MouseReportingMode::None,
            false
        ));
        assert!(!feed_hold_eligible(
            BlockState::CollectingOutput,
            MouseReportingMode::Sgr,
            false
        ));
        // Shift forces VTE's local selection over a mouse-reporting app; the
        // hold must protect that selection too.
        assert!(feed_hold_eligible(
            BlockState::CollectingOutput,
            MouseReportingMode::Sgr,
            true
        ));
        assert!(!feed_hold_eligible(
            BlockState::Idle,
            MouseReportingMode::Sgr,
            true
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
    fn surviving_selection_keeps_feed_parked_until_copy_or_input() {
        let (hold, log) = hold_with_log();
        hold.begin_drag();
        assert!(hold.try_buffer(b"codex redraw"));
        hold.end_drag(true);
        assert!(hold.try_buffer(b" stays parked"));
        assert!(log.borrow().is_empty());
        hold.flush_now();
        assert_eq!(
            log.borrow().as_slice(),
            [b"codex redraw stays parked".to_vec()]
        );
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

    #[test]
    fn reports_the_vte_sends_by_itself_do_not_release_the_hold() {
        // Focus reports (Alt+Tab away to paste the selection), bare pointer
        // motion under DECSET 1003, and every answer to a query.
        for passive in [
            &b"\x1b[O"[..],
            b"\x1b[I",
            b"\x1b[<35;10;5M",
            b"\x1b[?61;1;21;22c",
            b"\x1b[>61;7600;1c",
            b"\x1b[?2026;4$y",
            b"\x1bP>|VTE(7600)\x1b\\",
            b"\x1b[0n",
        ] {
            assert!(!commit_releases_hold(passive, false), "{passive:?}");
        }
        // A CPR answer only while one is owed; otherwise it is Shift+F3.
        assert!(!commit_releases_hold(b"\x1b[3;1R", true));
        assert!(commit_releases_hold(b"\x1b[1;2R", false));

        // The user acting on the program: keys, text, clicks and the wheel.
        for acting in [
            &b"a"[..],
            b"\r",
            b"\x1b",
            b"\x1b[A",
            b"\x1b[Z",
            b"\x1b[<0;1;1M",
            b"\x1b[<0;1;1m",
            b"\x1b[<32;3;4M",
            b"\x1b[<65;10;20M",
        ] {
            assert!(commit_releases_hold(acting, false), "{acting:?}");
        }
    }

    #[test]
    fn a_focus_or_motion_report_keeps_the_feed_parked_and_a_key_releases_it() {
        let (hold, log) = hold_with_log();
        hold.begin_drag();
        assert!(hold.try_buffer(b"codex redraw"));
        hold.end_drag(true);
        // What the commit hook does per commit.
        let commit = |bytes: &[u8]| {
            if commit_releases_hold(bytes, false) {
                hold.flush_now();
            }
        };
        commit(b"\x1b[O");
        commit(b"\x1b[<35;10;5M");
        assert!(hold.try_buffer(b" still parked"));
        assert!(log.borrow().is_empty());
        commit(b"a");
        assert_eq!(
            log.borrow().as_slice(),
            [b"codex redraw still parked".to_vec()]
        );
    }

    #[test]
    fn flush_then_orders_replay_before_exit_or_control_action() {
        let hold = SelectionFeedHold::new();
        let order = Rc::new(RefCell::new(Vec::new()));
        let replay_order = order.clone();
        hold.set_flush(move |_| replay_order.borrow_mut().push("replay"));
        hold.begin_drag();
        assert!(hold.try_buffer(b"tail"));

        let action_order = order.clone();
        hold.flush_then(move || action_order.borrow_mut().push("exit-or-control"));

        assert_eq!(order.borrow().as_slice(), ["replay", "exit-or-control"]);
    }
}
