//! Content-free feedback for the clipboard encoder's lossy safety fallback.
use libadwaita::prelude::*;
use std::rc::Rc;

pub(super) const PARTIAL: &str =
    "Only the first line was pasted. Multiline paste is unavailable in this terminal.";
pub(super) const EMPTY: &str = "Nothing was pasted. Multiline paste is unavailable and the first line contained no pasteable text.";

pub(super) fn report(notify: &dyn Fn(&str), truncated: bool, delivered: bool) {
    if truncated {
        notify(if delivered { PARTIAL } else { EMPTY });
    }
}

/// Resolve only the original, still-visible pane. Toasts contain no clipboard
/// content, have no action button, and never acquire keyboard focus.
pub(super) fn notifier(target: &impl IsA<gtk4::Widget>) -> Rc<dyn Fn(&str)> {
    let target = target.as_ref().downgrade();
    Rc::new(move |message| {
        let Some(target) = target.upgrade().filter(|widget| widget.is_mapped()) else {
            return;
        };
        let mut ancestor = target.parent();
        while let Some(widget) = ancestor {
            if let Ok(overlay) = widget.clone().downcast::<libadwaita::ToastOverlay>() {
                overlay.add_toast(libadwaita::Toast::new(message));
                return;
            }
            ancestor = widget.parent();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires DISPLAY; verifies passive paste feedback"]
    fn notice_preserves_focus() {
        libadwaita::init().expect("gtk4 display");
        let target = gtk4::Entry::new();
        let newer = gtk4::Entry::new();
        let column = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        column.append(&target);
        column.append(&newer);
        let overlay = libadwaita::ToastOverlay::new();
        overlay.set_child(Some(&column));
        let window = gtk4::Window::builder().child(&overlay).build();
        window.present();
        while gtk4::glib::MainContext::default().iteration(false) {}
        assert!(target.is_mapped());
        assert!(newer.grab_focus());
        let before = gtk4::prelude::GtkWindowExt::focus(&window);
        let notify = notifier(&target);
        assert!(before.is_some());
        notify(PARTIAL);
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(200) {
            while gtk4::glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        fn has_notice(widget: &gtk4::Widget) -> bool {
            if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
                if label.text() == PARTIAL {
                    return true;
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if has_notice(&widget) {
                    return true;
                }
                child = widget.next_sibling();
            }
            false
        }
        assert!(
            has_notice(overlay.upcast_ref()),
            "the toast is actually rendered"
        );
        assert_eq!(gtk4::prelude::GtkWindowExt::focus(&window), before);
        window.close();
        notify(EMPTY); // detached/hidden owners never create a late dialog
    }
}
