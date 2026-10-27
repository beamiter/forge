//! Passive, bounded feedback for human input that never entered the PTY.
//! Protocol replies and callers with their own error UI do not use this path.
use crate::pty::{PtyWriteError, MAX_PTY_INPUT_MESSAGE_BYTES};
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub(super) struct InputFeedback {
    target: gtk4::glib::WeakRef<gtk4::Widget>,
    current: RefCell<Option<(std::mem::Discriminant<PtyWriteError>, libadwaita::Toast)>>,
}

impl InputFeedback {
    pub(super) fn new(target: &impl IsA<gtk4::Widget>) -> Rc<Self> {
        let feedback = Rc::new(Self {
            target: target.as_ref().downgrade(),
            current: RefCell::new(None),
        });
        let weak = Rc::downgrade(&feedback);
        target.connect_unmap(move |_| {
            if let Some(feedback) = weak.upgrade() {
                feedback.accepted();
            }
        });
        feedback
    }

    #[cfg(test)]
    pub(super) fn detached() -> Rc<Self> {
        Rc::new(Self {
            target: gtk4::glib::WeakRef::new(),
            current: RefCell::new(None),
        })
    }

    pub(super) fn accepted(&self) {
        let previous = self.current.borrow_mut().take();
        if let Some((_, toast)) = previous {
            toast.dismiss();
        }
    }

    /// Keep at most one finite-lived notice and one bounded exact draft.
    /// Never retry input, acquire focus, show a modal, or retain a pane.
    pub(super) fn rejected(self: &Rc<Self>, error: PtyWriteError, draft: Option<&str>) {
        let category = std::mem::discriminant(&error);
        if self
            .current
            .borrow()
            .as_ref()
            .is_some_and(|(old, _)| *old == category)
        {
            return;
        }
        let Some(target) = self.target.upgrade().filter(|target| target.is_mapped()) else {
            return;
        };
        let mut parent = target.parent();
        let overlay = loop {
            let Some(widget) = parent else { return };
            if let Ok(overlay) = widget.clone().downcast::<libadwaita::ToastOverlay>() {
                break overlay;
            }
            parent = widget.parent();
        };
        self.accepted();
        let draft = recoverable_draft(draft);
        let message = if draft.is_some() {
            format!("{error}. Your draft remains in the terminal; you can copy it.")
        } else {
            error.to_string()
        };
        let toast = libadwaita::Toast::new(&message);
        toast.set_use_markup(false);
        toast.set_timeout(8);
        if let Some(draft) = draft {
            toast.set_button_label(Some("Copy draft"));
            let target = self.target.clone();
            toast.connect_button_clicked(move |_| {
                // The original pane must still exist and be visible; never
                // copy on behalf of a replacement session or hidden tab.
                if let Some(target) = target.upgrade().filter(|target| target.is_mapped()) {
                    target.clipboard().set_text(&draft);
                }
            });
        }
        let weak = Rc::downgrade(self);
        toast.connect_dismissed(move |toast| {
            if let Some(feedback) = weak.upgrade() {
                let mut current = feedback.current.borrow_mut();
                if current
                    .as_ref()
                    .is_some_and(|(_, current)| current == toast)
                {
                    current.take();
                }
            }
        });
        *self.current.borrow_mut() = Some((category, toast.clone()));
        overlay.add_toast(toast);
    }
}

fn recoverable_draft(draft: Option<&str>) -> Option<String> {
    draft
        .filter(|text| !text.is_empty() && text.len() <= MAX_PTY_INPUT_MESSAGE_BYTES)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires DISPLAY; verifies bounded input feedback and draft recovery"]
    fn rejected_input_notice_is_bounded_and_recovers_exact_draft() {
        libadwaita::init().expect("gtk4 display");
        let target = gtk4::Entry::new();
        let other = gtk4::Entry::new();
        let column = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        column.append(&target);
        column.append(&other);
        let overlay = libadwaita::ToastOverlay::new();
        overlay.set_child(Some(&column));
        let window = gtk4::Window::builder().child(&overlay).build();
        window.present();
        while gtk4::glib::MainContext::default().iteration(false) {}
        other.grab_focus();
        let focus = gtk4::prelude::GtkWindowExt::focus(&window);
        let feedback = InputFeedback::new(&target);
        let draft = "echo 你好🙂 <unchanged>";
        feedback.rejected(PtyWriteError::QueueFull { bytes: 1 }, Some(draft));
        let first = feedback.current.borrow().as_ref().unwrap().1.clone();
        assert_eq!(first.timeout(), 8);
        assert_eq!(first.button_label().as_deref(), Some("Copy draft"));
        assert!(!first.title().unwrap().contains(draft));
        for _ in 0..100 {
            feedback.rejected(PtyWriteError::QueueFull { bytes: 4 }, Some("newer"));
        }
        assert_eq!(feedback.current.borrow().as_ref().unwrap().1, first);
        assert_eq!(gtk4::prelude::GtkWindowExt::focus(&window), focus);
        first.emit_by_name::<()>("button-clicked", &[]);
        let copied = gtk4::glib::MainContext::default()
            .block_on(target.clipboard().read_text_future())
            .unwrap()
            .unwrap();
        assert_eq!(copied.as_str(), draft);
        feedback.rejected(PtyWriteError::Closed { bytes: 1 }, None);
        let closed = feedback.current.borrow().as_ref().unwrap().1.clone();
        assert_ne!(closed, first);
        assert!(closed.button_label().is_none());
        feedback.accepted();
        assert!(feedback.current.borrow().is_none());
        feedback.rejected(PtyWriteError::QueueFull { bytes: 1 }, None);
        let again = feedback.current.borrow().as_ref().unwrap().1.clone();
        again.dismiss();
        assert!(feedback.current.borrow().is_none());
        feedback.rejected(PtyWriteError::QueueFull { bytes: 1 }, None);
        target.set_visible(false);
        assert!(feedback.current.borrow().is_none());
        feedback.rejected(PtyWriteError::Closed { bytes: 1 }, Some("stale"));
        assert!(feedback.current.borrow().is_none());
        window.close();
    }

    #[test]
    fn recovery_is_bounded_and_unicode_exact() {
        let text = "你好🙂".repeat(MAX_PTY_INPUT_MESSAGE_BYTES / 10);
        assert_eq!(
            recoverable_draft(Some(&text)).as_deref(),
            Some(text.as_str())
        );
        assert!(recoverable_draft(Some(&"x".repeat(MAX_PTY_INPUT_MESSAGE_BYTES + 1))).is_none());
        assert!(recoverable_draft(Some("")).is_none());
        assert!(recoverable_draft(None).is_none());
    }
}
