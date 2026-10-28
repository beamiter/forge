//! Hosted-only integration of the single production dialog builder and worker.
//! Dialog controls are driven programmatically; a private XTest key also checks
//! restored parent button activation. This is not complete application tab-action coverage. PRIMARY is a synthetic same-process
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
    // A noneditable keyboard target cannot claim PRIMARY through selection.
    let parent_action = gtk4::Button::with_label("Parent keyboard target");
    let parent_focus: gtk4::Widget = parent_action.clone().upcast();
    let activations = Rc::new(Cell::new(0_usize));
    let clicked = activations.clone();
    parent_action.connect_clicked(move |_| clicked.set(clicked.get() + 1));
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content.append(&parent_action);
    stack.set_vexpand(true);
    content.append(&stack);
    let window_title = format!("Forge exact-preview lifecycle {}", std::process::id());
    let window = adw::Window::builder()
        .title(&window_title)
        .default_width(900)
        .default_height(700)
        .content(&content)
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
    // Same parent, no new preview and no application close: distinguish the
    // window's own hide/show behavior from early-preview cancellation.
    parent_action.grab_focus();
    until(|| gtk4::prelude::GtkWindowExt::focus(&window).as_ref() == Some(&parent_focus));
    check_primary();
    println!("PRIMARY_CONTROL stage=parent-only-before-hide");
    window.set_visible(false);
    settle();
    check_primary();
    println!("PRIMARY_CONTROL stage=parent-only-hidden");
    window.present();
    settle();
    check_primary();
    println!("PRIMARY_CONTROL stage=parent-only-remapped");
    // Application Escape before the first map frame retires immediately,
    // including a real worker reply released after cancellation. On 1.5 it
    // must finish the native map/close without ever showing an empty modal.
    for hide_parent in [false, true] {
        println!("APP_CLOSE hide_parent={hide_parent} stage=loop-entry");
        check_primary();
        let before_activation = activations.get();
        parent_action.grab_focus();
        until(|| gtk4::prelude::GtkWindowExt::focus(&window).as_ref() == Some(&parent_focus));
        println!("APP_CLOSE hide_parent={hide_parent} stage=before-first-map");
        let (early, early_controls) = present_exact_preview(&window, b.clone(), "absent").unwrap();
        let (arrived, release, published, finish) =
            early_controls.worker.pause_publication_for("different");
        entry(&early).set_text("different");
        // The real worker runs, but GTK intentionally has not processed a map
        // frame. Worker completion must never stand in for native readiness.
        arrived.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(early_controls.mapped_frames.get(), 0);
        check_primary();
        println!("APP_CLOSE hide_parent={hide_parent} stage=worker-paused-before-request");
        let controllers = early.observe_controllers();
        let escape = (0..controllers.n_items())
            .filter_map(|i| {
                controllers
                    .item(i)
                    .and_then(|c| c.downcast::<gtk4::EventControllerKey>().ok())
            })
            .find(|c| c.propagation_phase() == gtk4::PropagationPhase::Capture)
            .unwrap();
        assert!(escape.emit_by_name::<bool>(
            "key-pressed",
            &[
                &gtk4::gdk::Key::Escape,
                &0_u32,
                &gtk4::gdk::ModifierType::empty()
            ]
        ));
        assert!(
            early_controls.closed.get(),
            "application close retires at request time"
        );
        assert_eq!(early_controls.generation.get(), 0);
        assert!(early_controls.matches.borrow().is_none());
        assert_eq!(
            early_controls.text.upgrade().unwrap().buffer().char_count(),
            0
        );
        assert_eq!(early.opacity(), 0.0);
        assert!(
            !early.can_focus(),
            "retired modal cannot restore an input owner"
        );
        assert!(
            !early.can_target(),
            "retired modal cannot capture pointer input"
        );
        assert!(early
            .property::<Option<gtk4::Widget>>("focus-widget")
            .is_none());
        assert!(early_controls.close_pending.get());
        early_controls.request_close(); // A second request cannot rearm work.
        check_primary();
        println!("APP_CLOSE hide_parent={hide_parent} stage=request-retired-before-window-hide");
        let frames = Rc::new(Cell::new(0_u8));
        let frame_count = frames.clone();
        let early_weak = early.downgrade();
        window.add_tick_callback(move |_, _| {
            let Some(early) = early_weak.upgrade().filter(|d| d.parent().is_some()) else {
                return glib::ControlFlow::Break;
            };
            assert_eq!(early.opacity(), 0.0, "retired dialog became visible again");
            frame_count.set(frame_count.get().saturating_add(1));
            assert!(
                frame_count.get() < 120,
                "native modal did not leave its host"
            );
            glib::ControlFlow::Continue
        });
        if hide_parent {
            window.set_visible(false);
            settle();
            assert!(
                early_controls.map_tick.borrow().is_none(),
                "unmap must remove the bounded tick"
            );
            assert_eq!(early.opacity(), 0.0);
            check_primary();
            println!("APP_CLOSE hide_parent={hide_parent} stage=window-hidden");
            window.present();
            check_primary();
            println!("APP_CLOSE hide_parent={hide_parent} stage=window-present-requested");
        }
        release.send(()).unwrap();
        published.recv_timeout(Duration::from_secs(5)).unwrap();
        finish.send(()).unwrap();
        until(|| early.parent().is_none());
        assert!(!early_controls.close_pending.get());
        assert!(early_controls.map_tick.borrow().is_none());
        assert!(
            frames.get() > 0,
            "native dismissal must be observed through frames"
        );
        assert!(
            window.visible_dialog().is_none(),
            "transparent modal still owns the host"
        );
        until(|| gtk4::prelude::GtkWindowExt::focus(&window).as_ref() == Some(&parent_focus));
        assert_retired(&early_controls);
        check_primary();
        println!("APP_CLOSE hide_parent={hide_parent} stage=host-removed focus=original-button frames={}", frames.get());
        // XTest goes through the real X11/GTK input path. Do not grab parent
        // focus after closing: native host removal must restore it itself.
        let ids = std::process::Command::new("xdotool")
            .args([
                "search",
                "--onlyvisible",
                "--name",
                &format!("^{window_title}$"),
            ])
            .output()
            .unwrap();
        assert!(ids.status.success());
        let ids = String::from_utf8(ids.stdout).unwrap();
        let mut ids = ids.lines();
        let id = ids.next().unwrap();
        assert!(
            ids.next().is_none(),
            "private keyboard target must be unique"
        );
        assert!(std::process::Command::new("xdotool")
            .args(["windowfocus", "--sync", id])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("xdotool")
            .args(["key", "--clearmodifiers", "space"])
            .status()
            .unwrap()
            .success());
        until(|| activations.get() == before_activation + 1);
        settle();
        assert_eq!(
            activations.get(),
            before_activation + 1,
            "one native Space must activate only the original parent button"
        );
        assert_eq!(
            gtk4::prelude::GtkWindowExt::focus(&window).as_ref(),
            Some(&parent_focus)
        );
        check_primary();
        println!("APP_CLOSE hide_parent={hide_parent} stage=parent-XTest-Space PRIMARY=unchanged");
        drop(early);
        settle();
        check_primary();
        println!(
            "APP_CLOSE hide_parent={hide_parent} stage=dialog-reference-dropped PRIMARY=unchanged"
        );
    }
    // Direct native force_close has no application request. Establish its
    // actual opened-frame precondition, then retain the immediate assertion.
    let (forced, force_controls) = present_exact_preview(&window, b.clone(), "different").unwrap();
    until(|| status(&force_controls) == "Preview: 1 of 3");
    until(|| force_controls.mapped_frames.get() >= 3);
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
    assert!(
        last_controls.map_tick.borrow().is_none(),
        "parent destruction must remove the bounded map tick"
    );
    assert!(last_controls.matches.borrow().is_none());
    check_primary();
    clipboard.disconnect(handler);
}
