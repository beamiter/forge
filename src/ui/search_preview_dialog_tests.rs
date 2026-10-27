//! Hosted-only integration of the single production dialog builder and worker.
//! GTK signals are driven programmatically; this is not physical keyboard or
//! complete application tab-action coverage. PRIMARY is a synthetic same-process
//! provider on an isolated Xvfb server, never a user's clipboard.
use super::*;
use crate::block_view::{
    palette_activation_test_feed as feed, palette_activation_test_view as pane,
};

fn pump() {
    for _ in 0..128 {
        if !glib::MainContext::default().iteration(false) {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(2));
}
fn until(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "native preview condition timed out"
        );
        pump();
    }
}
fn settle() {
    let deadline = std::time::Instant::now() + Duration::from_millis(120);
    while std::time::Instant::now() < deadline {
        pump();
    }
}
fn descendants(widget: &gtk4::Widget) -> Vec<gtk4::Widget> {
    let mut result = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        result.extend(descendants(&current));
    }
    result
}
fn entry(dialog: &adw::Dialog) -> gtk4::SearchEntry {
    descendants(dialog.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast().ok())
        .unwrap()
}
fn regex(dialog: &adw::Dialog) -> gtk4::CheckButton {
    descendants(dialog.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast().ok())
        .unwrap()
}
fn seed(view: &TermView, word: &str) {
    feed(view, b"\x1b]133;A\x07$ \x1b]133;B\x07");
    settle();
    feed(view, format!("printf sample\r\n\x1b]133;C\x07{word} {word} {word}\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07").as_bytes());
    until(|| {
        view.capture_search_preview()
            .is_ok_and(|s| s.documents.iter().any(|d| d.text.contains(word)))
    });
}
fn assert_retired(c: &PreviewControls) {
    until(|| c.worker.has_exited());
    settle();
    let polls = c.poll_calls.get();
    settle();
    assert_eq!(c.poll_calls.get(), polls, "retired timer must be removed");
    assert!(c.matches.borrow().is_none());
}
fn status(c: &PreviewControls) -> String {
    c.status.upgrade().unwrap().text().to_string()
}
fn assert_current(c: &PreviewControls, ordinal: usize, total: usize) {
    assert_eq!(status(c), format!("Preview: {} of {total}", ordinal + 1));
    let text = c.text.upgrade().unwrap();
    let buffer = text.buffer();
    assert!(!buffer.has_selection());
    let matches = c.matches.borrow();
    let hit = &matches.as_ref().unwrap().hits[ordinal];
    assert!(buffer.iter_at_offset(hit.chars.start).has_tag(&c.tag));
    assert_eq!(
        buffer
            .iter_at_mark(&buffer.mark("exact-preview-scroll").unwrap())
            .offset(),
        hit.chars.start
    );
}

