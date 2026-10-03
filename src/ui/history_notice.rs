//! Persistent surface for Block-history fail-closed states.
//!
//! A Block-history save can refuse for reasons that stay true until somebody
//! acts: the file's revision moved under this window, the load it must not
//! overwrite failed, the volume is full, the advisory lock never cleared.
//! Those used to arrive as the same eight-second toast every other persistence
//! failure gets, so the one class of failure that *needs* a decision was the
//! class most likely to be missed — a toast that has already faded is not a
//! decision. This bar stays until it is answered, and it carries the answer.
//!
//! Retry is ReloadFirst when this pane's load Failed (saving again would only
//! refuse again), else SaveAgain. An Explicit Clear answers a Failed load on
//! its own: the worker writes HistoryWriteIntent::ExplicitReplace and does not
//! consult the Failed outcome — Clear parking must not revive the overwrite
//! refusal. Anvil mirrors that bypass on its sync arm path (upgrade rounds
//! 75/78/80 → forge 78 sticky story).

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

    /// Near-miss labels must not raise the sticky bar — the surface is keyed
    /// on the exact `"Save Block history"` operation string workers enqueue.
    #[test]
    fn near_miss_block_history_labels_stay_on_the_toast_surface() {
        for near_miss in [
            "Save Block history ",
            " Save Block history",
            "save Block history",
            "Save Block History",
            "Save block history",
            "Save Block histor",
            "Save Block history\n",
            // Whitespace / invisible catch-up beside anvil round 100–101.
            "Save Block history\t",
            "Save Block history\r",
            "Save\tBlock history",
            "Save Block\u{00a0}history",
            "Save Block\u{200b}history",
            "\u{feff}Save Block history",
            "Save Block history\u{000b}",
            // Soft hyphen / word joiner / bidi / ZWNJ near-misses beside the
            // ZWSP/BOM/VT wave — still toast-only, never sticky.
            "Save Block\u{00ad}history",
            "Save Block\u{2060}history",
            "Save\u{200e} Block history",
            "Save Block\u{200c}history",
            "Save Block\u{2007}history",
            // Line / paragraph separators stay toast-only beside soft-hyphen/WJ.
            "Save Block history\u{2028}",
            "Save Block history\u{2029}",
            "Save Block\u{2028}history",
            // Narrow NBSP / MMSP / invisible math separators stay toast-only.
            "Save Block\u{202f}history",
            "Save Block\u{205f}history",
            "Save Block\u{2062}history",
            "Save Block\u{2063}history",
            "Save Block\u{2064}history",
            // ZWJ / RLM / function-application / ideographic space / NEL stay
            // toast-only beside the NNBSP/math wave.
            "Save Block\u{200d}history",
            "Save\u{200f} Block history",
            "Save Block\u{2061}history",
            "Save Block\u{3000}history",
            "Save Block history\u{0085}",
            // Punctuation / thin / hair spaces + ALM / bidi isolates stay
            // toast-only beside the ZWJ/ideo wave.
            "Save Block\u{2008}history",
            "Save Block\u{2009}history",
            "Save Block\u{200a}history",
            "Save\u{061c} Block history",
            "\u{2066}Save Block history\u{2069}",
            // En/em/three/four/six-per-em spaces + RLI/FSI stay toast-only
            // beside the punct/thin/hair/ALM wave (figure space already pinned).
            "Save Block\u{2000}history",
            "Save Block\u{2001}history",
            "Save Block\u{2002}history",
            "Save Block\u{2003}history",
            "Save Block\u{2004}history",
            "Save Block\u{2005}history",
            "Save Block\u{2006}history",
            "\u{2067}Save Block history\u{2069}",
            "\u{2068}Save Block history\u{2069}",
            // Hangul fillers / Braille blank / deprecated format controls stay
            // toast-only beside the en/em/RLI wave.
            "Save Block\u{115f}history",
            "Save Block\u{1160}history",
            "Save Block\u{3164}history",
            "Save Block\u{ffa0}history",
            "Save Block\u{2800}history",
            "Save Block\u{206a}history",
            "Save Block\u{206f}history",
            // Ogham space / Mongolian vowel separator / CGJ / variation
            // selectors / Khmer inherents stay toast-only beside Hangul.
            "Save Block\u{1680}history",
            "Save Block\u{180e}history",
            "Save Block\u{034f}history",
            "Save Block\u{fe00}history",
            "Save Block\u{fe0e}history",
            "Save Block\u{fe0f}history",
            "Save Block\u{17b4}history",
            "Save Block\u{17b5}history",
            // Mongolian FVS / mid-string ZWNBSP / interlinear annotation
            // anchors stay toast-only beside the Ogham/MVS wave.
            "Save Block\u{180b}history",
            "Save Block\u{180c}history",
            "Save Block\u{180d}history",
            "Save Block\u{feff}history",
            "\u{fff9}Save Block history\u{fffb}",
            "Save\u{fffa} Block history",
            // Bidi embeddings/overrides + remaining deprecated format controls
            // stay toast-only beside FVS/interlinear (206A/206F already pinned).
            "\u{202a}Save Block history\u{202c}",
            "\u{202b}Save Block history\u{202c}",
            "\u{202d}Save Block history\u{202c}",
            "\u{202e}Save Block history\u{202c}",
            "Save Block\u{206b}history",
            "Save Block\u{206c}history",
            "Save Block\u{206d}history",
            "Save Block\u{206e}history",
            // Mid-range VS (FE01/FE0D) + Mongolian FVS4 stay toast-only beside
            // the bidi/deprecated wave (FE00/FE0E/FE0F + FVS1–3 already pinned).
            "Save Block\u{fe01}history",
            "Save Block\u{fe0d}history",
            "Save Block\u{180f}history",
            // Interior mid-range VS (FE02/FE0C) + Mongolian nirugu stay toast-only
            // beside FE01/FE0D/FVS4 (FE00/FE0E/FE0F + FVS1–3 already pinned).
            "Save Block\u{fe02}history",
            "Save Block\u{fe0c}history",
            "Save Block\u{180a}history",
            // Closer mid-range VS (FE03/FE0B) + Mongolian Todo soft hyphen stay
            // toast-only beside FE02/FE0C/nirugu.
            "Save Block\u{fe03}history",
            "Save Block\u{fe0b}history",
            "Save Block\u{1806}history",
            // Inner mid-range VS (FE04/FE0A) + Mongolian syllable boundary stay
            // toast-only beside FE03/FE0B/Todo soft hyphen.
            "Save Block\u{fe04}history",
            "Save Block\u{fe0a}history",
            "Save Block\u{1807}history",
            // Nesting mid-range VS (FE05/FE09) + Mongolian Manchu comma stay
            // toast-only beside FE04/FE0A/syllable boundary.
            "Save Block\u{fe05}history",
            "Save Block\u{fe09}history",
            "Save Block\u{1808}history",
            // Deeper nesting mid-range VS (FE06/FE08) + Mongolian Manchu full
            // stop stay toast-only beside FE05/FE09/Manchu comma.
            "Save Block\u{fe06}history",
            "Save Block\u{fe08}history",
            "Save Block\u{1809}history",
            // Center mid-range VS (FE07) + Mongolian birga stay toast-only
            // beside FE06/FE08/Manchu full stop.
            "Save Block\u{fe07}history",
            "Save Block\u{1800}history",
            // Mongolian ellipsis stays toast-only beside FE07/birga (one edge).
            "Save Block\u{1801}history",
            // Mongolian comma stays toast-only beside ellipsis (one edge).
            "Save Block\u{1802}history",
            // Mongolian full stop stays toast-only beside comma (one edge).
            "Save Block\u{1803}history",
            // Mongolian colon stays toast-only beside full stop (one edge).
            "Save Block\u{1804}history",
            // Mongolian four dots stays toast-only beside colon (one edge).
            "Save Block\u{1805}history",
            // Fullwidth colon stays toast-only beside Mongolian four dots (one edge).
            "Save Block\u{ff1a}history",
            // Fullwidth semicolon stays toast-only beside fullwidth colon (one edge).
            "Save Block\u{ff1b}history",
            // Fullwidth less-than stays toast-only beside fullwidth semicolon (one edge).
            "Save Block\u{ff1c}history",
            // Fullwidth equals stays toast-only beside fullwidth less-than (one edge).
            "Save Block\u{ff1d}history",
            // Fullwidth greater-than stays toast-only beside fullwidth equals (one edge).
            "Save Block\u{ff1e}history",
            // Fullwidth question mark stays toast-only beside fullwidth greater-than (one edge).
            "Save Block\u{ff1f}history",
            // Fullwidth commercial at stays toast-only beside fullwidth question mark (one edge).
            "Save Block\u{ff20}history",
            // Fullwidth latin A stays toast-only beside fullwidth commercial at (one edge).
            "Save Block\u{ff21}history",
            "Block history",
        ] {
            assert_eq!(
                persistence_failure_surface(near_miss),
                PersistenceFailureSurface::Toast,
                "{near_miss:?}"
            );
        }
    }

    /// Sticky Retry hides the bar optimistically; a synchronous
    /// `retry_history_persistence` Err must raise it again immediately so an
    /// empty bar never implies the retry was accepted (pairs anvil).
    #[test]
    fn retry_block_history_reopens_bar_on_sync_refusal() {
        let source = include_str!("history_notice.rs");
        let retry = source
            .split("pub(crate) fn retry_block_history(&self) {")
            .nth(1)
            .expect("retry_block_history")
            .split("\n}\n\n#[cfg(test)]")
            .next()
            .expect("retry closes before tests");
        assert!(
            retry.contains("set_visible(false)")
                && retry.contains("retry_history_persistence()")
                && retry.contains("show_block_history_failure"),
            "optimistic hide must re-show on sync refusal"
        );
    }

    /// Sticky Retry must walk every Block leaf even after a synchronous
    /// refusal — one pane's Err re-shows the bar but must not `break` /
    /// `return` before later leaves also get `retry_history_persistence`.
    #[test]
    fn retry_block_history_continues_after_sync_refusal() {
        let source = include_str!("history_notice.rs");
        let retry = source
            .split("pub(crate) fn retry_block_history(&self) {")
            .nth(1)
            .expect("retry_block_history")
            .split("\n}\n\n#[cfg(test)]")
            .next()
            .expect("retry closes before tests");
        assert!(
            !retry.contains("break;") && !retry.contains("return;"),
            "sync refusal must not short-circuit the leaf walk"
        );
        assert!(
            retry.contains("n_pages()") && retry.contains("leaves()"),
            "Retry must walk every notebook page leaf"
        );
    }


    /// Sticky Retry must `continue` past notebook pages / leaves without a
    /// Block view instead of aborting — non-Block chrome must not starve later
    /// Block leaves of `retry_history_persistence` (pairs anvil TermView skip).
    #[test]
    fn retry_block_history_skips_missing_block_views_without_aborting() {
        let source = include_str!("history_notice.rs");
        let retry = source
            .split("pub(crate) fn retry_block_history(&self) {")
            .nth(1)
            .expect("retry_block_history")
            .split("\n}\n\n#[cfg(test)]")
            .next()
            .expect("retry closes before tests");
        assert!(
            retry.contains("block_view()") && retry.contains("continue;"),
            "missing Block view must continue the leaf walk"
        );
        assert!(
            !retry.contains("break;") && !retry.contains("return;"),
            "missing Block view must not abort Retry"
        );
    }

    /// Optimistic hide must precede the notebook walk so a prior failure bar
    /// never stays visible while Retry is in flight; Ok results must not call
    /// `show_block_history_failure` (only Err re-raises).
    #[test]
    fn retry_block_history_hides_before_walk_and_stays_quiet_on_ok() {
        let source = include_str!("history_notice.rs");
        let retry = source
            .split("pub(crate) fn retry_block_history(&self) {")
            .nth(1)
            .expect("retry_block_history")
            .split("\n}\n\n#[cfg(test)]")
            .next()
            .expect("retry closes before tests");
        let hide = retry
            .find("set_visible(false)")
            .expect("optimistic hide");
        let walk = retry.find("n_pages()").expect("notebook walk");
        assert!(hide < walk, "hide must run before the notebook walk");
        assert!(
            retry.contains("if let Err(error) = view.retry_history_persistence()"),
            "only Err must re-raise the sticky bar"
        );
        assert!(
            !retry.contains("if let Ok"),
            "Ok path must stay quiet (no show on success)"
        );
    }
}
