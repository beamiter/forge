//! Shared native editor-seat mechanics, explicitly selected for new Block.
//! No GtkEntry, terminal, PTY or submission controller is created here.
use super::*;

pub(super) fn for_block(
    root: &gtk4::Box,
    history: &gtk4::Overlay,
    active: &ActiveBlock,
    compact: bool,
) -> Rc<chat::ChatPresentation> {
    root.add_css_class("developer-docked-input");
    active.active_vte.add_css_class("docked-live-terminal");
    active.active_vte.add_css_class("developer-live-terminal");
    // The one clip has the same available width in its prompt and running
    // seats. Keep padding on the moved clip rather than on either parent.
    active.live_clip().set_margin_start(16);
    active.live_clip().set_margin_end(16);
    // ChatPresentation contains the proven clip-seat, generation-bound Send /
    // Stop and IME boundaries. Its root's semantic Chat class is never added;
    // message cards/fixture/font rules remain selected only by chat_mode.
    Rc::new(chat::ChatPresentation::new(root, history, active, compact))
}