#[test]
#[ignore = "requires isolated hosted GTK/Xvfb; never run in restricted executor"]
fn production_dialog_is_frozen_stale_safe_and_primary_neutral() {
    adw::init().expect("native GTK/Adwaita display");
    let a = pane(crate::config::TerminalMode::Block);
    let b = pane(crate::config::TerminalMode::Block);
    let stack = gtk4::Stack::new();
    stack.add_named(&a.widget(), Some("a"));
    stack.add_named(&b.widget(), Some("b"));
    let window = adw::Window::builder()
        .default_width(900)
        .default_height(700)
        .content(&stack)
        .build();
    window.present();
    settle();
    seed(&a, "needle");
    seed(&b, "different");
    crate::block_view::preview_test_mark_partial(&a);
    let clipboard = gtk4::gdk::Display::default().unwrap().primary_clipboard();
    let sentinel =
        gtk4::gdk::ContentProvider::for_value(&"synthetic-preview-primary-sentinel".to_value());
    clipboard.set_content(Some(&sentinel)).unwrap();
    settle();
    let changes = Rc::new(Cell::new(0usize));
    let changes_cb = changes.clone();
    let handler = clipboard.connect_changed(move |_| changes_cb.set(changes_cb.get() + 1));
    let check_primary = || {
        assert_eq!(
            clipboard.content(),
            Some(sentinel.clone()),
            "PRIMARY provider changed"
        );
        assert_eq!(changes.get(), 0, "PRIMARY ownership/content changed");
    };
    let (dialog, controls) = present_exact_preview(&window, a.clone(), "needle").unwrap();
    let input = entry(&dialog);
    until(|| status(&controls) == "Preview: 1 of 3");
    settle(); // Include Adwaita's deferred initial-focus map ticks.
    let delegate = input.delegate().unwrap().downcast::<gtk4::Text>().unwrap();
    assert!(
        delegate.has_focus(),
        "initial keyboard focus stays in search input"
    );
    assert!(
        input.selection_bounds().is_none(),
        "initial seed must not be selected"
    );
    check_primary();
    assert!(controls
        .location
        .upgrade()
        .unwrap()
        .text()
        .contains("Partial retained text"));
    assert!(descendants(dialog.upcast_ref())
        .iter()
        .filter_map(|w| w.clone().downcast::<gtk4::Label>().ok())
        .any(|l| l.text().contains("Close and reopen to refresh")));
    for _ in 0..2 {
        for ordinal in [1, 2, 0] {
            controls.next.upgrade().unwrap().emit_clicked();
            assert_current(&controls, ordinal, 3);
        }
        for ordinal in [2, 1, 0] {
            controls.previous.upgrade().unwrap().emit_clicked();
            assert_current(&controls, ordinal, 3);
        }
    }
    check_primary();
    // Deterministic old-query completion at the real worker publication point.
    let rejected_before = controls.rejected_replies.get();
    let (arrived, release, published, finish) = controls.worker.pause_publication_for("need");
    input.set_text("need");
    until(|| arrived.try_recv().is_ok());
    input.set_text("absent");
    assert_eq!(controls.text.upgrade().unwrap().buffer().char_count(), 0);
    release.send(()).unwrap();
    until(|| published.try_recv().is_ok());
    until(|| controls.rejected_replies.get() > rejected_before);
    assert_eq!(controls.text.upgrade().unwrap().buffer().char_count(), 0);
    finish.send(()).unwrap();
    until(|| status(&controls) == "No matches in this snapshot.");
    settle();
    assert_eq!(controls.text.upgrade().unwrap().buffer().char_count(), 0);
    input.set_text("needle");
    until(|| status(&controls) == "Preview: 1 of 3");
    regex(&dialog).set_active(true);
    input.set_text("needle(?=x)");
    until(|| !status(&controls).starts_with("Searching"));
    assert!(controls.matches.borrow().is_none());
    regex(&dialog).set_active(false);
    input.set_text("needle");
    until(|| status(&controls) == "Preview: 1 of 3");
    // Hide A/show B without changing the frozen source or resolving B.
    stack.set_visible_child_name("b");
    settle();
    controls.next.upgrade().unwrap().emit_clicked();
    assert_current(&controls, 1, 3);
    assert!(controls
        .text
        .upgrade()
        .unwrap()
        .buffer()
        .text(
            &controls.text.upgrade().unwrap().buffer().start_iter(),
            &controls.text.upgrade().unwrap().buffer().end_iter(),
            true
        )
        .contains("needle"));
    // Hiding/unmapping the root does not unrealize it or retire this snapshot.
    let before_hide = controls.snapshot.id;
    window.set_visible(false);
    settle();
    assert!(window.is_realized());
    assert!(controls.owner_present());
    assert!(!controls.worker.has_exited());
    window.present();
    settle();
    assert_eq!(controls.snapshot.id, before_hide);
    controls.next.upgrade().unwrap().emit_clicked();
    assert_current(&controls, 2, 3);
    assert!(delegate.has_focus());
    assert!(input.selection_bounds().is_none());
    check_primary();
    let regex_focus = regex(&dialog);
    regex_focus.grab_focus();
    window.set_visible(false);
    settle();
    window.present();
    settle();
    assert!(
        regex_focus.has_focus(),
        "remap must preserve non-entry focus"
    );
    assert!(!delegate.has_focus());
    check_primary();
    seed(&a, "fresh");
    input.set_text("fresh");
    until(|| status(&controls) == "No matches in this snapshot.");
    let old_id = controls.snapshot.id;
    // Close while a real worker result is blocked. Retire before its release.
    let (arrived, release, published, finish) = controls.worker.pause_publication_for("needle");
    input.set_text("needle");
    until(|| arrived.try_recv().is_ok());
    assert!(dialog.close());
    assert!(
        controls.closed.get(),
        "closed must retire synchronously at close start"
    );
    release.send(()).unwrap();
    until(|| published.try_recv().is_ok());
    finish.send(()).unwrap();
    settle();
    assert!(controls.matches.borrow().is_none());
    assert_eq!(controls.text.upgrade().unwrap().buffer().char_count(), 0);
    assert_retired(&controls);
    check_primary();
    // Reopening performs a real capture and cannot receive the old dialog reply.
    let (reopened, refreshed) = present_exact_preview(&window, a.clone(), "fresh").unwrap();
    assert_ne!(refreshed.snapshot.id, old_id);
    until(|| status(&refreshed) == "Preview: 1 of 3");
    let (arrived, release, published, finish) = refreshed.worker.pause_publication_for("needle");
    entry(&reopened).set_text("needle");
    until(|| arrived.try_recv().is_ok());
    stack.remove(&a.widget());
    until(|| status(&refreshed).starts_with("Source pane unavailable"));
    release.send(()).unwrap();
    until(|| published.try_recv().is_ok());
    finish.send(()).unwrap();
    settle();
    assert!(refreshed.matches.borrow().is_none());
    assert_eq!(refreshed.text.upgrade().unwrap().buffer().char_count(), 0);
    reopened.force_close();
    assert!(refreshed.closed.get());
    assert_retired(&refreshed);
    settle();
    check_primary();
    // Force close and parent destruction also retire their actual poll/worker.
    let (forced, force_controls) = present_exact_preview(&window, b.clone(), "different").unwrap();
    until(|| status(&force_controls) == "Preview: 1 of 3");
    forced.force_close();
    assert!(force_controls.closed.get());
    assert_retired(&force_controls);
    settle();
    let (last, last_controls) = present_exact_preview(&window, b.clone(), "different").unwrap();
    until(|| status(&last_controls) == "Preview: 1 of 3");
    let (arrived, release, published, finish) =
        last_controls.worker.pause_publication_for("differ");
    entry(&last).set_text("differ");
    until(|| arrived.try_recv().is_ok());
    window.destroy();
    until(|| !last_controls.owner_present());
    release.send(()).unwrap();
    until(|| published.try_recv().is_ok());
    finish.send(()).unwrap();
    settle();
    assert!(!last_controls.owner_present());
    assert_retired(&last_controls);
    assert!(last_controls.matches.borrow().is_none());
    check_primary();
    clipboard.disconnect(handler);
}
