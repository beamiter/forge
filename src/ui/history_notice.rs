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
            // Fullwidth latin B stays toast-only beside fullwidth latin A (one edge).
            "Save Block\u{ff22}history",
            // Fullwidth latin C stays toast-only beside fullwidth latin B (one edge).
            "Save Block\u{ff23}history",
            // Fullwidth latin D stays toast-only beside fullwidth latin C (one edge).
            "Save Block\u{ff24}history",
            // Fullwidth latin E stays toast-only beside fullwidth latin D (one edge).
            "Save Block\u{ff25}history",
            // Fullwidth latin F stays toast-only beside fullwidth latin E (one edge).
            "Save Block\u{ff26}history",
            // Fullwidth latin G stays toast-only beside fullwidth latin F (one edge).
            "Save Block\u{ff27}history",
            // Fullwidth latin H stays toast-only beside fullwidth latin G (one edge).
            "Save Block\u{ff28}history",
            // Fullwidth latin I stays toast-only beside fullwidth latin H (one edge).
            "Save Block\u{ff29}history",
            // Fullwidth latin J stays toast-only beside fullwidth latin I (one edge).
            "Save Block\u{ff2a}history",
            // Fullwidth latin K stays toast-only beside fullwidth latin J (one edge).
            "Save Block\u{ff2b}history",
            // Fullwidth latin L stays toast-only beside fullwidth latin K (one edge).
            "Save Block\u{ff2c}history",
            // Fullwidth latin M stays toast-only beside fullwidth latin L (one edge).
            "Save Block\u{ff2d}history",
            // Fullwidth latin N stays toast-only beside fullwidth latin M (one edge).
            "Save Block\u{ff2e}history",
            // Fullwidth latin O stays toast-only beside fullwidth latin N (one edge).
            "Save Block\u{ff2f}history",
            // Fullwidth latin P stays toast-only beside fullwidth latin O (one edge).
            "Save Block\u{ff30}history",
            // Fullwidth latin Q stays toast-only beside fullwidth latin P (one edge).
            "Save Block\u{ff31}history",
            // Fullwidth latin R stays toast-only beside fullwidth latin Q (one edge).
            "Save Block\u{ff32}history",
            // Fullwidth latin S stays toast-only beside fullwidth latin R (one edge).
            "Save Block\u{ff33}history",
            // Fullwidth latin T stays toast-only beside fullwidth latin S (one edge).
            "Save Block\u{ff34}history",
            // Fullwidth latin U stays toast-only beside fullwidth latin T (one edge).
            "Save Block\u{ff35}history",
            // Fullwidth latin V stays toast-only beside fullwidth latin U (one edge).
            "Save Block\u{ff36}history",
            // Fullwidth latin W stays toast-only beside fullwidth latin V (one edge).
            "Save Block\u{ff37}history",
            // Fullwidth latin X stays toast-only beside fullwidth latin W (one edge).
            "Save Block\u{ff38}history",
            // Fullwidth latin Y stays toast-only beside fullwidth latin X (one edge).
            "Save Block\u{ff39}history",
            // Fullwidth latin Z stays toast-only beside fullwidth latin Y (one edge).
            "Save Block\u{ff3a}history",
            // Fullwidth left square bracket stays toast-only beside fullwidth latin Z (one edge).
            "Save Block\u{ff3b}history",
            // Fullwidth reverse solidus stays toast-only beside fullwidth left square bracket (one edge).
            "Save Block\u{ff3c}history",
            // Fullwidth right square bracket stays toast-only beside fullwidth reverse solidus (one edge).
            "Save Block\u{ff3d}history",
            // Fullwidth circumflex accent stays toast-only beside fullwidth right square bracket (one edge).
            "Save Block\u{ff3e}history",
            // Fullwidth low line stays toast-only beside fullwidth circumflex (one edge).
            "Save Block\u{ff3f}history",
            // Fullwidth grave accent stays toast-only beside fullwidth low line (one edge).
            "Save Block\u{ff40}history",
            // Fullwidth latin small a stays toast-only beside fullwidth grave (one edge).
            "Save Block\u{ff41}history",
            // Fullwidth latin small b stays toast-only beside fullwidth latin small a (one edge).
            "Save Block\u{ff42}history",
            // Fullwidth latin small c stays toast-only beside fullwidth latin small b (one edge).
            "Save Block\u{ff43}history",
            // Fullwidth latin small d stays toast-only beside fullwidth latin small c (one edge).
            "Save Block\u{ff44}history",
            // Fullwidth latin small e stays toast-only beside fullwidth latin small d (one edge).
            "Save Block\u{ff45}history",
            // Fullwidth latin small f stays toast-only beside fullwidth latin small e (one edge).
            "Save Block\u{ff46}history",
            // Fullwidth latin small g stays toast-only beside fullwidth latin small f (one edge).
            "Save Block\u{ff47}history",
            // Fullwidth latin small h stays toast-only beside fullwidth latin small g (one edge).
            "Save Block\u{ff48}history",
            // Fullwidth latin small i stays toast-only beside fullwidth latin small h (one edge).
            "Save Block\u{ff49}history",
            // Fullwidth latin small j stays toast-only beside fullwidth latin small i (one edge).
            "Save Block\u{ff4a}history",
            // Fullwidth latin small k stays toast-only beside fullwidth latin small j (one edge).
            "Save Block\u{ff4b}history",
            // Fullwidth latin small l stays toast-only beside fullwidth latin small k (one edge).
            "Save Block\u{ff4c}history",
            // Fullwidth latin small m stays toast-only beside fullwidth latin small l (one edge).
            "Save Block\u{ff4d}history",
            // Fullwidth latin small n stays toast-only beside fullwidth latin small m (one edge).
            "Save Block\u{ff4e}history",
            // Fullwidth latin small o stays toast-only beside fullwidth latin small n (one edge).
            "Save Block\u{ff4f}history",
            // Fullwidth latin small p stays toast-only beside fullwidth latin small o (one edge).
            "Save Block\u{ff50}history",
            // Fullwidth latin small q stays toast-only beside fullwidth latin small p (one edge).
            "Save Block\u{ff51}history",
            // Fullwidth latin small r stays toast-only beside fullwidth latin small q (one edge).
            "Save Block\u{ff52}history",
            // Fullwidth latin small s stays toast-only beside fullwidth latin small r (one edge).
            "Save Block\u{ff53}history",
            // Fullwidth latin small t stays toast-only beside fullwidth latin small s (one edge).
            "Save Block\u{ff54}history",
            // Fullwidth latin small u stays toast-only beside fullwidth latin small t (one edge).
            "Save Block\u{ff55}history",
            // Fullwidth latin small v stays toast-only beside fullwidth latin small u (one edge).
            "Save Block\u{ff56}history",
            // Fullwidth latin small w stays toast-only beside fullwidth latin small v (one edge).
            "Save Block\u{ff57}history",
            // Fullwidth latin small x stays toast-only beside fullwidth latin small w (one edge).
            "Save Block\u{ff58}history",
            // Fullwidth latin small y stays toast-only beside fullwidth latin small x (one edge).
            "Save Block\u{ff59}history",
            // Fullwidth latin small z stays toast-only beside fullwidth latin small y (one edge).
            "Save Block\u{ff5a}history",
            // Fullwidth left curly bracket stays toast-only beside fullwidth latin small z (one edge).
            "Save Block\u{ff5b}history",
            // Fullwidth vertical line stays toast-only beside fullwidth left curly bracket (one edge).
            "Save Block\u{ff5c}history",
            // Fullwidth right curly bracket stays toast-only beside fullwidth vertical line (one edge).
            "Save Block\u{ff5d}history",
            // Fullwidth tilde stays toast-only beside fullwidth right curly bracket (one edge).
            "Save Block\u{ff5e}history",
            // Fullwidth left white parenthesis stays toast-only beside fullwidth tilde (one edge).
            "Save Block\u{ff5f}history",
            // Fullwidth right white parenthesis stays toast-only beside fullwidth left white parenthesis (one edge).
            "Save Block\u{ff60}history",
            // Halfwidth ideographic full stop stays toast-only beside fullwidth right white parenthesis (one edge).
            "Save Block\u{ff61}history",
            // Halfwidth left corner bracket stays toast-only beside halfwidth ideographic full stop (one edge).
            "Save Block\u{ff62}history",
            // Halfwidth right corner bracket stays toast-only beside halfwidth left corner bracket (one edge).
            "Save Block\u{ff63}history",
            // Halfwidth ideographic comma stays toast-only beside halfwidth right corner bracket (one edge).
            "Save Block\u{ff64}history",
            // Halfwidth katakana middle dot stays toast-only beside halfwidth ideographic comma (one edge).
            "Save Block\u{ff65}history",
            // Halfwidth katakana letter wo stays toast-only beside halfwidth katakana middle dot (one edge).
            "Save Block\u{ff66}history",
            // Halfwidth katakana letter small a stays toast-only beside halfwidth katakana letter wo (one edge).
            "Save Block\u{ff67}history",
            // Halfwidth katakana letter small i stays toast-only beside halfwidth katakana letter small a (one edge).
            "Save Block\u{ff68}history",
            // Halfwidth katakana letter small u stays toast-only beside halfwidth katakana letter small i (one edge).
            "Save Block\u{ff69}history",
            // Halfwidth katakana letter small e stays toast-only beside halfwidth katakana letter small u (one edge).
            "Save Block\u{ff6a}history",
            // Halfwidth katakana letter small o stays toast-only beside halfwidth katakana letter small e (one edge).
            "Save Block\u{ff6b}history",
            // Halfwidth katakana letter small tu stays toast-only beside halfwidth katakana letter small o (one edge).
            "Save Block\u{ff6c}history",
            // Halfwidth katakana letter small ya stays toast-only beside halfwidth katakana letter small tu (one edge).
            "Save Block\u{ff6d}history",
            // Halfwidth katakana letter small yu stays toast-only beside halfwidth katakana letter small ya (one edge).
            "Save Block\u{ff6e}history",
            // Halfwidth katakana letter small yo stays toast-only beside halfwidth katakana letter small yu (one edge).
            "Save Block\u{ff6f}history",
            // Halfwidth katakana-hiragana prolonged sound mark stays toast-only beside halfwidth katakana letter small yo (one edge).
            "Save Block\u{ff70}history",
            // Halfwidth katakana letter a stays toast-only beside halfwidth katakana-hiragana prolonged sound mark (one edge).
            "Save Block\u{ff71}history",
            // Halfwidth katakana letter i stays toast-only beside halfwidth katakana letter a (one edge).
            "Save Block\u{ff72}history",
            // Halfwidth katakana letter u stays toast-only beside halfwidth katakana letter i (one edge).
            "Save Block\u{ff73}history",
            // Halfwidth katakana letter e stays toast-only beside halfwidth katakana letter u (one edge).
            "Save Block\u{ff74}history",
            // Halfwidth katakana letter o stays toast-only beside halfwidth katakana letter e (one edge).
            "Save Block\u{ff75}history",
            // Halfwidth katakana letter ka stays toast-only beside halfwidth katakana letter o (one edge).
            "Save Block\u{ff76}history",
            // Halfwidth katakana letter ki stays toast-only beside halfwidth katakana letter ka (one edge).
            "Save Block\u{ff77}history",
            // Halfwidth katakana letter ku stays toast-only beside halfwidth katakana letter ki (one edge).
            "Save Block\u{ff78}history",
            // Halfwidth katakana letter ke stays toast-only beside halfwidth katakana letter ku (one edge).
            "Save Block\u{ff79}history",
            // Halfwidth katakana letter ko stays toast-only beside halfwidth katakana letter ke (one edge).
            "Save Block\u{ff7a}history",
            // Halfwidth katakana letter sa stays toast-only beside halfwidth katakana letter ko (one edge).
            "Save Block\u{ff7b}history",
            // Halfwidth katakana letter si stays toast-only beside halfwidth katakana letter sa (one edge).
            "Save Block\u{ff7c}history",
            // Halfwidth katakana letter su stays toast-only beside halfwidth katakana letter si (one edge).
            "Save Block\u{ff7d}history",
            // Halfwidth katakana letter se stays toast-only beside halfwidth katakana letter su (one edge).
            "Save Block\u{ff7e}history",
            // Halfwidth katakana letter so stays toast-only beside halfwidth katakana letter se (one edge).
            "Save Block\u{ff7f}history",
            // Halfwidth katakana letter ta stays toast-only beside halfwidth katakana letter so (one edge).
            "Save Block\u{ff80}history",
            // Halfwidth katakana letter ti stays toast-only beside halfwidth katakana letter ta (one edge).
            "Save Block\u{ff81}history",
            // Halfwidth katakana letter tu stays toast-only beside halfwidth katakana letter ti (one edge).
            "Save Block\u{ff82}history",
            // Halfwidth katakana letter te stays toast-only beside halfwidth katakana letter tu (one edge).
            "Save Block\u{ff83}history",
            // Halfwidth katakana letter to stays toast-only beside halfwidth katakana letter te (one edge).
            "Save Block\u{ff84}history",
            // Halfwidth katakana letter na stays toast-only beside halfwidth katakana letter to (one edge).
            "Save Block\u{ff85}history",
            // Halfwidth katakana letter ni stays toast-only beside halfwidth katakana letter na (one edge).
            "Save Block\u{ff86}history",
            // Halfwidth katakana letter nu stays toast-only beside halfwidth katakana letter ni (one edge).
            "Save Block\u{ff87}history",
            // Halfwidth katakana letter ne stays toast-only beside halfwidth katakana letter nu (one edge).
            "Save Block\u{ff88}history",
            // Halfwidth katakana letter no stays toast-only beside halfwidth katakana letter ne (one edge).
            "Save Block\u{ff89}history",
            // Halfwidth katakana letter ha stays toast-only beside halfwidth katakana letter no (one edge).
            "Save Block\u{ff8a}history",
            // Halfwidth katakana letter hi stays toast-only beside halfwidth katakana letter ha (one edge).
            "Save Block\u{ff8b}history",
            // Halfwidth katakana letter hu stays toast-only beside halfwidth katakana letter hi (one edge).
            "Save Block\u{ff8c}history",
            // Halfwidth katakana letter he stays toast-only beside halfwidth katakana letter hu (one edge).
            "Save Block\u{ff8d}history",
            // Halfwidth katakana letter ho stays toast-only beside halfwidth katakana letter he (one edge).
            "Save Block\u{ff8e}history",
            // Halfwidth katakana letter ma stays toast-only beside halfwidth katakana letter ho (one edge).
            "Save Block\u{ff8f}history",
            // Halfwidth katakana letter mi stays toast-only beside halfwidth katakana letter ma (one edge).
            "Save Block\u{ff90}history",
            // Halfwidth katakana letter mu stays toast-only beside halfwidth katakana letter mi (one edge).
            "Save Block\u{ff91}history",
            // Halfwidth katakana letter me stays toast-only beside halfwidth katakana letter mu (one edge).
            "Save Block\u{ff92}history",
            // Halfwidth katakana letter mo stays toast-only beside halfwidth katakana letter me (one edge).
            "Save Block\u{ff93}history",
            // Halfwidth katakana letter ya stays toast-only beside halfwidth katakana letter mo (one edge).
            "Save Block\u{ff94}history",
            // Halfwidth katakana letter yu stays toast-only beside halfwidth katakana letter ya (one edge).
            "Save Block\u{ff95}history",
            // Halfwidth katakana letter yo stays toast-only beside halfwidth katakana letter yu (one edge).
            "Save Block\u{ff96}history",
            // Halfwidth katakana letter small tsu stays toast-only beside halfwidth katakana letter yo (one edge).
            "Save Block\u{ff97}history",
            // Halfwidth katakana letter ta stays toast-only beside halfwidth katakana letter small tsu (one edge).
            "Save Block\u{ff98}history",
            // Halfwidth katakana letter chi stays toast-only beside halfwidth katakana letter ta (one edge).
            "Save Block\u{ff99}history",
            // Halfwidth katakana letter tsu stays toast-only beside halfwidth katakana letter chi (one edge).
            "Save Block\u{ff9a}history",
            // Halfwidth katakana letter te stays toast-only beside halfwidth katakana letter tsu (one edge).
            "Save Block\u{ff9b}history",
            // Halfwidth katakana letter to stays toast-only beside halfwidth katakana letter te (one edge).
            "Save Block\u{ff9c}history",
            // Halfwidth katakana letter na stays toast-only beside halfwidth katakana letter to (one edge).
            "Save Block\u{ff9d}history",
            // Halfwidth katakana letter ni stays toast-only beside halfwidth katakana letter na (one edge).
            "Save Block\u{ff9e}history",
            // Halfwidth katakana letter nu stays toast-only beside halfwidth katakana letter ni (one edge).
            "Save Block\u{ff9f}history",
            // Halfwidth hangul filler stays toast-only beside halfwidth katakana letter nu (one edge).
            "Save Block\u{ffa0}history",
            // Halfwidth hangul letter kiyeok stays toast-only beside halfwidth hangul filler (one edge).
            "Save Block\u{ffa1}history",
            // Halfwidth hangul letter ssangkiyeok stays toast-only beside halfwidth hangul letter kiyeok (one edge).
            "Save Block\u{ffa2}history",
            // Halfwidth hangul letter kiyeok-sios stays toast-only beside halfwidth hangul letter ssangkiyeok (one edge).
            "Save Block\u{ffa3}history",
            // Halfwidth hangul letter nieun stays toast-only beside halfwidth hangul letter kiyeok-sios (one edge).
            "Save Block\u{ffa4}history",
            // Halfwidth hangul letter nieun-cieuc stays toast-only beside halfwidth hangul letter nieun (one edge).
            "Save Block\u{ffa5}history",
            // Halfwidth hangul letter nieun-hieuh stays toast-only beside halfwidth hangul letter nieun-cieuc (one edge).
            "Save Block\u{ffa6}history",
            // Halfwidth hangul letter tikeut stays toast-only beside halfwidth hangul letter nieun-hieuh (one edge).
            "Save Block\u{ffa7}history",
            // Halfwidth hangul letter ssangtikeut stays toast-only beside halfwidth hangul letter tikeut (one edge).
            "Save Block\u{ffa8}history",
            // Halfwidth hangul letter tikeut-sios stays toast-only beside halfwidth hangul letter ssangtikeut (one edge).
            "Save Block\u{ffa9}history",
            // Halfwidth hangul letter rieul stays toast-only beside halfwidth hangul letter tikeut-sios (one edge).
            "Save Block\u{ffaa}history",
            // Halfwidth hangul letter rieul-kiyeok stays toast-only beside halfwidth hangul letter rieul (one edge).
            "Save Block\u{ffab}history",
            // Halfwidth hangul letter rieul-mieum stays toast-only beside halfwidth hangul letter rieul-kiyeok (one edge).
            "Save Block\u{ffac}history",
            // Halfwidth hangul letter rieul-pieup stays toast-only beside halfwidth hangul letter rieul-mieum (one edge).
            "Save Block\u{ffad}history",
            // Halfwidth hangul letter rieul-sios stays toast-only beside halfwidth hangul letter rieul-pieup (one edge).
            "Save Block\u{ffae}history",
            // Halfwidth hangul letter rieul-thieuth stays toast-only beside halfwidth hangul letter rieul-sios (one edge).
            "Save Block\u{ffaf}history",
            // Halfwidth hangul letter rieul-phieuph stays toast-only beside halfwidth hangul letter rieul-thieuth (one edge).
            "Save Block\u{ffb0}history",
            // Halfwidth hangul letter rieul-hieuh stays toast-only beside halfwidth hangul letter rieul-phieuph (one edge).
            "Save Block\u{ffb1}history",
            // Halfwidth hangul letter mieum stays toast-only beside halfwidth hangul letter rieul-hieuh (one edge).
            "Save Block\u{ffb2}history",
            // Halfwidth hangul letter pieup stays toast-only beside halfwidth hangul letter mieum (one edge).
            "Save Block\u{ffb3}history",
            // Halfwidth hangul letter ssangpieup stays toast-only beside halfwidth hangul letter pieup (one edge).
            "Save Block\u{ffb4}history",
            // Halfwidth hangul letter pieup-sios stays toast-only beside halfwidth hangul letter ssangpieup (one edge).
            "Save Block\u{ffb5}history",
            // Halfwidth hangul letter sios stays toast-only beside halfwidth hangul letter pieup-sios (one edge).
            "Save Block\u{ffb6}history",
            // Halfwidth hangul letter ssangsios stays toast-only beside halfwidth hangul letter sios (one edge).
            "Save Block\u{ffb7}history",
            // Halfwidth hangul letter ieung stays toast-only beside halfwidth hangul letter ssangsios (one edge).
            "Save Block\u{ffb8}history",
            // Halfwidth hangul letter cieuc stays toast-only beside halfwidth hangul letter ieung (one edge).
            "Save Block\u{ffb9}history",
            // Halfwidth hangul letter ssangcieuc stays toast-only beside halfwidth hangul letter cieuc (one edge).
            "Save Block\u{ffba}history",
            // Halfwidth hangul letter chieuch stays toast-only beside halfwidth hangul letter ssangcieuc (one edge).
            "Save Block\u{ffbb}history",
            // Halfwidth hangul letter khieukh stays toast-only beside halfwidth hangul letter chieuch (one edge).
            "Save Block\u{ffbc}history",
            // Halfwidth hangul letter thieuth stays toast-only beside halfwidth hangul letter khieukh (one edge).
            "Save Block\u{ffbd}history",
            // Halfwidth hangul letter phieuph stays toast-only beside halfwidth hangul letter thieuth (one edge).
            "Save Block\u{ffbe}history",
            // Halfwidth hangul letter hieuh stays toast-only beside halfwidth hangul letter phieuph (one edge).
            "Save Block\u{ffbf}history",
            // Halfwidth cent sign stays toast-only beside halfwidth hangul letter hieuh (one edge).
            "Save Block\u{ffe0}history",
            // Halfwidth pound sign stays toast-only beside halfwidth cent sign (one edge).
            "Save Block\u{ffe1}history",
            // Halfwidth yen sign stays toast-only beside halfwidth pound sign (one edge).
            "Save Block\u{ffe2}history",
            // Halfwidth macron stays toast-only beside halfwidth yen sign (one edge).
            "Save Block\u{ffe3}history",
            // Halfwidth broken bar stays toast-only beside halfwidth macron (one edge).
            "Save Block\u{ffe4}history",
            // Halfwidth won sign stays toast-only beside halfwidth broken bar (one edge).
            "Save Block\u{ffe5}history",
            // Halfwidth double vertical line stays toast-only beside halfwidth won sign (one edge).
            "Save Block\u{ffe6}history",
            // Halfwidth forms light vertical stays toast-only beside halfwidth double vertical line (one edge).
            "Save Block\u{ffe8}history",
            // Halfwidth forms light down stays toast-only beside halfwidth forms light vertical (one edge).
            "Save Block\u{ffe9}history",
            // Halfwidth forms light up stays toast-only beside halfwidth forms light down (one edge).
            "Save Block\u{ffea}history",
            // Halfwidth forms light left stays toast-only beside halfwidth forms light up (one edge).
            "Save Block\u{ffeb}history",
            // Halfwidth forms light right stays toast-only beside halfwidth forms light left (one edge).
            "Save Block\u{ffec}history",
            // Halfwidth black square stays toast-only beside halfwidth forms light right (one edge).
            "Save Block\u{ffed}history",
            // Halfwidth white circle stays toast-only beside halfwidth black square (one edge).
            "Save Block\u{ffee}history",
            // Interlinear annotation anchor stays toast-only beside halfwidth white circle (one edge).
            "Save Block\u{fff9}history",
            // Interlinear annotation separator stays toast-only beside interlinear annotation anchor (one edge).
            "Save Block\u{fffa}history",
            // Interlinear annotation terminator stays toast-only beside interlinear annotation separator (one edge).
            "Save Block\u{fffb}history",
            // Object replacement character stays toast-only beside interlinear annotation terminator (one edge).
            "Save Block\u{fffc}history",
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
